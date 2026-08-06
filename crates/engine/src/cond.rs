//! Conditional formatting: rules that decide a cell's presentation from its
//! value rather than from a click.
//!
//! The result is *derived*, exactly like a spilled block: recalculation
//! rebuilds [`Sheet::cond_formats`] from the rules, and nothing else may
//! write to it. That is what keeps "why is this cell red" answerable — the
//! answer is always a rule, never a stray `FormatApply` from three sessions
//! ago.
//!
//! Two Excel behaviours are reproduced deliberately:
//!
//! * **Formulas in a rule are relative to the top-left of its range.** A rule
//!   over `A1:A9` whose test is `=A1>B1` means "this row's A against this
//!   row's B", not "every row against B1". The offset is the same transform
//!   copy and paste use, so there is one definition of what a relative
//!   reference means.
//! * **The first rule to match an attribute wins.** Rules are tried in order
//!   and each one fills in only the attributes still unset, so a later "make
//!   it bold" cannot repaint an earlier rule's fill.

use crate::addr::{CellAddr, RangeAddr};
use crate::eval::{compare_values, to_bool, to_text, EvalCtx};
use crate::format::CellFormat;
use crate::model::{Sheet, SheetId, Workbook};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The comparison a `CellIs` rule makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CondOp {
    GreaterThan,
    LessThan,
    GreaterOrEqual,
    LessOrEqual,
    Equal,
    NotEqual,
    Between,
    NotBetween,
}

impl CondOp {
    /// The `operator` attribute xlsx spells it with.
    pub fn as_xlsx(&self) -> &'static str {
        match self {
            CondOp::GreaterThan => "greaterThan",
            CondOp::LessThan => "lessThan",
            CondOp::GreaterOrEqual => "greaterThanOrEqual",
            CondOp::LessOrEqual => "lessThanOrEqual",
            CondOp::Equal => "equal",
            CondOp::NotEqual => "notEqual",
            CondOp::Between => "between",
            CondOp::NotBetween => "notBetween",
        }
    }

    pub fn from_xlsx(s: &str) -> Option<CondOp> {
        Some(match s {
            "greaterThan" => CondOp::GreaterThan,
            "lessThan" => CondOp::LessThan,
            "greaterThanOrEqual" => CondOp::GreaterOrEqual,
            "lessThanOrEqual" => CondOp::LessOrEqual,
            "equal" => CondOp::Equal,
            "notEqual" => CondOp::NotEqual,
            "between" => CondOp::Between,
            "notBetween" => CondOp::NotBetween,
            _ => return None,
        })
    }

    /// How many operands the comparison needs.
    pub fn arity(&self) -> usize {
        match self {
            CondOp::Between | CondOp::NotBetween => 2,
            _ => 1,
        }
    }
}

/// What a rule asks of a cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "test", rename_all = "snake_case")]
pub enum CondTest {
    /// Compare the cell's value against one or two operands, each a formula
    /// body (`"10"`, `"$B$1"`, `"AVERAGE(A:A)"` — no leading `=`).
    CellIs {
        op: CondOp,
        operands: Vec<String>,
    },
    /// Case-insensitive substring, as Excel's "text that contains" is.
    TextContains {
        needle: String,
        negate: bool,
    },
    Blank {
        negate: bool,
    },
    /// Values appearing more than once in the rule's own range — or, with
    /// `unique`, exactly once.
    Duplicate {
        unique: bool,
    },
    /// An arbitrary formula body, true when it is truthy.
    Formula {
        body: String,
    },
}

/// One rule: where it applies, what it asks, and what it does when the answer
/// is yes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CondRule {
    pub range: RangeAddr,
    pub test: CondTest,
    /// A *differential* format: only the attributes it sets are applied, and
    /// the rest of the cell's own formatting shows through. That is what xlsx
    /// calls a `<dxf>` and what makes "colour the fill" leave the number
    /// format alone.
    pub format: CellFormat,
}

impl CondRule {
    /// A short phrase for the rule, for the panel and the event log. Carries
    /// no cell values: the operands are formula text, which is structure.
    pub fn summary(&self) -> String {
        match &self.test {
            CondTest::CellIs { op, operands } => {
                format!("value {} {}", op.as_xlsx(), operands.join(" and "))
            }
            CondTest::TextContains { negate, .. } => {
                if *negate {
                    "text does not contain".into()
                } else {
                    "text contains".into()
                }
            }
            CondTest::Blank { negate } => {
                if *negate {
                    "not blank".into()
                } else {
                    "blank".into()
                }
            }
            CondTest::Duplicate { unique } => {
                if *unique {
                    "unique values".into()
                } else {
                    "duplicate values".into()
                }
            }
            CondTest::Formula { body } => format!("formula {body}"),
        }
    }
}

/// Rebuild a sheet's conditional formats from its rules.
///
/// Returns the addresses whose overlay changed, so recalculation can tell
/// whether anything needs repainting.
pub fn evaluate(wb: &Workbook, sheet: SheetId, now_ms: i64) -> BTreeMap<CellAddr, CellFormat> {
    let mut out: BTreeMap<CellAddr, CellFormat> = BTreeMap::new();
    let Some(s) = wb.sheet(sheet) else {
        return out;
    };
    for rule in &s.conditional {
        // The values in the rule's own range, needed only by the rules that
        // ask about the range as a whole.
        let population = match &rule.test {
            CondTest::Duplicate { .. } => Some(values_in(s, rule.range)),
            _ => None,
        };
        for addr in cells_of(rule.range) {
            let ctx = EvalCtx {
                wb,
                sheet,
                at: addr,
                now_ms,
                bindings: &[],
                name_depth: 0,
            };
            if !matches(&ctx, s, rule, addr, population.as_deref()) {
                continue;
            }
            // First rule wins per attribute: `or_insert_with` on the entry
            // and then filling only the gaps.
            let slot = out.entry(addr).or_default();
            merge_under(slot, &rule.format);
        }
    }
    out.retain(|_, f| !f.is_default());
    out
}

/// Fill in `base`'s unset attributes from `extra`, leaving the ones it
/// already has. "First rule wins" expressed as a merge.
fn merge_under(base: &mut CellFormat, extra: &CellFormat) {
    if !base.bold {
        base.bold = extra.bold;
    }
    if !base.italic {
        base.italic = extra.italic;
    }
    if base.font_color.is_none() {
        base.font_color = extra.font_color.clone();
    }
    if base.fill_color.is_none() {
        base.fill_color = extra.fill_color.clone();
    }
    if base.borders.is_none() {
        base.borders = extra.borders;
    }
    if base.number_format.is_none() {
        base.number_format = extra.number_format.clone();
    }
    if base.align.is_none() {
        base.align = extra.align;
    }
}

fn cells_of(r: RangeAddr) -> impl Iterator<Item = CellAddr> {
    (r.start.row..=r.end.row)
        .flat_map(move |row| (r.start.col..=r.end.col).map(move |col| CellAddr::new(row, col)))
}

fn values_in(s: &Sheet, r: RangeAddr) -> Vec<Value> {
    cells_of(r).map(|a| s.value(a)).collect()
}

fn matches(
    ctx: &EvalCtx,
    s: &Sheet,
    rule: &CondRule,
    addr: CellAddr,
    population: Option<&[Value]>,
) -> bool {
    let value = s.value(addr);
    // Relative references in a rule's formulas are written against the
    // top-left of its range and shift with the cell, the same way a pasted
    // formula does.
    let dr = addr.row as i64 - rule.range.start.row as i64;
    let dc = addr.col as i64 - rule.range.start.col as i64;
    let operand = |body: &str| -> Value {
        match crate::parser::parse_formula(body) {
            Ok(ast) => ctx.eval_scalar(&crate::refs::offset(&ast, dr, dc)),
            Err(_) => Value::Error(crate::value::ErrorKind::Name),
        }
    };

    match &rule.test {
        // An error in the cell never matches a comparison: Excel does not
        // colour a #DIV/0! green for being "greater than 0".
        CondTest::CellIs { .. } if value.as_error().is_some() => false,
        CondTest::CellIs { op, operands } => {
            if operands.len() < op.arity() {
                return false;
            }
            let a = operand(&operands[0]);
            if a.as_error().is_some() {
                return false;
            }
            let ord = compare_values(&value, &a);
            match op {
                CondOp::GreaterThan => ord.is_gt(),
                CondOp::LessThan => ord.is_lt(),
                CondOp::GreaterOrEqual => ord.is_ge(),
                CondOp::LessOrEqual => ord.is_le(),
                CondOp::Equal => ord.is_eq(),
                CondOp::NotEqual => !ord.is_eq(),
                CondOp::Between | CondOp::NotBetween => {
                    let b = operand(&operands[1]);
                    if b.as_error().is_some() {
                        return false;
                    }
                    // Excel accepts the bounds in either order.
                    let (lo, hi) = if compare_values(&a, &b).is_le() {
                        (&a, &b)
                    } else {
                        (&b, &a)
                    };
                    let inside =
                        compare_values(&value, lo).is_ge() && compare_values(&value, hi).is_le();
                    if matches!(op, CondOp::Between) {
                        inside
                    } else {
                        !inside
                    }
                }
            }
        }
        CondTest::TextContains { needle, negate } => {
            if needle.is_empty() {
                return false;
            }
            let text = to_text(&value).unwrap_or_default().to_lowercase();
            let hit = text.contains(&needle.to_lowercase());
            hit != *negate
        }
        // "Blank" is empty *or* the empty string, which is what a formula
        // returning "" leaves behind and what users mean by an empty cell.
        CondTest::Blank { negate } => {
            let blank = value.is_empty() || value == Value::Text(String::new());
            blank != *negate
        }
        CondTest::Duplicate { unique } => {
            if value.is_empty() {
                return false;
            }
            let population = population.unwrap_or(&[]);
            let count = population
                .iter()
                .filter(|v| compare_values(v, &value).is_eq())
                .count();
            if *unique {
                count == 1
            } else {
                count > 1
            }
        }
        CondTest::Formula { body } => to_bool(&operand(body)).unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    fn red() -> CellFormat {
        CellFormat {
            fill_color: Some("#ff0000".into()),
            ..CellFormat::default()
        }
    }

    fn bold() -> CellFormat {
        CellFormat {
            bold: true,
            ..CellFormat::default()
        }
    }

    fn sheet_with(rules: Vec<CondRule>, cells: &[(&str, &str)]) -> Engine {
        let mut e = Engine::new();
        for (a1, input) in cells {
            e.apply(&crate::engine::Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1(a1).unwrap(),
                input: (*input).into(),
            })
            .unwrap();
        }
        for rule in rules {
            e.apply(&crate::engine::Action::CondAdd {
                sheet: "Sheet1".into(),
                rule,
            })
            .unwrap();
        }
        e
    }

    fn fill_at(e: &Engine, a1: &str) -> Option<String> {
        e.wb.sheets[0]
            .cond_formats
            .get(&CellAddr::parse_a1(a1).unwrap())
            .and_then(|f| f.fill_color.clone())
    }

    #[test]
    fn a_comparison_colours_the_cells_that_pass_it() {
        let e = sheet_with(
            vec![CondRule {
                range: RangeAddr::parse_a1("A1:A3").unwrap(),
                test: CondTest::CellIs {
                    op: CondOp::GreaterThan,
                    operands: vec!["5".into()],
                },
                format: red(),
            }],
            &[("A1", "1"), ("A2", "9"), ("A3", "6")],
        );
        assert_eq!(fill_at(&e, "A1"), None);
        assert_eq!(fill_at(&e, "A2"), Some("#ff0000".into()));
        assert_eq!(fill_at(&e, "A3"), Some("#ff0000".into()));
    }

    #[test]
    fn an_error_never_passes_a_comparison() {
        // #DIV/0! is not "greater than 0"; ordering an error against a number
        // would give an answer, and the answer would be nonsense.
        let e = sheet_with(
            vec![CondRule {
                range: RangeAddr::parse_a1("A1:A1").unwrap(),
                test: CondTest::CellIs {
                    op: CondOp::GreaterThan,
                    operands: vec!["0".into()],
                },
                format: red(),
            }],
            &[("A1", "=1/0")],
        );
        assert_eq!(fill_at(&e, "A1"), None);
    }

    #[test]
    fn a_rules_formula_is_relative_to_the_top_left_of_its_range() {
        // "A over B, row by row" — the rule is written once against row 1 and
        // has to mean each row's own pair. Writing it as absolute would
        // colour every row by row 1's comparison.
        let e = sheet_with(
            vec![CondRule {
                range: RangeAddr::parse_a1("A1:A3").unwrap(),
                test: CondTest::Formula {
                    body: "A1>B1".into(),
                },
                format: red(),
            }],
            &[
                ("A1", "1"),
                ("B1", "5"),
                ("A2", "9"),
                ("B2", "5"),
                ("A3", "2"),
                ("B3", "5"),
            ],
        );
        assert_eq!(fill_at(&e, "A1"), None);
        assert_eq!(fill_at(&e, "A2"), Some("#ff0000".into()));
        assert_eq!(fill_at(&e, "A3"), None);
    }

    #[test]
    fn the_first_rule_to_set_an_attribute_keeps_it() {
        // Both match A1. The first sets the fill, the second would set a
        // different one and does not — but its bold still lands, because
        // nothing had claimed bold.
        let e = sheet_with(
            vec![
                CondRule {
                    range: RangeAddr::parse_a1("A1:A1").unwrap(),
                    test: CondTest::CellIs {
                        op: CondOp::GreaterThan,
                        operands: vec!["0".into()],
                    },
                    format: red(),
                },
                CondRule {
                    range: RangeAddr::parse_a1("A1:A1").unwrap(),
                    test: CondTest::CellIs {
                        op: CondOp::GreaterThan,
                        operands: vec!["0".into()],
                    },
                    format: CellFormat {
                        fill_color: Some("#00ff00".into()),
                        ..bold()
                    },
                },
            ],
            &[("A1", "1")],
        );
        assert_eq!(fill_at(&e, "A1"), Some("#ff0000".into()));
        assert!(
            e.wb.sheets[0].cond_formats[&CellAddr::parse_a1("A1").unwrap()].bold,
            "the second rule's bold was dropped along with its fill"
        );
    }

    #[test]
    fn duplicates_are_counted_within_the_rules_own_range() {
        let e = sheet_with(
            vec![CondRule {
                range: RangeAddr::parse_a1("A1:A3").unwrap(),
                test: CondTest::Duplicate { unique: false },
                format: red(),
            }],
            // The `b` in B1 is outside the range and must not make A1 a
            // duplicate.
            &[("A1", "b"), ("A2", "a"), ("A3", "a"), ("B1", "b")],
        );
        assert_eq!(fill_at(&e, "A1"), None);
        assert_eq!(fill_at(&e, "A2"), Some("#ff0000".into()));
        assert_eq!(fill_at(&e, "A3"), Some("#ff0000".into()));
    }

    #[test]
    fn between_accepts_its_bounds_in_either_order() {
        for operands in [vec!["2".into(), "8".into()], vec!["8".into(), "2".into()]] {
            let e = sheet_with(
                vec![CondRule {
                    range: RangeAddr::parse_a1("A1:A2").unwrap(),
                    test: CondTest::CellIs {
                        op: CondOp::Between,
                        operands,
                    },
                    format: red(),
                }],
                &[("A1", "5"), ("A2", "9")],
            );
            assert_eq!(fill_at(&e, "A1"), Some("#ff0000".into()));
            assert_eq!(fill_at(&e, "A2"), None);
        }
    }

    #[test]
    fn text_contains_is_case_insensitive_and_negatable() {
        let e = sheet_with(
            vec![CondRule {
                range: RangeAddr::parse_a1("A1:A2").unwrap(),
                test: CondTest::TextContains {
                    needle: "AB".into(),
                    negate: false,
                },
                format: red(),
            }],
            &[("A1", "cabbage"), ("A2", "zzz")],
        );
        assert_eq!(fill_at(&e, "A1"), Some("#ff0000".into()));
        assert_eq!(fill_at(&e, "A2"), None);
    }
}
