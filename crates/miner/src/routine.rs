//! Discovering a routine from a mined pattern.
//!
//! The routine *type* and its sandbox live in the engine
//! ([`engine::routine`]), because the miner, the server and the client all
//! need them and they must agree. What lives here is the half that is
//! specific to mining: turning a scored pattern back into the concrete
//! actions that produced it.
//!
//! **A routine is built from a real occurrence, not from the tokens.** Tokens
//! are deliberately lossy — that is what makes mining work — so synthesizing
//! from them would mean inventing the details back. The most recent
//! occurrence is used as the template, on the grounds that the user's latest
//! way of doing something is the one most likely to still be right.
//!
//! What cannot be rebuilt is stated rather than guessed. Under `structural`
//! capture a typed literal is a hash, and no amount of cleverness recovers
//! the number: those steps become [`Requirement`]s the routine reports and
//! does not perform.

use engine::{Action, CellAddr, RangeAddr};

/// Re-exported so a caller mining routines does not also have to import the
/// engine to name what it got back.
pub use engine::{Requirement, Routine};

use crate::mine::{Pattern, PatternKind};
use crate::normalize::Step;
use crate::score::Scored;

/// Re-exported so callers can reach the sandbox without also importing the
/// engine directly; the definition lives there.
pub use engine::routine::dry_run;

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
        match rebuild(event) {
            Rebuilt::Action(a) => {
                if anchor.is_none() {
                    anchor = primary_addr(&a);
                }
                actions.push(a);
            }
            Rebuilt::Missing { addr, kind } => {
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
            Rebuilt::Skip => {}
        }
    }

    if actions.is_empty() {
        return None;
    }
    let base = anchor.unwrap_or(CellAddr::new(0, 0));

    Some(Routine {
        id: routine_id(&scored.pattern),
        summary: summarize(
            &scored.pattern,
            scored.minutes_saved,
            sittings(&scored.pattern, steps),
        ),
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

/// What an envelope turns back into.
///
/// Public because the dataset exporter needs the same reconstruction, and a
/// second copy of "how do I read a payload back into an action" would be a
/// second thing to get wrong.
pub enum Rebuilt {
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
pub fn rebuild(event: &serde_json::Value) -> Rebuilt {
    let action = event["action"].as_str().unwrap_or_default();
    let p = &event["payload"];
    let sheet = || String::from("<routine>");
    let addr_of = |key: &str| CellAddr::parse_a1(p[key].as_str().unwrap_or_default());
    let range_of = |key: &str| RangeAddr::parse_a1(p[key].as_str().unwrap_or_default());

    match action {
        "cell.edit" => {
            let Some(addr) = addr_of("addr") else {
                return Rebuilt::Skip;
            };
            // A formula is verbatim in every capture mode; a literal is only
            // verbatim under `full`, and arrives as an object otherwise.
            match p["input"].as_str() {
                Some(input) => Rebuilt::Action(Action::CellEdit {
                    sheet: sheet(),
                    addr,
                    input: input.to_string(),
                }),
                None => Rebuilt::Missing {
                    addr,
                    kind: p["input"]["type"].as_str().unwrap_or("value").to_string(),
                },
            }
        }
        "cell.clear" => match (addr_of("addr"), range_of("range")) {
            (Some(addr), _) => Rebuilt::Action(Action::CellClear {
                sheet: sheet(),
                addr,
            }),
            (None, Some(range)) => Rebuilt::Action(Action::RangeClear {
                sheet: sheet(),
                range,
            }),
            _ => Rebuilt::Skip,
        },
        "fill.apply" => match (range_of("source"), range_of("target")) {
            (Some(source), Some(target)) => Rebuilt::Action(Action::FillApply {
                sheet: sheet(),
                source,
                target,
            }),
            _ => Rebuilt::Skip,
        },
        "row.insert" | "row.delete" | "col.insert" | "col.delete" => {
            let at = p["at"].as_u64().unwrap_or(0) as u32;
            let count = p["count"].as_u64().unwrap_or(1) as u32;
            let s = sheet();
            Rebuilt::Action(match action {
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
                return Rebuilt::Skip;
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
                return Rebuilt::Skip;
            }
            Rebuilt::Action(Action::SortApply {
                sheet: sheet(),
                range,
                keys,
                has_header: p["has_header"].as_bool().unwrap_or(false),
            })
        }
        "format.apply" => {
            let Some(range) = range_of("range") else {
                return Rebuilt::Skip;
            };
            match p["kind"].as_str() {
                Some("merge") => Rebuilt::Action(Action::MergeApply {
                    sheet: sheet(),
                    range,
                }),
                Some("unmerge") => Rebuilt::Action(Action::MergeClear {
                    sheet: sheet(),
                    range,
                }),
                Some("clear") => Rebuilt::Action(Action::FormatClear {
                    sheet: sheet(),
                    range,
                }),
                _ => {
                    let patches: Vec<engine::FormatPatch> =
                        serde_json::from_value(p["patches"].clone()).unwrap_or_default();
                    if patches.is_empty() {
                        return Rebuilt::Skip;
                    }
                    Rebuilt::Action(Action::FormatApply {
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
        _ => Rebuilt::Skip,
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

/// A one-line description. Deliberately plain: the panel is asking someone to
/// trust a suggestion, and vocabulary names are not an explanation.
fn summarize(p: &Pattern, minutes: f64, sittings: usize) -> String {
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
        // "In a row" is a claim about one sitting. The same loop run on three
        // separate mornings is twelve repetitions but not twelve in a row, and
        // saying so would put a sentence in front of the user that did not
        // happen to them.
        PatternKind::Loop if sittings > 1 => {
            format!("repeated {} times across {sittings} sittings", p.support)
        }
        PatternKind::Loop => format!("repeated {} times in a row", p.support),
        PatternKind::Recurring => format!("seen in {} sessions", p.support),
    };
    format!(
        "{} — {repeated}, about {:.0} min of work",
        capitalize(&parts.join(", ")),
        minutes
    )
}

/// How many distinct sittings a pattern's occurrences fall in.
fn sittings(p: &Pattern, steps: &[Step]) -> usize {
    let mut seen: Vec<&str> = Vec::new();
    for occ in &p.occurrences {
        if let Some(step) = steps.get(occ.start) {
            if !seen.contains(&step.session_id.as_str()) {
                seen.push(&step.session_id);
            }
        }
    }
    seen.len().max(1)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mine::Occurrence;
    use crate::normalize::Token;
    use engine::routine::dry_run;
    use engine::{CellChange, Engine};
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
        let text = summarize(&p, 7.4, 1);
        assert!(text.starts_with("Enter 1 formula"), "{text}");
        assert!(text.contains("repeated 12 times in a row"), "{text}");
        assert!(text.contains("about 7 min"), "{text}");

        // The same loop run on three separate mornings is twelve repetitions
        // but not twelve in a row, and the sentence has to say which.
        let across = summarize(&p, 7.4, 3);
        assert!(
            across.contains("repeated 12 times across 3 sittings"),
            "{across}"
        );
        assert!(!across.contains("in a row"), "{across}");
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
