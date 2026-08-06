//! What a planner is allowed to say.
//!
//! The single most important property of this file is what is *missing* from
//! it. There is no `RunCode`, no `Eval`, no `ClickAt`, no free-text action.
//! A planner returns values of `Step` or it returns nothing, which means:
//!
//! * **Every plan can be checked before it runs.** A validator can read
//!   `CreateDerivedColumn { after: "Price", header: "Total", .. }` and say
//!   what it will touch. It cannot say that about a string of Rust.
//! * **A plan survives the sheet moving.** Steps name *headers*, *named
//!   ranges* and *patterns*; the compiler turns those into addresses against
//!   the workbook actually in front of it. A plan that said `D2:D400` would
//!   be a macro, and a macro breaks the moment somebody inserts a row.
//! * **A failure is attributable.** When a task fails it is either the plan
//!   or the resolution, and they are separately inspectable.
//!
//! The cost, stated plainly: a planner can only express what this enum can
//! express. Anything else has to be added here first, deliberately, with a
//! compiler and a validator to match. That is the trade being made — a
//! narrower agent that can be reasoned about, over a general one that cannot.

use serde::{Deserialize, Serialize};

/// Which column, said the way a person would say it.
///
/// Never an index. A column named by its header still means the same column
/// after somebody inserts one to the left of it, and the whole point of the
/// planner/compiler split is that the planner does not have to know where
/// anything is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "column", rename_all = "snake_case")]
pub enum ColumnRef {
    /// Matched against detected table headers, case- and space-insensitively.
    Header { text: String },
    /// A workbook-level defined name.
    Name { name: String },
    /// The column immediately right of the table's last one — where a new
    /// derived column goes when the plan does not say otherwise.
    NextFree,
}

/// Which rows a step applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rows", rename_all = "snake_case")]
pub enum RowRange {
    /// Every body row of the table: header + 1 down to the last populated
    /// row. What "fill it down" almost always means.
    TableBody,
    /// A literal span, 1-based and inclusive, for the cases where it really
    /// is specific rows.
    Rows { first: u32, last: u32 },
}

/// A formula expressed against headers rather than addresses.
///
/// `{Qty} * {Price}` compiles to `=B2*C2` on one sheet and `=E7*G7` on
/// another, and the plan is the same plan. The braces are deliberately not
/// valid formula syntax, so a template that reached the engine unresolved
/// fails loudly instead of evaluating to something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormulaTemplate(pub String);

impl FormulaTemplate {
    pub fn new(text: impl Into<String>) -> Self {
        FormulaTemplate(text.into())
    }

    /// The header names this template mentions, in order of appearance.
    pub fn placeholders(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = self.0.as_str();
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else { break };
            out.push(after[..close].trim().to_string());
            rest = &after[close + 1..];
        }
        out
    }
}

/// How a range of rows should be reduced to one number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    Sum,
    Average,
    Count,
    Min,
    Max,
}

impl Aggregate {
    pub fn function(&self) -> &'static str {
        match self {
            Aggregate::Sum => "SUM",
            Aggregate::Average => "AVERAGE",
            Aggregate::Count => "COUNT",
            Aggregate::Min => "MIN",
            Aggregate::Max => "MAX",
        }
    }
}

/// A comparison a filter can make.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "predicate", rename_all = "snake_case")]
pub enum Predicate {
    Equals { value: String },
    NotEquals { value: String },
    GreaterThan { value: f64 },
    LessThan { value: f64 },
    Contains { text: String },
    IsBlank,
    IsNotBlank,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Find the table to work on and make it the subject of what follows.
    ///
    /// Produces no actions — it changes what the compiler resolves against.
    /// A step rather than an implicit "the first table" because a workbook
    /// with three tables needs the plan to say which one, and because a plan
    /// that names its subject out loud is one a person can read.
    LocateTable {
        /// The sheet, if the plan is sure. `None` means "wherever the
        /// headers below are".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sheet: Option<String>,
        /// Headers that must be present. The compiler picks the detected
        /// table that has all of them.
        must_have: Vec<String>,
    },
    /// Add a new column computed from existing ones.
    CreateDerivedColumn {
        header: String,
        /// Where it goes. `NextFree` is the usual answer.
        #[serde(default = "next_free")]
        at: ColumnRef,
        formula: FormulaTemplate,
        #[serde(default = "table_body")]
        rows: RowRange,
    },
    /// Put one formula in one place — a grand total, a reconciliation cell.
    ApplyFormula {
        /// `Sheet!A1`, or a bare `A1` on the located table's sheet.
        at: String,
        formula: FormulaTemplate,
    },
    /// Fill an existing column's formula down over the body rows.
    ///
    /// Distinct from `CreateDerivedColumn` because "somebody already wrote
    /// the first one, continue it" is a different intent from "make this
    /// column", and the compiler resolves them differently: this one reads
    /// the formula that is already there.
    FillRange {
        column: ColumnRef,
        #[serde(default = "table_body")]
        rows: RowRange,
    },
    /// Hide the rows that do not match.
    FilterRows {
        column: ColumnRef,
        predicate: Predicate,
    },
    /// Assert that two columns agree, and record the difference where they
    /// do not: the debits-equal-credits gesture, as one step.
    ReconcileTotals {
        left: ColumnRef,
        right: ColumnRef,
        /// Where to put the variance formula. Omitted means "do not write
        /// anything, just check" — which still compiles to no actions and is
        /// still worth saying, because the grader will check it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variance_at: Option<String>,
        #[serde(default)]
        aggregate_with: Option<Aggregate>,
    },
    /// Finish: the workbook is in the state the task asked for.
    ///
    /// Compiles to no actions. It exists so that "I am done" is something
    /// the planner *says* rather than something inferred from it running out
    /// of ideas — which is the difference between `Termination::Done` and
    /// `Termination::BudgetExhausted`, and they mean different things.
    ExportWorkbook {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

fn next_free() -> ColumnRef {
    ColumnRef::NextFree
}

fn table_body() -> RowRange {
    RowRange::TableBody
}

impl Step {
    /// A short label for logs and for clustering.
    pub fn kind(&self) -> &'static str {
        match self {
            Step::LocateTable { .. } => "locate_table",
            Step::CreateDerivedColumn { .. } => "create_derived_column",
            Step::ApplyFormula { .. } => "apply_formula",
            Step::FillRange { .. } => "fill_range",
            Step::FilterRows { .. } => "filter_rows",
            Step::ReconcileTotals { .. } => "reconcile_totals",
            Step::ExportWorkbook { .. } => "export_workbook",
        }
    }

    /// Whether this step ends the episode.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Step::ExportWorkbook { .. })
    }
}

/// An ordered plan, with the planner's reason for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Why, in the planner's words. Not executed and not parsed — kept
    /// because a plan nobody can follow the reasoning of is a plan nobody
    /// will trust enough to review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// How sure the planner is, 0.0 to 1.0. Read by the router, which sends
    /// a low-confidence proposal to the more expensive planner rather than
    /// letting the cheap one commit it.
    #[serde(default = "full_confidence")]
    pub confidence: f32,
}

fn full_confidence() -> f32 {
    1.0
}

impl Plan {
    pub fn new(steps: Vec<Step>) -> Self {
        Plan {
            steps,
            rationale: None,
            confidence: 1.0,
        }
    }

    pub fn with_confidence(mut self, c: f32) -> Self {
        self.confidence = c;
        self
    }

    pub fn because(mut self, why: impl Into<String>) -> Self {
        self.rationale = Some(why.into());
        self
    }

    /// The plan's shape, with the specifics stripped out: what gets clustered
    /// when looking for a repeated solution.
    ///
    /// `create_derived_column({a}*{b}) -> fill_range -> export` is the same
    /// shape whether the columns were Qty and Price or Hours and Rate, and
    /// that is exactly the generalization a micro-policy is.
    pub fn shape(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|s| match s {
                Step::CreateDerivedColumn { formula, .. } | Step::ApplyFormula { formula, .. } => {
                    format!("{}:{}", s.kind(), anonymize(&formula.0))
                }
                other => other.kind().to_string(),
            })
            .collect()
    }
}

/// Replace every `{Header}` with a positional slot, so two formulas that
/// differ only in which columns they name have the same shape.
fn anonymize(template: &str) -> String {
    let mut out = String::new();
    let mut seen: Vec<String> = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = after[..close].trim().to_string();
        let slot = match seen.iter().position(|s| *s == name) {
            Some(i) => i,
            None => {
                seen.push(name);
                seen.len() - 1
            }
        };
        out.push_str(&format!("{{{slot}}}"));
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_names_the_headers_it_needs() {
        let t = FormulaTemplate::new("={Qty} * {Unit Price}");
        assert_eq!(t.placeholders(), ["Qty", "Unit Price"]);
    }

    #[test]
    fn an_unterminated_placeholder_is_not_a_placeholder() {
        // It will fail to resolve and the step will be refused, which is the
        // right outcome; silently treating the rest of the formula as a
        // header name would produce a confusing error much later.
        assert!(FormulaTemplate::new("={Qty").placeholders().is_empty());
    }

    #[test]
    fn two_plans_that_differ_only_in_column_names_have_the_same_shape() {
        // The generalization a micro-policy is: "multiply two columns and
        // fill down" is one habit, not one per pair of column names.
        let a = Plan::new(vec![
            Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Qty}*{Price}"),
                rows: RowRange::TableBody,
            },
            Step::ExportWorkbook { path: None },
        ]);
        let b = Plan::new(vec![
            Step::CreateDerivedColumn {
                header: "Fee".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Hours}*{Rate}"),
                rows: RowRange::TableBody,
            },
            Step::ExportWorkbook { path: None },
        ]);
        assert_eq!(a.shape(), b.shape());
    }

    #[test]
    fn a_repeated_header_keeps_its_slot() {
        // `{A}-{B}` and `{A}-{A}` are different shapes: the second is
        // always zero and the first is not.
        let one = Plan::new(vec![Step::ApplyFormula {
            at: "D1".into(),
            formula: FormulaTemplate::new("={Debit}-{Credit}"),
        }]);
        let same = Plan::new(vec![Step::ApplyFormula {
            at: "Z9".into(),
            formula: FormulaTemplate::new("={In}-{Out}"),
        }]);
        let degenerate = Plan::new(vec![Step::ApplyFormula {
            at: "D1".into(),
            formula: FormulaTemplate::new("={Debit}-{Debit}"),
        }]);
        assert_eq!(one.shape(), same.shape());
        assert_ne!(one.shape(), degenerate.shape());
    }

    #[test]
    fn a_different_operation_is_a_different_shape() {
        let mul = Plan::new(vec![Step::ApplyFormula {
            at: "D1".into(),
            formula: FormulaTemplate::new("={A}*{B}"),
        }]);
        let add = Plan::new(vec![Step::ApplyFormula {
            at: "D1".into(),
            formula: FormulaTemplate::new("={A}+{B}"),
        }]);
        assert_ne!(mul.shape(), add.shape());
    }

    #[test]
    fn a_plan_round_trips_through_json() {
        // It is going into a dataset and coming back out of a model.
        let plan = Plan::new(vec![
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into(), "Price".into()],
            },
            Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Qty}*{Price}"),
                rows: RowRange::TableBody,
            },
            Step::ExportWorkbook { path: None },
        ])
        .because("Total is quantity times price, filled over the body")
        .with_confidence(0.8);
        let text = serde_json::to_string(&plan).unwrap();
        assert_eq!(plan, serde_json::from_str::<Plan>(&text).unwrap());
    }

    #[test]
    fn only_the_export_step_ends_an_episode() {
        assert!(Step::ExportWorkbook { path: None }.is_terminal());
        assert!(!Step::FillRange {
            column: ColumnRef::NextFree,
            rows: RowRange::TableBody
        }
        .is_terminal());
    }
}
