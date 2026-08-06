//! Remembering what worked, and generalising it.
//!
//! A plan that solved a task is worth keeping, but keeping it verbatim is
//! only worth anything if the same task comes back with the same column
//! names. What generalises is the *shape*: "the output column is two numeric
//! columns multiplied, filled over the table body" is one habit whether the
//! columns were Qty and Price or Hours and Rate.
//!
//! So a `PlanLibrary` keeps successful plans, and clustering collapses them
//! by shape into `MicroPolicy` values: a representative plan plus, for each
//! placeholder in it, what that slot has been bound to before and what type
//! it was. Instantiating one against a new observation binds the slots to
//! real headers and hands back a real plan.
//!
//! Two rules keep this from becoming a machine for confidently repeating
//! mistakes.
//!
//! **Only clean successes are remembered.** A plan that passed while changing
//! cells nobody asked about, or that needed three replans to get there, is
//! not a habit worth forming. It is recorded as a failure to learn from
//! elsewhere, not promoted into a policy.
//!
//! **An instantiation says how it bound its slots.** Binding a slot by a
//! header name seen before is nearly a fact; binding it by "it is the only
//! numeric column left" is a guess, and the confidence differs by enough
//! that a router sends the guess to a better planner. A micro-policy that
//! could not tell those apart would be a fast way to be wrong.

use std::collections::BTreeMap;

use env::observe::{CellType, TableView, WorkbookObservation};
use serde::{Deserialize, Serialize};

use crate::compile::normalize;
use crate::plan::{ColumnRef, FormulaTemplate, Plan, Step};
use crate::run::{Outcome, PlanContext, PlanError, Planner};

/// A plan that worked, kept with what it cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanRecord {
    pub task_id: String,
    pub instruction: String,
    pub plan: Plan,
    /// The plan's shape — what clustering groups by.
    pub shape: Vec<String>,
    /// Header names the plan's placeholders resolved to, in slot order.
    pub bindings: Vec<String>,
    /// The header of the column it produced, if it produced one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    pub replans: u32,
    pub incidental_changes: u32,
}

/// Everything remembered so far.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanLibrary {
    pub records: Vec<PlanRecord>,
}

impl PlanLibrary {
    pub fn new() -> Self {
        PlanLibrary::default()
    }

    /// Keep this run's plan, if it is worth keeping.
    ///
    /// Returns whether it was. A plan that passed while touching cells nobody
    /// asked about is not a habit to form: promoting it turns one careless
    /// success into a policy that is careless every time.
    pub fn remember(&mut self, outcome: &Outcome) -> bool {
        if !outcome.passed() || outcome.incidental_changes() > 0 {
            return false;
        }
        let plan = Plan::new(outcome.applied.iter().map(|a| a.step.clone()).collect());
        let bindings = slots_of(&plan);
        if bindings.is_empty() {
            // Nothing parameterisable — a plan with no formula in it cannot
            // be generalised, only replayed, and replaying is what macros do.
            return false;
        }
        self.records.push(PlanRecord {
            task_id: outcome
                .trajectory
                .task_id
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            instruction: outcome.instruction.clone(),
            shape: plan.shape(),
            output: output_of(&plan),
            bindings,
            plan,
            replans: outcome.replans,
            incidental_changes: outcome.incidental_changes(),
        });
        true
    }

    /// Collapse the records into micro-policies.
    ///
    /// `min_support` is how many times a shape has to have worked before it
    /// counts as a habit. Two is the smallest number that means anything —
    /// one is an anecdote — and a caller doing this over real usage should
    /// want more.
    pub fn cluster(&self, min_support: usize) -> Vec<MicroPolicy> {
        let mut by_shape: BTreeMap<Vec<String>, Vec<&PlanRecord>> = BTreeMap::new();
        for record in &self.records {
            by_shape
                .entry(record.shape.clone())
                .or_default()
                .push(record);
        }

        let mut out: Vec<MicroPolicy> = by_shape
            .into_iter()
            .filter(|(_, group)| group.len() >= min_support)
            .map(|(shape, group)| {
                let representative = group[0];
                let width = representative.bindings.len();
                let slots: Vec<Slot> = (0..width)
                    .map(|i| Slot {
                        seen: ranked(group.iter().filter_map(|r| r.bindings.get(i).cloned())),
                        cell_type: CellType::Number,
                    })
                    .collect();
                MicroPolicy {
                    id: shape.join(" -> "),
                    description: describe(&shape, &slots, representative.output.as_deref()),
                    shape,
                    support: group.len() as u32,
                    representative: representative.plan.clone(),
                    slots,
                    output: representative.output.clone(),
                    keywords: common_words(group.iter().map(|r| r.instruction.as_str())),
                }
            })
            .collect();
        // Most-supported first: what a router should reach for.
        out.sort_by(|a, b| b.support.cmp(&a.support).then(a.id.cmp(&b.id)));
        out
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut text = String::new();
        for record in &self.records {
            text.push_str(&serde_json::to_string(record).map_err(std::io::Error::other)?);
            text.push('\n');
        }
        std::fs::write(path, text)
    }

    pub fn load(path: &std::path::Path) -> Result<Self, std::io::Error> {
        let text = std::fs::read_to_string(path)?;
        let mut records = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            records.push(serde_json::from_str(line).map_err(std::io::Error::other)?);
        }
        Ok(PlanLibrary { records })
    }
}

/// One parameter of a micro-policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    /// Header names this slot has been bound to, most frequent first.
    pub seen: Vec<String>,
    /// What kind of column it has always been.
    pub cell_type: CellType,
}

/// A generalised habit: "find these columns, compute this, fill it down".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MicroPolicy {
    pub id: String,
    /// In words, for whoever has to decide whether to trust it.
    pub description: String,
    pub shape: Vec<String>,
    /// How many successful runs it was distilled from.
    pub support: u32,
    pub representative: Plan,
    pub slots: Vec<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Words the instructions had in common — a cheap check that this policy
    /// is being applied to the kind of request it came from.
    pub keywords: Vec<String>,
}

/// How a slot got bound, which is what its confidence rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binding {
    /// A header this slot has been bound to before. Nearly a fact.
    ByName,
    /// The only column of the right type left. A guess.
    ByType,
}

impl MicroPolicy {
    /// Turn this back into a plan against a particular workbook.
    ///
    /// `None` when the slots cannot be bound — which is the common case and
    /// the right answer: a habit that does not fit is not a habit to force.
    pub fn instantiate(&self, observation: &WorkbookObservation) -> Option<(Plan, f32)> {
        let table = observation.tables.first()?;
        let old = slots_of(&self.representative);
        if old.len() != self.slots.len() {
            return None;
        }

        let mut taken: Vec<String> = Vec::new();
        let mut mapping: BTreeMap<String, String> = BTreeMap::new();
        let mut how: Vec<Binding> = Vec::new();
        for (slot, was) in self.slots.iter().zip(&old) {
            let (header, binding) = bind(slot, table, &taken)?;
            taken.push(header.clone());
            mapping.insert(normalize(was), header);
            how.push(binding);
        }

        // The output column keeps its remembered name unless this table
        // already has a column by that name — renaming somebody's column
        // because a policy learned a different word for it is not an
        // improvement.
        let plan = rewrite(&self.representative, &mapping);

        // Every slot bound by name is a policy recognising a table it has
        // seen the like of. A slot bound by type is a guess, and one guess is
        // enough to make the whole thing something a router should second-
        // guess rather than commit.
        let confidence = if how.iter().all(|b| *b == Binding::ByName) {
            0.9
        } else {
            0.5
        };
        Some((plan.because(self.description.clone()), confidence))
    }
}

/// Find a column for this slot, preferring one it has seen before.
fn bind(slot: &Slot, table: &TableView, taken: &[String]) -> Option<(String, Binding)> {
    let free = |h: &str| !taken.iter().any(|t| normalize(t) == normalize(h));

    for name in &slot.seen {
        if let Some(c) = table
            .columns
            .iter()
            .find(|c| normalize(&c.header) == normalize(name) && free(&c.header))
        {
            return Some((c.header.clone(), Binding::ByName));
        }
    }

    let candidates: Vec<&str> = table
        .columns
        .iter()
        .filter(|c| c.cell_type == slot.cell_type && !c.header.is_empty() && free(&c.header))
        .map(|c| c.header.as_str())
        .collect();
    // Exactly one, or it is not an inference, it is a coin toss.
    match candidates.as_slice() {
        [only] => Some((only.to_string(), Binding::ByType)),
        _ => None,
    }
}

/// The distinct header names a plan's formulas mention, in order.
///
/// The same order `Plan::shape` assigns slots, so slot *i* here is `{i}`
/// there. Keeping the two in step is what makes a shape and its bindings
/// describe the same thing.
pub fn slots_of(plan: &Plan) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for step in &plan.steps {
        let template = match step {
            Step::CreateDerivedColumn { formula, .. } | Step::ApplyFormula { formula, .. } => {
                formula
            }
            _ => continue,
        };
        for name in template.placeholders() {
            if !out.iter().any(|h| normalize(h) == normalize(&name)) {
                out.push(name);
            }
        }
    }
    out
}

fn output_of(plan: &Plan) -> Option<String> {
    plan.steps.iter().find_map(|s| match s {
        Step::CreateDerivedColumn { header, .. } => Some(header.clone()),
        _ => None,
    })
}

/// Rewrite a plan's header references through a mapping.
fn rewrite(plan: &Plan, mapping: &BTreeMap<String, String>) -> Plan {
    let swap = |name: &str| -> String {
        mapping
            .get(&normalize(name))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    };
    let swap_column = |c: &ColumnRef| -> ColumnRef {
        match c {
            ColumnRef::Header { text } => ColumnRef::Header { text: swap(text) },
            other => other.clone(),
        }
    };
    let swap_formula = |f: &FormulaTemplate| -> FormulaTemplate {
        let mut out = String::new();
        let mut rest = f.0.as_str();
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                out.push_str(&rest[open..]);
                return FormulaTemplate::new(out);
            };
            out.push('{');
            out.push_str(&swap(after[..close].trim()));
            out.push('}');
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        FormulaTemplate::new(out)
    };

    let steps = plan
        .steps
        .iter()
        .map(|step| match step {
            Step::LocateTable { sheet, must_have } => Step::LocateTable {
                // The sheet is never carried over: a remembered plan's sheet
                // name is the least portable thing about it.
                sheet: sheet.as_ref().and(None),
                must_have: must_have.iter().map(|h| swap(h)).collect(),
            },
            Step::CreateDerivedColumn {
                header,
                at,
                formula,
                rows,
            } => Step::CreateDerivedColumn {
                header: header.clone(),
                at: swap_column(at),
                formula: swap_formula(formula),
                rows: rows.clone(),
            },
            Step::ApplyFormula { at, formula } => Step::ApplyFormula {
                at: at.clone(),
                formula: swap_formula(formula),
            },
            Step::FillRange { column, rows } => Step::FillRange {
                column: swap_column(column),
                rows: rows.clone(),
            },
            Step::FilterRows { column, predicate } => Step::FilterRows {
                column: swap_column(column),
                predicate: predicate.clone(),
            },
            Step::ReconcileTotals {
                left,
                right,
                variance_at,
                aggregate_with,
            } => Step::ReconcileTotals {
                left: swap_column(left),
                right: swap_column(right),
                variance_at: variance_at.clone(),
                aggregate_with: *aggregate_with,
            },
            other => other.clone(),
        })
        .collect();
    Plan::new(steps)
}

/// Names seen, most frequent first, ties broken alphabetically so the same
/// records always produce the same policy.
fn ranked(names: impl Iterator<Item = String>) -> Vec<String> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for n in names {
        *counts.entry(n).or_default() += 1;
    }
    let mut ordered: Vec<(String, u32)> = counts.into_iter().collect();
    ordered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ordered.into_iter().map(|(n, _)| n).collect()
}

/// Words that appeared in every instruction, minus the ones every English
/// sentence has.
fn common_words<'a>(instructions: impl Iterator<Item = &'a str>) -> Vec<String> {
    const NOISE: &[&str] = &[
        "the", "a", "an", "of", "for", "every", "each", "and", "to", "in", "is", "it", "this",
        "that", "with", "do", "not", "any", "other", "row", "rows", "column", "table",
    ];
    let mut sets: Vec<std::collections::BTreeSet<String>> = Vec::new();
    for text in instructions {
        sets.push(
            text.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2 && !NOISE.contains(w))
                .map(|w| w.to_string())
                .collect(),
        );
    }
    let Some((first, rest)) = sets.split_first() else {
        return Vec::new();
    };
    let mut shared: Vec<String> = first
        .iter()
        .filter(|w| rest.iter().all(|s| s.contains(*w)))
        .cloned()
        .collect();
    shared.sort();
    shared
}

/// A micro-policy in words: what a person reads before deciding to trust it.
fn describe(shape: &[String], slots: &[Slot], output: Option<&str>) -> String {
    let names: Vec<String> = slots
        .iter()
        .map(|s| match s.seen.first() {
            Some(n) => format!("a {n} column"),
            None => "a numeric column".to_string(),
        })
        .collect();
    let what = shape
        .iter()
        .find_map(|s| s.split_once(':').map(|(_, f)| f.to_string()))
        .unwrap_or_default();
    match output {
        Some(o) => format!(
            "find {}, then compute {o} as {what} and fill it to the last populated row",
            names.join(" and ")
        ),
        None => format!("find {}, then compute {what}", names.join(" and ")),
    }
}

/// A planner that answers from the library and stays quiet otherwise.
///
/// This is the cheap end of the router: no model call, no search, just "have
/// I seen a table like this and solved it before". It refuses far more often
/// than it answers, and that is the intended behaviour — a fast policy that
/// answers everything is a fast policy that is often wrong.
pub struct MemoPlanner {
    policies: Vec<MicroPolicy>,
}

impl MemoPlanner {
    pub fn new(policies: Vec<MicroPolicy>) -> Self {
        MemoPlanner { policies }
    }

    pub fn from_library(library: &PlanLibrary, min_support: usize) -> Self {
        MemoPlanner::new(library.cluster(min_support))
    }

    pub fn policies(&self) -> &[MicroPolicy] {
        &self.policies
    }
}

impl Planner for MemoPlanner {
    fn name(&self) -> &str {
        "memo"
    }

    fn propose(&mut self, ctx: &PlanContext<'_>) -> Result<Plan, PlanError> {
        if !ctx.feedback.is_empty() {
            return Err(PlanError::Refused(
                "a remembered plan was refused; this is not the situation it came from".into(),
            ));
        }
        let said = ctx.instruction.to_lowercase();
        for policy in &self.policies {
            // A shape that fits the sheet but not the request is the most
            // dangerous thing a memory can offer: it looks confident and
            // answers a question nobody asked.
            if !policy.keywords.is_empty()
                && !policy.keywords.iter().any(|k| said.contains(k.as_str()))
            {
                continue;
            }
            if let Some((plan, confidence)) = policy.instantiate(ctx.observation) {
                return Ok(plan.with_confidence(confidence));
            }
        }
        Err(PlanError::NoIdea(
            "nothing remembered fits this workbook".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::RowRange;
    use crate::run::{run, RunConfig};
    use crate::RulePlanner;
    use engine::Engine;
    use env::task::{Check, TaskSpec};
    use env::{Env, SnapshotStore};

    fn edit(sheet: &str, a1: &str, input: &str) -> engine::Action {
        engine::Action::CellEdit {
            sheet: sheet.into(),
            addr: engine::CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        }
    }

    /// A two-input table with an output column to fill.
    fn table(headers: [&str; 3], rows: &[(&str, &str)]) -> Engine {
        let mut e = Engine::new();
        for (i, h) in headers.iter().enumerate() {
            let col = (b'A' + i as u8) as char;
            e.apply(&edit("Sheet1", &format!("{col}1"), h)).unwrap();
        }
        for (i, (a, b)) in rows.iter().enumerate() {
            let r = i + 2;
            e.apply(&edit("Sheet1", &format!("A{r}"), a)).unwrap();
            e.apply(&edit("Sheet1", &format!("B{r}"), b)).unwrap();
        }
        e
    }

    fn task_for(engine: &Engine, id: &str, instruction: &str, sum: f64) -> (Env, TaskSpec) {
        let mut store = SnapshotStore::in_memory();
        let snapshot = store.put(&engine.wb).unwrap();
        (
            Env::new(store),
            TaskSpec {
                id: id.into(),
                instruction: instruction.into(),
                initial_snapshot: snapshot,
                checks: vec![
                    Check::RangeFilled {
                        range: "C2:C4".into(),
                    },
                    Check::CellDisplays {
                        at: "C1".into(),
                        expect: "Total".into(),
                    },
                    Check::Unchanged {
                        ranges: vec!["A1:B4".into()],
                    },
                    Check::SumEquals {
                        range: "C2:C4".into(),
                        expect: sum,
                        tolerance: 0.0,
                    },
                ],
                start_sheet: None,
                max_steps: 64,
                origin: None,
            },
        )
    }

    fn solved(headers: [&str; 3], instruction: &str, sum: f64) -> Outcome {
        let e = table(headers, &[("2", "3"), ("4", "5"), ("6", "7")]);
        let (env, task) = task_for(&e, "t", instruction, sum);
        let mut planner = RulePlanner::new();
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert!(outcome.passed(), "{:?}", outcome.rejected);
        outcome
    }

    fn qty_price() -> Outcome {
        solved(
            ["Qty", "Price", "Total"],
            "Fill in the Total column: Qty times Price.",
            6.0 + 20.0 + 42.0,
        )
    }

    fn hours_rate() -> Outcome {
        solved(
            ["Hours", "Rate", "Total"],
            "Fill in the Total column: Hours times Rate.",
            6.0 + 20.0 + 42.0,
        )
    }

    #[test]
    fn two_solutions_that_differ_only_in_column_names_become_one_policy() {
        // The generalisation the whole file exists for: "multiply two columns
        // and fill down" is one habit, not one per pair of column names.
        let mut library = PlanLibrary::new();
        assert!(library.remember(&qty_price()));
        assert!(library.remember(&hours_rate()));

        let policies = library.cluster(2);
        assert_eq!(policies.len(), 1, "{policies:?}");
        assert_eq!(policies[0].support, 2);
        assert_eq!(policies[0].slots.len(), 2);
        assert_eq!(policies[0].slots[0].seen, ["Hours", "Qty"]);
    }

    #[test]
    fn one_success_is_an_anecdote_not_a_policy() {
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        assert!(library.cluster(2).is_empty());
    }

    #[test]
    fn a_success_that_scribbled_outside_the_lines_is_not_remembered() {
        // Promoting it turns one careless success into a policy that is
        // careless every time.
        let mut outcome = qty_price();
        outcome.grade.as_mut().unwrap().incidental_changes = 3;
        let mut library = PlanLibrary::new();
        assert!(!library.remember(&outcome));
        assert!(library.records.is_empty());
    }

    #[test]
    fn a_failed_run_is_not_remembered() {
        let mut outcome = qty_price();
        outcome.grade.as_mut().unwrap().passed = false;
        let mut library = PlanLibrary::new();
        assert!(!library.remember(&outcome));
    }

    #[test]
    fn a_policy_binds_to_a_table_it_has_seen_the_like_of() {
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());
        let policy = &library.cluster(2)[0];

        let e = table(["Qty", "Price", "Total"], &[("1", "2")]);
        let obs = env::observe::observe(&e, "Sheet1", "A1", &[], "h".into());
        let (plan, confidence) = policy.instantiate(&obs).unwrap();
        assert!(confidence > 0.8, "a known table should not read as a guess");
        let Step::CreateDerivedColumn { formula, .. } = &plan.steps[1] else {
            panic!("expected a derived column");
        };
        assert_eq!(formula.0, "={Qty}*{Price}");
    }

    #[test]
    fn a_policy_applied_to_unfamiliar_columns_says_it_is_guessing() {
        // It binds by type rather than by name, which is an inference. A
        // router should second-guess it; committing it silently is how a
        // fast policy becomes a fast way to be wrong.
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());
        let policy = &library.cluster(2)[0];

        // One numeric column with a name it knows, one it does not.
        let e = table(["Qty", "Loading", "Total"], &[("1", "2")]);
        let obs = env::observe::observe(&e, "Sheet1", "A1", &[], "h".into());
        let (plan, confidence) = policy.instantiate(&obs).unwrap();
        assert!(
            confidence < 0.7,
            "a guessed binding claimed confidence {confidence}"
        );
        let Step::CreateDerivedColumn { formula, .. } = &plan.steps[1] else {
            panic!("expected a derived column");
        };
        assert_eq!(formula.0, "={Qty}*{Loading}");
    }

    #[test]
    fn a_policy_that_cannot_bind_its_slots_declines() {
        // A habit that does not fit is not a habit to force.
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());
        let policy = &library.cluster(2)[0];

        let mut e = Engine::new();
        for (a1, v) in [("A1", "Name"), ("B1", "Notes"), ("A2", "x"), ("B2", "y")] {
            e.apply(&edit("Sheet1", a1, v)).unwrap();
        }
        let obs = env::observe::observe(&e, "Sheet1", "A1", &[], "h".into());
        assert!(policy.instantiate(&obs).is_none());
    }

    #[test]
    fn a_remembered_policy_solves_a_task_the_rule_planner_never_saw() {
        // The point of the whole exercise: the habit transfers to a table
        // with different column names, without anyone writing a rule for it.
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());

        let e = table(
            ["Qty", "Price", "Total"],
            &[("2", "3"), ("4", "5"), ("6", "7")],
        );
        let (env, task) = task_for(&e, "fresh", "Work out the Total for each row.", 68.0);
        let mut planner = MemoPlanner::from_library(&library, 2);
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert!(outcome.passed(), "{:?}", outcome.rejected);
        assert_eq!(outcome.incidental_changes(), 0);
    }

    #[test]
    fn a_policy_is_not_offered_for_a_request_it_has_nothing_to_do_with() {
        // A shape that fits the sheet but not the question is the most
        // dangerous thing a memory can offer: it looks confident and answers
        // something nobody asked.
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());
        let mut planner = MemoPlanner::from_library(&library, 2);

        let e = table(["Qty", "Price", "Total"], &[("1", "2")]);
        let obs = env::observe::observe(&e, "Sheet1", "A1", &[], "h".into());
        let result = planner.propose(&PlanContext {
            instruction: "Sort the sheet by name.",
            observation: &obs,
            subject: None,
            attempt: 1,
            feedback: &[],
        });
        assert!(matches!(result, Err(PlanError::NoIdea(_))));
    }

    #[test]
    fn a_remembered_plan_is_not_offered_twice_after_it_was_refused() {
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());
        let mut planner = MemoPlanner::from_library(&library, 2);

        let e = table(["Qty", "Price", "Total"], &[("1", "2")]);
        let obs = env::observe::observe(&e, "Sheet1", "A1", &[], "h".into());
        let feedback = vec!["that was refused".to_string()];
        assert!(matches!(
            planner.propose(&PlanContext {
                instruction: "Fill in the Total column.",
                observation: &obs,
                subject: None,
                attempt: 2,
                feedback: &feedback,
            }),
            Err(PlanError::Refused(_))
        ));
    }

    #[test]
    fn a_policy_describes_itself_in_words_somebody_can_review() {
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());
        let policy = &library.cluster(2)[0];
        assert!(
            policy.description.contains("Total"),
            "{}",
            policy.description
        );
        assert!(
            policy.description.contains("last populated row"),
            "{}",
            policy.description
        );
    }

    #[test]
    fn a_library_survives_a_round_trip_through_disk() {
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());

        let dir = std::env::temp_dir().join(format!("gridline-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("plans.jsonl");
        library.save(&path).unwrap();
        let back = PlanLibrary::load(&path).unwrap();
        assert_eq!(library, back);
        assert_eq!(back.cluster(2).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_records_always_cluster_the_same_way() {
        // A policy set that shifted between runs would make every evaluation
        // of it incomparable with the last.
        let mut library = PlanLibrary::new();
        library.remember(&hours_rate());
        library.remember(&qty_price());
        let a = serde_json::to_string(&library.cluster(2)).unwrap();
        let b = serde_json::to_string(&library.cluster(2)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_remembered_plan_never_carries_its_original_sheet_name() {
        // The least portable thing about a plan. A habit that only fires on
        // a sheet called Sheet1 is a macro with extra steps.
        let mut library = PlanLibrary::new();
        library.remember(&qty_price());
        library.remember(&hours_rate());
        let policy = &library.cluster(2)[0];
        let e = table(["Qty", "Price", "Total"], &[("1", "2")]);
        let obs = env::observe::observe(&e, "Sheet1", "A1", &[], "h".into());
        let (plan, _) = policy.instantiate(&obs).unwrap();
        assert_eq!(
            plan.steps[0],
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into(), "Price".into(), "Total".into()],
            }
        );
    }

    #[test]
    fn slots_are_the_distinct_placeholders_in_order() {
        let plan = Plan::new(vec![Step::CreateDerivedColumn {
            header: "X".into(),
            at: ColumnRef::NextFree,
            formula: FormulaTemplate::new("={B}+{A}-{B}"),
            rows: RowRange::TableBody,
        }]);
        assert_eq!(slots_of(&plan), ["B", "A"]);
    }
}
