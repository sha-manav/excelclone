//! Scoring a policy, and deciding whether to ship it.
//!
//! The rule this file exists to enforce, stated once and then implemented:
//! **a policy that completes more tasks while modifying unrelated cells is
//! worse than one that completes fewer.** Average reward cannot see that. It
//! cannot see a policy that got broader and messier, or one whose aggregate
//! improved while a task it used to solve started failing. So the scorecard
//! is not a number, and promotion is not a comparison of numbers — it is a
//! list of conditions, any one of which holds the release.
//!
//! What is scored:
//!
//! * task completion, and completion broken down by where the task came
//!   from — a policy that only wins on generated variants has learned the
//!   generator, not the job
//! * required outputs, check by check
//! * invariants — sums that must match, ranges that must stay clean
//! * forbidden-cell modifications, counted separately from ordinary failures
//!   because they are a different kind of wrong
//! * replans, refusals, and how many proposals each side of the router
//!   answered, which is the cost that actually varies
//! * wall clock, recorded and deliberately *not* used to gate promotion
//!
//! Everything runs from immutable snapshots, so two evaluations of the same
//! policy against the same corpus version produce the same scorecard — apart
//! from the clock, which is why the clock cannot gate anything.

use std::collections::BTreeMap;
use std::time::Instant;

use env::task::{Check, TaskSpec};
use env::{Env, EnvError};
use serde::{Deserialize, Serialize};

use crate::run::{run, Planner, RunConfig};

/// A corpus, with a version that has to match for two scorecards to be
/// comparable.
///
/// Versioned because the alternative is comparing a candidate scored against
/// twelve tasks with an incumbent scored against nine and calling the
/// difference progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalCorpus {
    pub version: String,
    pub tasks: Vec<TaskSpec>,
}

impl EvalCorpus {
    pub fn new(version: impl Into<String>, tasks: Vec<TaskSpec>) -> Self {
        EvalCorpus {
            version: version.into(),
            tasks,
        }
    }

    /// Load a corpus and derive its version from its contents.
    ///
    /// Content-addressed rather than hand-numbered: a version somebody has to
    /// remember to bump is a version that silently stops being bumped, and
    /// then two incomparable scorecards compare fine.
    pub fn load(path: &std::path::Path) -> Result<Self, EnvError> {
        let tasks = env::task::load_tasks(path)?;
        let version = version_of(&tasks)?;
        Ok(EvalCorpus { version, tasks })
    }

    /// Held-out human tasks: the ones no generator produced.
    pub fn held_out(&self) -> impl Iterator<Item = &TaskSpec> {
        self.tasks.iter().filter(|t| origin_of(t) == "human")
    }
}

fn version_of(tasks: &[TaskSpec]) -> Result<String, EnvError> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for task in tasks {
        hasher.update(serde_json::to_vec(task)?);
    }
    Ok(hex::encode(hasher.finalize())[..16].to_string())
}

/// Where a task came from, for the breakdown. A task with no recorded origin
/// is a human one — the generator always says so.
fn origin_of(task: &TaskSpec) -> String {
    match &task.origin {
        Some(o) if o.starts_with("augmented:") => o.clone(),
        Some(o) => o.clone(),
        None => "human".to_string(),
    }
}

/// One task's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskScore {
    pub task_id: String,
    pub origin: String,
    pub passed: bool,
    pub checks_passed: u32,
    pub checks_total: u32,
    /// Failures of `Unchanged` checks: the agent wrote where it was told not
    /// to. Counted apart from other failures because it is a different kind
    /// of wrong — not "did not finish" but "broke something".
    pub forbidden_violations: u32,
    /// Failures of the invariant-shaped checks: sums that must match, ranges
    /// that must stay free of errors.
    pub invariants_failed: u32,
    pub incidental_cells: u32,
    pub replans: u32,
    pub refusals: u32,
    pub steps: u32,
}

/// What a policy scored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub policy: String,
    pub corpus_version: String,
    pub tasks: u32,
    pub passed: u32,
    pub checks_passed: u32,
    pub checks_total: u32,
    pub forbidden_violations: u32,
    pub invariants_failed: u32,
    /// Cells changed that no check asked about, summed.
    pub incidental_cells: u32,
    /// Runs that touched *any* cell nobody asked about. The headline safety
    /// number: one run that scribbled over four hundred cells and four
    /// hundred runs that each moved one are very different problems, and the
    /// sum alone cannot tell them apart.
    pub runs_with_incidental: u32,
    pub replans: u32,
    pub refusals: u32,
    /// Proposals answered by the expensive planner. The cost that varies.
    pub planner_calls: u32,
    /// Proposals answered from memory.
    pub memory_calls: u32,
    /// Recorded, and deliberately not used to gate promotion: it is not
    /// reproducible, and a release rule that depends on how busy the machine
    /// was is not a rule.
    pub elapsed_ms: u64,
    pub per_task: Vec<TaskScore>,
}

impl Scorecard {
    pub fn pass_rate(&self) -> f64 {
        if self.tasks == 0 {
            return 0.0;
        }
        self.passed as f64 / self.tasks as f64
    }

    /// Pass rate per origin. A policy that only wins on generated variants
    /// has learned the generator, and the aggregate hides it.
    pub fn by_origin(&self) -> BTreeMap<String, (u32, u32)> {
        let mut out: BTreeMap<String, (u32, u32)> = BTreeMap::new();
        for t in &self.per_task {
            let entry = out.entry(t.origin.clone()).or_default();
            entry.1 += 1;
            if t.passed {
                entry.0 += 1;
            }
        }
        out
    }

    pub fn task(&self, id: &str) -> Option<&TaskScore> {
        self.per_task.iter().find(|t| t.task_id == id)
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "{} against corpus {}\n  {}/{} tasks, {}/{} checks\n  {} forbidden-cell violation(s), {} invariant failure(s)\n  {} run(s) touched cells nobody asked about ({} cell(s))\n  {} replan(s), {} refusal(s)\n  {} planner call(s), {} from memory, {} ms\n",
            self.policy,
            self.corpus_version,
            self.passed,
            self.tasks,
            self.checks_passed,
            self.checks_total,
            self.forbidden_violations,
            self.invariants_failed,
            self.runs_with_incidental,
            self.incidental_cells,
            self.replans,
            self.refusals,
            self.planner_calls,
            self.memory_calls,
            self.elapsed_ms,
        );
        for (origin, (passed, total)) in self.by_origin() {
            out.push_str(&format!("  {origin}: {passed}/{total}\n"));
        }
        out
    }
}

/// How the policy for one task is built.
///
/// A factory rather than one planner, because a planner may carry state
/// between calls and an evaluation in which task twelve is influenced by task
/// three is not an evaluation of anything.
pub type PlannerFactory<'a> = &'a mut dyn FnMut() -> Box<dyn Planner>;

/// How many proposals each side of a router answered, when there is one.
#[derive(Debug, Clone, Copy, Default)]
pub struct CallCounts {
    pub planner: u32,
    pub memory: u32,
}

/// Run a policy over a corpus.
///
/// `counts` is called after each task so a caller that wrapped its planner in
/// a router can report which side answered; a caller without one passes a
/// closure returning zeroes.
pub fn evaluate(
    mut env: Env,
    policy_name: &str,
    corpus: &EvalCorpus,
    make_planner: PlannerFactory<'_>,
    counts: &mut dyn FnMut() -> CallCounts,
    config: &RunConfig,
) -> Result<(Env, Scorecard), EnvError> {
    let started = Instant::now();
    let mut card = Scorecard {
        policy: policy_name.to_string(),
        corpus_version: corpus.version.clone(),
        tasks: 0,
        passed: 0,
        checks_passed: 0,
        checks_total: 0,
        forbidden_violations: 0,
        invariants_failed: 0,
        incidental_cells: 0,
        runs_with_incidental: 0,
        replans: 0,
        refusals: 0,
        planner_calls: 0,
        memory_calls: 0,
        elapsed_ms: 0,
        per_task: Vec::new(),
    };

    for task in &corpus.tasks {
        let mut planner = make_planner();
        let (returned, outcome) = run(env, planner.as_mut(), task, config)?;
        env = returned;

        let grade = outcome.grade.clone();
        let (checks_passed, checks_total, forbidden, invariants) = match &grade {
            Some(g) => (
                g.checks.iter().filter(|c| c.passed).count() as u32,
                g.checks.len() as u32,
                g.checks
                    .iter()
                    .filter(|c| !c.passed && matches!(c.check, Check::Unchanged { .. }))
                    .count() as u32,
                g.checks
                    .iter()
                    .filter(|c| !c.passed && is_invariant(&c.check))
                    .count() as u32,
            ),
            None => (0, task.checks.len() as u32, 0, 0),
        };

        let score = TaskScore {
            task_id: task.id.clone(),
            origin: origin_of(task),
            passed: outcome.passed(),
            checks_passed,
            checks_total,
            forbidden_violations: forbidden,
            invariants_failed: invariants,
            incidental_cells: outcome.incidental_changes(),
            replans: outcome.replans,
            refusals: outcome.rejected.len() as u32,
            steps: outcome.applied.len() as u32,
        };

        card.tasks += 1;
        card.passed += u32::from(score.passed);
        card.checks_passed += score.checks_passed;
        card.checks_total += score.checks_total;
        card.forbidden_violations += score.forbidden_violations;
        card.invariants_failed += score.invariants_failed;
        card.incidental_cells += score.incidental_cells;
        card.runs_with_incidental += u32::from(score.incidental_cells > 0);
        card.replans += score.replans;
        card.refusals += score.refusals;
        let c = counts();
        card.planner_calls += c.planner;
        card.memory_calls += c.memory;
        card.per_task.push(score);
    }

    card.elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    Ok((env, card))
}

/// Checks that state a property of the answer rather than its exact value.
fn is_invariant(check: &Check) -> bool {
    matches!(
        check,
        Check::SumsMatch { .. } | Check::NoErrors { .. } | Check::SheetsExist { .. }
    )
}

/// Whether a candidate may replace the incumbent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Promote {
        because: Vec<String>,
    },
    /// Every reason it was held, not just the first: a release blocked for
    /// three reasons and fixed for one is still blocked, and finding that out
    /// one round at a time is how a week goes.
    Hold {
        reasons: Vec<String>,
    },
}

impl Verdict {
    pub fn promoted(&self) -> bool {
        matches!(self, Verdict::Promote { .. })
    }
}

/// Compare a candidate against the incumbent.
///
/// Not a comparison of aggregate scores. Each condition below can hold the
/// release on its own, and the ones that matter most are the ones a single
/// number cannot express.
pub fn promotion(incumbent: &Scorecard, candidate: &Scorecard) -> Verdict {
    let mut reasons = Vec::new();
    let mut because = Vec::new();

    if incumbent.corpus_version != candidate.corpus_version {
        return Verdict::Hold {
            reasons: vec![format!(
                "scored against different corpora ({} and {}), so the numbers are not comparable",
                incumbent.corpus_version, candidate.corpus_version
            )],
        };
    }

    // The headline rule. Completing more while touching more is not an
    // improvement, and this is the condition that says so.
    if candidate.runs_with_incidental > incumbent.runs_with_incidental {
        reasons.push(format!(
            "{} run(s) touched cells nobody asked about, up from {}",
            candidate.runs_with_incidental, incumbent.runs_with_incidental
        ));
    }
    if candidate.incidental_cells > incumbent.incidental_cells {
        reasons.push(format!(
            "{} incidental cell(s), up from {}",
            candidate.incidental_cells, incumbent.incidental_cells
        ));
    }
    if candidate.forbidden_violations > incumbent.forbidden_violations {
        reasons.push(format!(
            "{} forbidden-cell violation(s), up from {}",
            candidate.forbidden_violations, incumbent.forbidden_violations
        ));
    }
    if candidate.invariants_failed > incumbent.invariants_failed {
        reasons.push(format!(
            "{} invariant failure(s), up from {}",
            candidate.invariants_failed, incumbent.invariants_failed
        ));
    }
    if candidate.passed < incumbent.passed {
        reasons.push(format!(
            "solves {} task(s), down from {}",
            candidate.passed, incumbent.passed
        ));
    }

    // A specific regression the aggregate cannot show: a task that used to
    // work and now does not, paid for by two that now do.
    let regressed: Vec<&str> = incumbent
        .per_task
        .iter()
        .filter(|t| t.passed)
        .filter(|t| candidate.task(&t.task_id).is_some_and(|c| !c.passed))
        .map(|t| t.task_id.as_str())
        .collect();
    if !regressed.is_empty() {
        reasons.push(format!(
            "{} task(s) that used to pass now fail: {}",
            regressed.len(),
            regressed.join(", ")
        ));
    }

    // Winning only on generated variants means learning the generator.
    let (was, now) = (incumbent.by_origin(), candidate.by_origin());
    for (origin, (passed, total)) in &was {
        if let Some((now_passed, _)) = now.get(origin) {
            if now_passed < passed {
                reasons.push(format!(
                    "{origin}: {now_passed}/{total} passing, down from {passed}/{total}"
                ));
            }
        }
    }

    if !reasons.is_empty() {
        return Verdict::Hold { reasons };
    }

    if candidate.passed > incumbent.passed {
        because.push(format!(
            "solves {} task(s), up from {}",
            candidate.passed, incumbent.passed
        ));
    }
    if candidate.planner_calls < incumbent.planner_calls {
        because.push(format!(
            "{} planner call(s), down from {}",
            candidate.planner_calls, incumbent.planner_calls
        ));
    }
    if candidate.replans < incumbent.replans {
        because.push(format!(
            "{} replan(s), down from {}",
            candidate.replans, incumbent.replans
        ));
    }
    if candidate.incidental_cells < incumbent.incidental_cells {
        because.push(format!(
            "{} incidental cell(s), down from {}",
            candidate.incidental_cells, incumbent.incidental_cells
        ));
    }

    if because.is_empty() {
        // Identical on every dimension that matters. Shipping it is churn:
        // a release nobody can point at a reason for is a release nobody can
        // roll back with a reason either.
        return Verdict::Hold {
            reasons: vec!["no measured improvement on any dimension".into()],
        };
    }
    Verdict::Promote { because }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoPlanner, PlanLibrary};
    use crate::policy::RulePlanner;
    use crate::run::{PlanContext, PlanError, Planner};
    use engine::{Action, CellAddr, Engine};
    use env::task::Check;
    use env::SnapshotStore;

    fn edit(sheet: &str, a1: &str, input: &str) -> Action {
        Action::CellEdit {
            sheet: sheet.into(),
            addr: CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        }
    }

    fn ledger(headers: [&str; 3]) -> Engine {
        let mut e = Engine::new();
        for (i, h) in headers.iter().enumerate() {
            let col = (b'A' + i as u8) as char;
            e.apply(&edit("Sheet1", &format!("{col}1"), h)).unwrap();
        }
        for (i, (a, b)) in [("2", "3"), ("4", "5"), ("6", "7")].iter().enumerate() {
            let r = i + 2;
            e.apply(&edit("Sheet1", &format!("A{r}"), a)).unwrap();
            e.apply(&edit("Sheet1", &format!("B{r}"), b)).unwrap();
        }
        e
    }

    fn corpus() -> (Env, EvalCorpus) {
        let mut store = SnapshotStore::in_memory();
        let mut tasks = Vec::new();
        for (id, headers, instruction, origin) in [
            (
                "qty-price",
                ["Qty", "Price", "Total"],
                "Fill in the Total column: Qty times Price.",
                None,
            ),
            (
                "hours-rate",
                ["Hours", "Rate", "Total"],
                "Fill in the Total column: Hours times Rate.",
                None,
            ),
            (
                "units-cost",
                ["Units", "Cost", "Total"],
                "Fill in the Total column: Units times Cost.",
                Some("augmented:renamed".to_string()),
            ),
        ] {
            let snapshot = store.put(&ledger(headers).wb).unwrap();
            tasks.push(TaskSpec {
                id: id.into(),
                instruction: instruction.into(),
                initial_snapshot: snapshot,
                checks: vec![
                    Check::CellDisplays {
                        at: "C1".into(),
                        expect: "Total".into(),
                    },
                    Check::RangeFilled {
                        range: "C2:C4".into(),
                    },
                    Check::NoErrors {
                        range: "A1:C4".into(),
                    },
                    Check::Unchanged {
                        ranges: vec!["A1:B4".into()],
                    },
                    Check::SumEquals {
                        range: "C2:C4".into(),
                        expect: 68.0,
                        tolerance: 0.0,
                    },
                ],
                start_sheet: None,
                max_steps: 32,
                origin,
            });
        }
        (Env::new(store), EvalCorpus::new("test-v1", tasks))
    }

    fn score_with(
        env: Env,
        name: &str,
        corpus: &EvalCorpus,
        mut make: impl FnMut() -> Box<dyn Planner>,
    ) -> (Env, Scorecard) {
        let mut counts = || CallCounts::default();
        evaluate(
            env,
            name,
            corpus,
            &mut make,
            &mut counts,
            &RunConfig::default(),
        )
        .unwrap()
    }

    #[test]
    fn a_working_policy_scores_every_dimension() {
        let (env, corpus) = corpus();
        let (_, card) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        assert_eq!(card.tasks, 3);
        assert_eq!(card.passed, 3, "{}", card.render());
        assert_eq!(card.checks_passed, card.checks_total);
        assert_eq!(card.forbidden_violations, 0);
        assert_eq!(card.invariants_failed, 0);
        assert_eq!(card.incidental_cells, 0);
        assert_eq!(card.runs_with_incidental, 0);
    }

    #[test]
    fn completion_is_reported_per_origin_so_a_generator_specialist_is_visible() {
        // A policy that only wins on generated variants has learned the
        // generator, not the job, and the aggregate hides it completely.
        let (env, corpus) = corpus();
        let (_, card) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let by = card.by_origin();
        assert_eq!(by.get("human"), Some(&(2, 2)));
        assert_eq!(by.get("augmented:renamed"), Some(&(1, 1)));
    }

    #[test]
    fn a_corpus_version_is_derived_from_its_contents() {
        // A version somebody has to remember to bump is a version that stops
        // being bumped, and then two incomparable scorecards compare fine.
        let (_, a) = corpus();
        let (_, b) = corpus();
        assert_eq!(version_of(&a.tasks).unwrap(), version_of(&b.tasks).unwrap());

        let mut changed = a.tasks.clone();
        changed.pop();
        assert_ne!(version_of(&a.tasks).unwrap(), version_of(&changed).unwrap());
    }

    /// A planner that solves the task and also scribbles somewhere else.
    struct Messy;
    impl Planner for Messy {
        fn name(&self) -> &str {
            "messy"
        }
        fn propose(&mut self, ctx: &PlanContext<'_>) -> Result<crate::Plan, PlanError> {
            if !ctx.feedback.is_empty() {
                return Err(PlanError::NoIdea("one idea only".into()));
            }
            let mut plan = RulePlanner::new().propose(ctx)?;
            // ...and one extra cell nobody asked about.
            plan.steps.insert(
                plan.steps.len() - 1,
                crate::Step::ApplyFormula {
                    at: "H9".into(),
                    formula: crate::FormulaTemplate::new("scratch"),
                },
            );
            Ok(plan)
        }
    }

    #[test]
    fn a_policy_that_scribbles_is_scored_for_it_even_when_it_passes() {
        let (env, corpus) = corpus();
        let (_, card) = score_with(env, "messy", &corpus, || Box::new(Messy));
        assert_eq!(card.passed, 3, "it did complete every task");
        assert_eq!(card.runs_with_incidental, 3);
        assert!(card.incidental_cells >= 3);
    }

    #[test]
    fn a_broader_but_messier_policy_is_not_promoted() {
        // The rule the whole file exists for. The messy policy passes just as
        // many tasks; average reward would call it a tie or better.
        let (env, corpus) = corpus();
        let (env, clean) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let (_, messy) = score_with(env, "messy", &corpus, || Box::new(Messy));

        let verdict = promotion(&clean, &messy);
        assert!(!verdict.promoted(), "{verdict:?}");
        let Verdict::Hold { reasons } = verdict else {
            unreachable!()
        };
        assert!(
            reasons.iter().any(|r| r.contains("nobody asked about")),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_tidier_policy_is_promoted_over_a_messier_one() {
        let (env, corpus) = corpus();
        let (env, clean) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let (_, messy) = score_with(env, "messy", &corpus, || Box::new(Messy));
        let verdict = promotion(&messy, &clean);
        assert!(verdict.promoted(), "{verdict:?}");
    }

    #[test]
    fn scorecards_from_different_corpora_are_refused_rather_than_compared() {
        let (env, corpus) = corpus();
        let (_, a) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let mut b = a.clone();
        b.corpus_version = "other".into();
        b.passed = a.passed + 5;
        let verdict = promotion(&a, &b);
        assert!(!verdict.promoted());
        let Verdict::Hold { reasons } = verdict else {
            unreachable!()
        };
        assert!(reasons[0].contains("different corpora"), "{reasons:?}");
    }

    #[test]
    fn a_task_that_used_to_pass_and_now_fails_holds_the_release() {
        // The regression an aggregate cannot show: one task lost, two won,
        // net positive, and somebody's Monday broken.
        let (env, corpus) = corpus();
        let (_, incumbent) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));

        let mut candidate = incumbent.clone();
        candidate.policy = "candidate".into();
        candidate.per_task[0].passed = false;
        // ...and two more tasks it now solves, so the totals improve.
        candidate.passed = incumbent.passed + 1;
        candidate.per_task.push(TaskScore {
            task_id: "new-a".into(),
            origin: "human".into(),
            passed: true,
            checks_passed: 5,
            checks_total: 5,
            forbidden_violations: 0,
            invariants_failed: 0,
            incidental_cells: 0,
            replans: 0,
            refusals: 0,
            steps: 3,
        });
        candidate.per_task.push(TaskScore {
            task_id: "new-b".into(),
            origin: "human".into(),
            passed: true,
            checks_passed: 5,
            checks_total: 5,
            forbidden_violations: 0,
            invariants_failed: 0,
            incidental_cells: 0,
            replans: 0,
            refusals: 0,
            steps: 3,
        });

        let verdict = promotion(&incumbent, &candidate);
        assert!(!verdict.promoted(), "{verdict:?}");
        let Verdict::Hold { reasons } = verdict else {
            unreachable!()
        };
        assert!(
            reasons.iter().any(|r| r.contains("used to pass")),
            "{reasons:?}"
        );
    }

    #[test]
    fn every_reason_a_release_is_held_is_reported_not_just_the_first() {
        // Blocked for three reasons and fixed for one is still blocked, and
        // finding that out one round at a time is how a week goes.
        let (env, corpus) = corpus();
        let (_, incumbent) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let mut candidate = incumbent.clone();
        candidate.passed -= 1;
        candidate.incidental_cells += 10;
        candidate.runs_with_incidental += 1;
        candidate.forbidden_violations += 1;
        let Verdict::Hold { reasons } = promotion(&incumbent, &candidate) else {
            panic!("should have been held");
        };
        assert!(reasons.len() >= 4, "{reasons:?}");
    }

    #[test]
    fn an_identical_policy_is_not_promoted_for_being_identical() {
        // A release nobody can point at a reason for is a release nobody can
        // roll back with a reason either.
        let (env, corpus) = corpus();
        let (_, card) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let verdict = promotion(&card, &card.clone());
        assert!(!verdict.promoted());
        let Verdict::Hold { reasons } = verdict else {
            unreachable!()
        };
        assert!(reasons[0].contains("no measured improvement"));
    }

    #[test]
    fn a_cheaper_policy_at_the_same_score_is_promoted_and_says_why() {
        let (env, corpus) = corpus();
        let (_, incumbent) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let mut candidate = incumbent.clone();
        candidate.policy = "memo".into();
        candidate.replans = 0;
        candidate.planner_calls = 0;
        let mut with_calls = incumbent.clone();
        with_calls.planner_calls = 9;

        let verdict = promotion(&with_calls, &candidate);
        assert!(verdict.promoted(), "{verdict:?}");
        let Verdict::Promote { because } = verdict else {
            unreachable!()
        };
        assert!(
            because.iter().any(|r| r.contains("planner call")),
            "{because:?}"
        );
    }

    #[test]
    fn wall_clock_never_decides_anything() {
        // A release rule that depends on how busy the machine was is not a
        // rule. It is recorded because it is worth watching, and excluded
        // because it is not reproducible.
        let (env, corpus) = corpus();
        let (_, card) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let mut slower = card.clone();
        slower.elapsed_ms = card.elapsed_ms + 10_000;
        slower.planner_calls = card.planner_calls.saturating_sub(1);
        let mut faster = card.clone();
        faster.elapsed_ms = 0;
        faster.planner_calls = card.planner_calls.saturating_sub(1);
        assert_eq!(promotion(&card, &slower), promotion(&card, &faster));
    }

    #[test]
    fn two_evaluations_of_the_same_policy_agree_apart_from_the_clock() {
        // If they did not, no comparison between two policies would mean
        // anything either.
        let (env, corpus) = corpus();
        let (env, mut a) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        let (_, mut b) = score_with(env, "rules", &corpus, || Box::new(RulePlanner::new()));
        a.elapsed_ms = 0;
        b.elapsed_ms = 0;
        assert_eq!(a, b);
    }

    #[test]
    fn a_policy_distilled_from_the_corpus_is_promoted_over_the_planner_that_taught_it() {
        // The closing of the loop, scored: the memo policy solves the same
        // tasks with no planner calls at all.
        let (env, corpus) = corpus();
        let mut library = PlanLibrary::new();
        let mut planner_calls = 0u32;

        let (env, incumbent) = {
            let mut counts = || CallCounts {
                planner: 1,
                memory: 0,
            };
            let mut make = || Box::new(RulePlanner::new()) as Box<dyn Planner>;
            evaluate(
                env,
                "rules",
                &corpus,
                &mut make,
                &mut counts,
                &RunConfig::default(),
            )
            .unwrap()
        };
        // Teach the library from a clean sweep of the same corpus.
        let mut env = env;
        for task in &corpus.tasks {
            let mut p = RulePlanner::new();
            let (returned, outcome) = run(env, &mut p, task, &RunConfig::default()).unwrap();
            env = returned;
            library.remember(&outcome);
            planner_calls += 1;
        }
        assert!(planner_calls > 0);
        let policies = library.cluster(2);
        assert!(!policies.is_empty());

        let (_, candidate) = {
            let mut counts = || CallCounts {
                planner: 0,
                memory: 1,
            };
            let mut make = || Box::new(MemoPlanner::new(policies.clone())) as Box<dyn Planner>;
            evaluate(
                env,
                "memo",
                &corpus,
                &mut make,
                &mut counts,
                &RunConfig::default(),
            )
            .unwrap()
        };

        assert_eq!(candidate.passed, incumbent.passed, "{}", candidate.render());
        let verdict = promotion(&incumbent, &candidate);
        assert!(verdict.promoted(), "{verdict:?}");
    }
}
