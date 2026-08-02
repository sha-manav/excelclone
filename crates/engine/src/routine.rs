//! What a routine *is*, and how to run one safely.
//!
//! A routine is a macro of typed [`Action`]s. The engine owns the type and
//! the sandbox for the same reason it owns telemetry: three things need them
//! and they must agree. The miner discovers routines, the server stores them,
//! and the client runs them — a second definition of "what this macro does"
//! is a second thing to get wrong.
//!
//! Two properties are load-bearing:
//!
//! * **Running a routine goes through `Engine::apply` like everything else.**
//!   There is no second execution path, so a routine cannot do anything the
//!   user could not have done by hand, and every action it takes is captured
//!   like any other.
//! * **A routine keeps the coordinates it was recorded at**, plus the anchor
//!   it was recorded from, and is shifted once when it runs. Normalizing the
//!   actions to the origin first looks tidier and is quietly wrong:
//!   `=SUM(B5:D5)` written in E5 points three columns left, so moving it to
//!   A1 walks off the grid and the reference collapses to `#REF!` before it
//!   can be moved back.
//!
//! Shifting moves addresses *and* the relative references inside formulas, so
//! a routine recorded at row 5 and run at row 20 writes `=SUM(B20:D20)`.

use crate::addr::{CellAddr, RangeAddr};
use crate::engine::{Action, Engine, SortKey};
use serde::{Deserialize, Serialize};

/// A step the routine cannot perform because its value was redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// Where it goes, relative to wherever the routine is run.
    pub row_offset: i64,
    pub col_offset: i64,
    /// "number", "text", "bool" — the shape of what is missing.
    pub kind: String,
}

/// A runnable routine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    /// One line a person can read without knowing the vocabulary.
    pub summary: String,
    /// Actions with the coordinates they were recorded at.
    pub actions: Vec<Action>,
    /// Where they were recorded, so running elsewhere is one shift away.
    pub anchor: String,
    /// Values the routine cannot supply. Non-empty means partial.
    pub requires: Vec<Requirement>,
    pub support: usize,
    pub estimated_minutes_saved: f64,
    /// `loop` or `recurring`, so the panel can say why it is proposing this.
    pub kind: String,
}

impl Routine {
    pub fn is_partial(&self) -> bool {
        !self.requires.is_empty()
    }

    /// The actions this routine would apply at a given anchor.
    pub fn actions_at(&self, sheet: &str, anchor: CellAddr) -> Vec<Action> {
        let base = CellAddr::parse_a1(&self.anchor).unwrap_or(CellAddr::new(0, 0));
        let dr = anchor.row as i64 - base.row as i64;
        let dc = anchor.col as i64 - base.col as i64;
        self.actions
            .iter()
            .filter_map(|a| rebase(a, dr, dc))
            .map(|a| retarget_sheet(a, sheet))
            .collect()
    }
}

/// Shift an action by (dr, dc), including the relative references inside any
/// formula it carries. Returns None when the shift would leave the grid.
pub fn rebase(a: &Action, dr: i64, dc: i64) -> Option<Action> {
    let shift_addr = |x: CellAddr| -> Option<CellAddr> {
        let row = x.row as i64 + dr;
        let col = x.col as i64 + dc;
        (row >= 0 && col >= 0).then(|| CellAddr::new(row as u32, col as u32))
    };
    let shift_range = |r: RangeAddr| -> Option<RangeAddr> {
        Some(RangeAddr::new(shift_addr(r.start)?, shift_addr(r.end)?))
    };
    let shift_index = |i: u32, delta: i64| -> Option<u32> {
        let v = i as i64 + delta;
        (v >= 0).then_some(v as u32)
    };

    Some(match a.clone() {
        Action::CellEdit { sheet, addr, input } => Action::CellEdit {
            sheet,
            addr: shift_addr(addr)?,
            input: shift_formula(&input, dr, dc),
        },
        Action::CellClear { sheet, addr } => Action::CellClear {
            sheet,
            addr: shift_addr(addr)?,
        },
        Action::RangeClear { sheet, range } => Action::RangeClear {
            sheet,
            range: shift_range(range)?,
        },
        Action::FillApply {
            sheet,
            source,
            target,
        } => Action::FillApply {
            sheet,
            source: shift_range(source)?,
            target: shift_range(target)?,
        },
        Action::RowInsert { sheet, at, count } => Action::RowInsert {
            sheet,
            at: shift_index(at, dr)?,
            count,
        },
        Action::RowDelete { sheet, at, count } => Action::RowDelete {
            sheet,
            at: shift_index(at, dr)?,
            count,
        },
        Action::ColInsert { sheet, at, count } => Action::ColInsert {
            sheet,
            at: shift_index(at, dc)?,
            count,
        },
        Action::ColDelete { sheet, at, count } => Action::ColDelete {
            sheet,
            at: shift_index(at, dc)?,
            count,
        },
        Action::SortApply {
            sheet,
            range,
            keys,
            has_header,
        } => Action::SortApply {
            sheet,
            range: shift_range(range)?,
            keys: keys
                .into_iter()
                .map(|k| {
                    Some(SortKey {
                        column: shift_index(k.column, dc)?,
                        ascending: k.ascending,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            has_header,
        },
        Action::MergeApply { sheet, range } => Action::MergeApply {
            sheet,
            range: shift_range(range)?,
        },
        Action::MergeClear { sheet, range } => Action::MergeClear {
            sheet,
            range: shift_range(range)?,
        },
        Action::FormatApply {
            sheet,
            range,
            patches,
        } => Action::FormatApply {
            sheet,
            range: shift_range(range)?,
            patches,
        },
        Action::FormatClear { sheet, range } => Action::FormatClear {
            sheet,
            range: shift_range(range)?,
        },
        other => other,
    })
}

/// Shift the relative references inside a formula. Anything that is not a
/// formula, or that will not parse, is returned unchanged.
fn shift_formula(input: &str, dr: i64, dc: i64) -> String {
    let Some(body) = input.strip_prefix('=') else {
        return input.to_string();
    };
    match crate::parser::parse_formula(body) {
        Ok(ast) => format!("={}", crate::refs::offset(&ast, dr, dc).to_formula()),
        Err(_) => input.to_string(),
    }
}

/// Point an action at a different sheet.
///
/// Public because the dataset exporter replays a log whose sheet names were
/// hashed, and needs the same substitution a routine does.
pub fn retarget_sheet(a: Action, sheet: &str) -> Action {
    let s = sheet.to_string();
    match a {
        Action::CellEdit { addr, input, .. } => Action::CellEdit {
            sheet: s,
            addr,
            input,
        },
        Action::CellClear { addr, .. } => Action::CellClear { sheet: s, addr },
        Action::RangeClear { range, .. } => Action::RangeClear { sheet: s, range },
        Action::FillApply { source, target, .. } => Action::FillApply {
            sheet: s,
            source,
            target,
        },
        Action::RowInsert { at, count, .. } => Action::RowInsert {
            sheet: s,
            at,
            count,
        },
        Action::RowDelete { at, count, .. } => Action::RowDelete {
            sheet: s,
            at,
            count,
        },
        Action::ColInsert { at, count, .. } => Action::ColInsert {
            sheet: s,
            at,
            count,
        },
        Action::ColDelete { at, count, .. } => Action::ColDelete {
            sheet: s,
            at,
            count,
        },
        Action::SortApply {
            range,
            keys,
            has_header,
            ..
        } => Action::SortApply {
            sheet: s,
            range,
            keys,
            has_header,
        },
        Action::MergeApply { range, .. } => Action::MergeApply { sheet: s, range },
        Action::MergeClear { range, .. } => Action::MergeClear { sheet: s, range },
        Action::FormatApply { range, patches, .. } => Action::FormatApply {
            sheet: s,
            range,
            patches,
        },
        Action::FormatClear { range, .. } => Action::FormatClear { sheet: s, range },
        other => other,
    }
}

/// What a routine would change, without changing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DryRun {
    pub sheet: String,
    pub anchor: String,
    pub changes: Vec<CellChange>,
    /// Cells whose *formatting* the routine would change, described in
    /// words. Kept apart from `changes` because "B2 becomes bold" and "B2
    /// becomes 47" are different enough that folding them into one column
    /// would read as noise — but counted, because a routine that only
    /// formats still does something, and a preview that called it "no
    /// change" would disable Run on a routine that works.
    pub format_changes: Vec<CellChange>,
    /// Actions the engine refused, with its reason. A routine that cannot run
    /// cleanly must say so before the user presses Run, not after.
    pub errors: Vec<String>,
    pub requires: Vec<Requirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellChange {
    pub sheet: String,
    pub addr: String,
    pub before: String,
    pub after: String,
}

/// Apply a routine to a *copy* of the engine and report the difference.
///
/// The sandbox is a clone rather than an apply-then-undo: undo is itself
/// engine behaviour, and a preview that leaned on undo being correct could
/// not show the user a bug in undo.
pub fn dry_run(engine: &Engine, routine: &Routine, sheet: &str, anchor: CellAddr) -> DryRun {
    let before = engine.wb.state_snapshot();
    let mut sandbox = engine.clone();
    let mut errors = Vec::new();
    for action in routine.actions_at(sheet, anchor) {
        if let Err(e) = sandbox.apply(&action) {
            errors.push(e.to_string());
        }
    }
    let after = sandbox.wb.state_snapshot();

    DryRun {
        sheet: sheet.to_string(),
        anchor: anchor.to_a1(),
        changes: diff_snapshots(&before, &after),
        format_changes: diff_formats(&before, &after),
        errors,
        requires: routine.requires.clone(),
    }
}

/// Formatting differences between two snapshots, described in words.
fn diff_formats(before: &serde_json::Value, after: &serde_json::Value) -> Vec<CellChange> {
    let mut out = Vec::new();
    let empty = serde_json::Map::new();
    let sheets_of = |v: &serde_json::Value| -> Vec<SheetCells> {
        v["sheets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| {
                        (
                            s["name"].as_str().unwrap_or_default().to_string(),
                            s["formats"].as_object().cloned().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let before_sheets = sheets_of(before);
    let after_sheets = sheets_of(after);

    for (name, after_formats) in &after_sheets {
        let before_formats = before_sheets
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| c)
            .unwrap_or(&empty);
        let mut addrs: Vec<&String> = after_formats.keys().chain(before_formats.keys()).collect();
        addrs.sort_by_key(|a| CellAddr::parse_a1(a).unwrap_or(CellAddr::new(0, 0)));
        addrs.dedup();
        for addr in addrs {
            let b = before_formats.get(addr);
            let a = after_formats.get(addr);
            if a == b {
                continue;
            }
            out.push(CellChange {
                sheet: name.clone(),
                addr: addr.clone(),
                before: describe_format(b),
                after: describe_format(a),
            });
        }
    }
    out
}

/// A format as a short phrase. The snapshot omits defaults, so the keys that
/// are present are exactly the ones worth naming.
fn describe_format(f: Option<&serde_json::Value>) -> String {
    let Some(obj) = f.and_then(|v| v.as_object()) else {
        return "plain".to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    if obj.get("bold").and_then(|v| v.as_bool()) == Some(true) {
        parts.push("bold".into());
    }
    if obj.get("italic").and_then(|v| v.as_bool()) == Some(true) {
        parts.push("italic".into());
    }
    if let Some(c) = obj.get("font_color").and_then(|v| v.as_str()) {
        parts.push(format!("text {c}"));
    }
    if let Some(c) = obj.get("fill_color").and_then(|v| v.as_str()) {
        parts.push(format!("fill {c}"));
    }
    if obj.contains_key("borders") {
        parts.push("borders".into());
    }
    if let Some(code) = obj.get("number_format").and_then(|v| v.as_str()) {
        parts.push(code.to_string());
    }
    if let Some(a) = obj.get("align").and_then(|v| v.as_str()) {
        parts.push(format!("{a}-aligned"));
    }
    if parts.is_empty() {
        "plain".to_string()
    } else {
        parts.join(", ")
    }
}

type SheetCells = (String, serde_json::Map<String, serde_json::Value>);

/// Cell-level difference between two state snapshots, in reading order.
fn diff_snapshots(before: &serde_json::Value, after: &serde_json::Value) -> Vec<CellChange> {
    let mut out = Vec::new();
    let empty = serde_json::Map::new();
    let sheets_of = |v: &serde_json::Value| -> Vec<SheetCells> {
        v["sheets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|s| {
                        (
                            s["name"].as_str().unwrap_or_default().to_string(),
                            s["cells"].as_object().cloned().unwrap_or_default(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let before_sheets = sheets_of(before);
    let after_sheets = sheets_of(after);

    for (name, after_cells) in &after_sheets {
        let before_cells = before_sheets
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| c)
            .unwrap_or(&empty);
        let mut addrs: Vec<&String> = after_cells.keys().chain(before_cells.keys()).collect();
        addrs.sort_by_key(|a| CellAddr::parse_a1(a).unwrap_or(CellAddr::new(0, 0)));
        addrs.dedup();
        for addr in addrs {
            let b = before_cells
                .get(addr)
                .and_then(|c| c["value"].as_str())
                .unwrap_or("");
            let a = after_cells
                .get(addr)
                .and_then(|c| c["value"].as_str())
                .unwrap_or("");
            if a != b {
                out.push(CellChange {
                    sheet: name.clone(),
                    addr: addr.clone(),
                    before: b.to_string(),
                    after: a.to_string(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebasing_shifts_the_address_and_the_formula_together() {
        let a = Action::CellEdit {
            sheet: "S".into(),
            addr: CellAddr::parse_a1("E2").unwrap(),
            input: "=SUM(B2:D2)".into(),
        };
        match rebase(&a, 10, 0).unwrap() {
            Action::CellEdit { addr, input, .. } => {
                assert_eq!(addr.to_a1(), "E12");
                assert_eq!(input, "=SUM(B12:D12)");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rebasing_leaves_absolute_references_alone() {
        let a = Action::CellEdit {
            sheet: "S".into(),
            addr: CellAddr::parse_a1("E2").unwrap(),
            input: "=B2*$F$1".into(),
        };
        match rebase(&a, 5, 0).unwrap() {
            Action::CellEdit { input, .. } => assert_eq!(input, "=B7*$F$1"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rebasing_off_the_grid_is_refused_rather_than_clamped() {
        let a = Action::CellEdit {
            sheet: "S".into(),
            addr: CellAddr::parse_a1("A1").unwrap(),
            input: "1".into(),
        };
        assert!(rebase(&a, -1, 0).is_none());
    }

    #[test]
    fn rebasing_a_literal_does_not_mangle_it() {
        let a = Action::CellEdit {
            sheet: "S".into(),
            addr: CellAddr::parse_a1("A1").unwrap(),
            input: "A1 is fine as text".into(),
        };
        match rebase(&a, 3, 0).unwrap() {
            Action::CellEdit { input, .. } => assert_eq!(input, "A1 is fine as text"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_format_only_routine_is_not_reported_as_no_change() {
        // The bug this pins: the diff compared values only, so a routine that
        // just bolds a row previewed as "nothing would change" and the panel
        // disabled Run on a routine that works perfectly well.
        let mut engine = Engine::new();
        engine
            .apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1("A1").unwrap(),
                input: "total".into(),
            })
            .unwrap();
        let r = Routine {
            id: "rt_1".into(),
            summary: "bold it".into(),
            anchor: "A1".into(),
            actions: vec![Action::FormatApply {
                sheet: "<routine>".into(),
                range: RangeAddr::parse_a1("A1").unwrap(),
                patches: vec![crate::FormatPatch::Bold(true)],
            }],
            requires: Vec::new(),
            support: 5,
            estimated_minutes_saved: 3.0,
            kind: "loop".into(),
        };
        let preview = dry_run(&engine, &r, "Sheet1", CellAddr::parse_a1("A1").unwrap());
        assert!(preview.changes.is_empty(), "no value changes");
        assert_eq!(preview.format_changes.len(), 1);
        assert_eq!(preview.format_changes[0].addr, "A1");
        assert_eq!(preview.format_changes[0].before, "plain");
        assert_eq!(preview.format_changes[0].after, "bold");
    }

    #[test]
    fn a_format_change_is_described_in_words() {
        let f = serde_json::json!({
            "bold": true,
            "fill_color": "#eeeeee",
            "number_format": "$#,##0.00",
            "align": "center",
        });
        assert_eq!(
            describe_format(Some(&f)),
            "bold, fill #eeeeee, $#,##0.00, center-aligned"
        );
        assert_eq!(describe_format(None), "plain");
    }

    #[test]
    fn a_routine_shifts_from_its_own_anchor() {
        // The whole reason the anchor is stored: the actions keep the
        // coordinates they were recorded at, and the shift is the difference.
        let r = Routine {
            id: "rt_1".into(),
            summary: "test".into(),
            anchor: "E5".into(),
            actions: vec![Action::CellEdit {
                sheet: "<routine>".into(),
                addr: CellAddr::parse_a1("E5").unwrap(),
                input: "=SUM(B5:D5)".into(),
            }],
            requires: Vec::new(),
            support: 3,
            estimated_minutes_saved: 4.0,
            kind: "loop".into(),
        };
        match &r.actions_at("Ledger", CellAddr::parse_a1("E20").unwrap())[0] {
            Action::CellEdit { sheet, addr, input } => {
                assert_eq!(sheet, "Ledger");
                assert_eq!(addr.to_a1(), "E20");
                assert_eq!(input, "=SUM(B20:D20)");
            }
            other => panic!("{other:?}"),
        }
        // Run at its own anchor, it is the identity.
        assert_eq!(
            r.actions_at("<routine>", CellAddr::parse_a1("E5").unwrap()),
            r.actions
        );
    }

    #[test]
    fn a_routine_that_would_run_off_the_grid_drops_those_actions() {
        let r = Routine {
            id: "rt_1".into(),
            summary: "test".into(),
            anchor: "C3".into(),
            actions: vec![
                Action::CellEdit {
                    sheet: "S".into(),
                    addr: CellAddr::parse_a1("C3").unwrap(),
                    input: "1".into(),
                },
                Action::CellEdit {
                    sheet: "S".into(),
                    addr: CellAddr::parse_a1("A1").unwrap(),
                    input: "2".into(),
                },
            ],
            requires: Vec::new(),
            support: 3,
            estimated_minutes_saved: 4.0,
            kind: "loop".into(),
        };
        // Running at A1 shifts by (-2,-2), which puts the second action at
        // (-1,-1). Dropping it beats clamping it onto a cell the user did not
        // ask for.
        let at = r.actions_at("S", CellAddr::new(0, 0));
        assert_eq!(at.len(), 1);
    }
}
