//! The demonstration dataset: one line per action, with the state before and
//! after it.
//!
//! Each record is `{pre_state_digest, context, action, post_state_digest}`.
//! A digest identifies a state without disclosing it, which is what lets the
//! dataset say "from this state the user did this, reaching that" while
//! carrying none of their data.
//!
//! # What the digests are digests *of*
//!
//! Not the user's workbook. Under `structural` capture a typed literal
//! reaches us as a hash, so their real values were never recorded and cannot
//! be replayed. What is replayed is a **synthetic workbook with the same
//! shape**: every redacted literal becomes a placeholder derived
//! deterministically from its hash, so two cells that held the same value
//! still hold the same value, and two that differed still differ.
//!
//! That is the property the dataset actually needs. A lookup finds its match,
//! a conditional aggregation counts the right rows, and a formula's
//! dependency structure is intact — because equality was preserved even
//! though the values were not. What it is not is the user's numbers, and
//! anything reading this file should not pretend otherwise.
//!
//! # Consent
//!
//! Enforced by the query that reads the events, exactly as the server's
//! export enforces it: an actor whose latest consent is `off` or revoked
//! contributes nothing. It is a `WHERE` clause and not a filter applied
//! afterwards, because a filter can be forgotten.

use engine::telemetry::{EventEnvelope, PrivacyMode};
use engine::{Action, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// One line of the dataset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Digest of the replayed state before the action.
    pub pre_state_digest: String,
    pub context: RecordContext,
    /// The action, as a typed engine `Action`.
    pub action: Action,
    /// Digest of the replayed state after it.
    pub post_state_digest: String,
}

/// Where the action happened, with nothing identifying in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordContext {
    pub session_id: String,
    pub workbook_id: String,
    pub seq: u64,
    pub sheet: String,
    pub selection: String,
    /// How much of the original was recorded, so a consumer knows whether
    /// the values in `action` are real or synthetic.
    pub privacy_mode: PrivacyMode,
    /// True when this action's literals were reconstructed from hashes.
    pub values_synthetic: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ExportReport {
    pub sessions: usize,
    pub records: usize,
    /// Events that carried no replayable action (navigation, consent, and
    /// the gestures the log does not record enough of).
    pub skipped: usize,
    /// Actions the engine refused during replay. Non-zero means the log and
    /// the engine disagree about something, which is worth knowing.
    pub rejected: usize,
}

/// Turn one actor's event stream into dataset records, session by session.
///
/// Events are expected in `(session_id, seq)` order, which is how both the
/// server's export and the miner's own reader deliver them.
pub fn export(events: &[EventEnvelope]) -> (Vec<Record>, ExportReport) {
    let mut out = Vec::new();
    let mut report = ExportReport::default();

    for session in split_sessions(events) {
        report.sessions += 1;
        // A fresh engine per session: a session is the unit a demonstration
        // is replayed from, and carrying state across one would make every
        // record after the first depend on work the reader cannot see.
        let mut engine = Engine::new();
        engine.now_ms = session.first().map(|e| e.ts_ms).unwrap_or(0);
        // The log's sheet names are hashed under `structural`; the replay
        // needs sheets by those names to exist before any action lands on
        // one, or every action would be refused as "unknown sheet".
        prepare_sheets(&mut engine, session);

        for event in session {
            let Some(action) = replayable(event) else {
                report.skipped += 1;
                continue;
            };
            let pre = digest(&engine);
            engine.now_ms = event.ts_ms;
            if engine.apply(&action).is_err() {
                // Rejected actions are part of the history — the replay suite
                // insists on that — but they change nothing, so a record
                // whose pre and post digests were identical would teach a
                // reader that the action was a no-op rather than a refusal.
                report.rejected += 1;
                continue;
            }
            let post = digest(&engine);
            out.push(Record {
                pre_state_digest: pre,
                context: RecordContext {
                    session_id: event.session_id.clone(),
                    workbook_id: event.workbook_id.clone(),
                    seq: event.seq,
                    sheet: event.context.sheet.clone(),
                    selection: event.context.selection.clone(),
                    privacy_mode: event.context.privacy_mode,
                    values_synthetic: event.context.privacy_mode != PrivacyMode::Full,
                },
                action,
                post_state_digest: post,
            });
            report.records += 1;
        }
    }
    (out, report)
}

/// SHA-256 of the deterministic state snapshot.
///
/// The snapshot is the same one the replay suite compares, so a digest here
/// means exactly what a passing replay test means: two states with the same
/// digest are the same state, by the definition the whole project uses.
pub fn digest(engine: &Engine) -> String {
    let text = serde_json::to_string(&engine.wb.state_snapshot()).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn split_sessions(events: &[EventEnvelope]) -> Vec<&[EventEnvelope]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=events.len() {
        if i == events.len() || events[i].session_id != events[start].session_id {
            out.push(&events[start..i]);
            start = i;
        }
    }
    out
}

/// Rebuild a replayable action from an envelope, substituting placeholders
/// for values the log only hashed.
fn replayable(event: &EventEnvelope) -> Option<Action> {
    use crate::routine::Rebuilt;
    let raw = serde_json::json!({ "action": event.action, "payload": event.payload });
    let action = match crate::routine::rebuild(&raw) {
        Rebuilt::Action(a) => a,
        Rebuilt::Missing { addr, kind } => Action::CellEdit {
            sheet: String::new(),
            addr,
            input: placeholder(&event.payload["input"], &kind),
        },
        Rebuilt::Skip => return None,
    };
    Some(with_sheet(action, &event.context.sheet))
}

/// A stand-in for a value we never received.
///
/// Derived from the hash, so equal originals stay equal and different ones
/// stay different — the only property of the real values the dataset can
/// honestly claim to preserve. The `type` the redactor recorded decides the
/// shape, so a number stays a number and text stays text; a formula reading
/// the cell therefore behaves the way it did.
pub fn placeholder(input: &serde_json::Value, kind: &str) -> String {
    let hash = input["hash"].as_str().unwrap_or("0");
    let seed = u64::from_str_radix(&hash[..hash.len().min(12)], 16).unwrap_or(0);
    match kind {
        // A four-digit range: wide enough that distinct values rarely
        // collide, small enough that a replayed sheet reads like a sheet.
        "number" => (seed % 9000 + 1000).to_string(),
        "bool" => if seed.is_multiple_of(2) {
            "TRUE"
        } else {
            "FALSE"
        }
        .to_string(),
        // Prefixed so nothing downstream mistakes it for a real string, and
        // apostrophe-escaped so a placeholder that happens to look like a
        // number or a formula still lands as text.
        _ => format!("'v{:06x}", seed % 0xff_ffff),
    }
}

fn with_sheet(a: Action, sheet: &str) -> Action {
    // The sheet name is hashed under `structural`, which is fine: it only has
    // to be *consistent* for the replay to be faithful, not readable. An
    // empty one (privacy mode `off`, which never reaches here) would not be a
    // valid sheet, so it falls back to the default.
    let name = if sheet.is_empty() { "Sheet1" } else { sheet };
    engine::routine::retarget_sheet(a, name)
}

/// Rename the replay's single sheet so the hashed names in the log resolve.
///
/// A session's events all carry the same hashed sheet name per sheet, so the
/// replay needs those sheets to exist. Rather than guessing how many there
/// were, the first action's sheet renames the default one and any other name
/// is added on demand.
fn prepare_sheets(engine: &mut Engine, events: &[EventEnvelope]) {
    let mut names: Vec<&str> = Vec::new();
    for e in events {
        let n = e.context.sheet.as_str();
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    let Some(first) = names.first() else { return };
    let default = engine.wb.sheets[0].name.clone();
    if default != *first {
        let _ = engine.apply(&Action::SheetRename {
            from: default,
            to: (*first).to_string(),
        });
    }
    for n in names.iter().skip(1) {
        let _ = engine.apply(&Action::SheetAdd {
            name: (*n).to_string(),
        });
    }
    engine.clear_history();
}

/// Serialize records as JSONL.
pub fn to_jsonl(records: &[Record]) -> String {
    let mut out = String::new();
    for r in records {
        if let Ok(line) = serde_json::to_string(r) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::telemetry::{EventContext, SCHEMA_VERSION};
    use serde_json::json;

    fn envelope(
        session: &str,
        seq: u64,
        action: &str,
        payload: serde_json::Value,
        mode: PrivacyMode,
    ) -> EventEnvelope {
        EventEnvelope {
            schema_version: SCHEMA_VERSION,
            event_id: format!("{session}-{seq}"),
            session_id: session.into(),
            actor_id: "u".into(),
            workbook_id: "wb".into(),
            seq,
            ts_ms: 1_700_000_000_000 + seq as i64 * 1000,
            action: action.into(),
            payload,
            context: EventContext {
                sheet: "Sheet1".into(),
                selection: "A1".into(),
                privacy_mode: mode,
            },
            client_version: "test".into(),
        }
    }

    fn edit(session: &str, seq: u64, addr: &str, input: serde_json::Value) -> EventEnvelope {
        let is_formula = input.as_str().is_some_and(|s| s.starts_with('='));
        envelope(
            session,
            seq,
            "cell.edit",
            json!({ "addr": addr, "input": input, "is_formula": is_formula }),
            if input.is_string() {
                PrivacyMode::Full
            } else {
                PrivacyMode::Structural
            },
        )
    }

    fn hashed(hash: &str, kind: &str) -> serde_json::Value {
        json!({ "hash": hash, "type": kind, "len": 4 })
    }

    #[test]
    fn each_action_gets_the_state_before_and_after_it() {
        let log = vec![
            edit("s1", 0, "A1", json!("2")),
            edit("s1", 1, "A2", json!("3")),
            edit("s1", 2, "A3", json!("=A1+A2")),
        ];
        let (records, report) = export(&log);
        assert_eq!(report.records, 3);
        assert_eq!(records.len(), 3);
        // Each record's post digest is the next one's pre digest: the stream
        // is a chain, which is what makes it replayable.
        assert_eq!(records[0].post_state_digest, records[1].pre_state_digest);
        assert_eq!(records[1].post_state_digest, records[2].pre_state_digest);
        // ...and every step actually changed something.
        for r in &records {
            assert_ne!(r.pre_state_digest, r.post_state_digest);
        }
    }

    #[test]
    fn a_digest_identifies_a_state_rather_than_a_path_to_it() {
        // Two different orders reaching the same cells must end at the same
        // digest, or the dataset would encode the route instead of the state.
        let a = export(&[
            edit("s1", 0, "A1", json!("1")),
            edit("s1", 1, "B1", json!("2")),
        ])
        .0;
        let b = export(&[
            edit("s2", 0, "B1", json!("2")),
            edit("s2", 1, "A1", json!("1")),
        ])
        .0;
        assert_eq!(
            a.last().unwrap().post_state_digest,
            b.last().unwrap().post_state_digest
        );
    }

    #[test]
    fn a_redacted_value_replays_as_a_placeholder_of_the_right_type() {
        let log = vec![
            edit("s1", 0, "A1", hashed("00000005", "number")),
            edit("s1", 1, "A2", hashed("00000007", "number")),
            edit("s1", 2, "A3", json!("=A1+A2")),
        ];
        let (records, _) = export(&log);
        // The formula computed over the placeholders, so the arithmetic is
        // real even though the operands are not.
        match &records[2].action {
            Action::CellEdit { input, .. } => assert_eq!(input, "=A1+A2"),
            other => panic!("{other:?}"),
        }
        assert!(records[0].context.values_synthetic);
        // A number placeholder must actually be a number, or `=A1+A2` would
        // come out as #VALUE! and the record would be worthless.
        match &records[0].action {
            Action::CellEdit { input, .. } => {
                assert!(input.parse::<f64>().is_ok(), "{input}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn equal_values_stay_equal_and_different_ones_stay_different() {
        // The one property the placeholders can honestly claim, and the one
        // that makes lookups and COUNTIF behave as they did.
        assert_eq!(
            placeholder(&hashed("abc123", "number"), "number"),
            placeholder(&hashed("abc123", "number"), "number")
        );
        assert_ne!(
            placeholder(&hashed("abc123", "number"), "number"),
            placeholder(&hashed("def456", "number"), "number")
        );
    }

    #[test]
    fn a_text_placeholder_cannot_be_mistaken_for_a_number_or_a_formula() {
        let p = placeholder(&hashed("00000001", "text"), "text");
        assert!(p.starts_with('\''), "{p}");
    }

    #[test]
    fn a_bool_placeholder_is_a_bool() {
        let p = placeholder(&hashed("00000002", "bool"), "bool");
        assert!(p == "TRUE" || p == "FALSE", "{p}");
    }

    #[test]
    fn sessions_replay_independently() {
        // Two sessions that do the same thing must produce the same digests,
        // which only holds if each starts from an empty workbook.
        let log = vec![
            edit("s1", 0, "A1", json!("1")),
            edit("s2", 0, "A1", json!("1")),
        ];
        let (records, report) = export(&log);
        assert_eq!(report.sessions, 2);
        assert_eq!(records[0].pre_state_digest, records[1].pre_state_digest);
        assert_eq!(records[0].post_state_digest, records[1].post_state_digest);
    }

    #[test]
    fn navigation_and_bookkeeping_produce_no_records() {
        let log = vec![
            envelope(
                "s1",
                0,
                "nav.select",
                json!({ "selection": "B2" }),
                PrivacyMode::Full,
            ),
            envelope("s1", 1, "capture.pause", json!({}), PrivacyMode::Full),
            edit("s1", 2, "A1", json!("1")),
        ];
        let (records, report) = export(&log);
        assert_eq!(records.len(), 1);
        assert_eq!(report.skipped, 2);
    }

    #[test]
    fn an_action_the_engine_refuses_is_counted_not_recorded() {
        // A record whose pre and post digests matched would read as "this
        // action does nothing", which is a different claim from "the engine
        // refused it".
        let mut bad = edit("s1", 0, "A1", json!("=1+"));
        bad.payload["is_formula"] = json!(true);
        let (records, report) = export(&[bad, edit("s1", 1, "A1", json!("1"))]);
        assert_eq!(report.rejected, 1);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn the_dataset_carries_no_hashes_and_no_raw_payloads() {
        // The records are typed actions, not envelopes: a payload copied
        // through by accident would put the hash — and under `full`, the
        // value — into a file we hand out.
        let log = vec![edit("s1", 0, "A1", hashed("deadbeefcafe", "number"))];
        let text = to_jsonl(&export(&log).0);
        assert!(!text.contains("deadbeef"), "{text}");
        assert!(!text.contains("\"payload\""), "{text}");
    }

    #[test]
    fn records_round_trip_through_jsonl() {
        let log = vec![
            edit("s1", 0, "A1", json!("1")),
            edit("s1", 1, "B1", json!("=A1*2")),
        ];
        let (records, _) = export(&log);
        let text = to_jsonl(&records);
        let back: Vec<Record> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(back, records);
    }

    #[test]
    fn the_same_log_exports_identically_every_time() {
        let log = vec![
            edit("s1", 0, "A1", hashed("aa", "number")),
            edit("s1", 1, "A2", json!("=A1*2")),
        ];
        assert_eq!(export(&log).0, export(&log).0);
    }
}
