//! Workbook data model: ordered sheets over sparse cell stores.

use crate::addr::{CellAddr, RangeAddr};
use crate::ast::Expr;
use crate::format::{FormatId, FormatTable};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Stable sheet identifier: survives renames and reorders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SheetId(pub u32);

/// Fully-qualified cell key used across the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellKey {
    pub sheet: SheetId,
    pub addr: CellAddr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CellContent {
    /// A literal value typed or imported directly.
    Literal(Value),
    /// A formula: original source (without '='), parsed AST, cached value.
    Formula {
        src: String,
        ast: Expr,
        cached: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub content: CellContent,
}

impl Cell {
    pub fn literal(v: Value) -> Self {
        Cell {
            content: CellContent::Literal(v),
        }
    }

    /// The computed value of the cell.
    pub fn value(&self) -> &Value {
        match &self.content {
            CellContent::Literal(v) => v,
            CellContent::Formula { cached, .. } => cached,
        }
    }

    /// The text the user would see in the formula bar.
    pub fn input(&self) -> String {
        match &self.content {
            CellContent::Literal(v) => v.display(),
            CellContent::Formula { src, .. } => format!("={}", src),
        }
    }

    pub fn is_formula(&self) -> bool {
        matches!(self.content, CellContent::Formula { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sheet {
    pub id: SheetId,
    pub name: String,
    pub cells: HashMap<CellAddr, Cell>,
    /// Presentation, keyed by address and independent of whether the cell
    /// holds anything. Ordered so iteration — and therefore export and the
    /// state snapshot — is deterministic. Only non-default formats appear.
    #[serde(default)]
    pub formats: BTreeMap<CellAddr, FormatId>,
    /// Merged regions; anchor (top-left) holds the value.
    pub merged: Vec<RangeAddr>,
    /// Active value filter, if any.
    #[serde(default)]
    pub filter: Option<crate::engine::FilterSpec>,
    /// Rows hidden by the active filter, ascending. View state only.
    #[serde(default)]
    pub hidden_rows: Vec<u32>,
    /// Column widths in pixels, for the columns that are not the default.
    ///
    /// In the model rather than in the grid's React state, because a width is
    /// something the user *did*: it has to survive a save, and it has to be
    /// visible to the capture pipeline. Only non-default entries are stored,
    /// so an untouched sheet carries none.
    #[serde(default)]
    pub col_widths: BTreeMap<u32, f64>,
    /// Row heights in pixels, on the same terms.
    #[serde(default)]
    pub row_heights: BTreeMap<u32, f64>,
    /// Rows and columns held still while the rest of the sheet scrolls.
    ///
    /// View state, like `hidden_rows` — it changes nothing about any value —
    /// but it is something the user *did*, so it lives on the single mutation
    /// path and is written to the file rather than kept in the browser.
    #[serde(default)]
    pub frozen_rows: u32,
    #[serde(default)]
    pub frozen_cols: u32,
    /// Blocks produced by formulas on this sheet, keyed by the cell that
    /// produced them.
    ///
    /// Derived: recalculation rebuilds it, so it is not serialized and a
    /// workbook read back from disk is recalculated before anyone looks.
    #[serde(skip)]
    pub arrays: BTreeMap<CellAddr, crate::eval::Array>,
    /// Where each spilled value landed: address -> (the anchor that put it
    /// there, the value). Derived from `arrays` in one deterministic sweep,
    /// so two paths to the same workbook place spills identically even when
    /// they evaluated the formulas in different orders.
    #[serde(skip)]
    pub spill: BTreeMap<CellAddr, (CellAddr, Value)>,
}

impl Sheet {
    pub fn new(id: SheetId, name: impl Into<String>) -> Self {
        Sheet {
            id,
            name: name.into(),
            cells: HashMap::new(),
            formats: BTreeMap::new(),
            merged: Vec::new(),
            filter: None,
            hidden_rows: Vec::new(),
            col_widths: BTreeMap::new(),
            row_heights: BTreeMap::new(),
            frozen_rows: 0,
            frozen_cols: 0,
            arrays: BTreeMap::new(),
            spill: BTreeMap::new(),
        }
    }

    pub fn value(&self, addr: CellAddr) -> Value {
        if let Some(c) = self.cells.get(&addr) {
            return c.value().clone();
        }
        // Spilled values have no cell of their own. Checked after `cells`
        // because the anchor is a real cell and holds the first element.
        self.spill
            .get(&addr)
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Empty)
    }

    /// The anchor whose block put a value here, if this cell is spilled into.
    pub fn spill_anchor(&self, addr: CellAddr) -> Option<CellAddr> {
        self.spill.get(&addr).map(|(a, _)| *a)
    }

    /// The bounding box of populated cells, if any.
    ///
    /// Deliberately blind to formatting: this is the *data* extent, and it is
    /// what CSV export and whole-sheet formula ranges mean. A bold empty
    /// column is not data.
    pub fn used_range(&self) -> Option<RangeAddr> {
        // Spilled cells are data: they are what the user sees, and leaving
        // them out would make Ctrl+Down stop at the anchor and CSV export
        // drop everything below it.
        bounds(self.cells.keys().copied().chain(self.spill.keys().copied()))
    }

    /// The bounding box of everything the grid has to draw — cells, formats
    /// and merges. Larger than [`Sheet::used_range`] when the user has
    /// formatted or merged cells they have not typed into yet.
    pub fn painted_range(&self) -> Option<RangeAddr> {
        bounds(
            self.cells
                .keys()
                .copied()
                .chain(self.spill.keys().copied())
                .chain(self.formats.keys().copied())
                .chain(self.merged.iter().flat_map(|m| [m.start, m.end])),
        )
    }

    /// The format id at an address, if the cell carries one.
    pub fn format_id(&self, addr: CellAddr) -> Option<FormatId> {
        self.formats.get(&addr).copied()
    }
}

fn bounds(addrs: impl Iterator<Item = CellAddr>) -> Option<RangeAddr> {
    let mut it = addrs;
    let first = it.next()?;
    let mut r = RangeAddr::single(first);
    for a in it {
        r.start.row = r.start.row.min(a.row);
        r.start.col = r.start.col.min(a.col);
        r.end.row = r.end.row.max(a.row);
        r.end.col = r.end.col.max(a.col);
    }
    Some(r)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// The palette every `Sheet::formats` entry indexes into. Workbook-level
    /// rather than per-sheet so a format survives a cut-and-paste across
    /// sheets, and append-only so ids recorded for undo stay valid.
    #[serde(default)]
    pub formats: FormatTable,
    next_sheet_id: u32,
    /// Defined names, uppercased, mapped to what they refer to — an A1
    /// range, usually sheet-qualified and absolute (`Sheet1!$A$1:$B$4`),
    /// exactly as xlsx spells it.
    ///
    /// The text rather than a parsed range, because that is what round-trips:
    /// a name can refer to things this engine does not model, and keeping the
    /// string means writing back what was read rather than an approximation
    /// of it. Evaluation parses it on use.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    /// The original xlsx package this workbook was imported from, kept so
    /// export can patch only the parts we model and write everything else
    /// back unchanged. Bulk binary: never serialized.
    #[serde(default, skip)]
    pub preserved: Option<crate::io::xlsx::PreservedPackage>,
}

impl Default for Workbook {
    fn default() -> Self {
        Self::new()
    }
}

impl Workbook {
    /// An empty workbook with a single "Sheet1".
    pub fn new() -> Self {
        let mut wb = Workbook {
            sheets: Vec::new(),
            formats: FormatTable::default(),
            next_sheet_id: 0,
            names: BTreeMap::new(),
            preserved: None,
        };
        wb.add_sheet("Sheet1");
        wb
    }

    pub fn add_sheet(&mut self, name: impl Into<String>) -> SheetId {
        let id = SheetId(self.next_sheet_id);
        self.next_sheet_id += 1;
        self.sheets.push(Sheet::new(id, name));
        id
    }

    pub fn sheet(&self, id: SheetId) -> Option<&Sheet> {
        self.sheets.iter().find(|s| s.id == id)
    }

    pub fn sheet_mut(&mut self, id: SheetId) -> Option<&mut Sheet> {
        self.sheets.iter_mut().find(|s| s.id == id)
    }

    /// Case-insensitive sheet lookup by name (Excel semantics).
    pub fn sheet_by_name(&self, name: &str) -> Option<&Sheet> {
        self.sheets
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }

    pub fn sheet_id_by_name(&self, name: &str) -> Option<SheetId> {
        self.sheet_by_name(name).map(|s| s.id)
    }

    pub fn value(&self, key: CellKey) -> Value {
        self.sheet(key.sheet)
            .map(|s| s.value(key.addr))
            .unwrap_or(Value::Error(crate::value::ErrorKind::Ref))
    }

    /// Deterministic serialization of computed state, used by replay tests
    /// and state digests: sheets in order, cells sorted by address.
    pub fn state_snapshot(&self) -> serde_json::Value {
        let sheets: Vec<serde_json::Value> = self
            .sheets
            .iter()
            .map(|s| {
                let mut cells: Vec<(&CellAddr, &Cell)> = s.cells.iter().collect();
                cells.sort_by_key(|(a, _)| **a);
                let cells: serde_json::Map<String, serde_json::Value> = cells
                    .into_iter()
                    .map(|(a, c)| {
                        (
                            a.to_a1(),
                            serde_json::json!({
                                "input": c.input(),
                                "value": c.value().display(),
                            }),
                        )
                    })
                    .collect();
                // Formats resolve to their values rather than their ids. An
                // id is an artefact of the order formats happened to be
                // interned, which differs between two paths to the same
                // workbook — exactly the difference the replay suite must
                // *not* see.
                let formats: serde_json::Map<String, serde_json::Value> = s
                    .formats
                    .iter()
                    .map(|(a, id)| {
                        (
                            a.to_a1(),
                            serde_json::to_value(self.formats.resolve(Some(*id)))
                                .unwrap_or(serde_json::Value::Null),
                        )
                    })
                    .collect();
                let mut merged: Vec<String> = s.merged.iter().map(|r| r.to_a1()).collect();
                merged.sort();
                let mut hidden = s.hidden_rows.clone();
                hidden.sort_unstable();
                // Sizes as runs rather than one entry per index. A file
                // saying "every column is 90 wide" imports as sixteen
                // thousand entries, and a snapshot is meant to be read.
                serde_json::json!({
                    "name": s.name,
                    "cells": cells,
                    "formats": formats,
                    "merged": merged,
                    "hidden_rows": hidden,
                    "frozen": [s.frozen_rows, s.frozen_cols],
                    "col_widths": size_runs(&s.col_widths),
                    "row_heights": size_runs(&s.row_heights),
                    // Spilled cells are state the user can see and formulas
                    // can read, so a snapshot that left them out would call
                    // two different workbooks identical.
                    "spill": s
                        .spill
                        .iter()
                        .map(|(a, (anchor, v))| {
                            (
                                a.to_a1(),
                                serde_json::json!({
                                    "from": anchor.to_a1(),
                                    "value": v.display(),
                                }),
                            )
                        })
                        .collect::<serde_json::Map<String, serde_json::Value>>(),
                })
            })
            .collect();
        serde_json::json!({ "sheets": sheets })
    }
}

/// Collapse a size map into `[first, last, pixels]` runs.
fn size_runs(sizes: &BTreeMap<u32, f64>) -> Vec<serde_json::Value> {
    let mut runs: Vec<(u32, u32, f64)> = Vec::new();
    for (&i, &px) in sizes {
        match runs.last_mut() {
            Some((_, end, size)) if *end + 1 == i && *size == px => *end = i,
            _ => runs.push((i, i, px)),
        }
    }
    runs.into_iter()
        .map(|(a, b, px)| serde_json::json!([a, b, px]))
        .collect()
}
