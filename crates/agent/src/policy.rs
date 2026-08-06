//! Planners, and the rule for choosing between them.
//!
//! Two implementations live here and they are not rivals — they are the two
//! ends of a cost curve. `RulePlanner` is deterministic, instant and free,
//! and covers a small vocabulary. Anything more capable is expensive per
//! call. `Router` decides which one answers, and the decision is the point:
//!
//! * the cheap planner answers only when it is **confident**, and
//! * the moment anything has gone wrong, the expensive one takes over.
//!
//! That second rule matters more than the first. A cheap policy that is
//! allowed to retry after being refused will propose slight variations of the
//! same wrong step until the budget is gone, look busy, and produce nothing.
//! Escalating on the first refusal costs one expensive call and is the
//! difference between a loop that converges and one that grinds.
//!
//! `RulePlanner` is a *reference* implementation, and the honest description
//! of it is: it recognises a handful of phrasings over a detected table. It
//! exists so the interface, the compiler, the validator and the loop can all
//! be exercised end to end without a model in the way, and so that a model
//! planner has something to be compared against. It is not a general
//! spreadsheet agent and nothing here pretends otherwise.

use serde::{Deserialize, Serialize};

use crate::compile::normalize;
use crate::plan::{ColumnRef, FormulaTemplate, Plan, RowRange, Step};
use crate::run::{PlanContext, PlanError, Planner};

/// A deterministic planner over a small vocabulary.
#[derive(Debug, Default)]
pub struct RulePlanner;

impl RulePlanner {
    pub fn new() -> Self {
        RulePlanner
    }
}

/// An arithmetic word and the operator it means.
const OPERATORS: &[(&str, char)] = &[
    ("multiplied by", '*'),
    ("times", '*'),
    ("divided by", '/'),
    ("plus", '+'),
    ("added to", '+'),
    ("minus", '-'),
    ("less", '-'),
];

impl Planner for RulePlanner {
    fn name(&self) -> &str {
        "rules"
    }

    fn propose(&mut self, ctx: &PlanContext<'_>) -> Result<Plan, PlanError> {
        // Being asked again means the last plan was refused, and this
        // planner has exactly one idea per instruction. Saying so is better
        // than proposing it again and burning the budget.
        if !ctx.feedback.is_empty() {
            return Err(PlanError::Refused(format!(
                "the rule planner has one plan per instruction and it was refused: {}",
                ctx.feedback.last().cloned().unwrap_or_default()
            )));
        }

        let said = ctx.instruction.to_lowercase();
        let table = ctx
            .observation
            .tables
            .first()
            .ok_or_else(|| PlanError::NoIdea("no table was detected".into()))?;
        let headers: Vec<String> = table
            .columns
            .iter()
            .map(|c| c.header.clone())
            .filter(|h| !h.is_empty())
            .collect();

        let locate = Step::LocateTable {
            sheet: Some(table.sheet.clone()),
            must_have: headers.clone(),
        };

        // "<output> is <a> times <b>" — the derived-column shape, and by
        // some distance the most common thing anybody asks for.
        let output = output_header(&said, &headers);
        // The output column can never be one of its own inputs. Without this
        // the sentence "the total is the quantity multiplied by the price"
        // resolves its left operand to `Total`, because "quantity" is not a
        // header and "total" is the nearest word that is — and the plan
        // proposes a column that reads itself. The validator refuses it, one
        // round trip later; knowing it here is free.
        let operands: Vec<String> = headers
            .iter()
            .filter(|h| output.as_deref().map(normalize) != Some(normalize(h)))
            .cloned()
            .collect();

        if let Some(word) = operator_word(&said) {
            let output = output.clone().unwrap_or_else(|| "Result".to_string());
            let at = match table
                .columns
                .iter()
                .find(|c| normalize(&c.header) == normalize(&output))
            {
                Some(c) => ColumnRef::Header {
                    text: c.header.clone(),
                },
                None => ColumnRef::NextFree,
            };

            let (op, left, right, confidence, why) = match named_operands(&said, word, &operands) {
                Some((op, l, r)) => (
                    op,
                    l.clone(),
                    r.clone(),
                    0.85,
                    format!("{output} is {l} {op} {r}, over the table body"),
                ),
                // The operands were not named in words this table recognises
                // — "quantity" where the header is "Qty". The column *types*
                // still say which columns could be meant, so the plan is
                // proposed with the confidence that deserves: a router sends
                // anything this unsure to a better planner, and on its own
                // the grader decides. Only for commutative operators, because
                // guessing the order of a subtraction is guessing the answer.
                None => {
                    let (op, l, r) = inferred_operands(word, table, &operands)?;
                    (
                        op,
                        l.clone(),
                        r.clone(),
                        0.55,
                        format!(
                            "the instruction says {word:?} but names no columns this table has; \
                             {l} and {r} are the only numeric columns besides {output}"
                        ),
                    )
                }
            };

            return Ok(Plan::new(vec![
                locate,
                Step::CreateDerivedColumn {
                    header: output,
                    at,
                    formula: FormulaTemplate::new(format!("={{{left}}}{op}{{{right}}}")),
                    rows: RowRange::TableBody,
                },
                Step::ExportWorkbook { path: None },
            ])
            .because(why)
            .with_confidence(confidence));
        }

        // "fill <column> down" — continue what somebody already started.
        if said.contains("fill") {
            if let Some(column) = mentioned(&said, &headers) {
                return Ok(Plan::new(vec![
                    locate,
                    Step::FillRange {
                        column: ColumnRef::Header {
                            text: column.clone(),
                        },
                        rows: RowRange::TableBody,
                    },
                    Step::ExportWorkbook { path: None },
                ])
                .because(format!("continue the formula already in {column}"))
                .with_confidence(0.8));
            }
        }

        Err(PlanError::NoIdea(format!(
            "nothing in {:?} matches a known shape over the columns {headers:?}",
            ctx.instruction
        )))
    }
}

/// The first arithmetic word the instruction uses.
fn operator_word(said: &str) -> Option<&'static str> {
    OPERATORS
        .iter()
        .filter_map(|(word, _)| said.find(word).map(|i| (i, *word)))
        .min_by_key(|(i, _)| *i)
        .map(|(_, w)| w)
}

fn operator_of(word: &str) -> char {
    OPERATORS
        .iter()
        .find(|(w, _)| *w == word)
        .map(|(_, op)| *op)
        .unwrap_or('*')
}

/// "<a> <operator-word> <b>" where both `a` and `b` are headers of this table.
///
/// Deliberately requires both operands to be real headers. Guessing that an
/// unrecognised word is a column would produce a formula referring to
/// something that is not there, and the compiler would refuse it anyway —
/// one round trip later, with a worse error message.
fn named_operands<'a>(
    said: &str,
    word: &str,
    headers: &'a [String],
) -> Option<(char, &'a String, &'a String)> {
    let at = said.find(word)?;
    let left = last_mentioned(&said[..at], headers)?;
    let right = mentioned(&said[at + word.len()..], headers)?;
    Some((
        operator_of(word),
        headers.iter().find(|h| **h == left)?,
        headers.iter().find(|h| **h == right)?,
    ))
}

/// The two numeric columns, when the instruction did not name them.
///
/// Only for commutative operators: `{a}-{b}` and `{b}-{a}` are different
/// answers and nothing here can tell which was meant, so a subtraction the
/// instruction did not spell out is not something to guess at.
fn inferred_operands<'a>(
    word: &str,
    table: &env::observe::TableView,
    headers: &'a [String],
) -> Result<(char, &'a String, &'a String), PlanError> {
    let op = operator_of(word);
    if !matches!(op, '*' | '+') {
        return Err(PlanError::NoIdea(format!(
            "{word:?} is not commutative and the instruction names no columns this table has"
        )));
    }
    let numeric: Vec<&String> = headers
        .iter()
        .filter(|h| {
            table.columns.iter().any(|c| {
                normalize(&c.header) == normalize(h)
                    && matches!(c.cell_type, env::observe::CellType::Number)
            })
        })
        .collect();
    if numeric.len() != 2 {
        return Err(PlanError::NoIdea(format!(
            "the instruction says {word:?} but names no columns this table has, and there are {} numeric columns to choose from rather than two",
            numeric.len()
        )));
    }
    Ok((op, numeric[0], numeric[1]))
}

/// The first header named in this text.
fn mentioned(text: &str, headers: &[String]) -> Option<String> {
    headers
        .iter()
        .filter_map(|h| {
            let needle = normalize(h);
            (!needle.is_empty())
                .then(|| text.find(&needle).map(|i| (i, h.clone())))
                .flatten()
        })
        .min_by_key(|(i, _)| *i)
        .map(|(_, h)| h)
}

/// The last header named in this text — the operand nearest the operator.
fn last_mentioned(text: &str, headers: &[String]) -> Option<String> {
    headers
        .iter()
        .filter_map(|h| {
            let needle = normalize(h);
            (!needle.is_empty())
                .then(|| text.rfind(&needle).map(|i| (i, h.clone())))
                .flatten()
        })
        .max_by_key(|(i, _)| *i)
        .map(|(_, h)| h)
}

/// The name of the column being created, from "add a <name> column".
///
/// Read from the instruction rather than invented, because the header is
/// something a task can check and a made-up one fails for a reason that has
/// nothing to do with the arithmetic.
fn output_header(said: &str, headers: &[String]) -> Option<String> {
    for marker in [
        "add a ",
        "add an ",
        "fill in the ",
        "fill the ",
        "create a ",
    ] {
        let Some(at) = said.find(marker) else {
            continue;
        };
        let rest = &said[at + marker.len()..];
        let end = rest.find(" column").unwrap_or(rest.len());
        let name = rest[..end].trim();
        if name.is_empty() || name.len() > 40 {
            continue;
        }
        // Prefer the table's own spelling when the column already exists, so
        // "fill in the total column" targets `Total`.
        return Some(
            headers
                .iter()
                .find(|h| normalize(h) == normalize(name))
                .cloned()
                .unwrap_or_else(|| title_case(name)),
        );
    }
    None
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How often each side of the router answered.
///
/// Counted because "which policy actually did the work" is not visible in a
/// pass rate, and a router that silently escalates every call is a router
/// that has stopped saving anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterStats {
    pub fast: u32,
    pub slow: u32,
    /// Times the fast planner answered but was not confident enough.
    pub below_threshold: u32,
    /// Times the loop had already refused something, so the fast planner was
    /// skipped outright.
    pub escalated_after_failure: u32,
}

impl RouterStats {
    pub fn fast_share(&self) -> f64 {
        let total = self.fast + self.slow;
        if total == 0 {
            return 0.0;
        }
        self.fast as f64 / total as f64
    }
}

/// Ask the cheap planner; fall back to the expensive one.
pub struct Router<'a> {
    fast: &'a mut dyn Planner,
    slow: &'a mut dyn Planner,
    threshold: f32,
    stats: RouterStats,
    name: String,
}

impl<'a> Router<'a> {
    /// `threshold` is the confidence the fast planner must claim before it is
    /// allowed to answer.
    pub fn new(fast: &'a mut dyn Planner, slow: &'a mut dyn Planner, threshold: f32) -> Self {
        let name = format!("{}->{}", fast.name(), slow.name());
        Router {
            fast,
            slow,
            threshold,
            stats: RouterStats::default(),
            name,
        }
    }

    pub fn stats(&self) -> &RouterStats {
        &self.stats
    }
}

impl Planner for Router<'_> {
    fn name(&self) -> &str {
        &self.name
    }

    fn propose(&mut self, ctx: &PlanContext<'_>) -> Result<Plan, PlanError> {
        // Anything already refused means the cheap policy's model of the
        // situation is wrong. Letting it try again produces variations on the
        // same mistake until the budget is gone.
        if !ctx.feedback.is_empty() {
            self.stats.slow += 1;
            self.stats.escalated_after_failure += 1;
            return self.slow.propose(ctx);
        }

        match self.fast.propose(ctx) {
            Ok(plan) if plan.confidence >= self.threshold => {
                self.stats.fast += 1;
                Ok(plan)
            }
            Ok(_) => {
                self.stats.below_threshold += 1;
                self.stats.slow += 1;
                self.slow.propose(ctx)
            }
            Err(_) => {
                self.stats.slow += 1;
                self.slow.propose(ctx)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Engine;
    use env::observe::{observe, WorkbookObservation};

    fn edit(sheet: &str, a1: &str, input: &str) -> engine::Action {
        engine::Action::CellEdit {
            sheet: sheet.into(),
            addr: engine::CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        }
    }

    fn ledger() -> Engine {
        let mut e = Engine::new();
        for (a1, input) in [
            ("A1", "Item"),
            ("B1", "Qty"),
            ("C1", "Price"),
            ("A2", "Bolt"),
            ("B2", "4"),
            ("C2", "2.5"),
            ("A3", "Nut"),
            ("B3", "9"),
            ("C3", "0.5"),
            ("A4", "Washer"),
            ("B4", "2"),
            ("C4", "1.25"),
        ] {
            e.apply(&edit("Sheet1", a1, input)).unwrap();
        }
        e
    }

    fn ask(instruction: &str, obs: &WorkbookObservation) -> Result<Plan, PlanError> {
        RulePlanner::new().propose(&PlanContext {
            instruction,
            observation: obs,
            subject: None,
            attempt: 1,
            feedback: &[],
        })
    }

    fn look() -> WorkbookObservation {
        observe(&ledger(), "Sheet1", "A1", &[], "h".into())
    }

    #[test]
    fn the_rule_planner_reads_a_multiplication_out_of_an_instruction() {
        let plan = ask(
            "Add a Total column: Qty times Price for every row.",
            &look(),
        )
        .unwrap();
        assert_eq!(
            plan.steps[1],
            Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Qty}*{Price}"),
                rows: RowRange::TableBody,
            }
        );
        assert!(plan.steps.last().unwrap().is_terminal());
    }

    #[test]
    fn the_operands_come_out_in_the_order_they_were_said() {
        // `{Price}-{Qty}` and `{Qty}-{Price}` are different answers, and a
        // planner that got the order from the column layout rather than from
        // the sentence would be right half the time.
        let plan = ask("Add a Margin column: Price minus Qty.", &look()).unwrap();
        let Step::CreateDerivedColumn { formula, .. } = &plan.steps[1] else {
            panic!("expected a derived column");
        };
        assert_eq!(formula.0, "={Price}-{Qty}");
    }

    #[test]
    fn an_existing_column_is_targeted_rather_than_a_new_one_added_beside_it() {
        // "Fill in the Total column" when Total is already there means that
        // column, not a second one called Total.
        let mut e = ledger();
        e.apply(&edit("Sheet1", "D1", "Total")).unwrap();
        let obs = observe(&e, "Sheet1", "A1", &[], "h".into());
        let plan = ask("Fill in the Total column: Qty times Price.", &obs).unwrap();
        let Step::CreateDerivedColumn { at, .. } = &plan.steps[1] else {
            panic!("expected a derived column");
        };
        assert_eq!(
            *at,
            ColumnRef::Header {
                text: "Total".into()
            }
        );
    }

    #[test]
    fn an_output_column_is_never_one_of_its_own_inputs() {
        // The bug: "the total is the quantity multiplied by the price"
        // resolves its left operand to `Total`, because "quantity" is not a
        // header and "total" is the nearest word that is. The plan then
        // proposes a column that reads itself. The validator refuses it a
        // round trip later; knowing it here is free.
        let plan = ask(
            "Fill in the Total column: for every row, the total is the quantity multiplied by the price.",
            &look(),
        )
        .unwrap();
        let Step::CreateDerivedColumn { formula, .. } = &plan.steps[1] else {
            panic!("expected a derived column");
        };
        assert!(
            !formula.0.contains("{Total}"),
            "the output column appeared in its own formula: {}",
            formula.0
        );
        assert_eq!(formula.0, "={Qty}*{Price}");
    }

    #[test]
    fn operands_inferred_from_column_types_are_marked_as_a_guess() {
        // "Quantity" and "Cost" are not headers here. The column types still
        // say which two columns could be meant, so a plan is offered — with
        // the confidence that deserves, so a router sends it somewhere
        // better rather than committing it.
        let plan = ask("Add a Total column: Quantity times Cost.", &look()).unwrap();
        assert!(
            plan.confidence < 0.7,
            "an inferred plan claimed confidence {}",
            plan.confidence
        );
        assert!(plan.rationale.unwrap().contains("names no columns"));
    }

    #[test]
    fn the_order_of_a_subtraction_is_never_guessed() {
        // `{a}-{b}` and `{b}-{a}` are different answers. Inferring operands
        // from column types cannot tell which was meant, so it does not try.
        assert!(matches!(
            ask("Add a Margin column: Revenue minus Cost.", &look()),
            Err(PlanError::NoIdea(_))
        ));
    }

    #[test]
    fn an_ambiguous_table_gets_no_inferred_plan() {
        // Three numeric columns and no named operands is not two columns
        // with an obvious answer.
        let mut e = ledger();
        e.apply(&edit("Sheet1", "D1", "Weight")).unwrap();
        for row in 2..=4 {
            e.apply(&edit("Sheet1", &format!("D{row}"), "1")).unwrap();
        }
        let obs = observe(&e, "Sheet1", "A1", &[], "h".into());
        assert!(matches!(
            ask("Add a Total column: Quantity times Cost.", &obs),
            Err(PlanError::NoIdea(_))
        ));
    }

    #[test]
    fn an_instruction_it_does_not_understand_gets_no_plan_rather_than_a_guess() {
        assert!(matches!(
            ask("Tidy this up a bit and make it look nicer.", &look()),
            Err(PlanError::NoIdea(_))
        ));
    }

    #[test]
    fn the_rule_planner_refuses_to_repeat_itself_after_a_refusal() {
        // It has one idea per instruction. Offering it again is how a cheap
        // policy burns a whole budget looking busy.
        let obs = look();
        let feedback = vec!["it was refused".to_string()];
        let result = RulePlanner::new().propose(&PlanContext {
            instruction: "Add a Total column: Qty times Price.",
            observation: &obs,
            subject: None,
            attempt: 2,
            feedback: &feedback,
        });
        assert!(matches!(result, Err(PlanError::Refused(_))));
    }

    /// A planner that always answers, to stand in for the expensive one.
    struct Always(Plan, u32);
    impl Planner for Always {
        fn name(&self) -> &str {
            "always"
        }
        fn propose(&mut self, _: &PlanContext<'_>) -> Result<Plan, PlanError> {
            self.1 += 1;
            Ok(self.0.clone())
        }
    }

    /// A planner that answers with a stated confidence.
    struct AtConfidence(f32, u32);
    impl Planner for AtConfidence {
        fn name(&self) -> &str {
            "cheap"
        }
        fn propose(&mut self, _: &PlanContext<'_>) -> Result<Plan, PlanError> {
            self.1 += 1;
            Ok(Plan::new(vec![Step::ExportWorkbook { path: None }]).with_confidence(self.0))
        }
    }

    fn context<'a>(obs: &'a WorkbookObservation, feedback: &'a [String]) -> PlanContext<'a> {
        PlanContext {
            instruction: "do the thing",
            observation: obs,
            subject: None,
            attempt: 1,
            feedback,
        }
    }

    #[test]
    fn a_confident_cheap_answer_is_taken_and_the_expensive_one_is_not_called() {
        let obs = look();
        let mut cheap = AtConfidence(0.9, 0);
        let mut dear = Always(Plan::new(vec![]), 0);
        let mut router = Router::new(&mut cheap, &mut dear, 0.7);
        router.propose(&context(&obs, &[])).unwrap();
        assert_eq!(router.stats().fast, 1);
        assert_eq!(router.stats().slow, 0);
    }

    #[test]
    fn an_unconfident_cheap_answer_is_escalated() {
        let obs = look();
        let mut cheap = AtConfidence(0.4, 0);
        let mut dear = Always(Plan::new(vec![]), 0);
        let mut router = Router::new(&mut cheap, &mut dear, 0.7);
        router.propose(&context(&obs, &[])).unwrap();
        assert_eq!(router.stats().fast, 0);
        assert_eq!(router.stats().slow, 1);
        assert_eq!(router.stats().below_threshold, 1);
    }

    #[test]
    fn a_cheap_planner_with_no_idea_is_escalated_rather_than_ending_the_run() {
        let obs = look();
        let mut cheap = RulePlanner::new();
        let mut dear = Always(Plan::new(vec![Step::ExportWorkbook { path: None }]), 0);
        let mut router = Router::new(&mut cheap, &mut dear, 0.7);
        let plan = router
            .propose(&PlanContext {
                instruction: "make it nicer",
                observation: &obs,
                subject: None,
                attempt: 1,
                feedback: &[],
            })
            .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(router.stats().slow, 1);
    }

    #[test]
    fn once_something_has_been_refused_the_cheap_planner_is_skipped_entirely() {
        // The rule that matters most. A cheap policy allowed to retry after
        // a refusal proposes variations of the same mistake until the budget
        // is gone.
        let obs = look();
        let mut cheap = AtConfidence(1.0, 0);
        let mut dear = Always(Plan::new(vec![]), 0);
        let mut router = Router::new(&mut cheap, &mut dear, 0.7);
        let feedback = vec!["that was refused".to_string()];
        router.propose(&context(&obs, &feedback)).unwrap();
        assert_eq!(router.stats().fast, 0);
        assert_eq!(router.stats().escalated_after_failure, 1);
    }

    #[test]
    fn the_router_reports_which_side_did_the_work() {
        let obs = look();
        let mut cheap = AtConfidence(0.9, 0);
        let mut dear = Always(Plan::new(vec![]), 0);
        let mut router = Router::new(&mut cheap, &mut dear, 0.7);
        for _ in 0..3 {
            router.propose(&context(&obs, &[])).unwrap();
        }
        let after_failure = vec!["nope".to_string()];
        router.propose(&context(&obs, &after_failure)).unwrap();
        assert_eq!(router.stats().fast_share(), 0.75);
    }
}
