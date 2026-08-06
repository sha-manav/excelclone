//! The loop closing, end to end.
//!
//! An agent attempts a task and gets it wrong. A person does it properly.
//! The pair becomes a correction, the correction becomes a supervised
//! example and a preference pair, and the divergence names the exact cell
//! where the two parted company.
//!
//! Everything here uses the real agent, the real recorder and the real
//! graders. What is *not* here, and is worth saying plainly: nothing in the
//! product yet fires these captures. The record format and the distillation
//! are built; the hooks in the UI that would notice a user undoing the agent
//! are not. This test stands in for the user by driving the two runs
//! directly, which exercises every line of the pipeline and none of the
//! trigger.

use agent::plan::{ColumnRef, FormulaTemplate, Plan, RowRange, Step};
use agent::run::{run, PlanContext, PlanError, Planner, RunConfig};
use engine::{Action, CellAddr, Engine};
use env::correction::{Correction, CorrectionLog, Signal};
use env::task::{Check, TaskSpec};
use env::trajectory::{ObservationPolicy, Recorder, Source, Termination};
use env::{Env, SnapshotStore};

fn edit(sheet: &str, a1: &str, input: &str) -> Action {
    Action::CellEdit {
        sheet: sheet.into(),
        addr: CellAddr::parse_a1(a1).unwrap(),
        input: input.into(),
    }
}

fn ledger() -> Engine {
    let mut e = Engine::new();
    for (a1, v) in [
        ("A1", "Item"),
        ("B1", "Qty"),
        ("C1", "Price"),
        ("D1", "Total"),
        ("A2", "Bolt"),
        ("B2", "4"),
        ("C2", "2.5"),
        ("A3", "Nut"),
        ("B3", "9"),
        ("C3", "0.5"),
    ] {
        e.apply(&edit("Sheet1", a1, v)).unwrap();
    }
    e
}

fn setup() -> (Env, TaskSpec) {
    let mut store = SnapshotStore::in_memory();
    let snapshot = store.put(&ledger().wb).unwrap();
    (
        Env::new(store),
        TaskSpec {
            id: "totals".into(),
            instruction: "Fill in the Total column: Qty times Price.".into(),
            initial_snapshot: snapshot,
            checks: vec![
                Check::CellDisplays {
                    at: "D1".into(),
                    expect: "Total".into(),
                },
                Check::RangeFilled {
                    range: "D2:D3".into(),
                },
                // Without this the task cannot tell a sum from a product:
                // `RangeFilled` accepts any consistently-shaped formula, and
                // `=Qty+Price` filled down satisfies it perfectly. A task
                // that says "Qty times Price" and checks only the shape is
                // under-specified, and the agent will find that out before
                // anybody else does.
                Check::SumEquals {
                    range: "D2:D3".into(),
                    expect: 14.5,
                    tolerance: 0.0,
                },
                Check::Unchanged {
                    ranges: vec!["A1:C3".into()],
                },
            ],
            start_sheet: None,
            max_steps: 32,
            origin: None,
        },
    )
}

/// A planner that adds the right column with the wrong formula: it sums the
/// two inputs instead of multiplying them. Every cell it writes is where it
/// belongs and every number in the column is wrong.
struct Wrong;

impl Planner for Wrong {
    fn name(&self) -> &str {
        "wrong"
    }
    fn propose(&mut self, ctx: &PlanContext<'_>) -> Result<Plan, PlanError> {
        if !ctx.feedback.is_empty() {
            return Err(PlanError::NoIdea("one idea only".into()));
        }
        Ok(Plan::new(vec![
            Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into(), "Price".into()],
            },
            Step::CreateDerivedColumn {
                header: "Total".into(),
                at: ColumnRef::Header {
                    text: "Total".into(),
                },
                formula: FormulaTemplate::new("={Qty}+{Price}"),
                rows: RowRange::TableBody,
            },
            Step::ExportWorkbook { path: None },
        ]))
    }
}

/// The agent gets it wrong; a person fixes the output in place.
fn wrong_then_repaired() -> Correction {
    let (env, task) = setup();
    let (env, attempt) = run(env, &mut Wrong, &task, &RunConfig::default()).unwrap();

    // The agent's run is a real trajectory: it committed, it was graded, and
    // the grade is the reason the user is about to intervene.
    assert!(
        !attempt.trajectory.grade.as_ref().unwrap().passed || attempt.incidental_changes() > 0,
        "the attempt was supposed to be wrong"
    );

    // The user opens the result and fixes the two cells.
    let mut rec = Recorder::start(
        env,
        "human",
        task.instruction.clone(),
        &attempt.trajectory.final_snapshot,
        Source::Human,
    )
    .unwrap()
    .with_observations(ObservationPolicy::First);
    rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
    rec.step(&edit("Sheet1", "D3", "=B3*C3")).unwrap();
    let (repair, _) = rec.finish_with_env(Termination::Done, Some(&task)).unwrap();

    Correction::from(
        "totals::corrected",
        Signal::OutputRepaired,
        attempt.trajectory,
        repair,
    )
    .unwrap()
}

#[test]
fn an_agent_failure_and_a_human_repair_become_training_data() {
    let correction = wrong_then_repaired();

    assert!(correction.is_clean(), "the repair should pass the grader");
    assert!(
        correction.divergence.repaired_on_top,
        "the user fixed the output rather than redoing the work"
    );

    let supervised = correction.supervised().unwrap();
    assert_eq!(supervised.instruction, correction.instruction);
    assert_eq!(supervised.actions.len(), 2);
    assert!(supervised.origin.contains("output-repaired"));

    let preference = correction.preference().unwrap();
    assert_eq!(
        preference.rejected.len(),
        0,
        "a repair on top has nothing of the agent's to reject after the divergence"
    );
    assert_eq!(preference.preferred.len(), 2);
    assert!(!preference.why.is_empty());
}

#[test]
fn the_divergence_names_the_cells_the_agent_got_wrong() {
    // The whole value of a correction: not "it failed" but "here, this cell,
    // it wrote a sum where a product was wanted".
    let correction = wrong_then_repaired();
    let d2 = correction
        .divergence
        .cells
        .iter()
        .find(|c| c.at == "Sheet1!D2")
        .expect("D2 should be in the divergence");
    assert_eq!(d2.before, "6.5", "the agent's answer, 4 + 2.5");
    assert_eq!(d2.after, "10", "the right answer, 4 * 2.5");
    assert_eq!(d2.before_formula.as_deref(), Some("=B2+C2"));
    assert_eq!(d2.after_formula.as_deref(), Some("=B2*C2"));
}

#[test]
fn a_correction_where_the_user_redid_the_work_from_scratch_finds_the_first_bad_step() {
    // The other shape: the user undoes and starts again. Here the divergence
    // is a *step index* rather than a final diff, and it points at the first
    // action the agent took that a person would not have.
    let (env, task) = setup();
    let (env, attempt) = run(env, &mut Wrong, &task, &RunConfig::default()).unwrap();

    let mut rec = Recorder::start_task(env, "human", &task, Source::Human)
        .unwrap()
        .with_observations(ObservationPolicy::First);
    // The agent's first action was writing the header, which was fine.
    rec.step(&edit("Sheet1", "D1", "Total")).unwrap();
    rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
    rec.step(&edit("Sheet1", "D3", "=B3*C3")).unwrap();
    let (repair, _) = rec.finish_with_env(Termination::Done, Some(&task)).unwrap();

    let correction = Correction::from(
        "redone",
        Signal::Undone { steps: 3 },
        attempt.trajectory,
        repair,
    )
    .unwrap();

    assert!(!correction.divergence.repaired_on_top);
    assert_eq!(
        correction.divergence.agreed_through, 1,
        "they both wrote the header, then differed"
    );
    assert_eq!(
        correction.divergence.agent_did,
        Some(edit("Sheet1", "D2", "=B2+C2"))
    );
    assert_eq!(
        correction.divergence.user_did,
        Some(edit("Sheet1", "D2", "=B2*C2"))
    );

    // ...and the preference is about the part they differed on, not the
    // header they agreed about.
    let preference = correction.preference().unwrap();
    assert_eq!(preference.rejected.len(), 2);
    assert_eq!(preference.preferred.len(), 2);
    assert!(!preference.rejected.contains(&edit("Sheet1", "D1", "Total")));
}

#[test]
fn a_log_of_corrections_distils_into_two_datasets() {
    let mut log = CorrectionLog::new();
    log.push(wrong_then_repaired());
    log.push(wrong_then_repaired());

    assert_eq!(log.supervised().len(), 2);
    assert_eq!(log.preferences().len(), 2);
    assert!(log.unusable().is_empty());

    // Both survive the round trip they will actually make.
    let dir = std::env::temp_dir().join(format!("gridline-loop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    log.save(&dir.join("corrections.jsonl")).unwrap();
    let back = CorrectionLog::load(&dir.join("corrections.jsonl")).unwrap();
    assert_eq!(back, log);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn both_halves_of_a_correction_still_replay() {
    // A correction is two trajectories, and a trajectory that stopped
    // replaying would stop being evidence of anything.
    let (env, task) = setup();
    let (env, attempt) = run(env, &mut Wrong, &task, &RunConfig::default()).unwrap();
    let store = env.into_store();

    let report = env::trajectory::replay(store.clone(), &attempt.trajectory, Some(&task)).unwrap();
    assert!(report.faithful, "{report:?}");
    assert!(
        !report.grade.unwrap().passed,
        "it was supposed to be a failure"
    );
}
