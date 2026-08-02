//! Turning a mined pattern into something the user can actually run.
//!
//! A routine is a JSON macro of typed engine `Action`s, shifted so it can be
//! applied anywhere. Two consequences follow, and both are deliberate:
//!
//! * **Running a routine goes through `Engine::apply` like everything else.**
//!   There is no second execution path, so a routine cannot do anything the
//!   user could not have done by hand, and every action it takes is captured
//!   like any other.
//! * **A routine is built from a real occurrence, not from the tokens.**
//!   Tokens are deliberately lossy — that is what makes mining work — so
//!   synthesizing from them would mean inventing the details back. Instead
//!   the most recent occurrence's actual actions are kept.
//!
//! Rebasing shifts addresses *and* the relative references inside formulas,
//! so a routine mined at row 5 and run at row 20 writes `=SUM(B20:D20)`
//! rather than `=SUM(B5:D5)`.
//!
//! The actions are stored with the coordinates they were recorded at, next to
//! the anchor they were recorded from, and shifted once by the difference
//! when the routine runs. Normalizing them to the origin first would be
//! tidier to look at and quietly wrong: `=SUM(B5:D5)` written in E5 points
//! three columns left, so moving it to A1 walks off the grid and the
//! reference collapses to `#REF!` before it can be moved back.
//!
//! What cannot be rebuilt is stated rather than guessed. Under `structural`
//! capture a typed literal is a hash, and no amount of cleverness recovers
//! the number: those steps become [`Requirement`]s the routine reports and
//! does not perform.

use engine::{Action, CellAddr, Engine, RangeAddr};
use serde::{Deserialize, Serialize};

use crate::mine::{Pattern, PatternKind};
use crate::normalize::Step;
use crate::score::Scored;

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

/// Build a routine from a scored pattern and the steps it was mined from.
///
/// Returns `None` when the occurrence yields nothing runnable — a pattern
/// made entirely of redacted literals is a real observation but not a
/// routine, and proposing an empty one would waste the user's attention.
pub fn synthesize(
    scored: &Scored,
    steps: &[Step],
    events: &[serde_json::Value],
) -> Option<Routine> {
    // The most recent occurrence: the user's latest way of doing it is the
    // one most likely to still be right.
    let occurrence = *scored.pattern.occurrences.iter().max_by_key(|o| o.start)?;
    let slice = steps.get(occurrence.start..occurrence.end.min(steps.len()))?;

    let mut actions: Vec<Action> = Vec::new();
    let mut requires: Vec<Requirement> = Vec::new();
    let mut anchor: Option<CellAddr> = None;

    for step in slice {
        let Some(event) = events.get(step.source) else {
            continue;
        };
        match reconstruct(event) {
            Reconstructed::Action(a) => {
                if anchor.is_none() {
                    anchor = primary_addr(&a);
                }
                actions.push(a);
            }
            Reconstructed::Missing { addr, kind } => {
                let base = anchor.unwrap_or(addr);
                if anchor.is_none() {
                    anchor = Some(addr);
                }
                requires.push(Requirement {
                    row_offset: addr.row as i64 - base.row as i64,
                    col_offset: addr.col as i64 - base.col as i64,
                    kind,
                });
            }
            Reconstructed::Skip => {}
        }
    }

    if actions.is_empty() {
        return None;
    }
    let base = anchor.unwrap_or(CellAddr::new(0, 0));

    Some(Routine {
        id: routine_id(&scored.pattern),
        summary: summarize(&scored.pattern, scored.minutes_saved),
        actions,
        anchor: base.to_a1(),
        requires,
        support: scored.pattern.support,
        estimated_minutes_saved: (scored.minutes_saved * 10.0).round() / 10.0,
        kind: match scored.pattern.kind {
            PatternKind::Loop => "loop".into(),
            PatternKind::Recurring => "recurring".into(),
        },
    })
}

/// A stable id for a pattern, so re-running the miner updates a routine
/// rather than proposing a second copy. Derived from the token shapes, which
/// is what makes two mining runs agree.
fn routine_id(p: &Pattern) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    for t in &p.tokens {
        t.to_string().hash(&mut h);
    }
    format!("rt_{:016x}", h.finish())
}

enum Reconstructed {
    Action(Action),
    /// A cell edit whose value the log does not contain.
    Missing {
        addr: CellAddr,
        kind: String,
    },
    /// Not part of a routine (navigation, undo, bookkeeping), or an action
    /// the log does not carry enough of to rebuild.
    Skip,
}

/// Rebuild an engine action from a captured envelope's payload.
///
/// The sheet name is a placeholder: it is hashed under `structural` and, more
/// to the point, a routine should run where the user is now rather than where
/// it was recorded. [`Routine::actions_at`] fills in the real one.
fn reconstruct(event: &serde_json::Value) -> Reconstructed {
    let action = event["action"].as_str().unwrap_or_default();
    let p = &event["payload"];
    let sheet = || String::from("<routine>");
    let addr_of = |key: &str| CellAddr::parse_a1(p[key].as_str().unwrap_or_default());
    let range_of = |key: &str| RangeAddr::parse_a1(p[key].as_str().unwrap_or_default());

    match action {
        "cell.edit" => {
            let Some(addr) = addr_of("addr") else {
                return Reconstructed::Skip;
            };
            // A formula is verbatim in every capture mode; a literal is only
            // verbatim under `full`, and arrives as an object otherwise.
            match p["input"].as_str() {
                Some(input) => Reconstructed::Action(Action::CellEdit {
                    sheet: sheet(),
                    addr,
                    input: input.to_string(),
                }),
                None => Reconstructed::Missing {
                    addr,
                    kind: p["input"]["type"].as_str().unwrap_or("value").to_string(),
                },
            }
        }
        "cell.clear" => match (addr_of("addr"), range_of("range")) {
            (Some(addr), _) => Reconstructed::Action(Action::CellClear {
                sheet: sheet(),
                addr,
            }),
            (None, Some(range)) => Reconstructed::Action(Action::RangeClear {
                sheet: sheet(),
                range,
            }),
            _ => Reconstructed::Skip,
        },
        "fill.apply" => match (range_of("source"), range_of("target")) {
            (Some(source), Some(target)) => Reconstructed::Action(Action::FillApply {
                sheet: sheet(),
                source,
                target,
            }),
            _ => Reconstructed::Skip,
        },
        "row.insert" | "row.delete" | "col.insert" | "col.delete" => {
            let at = p["at"].as_u64().unwrap_or(0) as u32;
            let count = p["count"].as_u64().unwrap_or(1) as u32;
            let s = sheet();
            Reconstructed::Action(match action {
                "row.insert" => Action::RowInsert {
                    sheet: s,
                    at,
                    count,
                },
                "row.delete" => Action::RowDelete {
                    sheet: s,
                    at,
                    count,
                },
                "col.insert" => Action::ColInsert {
                    sheet: s,
                    at,
                    count,
                },
                _ => Action::ColDelete {
                    sheet: s,
                    at,
                    count,
                },
            })
        }
        "sort.apply" => {
            let Some(range) = range_of("range") else {
                return Reconstructed::Skip;
            };
            let keys: Vec<engine::SortKey> = p["keys"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|k| engine::SortKey {
                            column: k["column"].as_u64().unwrap_or(0) as u32,
                            ascending: k["ascending"].as_bool().unwrap_or(true),
                        })
                        .collect()
                })
                .unwrap_or_default();
            if keys.is_empty() {
                return Reconstructed::Skip;
            }
            Reconstructed::Action(Action::SortApply {
                sheet: sheet(),
                range,
                keys,
                has_header: p["has_header"].as_bool().unwrap_or(false),
            })
        }
        "format.apply" => {
            let Some(range) = range_of("range") else {
                return Reconstructed::Skip;
            };
            match p["kind"].as_str() {
                Some("merge") => Reconstructed::Action(Action::MergeApply {
                    sheet: sheet(),
                    range,
                }),
                Some("unmerge") => Reconstructed::Action(Action::MergeClear {
                    sheet: sheet(),
                    range,
                }),
                Some("clear") => Reconstructed::Action(Action::FormatClear {
                    sheet: sheet(),
                    range,
                }),
                _ => {
                    let patches: Vec<engine::FormatPatch> =
                        serde_json::from_value(p["patches"].clone()).unwrap_or_default();
                    if patches.is_empty() {
                        return Reconstructed::Skip;
                    }
                    Reconstructed::Action(Action::FormatApply {
                        sheet: sheet(),
                        range,
                        patches,
                    })
                }
            }
        }
        // A paste needs a clipboard the log does not carry; a filter needs
        // its allowed values and a replacement its terms, both hashed. Each
        // is skipped rather than approximated — a routine that pasted the
        // wrong block would be worse than one that does not paste.
        _ => Reconstructed::Skip,
    }
}

/// The address a routine anchors on: the first address the action touches.
fn primary_addr(a: &Action) -> Option<CellAddr> {
    match a {
        Action::CellEdit { addr, .. } | Action::CellClear { addr, .. } => Some(*addr),
        Action::RangeClear { range, .. }
        | Action::SortApply { range, .. }
        | Action::MergeApply { range, .. }
        | Action::MergeClear { range, .. }
        | Action::FormatApply { range, .. }
        | Action::FormatClear { range, .. } => Some(range.start),
        Action::FillApply { source, .. } => Some(source.start),
        Action::RowInsert { at, .. } | Action::RowDelete { at, .. } => Some(CellAddr::new(*at, 0)),
        Action::ColInsert { at, .. } | Action::ColDelete { at, .. } => Some(CellAddr::new(0, *at)),
        _ => None,
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
                    Some(engine::SortKey {
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
    match engine::parser::parse_formula(body) {
        Ok(ast) => format!("={}", engine::refs::offset(&ast, dr, dc).to_formula()),
        Err(_) => input.to_string(),
    }
}

fn retarget_sheet(a: Action, sheet: &str) -> Action {
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

/// A one-line description. Deliberately plain: the panel is asking someone to
/// trust a suggestion, and vocabulary names are not an explanation.
fn summarize(p: &Pattern, minutes: f64) -> String {
    use crate::normalize::Token;
    let mut parts: Vec<String> = Vec::new();
    let mut formulas = 0;
    let mut literals = 0;
    let mut formats: Vec<&str> = Vec::new();
    let mut structural = 0;
    for t in &p.tokens {
        match t {
            Token::Formula { .. } => formulas += 1,
            Token::Literal { .. } => literals += 1,
            Token::Format { attribute } => {
                if !formats.contains(&attribute.as_str()) {
                    formats.push(attribute);
                }
            }
            Token::Fill { .. } => parts.push("fill down".into()),
            Token::Sort { .. } => parts.push("sort".into()),
            Token::FilterApply => parts.push("filter".into()),
            Token::RowInsert | Token::RowDelete | Token::ColInsert | Token::ColDelete => {
                structural += 1
            }
            _ => {}
        }
    }
    if formulas > 0 {
        parts.insert(0, format!("enter {formulas} formula{}", plural(formulas)));
    }
    if literals > 0 {
        parts.push(format!("type {literals} value{}", plural(literals)));
    }
    if !formats.is_empty() {
        parts.push(format!("apply {}", formats.join(" and ")));
    }
    if structural > 0 {
        parts.push(format!(
            "{structural} row/column change{}",
            plural(structural)
        ));
    }
    if parts.is_empty() {
        parts.push(format!("{} steps", p.tokens.len()));
    }
    let repeated = match p.kind {
        PatternKind::Loop => format!("repeated {} times in a row", p.support),
        PatternKind::Recurring => format!("seen in {} sessions", p.support),
    };
    format!(
        "{} — {repeated}, about {:.0} min of work",
        capitalize(&parts.join(", ")),
        minutes
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// What a routine would change, without changing it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DryRun {
    pub sheet: String,
    pub anchor: String,
    pub changes: Vec<CellChange>,
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
        errors,
        requires: routine.requires.clone(),
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
    use crate::mine::Occurrence;
    use crate::normalize::Token;
    use serde_json::json;

    fn step(source: usize) -> Step {
        Step {
            token: Token::Clear,
            source,
            session_id: "s".into(),
            ts_ms: 0,
        }
    }

    fn edit_event(addr: &str, input: serde_json::Value, is_formula: bool) -> serde_json::Value {
        json!({
            "action": "cell.edit",
            "payload": { "addr": addr, "input": input, "is_formula": is_formula },
        })
    }

    fn scored(tokens: Vec<Token>, occ: Vec<Occurrence>) -> Scored {
        let pattern = Pattern {
            support: occ.len(),
            occurrences: occ,
            tokens,
            kind: PatternKind::Loop,
        };
        crate::score::score(&pattern)
    }

    fn engine_with(cells: &[(&str, &str)]) -> Engine {
        let mut e = Engine::new();
        for (a, v) in cells {
            e.apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1(a).unwrap(),
                input: (*v).into(),
            })
            .unwrap();
        }
        e
    }

    /* ------------------------------------------------------------ rebase */

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

    /* -------------------------------------------------------- synthesize */

    #[test]
    fn a_routine_is_rebased_to_the_origin_and_runs_where_it_is_asked() {
        let events = vec![
            edit_event("E5", json!("=SUM(B5:D5)"), true),
            edit_event("F5", json!("=E5*2"), true),
        ];
        let s = scored(
            vec![
                Token::Formula { shape: "x".into() },
                Token::Formula { shape: "y".into() },
            ],
            vec![Occurrence { start: 0, end: 2 }],
        );
        let r = synthesize(&s, &[step(0), step(1)], &events).expect("a routine");

        // Stored where it was recorded...
        assert_eq!(r.anchor, "E5");
        match &r.actions[0] {
            Action::CellEdit { addr, input, .. } => {
                assert_eq!(addr.to_a1(), "E5");
                assert_eq!(input, "=SUM(B5:D5)");
            }
            other => panic!("{other:?}"),
        }
        // ...and shifted once, on the way out.
        let at = r.actions_at("Ledger", CellAddr::parse_a1("E20").unwrap());
        match &at[0] {
            Action::CellEdit { sheet, addr, input } => {
                assert_eq!(sheet, "Ledger");
                assert_eq!(addr.to_a1(), "E20");
                assert_eq!(input, "=SUM(B20:D20)");
            }
            other => panic!("{other:?}"),
        }
        match &at[1] {
            Action::CellEdit { addr, input, .. } => {
                assert_eq!(addr.to_a1(), "F20");
                assert_eq!(input, "=E20*2");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_redacted_literal_becomes_a_stated_requirement_not_a_guess() {
        let events = vec![
            edit_event("A1", json!("=B1*2"), true),
            edit_event(
                "B1",
                json!({ "hash": "deadbeef", "type": "number", "len": 4 }),
                false,
            ),
        ];
        let s = scored(
            vec![
                Token::Formula { shape: "x".into() },
                Token::Literal {
                    kind: "number".into(),
                },
            ],
            vec![Occurrence { start: 0, end: 2 }],
        );
        let r = synthesize(&s, &[step(0), step(1)], &events).expect("a routine");
        assert!(r.is_partial());
        assert_eq!(
            r.requires,
            vec![Requirement {
                row_offset: 0,
                col_offset: 1,
                kind: "number".into()
            }]
        );
        // The formula step still runs; only the unknown value is withheld.
        assert_eq!(r.actions.len(), 1);
    }

    #[test]
    fn a_pattern_with_nothing_runnable_is_not_proposed() {
        let events = vec![edit_event(
            "A1",
            json!({ "hash": "x", "type": "text", "len": 1 }),
            false,
        )];
        let s = scored(
            vec![Token::Literal {
                kind: "text".into(),
            }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        assert!(synthesize(&s, &[step(0)], &events).is_none());
    }

    #[test]
    fn the_most_recent_occurrence_is_the_template() {
        let events = vec![
            edit_event("A1", json!("=1+1"), true),
            edit_event("A9", json!("=2+2"), true),
        ];
        let s = scored(
            vec![Token::Formula { shape: "x".into() }],
            vec![
                Occurrence { start: 0, end: 1 },
                Occurrence { start: 1, end: 2 },
            ],
        );
        let r = synthesize(&s, &[step(0), step(1)], &events).unwrap();
        match &r.actions[0] {
            Action::CellEdit { input, .. } => assert_eq!(input, "=2+2"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_id_is_stable_across_runs_and_differs_between_patterns() {
        let a = scored(
            vec![Token::Formula { shape: "x".into() }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        let b = scored(
            vec![Token::Formula { shape: "y".into() }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        let events = vec![edit_event("A1", json!("=1+1"), true)];
        let ra = synthesize(&a, &[step(0)], &events).unwrap();
        let ra2 = synthesize(&a, &[step(0)], &events).unwrap();
        let rb = synthesize(&b, &[step(0)], &events).unwrap();
        assert_eq!(ra.id, ra2.id);
        assert_ne!(ra.id, rb.id);
    }

    #[test]
    fn a_format_step_survives_synthesis_with_its_patches() {
        let events = vec![json!({
            "action": "format.apply",
            "payload": {
                "range": "A1:C1",
                "kind": "style",
                "attributes": ["bold"],
                "patches": [{ "set": "bold", "value": true }],
            }
        })];
        let s = scored(
            vec![Token::Format {
                attribute: "bold".into(),
            }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        let r = synthesize(&s, &[step(0)], &events).unwrap();
        match &r.actions[0] {
            Action::FormatApply { range, patches, .. } => {
                assert_eq!(range.to_a1(), "A1:C1");
                assert_eq!(patches.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_paste_is_skipped_rather_than_approximated() {
        // The log records the shape of a paste, not the clipboard. Guessing
        // would produce a routine that quietly pastes the wrong block.
        let events = vec![
            json!({ "action": "range.paste", "payload": { "source": "A1:A3", "target": "B1:B3" } }),
            edit_event("C1", json!("=1+1"), true),
        ];
        let s = scored(
            vec![
                Token::Paste {
                    values: false,
                    cut: false,
                },
                Token::Formula { shape: "x".into() },
            ],
            vec![Occurrence { start: 0, end: 2 }],
        );
        let r = synthesize(&s, &[step(0), step(1)], &events).unwrap();
        assert_eq!(r.actions.len(), 1);
    }

    /* ------------------------------------------------------------ dry run */

    #[test]
    fn a_dry_run_reports_the_changes_without_making_them() {
        let engine = engine_with(&[("B1", "2"), ("C1", "3"), ("D1", "4")]);
        let events = vec![edit_event("E1", json!("=SUM(B1:D1)"), true)];
        let s = scored(
            vec![Token::Formula { shape: "x".into() }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        let r = synthesize(&s, &[step(0)], &events).unwrap();

        let preview = dry_run(&engine, &r, "Sheet1", CellAddr::parse_a1("E1").unwrap());
        assert_eq!(
            preview.changes,
            vec![CellChange {
                sheet: "Sheet1".into(),
                addr: "E1".into(),
                before: String::new(),
                after: "9".into(),
            }]
        );
        assert!(preview.errors.is_empty());
        // The real engine is untouched.
        assert_eq!(engine.value_at("Sheet1", "E1"), engine::Value::Empty);
    }

    #[test]
    fn a_dry_run_surfaces_what_the_engine_would_refuse() {
        let engine = engine_with(&[("A1", "1")]);
        let events = vec![edit_event("A1", json!("=1+1"), true)];
        let s = scored(
            vec![Token::Formula { shape: "x".into() }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        let r = synthesize(&s, &[step(0)], &events).unwrap();
        let preview = dry_run(&engine, &r, "NoSuchSheet", CellAddr::new(0, 0));
        assert!(preview.changes.is_empty());
        assert!(
            preview.errors.iter().any(|e| e.contains("NoSuchSheet")),
            "{:?}",
            preview.errors
        );
    }

    #[test]
    fn a_dry_run_sees_downstream_recalculation_too() {
        // The preview has to show every cell that changes, not just the ones
        // the routine writes — that is the whole reason it exists.
        let mut engine = engine_with(&[("A1", "1")]);
        engine
            .apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1("C1").unwrap(),
                input: "=B1*10".into(),
            })
            .unwrap();
        let events = vec![edit_event("B1", json!("=A1+4"), true)];
        let s = scored(
            vec![Token::Formula { shape: "x".into() }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        let r = synthesize(&s, &[step(0)], &events).unwrap();

        let preview = dry_run(&engine, &r, "Sheet1", CellAddr::parse_a1("B1").unwrap());
        let addrs: Vec<&str> = preview.changes.iter().map(|c| c.addr.as_str()).collect();
        assert_eq!(addrs, vec!["B1", "C1"]);
        assert_eq!(preview.changes[1].after, "50");
    }

    #[test]
    fn the_summary_reads_as_a_sentence() {
        let p = Pattern {
            tokens: vec![
                Token::Formula { shape: "x".into() },
                Token::Fill { down: true },
                Token::Format {
                    attribute: "bold".into(),
                },
            ],
            support: 12,
            occurrences: vec![Occurrence { start: 0, end: 3 }],
            kind: PatternKind::Loop,
        };
        let text = summarize(&p, 7.4);
        assert!(text.starts_with("Enter 1 formula"), "{text}");
        assert!(text.contains("repeated 12 times in a row"), "{text}");
        assert!(text.contains("about 7 min"), "{text}");
    }

    #[test]
    fn a_routine_round_trips_through_json() {
        // The body is stored as JSON in the routines table and re-read by the
        // client, so the macro has to survive serialization exactly.
        let events = vec![edit_event("E5", json!("=SUM(B5:D5)"), true)];
        let s = scored(
            vec![Token::Formula { shape: "x".into() }],
            vec![Occurrence { start: 0, end: 1 }],
        );
        let r = synthesize(&s, &[step(0)], &events).unwrap();
        let text = serde_json::to_string(&r).unwrap();
        let back: Routine = serde_json::from_str(&text).unwrap();
        assert_eq!(back, r);
    }
}
