//! The Engine: the single mutation path `apply(Action) -> Vec<Event>` plus
//! incremental recalculation.

use crate::addr::{CellAddr, RangeAddr};
use crate::ast::{Expr, RefVisit};
use crate::deps::DepGraph;
use crate::eval::EvalCtx;
use crate::format::{FormatId, FormatPatch};
use crate::model::{Cell, CellContent, CellKey, Sheet, SheetId, Workbook};
use crate::parser::parse_formula;
use crate::refs::Axis;
use crate::value::{ErrorKind, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The most cells one formatting action may touch.
///
/// We model formatting per cell, not per row or column as xlsx does, so
/// "bold this whole column" would otherwise materialise a million map
/// entries. The limit fails loudly instead of quietly eating memory; raising
/// it properly means adding row and column format defaults, which v1 does not
/// have.
pub const MAX_FORMAT_CELLS: u64 = 200_000;

/// How many extra recalculation passes a sheet using OFFSET or INDIRECT may
/// take before the engine stops chasing the answer.
///
/// Three is enough for any chain a human writes; a sheet that still has not
/// settled is one where a computed reference points at another computed
/// reference several deep, and the alternative to a bound is a hang.
const MAX_DYNAMIC_REFERENCE_PASSES: usize = 3;

/// How many times spilled blocks may be laid out and re-read before the
/// engine stops. Same bound and the same reasoning as above.
const MAX_SPILL_PASSES: usize = 4;

/// What a paste carries over from the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteMode {
    /// Formulas (with references adjusted) and literals.
    Formulas,
    /// Computed results only.
    Values,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    /// Absolute column index of the key.
    pub column: u32,
    pub ascending: bool,
}

/// A checkbox-style value filter over one column of a range. The range's
/// first row is treated as a header and never hidden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterSpec {
    pub range: RangeAddr,
    pub column: u32,
    /// Display strings that remain visible.
    pub allowed: Vec<String>,
}

/// Semantic actions. Every state mutation flows through `Engine::apply`.
/// The serialized action log is the source of truth: replaying it from an
/// empty workbook reproduces the exact final state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Set a cell from user input ("=SUM(A1:A3)", "42", "hello", "TRUE").
    CellEdit {
        sheet: String,
        addr: CellAddr,
        input: String,
    },
    CellClear {
        sheet: String,
        addr: CellAddr,
    },
    RangeClear {
        sheet: String,
        range: RangeAddr,
    },
    /// Copy (or cut) a block and paste it elsewhere.
    RangePaste {
        source_sheet: String,
        source: RangeAddr,
        target_sheet: String,
        target: RangeAddr,
        mode: PasteMode,
        cut: bool,
    },
    /// Extend `source` across `target` (fill handle / Ctrl+D / Ctrl+R).
    FillApply {
        sheet: String,
        source: RangeAddr,
        target: RangeAddr,
    },
    RowInsert {
        sheet: String,
        at: u32,
        count: u32,
    },
    RowDelete {
        sheet: String,
        at: u32,
        count: u32,
    },
    ColInsert {
        sheet: String,
        at: u32,
        count: u32,
    },
    ColDelete {
        sheet: String,
        at: u32,
        count: u32,
    },
    SortApply {
        sheet: String,
        range: RangeAddr,
        keys: Vec<SortKey>,
        has_header: bool,
    },
    FilterApply {
        sheet: String,
        spec: FilterSpec,
    },
    FilterClear {
        sheet: String,
    },
    MergeApply {
        sheet: String,
        range: RangeAddr,
    },
    MergeClear {
        sheet: String,
        range: RangeAddr,
    },
    /// Change presentation over a range. Each patch names one attribute, so
    /// bolding a range leaves its fill colour alone.
    FormatApply {
        sheet: String,
        range: RangeAddr,
        patches: Vec<FormatPatch>,
    },
    /// Strip all formatting from a range, leaving contents untouched.
    FormatClear {
        sheet: String,
        range: RangeAddr,
    },
    /// Replace text across a range (the whole sheet when `range` is None),
    /// matching against what the formula bar would show — so a formula is
    /// matched and rewritten by its source, never by its result.
    FindReplace {
        sheet: String,
        range: Option<RangeAddr>,
        find: String,
        replace: String,
        match_case: bool,
        whole_cell: bool,
    },
    SheetAdd {
        name: String,
    },
    SheetRename {
        from: String,
        to: String,
    },
    SheetDelete {
        name: String,
    },
    /// Resize columns or rows. `size` is in pixels; `None` restores the
    /// default, which is how a "reset width" gesture is expressed without a
    /// second action.
    Resize {
        sheet: String,
        axis: Axis,
        /// First index, then how many. A drag resizes one; a multi-column
        /// selection resizes the run, and autofit resizes each to its own
        /// width, which arrives as several of these in one batch.
        at: u32,
        count: u32,
        size: Option<f64>,
    },
    /// Define a workbook-level name, or redefine one. `refers_to` is an A1
    /// range as xlsx spells it — usually sheet-qualified and absolute.
    NameDefine {
        name: String,
        refers_to: String,
    },
    NameDelete {
        name: String,
    },
    Undo,
    Redo,
}

/// What happened as a result of an action. Events are semantic and describe
/// intent; `Recalced` is derived state for the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    CellEdited {
        sheet: String,
        addr: CellAddr,
        input: String,
        prev_input: Option<String>,
    },
    CellCleared {
        sheet: String,
        addr: CellAddr,
        prev_input: Option<String>,
    },
    RangeCleared {
        sheet: String,
        range: RangeAddr,
        cleared: u32,
    },
    RangePasted {
        source: String,
        target: String,
        mode: PasteMode,
        cut: bool,
    },
    FillApplied {
        sheet: String,
        source: RangeAddr,
        target: RangeAddr,
        filled: u32,
    },
    RowsInserted {
        sheet: String,
        at: u32,
        count: u32,
    },
    RowsDeleted {
        sheet: String,
        at: u32,
        count: u32,
    },
    ColsInserted {
        sheet: String,
        at: u32,
        count: u32,
    },
    ColsDeleted {
        sheet: String,
        at: u32,
        count: u32,
    },
    SortApplied {
        sheet: String,
        range: RangeAddr,
        keys: Vec<SortKey>,
    },
    FilterApplied {
        sheet: String,
        column: u32,
        hidden: u32,
    },
    FilterCleared {
        sheet: String,
    },
    MergeApplied {
        sheet: String,
        range: RangeAddr,
    },
    MergeCleared {
        sheet: String,
        range: RangeAddr,
    },
    FormatApplied {
        sheet: String,
        range: RangeAddr,
        attributes: Vec<String>,
        cells: u32,
    },
    FormatCleared {
        sheet: String,
        range: RangeAddr,
        cells: u32,
    },
    Replaced {
        sheet: String,
        cells: u32,
    },
    SheetAdded {
        name: String,
    },
    SheetRenamed {
        from: String,
        to: String,
    },
    SheetDeleted {
        name: String,
    },
    Undone {
        label: String,
    },
    Redone {
        label: String,
    },
    NameDefined {
        name: String,
        refers_to: String,
        /// What it meant before, when this replaced an existing definition.
        prev: Option<String>,
    },
    NameDeleted {
        name: String,
    },
    Resized {
        sheet: String,
        axis: Axis,
        at: u32,
        count: u32,
        /// None when the run went back to the default size.
        size: Option<f64>,
    },
    /// Cells whose computed value changed due to recalculation (derived
    /// state; informational for the UI, not required for replay).
    Recalced {
        cells: Vec<(String, CellAddr)>,
    },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ApplyError {
    #[error("unknown sheet '{0}'")]
    UnknownSheet(String),
    #[error("invalid cell address")]
    BadAddr,
    #[error("formula parse error: {0}")]
    Formula(#[from] crate::parser::ParseError),
    #[error("sheet name '{0}' already exists")]
    DuplicateSheet(String),
    #[error("cannot delete the last sheet")]
    LastSheet,
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("{0}")]
    Invalid(String),
}

/// The previous state an action must restore to be undone. Cell-level
/// operations record only what they touched; operations that relocate cells
/// wholesale record the affected sheets.
#[derive(Debug, Clone)]
pub enum UndoState {
    Cells(Vec<(SheetId, CellAddr, Option<Cell>)>),
    /// Format ids, not formats: the palette is append-only, so an id recorded
    /// here still resolves after any number of intervening changes.
    Formats(Vec<(SheetId, CellAddr, Option<FormatId>)>),
    /// Restored in order, so an operation that moves contents *and* their
    /// formatting undoes as one step.
    Compound(Vec<UndoState>),
    Sheets(Vec<Sheet>),
    /// The whole width (or height) map for one sheet. Small enough to copy
    /// wholesale — a sheet has at most a few hundred non-default entries —
    /// and copying it means an autofit over a selection undoes as one map
    /// swap instead of a list of per-column patches.
    Sizes(SheetId, Axis, BTreeMap<u32, f64>),
    /// The whole name table. A handful of entries at most, and swapping it
    /// wholesale means a redefinition and a deletion undo the same way.
    Names(BTreeMap<String, String>),
}

impl UndoState {
    /// How many cells' contents this records, for the "n cells changed"
    /// counts events carry. Formats are counted separately or not at all.
    pub fn cell_count(&self) -> u32 {
        match self {
            UndoState::Cells(c) => c.len() as u32,
            UndoState::Compound(parts) => parts.iter().map(|p| p.cell_count()).sum(),
            UndoState::Formats(_)
            | UndoState::Sheets(_)
            | UndoState::Sizes(..)
            | UndoState::Names(_) => 0,
        }
    }
}

#[derive(Debug, Clone)]
struct UndoEntry {
    label: String,
    state: UndoState,
}

#[derive(Debug, Clone, Default)]
pub struct Engine {
    pub wb: Workbook,
    deps: DepGraph,
    volatile: HashSet<CellKey>,
    /// Anchors whose block had nowhere to go at the last placement. Kept so
    /// re-evaluating one does not flip it back to its first element for a
    /// pass; see `place_spills`.
    spill_blocked: HashSet<CellKey>,
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    /// Injected clock for NOW/TODAY so evaluation is replayable; the shell
    /// updates this from event timestamps.
    pub now_ms: i64,
}

impl Engine {
    pub fn new() -> Self {
        Engine {
            wb: Workbook::new(),
            deps: DepGraph::default(),
            volatile: HashSet::new(),
            spill_blocked: HashSet::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            now_ms: 0,
        }
    }

    pub fn apply(&mut self, action: &Action) -> Result<Vec<Event>, ApplyError> {
        match action {
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            other => {
                let events = self.apply_forward(other)?;
                // Any new action invalidates the redo history.
                self.redo_stack.clear();
                Ok(events)
            }
        }
    }

    /// Apply several actions as one user-visible gesture.
    ///
    /// The difference from calling [`Engine::apply`] in a loop is undo: this
    /// coalesces everything the batch pushed into a single entry, so the
    /// gesture comes back in one Ctrl+Z. A routine that took five actions to
    /// express is still one thing the user asked for, and making them reject
    /// it five times would be a good way to stop anyone using routines.
    ///
    /// A failure part-way through leaves the earlier actions applied, as it
    /// does for a loop of `apply` — and, in that case, uncoalesced, because
    /// the caller is now looking at a partial result and should be able to
    /// step back through it.
    pub fn apply_batch(
        &mut self,
        actions: &[Action],
        label: &str,
    ) -> Result<Vec<Event>, ApplyError> {
        let mark = self.undo_stack.len();
        let mut events = Vec::new();
        for a in actions {
            events.extend(self.apply(a)?);
        }
        // `>` and not `>=`: one entry is already one undo step, and an
        // `Undo` inside the batch can leave the stack shorter than the mark.
        if self.undo_stack.len() > mark + 1 {
            let parts: Vec<UndoState> = self
                .undo_stack
                .drain(mark..)
                .map(|entry| entry.state)
                .collect();
            self.undo_stack.push(UndoEntry {
                label: label.to_string(),
                state: UndoState::Compound(parts),
            });
        }
        Ok(events)
    }

    fn apply_forward(&mut self, action: &Action) -> Result<Vec<Event>, ApplyError> {
        match action {
            Action::CellEdit { sheet, addr, input } => self.cell_edit(sheet, *addr, input),
            Action::CellClear { sheet, addr } => self.cell_clear(sheet, *addr),
            Action::RangeClear { sheet, range } => self.range_clear(sheet, *range),
            Action::RangePaste {
                source_sheet,
                source,
                target_sheet,
                target,
                mode,
                cut,
            } => self.range_paste(source_sheet, *source, target_sheet, *target, *mode, *cut),
            Action::FillApply {
                sheet,
                source,
                target,
            } => self.fill_apply(sheet, *source, *target),
            Action::RowInsert { sheet, at, count } => {
                self.shift(sheet, Axis::Row, *at, *count, true)
            }
            Action::RowDelete { sheet, at, count } => {
                self.shift(sheet, Axis::Row, *at, *count, false)
            }
            Action::ColInsert { sheet, at, count } => {
                self.shift(sheet, Axis::Col, *at, *count, true)
            }
            Action::ColDelete { sheet, at, count } => {
                self.shift(sheet, Axis::Col, *at, *count, false)
            }
            Action::SortApply {
                sheet,
                range,
                keys,
                has_header,
            } => self.sort_apply(sheet, *range, keys, *has_header),
            Action::FilterApply { sheet, spec } => self.filter_apply(sheet, Some(spec.clone())),
            Action::FilterClear { sheet } => self.filter_apply(sheet, None),
            Action::MergeApply { sheet, range } => self.merge(sheet, *range, true),
            Action::MergeClear { sheet, range } => self.merge(sheet, *range, false),
            Action::FormatApply {
                sheet,
                range,
                patches,
            } => self.format_apply(sheet, *range, patches),
            Action::FormatClear { sheet, range } => self.format_clear(sheet, *range),
            Action::FindReplace {
                sheet,
                range,
                find,
                replace,
                match_case,
                whole_cell,
            } => self.find_replace(sheet, *range, find, replace, *match_case, *whole_cell),
            Action::SheetAdd { name } => self.sheet_add(name),
            Action::SheetRename { from, to } => self.sheet_rename(from, to),
            Action::SheetDelete { name } => self.sheet_delete(name),
            Action::Resize {
                sheet,
                axis,
                at,
                count,
                size,
            } => self.resize(sheet, *axis, *at, *count, *size),
            Action::NameDefine { name, refers_to } => self.name_define(name, refers_to),
            Action::NameDelete { name } => self.name_delete(name),
            Action::Undo | Action::Redo => unreachable!("handled in apply"),
        }
    }

    /// Restore a recorded previous state, returning the state that was
    /// replaced (so undo and redo are the same operation in both directions).
    fn restore(&mut self, state: UndoState) -> UndoState {
        match state {
            UndoState::Cells(patches) => {
                let mut inverse = Vec::with_capacity(patches.len());
                // Restore in reverse order so a cell touched twice by one
                // action ends at its original value.
                for (sid, addr, cell) in patches.into_iter().rev() {
                    let Some(sheet) = self.wb.sheet_mut(sid) else {
                        continue;
                    };
                    let replaced = match cell {
                        Some(c) => sheet.cells.insert(addr, c),
                        None => sheet.cells.remove(&addr),
                    };
                    inverse.push((sid, addr, replaced));
                }
                inverse.reverse();
                UndoState::Cells(inverse)
            }
            UndoState::Formats(patches) => {
                let mut inverse = Vec::with_capacity(patches.len());
                for (sid, addr, id) in patches.into_iter().rev() {
                    let Some(sheet) = self.wb.sheet_mut(sid) else {
                        continue;
                    };
                    let replaced = match id {
                        Some(i) => sheet.formats.insert(addr, i),
                        None => sheet.formats.remove(&addr),
                    };
                    inverse.push((sid, addr, replaced));
                }
                inverse.reverse();
                UndoState::Formats(inverse)
            }
            UndoState::Compound(parts) => {
                // Reverse order, so restoring undoes the parts in the
                // opposite sequence to the one that applied them.
                let mut inverse: Vec<UndoState> =
                    parts.into_iter().rev().map(|p| self.restore(p)).collect();
                inverse.reverse();
                UndoState::Compound(inverse)
            }
            UndoState::Sheets(sheets) => {
                let replaced = std::mem::replace(&mut self.wb.sheets, sheets);
                UndoState::Sheets(replaced)
            }
            UndoState::Names(names) => {
                UndoState::Names(std::mem::replace(&mut self.wb.names, names))
            }
            UndoState::Sizes(sid, axis, sizes) => {
                let Some(sheet) = self.wb.sheet_mut(sid) else {
                    return UndoState::Sizes(sid, axis, sizes);
                };
                let map = match axis {
                    Axis::Col => &mut sheet.col_widths,
                    Axis::Row => &mut sheet.row_heights,
                };
                UndoState::Sizes(sid, axis, std::mem::replace(map, sizes))
            }
        }
    }

    fn undo(&mut self) -> Result<Vec<Event>, ApplyError> {
        let entry = self.undo_stack.pop().ok_or(ApplyError::NothingToUndo)?;
        let label = entry.label.clone();
        let inverse = self.restore(entry.state);
        self.redo_stack.push(UndoEntry {
            label: label.clone(),
            state: inverse,
        });
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::Undone { label }])
    }

    fn redo(&mut self) -> Result<Vec<Event>, ApplyError> {
        let entry = self.redo_stack.pop().ok_or(ApplyError::NothingToRedo)?;
        let label = entry.label.clone();
        let inverse = self.restore(entry.state);
        self.undo_stack.push(UndoEntry {
            label: label.clone(),
            state: inverse,
        });
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::Redone { label }])
    }

    fn push_undo(&mut self, label: &str, state: UndoState) {
        self.undo_stack.push(UndoEntry {
            label: label.to_string(),
            state,
        });
    }

    /// Rebuild the dependency graph and recalculate every formula. A full
    /// recalculation must always agree with the incremental one; the
    /// property tests assert exactly that.
    pub fn recalc_all(&mut self) {
        self.rebuild_deps_and_recalc_all();
    }

    /// Forget the undo and redo history, keeping the workbook.
    ///
    /// Import replays a file cell by cell through `apply`, which is what keeps
    /// the importer honest — but it also means a freshly opened workbook
    /// arrives with one undo entry per imported cell, and the user's first
    /// Ctrl+Z un-types a cell they never typed. Opening a file is a new
    /// starting point, not an edit.
    pub fn clear_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn range_clear(&mut self, sheet: &str, range: RangeAddr) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let undo = self.op_range_clear(sid, range)?;
        let cleared = undo.cell_count();
        self.push_undo("clear", undo);
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::RangeCleared {
            sheet: sheet.to_string(),
            range,
            cleared,
        }])
    }

    #[allow(clippy::too_many_arguments)]
    fn range_paste(
        &mut self,
        src_sheet: &str,
        src: RangeAddr,
        dst_sheet: &str,
        dst: RangeAddr,
        mode: PasteMode,
        cut: bool,
    ) -> Result<Vec<Event>, ApplyError> {
        let ssid = self.sheet_id(src_sheet)?;
        let dsid = self.sheet_id(dst_sheet)?;
        let undo = self.op_paste(ssid, src, dsid, dst, mode, cut)?;
        self.push_undo(if cut { "cut" } else { "paste" }, undo);
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::RangePasted {
            source: format!("{}!{}", src_sheet, src),
            target: format!("{}!{}", dst_sheet, dst),
            mode,
            cut,
        }])
    }

    fn fill_apply(
        &mut self,
        sheet: &str,
        src: RangeAddr,
        dst: RangeAddr,
    ) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        if !(dst.start.row <= src.start.row
            && dst.end.row >= src.end.row
            && dst.start.col <= src.start.col
            && dst.end.col >= src.end.col)
        {
            return Err(ApplyError::Invalid(
                "fill target must contain the source range".into(),
            ));
        }
        let undo = self.op_fill(sid, src, dst)?;
        let filled = undo.cell_count();
        self.push_undo("fill", undo);
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::FillApplied {
            sheet: sheet.to_string(),
            source: src,
            target: dst,
            filled,
        }])
    }

    fn shift(
        &mut self,
        sheet: &str,
        axis: Axis,
        at: u32,
        count: u32,
        insert: bool,
    ) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let undo = self.op_shift(sid, axis, at, count, insert)?;
        self.push_undo(
            match (axis, insert) {
                (Axis::Row, true) => "insert rows",
                (Axis::Row, false) => "delete rows",
                (Axis::Col, true) => "insert columns",
                (Axis::Col, false) => "delete columns",
            },
            undo,
        );
        self.rebuild_deps_and_recalc_all();
        let sheet = sheet.to_string();
        Ok(vec![match (axis, insert) {
            (Axis::Row, true) => Event::RowsInserted { sheet, at, count },
            (Axis::Row, false) => Event::RowsDeleted { sheet, at, count },
            (Axis::Col, true) => Event::ColsInserted { sheet, at, count },
            (Axis::Col, false) => Event::ColsDeleted { sheet, at, count },
        }])
    }

    fn sort_apply(
        &mut self,
        sheet: &str,
        range: RangeAddr,
        keys: &[SortKey],
        has_header: bool,
    ) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        if keys.is_empty() {
            return Err(ApplyError::Invalid("sort needs at least one key".into()));
        }
        let undo = self.op_sort(sid, range, keys, has_header)?;
        self.push_undo("sort", undo);
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::SortApplied {
            sheet: sheet.to_string(),
            range,
            keys: keys.to_vec(),
        }])
    }

    fn filter_apply(
        &mut self,
        sheet: &str,
        spec: Option<FilterSpec>,
    ) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let column = spec.as_ref().map(|s| s.column);
        let undo = self.op_filter(sid, spec)?;
        self.push_undo("filter", undo);
        let hidden = self.wb.sheet(sid).unwrap().hidden_rows.len() as u32;
        Ok(vec![match column {
            Some(column) => Event::FilterApplied {
                sheet: sheet.to_string(),
                column,
                hidden,
            },
            None => Event::FilterCleared {
                sheet: sheet.to_string(),
            },
        }])
    }

    fn merge(
        &mut self,
        sheet: &str,
        range: RangeAddr,
        merge: bool,
    ) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let undo = self.op_merge(sid, range, merge)?;
        self.push_undo(if merge { "merge" } else { "unmerge" }, undo);
        self.rebuild_deps_and_recalc_all();
        let sheet = sheet.to_string();
        Ok(vec![if merge {
            Event::MergeApplied { sheet, range }
        } else {
            Event::MergeCleared { sheet, range }
        }])
    }

    /// Apply presentation patches over a range.
    ///
    /// Formatting never touches contents and never triggers a recalculation:
    /// a number format changes how a value reads, not what it is. That is
    /// also why `=A1&""` does not see the format — Excel behaves the same way.
    fn format_apply(
        &mut self,
        sheet: &str,
        range: RangeAddr,
        patches: &[FormatPatch],
    ) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        if patches.is_empty() {
            return Err(ApplyError::Invalid(
                "format needs at least one patch".into(),
            ));
        }
        if range.cell_count() > MAX_FORMAT_CELLS {
            return Err(ApplyError::Invalid(format!(
                "formatting {} cells exceeds the {} cell limit",
                range.cell_count(),
                MAX_FORMAT_CELLS
            )));
        }
        let undo = self.op_format(sid, range, patches);
        let cells = undo.len() as u32;
        self.push_undo("format", UndoState::Formats(undo));
        Ok(vec![Event::FormatApplied {
            sheet: sheet.to_string(),
            range,
            attributes: patches.iter().map(|p| p.attribute().to_string()).collect(),
            cells,
        }])
    }

    fn format_clear(&mut self, sheet: &str, range: RangeAddr) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let s = self.wb.sheet(sid).expect("sheet exists");
        let addrs: Vec<CellAddr> = s
            .formats
            .keys()
            .copied()
            .filter(|a| range.contains(*a))
            .collect();
        let mut undo = Vec::with_capacity(addrs.len());
        for a in addrs {
            let old = self.wb.sheet_mut(sid).unwrap().formats.remove(&a);
            undo.push((sid, a, old));
        }
        let cells = undo.len() as u32;
        self.push_undo("clear formatting", UndoState::Formats(undo));
        Ok(vec![Event::FormatCleared {
            sheet: sheet.to_string(),
            range,
            cells,
        }])
    }

    /// Replace text across a range, matching on what the formula bar shows.
    ///
    /// Matching the *input* rather than the computed value is the only
    /// coherent choice: there is no way to write a replacement back into a
    /// formula's result, so a search that matched results would either refuse
    /// to replace or destroy the formula that produced them. Excel's default
    /// "Look in: Formulas" does the same thing.
    fn find_replace(
        &mut self,
        sheet: &str,
        range: Option<RangeAddr>,
        find: &str,
        replace: &str,
        match_case: bool,
        whole_cell: bool,
    ) -> Result<Vec<Event>, ApplyError> {
        if find.is_empty() {
            return Err(ApplyError::Invalid("nothing to find".into()));
        }
        let sid = self.sheet_id(sheet)?;
        let hits = self.matches_in(sid, range, find, match_case, whole_cell);

        let mut undo = Vec::new();
        let mut seeds = Vec::new();
        for (addr, input) in hits {
            let next = replace_text(&input, find, replace, match_case, whole_cell);
            if next == input {
                continue;
            }
            let key = CellKey { sheet: sid, addr };
            let prev = self.wb.sheet(sid).unwrap().cells.get(&addr).cloned();
            if next.is_empty() {
                self.deps.clear(key);
                self.volatile.remove(&key);
                self.wb.sheet_mut(sid).unwrap().cells.remove(&addr);
            } else {
                // A replacement can turn a literal into a formula or the
                // reverse, so the cell is rebuilt from its text exactly as a
                // typed edit would be. A replacement that produces an
                // unparseable formula leaves that cell alone rather than
                // failing the whole operation part-way through.
                let Ok(cell) = build_cell(&next) else {
                    continue;
                };
                self.wb.sheet_mut(sid).unwrap().cells.insert(addr, cell);
            }
            undo.push((sid, addr, prev));
            seeds.push(key);
        }

        let cells = undo.len() as u32;
        self.push_undo("replace", UndoState::Cells(undo));
        // One rebuild for the whole operation rather than one per cell.
        self.rebuild_deps_and_recalc_all();
        let mut events = vec![Event::Replaced {
            sheet: sheet.to_string(),
            cells,
        }];
        if !seeds.is_empty() {
            events.push(Event::Recalced {
                cells: self.keys_to_names(&seeds),
            });
        }
        Ok(events)
    }

    /// Addresses whose formula-bar text matches, with that text. Read-only,
    /// so the UI can drive find-next through the same matching rules that
    /// replace uses rather than a second implementation of them.
    pub fn matches_in(
        &self,
        sheet: SheetId,
        range: Option<RangeAddr>,
        find: &str,
        match_case: bool,
        whole_cell: bool,
    ) -> Vec<(CellAddr, String)> {
        let Some(s) = self.wb.sheet(sheet) else {
            return Vec::new();
        };
        let mut hits: Vec<(CellAddr, String)> = s
            .cells
            .iter()
            .filter(|(a, _)| range.map(|r| r.contains(**a)).unwrap_or(true))
            .map(|(a, c)| (*a, c.input()))
            .filter(|(_, input)| text_matches(input, find, match_case, whole_cell))
            .collect();
        // Reading order, so "find next" walks the sheet the way a user reads
        // it rather than in hash order.
        hits.sort_by_key(|(a, _)| *a);
        hits
    }

    /// Find matches by sheet name, for callers outside the engine.
    pub fn find_matches(
        &self,
        sheet: &str,
        range: Option<RangeAddr>,
        find: &str,
        match_case: bool,
        whole_cell: bool,
    ) -> Vec<CellAddr> {
        let Some(sid) = self.wb.sheet_id_by_name(sheet) else {
            return Vec::new();
        };
        self.matches_in(sid, range, find, match_case, whole_cell)
            .into_iter()
            .map(|(a, _)| a)
            .collect()
    }

    fn sheet_id(&self, name: &str) -> Result<SheetId, ApplyError> {
        self.wb
            .sheet_id_by_name(name)
            .ok_or_else(|| ApplyError::UnknownSheet(name.to_string()))
    }

    fn cell_edit(
        &mut self,
        sheet: &str,
        addr: CellAddr,
        input: &str,
    ) -> Result<Vec<Event>, ApplyError> {
        if !addr.is_valid() {
            return Err(ApplyError::BadAddr);
        }
        // Committing an empty edit leaves the cell blank rather than storing
        // an empty string, so ISBLANK and COUNTBLANK behave as in Excel.
        if input.is_empty() {
            return self.cell_clear(sheet, addr);
        }
        let sid = self.sheet_id(sheet)?;
        let key = CellKey { sheet: sid, addr };
        let cell = build_cell(input)?;
        let prev_input = self.prev_input(key);
        let prev_cell = self.wb.sheet(sid).and_then(|s| s.cells.get(&addr)).cloned();
        self.push_undo("edit", UndoState::Cells(vec![(sid, addr, prev_cell)]));

        // Maintain the dependency graph for the new content.
        match &cell.content {
            CellContent::Formula { ast, .. } => {
                let (exact, ranges) = self.precedents_of_ast(sid, ast);
                self.deps.set_precedents(key, exact, ranges);
                if ast.is_volatile() {
                    self.volatile.insert(key);
                } else {
                    self.volatile.remove(&key);
                }
            }
            CellContent::Literal(_) => {
                self.deps.clear(key);
                self.volatile.remove(&key);
            }
        }
        self.wb
            .sheet_mut(sid)
            .expect("sheet exists")
            .cells
            .insert(addr, cell);

        let recalced = self.recalc(vec![key]);
        let mut events = vec![Event::CellEdited {
            sheet: self.wb.sheet(sid).unwrap().name.clone(),
            addr,
            input: input.to_string(),
            prev_input,
        }];
        if !recalced.is_empty() {
            events.push(Event::Recalced {
                cells: self.keys_to_names(&recalced),
            });
        }
        Ok(events)
    }

    fn cell_clear(&mut self, sheet: &str, addr: CellAddr) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let key = CellKey { sheet: sid, addr };
        let prev_input = self.prev_input(key);
        self.deps.clear(key);
        self.volatile.remove(&key);
        let prev_cell = self
            .wb
            .sheet_mut(sid)
            .expect("sheet exists")
            .cells
            .remove(&addr);
        self.push_undo("clear", UndoState::Cells(vec![(sid, addr, prev_cell)]));
        let recalced = self.recalc(vec![key]);
        let mut events = vec![Event::CellCleared {
            sheet: self.wb.sheet(sid).unwrap().name.clone(),
            addr,
            prev_input,
        }];
        if !recalced.is_empty() {
            events.push(Event::Recalced {
                cells: self.keys_to_names(&recalced),
            });
        }
        Ok(events)
    }

    fn sheet_add(&mut self, name: &str) -> Result<Vec<Event>, ApplyError> {
        if self.wb.sheet_by_name(name).is_some() {
            return Err(ApplyError::DuplicateSheet(name.to_string()));
        }
        self.push_undo("add sheet", UndoState::Sheets(self.wb.sheets.clone()));
        self.wb.add_sheet(name);
        // A new sheet can satisfy previously-broken cross-sheet refs.
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::SheetAdded {
            name: name.to_string(),
        }])
    }

    fn sheet_rename(&mut self, from: &str, to: &str) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(from)?;
        if !from.eq_ignore_ascii_case(to) && self.wb.sheet_by_name(to).is_some() {
            return Err(ApplyError::DuplicateSheet(to.to_string()));
        }
        self.push_undo("rename sheet", UndoState::Sheets(self.wb.sheets.clone()));
        // Excel rewrites formulas on rename; we do the same so formula text
        // stays consistent with sheet names.
        self.rewrite_sheet_refs(from, Some(to));
        self.wb.sheet_mut(sid).unwrap().name = to.to_string();
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::SheetRenamed {
            from: from.to_string(),
            to: to.to_string(),
        }])
    }

    fn sheet_delete(&mut self, name: &str) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(name)?;
        if self.wb.sheets.len() == 1 {
            return Err(ApplyError::LastSheet);
        }
        self.push_undo("delete sheet", UndoState::Sheets(self.wb.sheets.clone()));
        // Refs into the deleted sheet become #REF! (loud failure).
        self.rewrite_sheet_refs(name, None);
        self.wb.sheets.retain(|s| s.id != sid);
        self.volatile.retain(|k| k.sheet != sid);
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::SheetDeleted {
            name: name.to_string(),
        }])
    }

    /// Define or redefine a workbook name.
    fn name_define(&mut self, name: &str, refers_to: &str) -> Result<Vec<Event>, ApplyError> {
        let key = name.trim().to_ascii_uppercase();
        if !is_valid_name(&key) {
            return Err(ApplyError::Invalid(format!(
                "'{name}' is not a usable name: names start with a letter or \
                 underscore, contain no spaces, and must not look like a cell \
                 address"
            )));
        }
        // Parsed here rather than at evaluation so a typo is refused when it
        // is made, not silently every time the name is used.
        let body = refers_to.strip_prefix('=').unwrap_or(refers_to);
        // A definition that parses to nothing but an error — `Sheet1!$A$`,
        // say — is a typo the parser is willing to tolerate as an error node.
        // Storing it would mean the name silently answers #REF! forever.
        if matches!(parse_formula(body)?, Expr::Error(_)) {
            return Err(ApplyError::Invalid(format!(
                "'{refers_to}' is not something a name can refer to"
            )));
        }
        let before = self.wb.names.clone();
        let prev = self.wb.names.insert(key.clone(), refers_to.to_string());
        if prev.as_deref() == Some(refers_to) {
            return Ok(Vec::new());
        }
        self.push_undo("define name", UndoState::Names(before));
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::NameDefined {
            name: key,
            refers_to: refers_to.to_string(),
            prev,
        }])
    }

    fn name_delete(&mut self, name: &str) -> Result<Vec<Event>, ApplyError> {
        let key = name.trim().to_ascii_uppercase();
        let before = self.wb.names.clone();
        if self.wb.names.remove(&key).is_none() {
            return Err(ApplyError::Invalid(format!("no name '{name}'")));
        }
        self.push_undo("delete name", UndoState::Names(before));
        // Formulas using it now say #NAME?, which is the right answer and the
        // reason deleting a name is worth an undo entry.
        self.rebuild_deps_and_recalc_all();
        Ok(vec![Event::NameDeleted { name: key }])
    }

    /// Set or clear a run of column widths or row heights.
    fn resize(
        &mut self,
        sheet: &str,
        axis: Axis,
        at: u32,
        count: u32,
        mut size: Option<f64>,
    ) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        if let Some(px) = size {
            // A non-positive width is a hidden column in Excel, which is a
            // different feature; refusing is better than silently rounding it
            // up to something visible.
            if !(px.is_finite() && px > 0.0) {
                return Err(ApplyError::Invalid(format!("size {px} is not a width")));
            }
            // Whole pixels. Half a pixel is not a width the grid can draw or
            // the file can hold, and keeping one in the model would mean a
            // size that quietly changes the first time the workbook is saved.
            size = Some(px.round());
        }
        let s = self.wb.sheet_mut(sid).expect("sheet exists");
        let map = match axis {
            Axis::Col => &mut s.col_widths,
            Axis::Row => &mut s.row_heights,
        };
        let before = map.clone();
        for i in at..at.saturating_add(count.max(1)) {
            match size {
                Some(px) => {
                    map.insert(i, px);
                }
                // Removing the entry *is* the default, so a reset leaves no
                // trace in the model or in the exported file.
                None => {
                    map.remove(&i);
                }
            }
        }
        if *map == before {
            return Ok(Vec::new());
        }
        self.push_undo("resize", UndoState::Sizes(sid, axis, before));
        Ok(vec![Event::Resized {
            sheet: sheet.to_string(),
            axis,
            at,
            count: count.max(1),
            size,
        }])
    }

    /// Rewrite formulas referencing sheet `from`: rename to `to`, or replace
    /// the ref with #REF! when `to` is None (sheet deleted).
    fn rewrite_sheet_refs(&mut self, from: &str, to: Option<&str>) {
        for si in 0..self.wb.sheets.len() {
            let addrs: Vec<CellAddr> = self.wb.sheets[si]
                .cells
                .iter()
                .filter(|(_, c)| c.is_formula())
                .map(|(a, _)| *a)
                .collect();
            for addr in addrs {
                let cell = self.wb.sheets[si].cells.get(&addr).unwrap();
                let CellContent::Formula { ast, cached, .. } = &cell.content else {
                    continue;
                };
                let mut changed = false;
                let new_ast = rewrite_sheet_in_expr(ast, from, to, &mut changed);
                if changed {
                    let cached = cached.clone();
                    let src = new_ast.to_formula();
                    self.wb.sheets[si].cells.insert(
                        addr,
                        Cell {
                            content: CellContent::Formula {
                                src,
                                ast: new_ast,
                                cached,
                            },
                        },
                    );
                }
            }
        }
        // Defined names point at sheets too. A name left saying `Sales!$A$1`
        // after Sales was deleted is the stale `<definedName>` the handoff
        // notes recorded as a known gap; now that names are modeled it is
        // rewritten like any other reference.
        let names = std::mem::take(&mut self.wb.names);
        self.wb.names = names
            .into_iter()
            .map(|(name, refers_to)| {
                let body = refers_to.strip_prefix('=').unwrap_or(&refers_to);
                let Ok(ast) = parse_formula(body) else {
                    return (name, refers_to);
                };
                let mut changed = false;
                let new_ast = rewrite_sheet_in_expr(&ast, from, to, &mut changed);
                if changed {
                    (name, new_ast.to_formula())
                } else {
                    (name, refers_to)
                }
            })
            .collect();
    }

    fn prev_input(&self, key: CellKey) -> Option<String> {
        self.wb
            .sheet(key.sheet)
            .and_then(|s| s.cells.get(&key.addr))
            .map(|c| c.input())
    }

    fn keys_to_names(&self, keys: &[CellKey]) -> Vec<(String, CellAddr)> {
        keys.iter()
            .filter_map(|k| self.wb.sheet(k.sheet).map(|s| (s.name.clone(), k.addr)))
            .collect()
    }

    /// Resolve AST refs to concrete precedent keys for the dependency graph.
    fn precedents_of_ast(
        &self,
        current: SheetId,
        ast: &Expr,
    ) -> (Vec<CellKey>, Vec<(SheetId, crate::addr::RangeAddr)>) {
        let mut exact = Vec::new();
        let mut ranges = Vec::new();
        ast.visit_refs(&mut |r| match r {
            RefVisit::Cell(c) => {
                let sid = match &c.sheet {
                    None => Some(current),
                    Some(n) => self.wb.sheet_id_by_name(n),
                };
                if let Some(sid) = sid {
                    exact.push(CellKey {
                        sheet: sid,
                        addr: c.r.addr(),
                    });
                }
            }
            RefVisit::Range(rr) => {
                let sid = match &rr.sheet {
                    None => Some(current),
                    Some(n) => self.wb.sheet_id_by_name(n),
                };
                if let Some(sid) = sid {
                    ranges.push((
                        sid,
                        crate::addr::RangeAddr::new(rr.start.addr(), rr.end.addr()),
                    ));
                }
            }
        });
        (exact, ranges)
    }

    fn rebuild_deps_and_recalc_all(&mut self) {
        self.deps.reset();
        self.volatile.clear();
        let mut formula_keys = Vec::new();
        for s in &self.wb.sheets {
            for (addr, cell) in &s.cells {
                if let CellContent::Formula { ast, .. } = &cell.content {
                    formula_keys.push((
                        CellKey {
                            sheet: s.id,
                            addr: *addr,
                        },
                        ast.clone(),
                    ));
                }
            }
        }
        formula_keys.sort_by_key(|(k, _)| *k);
        for (key, ast) in &formula_keys {
            let (exact, ranges) = self.precedents_of_ast(key.sheet, ast);
            self.deps.set_precedents(*key, exact, ranges);
            if ast.is_volatile() {
                self.volatile.insert(*key);
            }
        }
        self.recalc(formula_keys.into_iter().map(|(k, _)| k).collect());
    }

    /// Incremental recalculation from seed cells: mark the transitive
    /// dependent closure dirty (plus volatile cells), evaluate in
    /// topological order, mark cycles #CIRC!. Returns cells whose computed
    /// value changed.
    pub fn recalc(&mut self, seeds: Vec<CellKey>) -> Vec<CellKey> {
        let mut changed = self.recalc_pass(seeds);

        // Blocks are laid out after the pass, in one deterministic sweep, and
        // whatever that moves is a value some other formula may have read.
        // Bounded like the loop below and for the same reason: a chain of
        // formulas reading each other's spilled cells settles in a few
        // rounds, and a sheet that does not settle is one where the
        // alternative to a bound is a hang.
        if self.has_spills() {
            for _ in 0..MAX_SPILL_PASSES {
                let moved = self.place_spills();
                if moved.is_empty() {
                    break;
                }
                for k in &moved {
                    if !changed.contains(k) {
                        changed.push(*k);
                    }
                }
                let again = self.recalc_pass(moved);
                for k in again {
                    if !changed.contains(&k) {
                        changed.push(k);
                    }
                }
            }
        }

        // A cell holding OFFSET or INDIRECT reads cells the dependency graph
        // never saw — that is what makes it volatile — so one topological pass
        // can evaluate it *before* the value it actually depends on, and leave
        // it holding a stale answer. Excel settles this by iterating; so does
        // this, seeded with what moved and bounded so a pathological sheet
        // cannot spin.
        //
        // Only reference-volatile cells need it. A clock or random function
        // recalculates every pass but reads nothing, so its answer is never
        // stale and an extra pass would buy nothing but a different random
        // number.
        if self.has_dynamic_references() {
            for _ in 0..MAX_DYNAMIC_REFERENCE_PASSES {
                let moved: Vec<CellKey> = changed
                    .iter()
                    .copied()
                    .filter(|k| !self.volatile.contains(k))
                    .collect();
                if moved.is_empty() {
                    break;
                }
                let again = self.recalc_pass(moved);
                let settled = again
                    .iter()
                    .all(|k| self.volatile.contains(k) && changed.contains(k));
                for k in again {
                    if !changed.contains(&k) {
                        changed.push(k);
                    }
                }
                if settled {
                    break;
                }
            }
        }
        changed
    }

    /// Whether any volatile cell computes its own references.
    ///
    /// Scanned rather than tracked: the volatile set is small by nature, and a
    /// second set to keep in step with it is a second thing to forget.
    fn has_dynamic_references(&self) -> bool {
        self.volatile.iter().any(|k| {
            self.wb
                .sheet(k.sheet)
                .and_then(|s| s.cells.get(&k.addr))
                .map(|c| match &c.content {
                    CellContent::Formula { ast, .. } => ast.has_dynamic_reference(),
                    _ => false,
                })
                .unwrap_or(false)
        })
    }

    fn recalc_pass(&mut self, seeds: Vec<CellKey>) -> Vec<CellKey> {
        // 1. Dirty closure.
        let mut dirty: HashSet<CellKey> = HashSet::new();
        let mut queue: Vec<CellKey> = Vec::new();
        for s in seeds.into_iter().chain(self.volatile.iter().copied()) {
            if dirty.insert(s) {
                queue.push(s);
            }
        }
        while let Some(k) = queue.pop() {
            for d in self.deps.dependents_of(k) {
                if dirty.insert(d) {
                    queue.push(d);
                }
            }
        }

        // 2. Restrict to formula cells; build the intra-dirty subgraph.
        let mut dirty_formulas: Vec<CellKey> = dirty
            .iter()
            .copied()
            .filter(|k| {
                self.wb
                    .sheet(k.sheet)
                    .and_then(|s| s.cells.get(&k.addr))
                    .map(|c| c.is_formula())
                    .unwrap_or(false)
            })
            .collect();
        dirty_formulas.sort();
        let dirty_set: HashSet<CellKey> = dirty_formulas.iter().copied().collect();

        let mut indeg: HashMap<CellKey, usize> = dirty_formulas.iter().map(|k| (*k, 0)).collect();
        let mut edges: HashMap<CellKey, Vec<CellKey>> = HashMap::new(); // precedent -> dependents
        for &d in &dirty_formulas {
            if let Some((exact, ranges)) = self.deps.precedents_of(d) {
                let mut precs: HashSet<CellKey> = HashSet::new();
                for p in exact {
                    if *p != d && dirty_set.contains(p) {
                        precs.insert(*p);
                    }
                }
                for (sid, range) in ranges {
                    for &x in &dirty_formulas {
                        if x != d && x.sheet == *sid && range.contains(x.addr) {
                            precs.insert(x);
                        }
                    }
                }
                // Self-references (A1 = A1+1) are cycles; keep the self edge
                // so the cell never enters the ready queue.
                let self_ref = exact.contains(&d)
                    || ranges
                        .iter()
                        .any(|(sid, r)| *sid == d.sheet && r.contains(d.addr));
                if self_ref {
                    edges.entry(d).or_default().push(d);
                    *indeg.get_mut(&d).unwrap() += 1;
                }
                for p in precs {
                    edges.entry(p).or_default().push(d);
                    *indeg.get_mut(&d).unwrap() += 1;
                }
            }
        }

        // 3. Kahn's algorithm in deterministic order.
        let mut ready: Vec<CellKey> = dirty_formulas
            .iter()
            .copied()
            .filter(|k| indeg[k] == 0)
            .collect();
        ready.sort();
        ready.reverse(); // pop from the end -> ascending order
        let mut changed: Vec<CellKey> = Vec::new();
        let mut processed: HashSet<CellKey> = HashSet::new();
        while let Some(k) = ready.pop() {
            processed.insert(k);
            if self.eval_and_store(k) {
                changed.push(k);
            }
            if let Some(deps) = edges.get(&k) {
                let mut newly: Vec<CellKey> = Vec::new();
                for d in deps.clone() {
                    let e = indeg.get_mut(&d).unwrap();
                    *e -= 1;
                    if *e == 0 {
                        newly.push(d);
                    }
                }
                newly.sort();
                for n in newly.into_iter().rev() {
                    ready.push(n);
                }
            }
        }

        // 4. Whatever Kahn could not order is a cycle or sits downstream of
        //    one. Only cells genuinely on a cycle become #CIRC!; the rest
        //    evaluate normally, reading #CIRC! from their precedents only
        //    where they actually use them. Distinguishing the two matters:
        //    otherwise `=IF(FALSE,circular,ok)` would report #CIRC! after a
        //    full recalculation but its real value after an incremental one,
        //    and the replay invariant would not hold.
        let remaining: Vec<CellKey> = dirty_formulas
            .iter()
            .copied()
            .filter(|k| !processed.contains(k))
            .collect();
        let cyclic = cyclic_nodes(&remaining, &edges);
        for &k in &remaining {
            if cyclic.contains(&k) && self.store_value(k, Value::Error(ErrorKind::Circ)) {
                changed.push(k);
            }
        }

        // 5. The survivors form a DAG once the cycles are pinned; evaluate
        //    them in topological order.
        let rest: Vec<CellKey> = remaining
            .iter()
            .copied()
            .filter(|k| !cyclic.contains(k))
            .collect();
        let rest_set: HashSet<CellKey> = rest.iter().copied().collect();
        let mut rest_indeg: HashMap<CellKey, usize> = rest.iter().map(|k| (*k, 0)).collect();
        for (from, tos) in &edges {
            if !rest_set.contains(from) {
                continue;
            }
            for to in tos {
                if let Some(e) = rest_indeg.get_mut(to) {
                    *e += 1;
                }
            }
        }
        let mut ready: Vec<CellKey> = rest
            .iter()
            .copied()
            .filter(|k| rest_indeg[k] == 0)
            .collect();
        ready.sort();
        ready.reverse();
        while let Some(k) = ready.pop() {
            if self.eval_and_store(k) {
                changed.push(k);
            }
            if let Some(deps) = edges.get(&k) {
                let mut newly: Vec<CellKey> = Vec::new();
                for d in deps.clone() {
                    if let Some(e) = rest_indeg.get_mut(&d) {
                        *e -= 1;
                        if *e == 0 {
                            newly.push(d);
                        }
                    }
                }
                newly.sort();
                for n in newly.into_iter().rev() {
                    ready.push(n);
                }
            }
        }

        // Legacy fallback: nothing should be left, but never leave a formula
        // cell holding a stale value.
        for &k in &dirty_formulas {
            if !processed.contains(&k)
                && !cyclic.contains(&k)
                && !rest_set.contains(&k)
                && self.store_value(k, Value::Error(ErrorKind::Circ))
            {
                changed.push(k);
            }
        }
        changed.sort();
        changed
    }

    /// Evaluate one formula cell and store the result; true if it changed.
    fn eval_and_store(&mut self, key: CellKey) -> bool {
        let Some(sheet) = self.wb.sheet(key.sheet) else {
            return false;
        };
        let Some(cell) = sheet.cells.get(&key.addr) else {
            return false;
        };
        let CellContent::Formula { ast, .. } = &cell.content else {
            return false;
        };
        let ctx = EvalCtx {
            wb: &self.wb,
            sheet: key.sheet,
            at: key.addr,
            now_ms: self.now_ms,
            bindings: &[],
            name_depth: 0,
        };
        let ast = ast.clone();
        let operand = ctx.eval_operand(&ast);
        // A block wider than one cell is recorded rather than placed. Where
        // it lands depends on what every *other* block is doing, so placement
        // is one deterministic sweep after the pass rather than a race
        // between formulas.
        let (value, array) = match operand {
            crate::eval::Operand::Array(a) if !a.is_single() => {
                // A block whose last placement was blocked keeps saying so.
                // Without this the value flips between the first element and
                // #SPILL! on alternate passes, and whichever pass ran last
                // wins — which is not a rule anybody could rely on.
                let head = if self.spill_blocked.contains(&key) {
                    Value::Error(ErrorKind::Spill)
                } else {
                    a.values.first().cloned().unwrap_or(Value::Empty)
                };
                (head, Some(a))
            }
            other => (ctx.scalar_of(other), None),
        };
        if let Some(s) = self.wb.sheet_mut(key.sheet) {
            match array {
                Some(a) => s.arrays.insert(key.addr, a),
                None => s.arrays.remove(&key.addr),
            };
        }
        self.store_value(key, value)
    }

    /// Place every block on the grid, and report the addresses whose value
    /// changed as a result.
    ///
    /// One sweep over all anchors in address order, on every sheet, clearing
    /// the overlay first. Deterministic by construction: two workbooks with
    /// the same blocks get the same layout however their formulas happened to
    /// be scheduled, which is what the replay invariant needs. Doing it
    /// inline as each formula evaluated would have made the layout depend on
    /// the dependency graph.
    fn place_spills(&mut self) -> Vec<CellKey> {
        let mut moved = Vec::new();
        let mut blocked_keys: HashSet<CellKey> = HashSet::new();
        for sheet in &mut self.wb.sheets {
            // An anchor that is no longer a formula left its block behind.
            sheet
                .arrays
                .retain(|addr, _| sheet.cells.get(addr).map(|c| c.is_formula()) == Some(true));

            let before = std::mem::take(&mut sheet.spill);
            let mut blocked: Vec<CellAddr> = Vec::new();
            let anchors: Vec<(CellAddr, (u32, u32))> = sheet
                .arrays
                .iter()
                .map(|(a, arr)| (*a, (arr.rows, arr.cols)))
                .collect();
            for (anchor, (rows, cols)) in anchors {
                let last_row = anchor.row as u64 + rows as u64 - 1;
                let last_col = anchor.col as u64 + cols as u64 - 1;
                let fits = last_row < crate::addr::MAX_ROWS as u64
                    && last_col < crate::addr::MAX_COLS as u64;
                let region: Vec<CellAddr> = if fits {
                    (anchor.row..=last_row as u32)
                        .flat_map(|r| {
                            (anchor.col..=last_col as u32).map(move |c| CellAddr::new(r, c))
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                // Anything already in the way stops the whole block: a
                // half-spilled array would be worse than an error, because
                // the user could not tell which half was real.
                let clear = fits
                    && region.iter().all(|a| {
                        *a == anchor
                            || (!sheet.cells.contains_key(a) && !sheet.spill.contains_key(a))
                    });
                if !clear {
                    blocked.push(anchor);
                    continue;
                }
                let arr = &sheet.arrays[&anchor];
                for (i, a) in region.into_iter().enumerate() {
                    if a == anchor {
                        continue;
                    }
                    sheet.spill.insert(a, (anchor, arr.values[i].clone()));
                }
            }

            for (addr, (_, v)) in &sheet.spill {
                if before.get(addr).map(|(_, b)| b) != Some(v) {
                    moved.push(CellKey {
                        sheet: sheet.id,
                        addr: *addr,
                    });
                }
            }
            for addr in before.keys() {
                if !sheet.spill.contains_key(addr) {
                    moved.push(CellKey {
                        sheet: sheet.id,
                        addr: *addr,
                    });
                }
            }

            // The anchor's value is settled here rather than at evaluation,
            // because whether the block fits is a fact about the whole sheet.
            // A blocked one says #SPILL! instead of showing its first element,
            // which would look like a working formula returning one value.
            let sid = sheet.id;
            let anchors: Vec<CellAddr> = sheet.arrays.keys().copied().collect();
            for anchor in anchors {
                let is_blocked = blocked.contains(&anchor);
                if is_blocked {
                    blocked_keys.insert(CellKey {
                        sheet: sid,
                        addr: anchor,
                    });
                }
                let want = if is_blocked {
                    Value::Error(ErrorKind::Spill)
                } else {
                    sheet.arrays[&anchor]
                        .values
                        .first()
                        .cloned()
                        .unwrap_or(Value::Empty)
                };
                if let Some(cell) = sheet.cells.get_mut(&anchor) {
                    if let CellContent::Formula { cached, .. } = &mut cell.content {
                        if *cached != want {
                            *cached = want;
                            moved.push(CellKey {
                                sheet: sid,
                                addr: anchor,
                            });
                        }
                    }
                }
            }
        }
        self.spill_blocked = blocked_keys;
        moved.sort();
        moved.dedup();
        moved
    }

    /// Whether any formula in the workbook produced a block.
    fn has_spills(&self) -> bool {
        self.wb.sheets.iter().any(|s| !s.arrays.is_empty())
    }

    fn store_value(&mut self, key: CellKey, v: Value) -> bool {
        let Some(sheet) = self.wb.sheet_mut(key.sheet) else {
            return false;
        };
        let Some(cell) = sheet.cells.get_mut(&key.addr) else {
            return false;
        };
        let CellContent::Formula { cached, .. } = &mut cell.content else {
            return false;
        };
        if *cached == v {
            false
        } else {
            *cached = v;
            true
        }
    }

    /// Convenience accessors used by tests and the UI layer.
    pub fn value_at(&self, sheet: &str, a1: &str) -> Value {
        let Some(s) = self.wb.sheet_by_name(sheet) else {
            return Value::Error(ErrorKind::Ref);
        };
        let Some(addr) = CellAddr::parse_a1(a1) else {
            return Value::Error(ErrorKind::Ref);
        };
        s.value(addr)
    }
}

/// Case-insensitive matching is ASCII-only, deliberately.
///
/// Full Unicode case folding changes byte lengths — `İ` lowercases to two
/// chars — so an offset found in a folded haystack does not point at the same
/// place in the original, and splicing a replacement at it corrupts the text.
/// `to_ascii_lowercase` maps only `A-Z`, so offsets stay valid for any input.
/// Users who need case-insensitive matching outside ASCII get exact matching
/// with "Match case" on rather than silently mangled cells.
fn ascii_fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Whether a cell's formula-bar text matches a search term.
fn text_matches(input: &str, find: &str, match_case: bool, whole_cell: bool) -> bool {
    match (whole_cell, match_case) {
        (true, true) => input == find,
        (true, false) => ascii_fold(input) == ascii_fold(find),
        (false, true) => input.contains(find),
        (false, false) => ascii_fold(input).contains(&ascii_fold(find)),
    }
}

/// The text a cell holds after a replacement. Substring mode replaces every
/// occurrence, as Excel's Replace All does within a cell.
fn replace_text(
    input: &str,
    find: &str,
    replace: &str,
    match_case: bool,
    whole_cell: bool,
) -> String {
    if whole_cell {
        return replace.to_string();
    }
    if match_case {
        return input.replace(find, replace);
    }
    let hay = ascii_fold(input);
    let needle = ascii_fold(find);
    debug_assert_eq!(
        hay.len(),
        input.len(),
        "ascii folding must preserve offsets"
    );
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while let Some(rel) = hay[i..].find(&needle) {
        let at = i + rel;
        out.push_str(&input[i..at]);
        out.push_str(replace);
        i = at + needle.len();
    }
    out.push_str(&input[i..]);
    out
}

/// Parse raw user input into a cell (formula, number, bool, error, or text).
fn build_cell(input: &str) -> Result<Cell, ApplyError> {
    if let Some(body) = input.strip_prefix('=') {
        let ast = parse_formula(body)?;
        // Formula text is stored as the user wrote it, with one exception:
        // xlsx stores post-2007 functions as `_xlfn.NAME`, which Excel hides.
        // Re-render from the AST in that case so the formula bar shows what
        // Excel would show rather than the storage form.
        let src = if body.to_uppercase().contains("_XLFN.") {
            ast.to_formula()
        } else {
            body.to_string()
        };
        return Ok(Cell {
            content: CellContent::Formula {
                src,
                ast,
                cached: Value::Empty,
            },
        });
    }
    // Leading apostrophe forces text.
    if let Some(text) = input.strip_prefix('\'') {
        return Ok(Cell::literal(Value::Text(text.to_string())));
    }
    if let Some(n) = crate::eval::parse_number_text(input) {
        return Ok(Cell::literal(Value::Number(n)));
    }
    if input.eq_ignore_ascii_case("TRUE") {
        return Ok(Cell::literal(Value::Bool(true)));
    }
    if input.eq_ignore_ascii_case("FALSE") {
        return Ok(Cell::literal(Value::Bool(false)));
    }
    if let Some(e) = ErrorKind::from_code(input) {
        return Ok(Cell::literal(Value::Error(e)));
    }
    Ok(Cell::literal(Value::Text(input.to_string())))
}

/// Replace or strip references to a sheet name across an expression.
fn rewrite_sheet_in_expr(e: &Expr, from: &str, to: Option<&str>, changed: &mut bool) -> Expr {
    match e {
        Expr::Cell(c) if sheet_matches(&c.sheet, from) => {
            *changed = true;
            match to {
                Some(t) => Expr::Cell(crate::ast::CellRef {
                    sheet: Some(t.to_string()),
                    r: c.r,
                }),
                None => Expr::Error(ErrorKind::Ref),
            }
        }
        Expr::Range(r) if sheet_matches(&r.sheet, from) => {
            *changed = true;
            match to {
                Some(t) => Expr::Range(crate::ast::RangeRef {
                    sheet: Some(t.to_string()),
                    start: r.start,
                    end: r.end,
                }),
                None => Expr::Error(ErrorKind::Ref),
            }
        }
        Expr::Func(name, args) => Expr::Func(
            name.clone(),
            args.iter()
                .map(|a| rewrite_sheet_in_expr(a, from, to, changed))
                .collect(),
        ),
        Expr::Binary(op, l, r) => Expr::Binary(
            *op,
            Box::new(rewrite_sheet_in_expr(l, from, to, changed)),
            Box::new(rewrite_sheet_in_expr(r, from, to, changed)),
        ),
        Expr::Neg(x) => Expr::Neg(Box::new(rewrite_sheet_in_expr(x, from, to, changed))),
        Expr::Pos(x) => Expr::Pos(Box::new(rewrite_sheet_in_expr(x, from, to, changed))),
        Expr::Percent(x) => Expr::Percent(Box::new(rewrite_sheet_in_expr(x, from, to, changed))),
        other => other.clone(),
    }
}

fn sheet_matches(sheet: &Option<String>, name: &str) -> bool {
    sheet
        .as_deref()
        .map(|s| s.eq_ignore_ascii_case(name))
        .unwrap_or(false)
}

/// The cells that genuinely lie on a dependency cycle: members of a strongly
/// connected component of more than one node, plus self-referencing cells.
/// Nodes merely reachable *from* a cycle are excluded, so they can still be
/// evaluated normally.
///
/// Iterative Tarjan — a recursive implementation would risk blowing the stack
/// on deep dependency chains in real workbooks.
fn cyclic_nodes(nodes: &[CellKey], edges: &HashMap<CellKey, Vec<CellKey>>) -> HashSet<CellKey> {
    let node_set: HashSet<CellKey> = nodes.iter().copied().collect();
    let mut index_of: HashMap<CellKey, u32> = HashMap::new();
    let mut low: HashMap<CellKey, u32> = HashMap::new();
    let mut on_stack: HashSet<CellKey> = HashSet::new();
    let mut stack: Vec<CellKey> = Vec::new();
    let mut next_index: u32 = 0;
    let mut cyclic: HashSet<CellKey> = HashSet::new();

    // Each frame tracks how many of the node's successors we have visited.
    let mut frames: Vec<(CellKey, usize)> = Vec::new();

    for &root in nodes {
        if index_of.contains_key(&root) {
            continue;
        }
        frames.push((root, 0));
        index_of.insert(root, next_index);
        low.insert(root, next_index);
        next_index += 1;
        stack.push(root);
        on_stack.insert(root);

        while let Some(&mut (v, ref mut child_i)) = frames.last_mut() {
            let successors = edges.get(&v).map(|s| s.as_slice()).unwrap_or(&[]);
            // Skip successors outside the subgraph under consideration.
            let mut advanced = false;
            while *child_i < successors.len() {
                let w = successors[*child_i];
                *child_i += 1;
                if !node_set.contains(&w) {
                    continue;
                }
                if w == v {
                    // A self-reference is a cycle of one.
                    cyclic.insert(v);
                    continue;
                }
                if let std::collections::hash_map::Entry::Vacant(slot) = index_of.entry(w) {
                    slot.insert(next_index);
                    low.insert(w, next_index);
                    next_index += 1;
                    stack.push(w);
                    on_stack.insert(w);
                    frames.push((w, 0));
                    advanced = true;
                    break;
                } else if on_stack.contains(&w) {
                    let lw = index_of[&w];
                    let lv = low[&v];
                    low.insert(v, lv.min(lw));
                }
            }
            if advanced {
                continue;
            }

            // All successors explored: close this node out.
            let (v, _) = frames.pop().expect("frame exists");
            if low[&v] == index_of[&v] {
                let mut component = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack.remove(&w);
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                if component.len() > 1 {
                    cyclic.extend(component);
                }
            }
            if let Some(&mut (parent, _)) = frames.last_mut() {
                let lv = low[&v];
                let lp = low[&parent];
                low.insert(parent, lp.min(lv));
            }
        }
    }
    cyclic
}

/// Whether a string can be a defined name.
///
/// Excel's rules, minus the ones that need locale data: it must not be
/// readable as a cell address (or `R`/`C`, which are R1C1 shorthand), must
/// start with a letter, underscore or backslash, and must contain no spaces
/// or operators. The address rule is the one that matters — a name spelled
/// `A1` would shadow the cell everywhere and there would be no way to say
/// which was meant.
fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_alphabetic() || first == '_' || first == '\\') {
        return false;
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\')
    {
        return false;
    }
    if name == "R" || name == "C" {
        return false;
    }
    CellAddr::parse_a1(name).is_none()
}
