//! The event envelope and the privacy redaction that governs it.
//!
//! This lives in the engine, next to the actions it describes, for one
//! reason: redaction is a privacy guarantee, and a guarantee implemented
//! twice is a guarantee that will eventually be implemented differently. The
//! client redacts through this code compiled to wasm, the server and miner
//! read the same types, and there is exactly one definition of what
//! `structural` mode means.
//!
//! The vocabulary here is documented for users in `docs/EVENTS.md`; the two
//! are kept in step by `vocabulary_matches_docs` in the tests.

use crate::engine::{Action, PasteMode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;

/// How much of what the user typed may be recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    /// Values and formulas verbatim.
    Full,
    /// Formulas verbatim; literal values replaced by salted hashes.
    #[default]
    Structural,
    /// Nothing captured or transmitted.
    Off,
}

impl PrivacyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrivacyMode::Full => "full",
            PrivacyMode::Structural => "structural",
            PrivacyMode::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<PrivacyMode> {
        match s {
            "full" => Some(PrivacyMode::Full),
            "structural" => Some(PrivacyMode::Structural),
            "off" => Some(PrivacyMode::Off),
            _ => None,
        }
    }

    /// Whether anything at all may be emitted in this mode.
    pub fn captures(&self) -> bool {
        !matches!(self, PrivacyMode::Off)
    }
}

/// The complete action vocabulary. Every engine `Action` maps to exactly one
/// of these, plus the entries the shell emits for capture and file lifecycle.
pub const ACTION_VOCABULARY: &[&str] = &[
    "cell.edit",
    "cell.clear",
    "range.copy",
    "range.cut",
    "range.paste",
    "fill.apply",
    "row.insert",
    "row.delete",
    "col.insert",
    "col.delete",
    "sort.apply",
    "filter.apply",
    "filter.clear",
    "sheet.add",
    "sheet.rename",
    "sheet.delete",
    "format.apply",
    "find.replace",
    "file.new",
    "file.open",
    "file.import",
    "file.export",
    "file.save",
    "nav.select",
    "undo",
    "redo",
    "routine.run",
    "capture.pause",
    "capture.resume",
    "consent.granted",
    "consent.revoked",
];

/// Where the user was when the action happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventContext {
    pub sheet: String,
    pub selection: String,
    pub privacy_mode: PrivacyMode,
}

/// The wire format. Field order matches `docs/EVENTS.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub schema_version: u32,
    pub event_id: String,
    pub session_id: String,
    pub actor_id: String,
    pub workbook_id: String,
    pub seq: u64,
    pub ts_ms: i64,
    pub action: String,
    pub payload: Json,
    pub context: EventContext,
    pub client_version: String,
}

/// A redacted literal value: enough to spot repetition, not enough to read.
fn hash_literal(text: &str, kind: &str, salt: &str) -> Json {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b"\x1f"); // separator, so salt||text is unambiguous
    hasher.update(text.as_bytes());
    let digest = hex::encode(hasher.finalize());
    json!({
        "hash": &digest[..16],
        "type": kind,
        "len": text.chars().count(),
    })
}

/// Classify a raw cell input the way the redactor needs to.
fn literal_kind(input: &str) -> &'static str {
    if input.eq_ignore_ascii_case("true") || input.eq_ignore_ascii_case("false") {
        "bool"
    } else if crate::eval::parse_number_text(input).is_some() {
        "number"
    } else {
        "text"
    }
}

/// Redact one piece of user-entered text according to the mode.
///
/// Formulas are always kept verbatim: their structure is the entire point of
/// mining, and they describe shape rather than content. Literal values are
/// hashed under `structural`.
pub fn redact_input(input: &str, mode: PrivacyMode, salt: &str) -> Json {
    match mode {
        PrivacyMode::Full => json!(input),
        PrivacyMode::Off => Json::Null,
        PrivacyMode::Structural => {
            if input.starts_with('=') {
                json!(input)
            } else {
                hash_literal(input, literal_kind(input), salt)
            }
        }
    }
}

/// Redact a free-text label (sheet name, search term, filter value).
fn redact_label(text: &str, mode: PrivacyMode, salt: &str) -> Json {
    match mode {
        PrivacyMode::Full => json!(text),
        PrivacyMode::Off => Json::Null,
        PrivacyMode::Structural => hash_literal(text, "text", salt),
    }
}

/// Redact a label that has to stay a plain string on the wire, such as
/// `context.sheet`.
///
/// The envelope's context carries the sheet name on *every* event. Leaving it
/// in clear while hashing the same name inside payloads would be worse than
/// not hashing at all: it leaks the name anyway, and it hands an observer a
/// matched hash/plaintext pair for this workbook's salt, which unpicks every
/// other hash of that value.
pub fn redact_label_text(text: &str, mode: PrivacyMode, salt: &str) -> String {
    match mode {
        PrivacyMode::Full => text.to_string(),
        PrivacyMode::Off => String::new(),
        PrivacyMode::Structural => hash_literal(text, "text", salt)["hash"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    }
}

fn a1(addr: &crate::addr::CellAddr) -> String {
    addr.to_a1()
}

/// Map an engine action to its vocabulary name and payload, already redacted
/// for the given mode.
///
/// Positions, shapes, counts and references are always preserved — they carry
/// no personal content and they are what the miner reasons about.
pub fn describe(action: &Action, mode: PrivacyMode, salt: &str) -> (String, Json) {
    let name = action_name(action).to_string();
    let payload = match action {
        Action::CellEdit { sheet, addr, input } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "addr": a1(addr),
            "input": redact_input(input, mode, salt),
            "is_formula": input.starts_with('='),
        }),
        Action::CellClear { sheet, addr } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "addr": a1(addr),
        }),
        Action::RangeClear { sheet, range } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "range": range.to_a1(),
            "cells": range.cell_count(),
        }),
        Action::RangePaste {
            source_sheet,
            source,
            target_sheet,
            target,
            mode: paste_mode,
            cut,
        } => json!({
            "source": format!("{}!{}", "sheet", source.to_a1()),
            "source_sheet": redact_label(source_sheet, mode, salt),
            "target": format!("{}!{}", "sheet", target.to_a1()),
            "target_sheet": redact_label(target_sheet, mode, salt),
            "shape": [source.rows(), source.cols()],
            "mode": match paste_mode { PasteMode::Formulas => "formulas", PasteMode::Values => "values" },
            "ref_adjust": if *cut { "moved" } else { "relative" },
            "cut": cut,
        }),
        Action::FillApply {
            sheet,
            source,
            target,
        } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "source": source.to_a1(),
            "target": target.to_a1(),
            "direction": if target.rows() > source.rows() { "down" } else { "right" },
            "steps": target.cell_count().saturating_sub(source.cell_count()),
        }),
        Action::RowInsert { sheet, at, count }
        | Action::RowDelete { sheet, at, count }
        | Action::ColInsert { sheet, at, count }
        | Action::ColDelete { sheet, at, count } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "at": at,
            "count": count,
        }),
        Action::SortApply {
            sheet,
            range,
            keys,
            has_header,
        } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "range": range.to_a1(),
            "keys": keys.iter().map(|k| json!({ "column": k.column, "ascending": k.ascending })).collect::<Vec<_>>(),
            "has_header": has_header,
        }),
        Action::FilterApply { sheet, spec } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "range": spec.range.to_a1(),
            "column": spec.column,
            // The chosen values are user data; only their count is structural.
            "allowed_count": spec.allowed.len(),
            "allowed": spec.allowed.iter().map(|v| redact_label(v, mode, salt)).collect::<Vec<_>>(),
        }),
        Action::FilterClear { sheet } => json!({
            "sheet": redact_label(sheet, mode, salt),
        }),
        Action::MergeApply { sheet, range } | Action::MergeClear { sheet, range } => json!({
            "sheet": redact_label(sheet, mode, salt),
            "range": range.to_a1(),
        }),
        Action::SheetAdd { name } => json!({ "name": redact_label(name, mode, salt) }),
        Action::SheetRename { from, to } => json!({
            "from": redact_label(from, mode, salt),
            "to": redact_label(to, mode, salt),
        }),
        Action::SheetDelete { name } => json!({ "name": redact_label(name, mode, salt) }),
        Action::Undo | Action::Redo => json!({}),
    };
    (name, payload)
}

/// The vocabulary entry for an action.
pub fn action_name(action: &Action) -> &'static str {
    match action {
        Action::CellEdit { .. } => "cell.edit",
        Action::CellClear { .. } => "cell.clear",
        Action::RangeClear { .. } => "cell.clear",
        Action::RangePaste { cut, .. } => {
            if *cut {
                "range.cut"
            } else {
                "range.paste"
            }
        }
        Action::FillApply { .. } => "fill.apply",
        Action::RowInsert { .. } => "row.insert",
        Action::RowDelete { .. } => "row.delete",
        Action::ColInsert { .. } => "col.insert",
        Action::ColDelete { .. } => "col.delete",
        Action::SortApply { .. } => "sort.apply",
        Action::FilterApply { .. } => "filter.apply",
        Action::FilterClear { .. } => "filter.clear",
        Action::MergeApply { .. } | Action::MergeClear { .. } => "format.apply",
        Action::SheetAdd { .. } => "sheet.add",
        Action::SheetRename { .. } => "sheet.rename",
        Action::SheetDelete { .. } => "sheet.delete",
        Action::Undo => "undo",
        Action::Redo => "redo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addr::{CellAddr, RangeAddr};

    fn edit(input: &str) -> Action {
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: CellAddr::new(0, 0),
            input: input.into(),
        }
    }

    #[test]
    fn structural_mode_hides_values_but_keeps_shape() {
        let (name, payload) = describe(&edit("48250"), PrivacyMode::Structural, "salt");
        assert_eq!(name, "cell.edit");
        assert_eq!(payload["addr"], "A1");
        // The number itself must not appear anywhere in the payload.
        let text = payload.to_string();
        assert!(!text.contains("48250"), "value leaked: {text}");
        assert_eq!(payload["input"]["type"], "number");
        assert_eq!(payload["input"]["len"], 5);
        assert_eq!(payload["input"]["hash"].as_str().unwrap().len(), 16);
    }

    #[test]
    fn structural_mode_keeps_formulas_verbatim() {
        let (_, payload) = describe(&edit("=SUM(A1:A9)*2"), PrivacyMode::Structural, "salt");
        assert_eq!(payload["input"], "=SUM(A1:A9)*2");
        assert_eq!(payload["is_formula"], true);
    }

    #[test]
    fn full_mode_keeps_values() {
        let (_, payload) = describe(&edit("48250"), PrivacyMode::Full, "salt");
        assert_eq!(payload["input"], "48250");
    }

    #[test]
    fn off_mode_records_no_content() {
        let (_, payload) = describe(&edit("secret"), PrivacyMode::Off, "salt");
        assert!(payload["input"].is_null());
        assert!(!PrivacyMode::Off.captures());
    }

    #[test]
    fn hashes_are_stable_within_a_salt_and_differ_across_salts() {
        let a = describe(&edit("hello"), PrivacyMode::Structural, "salt-1").1;
        let b = describe(&edit("hello"), PrivacyMode::Structural, "salt-1").1;
        let c = describe(&edit("hello"), PrivacyMode::Structural, "salt-2").1;
        // Repetition is detectable within one workbook...
        assert_eq!(a["input"]["hash"], b["input"]["hash"]);
        // ...but not correlatable across workbooks.
        assert_ne!(a["input"]["hash"], c["input"]["hash"]);
    }

    #[test]
    fn salt_and_text_cannot_be_confused() {
        // Without a separator, salt "ab" + text "c" would collide with salt
        // "a" + text "bc"; the separator byte prevents that.
        let a = describe(&edit("c"), PrivacyMode::Structural, "ab").1;
        let b = describe(&edit("bc"), PrivacyMode::Structural, "a").1;
        assert_ne!(a["input"]["hash"], b["input"]["hash"]);
    }

    #[test]
    fn literal_kinds_are_classified() {
        assert_eq!(literal_kind("42"), "number");
        assert_eq!(literal_kind("3.5%"), "number");
        assert_eq!(literal_kind("TRUE"), "bool");
        assert_eq!(literal_kind("false"), "bool");
        assert_eq!(literal_kind("hello"), "text");
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        let (_, payload) = describe(&edit("café☕"), PrivacyMode::Structural, "s");
        assert_eq!(payload["input"]["len"], 5);
    }

    #[test]
    fn structural_mode_hides_sheet_names_and_filter_values() {
        let action = Action::SheetRename {
            from: "Payroll Q3".into(),
            to: "Payroll Q4".into(),
        };
        let (_, payload) = describe(&action, PrivacyMode::Structural, "s");
        let text = payload.to_string();
        assert!(!text.contains("Payroll"), "sheet name leaked: {text}");

        let action = Action::FilterApply {
            sheet: "S".into(),
            spec: crate::engine::FilterSpec {
                range: RangeAddr::parse_a1("A1:A9").unwrap(),
                column: 0,
                allowed: vec!["alice@example.com".into()],
            },
        };
        let (_, payload) = describe(&action, PrivacyMode::Structural, "s");
        let text = payload.to_string();
        assert!(!text.contains("alice"), "filter value leaked: {text}");
        // The shape survives: a filter on one column with one allowed value.
        assert_eq!(payload["range"], "A1:A9");
        assert_eq!(payload["allowed_count"], 1);
    }

    #[test]
    fn context_labels_are_redacted_consistently_with_payloads() {
        // The same sheet name must not be hashed in one place and clear in
        // another: that leaks it and reveals the hash of a known value.
        let payload_hash = describe(
            &Action::SheetAdd {
                name: "Payroll Q3".into(),
            },
            PrivacyMode::Structural,
            "s",
        )
        .1["name"]["hash"]
            .as_str()
            .unwrap()
            .to_string();
        let context = redact_label_text("Payroll Q3", PrivacyMode::Structural, "s");
        assert_eq!(context, payload_hash);
        assert!(!context.contains("Payroll"));

        assert_eq!(
            redact_label_text("Payroll Q3", PrivacyMode::Full, "s"),
            "Payroll Q3"
        );
        assert_eq!(redact_label_text("Payroll Q3", PrivacyMode::Off, "s"), "");
    }

    #[test]
    fn every_action_maps_into_the_vocabulary() {
        let samples = vec![
            edit("1"),
            Action::CellClear {
                sheet: "S".into(),
                addr: CellAddr::new(0, 0),
            },
            Action::RangeClear {
                sheet: "S".into(),
                range: RangeAddr::parse_a1("A1:B2").unwrap(),
            },
            Action::RangePaste {
                source_sheet: "S".into(),
                source: RangeAddr::parse_a1("A1").unwrap(),
                target_sheet: "S".into(),
                target: RangeAddr::parse_a1("B1").unwrap(),
                mode: PasteMode::Formulas,
                cut: false,
            },
            Action::RangePaste {
                source_sheet: "S".into(),
                source: RangeAddr::parse_a1("A1").unwrap(),
                target_sheet: "S".into(),
                target: RangeAddr::parse_a1("B1").unwrap(),
                mode: PasteMode::Values,
                cut: true,
            },
            Action::FillApply {
                sheet: "S".into(),
                source: RangeAddr::parse_a1("A1").unwrap(),
                target: RangeAddr::parse_a1("A1:A9").unwrap(),
            },
            Action::RowInsert {
                sheet: "S".into(),
                at: 0,
                count: 1,
            },
            Action::RowDelete {
                sheet: "S".into(),
                at: 0,
                count: 1,
            },
            Action::ColInsert {
                sheet: "S".into(),
                at: 0,
                count: 1,
            },
            Action::ColDelete {
                sheet: "S".into(),
                at: 0,
                count: 1,
            },
            Action::SortApply {
                sheet: "S".into(),
                range: RangeAddr::parse_a1("A1:B9").unwrap(),
                keys: vec![crate::engine::SortKey {
                    column: 0,
                    ascending: true,
                }],
                has_header: true,
            },
            Action::FilterClear { sheet: "S".into() },
            Action::MergeApply {
                sheet: "S".into(),
                range: RangeAddr::parse_a1("A1:B1").unwrap(),
            },
            Action::SheetAdd { name: "S2".into() },
            Action::SheetRename {
                from: "S".into(),
                to: "T".into(),
            },
            Action::SheetDelete { name: "T".into() },
            Action::Undo,
            Action::Redo,
        ];
        for a in &samples {
            let (name, _) = describe(a, PrivacyMode::Structural, "s");
            assert!(
                ACTION_VOCABULARY.contains(&name.as_str()),
                "{name} is not in the documented vocabulary"
            );
        }
    }

    /// The transparency page renders `docs/EVENTS.md`, so a name that exists
    /// in code but not in the document would be captured without being
    /// disclosed. That must fail the build, not ship.
    #[test]
    fn vocabulary_matches_docs() {
        let docs = include_str!("../../../docs/EVENTS.md");
        let missing: Vec<&&str> = ACTION_VOCABULARY
            .iter()
            .filter(|name| !docs.contains(&format!("`{name}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "actions missing from docs/EVENTS.md: {missing:?}"
        );
    }

    #[test]
    fn envelope_round_trips() {
        let env = EventEnvelope {
            schema_version: SCHEMA_VERSION,
            event_id: "01J8Z9".into(),
            session_id: "01J8Z8".into(),
            actor_id: "u_7f3a".into(),
            workbook_id: "wb_19c2".into(),
            seq: 4172,
            ts_ms: 1_767_225_600_123,
            action: "cell.edit".into(),
            payload: json!({ "addr": "A1" }),
            context: EventContext {
                sheet: "Sheet1".into(),
                selection: "A1".into(),
                privacy_mode: PrivacyMode::Structural,
            },
            client_version: "0.1.0".into(),
        };
        let text = serde_json::to_string(&env).unwrap();
        let back: EventEnvelope = serde_json::from_str(&text).unwrap();
        assert_eq!(back.event_id, env.event_id);
        assert_eq!(back.context.privacy_mode, PrivacyMode::Structural);
        assert!(text.contains("\"schema_version\":1"));
    }
}
