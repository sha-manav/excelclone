//! The Engine: the single mutation path `apply(Action) -> Vec<Event>` plus
//! incremental recalculation.

use crate::addr::{CellAddr, RangeAddr};
use crate::ast::{Expr, RefVisit};
use crate::deps::DepGraph;
use crate::eval::EvalCtx;
use crate::model::{Cell, CellContent, CellKey, Sheet, SheetId, Workbook};
use crate::parser::parse_formula;
use crate::refs::Axis;
use crate::value::{ErrorKind, Value};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
    Sheets(Vec<Sheet>),
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
            Action::SheetAdd { name } => self.sheet_add(name),
            Action::SheetRename { from, to } => self.sheet_rename(from, to),
            Action::SheetDelete { name } => self.sheet_delete(name),
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
            UndoState::Sheets(sheets) => {
                let replaced = std::mem::replace(&mut self.wb.sheets, sheets);
                UndoState::Sheets(replaced)
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

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    fn range_clear(&mut self, sheet: &str, range: RangeAddr) -> Result<Vec<Event>, ApplyError> {
        let sid = self.sheet_id(sheet)?;
        let undo = self.op_range_clear(sid, range)?;
        let cleared = match &undo {
            UndoState::Cells(c) => c.len() as u32,
            _ => 0,
        };
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
        let filled = match &undo {
            UndoState::Cells(c) => c.len() as u32,
            _ => 0,
        };
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
            now_ms: self.now_ms,
        };
        let ast = ast.clone();
        let v = ctx.eval_scalar(&ast);
        self.store_value(key, v)
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
