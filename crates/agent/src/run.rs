//! Propose, check, rehearse, commit.
//!
//! The loop is deliberately paranoid, and each layer catches something the
//! others cannot:
//!
//! 1. **Compile.** A step that names a column that is not there fails here,
//!    before anything has been touched.
//! 2. **Validate.** A step whose actions would write outside the scope it
//!    declared, make an unrequested structural change, or move more cells
//!    than the task allows, fails here. Still nothing touched.
//! 3. **Rehearse on a clone.** The actions are applied to a copy of the
//!    workbook and the result is inspected. This is what catches effects the
//!    static checks cannot see — a spilled array overflowing its column, a
//!    recalculation turning a distant cell into an error, a step that
//!    changes nothing at all.
//! 4. **Commit.** Only now do the actions touch the real environment, and
//!    they go through `Env::step` like everything else, so the trajectory
//!    records them and the whole episode replays.
//!
//! A failure at any layer becomes *feedback* and the planner is asked again.
//! That is the difference between replanning and retrying: the planner is
//! told what went wrong, in the same words a person would be told, and gets
//! to propose something different.
//!
//! Every committed step is followed by a checkpoint — a content-addressed
//! snapshot id — so a long task can be resumed at a step boundary rather than
//! from the beginning.

use engine::Engine;
use env::task::TaskSpec;
use env::trajectory::{ObservationPolicy, Recorder, Source, Termination, Trajectory};
use env::{Env, EnvError, GradeResult, SnapshotId};
use serde::{Deserialize, Serialize};

use crate::compile::{compile, CompileError, Subject};
use crate::plan::{Plan, Step};
use crate::validate::{validate, Estimate, Limits, Refusal};

/// What a planner is given.
pub struct PlanContext<'a> {
    pub instruction: &'a str,
    pub observation: &'a env::observe::WorkbookObservation,
    /// The table the plan is currently working on, once one has been found.
    pub subject: Option<&'a Subject>,
    /// Which attempt this is, from 1.
    pub attempt: u32,
    /// What went wrong on the previous attempts, newest last, in the words a
    /// person would be given. A planner that cannot see why it was refused
    /// can only guess again.
    pub feedback: &'a [String],
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PlanError {
    #[error("no plan for this instruction: {0}")]
    NoIdea(String),
    #[error("{0}")]
    Refused(String),
}

/// Something that turns an instruction and an observation into typed steps.
///
/// Note what the signature does not allow: no access to the `Engine`, no
/// filesystem, no way to return anything but `Plan`. A planner cannot reach
/// past the observation it was given, which is what makes "the policy was
/// misled by a bad summary" a scoring problem rather than a safety one.
pub trait Planner {
    fn name(&self) -> &str;
    fn propose(&mut self, ctx: &PlanContext<'_>) -> Result<Plan, PlanError>;
}

/// How hard to try.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunConfig {
    /// How many times the planner may be asked again after a failure.
    pub max_replans: u32,
    /// The lowest table-detection confidence the agent will act on.
    ///
    /// Table detection is a heuristic that says how sure it is; acting on a
    /// guess without noticing it was a guess is how an agent writes a column
    /// of formulas into somebody's data.
    pub min_table_confidence: f32,
    pub limits: Limits,
    pub observations: ObservationPolicy,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            max_replans: 3,
            min_table_confidence: 0.5,
            limits: Limits::default(),
            observations: ObservationPolicy::Every,
        }
    }
}

/// One step that made it all the way through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedStep {
    pub step: Step,
    /// How many actions it compiled to, and what they would cost.
    pub estimate: Estimate,
    /// The workbook after this step, by hash. Resume points.
    pub checkpoint: SnapshotId,
}

/// A step that did not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectedStep {
    pub step: Step,
    pub attempt: u32,
    /// In the words the planner is given back.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<Refusal>,
}

/// Why the run ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ending", rename_all = "snake_case")]
pub enum Ending {
    /// The plan said it was finished.
    Declared,
    /// The planner ran out of ideas.
    PlannerGaveUp { why: String },
    /// Too many refused or failed steps.
    OutOfReplans,
    /// The environment's step budget ran out.
    OutOfSteps,
}

/// Everything the run produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub planner: String,
    pub instruction: String,
    pub ending: Ending,
    pub applied: Vec<AppliedStep>,
    pub rejected: Vec<RejectedStep>,
    /// How many times the planner had to be asked again. A policy that
    /// finishes in one plan is better than one that finishes in five, and
    /// task completion alone cannot see the difference.
    pub replans: u32,
    pub grade: Option<GradeResult>,
    pub trajectory: Trajectory,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.grade.as_ref().is_some_and(|g| g.passed)
    }

    /// Cells the agent changed that no check asked about. The number that
    /// separates "did the task" from "did the task and nothing else".
    pub fn incidental_changes(&self) -> u32 {
        self.grade.as_ref().map_or(0, |g| g.incidental_changes)
    }
}

/// Run one task to completion.
pub fn run(
    env: Env,
    planner: &mut dyn Planner,
    task: &TaskSpec,
    config: &RunConfig,
) -> Result<(Env, Outcome), EnvError> {
    run_from(env, planner, task, config, None)
}

/// Run a task, optionally starting from a checkpoint of an earlier run.
///
/// `from` is an `AppliedStep::checkpoint` — the state after some step that
/// was validated, rehearsed and committed. Resuming there rather than from
/// the beginning is what makes a long task survive a crashed worker, and it
/// is deterministic for the same reason everything else here is: a checkpoint
/// is a content-addressed snapshot, and loading one recalculates.
///
/// Two things the resumed run does *not* inherit, on purpose:
///
/// * **The plan.** The planner is asked afresh against the state it finds. A
///   plan is a description of intent formed from an observation, and the
///   observation has moved on; replaying the rest of a stale plan is exactly
///   the fixed-coordinate macro replay this design exists to avoid.
/// * **The trajectory.** The resumed run records its own, starting from the
///   checkpoint. Stitching two recordings into one would produce a
///   trajectory whose state hashes are real but whose action list never
///   happened in one sitting, and it would not replay.
pub fn run_from(
    env: Env,
    planner: &mut dyn Planner,
    task: &TaskSpec,
    config: &RunConfig,
    from: Option<&SnapshotId>,
) -> Result<(Env, Outcome), EnvError> {
    let start = from
        .cloned()
        .unwrap_or_else(|| task.initial_snapshot.clone());
    let mut recorder = Recorder::start_task_at(
        env,
        format!("{}::{}", task.id, planner.name()),
        task,
        &start,
        Source::Policy {
            name: planner.name().to_string(),
        },
    )?
    .with_observations(config.observations);

    let mut subject: Option<Subject> = None;
    let mut applied: Vec<AppliedStep> = Vec::new();
    let mut rejected: Vec<RejectedStep> = Vec::new();
    let mut feedback: Vec<String> = Vec::new();
    let mut replans = 0u32;
    let mut attempt = 1u32;

    let ending = 'episode: loop {
        if replans > config.max_replans {
            break Ending::OutOfReplans;
        }
        let observation = recorder.observe()?;
        let plan = {
            let ctx = PlanContext {
                instruction: &task.instruction,
                observation: &observation,
                subject: subject.as_ref(),
                attempt,
                feedback: &feedback,
            };
            match planner.propose(&ctx) {
                Ok(plan) => plan,
                Err(e) => break Ending::PlannerGaveUp { why: e.to_string() },
            }
        };

        for step in &plan.steps {
            match attempt_step(&mut recorder, &observation, subject.as_ref(), step, config)? {
                StepOutcome::Committed {
                    estimate,
                    new_subject,
                    checkpoint,
                } => {
                    if let Some(s) = new_subject {
                        subject = Some(s);
                    }
                    applied.push(AppliedStep {
                        step: step.clone(),
                        estimate,
                        checkpoint,
                    });
                    if step.is_terminal() {
                        break 'episode Ending::Declared;
                    }
                }
                StepOutcome::Refused { reason, refusals } => {
                    rejected.push(RejectedStep {
                        step: step.clone(),
                        attempt,
                        reason: reason.clone(),
                        refusals,
                    });
                    feedback.push(format!("`{}` was refused: {reason}", step.kind()));
                    replans += 1;
                    attempt += 1;
                    // Abandon the rest of this plan. Steps after a failed one
                    // were written assuming it succeeded, and running them
                    // anyway is how an agent digs a hole.
                    continue 'episode;
                }
                StepOutcome::BudgetSpent => break 'episode Ending::OutOfSteps,
            }
        }

        // The plan ran out without saying it was done. That is a plan that
        // did not finish the job, and the planner is told so.
        feedback.push("the plan ended without an export_workbook step".into());
        replans += 1;
        attempt += 1;
    };

    let termination = match &ending {
        Ending::Declared => Termination::Done,
        Ending::OutOfSteps => Termination::BudgetExhausted,
        Ending::PlannerGaveUp { .. } | Ending::OutOfReplans => Termination::Abandoned,
    };
    let (trajectory, env) = recorder.finish_with_env(termination, Some(task))?;

    Ok((
        env,
        Outcome {
            planner: planner.name().to_string(),
            instruction: task.instruction.clone(),
            ending,
            applied,
            rejected,
            replans,
            grade: trajectory.grade.clone(),
            trajectory,
        },
    ))
}

enum StepOutcome {
    Committed {
        estimate: Estimate,
        new_subject: Option<Subject>,
        checkpoint: SnapshotId,
    },
    Refused {
        reason: String,
        refusals: Vec<Refusal>,
    },
    BudgetSpent,
}

/// Compile, validate, rehearse, commit — one step.
fn attempt_step(
    recorder: &mut Recorder,
    observation: &env::observe::WorkbookObservation,
    subject: Option<&Subject>,
    step: &Step,
    config: &RunConfig,
) -> Result<StepOutcome, EnvError> {
    let engine = recorder.env().engine()?;

    let compiled = match compile(step, engine, observation, subject) {
        Ok(c) => c,
        Err(e) => {
            return Ok(StepOutcome::Refused {
                reason: describe_compile_error(&e),
                refusals: Vec::new(),
            })
        }
    };

    // A table found by guessing is not something to write formulas into.
    if let Some(new) = &compiled.subject {
        if new.confidence < config.min_table_confidence {
            return Ok(StepOutcome::Refused {
                reason: format!(
                    "the table on {} was detected with confidence {:.2}, below the {:.2} this task requires; say which sheet and headers to use",
                    new.sheet, new.confidence, config.min_table_confidence
                ),
                refusals: Vec::new(),
            });
        }
    }

    let estimate = match validate(&compiled, engine, &config.limits) {
        Ok(e) => e,
        Err(refusals) => {
            let reason = refusals
                .iter()
                .map(|r| r.explain())
                .collect::<Vec<_>>()
                .join("; ");
            return Ok(StepOutcome::Refused { reason, refusals });
        }
    };

    // Rehearse. Everything above is static; this is where a step gets to
    // prove it does what it claims before the real workbook sees it.
    if !compiled.actions.is_empty() {
        let before = engine.clone();
        let mut clone = engine.clone();
        for action in &compiled.actions {
            if let Err(e) = clone.apply(action) {
                return Ok(StepOutcome::Refused {
                    reason: format!("the engine refused it: {e}"),
                    refusals: Vec::new(),
                });
            }
        }
        if let Some(problem) = rehearsal_problem(&before, &clone, &compiled) {
            return Ok(StepOutcome::Refused {
                reason: problem,
                refusals: Vec::new(),
            });
        }
    }

    // Commit, through the same door everything else uses.
    for action in &compiled.actions {
        let result = recorder.step(action)?;
        if !result.applied {
            // Cannot happen after a clean rehearsal on a clone of this exact
            // state, and worth saying out loud rather than ignoring if it
            // ever does.
            return Ok(StepOutcome::Refused {
                reason: format!(
                    "committed action was rejected after rehearsing cleanly: {}",
                    result.error.unwrap_or_default()
                ),
                refusals: Vec::new(),
            });
        }
        if result.budget_exhausted {
            return Ok(StepOutcome::BudgetSpent);
        }
    }

    let checkpoint = recorder.checkpoint()?;
    Ok(StepOutcome::Committed {
        estimate,
        new_subject: compiled.subject,
        checkpoint,
    })
}

/// What the rehearsal found wrong, if anything.
///
/// Three things, and none of them is visible to a static check:
///
/// * **A step that changed nothing.** Not an error to the engine, and a loop
///   that accepts it will propose it forever.
/// * **A new error cell.** A formula that parses, validates and evaluates to
///   `#VALUE!` has passed every check up to here.
/// * **A literal changed outside the declared scope.** The validator checks
///   the *actions*; this checks the *result*, which is what catches a spilled
///   array landing where nobody said it would.
fn rehearsal_problem(
    before: &Engine,
    after: &Engine,
    compiled: &crate::Compiled,
) -> Option<String> {
    let changed = env::task::changed_cells(before, after);
    if changed.is_empty() {
        return Some("it changed nothing".into());
    }

    let errors_before = count_errors(before);
    let errors_after = count_errors(after);
    if errors_after > errors_before {
        let first = first_new_error(before, after);
        return Some(match first {
            Some((at, code)) => format!("it would put {code} in {at}"),
            None => format!(
                "it would add {} error cell(s)",
                errors_after - errors_before
            ),
        });
    }

    for (sheet, addr) in &changed {
        let in_scope = compiled
            .scope
            .iter()
            .any(|(s, r)| s == sheet && r.contains(*addr));
        if in_scope {
            continue;
        }
        // Outside the scope, only recalculation is allowed: the cell has to
        // have already held a formula. Anything else means the step wrote
        // somewhere it did not say it would, by a route the action list did
        // not show.
        let was_formula = before
            .wb
            .sheet_by_name(sheet)
            .and_then(|s| s.cells.get(addr))
            .is_some_and(|c| c.is_formula());
        if !was_formula {
            return Some(format!(
                "it changed {sheet}!{} which is outside the range it declared",
                addr.to_a1()
            ));
        }
    }
    None
}

fn count_errors(engine: &Engine) -> usize {
    engine
        .wb
        .sheets
        .iter()
        .map(|s| {
            s.cells
                .keys()
                .filter(|a| matches!(s.value(**a), engine::Value::Error(_)))
                .count()
                + s.spill
                    .values()
                    .filter(|(_, v)| matches!(v, engine::Value::Error(_)))
                    .count()
        })
        .sum()
}

fn first_new_error(before: &Engine, after: &Engine) -> Option<(String, String)> {
    for sheet in &after.wb.sheets {
        let mut addrs: Vec<engine::CellAddr> = sheet.cells.keys().copied().collect();
        addrs.sort();
        for addr in addrs {
            let engine::Value::Error(kind) = sheet.value(addr) else {
                continue;
            };
            let was = before
                .wb
                .sheet_by_name(&sheet.name)
                .map(|s| s.value(addr))
                .unwrap_or(engine::Value::Empty);
            if !matches!(was, engine::Value::Error(_)) {
                return Some((
                    format!("{}!{}", sheet.name, addr.to_a1()),
                    kind.code().to_string(),
                ));
            }
        }
    }
    None
}

/// Compile errors go back to the planner as instructions, not as diagnostics.
/// "No column headed Quantity; this table has Item, Qty, Price" is something
/// a planner can act on; `NoSuchColumn` is not.
fn describe_compile_error(e: &CompileError) -> String {
    match e {
        CompileError::NoSuchColumn {
            looked_for,
            available,
        } => format!(
            "there is no column headed {looked_for:?}; the ones here are {}",
            available
                .iter()
                .filter(|h| !h.is_empty())
                .map(|h| format!("{h:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CompileError::NoSuchTable(headers) => format!(
            "no table has all of {}; locate one that exists",
            headers
                .iter()
                .map(|h| format!("{h:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CompileError::NoSubject => "the plan must locate a table before working on one".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ColumnRef, FormulaTemplate, RowRange};
    use env::task::Check;
    use env::SnapshotStore;

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

    fn task_and_env() -> (Env, TaskSpec) {
        let mut store = SnapshotStore::in_memory();
        let id = store.put(&ledger().wb).unwrap();
        let task = TaskSpec {
            id: "totals".into(),
            instruction: "Add a Total column: quantity times price for every row.".into(),
            initial_snapshot: id,
            checks: vec![
                // The header is part of "add a Total column", so the task
                // names it. Without this the agent does exactly the right
                // thing and D1 counts as an incidental change — which is the
                // grader being right about an under-specified task.
                Check::CellDisplays {
                    at: "D1".into(),
                    expect: "Total".into(),
                },
                Check::RangeFilled {
                    range: "D2:D4".into(),
                },
                Check::Unchanged {
                    ranges: vec!["A1:C4".into()],
                },
                Check::SumEquals {
                    range: "D2:D4".into(),
                    expect: 17.0,
                    tolerance: 0.0,
                },
            ],
            start_sheet: None,
            max_steps: 64,
            origin: None,
        };
        (Env::new(store), task)
    }

    /// A planner that returns a fixed sequence of plans, one per attempt, and
    /// remembers what it was told.
    struct Scripted {
        plans: Vec<Plan>,
        seen_feedback: Vec<String>,
    }

    impl Scripted {
        fn new(plans: Vec<Plan>) -> Self {
            Scripted {
                plans,
                seen_feedback: Vec::new(),
            }
        }
    }

    impl Planner for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }
        fn propose(&mut self, ctx: &PlanContext<'_>) -> Result<Plan, PlanError> {
            self.seen_feedback = ctx.feedback.to_vec();
            if self.plans.is_empty() {
                return Err(PlanError::NoIdea("out of scripted plans".into()));
            }
            Ok(self.plans.remove(0))
        }
    }

    fn good_plan() -> Plan {
        Plan::new(vec![
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
    }

    #[test]
    fn a_correct_plan_runs_and_passes() {
        let (env, task) = task_and_env();
        let mut planner = Scripted::new(vec![good_plan()]);
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert_eq!(outcome.ending, Ending::Declared, "{:?}", outcome.rejected);
        assert!(outcome.passed(), "{:?}", outcome.grade);
        assert_eq!(outcome.replans, 0);
        assert_eq!(
            outcome.incidental_changes(),
            0,
            "it should not have touched anything else"
        );
    }

    #[test]
    fn every_committed_step_leaves_a_checkpoint_that_can_be_resumed_from() {
        let (env, task) = task_and_env();
        let mut planner = Scripted::new(vec![good_plan()]);
        let (env, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert_eq!(outcome.applied.len(), 3);

        // The checkpoint after the derived column really is the state after
        // it, and resetting to it is a resume.
        let mut resumed = env;
        resumed
            .reset(&outcome.applied[1].checkpoint)
            .expect("the checkpoint must be in the store");
        assert_eq!(
            resumed.engine().unwrap().value_at("Sheet1", "D2"),
            engine::Value::Number(10.0)
        );
    }

    #[test]
    fn the_whole_run_replays_as_a_trajectory() {
        // The agent's output is a trajectory like any other and has to
        // satisfy the same invariant, or it cannot go in the dataset.
        let (env, task) = task_and_env();
        let mut planner = Scripted::new(vec![good_plan()]);
        let (env, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        let store = env.into_store();
        let report = env::trajectory::replay(store, &outcome.trajectory, Some(&task)).unwrap();
        assert!(report.faithful, "{report:?}");
        assert!(
            !outcome.trajectory.steps.is_empty(),
            "a run that did work must have recorded steps"
        );
        assert!(report.grade.unwrap().passed);
    }

    #[test]
    fn a_step_naming_a_column_that_is_not_there_is_told_which_ones_are() {
        // Feedback a planner can act on, not a diagnostic code.
        let (env, task) = task_and_env();
        let wrong = Plan::new(vec![
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into()],
            },
            Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Quantity}*{Price}"),
                rows: RowRange::TableBody,
            },
            Step::ExportWorkbook { path: None },
        ]);
        let mut planner = Scripted::new(vec![wrong, good_plan()]);
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();

        assert_eq!(outcome.replans, 1);
        assert!(outcome.passed(), "it should have recovered");
        let told = &outcome.rejected[0].reason;
        assert!(told.contains("Quantity"), "{told}");
        assert!(told.contains("Qty"), "{told}");
    }

    #[test]
    fn the_planner_is_actually_given_the_feedback() {
        // Otherwise "replanning" is just retrying, and the loop would burn
        // its whole budget proposing the same broken step.
        let (env, task) = task_and_env();
        let wrong = Plan::new(vec![
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Nope".into()],
            },
            Step::ExportWorkbook { path: None },
        ]);
        let mut planner = Scripted::new(vec![wrong, good_plan()]);
        let _ = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert_eq!(planner.seen_feedback.len(), 1);
        assert!(
            planner.seen_feedback[0].contains("Nope"),
            "{:?}",
            planner.seen_feedback
        );
    }

    #[test]
    fn the_steps_after_a_failed_one_are_abandoned_not_run_anyway() {
        // They were written assuming it succeeded. Running them anyway is
        // how an agent digs a hole.
        let (env, task) = task_and_env();
        let plan = Plan::new(vec![
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into()],
            },
            Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                formula: FormulaTemplate::new("={Nope}*2"),
                rows: RowRange::TableBody,
            },
            // This would have overwritten column A had it run.
            Step::ApplyFormula {
                at: "A2".into(),
                formula: FormulaTemplate::new("=1"),
            },
            Step::ExportWorkbook { path: None },
        ]);
        let mut planner = Scripted::new(vec![plan]);
        let (env, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert_eq!(outcome.rejected.len(), 1);
        assert_eq!(
            env.engine().unwrap().value_at("Sheet1", "A2"),
            engine::Value::Text("Bolt".into()),
            "a step after the failure ran"
        );
    }

    #[test]
    fn a_plan_that_never_says_it_is_done_runs_out_of_replans_rather_than_looping() {
        let (env, task) = task_and_env();
        let never_finishes = || {
            Plan::new(vec![Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into()],
            }])
        };
        let mut planner = Scripted::new(vec![
            never_finishes(),
            never_finishes(),
            never_finishes(),
            never_finishes(),
            never_finishes(),
            never_finishes(),
        ]);
        let config = RunConfig {
            max_replans: 2,
            ..RunConfig::default()
        };
        let (_, outcome) = run(env, &mut planner, &task, &config).unwrap();
        assert_eq!(outcome.ending, Ending::OutOfReplans);
        assert!(!outcome.passed());
    }

    #[test]
    fn a_planner_with_no_idea_ends_the_run_rather_than_being_asked_again() {
        let (env, task) = task_and_env();
        let mut planner = Scripted::new(vec![]);
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert!(matches!(outcome.ending, Ending::PlannerGaveUp { .. }));
        assert_eq!(
            outcome.trajectory.termination,
            Termination::Abandoned,
            "an abandoned run is not a demonstration"
        );
        assert!(!outcome.trajectory.is_demonstration());
    }

    #[test]
    fn a_step_that_would_change_nothing_is_refused() {
        // Not an error to the engine. A loop that accepts it proposes it
        // forever, and the rehearsal is the only place it shows up.
        let (env, task) = task_and_env();
        let plan = Plan::new(vec![
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into()],
            },
            // Writing "Bolt" over the "Bolt" that is already there.
            Step::ApplyFormula {
                at: "A2".into(),
                formula: FormulaTemplate::new("Bolt"),
            },
            Step::ExportWorkbook { path: None },
        ]);
        let mut planner = Scripted::new(vec![plan, good_plan()]);
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert_eq!(outcome.rejected.len(), 1);
        assert!(
            outcome.rejected[0].reason.contains("changed nothing"),
            "{}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn a_step_that_would_create_an_error_cell_is_refused_before_the_workbook_sees_it() {
        // It parses, it validates, and it evaluates to #VALUE!. Only running
        // it on a clone finds that out.
        let (env, task) = task_and_env();
        let plan = Plan::new(vec![
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into()],
            },
            Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::NextFree,
                // Item is text; multiplying it is a #VALUE!.
                formula: FormulaTemplate::new("={Item}*{Qty}"),
                rows: RowRange::TableBody,
            },
            Step::ExportWorkbook { path: None },
        ]);
        let mut planner = Scripted::new(vec![plan, good_plan()]);
        let (env, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();

        assert_eq!(outcome.rejected.len(), 1);
        assert!(
            outcome.rejected[0].reason.contains("#VALUE!"),
            "{}",
            outcome.rejected[0].reason
        );
        // ...and the real workbook never held the error.
        assert!(outcome.passed());
        assert_eq!(
            env.engine().unwrap().value_at("Sheet1", "D2"),
            engine::Value::Number(10.0)
        );
    }

    #[test]
    fn a_step_over_the_cell_limit_is_refused_with_the_number() {
        let (env, task) = task_and_env();
        let config = RunConfig {
            limits: Limits {
                max_cells: 2,
                ..Limits::default()
            },
            ..RunConfig::default()
        };
        let mut planner = Scripted::new(vec![good_plan()]);
        let (_, outcome) = run(env, &mut planner, &task, &config).unwrap();
        assert!(!outcome.passed());
        assert!(
            outcome.rejected[0].reason.contains("limit of 2"),
            "{}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn a_low_confidence_table_is_not_written_into() {
        // Table detection says how sure it is. Acting on a guess without
        // noticing it was a guess is how an agent writes formulas into
        // somebody's data.
        let mut store = SnapshotStore::in_memory();
        let mut e = Engine::new();
        // A header row over columns that mix text and numbers: detection
        // finds a table and says it is not sure the range is right.
        for (a1, v) in [
            ("A1", "One"),
            ("B1", "Two"),
            ("A2", "1"),
            ("B2", "text"),
            ("A3", "more text"),
            ("B3", "2"),
        ] {
            e.apply(&edit("Sheet1", a1, v)).unwrap();
        }
        let id = store.put(&e.wb).unwrap();
        let task = TaskSpec {
            id: "t".into(),
            instruction: "do something".into(),
            initial_snapshot: id,
            checks: vec![Check::SheetsExist {
                names: vec!["Sheet1".into()],
            }],
            start_sheet: None,
            max_steps: 16,
            origin: None,
        };
        let plan = Plan::new(vec![
            Step::LocateTable {
                sheet: Some("Sheet1".into()),
                must_have: vec![],
            },
            Step::ExportWorkbook { path: None },
        ]);
        let mut planner = Scripted::new(vec![plan]);
        let config = RunConfig {
            max_replans: 0,
            ..RunConfig::default()
        };
        let (_, outcome) = run(Env::new(store), &mut planner, &task, &config).unwrap();
        assert_eq!(outcome.rejected.len(), 1);
        assert!(
            outcome.rejected[0].reason.contains("confidence"),
            "{}",
            outcome.rejected[0].reason
        );
    }

    #[test]
    fn the_number_of_replans_is_reported_because_it_is_part_of_the_score() {
        // A policy that finishes in one plan is better than one that
        // finishes in five, and task completion alone cannot see it.
        let (env, task) = task_and_env();
        let bad = || {
            Plan::new(vec![
                Step::LocateTable {
                    sheet: None,
                    must_have: vec!["Nope".into()],
                },
                Step::ExportWorkbook { path: None },
            ])
        };
        let mut planner = Scripted::new(vec![bad(), bad(), good_plan()]);
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert!(outcome.passed());
        assert_eq!(outcome.replans, 2);
    }
}
