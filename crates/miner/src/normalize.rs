//! Turning a captured event stream into abstract tokens.
//!
//! Mining works on *shape*. Two events are the same gesture when they do the
//! same thing, not when they do it to the same cell — typing `=SUM(B2:D2)` in
//! E2 and `=SUM(B3:D3)` in E3 is one habit repeated, and a tokenizer that
//! keeps the addresses would never notice. So a token records what happened
//! and the *relative* shape of any formula, and drops absolute position.
//!
//! Two rules follow from that, and both matter:
//!
//! * **References become R1C1 offsets from the writing cell.** That is what
//!   collapses a filled-down column into one repeated token.
//! * **Literal values never enter a token.** Under `structural` capture they
//!   are hashed anyway, but they must not distinguish two occurrences even
//!   under `full` capture: "typed a number here" is the habit, and the
//!   particular number is the thing we promised not to mine.
//!
//! Every token keeps the index of the envelope it came from, so a mined
//! pattern can be walked back to the concrete actions that made it.

use engine::ast::Expr;
use engine::parser::parse_formula;
use engine::telemetry::EventEnvelope;
use serde::{Deserialize, Serialize};
use std::fmt;

/// One normalized step. `Display` is the symbol the miners compare, so two
/// tokens are equal exactly when their rendered forms are.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Token {
    /// A typed literal. The kind is kept ("number", "text", "bool"); the
    /// value never is.
    Literal {
        kind: String,
    },
    /// A typed formula, reduced to its relative skeleton.
    Formula {
        shape: String,
    },
    Clear,
    Paste {
        values: bool,
        cut: bool,
    },
    Fill {
        down: bool,
    },
    RowInsert,
    RowDelete,
    ColInsert,
    ColDelete,
    Sort {
        keys: usize,
        has_header: bool,
    },
    FilterApply,
    FilterClear,
    /// One formatting attribute; a multi-attribute gesture yields one token
    /// per attribute so "bold then colour" and "bold and colour" mine alike.
    Format {
        attribute: String,
    },
    Replace,
    SheetAdd,
    SheetRename,
    SheetDelete,
    Undo,
    Redo,
    /// Anything the vocabulary grows that this version does not model.
    Other {
        action: String,
    },
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Literal { kind } => write!(f, "lit:{kind}"),
            Token::Formula { shape } => write!(f, "fx:{shape}"),
            Token::Clear => write!(f, "clear"),
            Token::Paste { values, cut } => write!(
                f,
                "paste:{}{}",
                if *values { "values" } else { "formulas" },
                if *cut { ":cut" } else { "" }
            ),
            Token::Fill { down } => write!(f, "fill:{}", if *down { "down" } else { "right" }),
            Token::RowInsert => write!(f, "row.insert"),
            Token::RowDelete => write!(f, "row.delete"),
            Token::ColInsert => write!(f, "col.insert"),
            Token::ColDelete => write!(f, "col.delete"),
            Token::Sort { keys, has_header } => write!(f, "sort:{keys}:{has_header}"),
            Token::FilterApply => write!(f, "filter.apply"),
            Token::FilterClear => write!(f, "filter.clear"),
            Token::Format { attribute } => write!(f, "fmt:{attribute}"),
            Token::Replace => write!(f, "replace"),
            Token::SheetAdd => write!(f, "sheet.add"),
            Token::SheetRename => write!(f, "sheet.rename"),
            Token::SheetDelete => write!(f, "sheet.delete"),
            Token::Undo => write!(f, "undo"),
            Token::Redo => write!(f, "redo"),
            Token::Other { action } => write!(f, "other:{action}"),
        }
    }
}

impl Token {
    /// A rough seconds cost of doing this by hand, used for scoring. These
    /// are estimates and are labelled as such wherever a number reaches the
    /// user; the ordering between them is what matters, not the absolute
    /// values.
    pub fn manual_seconds(&self) -> f64 {
        match self {
            Token::Formula { shape } => 4.0 + (shape.len() as f64 / 12.0).min(6.0),
            Token::Literal { .. } => 3.0,
            Token::Clear => 1.5,
            Token::Paste { .. } => 2.0,
            Token::Fill { .. } => 2.5,
            Token::RowInsert | Token::RowDelete | Token::ColInsert | Token::ColDelete => 2.5,
            Token::Sort { keys, .. } => 6.0 + 2.0 * (*keys as f64),
            Token::FilterApply => 6.0,
            Token::FilterClear => 1.5,
            Token::Format { .. } => 2.0,
            Token::Replace => 8.0,
            Token::SheetAdd | Token::SheetRename | Token::SheetDelete => 4.0,
            // Undo and redo are corrections, not work. Counting them as time
            // saved would reward routines that automate a user's mistakes.
            Token::Undo | Token::Redo => 0.0,
            Token::Other { .. } => 1.0,
        }
    }
}

/// A token plus where it came from.
#[derive(Debug, Clone)]
pub struct Step {
    pub token: Token,
    /// Index into the envelope slice this was normalized from.
    pub source: usize,
    pub session_id: String,
    pub ts_ms: i64,
}

/// Normalize a run of envelopes, dropping the ones that carry no gesture.
///
/// Navigation, capture control and consent events are excluded: they are
/// bookkeeping, and leaving them in would let "the user clicked around a bit"
/// become part of a mined routine.
pub fn normalize(events: &[EventEnvelope]) -> Vec<Step> {
    let mut out = Vec::with_capacity(events.len());
    for (i, e) in events.iter().enumerate() {
        for token in tokens_for(e) {
            out.push(Step {
                token,
                source: i,
                session_id: e.session_id.clone(),
                ts_ms: e.ts_ms,
            });
        }
    }
    out
}

/// One event can produce several tokens: a `format.apply` naming three
/// attributes is three steps, so that gesture mines the same whether the user
/// made it in one click or three.
fn tokens_for(e: &EventEnvelope) -> Vec<Token> {
    let p = &e.payload;
    match e.action.as_str() {
        "cell.edit" => {
            let is_formula = p["is_formula"].as_bool().unwrap_or(false);
            if is_formula {
                // Under `structural` the formula is verbatim, which is the
                // whole reason formulas are exempt from hashing.
                let src = p["input"].as_str().unwrap_or_default();
                let addr = p["addr"].as_str().unwrap_or("A1");
                vec![Token::Formula {
                    shape: formula_shape(src, addr),
                }]
            } else {
                vec![Token::Literal {
                    kind: literal_kind(&p["input"]),
                }]
            }
        }
        "cell.clear" => vec![Token::Clear],
        "range.paste" | "range.cut" => vec![Token::Paste {
            values: p["mode"].as_str() == Some("values"),
            cut: p["cut"].as_bool().unwrap_or(e.action == "range.cut"),
        }],
        "fill.apply" => vec![Token::Fill {
            down: p["direction"].as_str() != Some("right"),
        }],
        "row.insert" => vec![Token::RowInsert],
        "row.delete" => vec![Token::RowDelete],
        "col.insert" => vec![Token::ColInsert],
        "col.delete" => vec![Token::ColDelete],
        "sort.apply" => vec![Token::Sort {
            keys: p["keys"].as_array().map(|a| a.len()).unwrap_or(1),
            has_header: p["has_header"].as_bool().unwrap_or(false),
        }],
        "filter.apply" => vec![Token::FilterApply],
        "filter.clear" => vec![Token::FilterClear],
        "format.apply" => match p["attributes"].as_array() {
            Some(attrs) if !attrs.is_empty() => attrs
                .iter()
                .map(|a| Token::Format {
                    attribute: a.as_str().unwrap_or("style").to_string(),
                })
                .collect(),
            // Merge and unmerge arrive under the same vocabulary entry with
            // no attribute list.
            _ => vec![Token::Format {
                attribute: p["kind"].as_str().unwrap_or("style").to_string(),
            }],
        },
        "find.replace" => vec![Token::Replace],
        "sheet.add" => vec![Token::SheetAdd],
        "sheet.rename" => vec![Token::SheetRename],
        "sheet.delete" => vec![Token::SheetDelete],
        "undo" => vec![Token::Undo],
        "redo" => vec![Token::Redo],
        // Selection, capture control, consent and file lifecycle are not
        // gestures a routine can repeat.
        "nav.select" | "capture.pause" | "capture.resume" | "consent.granted"
        | "consent.revoked" | "file.new" | "file.open" | "file.import" | "file.export"
        | "file.save" | "routine.run" => Vec::new(),
        other => vec![Token::Other {
            action: other.to_string(),
        }],
    }
}

/// The `type` a redacted literal carries, or a guess from a verbatim one.
fn literal_kind(input: &serde_json::Value) -> String {
    if let Some(kind) = input["type"].as_str() {
        return kind.to_string();
    }
    match input.as_str() {
        None => "unknown".into(),
        Some(s) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") => {
            "bool".into()
        }
        Some(s) if s.trim().parse::<f64>().is_ok() => "number".into(),
        Some(_) => "text".into(),
    }
}

/// A formula reduced to its relative skeleton.
///
/// `=SUM(B2:D2)*$F$1` written in E2 becomes `SUM(R[0]C[-3]:R[0]C[-1])*R0C5`.
/// Absolute references keep their absolute coordinates, because moving the
/// gesture down a row would not move them either — the distinction is the
/// difference between a formula that fills and one that does not.
///
/// A formula we cannot parse — a whole-column reference, a named range, a
/// function from a later version — falls back to a *scrubbed* skeleton
/// rather than its raw text. Quoted strings and sheet qualifiers are the two
/// places user content hides in a formula, and the fallback path must not be
/// the one that leaks what the parsed path is careful to drop.
pub fn formula_shape(input: &str, addr_a1: &str) -> String {
    let body = input.strip_prefix('=').unwrap_or(input);
    let anchor = engine::CellAddr::parse_a1(addr_a1);
    match (anchor, parse_formula(body)) {
        (Some(a), Ok(ast)) => render_r1c1(&ast, a.row as i64, a.col as i64),
        _ => scrub_unparsed(body),
    }
}

/// Blank quoted strings and anonymize sheet qualifiers in text we could not
/// parse, then uppercase so the result is a stable token.
fn scrub_unparsed(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    // Start of the current run of characters that could still turn out to be
    // a sheet qualifier, i.e. `Name!` or `'Some name'!`.
    let mut word = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                out.push_str(&word.to_uppercase());
                word.clear();
                out.push_str("\"\"");
                i += 1;
                while i < chars.len() {
                    if chars[i] == '"' {
                        // A doubled quote is an escaped quote, not the end.
                        if chars.get(i + 1) == Some(&'"') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            '\'' => {
                out.push_str(&word.to_uppercase());
                word.clear();
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    i += 1;
                }
                i += 1; // closing quote
                        // A quoted name is only ever a sheet qualifier when followed
                        // by '!'; otherwise it was something we do not understand.
                if chars.get(i) == Some(&'!') {
                    out.push_str("X!");
                    i += 1;
                } else {
                    out.push_str("''");
                }
            }
            '!' => {
                // Everything accumulated since the last delimiter was a
                // sheet name.
                word.clear();
                out.push_str("X!");
                i += 1;
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' || c == '$' => {
                word.push(c);
                i += 1;
            }
            other => {
                out.push_str(&word.to_uppercase());
                word.clear();
                out.push(other);
                i += 1;
            }
        }
    }
    out.push_str(&word.to_uppercase());
    out
}

fn render_r1c1(e: &Expr, row: i64, col: i64) -> String {
    match e {
        Expr::Number(n) => engine::value::format_number_general(*n),
        // A string literal inside a formula is user content. Its presence is
        // structural; its contents are not.
        Expr::Text(_) => "\"\"".to_string(),
        Expr::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Expr::Error(k) => k.code().to_string(),
        Expr::Cell(c) => {
            format!("{}{}", sheet_prefix(&c.sheet), r1c1(&c.r, row, col))
        }
        Expr::Range(r) => format!(
            "{}{}:{}",
            sheet_prefix(&r.sheet),
            r1c1(&r.start, row, col),
            r1c1(&r.end, row, col)
        ),
        Expr::Func(name, args) => format!(
            "{}({})",
            name,
            args.iter()
                .map(|a| render_r1c1(a, row, col))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Expr::Binary(op, l, r) => format!(
            "({}{}{})",
            render_r1c1(l, row, col),
            op.symbol(),
            render_r1c1(r, row, col)
        ),
        Expr::Neg(x) => format!("-{}", render_r1c1(x, row, col)),
        Expr::Pos(x) => format!("+{}", render_r1c1(x, row, col)),
        Expr::Percent(x) => format!("{}%", render_r1c1(x, row, col)),
    }
}

/// Sheet names are user content, so a cross-sheet reference is marked as
/// such without naming the sheet. Two formulas that reach into *a* different
/// sheet still mine together; which sheet stays out of the token.
fn sheet_prefix(sheet: &Option<String>) -> &'static str {
    match sheet {
        Some(_) => "X!",
        None => "",
    }
}

fn r1c1(r: &engine::addr::ParsedRef, row: i64, col: i64) -> String {
    let rp = if r.abs_row {
        format!("R{}", r.row)
    } else {
        format!("R[{}]", r.row as i64 - row)
    };
    let cp = if r.abs_col {
        format!("C{}", r.col)
    } else {
        format!("C[{}]", r.col as i64 - col)
    };
    format!("{rp}{cp}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::telemetry::{EventContext, PrivacyMode, SCHEMA_VERSION};
    use serde_json::json;

    fn env(action: &str, payload: serde_json::Value) -> EventEnvelope {
        EventEnvelope {
            schema_version: SCHEMA_VERSION,
            event_id: "e".into(),
            session_id: "s".into(),
            actor_id: "a".into(),
            workbook_id: "w".into(),
            seq: 0,
            ts_ms: 0,
            action: action.into(),
            payload,
            context: EventContext {
                sheet: "S".into(),
                selection: "A1".into(),
                privacy_mode: PrivacyMode::Structural,
            },
            client_version: "0".into(),
        }
    }

    fn one(action: &str, payload: serde_json::Value) -> Token {
        tokens_for(&env(action, payload)).remove(0)
    }

    #[test]
    fn the_same_formula_filled_down_is_one_token() {
        let a = formula_shape("=SUM(B2:D2)", "E2");
        let b = formula_shape("=SUM(B3:D3)", "E3");
        let c = formula_shape("=SUM(B9:D9)", "E9");
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a, "SUM(R[0]C[-3]:R[0]C[-1])");
    }

    #[test]
    fn a_different_shape_is_a_different_token() {
        assert_ne!(
            formula_shape("=SUM(B2:D2)", "E2"),
            formula_shape("=SUM(B2:C2)", "E2")
        );
        assert_ne!(
            formula_shape("=SUM(B2:D2)", "E2"),
            formula_shape("=AVERAGE(B2:D2)", "E2")
        );
    }

    #[test]
    fn absolute_references_stay_absolute() {
        // The distinction is the difference between a formula that fills and
        // one that does not, so it has to survive normalization.
        let a = formula_shape("=B2*$F$1", "E2");
        let b = formula_shape("=B3*$F$1", "E3");
        assert_eq!(a, b);
        assert!(a.contains("R0C5"), "{a}");
        assert_ne!(a, formula_shape("=B2*F1", "E2"));
    }

    #[test]
    fn literal_values_never_reach_a_token() {
        // Even under `full` capture: the habit is "typed a number here", and
        // the number itself is what we promised not to mine.
        let t = one(
            "cell.edit",
            json!({ "addr": "A1", "input": "48250", "is_formula": false }),
        );
        assert_eq!(t.to_string(), "lit:number");
        let hashed = one(
            "cell.edit",
            json!({ "addr": "A1", "input": { "hash": "ab", "type": "number", "len": 5 },
                    "is_formula": false }),
        );
        assert_eq!(t, hashed, "hashed and verbatim literals must mine alike");
    }

    #[test]
    fn text_inside_a_formula_is_blanked_but_its_presence_is_kept() {
        let s = formula_shape("=IF(A1>2,\"overdue\",\"ok\")", "B1");
        assert!(!s.contains("overdue"), "{s}");
        assert_eq!(s, formula_shape("=IF(A1>2,\"chase\",\"fine\")", "B1"));
        // ...but a formula with no strings is still distinguishable.
        assert_ne!(s, formula_shape("=IF(A1>2,1,0)", "B1"));
    }

    #[test]
    fn cross_sheet_references_do_not_carry_the_sheet_name() {
        let s = formula_shape("=VLOOKUP(A2,Payroll!A:B,2,FALSE)", "C2");
        assert!(!s.contains("Payroll"), "{s}");
        assert!(s.contains("X!"), "{s}");
    }

    #[test]
    fn an_unparseable_formula_still_yields_a_stable_token() {
        let a = formula_shape("=1+", "A1");
        let b = formula_shape("=1+", "B7");
        assert_eq!(a, b);
    }

    #[test]
    fn the_unparseable_fallback_leaks_neither_strings_nor_sheet_names() {
        // Whole-column references do not parse in v1, so this is the path a
        // real workbook takes — and it must scrub what the parsed path drops,
        // or the fallback becomes the leak.
        let s = formula_shape("=VLOOKUP(A2,Payroll!A:B,2,\"secret\")", "C2");
        assert!(!s.contains("Payroll"), "{s}");
        assert!(!s.contains("secret"), "{s}");
        assert!(s.contains("X!"), "{s}");
        assert!(s.contains("VLOOKUP"), "{s}");

        // Quoted sheet names too.
        let q = formula_shape("='Payroll Q3'!A:B", "A1");
        assert!(!q.contains("Payroll"), "{q}");

        // Two formulas differing only in a string still mine together.
        assert_eq!(
            formula_shape("=NOPE(A:B,\"one\")", "A1"),
            formula_shape("=NOPE(A:B,\"two\")", "A1")
        );
        // ...and two genuinely different ones do not.
        assert_ne!(
            formula_shape("=NOPE(A:B)", "A1"),
            formula_shape("=NOPE(A:C)", "A1")
        );
    }

    #[test]
    fn a_multi_attribute_format_becomes_one_token_per_attribute() {
        let tokens = tokens_for(&env(
            "format.apply",
            json!({ "kind": "style", "attributes": ["bold", "fill_color"] }),
        ));
        assert_eq!(
            tokens.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
            vec!["fmt:bold", "fmt:fill_color"]
        );
    }

    #[test]
    fn merge_arrives_without_an_attribute_list_and_still_tokenizes() {
        let t = one("format.apply", json!({ "kind": "merge", "range": "A1:C1" }));
        assert_eq!(t.to_string(), "fmt:merge");
    }

    #[test]
    fn bookkeeping_events_produce_no_steps() {
        for action in [
            "nav.select",
            "capture.pause",
            "consent.granted",
            "file.export",
            "routine.run",
        ] {
            assert!(
                tokens_for(&env(action, json!({}))).is_empty(),
                "{action} should not be minable"
            );
        }
    }

    #[test]
    fn an_unknown_action_is_kept_rather_than_dropped() {
        // A vocabulary that grows must not silently vanish from the miner's
        // view; an unknown token simply never reaches support.
        assert_eq!(
            one("pivot.create", json!({})).to_string(),
            "other:pivot.create"
        );
    }

    #[test]
    fn corrections_are_worth_no_time() {
        assert_eq!(Token::Undo.manual_seconds(), 0.0);
        assert_eq!(Token::Redo.manual_seconds(), 0.0);
    }

    #[test]
    fn normalize_keeps_a_route_back_to_the_source_event() {
        let events = vec![
            env("nav.select", json!({})),
            env(
                "cell.edit",
                json!({ "addr": "A1", "input": "1", "is_formula": false }),
            ),
            env(
                "format.apply",
                json!({ "kind": "style", "attributes": ["bold", "italic"] }),
            ),
        ];
        let steps = normalize(&events);
        assert_eq!(steps.len(), 3, "nav is dropped, format yields two");
        assert_eq!(steps[0].source, 1);
        assert_eq!(steps[1].source, 2);
        assert_eq!(steps[2].source, 2);
    }
}
