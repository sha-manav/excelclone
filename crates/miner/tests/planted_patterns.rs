//! The miner's acceptance test: plant a habit in a synthetic log, and check
//! the pipeline finds it, prices it, and turns it into something that
//! actually runs.
//!
//! Planted-pattern tests are the only honest way to test a miner. Running it
//! over a real log and eyeballing the output tells you what it found, not
//! whether what it found is what is there — and a miner that reports
//! plausible nonsense looks exactly like one that works.
//!
//! Each test therefore states the habit first, in the fixture, and then
//! asserts the miner recovered *that* and priced it sanely. The negative
//! cases matter at least as much: a miner that proposes something for every
//! log is a miner nobody will read twice.

use engine::telemetry::{EventContext, EventEnvelope, PrivacyMode, SCHEMA_VERSION};
use engine::{Action, CellAddr, Engine};
use miner::mine::MineConfig;
use miner::routine::dry_run;
use serde_json::json;

/// Build an envelope the way the client would, so the miner is reading the
/// same shape it will read in production.
fn envelope(session: &str, seq: u64, action: &str, payload: serde_json::Value) -> EventEnvelope {
    EventEnvelope {
        schema_version: SCHEMA_VERSION,
        event_id: format!("{session}-{seq}"),
        session_id: session.to_string(),
        actor_id: "u_test".into(),
        workbook_id: "wb_test".into(),
        seq,
        ts_ms: 1_700_000_000_000 + seq as i64 * 5_000,
        action: action.to_string(),
        payload,
        context: EventContext {
            sheet: "Ledger".into(),
            selection: "A1".into(),
            privacy_mode: PrivacyMode::Structural,
        },
        client_version: "test".into(),
    }
}

fn formula(session: &str, seq: u64, addr: &str, src: &str) -> EventEnvelope {
    envelope(
        session,
        seq,
        "cell.edit",
        json!({ "addr": addr, "input": src, "is_formula": true }),
    )
}

/// A literal as `structural` capture records it: a hash, not a value.
fn hashed_literal(session: &str, seq: u64, addr: &str, kind: &str) -> EventEnvelope {
    envelope(
        session,
        seq,
        "cell.edit",
        json!({
            "addr": addr,
            "input": { "hash": "0123456789abcdef", "type": kind, "len": 4 },
            "is_formula": false,
        }),
    )
}

fn bold(session: &str, seq: u64, range: &str) -> EventEnvelope {
    envelope(
        session,
        seq,
        "format.apply",
        json!({
            "range": range,
            "kind": "style",
            "cells": 1,
            "attributes": ["bold"],
            "patches": [{ "set": "bold", "value": true }],
        }),
    )
}

fn nav(session: &str, seq: u64) -> EventEnvelope {
    envelope(session, seq, "nav.select", json!({ "selection": "A1" }))
}

/// The habit: for each of 12 rows, write two formulas and bold the second.
/// Interleaved with the navigation the client samples, because a real log is
/// never a clean run of gestures.
fn monthly_close() -> Vec<EventEnvelope> {
    let mut log = Vec::new();
    let mut seq = 0;
    let mut push = |e: EventEnvelope, seq: &mut u64| {
        log.push(e);
        *seq += 1;
    };
    for row in 2..32u32 {
        push(nav("s1", seq), &mut seq);
        push(
            formula(
                "s1",
                seq,
                &format!("E{row}"),
                &format!("=SUM(B{row}:D{row})"),
            ),
            &mut seq,
        );
        push(
            formula("s1", seq, &format!("F{row}"), &format!("=E{row}*0.2")),
            &mut seq,
        );
        push(bold("s1", seq, &format!("F{row}")), &mut seq);
    }
    log
}

#[test]
fn a_planted_loop_is_found_with_the_right_shape_and_length() {
    let routines = miner::mine_routines(&monthly_close(), MineConfig::default());
    assert!(!routines.is_empty(), "the planted habit was not found");

    let top = &routines[0];
    // Three gestures per row: two formulas and a bold.
    assert_eq!(
        top.actions.len(),
        3,
        "expected the three-step gesture, got {:?}",
        top.actions
    );
    assert_eq!(top.support, 30, "one occurrence per row");
    assert_eq!(top.kind, "loop");
    assert!(!top.is_partial(), "nothing here was redacted");
}

#[test]
fn navigation_between_the_steps_does_not_break_the_loop() {
    // The client samples selection changes into the log. If those counted as
    // part of the gesture, the period would be four rather than three and the
    // synthesized routine would try to "run" a cursor move.
    let routines = miner::mine_routines(&monthly_close(), MineConfig::default());
    assert!(routines[0]
        .actions
        .iter()
        .all(|a| !matches!(a, Action::Undo | Action::Redo)));
    assert_eq!(routines[0].actions.len(), 3);
}

#[test]
fn the_routine_runs_where_it_is_asked_and_the_preview_says_what_changes() {
    let routines = miner::mine_routines(&monthly_close(), MineConfig::default());
    let top = &routines[0];

    // A workbook shaped like the one the habit was recorded against, with a
    // fresh row 20 that has not been closed yet.
    let mut engine = Engine::new();
    for (addr, value) in [("B20", "10"), ("C20", "20"), ("D20", "30")] {
        engine
            .apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1(addr).unwrap(),
                input: value.into(),
            })
            .unwrap();
    }

    let preview = dry_run(&engine, top, "Sheet1", CellAddr::parse_a1("E20").unwrap());
    assert!(preview.errors.is_empty(), "{:?}", preview.errors);

    let changed: Vec<(&str, &str)> = preview
        .changes
        .iter()
        .map(|c| (c.addr.as_str(), c.after.as_str()))
        .collect();
    assert_eq!(changed, vec![("E20", "60"), ("F20", "12")]);

    // And the preview really was a preview.
    assert_eq!(engine.value_at("Sheet1", "E20"), engine::Value::Empty);
}

#[test]
fn a_redacted_value_is_reported_as_needed_input_rather_than_invented() {
    // The same habit, but each row starts with a typed number the log only
    // has a hash of. The routine must still be useful — and must say what it
    // cannot do rather than filling in a plausible number.
    let mut log = Vec::new();
    let mut seq = 0;
    for row in 2..22u32 {
        log.push(hashed_literal("s1", seq, &format!("B{row}"), "number"));
        seq += 1;
        log.push(formula(
            "s1",
            seq,
            &format!("E{row}"),
            &format!("=SUM(B{row}:D{row})"),
        ));
        seq += 1;
        log.push(formula(
            "s1",
            seq,
            &format!("F{row}"),
            &format!("=E{row}*0.2"),
        ));
        seq += 1;
    }
    let routines = miner::mine_routines(&log, MineConfig::default());
    let top = routines.first().expect("a routine");
    assert!(top.is_partial(), "the redacted value should be reported");
    assert_eq!(top.requires.len(), 1);
    assert_eq!(top.requires[0].kind, "number");
    // The formulas still run.
    assert_eq!(top.actions.len(), 2);
}

#[test]
fn a_habit_spread_across_sessions_is_found_too() {
    // Three separate sittings, each doing the same two-step gesture once.
    // No loop to find, so this is PrefixSpan's case.
    let mut log = Vec::new();
    for (i, session) in ["s1", "s2", "s3"].iter().enumerate() {
        let row = 2 + i as u32;
        log.push(nav(session, 0));
        // A report header block: the kind of setup that is worth automating
        // precisely because it is long and only done occasionally.
        // Eight distinct formulas — distinct from each other, identical from
        // session to session. Absolute references keep the shape stable even
        // though each sitting builds the block on a different row, and the
        // varying multiplier keeps the eight from collapsing into one
        // repeated token that the loop miner would claim instead.
        for (n, col) in ["E", "F", "G", "H", "I", "J", "K", "L"].iter().enumerate() {
            log.push(formula(
                session,
                1 + n as u64,
                &format!("{col}{row}"),
                &format!("=SUMIFS($B$2:$B$999,$A$2:$A$999,$D$1)*{}", n + 1),
            ));
        }
        // Unrelated work, so the sessions are not identical.
        log.push(envelope(
            session,
            20,
            "row.insert",
            json!({ "at": 40 + i, "count": 1 }),
        ));
    }
    let routines = miner::mine_routines(&log, MineConfig::default());
    assert!(
        routines.iter().any(|r| r.kind == "recurring"),
        "no cross-session pattern found: {:?}",
        routines.iter().map(|r| &r.summary).collect::<Vec<_>>()
    );
}

#[test]
fn a_log_with_no_habit_proposes_nothing() {
    // Twenty different one-off edits. A miner that finds a routine here is a
    // miner whose suggestions mean nothing.
    let log: Vec<EventEnvelope> = (0..20)
        .map(|i| {
            formula(
                "s1",
                i,
                &format!("A{}", i + 1),
                &format!("=B{}*{}", i + 1, i + 2),
            )
        })
        .collect();
    let routines = miner::mine_routines(&log, MineConfig::default());
    assert!(
        routines.is_empty(),
        "invented a routine from noise: {:?}",
        routines.iter().map(|r| &r.summary).collect::<Vec<_>>()
    );
}

#[test]
fn a_habit_repeated_only_twice_is_below_the_threshold() {
    // Two rows is not yet a habit worth two minutes of anyone's attention.
    let mut log = Vec::new();
    let mut seq = 0;
    for row in 2..4u32 {
        log.push(formula(
            "s1",
            seq,
            &format!("E{row}"),
            &format!("=SUM(B{row}:D{row})"),
        ));
        seq += 1;
        log.push(bold("s1", seq, &format!("E{row}")));
        seq += 1;
    }
    assert!(miner::mine_routines(&log, MineConfig::default()).is_empty());
}

#[test]
fn the_same_log_always_produces_the_same_routines() {
    // The panel's contents must not depend on hash iteration order: a user
    // who reloads must see the same suggestions in the same order.
    let log = monthly_close();
    let a = miner::mine_routines(&log, MineConfig::default());
    let b = miner::mine_routines(&log, MineConfig::default());
    assert_eq!(a, b);
}

#[test]
fn a_routine_never_carries_a_literal_value_or_a_sheet_name_in_its_summary() {
    // The summary is stored server-side and shown in the panel. It describes
    // shape; it must not become a side channel for the content the tokenizer
    // was careful to drop.
    let mut log = monthly_close();
    log.push(envelope(
        "s1",
        900,
        "sheet.rename",
        json!({ "from": { "hash": "aa", "type": "text", "len": 6 },
                "to": { "hash": "bb", "type": "text", "len": 6 } }),
    ));
    for r in miner::mine_routines(&log, MineConfig::default()) {
        assert!(!r.summary.contains("hash"), "{}", r.summary);
        assert!(!r.summary.contains("Ledger"), "{}", r.summary);
        assert!(!r.summary.contains("0.2"), "{}", r.summary);
    }
}

#[test]
fn running_a_routine_twice_is_visible_in_the_preview() {
    // A routine is not idempotent in general, and the preview is how a user
    // finds that out before it matters.
    let routines = miner::mine_routines(&monthly_close(), MineConfig::default());
    let top = &routines[0];
    let mut engine = Engine::new();
    for (addr, value) in [("B20", "10"), ("C20", "20"), ("D20", "30")] {
        engine
            .apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1(addr).unwrap(),
                input: value.into(),
            })
            .unwrap();
    }
    for action in top.actions_at("Sheet1", CellAddr::parse_a1("E20").unwrap()) {
        engine.apply(&action).unwrap();
    }
    // Second time over the same anchor: nothing further changes, because the
    // cells already hold what the routine writes.
    let again = dry_run(&engine, top, "Sheet1", CellAddr::parse_a1("E20").unwrap());
    assert!(again.changes.is_empty(), "{:?}", again.changes);
}

#[test]
fn a_routine_body_survives_the_round_trip_the_server_puts_it_through() {
    // The server stores the body as TEXT and hands it back as JSON; the
    // client then asks the engine to apply it. Anything lost in there would
    // surface as a routine that runs differently than it previewed.
    let routines = miner::mine_routines(&monthly_close(), MineConfig::default());
    let stored = serde_json::to_string(&routines[0]).unwrap();
    let back: miner::routine::Routine = serde_json::from_str(&stored).unwrap();
    assert_eq!(back, routines[0]);
    assert_eq!(
        back.actions_at("Sheet1", CellAddr::new(30, 4)),
        routines[0].actions_at("Sheet1", CellAddr::new(30, 4))
    );
}
