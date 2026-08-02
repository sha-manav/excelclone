//! The flagship invariant: **replaying a log reproduces the exact state**.
//!
//! Everything else in Gridline rests on this. Undo is a recorded state
//! restore, routine replay re-runs mined actions through `apply`, and the
//! exported demonstration dataset claims that a given action sequence
//! produces a given result. If replay could diverge — even in one obscure
//! corner — all three would be quietly lying.
//!
//! Three levels of assurance here:
//!   1. recorded fixture logs in `fixtures/logs/` replay byte-identically
//!   2. property-based: 500 random action sequences, applied live vs replayed
//!   3. the awkward cases that property generators rarely reach on their own
//!
//! Fixture logs are JSON arrays of `Action`. Each has a sibling
//! `<name>.final.json` holding the expected final state; run with
//! `UPDATE_GOLDEN=1` to regenerate after an intentional change.

use engine::{Action, CellAddr, Engine, PasteMode, RangeAddr, SortKey};
use proptest::prelude::*;
use std::path::PathBuf;

fn logs_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures")
        .join("logs")
}

/// Apply a log to a fresh engine. The clock is pinned so volatile functions
/// are part of the determinism guarantee rather than an excuse against it.
fn replay(actions: &[Action]) -> Engine {
    let mut e = Engine::new();
    e.now_ms = 1_704_110_400_000;
    for a in actions {
        // Rejected actions are part of the log's history too: replay must
        // reject exactly the same ones, so the outcome is ignored but the
        // attempt is not skipped.
        let _ = e.apply(a);
    }
    e
}

fn a1(s: &str) -> CellAddr {
    CellAddr::parse_a1(s).unwrap()
}

fn r(s: &str) -> RangeAddr {
    RangeAddr::parse_a1(s).unwrap()
}

fn edit(sheet: &str, cell: &str, input: &str) -> Action {
    Action::CellEdit {
        sheet: sheet.into(),
        addr: a1(cell),
        input: input.into(),
    }
}

/// A back-office session: build a ledger, fill formulas down, paste, sort,
/// restructure, and undo some of it — the shape of work the miner targets.
fn ledger_session() -> Vec<Action> {
    let mut log = vec![
        Action::SheetRename {
            from: "Sheet1".into(),
            to: "Ledger".into(),
        },
        Action::SheetAdd {
            name: "Rates".into(),
        },
        edit("Rates", "A1", "basic"),
        edit("Rates", "B1", "120"),
        edit("Rates", "A2", "pro"),
        edit("Rates", "B2", "750"),
        edit("Ledger", "A1", "member"),
        edit("Ledger", "B1", "tier"),
        edit("Ledger", "C1", "paid"),
        edit("Ledger", "D1", "owed"),
    ];
    for (i, (name, tier, paid)) in [
        ("Ada", "pro", "750"),
        ("Grace", "basic", "0"),
        ("Alan", "pro", "150"),
        ("Edsger", "basic", "120"),
    ]
    .iter()
    .enumerate()
    {
        let row = i + 2;
        log.push(edit("Ledger", &format!("A{row}"), name));
        log.push(edit("Ledger", &format!("B{row}"), tier));
        log.push(edit("Ledger", &format!("C{row}"), paid));
    }
    log.push(edit("Ledger", "D2", "=VLOOKUP(B2,Rates!A1:B2,2,FALSE)-C2"));
    log.push(Action::FillApply {
        sheet: "Ledger".into(),
        source: r("D2"),
        target: r("D2:D5"),
    });
    log.push(edit("Ledger", "F1", "outstanding"));
    log.push(edit("Ledger", "F2", "=SUM(D2:D5)"));
    log.push(Action::RangePaste {
        source_sheet: "Ledger".into(),
        source: r("D2:D5"),
        target_sheet: "Ledger".into(),
        target: r("E2:E5"),
        mode: PasteMode::Values,
        cut: false,
    });
    log.push(Action::SortApply {
        sheet: "Ledger".into(),
        range: r("A1:E5"),
        keys: vec![SortKey {
            column: 0,
            ascending: true,
        }],
        has_header: true,
    });
    log.push(Action::RowInsert {
        sheet: "Ledger".into(),
        at: 1,
        count: 1,
    });
    log.push(edit("Ledger", "A2", "Zoe"));
    log.push(Action::Undo);
    log.push(Action::Undo);
    log.push(Action::Redo);
    log
}

/// Every error path and awkward corner in one log.
///
/// The interesting cells sit from row 3 down so the row deletion below does
/// not simply erase them: the point of a fixture is that its *final* state
/// still exercises the behaviour, not just the actions along the way.
fn edge_case_session() -> Vec<Action> {
    vec![
        edit("Sheet1", "A3", "=B3"),
        edit("Sheet1", "B3", "=A3"),              // a cycle
        edit("Sheet1", "C3", "=IF(FALSE,A3,42)"), // reads a cycle but not really
        edit("Sheet1", "D3", "=A3+1"),            // genuinely downstream of a cycle
        edit("Sheet1", "E3", "=1/0"),
        edit("Sheet1", "F3", "=E3+1"), // error propagation
        edit("Sheet1", "G3", "=NOSUCH()"),
        edit("Sheet1", "H3", "=IFERROR(E3,\"caught\")"),
        edit("Sheet1", "A4", "=TODAY()"),     // volatile
        edit("Sheet1", "B4", "=RAND()"),      // volatile and derived from the clock
        edit("Sheet1", "C4", "=SUM(C1:C10)"), // self-overlapping range
        edit("Sheet1", "D4", "0.1"),
        edit("Sheet1", "E4", "=D4*3"),   // float formatting
        edit("Sheet1", "F4", "café ☕"), // non-ASCII
        Action::SheetAdd {
            name: "Other".into(),
        },
        edit("Other", "A1", "=Sheet1!E3"),
        Action::SheetRename {
            from: "Other".into(),
            to: "Renamed".into(),
        },
        edit("Sheet1", "G4", "=Renamed!A1"),
        Action::SheetDelete {
            name: "Renamed".into(),
        }, // leaves #REF! behind
        Action::RowDelete {
            sheet: "Sheet1".into(),
            at: 0,
            count: 1,
        },
        // Actions the engine must reject, identically, on replay.
        edit("NoSuchSheet", "A1", "1"),
        edit("Sheet1", "A1", "=1+"),
        Action::SheetDelete {
            name: "Sheet1".into(),
        },
        Action::Undo,
        Action::Undo,
        Action::Redo,
        Action::Redo,
        Action::Redo, // one more than there is history for
    ]
}

fn fixture_logs() -> Vec<(&'static str, Vec<Action>)> {
    vec![
        ("ledger-session", ledger_session()),
        ("edge-cases", edge_case_session()),
    ]
}

/// Write the fixture logs to disk so they are reviewable, and check each one
/// replays to its stored final state.
#[test]
fn fixture_logs_replay_to_their_recorded_state() {
    let dir = logs_dir();
    std::fs::create_dir_all(&dir).expect("create logs dir");

    for (name, log) in fixture_logs() {
        let log_path = dir.join(format!("{name}.json"));
        let state_path = dir.join(format!("{name}.final.json"));
        let log_json = serde_json::to_string_pretty(&log).unwrap();

        if std::env::var("UPDATE_GOLDEN").is_ok() || !log_path.exists() {
            std::fs::write(&log_path, format!("{log_json}\n")).expect("write log");
        }

        // Read the log back from disk: this also proves the on-disk format
        // deserializes into exactly the actions we serialized.
        let stored: Vec<Action> =
            serde_json::from_str(&std::fs::read_to_string(&log_path).expect("read log"))
                .expect("parse log");
        assert_eq!(
            stored, log,
            "{name}: on-disk log differs from the generator"
        );

        let final_state = replay(&stored).wb.state_snapshot();
        let pretty = serde_json::to_string_pretty(&final_state).unwrap();
        if std::env::var("UPDATE_GOLDEN").is_ok() || !state_path.exists() {
            std::fs::write(&state_path, format!("{pretty}\n")).expect("write state");
            continue;
        }

        let expected: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path).expect("read state"))
                .expect("parse state");
        assert_eq!(
            final_state, expected,
            "{name}: replaying the recorded log no longer reproduces the recorded state.\n\
             If this change is intentional, rerun with UPDATE_GOLDEN=1 and review the diff."
        );
    }
}

/// The serialized state must be byte-identical, not merely equal as JSON:
/// the dataset export and the state digests compare strings.
#[test]
fn replay_is_byte_identical_not_just_equal() {
    for (name, log) in fixture_logs() {
        let a = serde_json::to_string(&replay(&log).wb.state_snapshot()).unwrap();
        let b = serde_json::to_string(&replay(&log).wb.state_snapshot()).unwrap();
        assert_eq!(a, b, "{name}: two replays serialized differently");
    }
}

/// Replaying incrementally must match replaying in one go, and must also
/// match a full recalculation of the result.
#[test]
fn live_application_matches_replay_and_full_recalc() {
    for (name, log) in fixture_logs() {
        let live = replay(&log);
        let live_state = live.wb.state_snapshot();

        let replayed = replay(&log);
        assert_eq!(replayed.wb.state_snapshot(), live_state, "{name}: replay");

        let mut recalced = replay(&log);
        recalced.recalc_all();
        assert_eq!(
            recalced.wb.state_snapshot(),
            live_state,
            "{name}: full recalculation disagreed with incremental"
        );
    }
}

/// Replaying a prefix and then the remainder must equal replaying the whole
/// log — a log has no hidden state that only a full pass establishes.
#[test]
fn replay_can_be_resumed_at_any_point() {
    for (name, log) in fixture_logs() {
        let whole = replay(&log).wb.state_snapshot();
        for split in [1, log.len() / 3, log.len() / 2, log.len() - 1] {
            let mut e = Engine::new();
            e.now_ms = 1_704_110_400_000;
            for a in &log[..split] {
                let _ = e.apply(a);
            }
            for a in &log[split..] {
                let _ = e.apply(a);
            }
            assert_eq!(
                e.wb.state_snapshot(),
                whole,
                "{name}: splitting the log at {split} changed the outcome"
            );
        }
    }
}

// ---------------------------------------------------------------- property

const MAX_ROW: u32 = 6;
const MAX_COL: u32 = 4;

fn any_addr() -> impl Strategy<Value = CellAddr> {
    (0..MAX_ROW, 0..MAX_COL).prop_map(|(r, c)| CellAddr::new(r, c))
}

fn any_range() -> impl Strategy<Value = RangeAddr> {
    (any_addr(), any_addr()).prop_map(|(a, b)| RangeAddr::new(a, b))
}

fn any_input() -> impl Strategy<Value = String> {
    prop_oneof![
        (0..50i64).prop_map(|n| n.to_string()),
        Just("text".to_string()),
        Just("TRUE".to_string()),
        Just("=A1+B2".to_string()),
        Just("=$A$1*2".to_string()),
        Just("=SUM(A1:B4)".to_string()),
        Just("=IF(A1>2,B1,C1)".to_string()),
        Just("=TODAY()".to_string()),
        Just("=RAND()".to_string()),
        Just("=A1".to_string()),       // easy cycles
        Just("=1/0".to_string()),      // errors
        Just("=NOSUCH()".to_string()), // #NAME?
        Just("=1+".to_string()),       // rejected by the parser
        Just("".to_string()),          // clears the cell
    ]
}

fn any_action() -> impl Strategy<Value = Action> {
    let sheets = prop_oneof![Just("Sheet1".to_string()), Just("Two".to_string())];
    prop_oneof![
        (sheets.clone(), any_addr(), any_input())
            .prop_map(|(sheet, addr, input)| Action::CellEdit { sheet, addr, input }),
        (sheets.clone(), any_addr()).prop_map(|(sheet, addr)| Action::CellClear { sheet, addr }),
        (sheets.clone(), any_range())
            .prop_map(|(sheet, range)| Action::RangeClear { sheet, range }),
        (
            sheets.clone(),
            any_range(),
            any_addr(),
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(|(sheet, source, anchor, values, cut)| Action::RangePaste {
                source_sheet: sheet.clone(),
                source,
                target_sheet: sheet,
                target: RangeAddr::single(anchor),
                mode: if values {
                    PasteMode::Values
                } else {
                    PasteMode::Formulas
                },
                cut,
            }),
        (sheets.clone(), any_range(), 1..3u32).prop_map(|(sheet, source, extra)| {
            Action::FillApply {
                sheet,
                target: RangeAddr::new(
                    source.start,
                    CellAddr::new((source.end.row + extra).min(MAX_ROW), source.end.col),
                ),
                source,
            }
        }),
        (sheets.clone(), 0..MAX_ROW, 1..3u32).prop_map(|(sheet, at, count)| Action::RowInsert {
            sheet,
            at,
            count
        }),
        (sheets.clone(), 0..MAX_ROW, 1..3u32).prop_map(|(sheet, at, count)| Action::RowDelete {
            sheet,
            at,
            count
        }),
        (sheets.clone(), 0..MAX_COL, 1..3u32).prop_map(|(sheet, at, count)| Action::ColInsert {
            sheet,
            at,
            count
        }),
        (sheets.clone(), 0..MAX_COL, 1..3u32).prop_map(|(sheet, at, count)| Action::ColDelete {
            sheet,
            at,
            count
        }),
        (sheets.clone(), any_range(), any::<bool>(), any::<bool>()).prop_map(
            |(sheet, range, ascending, has_header)| Action::SortApply {
                sheet,
                keys: vec![SortKey {
                    column: range.start.col,
                    ascending
                }],
                range,
                has_header,
            }
        ),
        (sheets.clone(), any_range())
            .prop_map(|(sheet, range)| Action::MergeApply { sheet, range }),
        sheets.clone().prop_map(|name| Action::SheetAdd { name }),
        sheets.clone().prop_map(|name| Action::SheetDelete { name }),
        (sheets.clone(), sheets.clone()).prop_map(|(from, to)| Action::SheetRename { from, to }),
        Just(Action::Undo),
        Just(Action::Redo),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// 500 random action sequences: apply live, replay the same log from an
    /// empty workbook, and require identical serialized state.
    ///
    /// The two runs share a code path, so what this really hunts is hidden
    /// nondeterminism — hash iteration order leaking into output, float
    /// formatting drift, an ambient clock read.
    #[test]
    fn random_logs_replay_exactly(log in prop::collection::vec(any_action(), 0..40)) {
        let live = serde_json::to_string(&replay(&log).wb.state_snapshot()).unwrap();
        let again = serde_json::to_string(&replay(&log).wb.state_snapshot()).unwrap();
        prop_assert_eq!(&live, &again);
    }

    /// The emitted events must be deterministic too, not just the state.
    /// The capture log is what reaches the server and, eventually, a training
    /// set: two identical sessions have to produce two identical logs.
    #[test]
    fn event_streams_are_deterministic(log in prop::collection::vec(any_action(), 0..30)) {
        let run = |log: &[Action]| {
            let mut e = Engine::new();
            e.now_ms = 1_704_110_400_000;
            let mut events = Vec::new();
            for a in log {
                if let Ok(evs) = e.apply(a) {
                    events.extend(evs);
                }
            }
            serde_json::to_string(&events).unwrap()
        };
        prop_assert_eq!(run(&log), run(&log));
    }
}

proptest! {
    // Forcing a full rebuild after every action is genuinely a different
    // execution path from the incremental one, so this runs fewer, heavier
    // cases than the properties above.
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// Recalculating everything after each action must never change the
    /// result. This is the strongest form of the invariant that broke once
    /// already: a workbook's values cannot depend on how it was reached.
    #[test]
    fn full_recalc_after_every_action_changes_nothing(
        log in prop::collection::vec(any_action(), 0..25),
    ) {
        let mut incremental = Engine::new();
        incremental.now_ms = 1_704_110_400_000;
        let mut rebuilt = Engine::new();
        rebuilt.now_ms = 1_704_110_400_000;

        for a in &log {
            let _ = incremental.apply(a);
            let _ = rebuilt.apply(a);
            rebuilt.recalc_all();
            prop_assert_eq!(
                serde_json::to_string(&incremental.wb.state_snapshot()).unwrap(),
                serde_json::to_string(&rebuilt.wb.state_snapshot()).unwrap(),
                "diverged after {:?}", a
            );
        }
    }

    /// A log that survives a round trip through JSON must replay the same
    /// way — the on-disk format cannot lose anything replay depends on.
    #[test]
    fn logs_survive_serialization(log in prop::collection::vec(any_action(), 0..30)) {
        let text = serde_json::to_string(&log).unwrap();
        let parsed: Vec<Action> = serde_json::from_str(&text).unwrap();
        prop_assert_eq!(&parsed, &log);
        prop_assert_eq!(
            serde_json::to_string(&replay(&log).wb.state_snapshot()).unwrap(),
            serde_json::to_string(&replay(&parsed).wb.state_snapshot()).unwrap()
        );
    }

    /// Incremental recalculation must always agree with a full one, whatever
    /// path the workbook took to get there.
    #[test]
    fn incremental_matches_full_recalc(log in prop::collection::vec(any_action(), 0..40)) {
        let mut e = replay(&log);
        let incremental = serde_json::to_string(&e.wb.state_snapshot()).unwrap();
        e.recalc_all();
        prop_assert_eq!(incremental, serde_json::to_string(&e.wb.state_snapshot()).unwrap());
    }
}
