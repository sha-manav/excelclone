//! What the user did instead.
//!
//! The most valuable signal a working system produces is not its successes —
//! those are cheap to generate. It is the moment somebody undoes what the
//! agent did and does it differently. That moment contains the instruction,
//! the exact state the agent was in when it went wrong, what it did there,
//! and what should have been done instead. Nothing generated can substitute
//! for it.
//!
//! So a `Correction` pairs two trajectories over the same starting state and
//! locates the point where they part company. From that pair, two kinds of
//! training data fall out:
//!
//! * **A supervised example** — the repair, when the repair passes the
//!   grader. The instruction and what a competent person actually did.
//! * **A preference pair** — the agent's attempt as the rejected side and
//!   the repair as the preferred one, anchored at the state where they first
//!   differed. Which is where the difference is *about* something.
//!
//! Two things this file is careful about.
//!
//! **A repair is only a correction if it worked.** A user who undid the
//! agent and then did something equally wrong is not a teacher. The grader
//! decides, and an ungraded correction produces no supervised example.
//!
//! **The divergence is computed from state hashes, not from the actions.**
//! Two different action sequences can reach the same workbook, and the point
//! worth learning from is the first place the *states* differ — not the
//! first place the keystrokes did.

use serde::{Deserialize, Serialize};

use crate::snapshot::SnapshotId;
use crate::task::GradeResult;
use crate::trajectory::{CellDiff, Trajectory};
use crate::EnvError;

/// How the user said the agent was wrong.
///
/// Kept apart because they mean different things about how far the agent got
/// and how much of its work survived. A preview edited before it ran cost
/// nobody anything; a final result rejected after the fact cost the user the
/// time to find out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum Signal {
    /// The user undid the agent's work.
    Undone { steps: u32 },
    /// The user changed the proposal before letting it run. The cheapest
    /// correction there is, and the most informative: it says what was wrong
    /// with the *plan* rather than with the result.
    PreviewEdited,
    /// The user fixed cells the agent had already written.
    OutputRepaired,
    /// The user threw the result away without repairing it.
    Rejected,
}

impl Signal {
    /// Whether the agent's work reached the workbook before being corrected.
    pub fn reached_the_sheet(&self) -> bool {
        !matches!(self, Signal::PreviewEdited)
    }
}

/// Where two runs from the same start stop agreeing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    /// How many steps both runs were in the same state for. Zero means they
    /// differed from the first action.
    pub agreed_through: u32,
    /// The state they were both in at that point.
    pub common_state: String,
    /// What the agent did next, if it did anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<engine::Action>,
    /// What the user did next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_did: Option<engine::Action>,
    /// True when the repair started from the agent's *result* rather than
    /// from the same starting state — the user fixed the output rather than
    /// redoing the work. The agent's whole run then counts as agreed, which
    /// is accurate and worth flagging rather than inferring from the number.
    pub repaired_on_top: bool,
    /// Cells whose final value or formula differs between the two runs.
    /// Capped; `cells_total` is the real figure.
    pub cells: Vec<CellDiff>,
    pub cells_total: u32,
}

/// How many differing cells a divergence will name.
const DIVERGENCE_BUDGET: usize = 128;

/// One episode the agent got wrong and a person put right.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    pub id: String,
    pub signal: Signal,
    pub instruction: String,
    pub initial_snapshot: SnapshotId,
    /// What the agent did.
    pub attempt: Trajectory,
    /// What the user did instead.
    pub repair: Trajectory,
    /// Where the agent ended up, by hash.
    pub attempted_snapshot: SnapshotId,
    /// Where the user ended up.
    pub corrected_snapshot: SnapshotId,
    pub divergence: Divergence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade_attempted: Option<GradeResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade_corrected: Option<GradeResult>,
}

impl Correction {
    /// Build one from the two trajectories.
    ///
    /// `repair` may start from the same snapshot as `attempt` (the user
    /// redid the work) or from the attempt's final state (the user fixed the
    /// output). Anything else is not a correction of this attempt, and
    /// saying so is better than producing a divergence computed against an
    /// unrelated run.
    pub fn from(
        id: impl Into<String>,
        signal: Signal,
        attempt: Trajectory,
        repair: Trajectory,
    ) -> Result<Correction, EnvError> {
        let on_top = repair.initial_snapshot == attempt.final_snapshot;
        let same_start = repair.initial_snapshot == attempt.initial_snapshot;
        if !on_top && !same_start {
            return Err(EnvError::Perturbation(format!(
                "the repair starts from {} which is neither the attempt's start ({}) nor its result ({})",
                repair.initial_snapshot, attempt.initial_snapshot, attempt.final_snapshot
            )));
        }

        let divergence = if on_top {
            Divergence {
                agreed_through: attempt.steps.len() as u32,
                common_state: attempt.final_snapshot.0.clone(),
                agent_did: None,
                user_did: repair.steps.first().map(|s| s.action.clone()),
                repaired_on_top: true,
                cells: repair
                    .diff
                    .cells
                    .iter()
                    .take(DIVERGENCE_BUDGET)
                    .cloned()
                    .collect(),
                cells_total: repair.diff.total,
            }
        } else {
            let agreed = attempt
                .steps
                .iter()
                .zip(&repair.steps)
                .take_while(|(a, b)| a.state_hash == b.state_hash)
                .count();
            Divergence {
                agreed_through: agreed as u32,
                common_state: match agreed {
                    0 => attempt.initial_snapshot.0.clone(),
                    n => attempt.steps[n - 1].state_hash.clone(),
                },
                agent_did: attempt.steps.get(agreed).map(|s| s.action.clone()),
                user_did: repair.steps.get(agreed).map(|s| s.action.clone()),
                repaired_on_top: false,
                cells: final_difference(&attempt, &repair),
                cells_total: attempt.diff.total.saturating_add(repair.diff.total),
            }
        };

        Ok(Correction {
            id: id.into(),
            signal,
            instruction: repair.instruction.clone(),
            initial_snapshot: attempt.initial_snapshot.clone(),
            attempted_snapshot: attempt.final_snapshot.clone(),
            corrected_snapshot: repair.final_snapshot.clone(),
            grade_attempted: attempt.grade.clone(),
            grade_corrected: repair.grade.clone(),
            divergence,
            attempt,
            repair,
        })
    }

    /// Whether the repair actually repaired anything.
    ///
    /// A user who undid the agent and then did something equally wrong is
    /// not a teacher, and training on it would be worse than ignoring the
    /// episode. Ungraded is not clean either — nothing has confirmed it.
    pub fn is_clean(&self) -> bool {
        self.grade_corrected.as_ref().is_some_and(|g| g.passed)
    }

    /// The repair, as something to imitate.
    pub fn supervised(&self) -> Option<SupervisedExample> {
        if !self.is_clean() {
            return None;
        }
        Some(SupervisedExample {
            id: format!("{}::supervised", self.id),
            instruction: self.instruction.clone(),
            initial_snapshot: self.repair.initial_snapshot.clone(),
            observation: self
                .repair
                .steps
                .first()
                .and_then(|s| s.observation.clone()),
            actions: self.repair.actions().cloned().collect(),
            final_snapshot: self.corrected_snapshot.clone(),
            origin: format!("correction:{}", signal_name(&self.signal)),
        })
    }

    /// The pair, as something to prefer between.
    ///
    /// Anchored at the diverging state rather than at the start: the two
    /// sides agreed up to that point, and a preference over a shared prefix
    /// is a preference about nothing.
    pub fn preference(&self) -> Option<PreferencePair> {
        if !self.is_clean() {
            return None;
        }
        // A correction with no attempt to compare against has no preference
        // in it — it is a demonstration, and `supervised` already has it.
        if self.attempt.steps.is_empty() {
            return None;
        }
        let at = self.divergence.agreed_through as usize;
        let rejected: Vec<engine::Action> = self.attempt.steps[at.min(self.attempt.steps.len())..]
            .iter()
            .map(|s| s.action.clone())
            .collect();
        let preferred: Vec<engine::Action> = if self.divergence.repaired_on_top {
            self.repair.actions().cloned().collect()
        } else {
            self.repair.steps[at.min(self.repair.steps.len())..]
                .iter()
                .map(|s| s.action.clone())
                .collect()
        };
        if rejected.is_empty() && preferred.is_empty() {
            return None;
        }
        Some(PreferencePair {
            id: format!("{}::preference", self.id),
            instruction: self.instruction.clone(),
            // The state both were in when they still agreed.
            from_state: self.divergence.common_state.clone(),
            rejected,
            preferred,
            why: describe(self),
        })
    }
}

fn signal_name(signal: &Signal) -> &'static str {
    match signal {
        Signal::Undone { .. } => "undone",
        Signal::PreviewEdited => "preview-edited",
        Signal::OutputRepaired => "output-repaired",
        Signal::Rejected => "rejected",
    }
}

/// Why the preferred side is preferred, from the graders rather than from an
/// opinion. A preference pair with no stated reason cannot be audited.
fn describe(correction: &Correction) -> String {
    match (&correction.grade_attempted, &correction.grade_corrected) {
        (Some(before), Some(after)) if !before.passed && after.passed => {
            let failed: Vec<String> = before.failures().map(|c| c.detail.clone()).collect();
            format!("the attempt failed: {}", failed.join("; "))
        }
        (Some(before), Some(_)) if before.incidental_changes > 0 => format!(
            "the attempt changed {} cell(s) nobody asked about",
            before.incidental_changes
        ),
        _ => format!("the user {} it", signal_name(&correction.signal)),
    }
}

/// Cells where the two runs' final workbooks differ.
///
/// Built from the two recorded diffs rather than by loading both workbooks:
/// a correction is a record, and a record that needed the snapshot store to
/// be readable to mean anything would be useless in a dataset shipped
/// without one.
fn final_difference(attempt: &Trajectory, repair: &Trajectory) -> Vec<CellDiff> {
    let mut out: Vec<CellDiff> = Vec::new();
    for cell in &repair.diff.cells {
        let agent_left = attempt
            .diff
            .cells
            .iter()
            .find(|c| c.at == cell.at)
            .map(|c| (c.after.clone(), c.after_formula.clone()))
            // The agent did not touch it, so it still holds what it started
            // with — which the repair's own `before` records.
            .unwrap_or_else(|| (cell.before.clone(), cell.before_formula.clone()));
        if agent_left.0 == cell.after && agent_left.1 == cell.after_formula {
            continue;
        }
        out.push(CellDiff {
            at: cell.at.clone(),
            before: agent_left.0,
            before_formula: agent_left.1,
            after: cell.after.clone(),
            after_formula: cell.after_formula.clone(),
        });
        if out.len() >= DIVERGENCE_BUDGET {
            break;
        }
    }
    // Cells the agent touched and the repair did not: the agent's leftovers.
    for cell in &attempt.diff.cells {
        if out.len() >= DIVERGENCE_BUDGET {
            break;
        }
        if repair.diff.cells.iter().any(|c| c.at == cell.at) {
            continue;
        }
        out.push(CellDiff {
            at: cell.at.clone(),
            before: cell.after.clone(),
            before_formula: cell.after_formula.clone(),
            after: cell.before.clone(),
            after_formula: cell.before_formula.clone(),
        });
    }
    out.sort_by(|a, b| a.at.cmp(&b.at));
    out
}

/// One thing to imitate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisedExample {
    pub id: String,
    pub instruction: String,
    pub initial_snapshot: SnapshotId,
    /// What the person could see when they started, when it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<crate::observe::WorkbookObservation>,
    pub actions: Vec<engine::Action>,
    pub final_snapshot: SnapshotId,
    pub origin: String,
}

/// One thing to prefer over another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreferencePair {
    pub id: String,
    pub instruction: String,
    /// The state both sides were in when they still agreed.
    pub from_state: String,
    pub rejected: Vec<engine::Action>,
    pub preferred: Vec<engine::Action>,
    /// Why, from the graders. A preference with no stated reason cannot be
    /// audited, and an unauditable preference is how a dataset acquires
    /// somebody's bad afternoon as a training signal.
    pub why: String,
}

/// Corrections on disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CorrectionLog {
    pub corrections: Vec<Correction>,
}

impl CorrectionLog {
    pub fn new() -> Self {
        CorrectionLog::default()
    }

    pub fn push(&mut self, correction: Correction) {
        self.corrections.push(correction);
    }

    /// Every clean correction, as supervised examples.
    pub fn supervised(&self) -> Vec<SupervisedExample> {
        self.corrections
            .iter()
            .filter_map(|c| c.supervised())
            .collect()
    }

    /// Every clean correction that had something to compare against.
    pub fn preferences(&self) -> Vec<PreferencePair> {
        self.corrections
            .iter()
            .filter_map(|c| c.preference())
            .collect()
    }

    /// How many corrections were not usable, and why — reported rather than
    /// silently dropped, because a capture pipeline that quietly discards
    /// most of what it sees looks the same as one that is working.
    pub fn unusable(&self) -> Vec<(String, String)> {
        self.corrections
            .iter()
            .filter(|c| !c.is_clean())
            .map(|c| {
                let why = match &c.grade_corrected {
                    None => "the repair was never graded".to_string(),
                    Some(g) => format!(
                        "the repair did not pass either: {}",
                        g.failures()
                            .map(|f| f.detail.clone())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                };
                (c.id.clone(), why)
            })
            .collect()
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), EnvError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let mut text = String::new();
        for c in &self.corrections {
            text.push_str(&serde_json::to_string(c)?);
            text.push('\n');
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn load(path: &std::path::Path) -> Result<Self, EnvError> {
        let text = std::fs::read_to_string(path)?;
        let mut corrections = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            corrections.push(serde_json::from_str(line)?);
        }
        Ok(CorrectionLog { corrections })
    }
}

/// Write a list of records as JSONL.
pub fn write_jsonl<T: Serialize>(path: &std::path::Path, items: &[T]) -> Result<(), EnvError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut text = String::new();
    for item in items {
        text.push_str(&serde_json::to_string(item)?);
        text.push('\n');
    }
    std::fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{Check, TaskSpec};
    use crate::trajectory::{ObservationPolicy, Recorder, Source, Termination};
    use crate::{Env, SnapshotStore};
    use engine::{Action, CellAddr, Engine};

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
        let id = store.put(&ledger().wb).unwrap();
        (
            Env::new(store),
            TaskSpec {
                id: "totals".into(),
                instruction: "Fill the Total column with Qty times Price.".into(),
                initial_snapshot: id,
                checks: vec![
                    Check::RangeFilled {
                        range: "D2:D3".into(),
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

    /// Record a run of `actions` against the task, from the task's start.
    fn record(
        env: Env,
        task: &TaskSpec,
        id: &str,
        actions: &[Action],
        who: Source,
    ) -> (Trajectory, Env) {
        let mut rec = Recorder::start_task(env, id, task, who)
            .unwrap()
            .with_observations(ObservationPolicy::First);
        for a in actions {
            rec.step(a).unwrap();
        }
        rec.finish_with_env(Termination::Done, Some(task)).unwrap()
    }

    /// The agent pastes literals; the user replaces them with formulas.
    fn literals_then_formulas() -> Correction {
        let (env, task) = setup();
        let (attempt, env) = record(
            env,
            &task,
            "agent",
            &[edit("Sheet1", "D2", "10"), edit("Sheet1", "D3", "4.5")],
            Source::Policy {
                name: "test".into(),
            },
        );
        let (repair, _) = record(
            env,
            &task,
            "human",
            &[
                edit("Sheet1", "D2", "=B2*C2"),
                edit("Sheet1", "D3", "=B3*C3"),
            ],
            Source::Human,
        );
        Correction::from("c1", Signal::Undone { steps: 2 }, attempt, repair).unwrap()
    }

    #[test]
    fn a_correction_finds_where_the_two_runs_stopped_agreeing() {
        let c = literals_then_formulas();
        assert_eq!(
            c.divergence.agreed_through, 0,
            "they differed from the first action"
        );
        assert_eq!(c.divergence.common_state, c.initial_snapshot.0);
        assert_eq!(c.divergence.agent_did, Some(edit("Sheet1", "D2", "10")));
        assert_eq!(c.divergence.user_did, Some(edit("Sheet1", "D2", "=B2*C2")));
    }

    #[test]
    fn a_shared_prefix_is_not_a_divergence() {
        // The two runs did the same thing for two steps and then differed.
        // Anchoring the preference at the start would make it a preference
        // about the part they agreed on.
        let (env, task) = setup();
        let common = [edit("Sheet1", "D2", "=B2*C2")];
        let (attempt, env) = record(
            env,
            &task,
            "agent",
            &[common[0].clone(), edit("Sheet1", "D3", "4.5")],
            Source::Policy {
                name: "test".into(),
            },
        );
        let (repair, _) = record(
            env,
            &task,
            "human",
            &[common[0].clone(), edit("Sheet1", "D3", "=B3*C3")],
            Source::Human,
        );
        let c = Correction::from("c", Signal::OutputRepaired, attempt, repair).unwrap();
        assert_eq!(c.divergence.agreed_through, 1);
        assert_eq!(c.divergence.agent_did, Some(edit("Sheet1", "D3", "4.5")));

        let pref = c.preference().unwrap();
        assert_eq!(pref.rejected.len(), 1, "only the part they differed on");
        assert_eq!(pref.preferred.len(), 1);
        assert_eq!(pref.from_state, c.divergence.common_state);
    }

    #[test]
    fn the_divergence_names_the_cells_that_ended_up_different() {
        let c = literals_then_formulas();
        let d2 = c
            .divergence
            .cells
            .iter()
            .find(|x| x.at == "Sheet1!D2")
            .unwrap();
        assert_eq!(d2.before, "10", "what the agent left");
        assert_eq!(d2.after, "10", "what the user left, by value");
        assert_eq!(d2.before_formula, None, "the agent pasted a literal");
        assert_eq!(
            d2.after_formula.as_deref(),
            Some("=B2*C2"),
            "which is the whole difference and it is invisible in the value"
        );
    }

    #[test]
    fn a_clean_correction_becomes_a_supervised_example() {
        let c = literals_then_formulas();
        assert!(c.is_clean(), "the repair should pass the grader");
        let example = c.supervised().unwrap();
        assert_eq!(example.instruction, c.instruction);
        assert_eq!(example.actions.len(), 2);
        assert!(example.actions.iter().all(|a| matches!(
            a,
            Action::CellEdit { input, .. } if input.starts_with('=')
        )));
        assert!(example.origin.contains("undone"));
    }

    #[test]
    fn a_repair_that_is_also_wrong_teaches_nothing() {
        // A user who undid the agent and then did something equally wrong is
        // not a teacher. Training on it would be worse than ignoring it.
        let (env, task) = setup();
        let (attempt, env) = record(
            env,
            &task,
            "agent",
            &[edit("Sheet1", "D2", "10")],
            Source::Policy {
                name: "test".into(),
            },
        );
        let (repair, _) = record(
            env,
            &task,
            "human",
            &[edit("Sheet1", "D2", "11")],
            Source::Human,
        );
        let c = Correction::from("c", Signal::Rejected, attempt, repair).unwrap();
        assert!(!c.is_clean());
        assert!(c.supervised().is_none());
        assert!(c.preference().is_none());
    }

    #[test]
    fn a_failed_attempt_and_a_working_repair_become_a_preference_pair() {
        let c = literals_then_formulas();
        let pref = c.preference().unwrap();
        assert_eq!(pref.rejected.len(), 2);
        assert_eq!(pref.preferred.len(), 2);
        assert!(
            pref.why.contains("no formula") || pref.why.contains("failed"),
            "the reason should come from the grader: {}",
            pref.why
        );
    }

    #[test]
    fn a_preference_pair_always_says_why() {
        // An unauditable preference is how a dataset acquires somebody's bad
        // afternoon as a training signal.
        let c = literals_then_formulas();
        assert!(!c.preference().unwrap().why.is_empty());
    }

    #[test]
    fn a_repair_on_top_of_the_agents_output_is_recognised_as_one() {
        // The commonest correction in practice: the user does not undo, they
        // fix what is there. Both runs "agree" through the whole attempt,
        // and saying so is accurate rather than a divergence computed
        // against a run that never happened.
        let (env, task) = setup();
        let (attempt, mut env) = record(
            env,
            &task,
            "agent",
            &[edit("Sheet1", "D2", "10"), edit("Sheet1", "D3", "4.5")],
            Source::Policy {
                name: "test".into(),
            },
        );
        // The user starts from where the agent left off.
        env.reset(&attempt.final_snapshot).unwrap();
        let mut rec = Recorder::start(
            env,
            "human",
            task.instruction.clone(),
            &attempt.final_snapshot,
            Source::Human,
        )
        .unwrap()
        .with_observations(ObservationPolicy::First);
        rec.step(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        rec.step(&edit("Sheet1", "D3", "=B3*C3")).unwrap();
        let (repair, _) = rec.finish_with_env(Termination::Done, Some(&task)).unwrap();

        let c = Correction::from("c", Signal::OutputRepaired, attempt, repair).unwrap();
        assert!(c.divergence.repaired_on_top);
        assert_eq!(c.divergence.agreed_through, 2);
        assert_eq!(c.divergence.agent_did, None, "the agent had finished");
        assert!(c.is_clean());
        assert_eq!(c.preference().unwrap().rejected.len(), 0);
    }

    #[test]
    fn a_repair_of_a_different_episode_is_refused() {
        // Otherwise the divergence is computed against a run that has
        // nothing to do with it, and the resulting preference is noise
        // wearing a schema.
        let (env, task) = setup();
        let (attempt, env) = record(
            env,
            &task,
            "agent",
            &[edit("Sheet1", "D2", "10")],
            Source::Policy {
                name: "test".into(),
            },
        );
        let mut other = task.clone();
        let mut store = SnapshotStore::in_memory();
        let mut different = ledger();
        different.apply(&edit("Sheet1", "A2", "Screw")).unwrap();
        other.initial_snapshot = store.put(&different.wb).unwrap();
        let (repair, _) = record(
            Env::new(store),
            &other,
            "human",
            &[edit("Sheet1", "D2", "=B2*C2")],
            Source::Human,
        );
        let _ = env;
        assert!(Correction::from("c", Signal::Rejected, attempt, repair).is_err());
    }

    #[test]
    fn a_preview_edited_before_it_ran_never_touched_the_sheet() {
        assert!(!Signal::PreviewEdited.reached_the_sheet());
        assert!(Signal::Undone { steps: 1 }.reached_the_sheet());
    }

    #[test]
    fn a_log_reports_what_it_could_not_use_rather_than_dropping_it_quietly() {
        // A capture pipeline that silently discards most of what it sees
        // looks exactly like one that is working.
        let (env, task) = setup();
        let (attempt, env) = record(
            env,
            &task,
            "agent",
            &[edit("Sheet1", "D2", "10")],
            Source::Policy { name: "t".into() },
        );
        let (repair, _) = record(
            env,
            &task,
            "human",
            &[edit("Sheet1", "D2", "11")],
            Source::Human,
        );
        let mut log = CorrectionLog::new();
        log.push(Correction::from("bad", Signal::Rejected, attempt, repair).unwrap());
        log.push(literals_then_formulas());

        assert_eq!(log.supervised().len(), 1);
        assert_eq!(log.preferences().len(), 1);
        let unusable = log.unusable();
        assert_eq!(unusable.len(), 1);
        assert_eq!(unusable[0].0, "bad");
        assert!(unusable[0].1.contains("did not pass"));
    }

    #[test]
    fn a_correction_log_survives_a_round_trip_through_disk() {
        let mut log = CorrectionLog::new();
        log.push(literals_then_formulas());
        let dir = std::env::temp_dir().join(format!("gridline-corrections-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("corrections.jsonl");
        log.save(&path).unwrap();
        assert_eq!(CorrectionLog::load(&path).unwrap(), log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_recorded_attempt_still_replays() {
        // A correction carries two trajectories and they are trajectories
        // like any other: if they stopped replaying they would stop being
        // evidence of anything.
        let (env, task) = setup();
        let (attempt, env) = record(
            env,
            &task,
            "agent",
            &[edit("Sheet1", "D2", "10")],
            Source::Policy { name: "t".into() },
        );
        let store = env.into_store();
        let report = crate::trajectory::replay(store, &attempt, Some(&task)).unwrap();
        assert!(report.faithful);
        assert!(!report.grade.unwrap().passed, "it was a failed attempt");
    }
}
