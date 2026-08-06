//! The agent against the generated corpus.
//!
//! Unit tests can show that each piece works. What they cannot show is the
//! claim the whole design rests on: that a plan written once solves the task
//! again when the table has moved, the sheet has been renamed, an irrelevant
//! column has been dropped in the middle, and the data is thirty rows longer.
//!
//! That is the difference between an agent and a recorded macro, and it is
//! the only thing here worth calling a result. So these tests run the real
//! planner through the real loop against variants produced by the real
//! augmenter — and then assert the boring, important things: it passed, it
//! did not touch anything else, and what it did replays.

use agent::plan::{ColumnRef, FormulaTemplate, Plan, RowRange, Step};
use agent::run::{run, Ending, PlanContext, PlanError, Planner, RunConfig};
use agent::{Limits, RulePlanner};
use engine::{Action, CellAddr, Engine};
use env::augment::{augment, InsertFill, Perturbation, Recipe};
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

/// A five-row purchase ledger with an empty Total column.
fn ledger() -> Engine {
    let mut e = Engine::new();
    let rows = [
        ("Hex bolt M8", "120", "0.42"),
        ("Washer M8", "400", "0.03"),
        ("Nyloc nut M8", "120", "0.11"),
        ("Threadlock 50ml", "6", "7.95"),
        ("Anti-seize 100g", "3", "12.4"),
    ];
    for (a1, v) in [
        ("A1", "Item"),
        ("B1", "Qty"),
        ("C1", "Price"),
        ("D1", "Total"),
    ] {
        e.apply(&edit("Sheet1", a1, v)).unwrap();
    }
    for (i, (item, qty, price)) in rows.iter().enumerate() {
        let r = i + 2;
        e.apply(&edit("Sheet1", &format!("A{r}"), item)).unwrap();
        e.apply(&edit("Sheet1", &format!("B{r}"), qty)).unwrap();
        e.apply(&edit("Sheet1", &format!("C{r}"), price)).unwrap();
    }
    e
}

const INSTRUCTION: &str = "Fill in the Total column: Qty times Price for every row of the table.";

fn task(initial: env::SnapshotId) -> TaskSpec {
    TaskSpec {
        id: "ledger-totals".into(),
        instruction: INSTRUCTION.into(),
        initial_snapshot: initial,
        checks: vec![
            Check::RangeFilled {
                range: "D2:D6".into(),
            },
            Check::NoErrors {
                range: "A1:D6".into(),
            },
            Check::Unchanged {
                ranges: vec!["A1:C6".into()],
            },
            Check::SumEquals {
                range: "D2:D6".into(),
                expect: 160.5,
                tolerance: 0.0,
            },
        ],
        start_sheet: None,
        max_steps: 128,
        origin: None,
    }
}

fn seeded() -> (Env, TaskSpec) {
    let mut store = SnapshotStore::in_memory();
    let id = store.put(&ledger().wb).unwrap();
    (Env::new(store), task(id))
}

#[test]
fn the_rule_planner_solves_the_task_it_was_written_for() {
    let (env, task) = seeded();
    let mut planner = RulePlanner::new();
    let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
    assert_eq!(outcome.ending, Ending::Declared, "{:?}", outcome.rejected);
    assert!(outcome.passed(), "{:?}", outcome.grade);
    assert_eq!(outcome.replans, 0, "it should not have needed a second try");
    assert_eq!(
        outcome.incidental_changes(),
        0,
        "it wrote outside what the task asked for"
    );
}

/// Build the variants the agent will be tested against, from a validated
/// human demonstration of the same task.
fn variants() -> Vec<(Env, TaskSpec)> {
    let mut store = SnapshotStore::in_memory();
    let id = store.put(&ledger().wb).unwrap();
    let base = task(id);

    let mut rec = Recorder::start_task(Env::new(store), "human", &base, Source::Human)
        .unwrap()
        .with_observations(ObservationPolicy::None);
    for row in 2..=6 {
        rec.step(&edit(
            "Sheet1",
            &format!("D{row}"),
            &format!("=B{row}*C{row}"),
        ))
        .unwrap();
    }
    let (demonstration, env) = rec.finish_with_env(Termination::Done, Some(&base)).unwrap();
    assert!(
        demonstration.is_demonstration(),
        "the demonstration itself must be valid"
    );

    let recipes = vec![
        Recipe::new(
            "moved-down-and-right",
            vec![
                Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: engine::Axis::Row,
                    at: 0,
                    count: 4,
                    fill: None,
                },
                Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: engine::Axis::Col,
                    at: 0,
                    count: 2,
                    fill: None,
                },
            ],
        ),
        Recipe::new(
            "renamed-sheet",
            vec![Perturbation::RenameSheet {
                from: "Sheet1".into(),
                to: "Q3 Purchases".into(),
            }],
        ),
        Recipe::new(
            "distractor-column",
            vec![Perturbation::Insert {
                sheet: "Sheet1".into(),
                axis: engine::Axis::Col,
                at: 1,
                count: 1,
                // Filled, so it is a column to ignore rather than a gap that
                // splits the table in two.
                fill: Some(InsertFill {
                    header: "Bin".into(),
                    value: "A-12".into(),
                }),
            }],
        ),
        Recipe::new(
            "longer-table",
            vec![Perturbation::AppendRows {
                sheet: "Sheet1".into(),
                count: 30,
            }],
        ),
        Recipe::new(
            "everything-at-once",
            vec![
                Perturbation::RenameSheet {
                    from: "Sheet1".into(),
                    to: "Q3 Purchases".into(),
                },
                Perturbation::Insert {
                    sheet: "Q3 Purchases".into(),
                    axis: engine::Axis::Row,
                    at: 0,
                    count: 6,
                    fill: None,
                },
                Perturbation::Insert {
                    sheet: "Q3 Purchases".into(),
                    axis: engine::Axis::Col,
                    at: 2,
                    count: 1,
                    fill: Some(InsertFill {
                        header: "Bin".into(),
                        value: "A-12".into(),
                    }),
                },
                Perturbation::AppendRows {
                    sheet: "Q3 Purchases".into(),
                    count: 12,
                },
            ],
        ),
    ];

    let (env, report) = augment(env, &demonstration, &base, &recipes).unwrap();
    assert!(
        report.rejected.is_empty(),
        "the corpus itself failed to generate: {:?}",
        report.rejected
    );
    assert_eq!(report.accepted.len(), 5);

    let store = env.into_store();
    report
        .accepted
        .into_iter()
        .map(|v| (Env::new(store.clone()), v.task))
        .collect()
}

#[test]
fn the_same_plan_solves_every_generated_variant() {
    // The claim the whole planner/compiler split exists to support. A macro
    // recorded on the original workbook fails all five of these.
    for (env, task) in variants() {
        let mut planner = RulePlanner::new();
        let (_, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        assert!(
            outcome.passed(),
            "{}: {:?} / rejected {:?}",
            task.id,
            outcome.grade,
            outcome.rejected
        );
        assert_eq!(
            outcome.incidental_changes(),
            0,
            "{} touched cells nobody asked about",
            task.id
        );
        assert_eq!(outcome.replans, 0, "{} needed a second try", task.id);
    }
}

#[test]
fn a_macro_of_the_original_addresses_fails_where_the_agent_succeeds() {
    // The control. Without it, "the agent solved the variants" could just
    // mean the variants were not different enough to matter.
    for (mut env, task) in variants() {
        env.reset_for(&task).unwrap();
        for row in 2..=6 {
            // Replaying the literal actions of the original demonstration.
            let _ = env.step(&edit(
                "Sheet1",
                &format!("D{row}"),
                &format!("=B{row}*C{row}"),
            ));
        }
        let grade = env.grade(&task).unwrap();
        assert!(
            !grade.passed,
            "{} was solved by fixed-address replay, so it is not a real variant",
            task.id
        );
    }
}

#[test]
fn every_run_the_agent_produces_replays() {
    // The agent's output is a trajectory like any other. If it did not
    // replay it could not go in the dataset, and the whole loop closing
    // depends on it going in the dataset.
    for (env, task) in variants() {
        let mut planner = RulePlanner::new();
        let (env, outcome) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();
        let store = env.into_store();
        let report = env::trajectory::replay(store, &outcome.trajectory, Some(&task)).unwrap();
        assert!(report.faithful, "{}: {report:?}", task.id);
        assert!(!outcome.trajectory.steps.is_empty());
        assert!(report.grade.unwrap().passed, "{}", task.id);
    }
}

#[test]
fn a_plan_that_would_wreck_the_sheet_is_stopped_before_it_touches_it() {
    // The validator, exercised through the whole loop rather than alone.
    // A plan can be perfectly well-formed and still be something nobody
    // asked for.
    struct Vandal;
    impl Planner for Vandal {
        fn name(&self) -> &str {
            "vandal"
        }
        fn propose(&mut self, _: &PlanContext<'_>) -> Result<Plan, PlanError> {
            Ok(Plan::new(vec![
                Step::LocateTable {
                    sheet: None,
                    must_have: vec!["Qty".into()],
                },
                // Overwriting the source data with the answer.
                Step::CreateDerivedColumn {
                    header: "Qty".into(),
                    at: ColumnRef::Header { text: "Qty".into() },
                    formula: FormulaTemplate::new("={Qty}*{Price}"),
                    rows: RowRange::TableBody,
                },
                Step::ExportWorkbook { path: None },
            ]))
        }
    }

    let (env, task) = seeded();
    let config = RunConfig {
        max_replans: 0,
        limits: Limits::default(),
        ..RunConfig::default()
    };
    let (env, outcome) = run(env, &mut Vandal, &task, &config).unwrap();
    assert!(!outcome.passed());
    assert_eq!(outcome.rejected.len(), 1);
    // ...and the source column is exactly as it was.
    assert_eq!(
        env.engine().unwrap().value_at("Sheet1", "B2"),
        engine::Value::Number(120.0),
        "the data was modified despite the step being refused"
    );
}

#[test]
fn a_thirty_five_row_table_costs_the_same_number_of_plan_steps_as_a_five_row_one() {
    // A plan is a description of intent, so it does not get longer when the
    // data does. The *actions* do, which is exactly the right place for the
    // size of the job to show up.
    let (env, task) = seeded();
    let mut planner = RulePlanner::new();
    let (_, small) = run(env, &mut planner, &task, &RunConfig::default()).unwrap();

    let long = variants()
        .into_iter()
        .find(|(_, t)| t.id.contains("longer-table"))
        .expect("the longer-table variant");
    let mut planner = RulePlanner::new();
    let (_, big) = run(long.0, &mut planner, &long.1, &RunConfig::default()).unwrap();

    assert!(big.passed());
    assert_eq!(small.applied.len(), big.applied.len());
    assert!(
        big.applied[1].estimate.written > small.applied[1].estimate.written * 5,
        "the longer table should have cost more actions: {} vs {}",
        big.applied[1].estimate.written,
        small.applied[1].estimate.written
    );
}

#[test]
fn a_second_pass_over_the_same_corpus_costs_less_and_scores_no_worse() {
    // The claim distillation rests on, measured rather than asserted: after
    // one pass, the shapes that recurred are policies, and the second pass
    // answers from them instead of planning. The score must not drop —
    // cheaper *and worse* is not an improvement, it is a regression with a
    // budget line.
    use agent::memory::{MemoPlanner, PlanLibrary};
    use agent::policy::Router;

    let corpus = variants();
    let mut library = PlanLibrary::new();
    let mut first_pass_passed = 0;
    let mut first_pass_planner_calls = 0;
    let mut first_pass_replans = 0;

    for (env, task) in corpus {
        let mut memo = MemoPlanner::new(Vec::new());
        let mut rules = RulePlanner::new();
        let mut router = Router::new(&mut memo, &mut rules, 0.7);
        let (_, outcome) = run(env, &mut router, &task, &RunConfig::default()).unwrap();
        first_pass_planner_calls += router.stats().slow;
        first_pass_replans += outcome.replans;
        if outcome.passed() {
            first_pass_passed += 1;
        }
        library.remember(&outcome);
    }
    assert!(first_pass_passed > 0, "the first pass solved nothing");
    assert!(first_pass_planner_calls > 0);

    let policies = library.cluster(2);
    assert!(
        !policies.is_empty(),
        "nothing recurred often enough to distil"
    );

    let mut second_pass_passed = 0;
    let mut second_pass_planner_calls = 0;
    let mut answered_from_memory = 0;
    for (env, task) in variants() {
        let mut memo = MemoPlanner::new(policies.clone());
        let mut rules = RulePlanner::new();
        let mut router = Router::new(&mut memo, &mut rules, 0.7);
        let (_, outcome) = run(env, &mut router, &task, &RunConfig::default()).unwrap();
        second_pass_planner_calls += router.stats().slow;
        answered_from_memory += router.stats().fast;
        if outcome.passed() {
            second_pass_passed += 1;
        }
        assert_eq!(
            outcome.incidental_changes(),
            0,
            "{}: a remembered policy touched cells nobody asked about",
            task.id
        );
    }

    assert!(answered_from_memory > 0, "memory answered nothing");
    assert!(
        second_pass_planner_calls < first_pass_planner_calls,
        "memory saved no planner calls: {second_pass_planner_calls} vs {first_pass_planner_calls}"
    );
    assert!(
        second_pass_passed >= first_pass_passed,
        "memory made it cheaper and worse: {second_pass_passed} vs {first_pass_passed}"
    );
    let _ = first_pass_replans;
}
