//! What happened, recorded so it can be replayed.
//!
//! A trajectory is the unit of the dataset: one instruction, one starting
//! state, the ordered actions that were taken, and the grader's verdict. It
//! is written as one JSON object per line, and it is designed around a single
//! requirement that turns out to constrain everything else —
//!
//! **A trajectory must replay.** Given its `initial_snapshot` and its
//! actions, running them again has to reproduce every per-step state hash it
//! recorded. That is what makes a recorded demonstration a *fact* rather than
//! a claim: a trajectory that no longer replays is either corrupt or the
//! engine changed underneath it, and either way it must stop being training
//! data before it teaches something that is no longer true.
//!
//! Two consequences of that requirement:
//!
//! * **The workbook is never inline.** Starting and ending states are stored
//!   in the snapshot store and named by hash. Ten thousand augmented variants
//!   of one task reference a handful of snapshots between them, and the JSONL
//!   file stays something a person can open.
//! * **Observations are recorded but not trusted.** They are what the policy
//!   *saw*, which is the input side of a training example — but nothing in
//!   replay reads them, so a change to the summarizer invalidates no
//!   trajectory.

use engine::{Action, CellAddr, Engine, Value};
use serde::{Deserialize, Serialize};

use crate::observe::WorkbookObservation;
use crate::snapshot::{SnapshotId, SnapshotStore};
use crate::task::{GradeResult, TaskSpec};
use crate::{Env, EnvError};

/// How many cells a final diff will name before it summarises. Larger than
/// the per-step budget: this one is written once per episode and is the thing
/// somebody reads when asking what the agent actually did.
const DIFF_BUDGET: usize = 512;

/// Why an episode stopped.
///
/// Recorded rather than inferred, because "ran out of steps" and "decided it
/// was done" produce the same final state surprisingly often and mean
/// completely different things about the policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "termination", rename_all = "snake_case")]
pub enum Termination {
    /// The actor said it was finished.
    Done,
    /// The step budget ran out first.
    BudgetExhausted,
    /// A human walked away, or a recording session ended mid-task. Not a
    /// failure — an incomplete demonstration, which is a different thing and
    /// must not be trained on as if the last state were the answer.
    Abandoned,
    /// A human rejected the result. Phase 3's raw material.
    Rejected,
    /// The harness itself broke.
    Failed { reason: String },
}

/// Where a trajectory came from. Kept so a score can be broken down by
/// provenance — a policy that only wins on generated variants has learned the
/// generator, not the task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Source {
    /// Captured from somebody using the spreadsheet.
    Human,
    /// Produced by a policy under evaluation.
    Policy { name: String },
    /// Generated from another trajectory by a perturbation.
    Augmented { from: String, perturbation: String },
    /// Written by hand, usually as a fixture.
    Authored,
}

/// One step: what was seen, what was done, and what state that left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrajectoryStep {
    /// 1-based.
    pub index: u32,
    /// What the actor saw before choosing. Omitted under the cheaper
    /// recording policies; never read by replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<WorkbookObservation>,
    pub action: Action,
    /// False when the engine refused. A refused step is kept, not dropped:
    /// what a policy tried and could not do is the most informative thing in
    /// the record, and dropping it would make the step count a lie.
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The workbook hash *after* this step. This is what replay checks.
    pub state_hash: String,
    /// Cells this step touched, capped as `StepResult::changed` is.
    pub changed: Vec<String>,
}

/// One cell's before and after.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellDiff {
    pub at: String,
    pub before: String,
    pub after: String,
    /// Formulas separately from displayed values, because replacing `=B2*C2`
    /// with the literal `10` changes nothing you can see and everything that
    /// matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_formula: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_formula: Option<String>,
}

/// Everything that differs between the starting and ending workbooks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkbookDiff {
    pub cells: Vec<CellDiff>,
    pub total: u32,
    pub truncated: bool,
    pub sheets_added: Vec<String>,
    pub sheets_removed: Vec<String>,
}

/// One episode, end to end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trajectory {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub instruction: String,
    pub initial_snapshot: SnapshotId,
    /// The state it ended in, also by hash. Equal to the last step's
    /// `state_hash`, and stored anyway so a consumer that reads only the
    /// header knows where it landed.
    pub final_snapshot: SnapshotId,
    pub steps: Vec<TrajectoryStep>,
    pub diff: WorkbookDiff,
    pub termination: Termination,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<GradeResult>,
    pub source: Source,
    /// The engine that produced it. A trajectory recorded against a different
    /// engine may legitimately fail to replay, and without this the failure
    /// looks like corruption.
    pub engine_version: String,
}

impl Trajectory {
    /// Whether this is fit to train on: it finished, and the grader agreed.
    ///
    /// An abandoned episode is not a failure but it is not a demonstration
    /// either — its last state is where somebody stopped, not the answer.
    pub fn is_demonstration(&self) -> bool {
        matches!(self.termination, Termination::Done)
            && self.grade.as_ref().is_some_and(|g| g.passed)
    }

    pub fn actions(&self) -> impl Iterator<Item = &Action> {
        self.steps.iter().map(|s| &s.action)
    }
}

/// How much of what the policy saw to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationPolicy {
    /// Every step. What supervised training wants, and the largest.
    #[default]
    Every,
    /// The first only — enough to know what the task looked like, for
    /// augmented variants where every step's observation is derivable.
    First,
    /// None. For replay-validation runs, where they are dead weight.
    None,
}

/// What a recorded step did, for the caller that has to decide what next.
///
/// Everything here is also in the trajectory; this is the caller's copy, so a
/// loop does not have to reach back into the recording to find out whether
/// its own action was accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub applied: bool,
    pub error: Option<String>,
    pub budget_exhausted: bool,
}

/// Records a trajectory while an episode runs.
///
/// Wraps the `Env` rather than sitting beside it so that no step can be taken
/// without being recorded. An unrecorded step would leave a trajectory that
/// does not replay, and the whole format rests on replay.
pub struct Recorder {
    env: Env,
    id: String,
    task_id: Option<String>,
    instruction: String,
    initial: SnapshotId,
    source: Source,
    policy: ObservationPolicy,
    steps: Vec<TrajectoryStep>,
}

impl Recorder {
    /// Start recording from a snapshot. `id` names the trajectory; the caller
    /// supplies it because the environment has no clock and no randomness —
    /// both would make a run unreproducible, which is the one thing this
    /// crate cannot afford.
    pub fn start(
        mut env: Env,
        id: impl Into<String>,
        instruction: impl Into<String>,
        snapshot: &SnapshotId,
        source: Source,
    ) -> Result<Self, EnvError> {
        env.reset(snapshot)?;
        Ok(Recorder {
            env,
            id: id.into(),
            task_id: None,
            instruction: instruction.into(),
            initial: snapshot.clone(),
            source,
            policy: ObservationPolicy::default(),
            steps: Vec::new(),
        })
    }

    /// Start from a task, adopting its instruction, sheet and step budget.
    pub fn start_task(
        mut env: Env,
        id: impl Into<String>,
        task: &TaskSpec,
        source: Source,
    ) -> Result<Self, EnvError> {
        env.reset_for(task)?;
        Ok(Recorder {
            env,
            id: id.into(),
            task_id: Some(task.id.clone()),
            instruction: task.instruction.clone(),
            initial: task.initial_snapshot.clone(),
            source,
            policy: ObservationPolicy::default(),
            steps: Vec::new(),
        })
    }

    pub fn with_observations(mut self, policy: ObservationPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    /// Store the current workbook and return its id — a resume point.
    ///
    /// This and the two below are what a caller is given instead of `&mut
    /// Env`. Handing out the environment would let a caller step it directly,
    /// and a step taken outside the recorder is a step missing from the
    /// trajectory: the record would then replay to a state nobody was ever
    /// in, and `faithful` would say yes because there was nothing to check.
    /// That is not hypothetical — the agent's execution loop did exactly
    /// that, and produced empty trajectories that replayed perfectly.
    pub fn checkpoint(&mut self) -> Result<SnapshotId, EnvError> {
        self.env.checkpoint()
    }

    pub fn set_selection(&mut self, a1: &str) {
        self.env.set_selection(a1);
    }

    pub fn set_active_sheet(&mut self, name: &str) -> Result<(), EnvError> {
        self.env.set_active_sheet(name)
    }

    pub fn observe(&mut self) -> Result<WorkbookObservation, EnvError> {
        self.env.observe()
    }

    pub fn steps_taken(&self) -> u32 {
        self.env.steps_taken()
    }

    /// Take a step and record it.
    pub fn step(&mut self, action: &Action) -> Result<Recorded, EnvError> {
        let observation = match self.policy {
            ObservationPolicy::Every => Some(self.env.observe()?),
            ObservationPolicy::First if self.steps.is_empty() => Some(self.env.observe()?),
            _ => None,
        };
        let result = self.env.step(action)?;
        let recorded = Recorded {
            applied: result.applied,
            error: result.error.clone(),
            budget_exhausted: result.budget_exhausted,
        };
        self.steps.push(TrajectoryStep {
            index: result.step,
            observation,
            action: action.clone(),
            applied: result.applied,
            error: result.error,
            state_hash: result.state_hash,
            changed: result.changed,
        });
        Ok(recorded)
    }

    /// Close the recording. Stores the final workbook so the trajectory can
    /// name it, computes the diff, and grades if a task was given.
    pub fn finish(
        self,
        termination: Termination,
        task: Option<&TaskSpec>,
    ) -> Result<Trajectory, EnvError> {
        Ok(self.finish_with_env(termination, task)?.0)
    }

    /// Close the recording and hand the environment back, so a batch can run
    /// the next episode against the same snapshot store.
    pub fn finish_with_env(
        mut self,
        termination: Termination,
        task: Option<&TaskSpec>,
    ) -> Result<(Trajectory, Env), EnvError> {
        let grade = match task {
            Some(t) => Some(self.env.grade(t)?),
            None => None,
        };
        let diff = diff_of(self.env.initial()?, self.env.engine()?)?;
        let final_snapshot = self.env.checkpoint()?;
        Ok((
            Trajectory {
                id: self.id,
                task_id: self.task_id.or_else(|| task.map(|t| t.id.clone())),
                instruction: self.instruction,
                initial_snapshot: self.initial,
                final_snapshot,
                steps: self.steps,
                diff,
                termination,
                grade,
                source: self.source,
                engine_version: engine::engine_version().to_string(),
            },
            self.env,
        ))
    }

    /// Give the recorder back its environment without producing a trajectory
    /// — for a run that turned out not to be worth keeping.
    pub fn discard(self) -> Env {
        self.env
    }
}

/// What changed between two workbooks, in the form a person reads.
pub fn diff_of(before: &Engine, after: &Engine) -> Result<WorkbookDiff, EnvError> {
    let changed = crate::task::changed_cells(before, after);
    let total = changed.len() as u32;
    let cells: Vec<CellDiff> = changed
        .iter()
        .take(DIFF_BUDGET)
        .map(|(sheet, addr)| CellDiff {
            at: format!("{sheet}!{}", addr.to_a1()),
            before: display(before, sheet, *addr),
            after: display(after, sheet, *addr),
            before_formula: formula(before, sheet, *addr),
            after_formula: formula(after, sheet, *addr),
        })
        .collect();

    let names =
        |e: &Engine| -> Vec<String> { e.wb.sheets.iter().map(|s| s.name.clone()).collect() };
    let (was, now) = (names(before), names(after));
    Ok(WorkbookDiff {
        truncated: total as usize > cells.len(),
        cells,
        total,
        sheets_added: now.iter().filter(|n| !was.contains(n)).cloned().collect(),
        sheets_removed: was.iter().filter(|n| !now.contains(n)).cloned().collect(),
    })
}

fn display(engine: &Engine, sheet: &str, addr: CellAddr) -> String {
    engine
        .wb
        .sheet_by_name(sheet)
        .map(|s| s.value(addr))
        .unwrap_or(Value::Empty)
        .display()
}

fn formula(engine: &Engine, sheet: &str, addr: CellAddr) -> Option<String> {
    engine
        .wb
        .sheet_by_name(sheet)?
        .cells
        .get(&addr)
        .filter(|c| c.is_formula())
        .map(|c| c.input())
}

/// What replaying a trajectory found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub trajectory_id: String,
    /// Every recorded state hash was reproduced.
    pub faithful: bool,
    /// The first step whose hash differed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diverged_at: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The grade a re-run produces, when a task was supplied. A trajectory
    /// that replays faithfully but no longer passes means the *task* moved,
    /// not the engine — worth telling apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<GradeResult>,
}

/// Replay a trajectory and check it against itself.
///
/// This is the validation gate for the dataset: nothing enters it without
/// passing here, and everything in it is re-checked when the engine changes.
/// The check is per-step rather than only on the final state, because two
/// different sequences can arrive at the same workbook and only one of them
/// is what was recorded.
pub fn replay(
    store: SnapshotStore,
    trajectory: &Trajectory,
    task: Option<&TaskSpec>,
) -> Result<ReplayReport, EnvError> {
    let mut env = Env::new(store);
    env.reset(&trajectory.initial_snapshot)?;
    // Replay must not be stopped by the recorded episode's own budget.
    for step in &trajectory.steps {
        let result = env.step(&step.action)?;
        if result.state_hash != step.state_hash {
            return Ok(ReplayReport {
                trajectory_id: trajectory.id.clone(),
                faithful: false,
                diverged_at: Some(step.index),
                detail: Some(format!(
                    "step {} recorded {} but replayed to {}",
                    step.index, step.state_hash, result.state_hash
                )),
                grade: None,
            });
        }
        if result.applied != step.applied {
            return Ok(ReplayReport {
                trajectory_id: trajectory.id.clone(),
                faithful: false,
                diverged_at: Some(step.index),
                detail: Some(format!(
                    "step {} was recorded as {} but replayed as {}",
                    step.index,
                    if step.applied { "applied" } else { "refused" },
                    if result.applied { "applied" } else { "refused" },
                )),
                grade: None,
            });
        }
    }
    let grade = match task {
        Some(t) => Some(env.grade(t)?),
        None => None,
    };
    Ok(ReplayReport {
        trajectory_id: trajectory.id.clone(),
        faithful: true,
        diverged_at: None,
        detail: None,
        grade,
    })
}

// --- the dataset on disk ----------------------------------------------------

/// Append trajectories to a JSONL file.
///
/// One object per line, no enclosing array: a dataset that has to be parsed
/// whole before its first record can be read is a dataset nobody streams, and
/// an interrupted write of a JSON array leaves a file that will not parse at
/// all, where an interrupted JSONL write loses one line.
pub fn append_jsonl(path: &std::path::Path, trajectories: &[Trajectory]) -> Result<(), EnvError> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for t in trajectories {
        let line = serde_json::to_string(t)?;
        debug_assert!(!line.contains('\n'));
        writeln!(file, "{line}")?;
    }
    Ok(())
}

pub fn load_jsonl(path: &std::path::Path) -> Result<Vec<Trajectory>, EnvError> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Check;

    fn edit(sheet: &str, a1: &str, input: &str) -> Action {
        Action::CellEdit {
            sheet: sheet.into(),
            addr: CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        }
    }

    /// A ledger with an empty Total column, stored, plus the store it is in.
    fn seeded() -> (SnapshotStore, SnapshotId) {
        let mut base = Engine::new();
        for (a1, input) in [
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
            base.apply(&edit("Sheet1", a1, input)).unwrap();
        }
        let mut store = SnapshotStore::in_memory();
        let id = store.put(&base.wb).unwrap();
        (store, id)
    }

    fn task(id: &SnapshotId) -> TaskSpec {
        TaskSpec {
            id: "totals".into(),
            instruction: "Fill the Total column".into(),
            initial_snapshot: id.clone(),
            checks: vec![
                Check::RangeFilled {
                    range: "D2:D3".into(),
                },
                Check::Unchanged {
                    ranges: vec!["A1:C3".into()],
                },
            ],
            start_sheet: None,
            max_steps: 8,
            origin: None,
        }
    }

    fn solved() -> (SnapshotStore, TaskSpec, Trajectory) {
        let (store, id) = seeded();
        let spec = task(&id);
        let mut rec =
            Recorder::start_task(Env::new(store), "traj-1", &spec, Source::Human).unwrap();
        rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        rec.step(&edit("Sheet1", "D3", "=B3*C3")).unwrap();
        let t = rec.finish(Termination::Done, Some(&spec)).unwrap();
        // The recorder owns the store; rebuild one holding the same content.
        let (store, _) = seeded();
        (store, spec, t)
    }

    #[test]
    fn a_recorded_trajectory_replays_to_the_same_hashes() {
        // The whole format rests on this. Everything else is presentation.
        let (store, spec, t) = solved();
        assert_eq!(t.steps.len(), 2);
        let report = replay(store, &t, Some(&spec)).unwrap();
        assert!(report.faithful, "{report:?}");
        assert!(report.grade.unwrap().passed);
    }

    #[test]
    fn a_tampered_trajectory_does_not_replay() {
        // The negative half, and the one that matters: a validation gate that
        // passes everything is not a gate. Swapping one action leaves a
        // record that still looks plausible and no longer reproduces.
        let (store, _, mut t) = solved();
        t.steps[1].action = edit("Sheet1", "D3", "=B3*C3+1");
        let report = replay(store, &t, None).unwrap();
        assert!(!report.faithful);
        assert_eq!(report.diverged_at, Some(2));
        assert!(report.detail.unwrap().contains("replayed to"));
    }

    #[test]
    fn a_trajectory_with_a_doctored_hash_does_not_replay() {
        // The other direction: right actions, wrong recorded state.
        let (store, _, mut t) = solved();
        t.steps[0].state_hash = "0".repeat(32);
        let report = replay(store, &t, None).unwrap();
        assert!(!report.faithful);
        assert_eq!(report.diverged_at, Some(1));
    }

    #[test]
    fn a_refused_step_is_kept_and_replays_as_a_refusal() {
        // Dropping refused steps would make the step count a lie and throw
        // away the most informative thing in the record.
        let (store, id) = seeded();
        let mut rec = Recorder::start(
            Env::new(store),
            "traj-2",
            "try something impossible",
            &id,
            Source::Policy {
                name: "test".into(),
            },
        )
        .unwrap();
        rec.step(&edit("NoSuchSheet", "A1", "1")).unwrap();
        rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        let t = rec.finish(Termination::Done, None).unwrap();

        assert_eq!(t.steps.len(), 2);
        assert!(!t.steps[0].applied);
        assert!(t.steps[0].error.is_some());
        assert_eq!(
            t.steps[0].state_hash, t.initial_snapshot.0,
            "a refused step must leave the state alone"
        );

        let (store, _) = seeded();
        assert!(replay(store, &t, None).unwrap().faithful);
    }

    #[test]
    fn the_final_diff_shows_what_the_episode_actually_did() {
        let (_, _, t) = solved();
        assert_eq!(t.diff.total, 2);
        let d = t.diff.cells.iter().find(|c| c.at == "Sheet1!D2").unwrap();
        assert_eq!(d.before, "");
        assert_eq!(d.after, "10");
        assert_eq!(d.before_formula, None);
        assert_eq!(d.after_formula.as_deref(), Some("=B2*C2"));
    }

    #[test]
    fn a_literal_replacing_a_formula_is_visible_in_the_diff() {
        // Same displayed value, completely different workbook. A diff that
        // compared only what you can see would call this no change.
        let (store, id) = seeded();
        let mut rec = Recorder::start(Env::new(store), "t", "x", &id, Source::Authored).unwrap();
        rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        rec.step(&edit("Sheet1", "D2", "10")).unwrap();
        let t = rec.finish(Termination::Done, None).unwrap();

        let d = t.diff.cells.iter().find(|c| c.at == "Sheet1!D2").unwrap();
        assert_eq!(d.after, "10");
        assert_eq!(
            d.after_formula, None,
            "the formula was replaced by a literal and the diff must say so"
        );
    }

    #[test]
    fn an_abandoned_episode_is_not_a_demonstration_even_if_it_passed() {
        // Its last state is where somebody stopped, not the answer. Training
        // on it teaches the agent to stop early.
        let (store, id) = seeded();
        let spec = task(&id);
        let mut rec = Recorder::start_task(Env::new(store), "t", &spec, Source::Human).unwrap();
        rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        rec.step(&edit("Sheet1", "D3", "=B3*C3")).unwrap();
        let t = rec.finish(Termination::Abandoned, Some(&spec)).unwrap();
        assert!(t.grade.as_ref().unwrap().passed);
        assert!(!t.is_demonstration());
    }

    #[test]
    fn a_failed_episode_is_not_a_demonstration() {
        let (store, id) = seeded();
        let spec = task(&id);
        let mut rec = Recorder::start_task(Env::new(store), "t", &spec, Source::Human).unwrap();
        rec.step(&edit("Sheet1", "D2", "10")).unwrap();
        let t = rec.finish(Termination::Done, Some(&spec)).unwrap();
        assert!(!t.grade.as_ref().unwrap().passed);
        assert!(!t.is_demonstration());
    }

    #[test]
    fn the_step_budget_is_reported_rather_than_enforced_silently() {
        let (store, id) = seeded();
        let mut spec = task(&id);
        spec.max_steps = 1;
        let mut rec = Recorder::start_task(Env::new(store), "t", &spec, Source::Human).unwrap();
        assert!(
            rec.step(&edit("Sheet1", "D2", "=B2*C2"))
                .unwrap()
                .budget_exhausted
        );
        let t = rec
            .finish(Termination::BudgetExhausted, Some(&spec))
            .unwrap();
        assert_eq!(t.termination, Termination::BudgetExhausted);
        assert!(!t.is_demonstration());
    }

    #[test]
    fn observations_are_recorded_but_replay_ignores_them() {
        // A change to the summarizer must not invalidate the dataset.
        let (store, _, mut t) = solved();
        assert!(t.steps[0].observation.is_some());
        for step in &mut t.steps {
            step.observation = None;
        }
        assert!(replay(store, &t, None).unwrap().faithful);
    }

    #[test]
    fn the_cheap_recording_policies_keep_fewer_observations() {
        let (store, id) = seeded();
        let mut rec = Recorder::start(Env::new(store), "t", "x", &id, Source::Authored)
            .unwrap()
            .with_observations(ObservationPolicy::First);
        rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        rec.step(&edit("Sheet1", "D3", "=B3*C3")).unwrap();
        let t = rec.finish(Termination::Done, None).unwrap();
        assert!(t.steps[0].observation.is_some());
        assert!(t.steps[1].observation.is_none());
    }

    #[test]
    fn a_trajectory_survives_a_round_trip_through_jsonl() {
        let (_, _, t) = solved();
        let dir = std::env::temp_dir().join(format!("gridline-traj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("dataset.jsonl");
        append_jsonl(&path, std::slice::from_ref(&t)).unwrap();
        append_jsonl(&path, std::slice::from_ref(&t)).unwrap();
        let back = load_jsonl(&path).unwrap();
        assert_eq!(back.len(), 2, "append must append, not overwrite");
        assert_eq!(back[0], t);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trajectory_does_not_carry_the_workbook_inline() {
        // The size discipline, asserted rather than hoped for: what makes a
        // dataset of thousands of variants a file somebody can open.
        let (_, _, t) = solved();
        let line = serde_json::to_string(&t).unwrap();
        assert!(
            !line.contains("\"cells\":{"),
            "a workbook leaked into the record"
        );
        assert!(line.contains(t.initial_snapshot.as_str()));
        assert!(line.contains(t.final_snapshot.as_str()));
    }
}
