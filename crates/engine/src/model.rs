//! Workbook data model: ordered sheets over sparse cell stores.

use crate::addr::{CellAddr, RangeAddr};
use crate::ast::Expr;
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Merged regions; anchor (top-left) holds the value.
    pub merged: Vec<RangeAddr>,
}

impl Sheet {
    pub fn new(id: SheetId, name: impl Into<String>) -> Self {
        Sheet {
            id,
            name: name.into(),
            cells: HashMap::new(),
            merged: Vec::new(),
        }
    }

    pub fn value(&self, addr: CellAddr) -> Value {
        self.cells
            .get(&addr)
            .map(|c| c.value().clone())
            .unwrap_or(Value::Empty)
    }

    /// The bounding box of populated cells, if any.
    pub fn used_range(&self) -> Option<RangeAddr> {
        let mut it = self.cells.keys();
        let first = *it.next()?;
        let mut r = RangeAddr::single(first);
        for a in it {
            r.start.row = r.start.row.min(a.row);
            r.start.col = r.start.col.min(a.col);
            r.end.row = r.end.row.max(a.row);
            r.end.col = r.end.col.max(a.col);
        }
        Some(r)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    next_sheet_id: u32,
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
            next_sheet_id: 0,
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
                let mut merged: Vec<String> = s.merged.iter().map(|r| r.to_a1()).collect();
                merged.sort();
                serde_json::json!({ "name": s.name, "cells": cells, "merged": merged })
            })
            .collect();
        serde_json::json!({ "sheets": sheets })
    }
}
