//! Reference rewriting: the rules that keep formulas meaningful when cells
//! are copied, moved, or shifted by structural edits.
//!
//! Three distinct transforms, matching Excel:
//! - `offset`: copy/paste and fill. Relative refs shift by the paste delta,
//!   absolute refs (`$A$1`) stay put. Shifting off the grid gives `#REF!`.
//! - `structural`: insert/delete of rows or columns. Every ref in the
//!   workbook that points at the affected sheet is remapped; refs to deleted
//!   targets become `#REF!`, and ranges spanning a deletion shrink.
//! - `moved`: cut/paste. Refs *pointing at* the moved cells follow them,
//!   wherever in the workbook those refs live.

use crate::addr::RangeAddr;
use crate::ast::{CellRef, Expr, RangeRef};
use crate::model::SheetId;
use crate::value::ErrorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Row,
    Col,
}

/// An insert or delete of `count` rows/columns starting at index `at`.
#[derive(Debug, Clone, Copy)]
pub struct StructuralShift {
    pub sheet: SheetId,
    pub axis: Axis,
    pub at: u32,
    pub count: u32,
    pub insert: bool,
}

impl StructuralShift {
    /// Map one index; None means the index was deleted.
    pub fn map_index(&self, i: u32) -> Option<u32> {
        if self.insert {
            if i >= self.at {
                Some(i + self.count)
            } else {
                Some(i)
            }
        } else if i < self.at {
            Some(i)
        } else if i < self.at + self.count {
            None
        } else {
            Some(i - self.count)
        }
    }

    /// Map a range's span. None means the whole span was deleted.
    /// A deletion that removes part of the span shrinks it (Excel behavior).
    pub fn map_span(&self, start: u32, end: u32) -> Option<(u32, u32)> {
        if self.insert {
            // Inserting strictly inside a span widens it; inserting at the
            // start pushes the whole span down.
            let new_start = if start >= self.at {
                start + self.count
            } else {
                start
            };
            let new_end = if end >= self.at {
                end + self.count
            } else {
                end
            };
            return Some((new_start, new_end));
        }
        let del_start = self.at;
        let del_end = self.at + self.count - 1;
        if del_start <= start && del_end >= end {
            return None; // entire span deleted
        }
        let overlap = end.min(del_end).saturating_sub(start.max(del_start)) + 1;
        let overlap = if start.max(del_start) > end.min(del_end) {
            0
        } else {
            overlap
        };
        let new_start = if start > del_end {
            start - self.count
        } else {
            start
        };
        let new_end = end - overlap - if start > del_end { self.count } else { 0 };
        Some((new_start, new_end))
    }
}

/// A cut/paste: cells in `from` on `sheet` moved by (dr, dc).
#[derive(Debug, Clone, Copy)]
pub struct MoveShift {
    pub sheet: SheetId,
    pub from: RangeAddr,
    pub dr: i64,
    pub dc: i64,
}

/// Resolve the sheet a reference points at. `None` on the ref means the
/// sheet the formula itself lives on.
fn ref_sheet(
    sheet: &Option<String>,
    current: SheetId,
    lookup: &dyn Fn(&str) -> Option<SheetId>,
) -> Option<SheetId> {
    match sheet {
        None => Some(current),
        Some(n) => lookup(n),
    }
}

/// Copy/paste and fill: shift relative refs by (dr, dc).
pub fn offset(e: &Expr, dr: i64, dc: i64) -> Expr {
    map_refs(e, &mut |r| match r {
        RefKind::Cell(c) => match c.r.shifted(dr, dc) {
            Some(new) => Some(Expr::Cell(CellRef {
                sheet: c.sheet.clone(),
                r: new,
            })),
            None => Some(Expr::Error(ErrorKind::Ref)),
        },
        RefKind::Range(rr) => match (rr.start.shifted(dr, dc), rr.end.shifted(dr, dc)) {
            (Some(s), Some(en)) => Some(Expr::Range(RangeRef {
                sheet: rr.sheet.clone(),
                start: s,
                end: en,
            })),
            _ => Some(Expr::Error(ErrorKind::Ref)),
        },
    })
}

/// Insert/delete of rows or columns: remap every ref into the shifted sheet.
pub fn structural(
    e: &Expr,
    shift: &StructuralShift,
    current: SheetId,
    lookup: &dyn Fn(&str) -> Option<SheetId>,
) -> Expr {
    map_refs(e, &mut |r| match r {
        RefKind::Cell(c) => {
            if ref_sheet(&c.sheet, current, lookup) != Some(shift.sheet) {
                return None;
            }
            let idx = match shift.axis {
                Axis::Row => c.r.row,
                Axis::Col => c.r.col,
            };
            match shift.map_index(idx) {
                None => Some(Expr::Error(ErrorKind::Ref)),
                Some(new_idx) => {
                    let mut nr = c.r;
                    match shift.axis {
                        Axis::Row => nr.row = new_idx,
                        Axis::Col => nr.col = new_idx,
                    }
                    Some(Expr::Cell(CellRef {
                        sheet: c.sheet.clone(),
                        r: nr,
                    }))
                }
            }
        }
        RefKind::Range(rr) => {
            if ref_sheet(&rr.sheet, current, lookup) != Some(shift.sheet) {
                return None;
            }
            let (s, e2) = match shift.axis {
                Axis::Row => (rr.start.row.min(rr.end.row), rr.start.row.max(rr.end.row)),
                Axis::Col => (rr.start.col.min(rr.end.col), rr.start.col.max(rr.end.col)),
            };
            match shift.map_span(s, e2) {
                None => Some(Expr::Error(ErrorKind::Ref)),
                Some((ns, ne)) => {
                    let mut start = rr.start;
                    let mut end = rr.end;
                    match shift.axis {
                        Axis::Row => {
                            start.row = ns;
                            end.row = ne;
                        }
                        Axis::Col => {
                            start.col = ns;
                            end.col = ne;
                        }
                    }
                    Some(Expr::Range(RangeRef {
                        sheet: rr.sheet.clone(),
                        start,
                        end,
                    }))
                }
            }
        }
    })
}

/// Cut/paste: refs pointing into the moved block follow it. A range ref
/// follows only when it lies entirely inside the moved block (Excel leaves
/// partially-overlapping ranges alone).
pub fn moved(
    e: &Expr,
    mv: &MoveShift,
    current: SheetId,
    lookup: &dyn Fn(&str) -> Option<SheetId>,
) -> Expr {
    map_refs(e, &mut |r| match r {
        RefKind::Cell(c) => {
            if ref_sheet(&c.sheet, current, lookup) != Some(mv.sheet)
                || !mv.from.contains(c.r.addr())
            {
                return None;
            }
            // Absolute markers are preserved, but a moved target always
            // follows: the ref names a cell, and that cell relocated.
            let row = c.r.row as i64 + mv.dr;
            let col = c.r.col as i64 + mv.dc;
            if row < 0 || col < 0 {
                return Some(Expr::Error(ErrorKind::Ref));
            }
            let mut nr = c.r;
            nr.row = row as u32;
            nr.col = col as u32;
            Some(Expr::Cell(CellRef {
                sheet: c.sheet.clone(),
                r: nr,
            }))
        }
        RefKind::Range(rr) => {
            if ref_sheet(&rr.sheet, current, lookup) != Some(mv.sheet) {
                return None;
            }
            let span = RangeAddr::new(rr.start.addr(), rr.end.addr());
            if !mv.from.contains(span.start) || !mv.from.contains(span.end) {
                return None;
            }
            let (sr, sc) = (rr.start.row as i64 + mv.dr, rr.start.col as i64 + mv.dc);
            let (er, ec) = (rr.end.row as i64 + mv.dr, rr.end.col as i64 + mv.dc);
            if sr < 0 || sc < 0 || er < 0 || ec < 0 {
                return Some(Expr::Error(ErrorKind::Ref));
            }
            let mut start = rr.start;
            let mut end = rr.end;
            start.row = sr as u32;
            start.col = sc as u32;
            end.row = er as u32;
            end.col = ec as u32;
            Some(Expr::Range(RangeRef {
                sheet: rr.sheet.clone(),
                start,
                end,
            }))
        }
    })
}

pub enum RefKind<'a> {
    Cell(&'a CellRef),
    Range(&'a RangeRef),
}

/// Rebuild an expression, letting `f` replace any reference node. Returning
/// None from `f` leaves that reference untouched.
///
/// Public because it is the primitive the three transforms above are built
/// from, and a caller outside the engine needs a fourth: the dataset
/// generator has to grow a range whose bottom sat on the last row of the
/// data. Exposing the primitive is better than exposing a fourth
/// special-purpose function, and much better than the generator growing its
/// own expression walker that would drift from this one.
pub fn map_refs(e: &Expr, f: &mut impl FnMut(RefKind) -> Option<Expr>) -> Expr {
    match e {
        Expr::Cell(c) => f(RefKind::Cell(c)).unwrap_or_else(|| e.clone()),
        Expr::Range(r) => f(RefKind::Range(r)).unwrap_or_else(|| e.clone()),
        Expr::Func(name, args) => {
            Expr::Func(name.clone(), args.iter().map(|a| map_refs(a, f)).collect())
        }
        Expr::Binary(op, l, r) => {
            Expr::Binary(*op, Box::new(map_refs(l, f)), Box::new(map_refs(r, f)))
        }
        Expr::Neg(x) => Expr::Neg(Box::new(map_refs(x, f))),
        Expr::Pos(x) => Expr::Pos(Box::new(map_refs(x, f))),
        Expr::Percent(x) => Expr::Percent(Box::new(map_refs(x, f))),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_formula;

    fn rt(src: &str, f: impl Fn(&Expr) -> Expr) -> String {
        f(&parse_formula(src).unwrap()).to_formula()
    }

    #[test]
    fn offset_shifts_relative_keeps_absolute() {
        assert_eq!(rt("A1+B2", |e| offset(e, 1, 0)), "A2+B3");
        assert_eq!(rt("$A$1+A1", |e| offset(e, 2, 3)), "$A$1+D3");
        assert_eq!(rt("$A1+A$1", |e| offset(e, 1, 1)), "$A2+B$1");
        assert_eq!(rt("SUM(A1:A5)", |e| offset(e, 0, 1)), "SUM(B1:B5)");
        // Shifting off the grid is #REF!.
        assert_eq!(rt("A1", |e| offset(e, -1, 0)), "#REF!");
    }

    #[test]
    fn structural_insert_shifts_below() {
        let s = StructuralShift {
            sheet: SheetId(0),
            axis: Axis::Row,
            at: 2,
            count: 1,
            insert: true,
        };
        let look = |_: &str| None;
        // A1 is above the insert point; A5 moves down.
        assert_eq!(
            rt("A1+A5", |e| structural(e, &s, SheetId(0), &look)),
            "A1+A6"
        );
        // A range spanning the insert point widens.
        assert_eq!(
            rt("SUM(A1:A10)", |e| structural(e, &s, SheetId(0), &look)),
            "SUM(A1:A11)"
        );
    }

    #[test]
    fn structural_delete_remaps_and_breaks() {
        let s = StructuralShift {
            sheet: SheetId(0),
            axis: Axis::Row,
            at: 2,
            count: 2,
            insert: false,
        };
        let look = |_: &str| None;
        // Rows 3-4 (0-based 2-3) deleted: A1 stays, A6 -> A4.
        assert_eq!(
            rt("A1+A6", |e| structural(e, &s, SheetId(0), &look)),
            "A1+A4"
        );
        // A ref to a deleted row is #REF!.
        assert_eq!(rt("A3", |e| structural(e, &s, SheetId(0), &look)), "#REF!");
        // A range spanning the deletion shrinks.
        assert_eq!(
            rt("SUM(A1:A10)", |e| structural(e, &s, SheetId(0), &look)),
            "SUM(A1:A8)"
        );
        // A range entirely deleted becomes a #REF! argument, as in Excel.
        assert_eq!(
            rt("SUM(A3:A4)", |e| structural(e, &s, SheetId(0), &look)),
            "SUM(#REF!)"
        );
    }

    #[test]
    fn structural_ignores_other_sheets() {
        let s = StructuralShift {
            sheet: SheetId(1),
            axis: Axis::Row,
            at: 0,
            count: 5,
            insert: true,
        };
        let look = |_: &str| None;
        // Unqualified refs belong to sheet 0 here, so they are untouched.
        assert_eq!(rt("A1", |e| structural(e, &s, SheetId(0), &look)), "A1");
    }

    #[test]
    fn moved_refs_follow_cut_cells() {
        let mv = MoveShift {
            sheet: SheetId(0),
            from: RangeAddr::parse_a1("A1:A3").unwrap(),
            dr: 0,
            dc: 5,
        };
        let look = |_: &str| None;
        assert_eq!(rt("A1*2", |e| moved(e, &mv, SheetId(0), &look)), "F1*2");
        // Outside the moved block: untouched.
        assert_eq!(rt("A9", |e| moved(e, &mv, SheetId(0), &look)), "A9");
        // Fully-contained range follows; partial overlap does not.
        assert_eq!(
            rt("SUM(A1:A2)", |e| moved(e, &mv, SheetId(0), &look)),
            "SUM(F1:F2)"
        );
        assert_eq!(
            rt("SUM(A1:A9)", |e| moved(e, &mv, SheetId(0), &look)),
            "SUM(A1:A9)"
        );
    }

    #[test]
    fn span_mapping_edge_cases() {
        let del = StructuralShift {
            sheet: SheetId(0),
            axis: Axis::Row,
            at: 5,
            count: 3,
            insert: false,
        };
        // Deletion entirely after the span: unchanged.
        assert_eq!(del.map_span(0, 4), Some((0, 4)));
        // Deletion entirely before: shifts up by count.
        assert_eq!(del.map_span(10, 12), Some((7, 9)));
        // Overlapping tail.
        assert_eq!(del.map_span(0, 6), Some((0, 4)));
        // Fully covered.
        assert_eq!(del.map_span(5, 7), None);
        // Covers more than the span.
        assert_eq!(del.map_span(6, 6), None);
    }
}
