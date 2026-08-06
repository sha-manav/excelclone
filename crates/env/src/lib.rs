//! A deterministic environment around the spreadsheet engine.
//!
//! The engine is already the right shape for this: every state change goes
//! through `Engine::apply(Action) -> Vec<Event>`, and replaying a log from an
//! empty workbook reproduces the exact final state. What was missing is the
//! four things a training loop needs from an environment, and nothing else:
//!
//! * `reset(snapshot_id)` — put the world back exactly as it was.
//! * `observe()` — say what is there, in a summary small enough to send.
//! * `step(action)` — take one action, report what it did.
//! * `grade(task)` — decide whether the task was accomplished.
//!
//! Two design rules run through the whole crate.
//!
//! **The environment never guesses on the policy's behalf.** `step` takes an
//! `engine::Action` — the same type the UI dispatches — not a natural-language
//! request. Anything that has to turn intent into actions lives above this
//! layer, where it can be evaluated separately from the layer that decides
//! whether it worked.
//!
//! **The grader reads the workbook, not the observation.** If `observe`
//! summarizes something wrongly, the policy is misled and scores worse; the
//! score itself stays correct. Any other arrangement lets a summarization bug
//! silently inflate results.

pub mod observe;
pub mod snapshot;
pub mod task;

use engine::{Action, CellAddr, Engine, Event, RangeAddr};
use serde::{Deserialize, Serialize};

pub use observe::{observe as observe_workbook, WorkbookObservation};
pub use snapshot::{canonical, SnapshotId, SnapshotStore};
pub use task::{grade as grade_workbook, Check, GradeResult, TaskSpec};

/// Everything that can go wrong down here. Deliberately small: an action the
/// engine rejects is *not* an error at this level — a policy proposing an
/// illegal edit is ordinary, and the loop needs to see it as a failed step it
/// can learn from rather than as an exception that ends the episode.
#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("no snapshot with id {0}")]
    UnknownSnapshot(SnapshotId),
    #[error("the environment has not been reset to a snapshot yet")]
    NotStarted,
    #[error("no sheet named {0}")]
    UnknownSheet(String),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// How a step ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    /// 1-based index of this step within the episode.
    pub step: u32,
    /// Whether the engine accepted the action. A rejected action leaves the
    /// workbook untouched — `state_hash` is unchanged from the previous step,
    /// which is how a replay proves it really was a no-op.
    pub applied: bool,
    /// The engine's complaint, when it refused.
    pub error: Option<String>,
    pub events: Vec<Event>,
    /// Cells this step touched, `Sheet!A1`, in reading order. Includes cells
    /// that only changed by recalculation: a formula edit three columns away
    /// is still a change to this cell, and a policy that cannot see that
    /// cannot tell an intended effect from a side one.
    pub changed: Vec<String>,
    /// Real number of cells touched, which may exceed `changed.len()`.
    pub changed_total: u32,
    pub state_hash: String,
    /// True once the episode has used its whole step budget.
    pub budget_exhausted: bool,
}

/// The environment: one workbook, reset from a snapshot, stepped by actions.
///
/// Not `Clone`, and that is on purpose. Branching an episode should go
/// through the snapshot store — `put` the current workbook, `load` it into a
/// second `Env` — so that a branch is a thing with an id that can be recorded
/// in a trajectory, rather than an anonymous copy in somebody's memory.
pub struct Env {
    store: SnapshotStore,
    engine: Option<Engine>,
    /// The state `reset` produced, kept for the checks that ask what changed.
    initial: Option<Engine>,
    initial_id: Option<SnapshotId>,
    active_sheet: String,
    selection: String,
    steps: u32,
    max_steps: u32,
    /// What the last step touched, which is what the next observation reports.
    last_changed: Vec<(String, CellAddr)>,
    /// Cached hash of the current workbook, invalidated by every applied step.
    hash_cache: Option<String>,
}

/// The default step budget, matching `TaskSpec::max_steps`.
const DEFAULT_MAX_STEPS: u32 = 64;

/// How many cells one step will name individually before it summarises.
const STEP_CHANGE_BUDGET: usize = 256;

impl Env {
    pub fn new(store: SnapshotStore) -> Self {
        Env {
            store,
            engine: None,
            initial: None,
            initial_id: None,
            active_sheet: String::new(),
            selection: "A1".to_string(),
            steps: 0,
            max_steps: DEFAULT_MAX_STEPS,
            last_changed: Vec::new(),
            hash_cache: None,
        }
    }

    /// An environment that keeps its snapshots only in memory. What tests and
    /// short-lived generation runs want.
    pub fn in_memory() -> Self {
        Env::new(SnapshotStore::in_memory())
    }

    pub fn store(&self) -> &SnapshotStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut SnapshotStore {
        &mut self.store
    }

    /// Put the world back to `id`, discarding whatever was there.
    ///
    /// Every episode starts here, and two episodes reset to the same id are
    /// bit-identical: `SnapshotStore::load` recalculates on the way in, so a
    /// stale cached value in a snapshot file cannot leak into a run.
    pub fn reset(&mut self, id: &SnapshotId) -> Result<(), EnvError> {
        let engine = self.store.load(id)?;
        self.active_sheet = engine
            .wb
            .sheets
            .first()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        self.initial = Some(engine.clone());
        self.engine = Some(engine);
        self.initial_id = Some(id.clone());
        self.selection = "A1".to_string();
        self.steps = 0;
        self.max_steps = DEFAULT_MAX_STEPS;
        self.last_changed.clear();
        self.hash_cache = None;
        Ok(())
    }

    /// Reset to a task's starting state, adopting its sheet and step budget.
    pub fn reset_for(&mut self, task: &TaskSpec) -> Result<(), EnvError> {
        self.reset(&task.initial_snapshot)?;
        if let Some(sheet) = &task.start_sheet {
            self.set_active_sheet(sheet)?;
        }
        self.max_steps = task.max_steps;
        Ok(())
    }

    /// Store the current workbook and return its id, so a branch point or a
    /// generated variant can be referred to by content rather than carried.
    pub fn checkpoint(&mut self) -> Result<SnapshotId, EnvError> {
        let wb = self.engine.as_ref().ok_or(EnvError::NotStarted)?.wb.clone();
        self.store.put(&wb)
    }

    pub fn snapshot_id(&self) -> Option<&SnapshotId> {
        self.initial_id.as_ref()
    }

    pub fn engine(&self) -> Result<&Engine, EnvError> {
        self.engine.as_ref().ok_or(EnvError::NotStarted)
    }

    pub fn initial(&self) -> Result<&Engine, EnvError> {
        self.initial.as_ref().ok_or(EnvError::NotStarted)
    }

    pub fn steps_taken(&self) -> u32 {
        self.steps
    }

    pub fn max_steps(&self) -> u32 {
        self.max_steps
    }

    pub fn active_sheet(&self) -> &str {
        &self.active_sheet
    }

    pub fn selection(&self) -> &str {
        &self.selection
    }

    /// Move the cursor. Selection is environment state rather than workbook
    /// state — it is not in the snapshot and does not affect the hash — but a
    /// policy needs somewhere to put "the range I am working on", and a
    /// trajectory that omits it is unreadable afterwards.
    pub fn set_selection(&mut self, a1: &str) {
        self.selection = a1.to_string();
    }

    pub fn set_active_sheet(&mut self, name: &str) -> Result<(), EnvError> {
        let engine = self.engine.as_ref().ok_or(EnvError::NotStarted)?;
        if !engine.wb.sheets.iter().any(|s| s.name == name) {
            return Err(EnvError::UnknownSheet(name.to_string()));
        }
        self.active_sheet = name.to_string();
        Ok(())
    }

    /// The hash of the current workbook. Computed once per state and cached:
    /// it is a full canonical serialization, which is affordable once a step
    /// but not three times.
    pub fn state_hash(&mut self) -> Result<String, EnvError> {
        if let Some(h) = &self.hash_cache {
            return Ok(h.clone());
        }
        let engine = self.engine.as_ref().ok_or(EnvError::NotStarted)?;
        let (id, _) = canonical(&engine.wb)?;
        self.hash_cache = Some(id.0.clone());
        Ok(id.0)
    }

    /// What the policy sees.
    pub fn observe(&mut self) -> Result<WorkbookObservation, EnvError> {
        let hash = self.state_hash()?;
        let engine = self.engine.as_ref().ok_or(EnvError::NotStarted)?;
        Ok(observe::observe(
            engine,
            &self.active_sheet,
            &self.selection,
            &self.last_changed,
            hash,
        ))
    }

    /// Take one action.
    ///
    /// A rejected action still counts as a step. It has to: a policy that
    /// could propose illegal edits for free would be scored as if flailing
    /// were costless, and "how many steps did this take" would stop being
    /// comparable between policies.
    pub fn step(&mut self, action: &Action) -> Result<StepResult, EnvError> {
        let engine = self.engine.as_mut().ok_or(EnvError::NotStarted)?;
        self.steps += 1;
        let outcome = engine.apply(action);
        let (applied, error, events) = match outcome {
            Ok(events) => (true, None, events),
            Err(e) => (false, Some(e.to_string()), Vec::new()),
        };
        if applied {
            self.hash_cache = None;
            // A sheet the action just deleted cannot stay the active one.
            if !engine.wb.sheets.iter().any(|s| s.name == self.active_sheet) {
                self.active_sheet = engine
                    .wb
                    .sheets
                    .first()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
            }
        }
        let touched = cells_touched(&events);
        self.last_changed = touched.clone();
        let changed_total = touched.len() as u32;
        let changed: Vec<String> = touched
            .iter()
            .take(STEP_CHANGE_BUDGET)
            .map(|(sheet, addr)| format!("{sheet}!{}", addr.to_a1()))
            .collect();
        let state_hash = self.state_hash()?;
        Ok(StepResult {
            step: self.steps,
            applied,
            error,
            events,
            changed,
            changed_total,
            state_hash,
            budget_exhausted: self.steps >= self.max_steps,
        })
    }

    /// Score the current workbook against a task.
    ///
    /// Reads the workbook directly and never consults an observation — see
    /// the module comment for why that separation is load-bearing.
    pub fn grade(&self, task: &TaskSpec) -> Result<GradeResult, EnvError> {
        let engine = self.engine.as_ref().ok_or(EnvError::NotStarted)?;
        let initial = self.initial.as_ref().ok_or(EnvError::NotStarted)?;
        Ok(task::grade(task, engine, initial))
    }

    /// Every cell that differs from the starting state. The expensive,
    /// exact answer, for grading and for a trajectory's final diff — the
    /// per-step `changed` list is the cheap, event-derived one.
    pub fn diff_from_start(&self) -> Result<Vec<(String, CellAddr)>, EnvError> {
        let engine = self.engine.as_ref().ok_or(EnvError::NotStarted)?;
        let initial = self.initial.as_ref().ok_or(EnvError::NotStarted)?;
        Ok(task::changed_cells(initial, engine))
    }
}

/// The cells an event list touched, deduplicated and in reading order.
///
/// Derived from events rather than by diffing two workbooks, because a diff
/// costs a full walk of every sheet on every step. The events are the
/// engine's own account of what it did, so this is not an approximation for
/// the cases it covers — but the range-shaped ones (a sort, a row insert)
/// report the *region* affected rather than each cell, and enumerating a
/// whole region would be worse than useless. Those are reported as their
/// region's cells, capped; `Env::diff_from_start` is the exact answer when an
/// exact answer is needed.
fn cells_touched(events: &[Event]) -> Vec<(String, CellAddr)> {
    /// A single event will not name more cells than this. A sort over ten
    /// thousand rows means "the table moved", not ten thousand facts.
    const PER_EVENT_CAP: usize = 4_096;

    let mut out: Vec<(String, CellAddr)> = Vec::new();
    let push_range = |sheet: &str, range: &RangeAddr, out: &mut Vec<(String, CellAddr)>| {
        let mut n = 0usize;
        for row in range.start.row..=range.end.row {
            for col in range.start.col..=range.end.col {
                if n >= PER_EVENT_CAP {
                    return;
                }
                out.push((sheet.to_string(), CellAddr { row, col }));
                n += 1;
            }
        }
    };

    for event in events {
        match event {
            Event::CellEdited { sheet, addr, .. } | Event::CellCleared { sheet, addr, .. } => {
                out.push((sheet.clone(), *addr));
            }
            Event::RangeCleared { sheet, range, .. }
            | Event::SortApplied { sheet, range, .. }
            | Event::MergeApplied { sheet, range }
            | Event::MergeCleared { sheet, range }
            | Event::FormatApplied { sheet, range, .. }
            | Event::FormatCleared { sheet, range, .. }
            | Event::CondAdded { sheet, range, .. }
            | Event::CondCleared { sheet, range, .. } => push_range(sheet, range, &mut out),
            Event::FillApplied { sheet, target, .. } => push_range(sheet, target, &mut out),
            Event::Recalced { cells } => {
                out.extend(cells.iter().cloned());
            }
            // Structural, sheet-level and workbook-level events name no
            // cells. Their effect shows up in the `Recalced` list that
            // follows them, and in the state hash.
            _ => {}
        }
    }

    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Value;

    fn seeded() -> (Env, SnapshotId) {
        let mut base = Engine::new();
        for (a1, input) in [("A1", "Amount"), ("A2", "10"), ("A3", "20")] {
            base.apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1(a1).unwrap(),
                input: input.into(),
            })
            .unwrap();
        }
        let mut env = Env::in_memory();
        let id = env.store_mut().put(&base.wb).unwrap();
        env.reset(&id).unwrap();
        (env, id)
    }

    fn edit(a1: &str, input: &str) -> Action {
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        }
    }

    #[test]
    fn using_the_environment_before_reset_is_an_error_not_an_empty_workbook() {
        let mut env = Env::in_memory();
        assert!(matches!(env.observe(), Err(EnvError::NotStarted)));
        assert!(matches!(
            env.step(&edit("A1", "1")),
            Err(EnvError::NotStarted)
        ));
    }

    #[test]
    fn reset_returns_to_exactly_the_same_state() {
        // The whole training loop rests on this. If two episodes from the
        // same id could differ, no score computed across them would mean
        // anything.
        let (mut env, id) = seeded();
        let before = env.state_hash().unwrap();
        env.step(&edit("A4", "=SUM(A2:A3)")).unwrap();
        let after = env.state_hash().unwrap();
        assert_ne!(before, after);
        env.reset(&id).unwrap();
        assert_eq!(env.state_hash().unwrap(), before);
        assert_eq!(env.steps_taken(), 0);
    }

    #[test]
    fn the_same_actions_from_the_same_snapshot_reach_the_same_hash() {
        let (mut a, id) = seeded();
        // A second environment with its own store, seeded from the same
        // workbook — so this tests the snapshot round-trip too, not just two
        // handles onto one cached blob.
        let wb = a.store().load(&id).unwrap().wb;
        let mut b = Env::in_memory();
        let b_id = b.store_mut().put(&wb).unwrap();
        assert_eq!(
            b_id, id,
            "the same workbook must get the same id in any store"
        );
        b.reset(&b_id).unwrap();
        for action in [
            edit("B1", "Tax"),
            edit("B2", "=A2*0.2"),
            edit("B3", "=A3*0.2"),
        ] {
            a.step(&action).unwrap();
            b.step(&action).unwrap();
        }
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    #[test]
    fn a_rejected_action_leaves_the_state_alone_but_still_costs_a_step() {
        let (mut env, _) = seeded();
        let before = env.state_hash().unwrap();
        let result = env
            .step(&Action::CellEdit {
                sheet: "NoSuchSheet".into(),
                addr: CellAddr::parse_a1("A1").unwrap(),
                input: "1".into(),
            })
            .unwrap();
        assert!(!result.applied);
        assert!(result.error.is_some(), "a refusal must say why");
        assert_eq!(result.state_hash, before, "a refused action changed state");
        assert_eq!(env.steps_taken(), 1, "flailing has to cost something");
    }

    #[test]
    fn a_step_reports_the_cells_recalculation_touched_not_just_the_one_edited() {
        // The trap: reporting only the edited cell makes a policy blind to
        // the blast radius of its own edit, which is exactly what the
        // validator upstream needs to reason about.
        let (mut env, _) = seeded();
        env.step(&edit("B2", "=A2*2")).unwrap();
        let result = env.step(&edit("A2", "11")).unwrap();
        assert!(result.changed.contains(&"Sheet1!A2".to_string()));
        assert!(
            result.changed.contains(&"Sheet1!B2".to_string()),
            "the dependent cell changed too: {:?}",
            result.changed
        );
    }

    #[test]
    fn an_observation_describes_the_state_it_claims_to() {
        let (mut env, _) = seeded();
        env.step(&edit("A4", "=SUM(A2:A3)")).unwrap();
        let hash = env.state_hash().unwrap();
        let obs = env.observe().unwrap();
        assert_eq!(obs.state_hash, hash);
        assert_eq!(obs.active_sheet, "Sheet1");
        assert!(obs.recent_changes.cells.contains(&"Sheet1!A4".to_string()));
    }

    #[test]
    fn selection_is_not_part_of_the_state_hash() {
        // Two runs that scrolled differently must not be different states,
        // or snapshot deduplication stops working.
        let (mut env, _) = seeded();
        let before = env.state_hash().unwrap();
        env.set_selection("Z99");
        assert_eq!(env.state_hash().unwrap(), before);
    }

    #[test]
    fn deleting_the_active_sheet_moves_the_cursor_somewhere_real() {
        let (mut env, _) = seeded();
        env.step(&Action::SheetAdd { name: "Two".into() }).unwrap();
        env.set_active_sheet("Two").unwrap();
        env.step(&Action::SheetDelete { name: "Two".into() })
            .unwrap();
        assert_eq!(env.active_sheet(), "Sheet1");
    }

    #[test]
    fn a_checkpoint_can_be_reset_to() {
        let (mut env, _) = seeded();
        env.step(&edit("A4", "=SUM(A2:A3)")).unwrap();
        let mid = env.checkpoint().unwrap();
        env.step(&edit("A2", "999")).unwrap();
        env.reset(&mid).unwrap();
        assert_eq!(
            env.engine().unwrap().value_at("Sheet1", "A2"),
            Value::Number(10.0)
        );
        assert_eq!(
            env.engine().unwrap().value_at("Sheet1", "A4"),
            Value::Number(30.0)
        );
    }
}
