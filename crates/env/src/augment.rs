//! Turning one validated demonstration into many.
//!
//! A human fills a Total column once. The lesson in that is not "write
//! `=B2*C2` into D2" — it is "find the quantity and price columns, multiply
//! them, and fill to the bottom of the data". A policy trained on the single
//! recording learns the first, and the way to make it learn the second is to
//! show it the same task with the table two rows lower, on a differently
//! named sheet, with an irrelevant column in the middle and three more rows
//! of data.
//!
//! Every perturbation here is expressed as **engine actions plus a matching
//! address remap**, never as a direct edit of the model. Inserting two rows
//! is `Action::RowInsert`, which the engine already knows how to do — every
//! formula in the workbook is rewritten by the same reference-rewriting code
//! that copy, paste and fill go through. Writing a second implementation of
//! "what happens to a reference when a row appears above it" is how the
//! generated data would come to disagree with the product.
//!
//! And then the gate: **a variant is replayed and graded, and kept only if it
//! passes.** The generator is heuristic and is allowed to be — extending a
//! filled column when rows are appended is a guess about what the
//! demonstration meant. The gate is not a formality, it is the thing that
//! makes a guess safe. The rejection list is returned rather than swallowed,
//! because a perturbation that never survives is a bug in the generator and
//! silently producing fewer variants is how nobody notices.

use engine::{Action, Axis, CellAddr, Engine, RangeAddr};
use serde::{Deserialize, Serialize};

use crate::snapshot::SnapshotId;
use crate::task::{Check, TaskSpec};
use crate::trajectory::{Recorder, Source, Termination, Trajectory};
use crate::{Env, EnvError};

/// One structural change to a task's starting workbook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "perturbation", rename_all = "snake_case")]
pub enum Perturbation {
    /// Insert blank rows or columns, moving everything at or after `at`.
    ///
    /// `at: 0` moves the whole table down or right; an `at` inside the table
    /// is the irrelevant-column case. One variant covers both because they
    /// are the same operation, and the engine only has one of them.
    ///
    /// A column insert with no `fill` leaves a blank column, and a blank
    /// column is a *gap*, not a distractor: table detection quite correctly
    /// stops at it, so the variant is a table that got narrower rather than
    /// one with something irrelevant in the middle. That made a generated
    /// "distractor-column" variant impossible for any agent that finds its
    /// own table — and the demonstration replay still passed it, because
    /// fixed addresses do not care where the table ends. Give it a `fill`.
    Insert {
        sheet: String,
        axis: Axis,
        at: u32,
        count: u32,
        /// What to put in the new column, so it is something to ignore
        /// rather than a hole. Column inserts only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fill: Option<InsertFill>,
    },
    /// Rename a sheet. Every reference to it — in the workbook, in the
    /// recorded actions, and in the task's checks — follows.
    RenameSheet { from: String, to: String },
    /// Repeat the last populated row of a sheet `count` more times, so the
    /// data is longer.
    ///
    /// Only literal cells are copied. A formula in the last row belongs to
    /// the demonstration, not the data, and duplicating it would hand the
    /// agent the answer for the new rows.
    ///
    /// Nothing that sat *below* the data is moved, and that is a recorded
    /// limitation rather than an oversight: telling "a footer under the
    /// table" from "an empty cell inside it" is the same guess table
    /// detection makes, and guessing wrong would silently relocate an output.
    /// A demonstration that writes a grand-total row is therefore rejected by
    /// the gate rather than mangled — the outcome to prefer.
    AppendRows { sheet: String, count: u32 },
    /// Multiply every numeric literal in a range by `factor`.
    ///
    /// Formulas are left alone: the point is to change the inputs, not the
    /// method. This changes what the right answer *is*, which is why
    /// `changes_values` exists.
    ScaleLiterals {
        sheet: String,
        range: RangeAddr,
        factor: f64,
    },
}

/// The contents of an inserted column: a header, and one value repeated down
/// the rows the table already occupied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsertFill {
    pub header: String,
    pub value: String,
}

impl Perturbation {
    pub fn label(&self) -> String {
        match self {
            Perturbation::Insert {
                sheet,
                axis,
                at,
                count,
                ..
            } => {
                let what = match axis {
                    Axis::Row => "rows",
                    Axis::Col => "cols",
                };
                format!("insert-{count}-{what}-at-{at}-on-{sheet}")
            }
            Perturbation::RenameSheet { from, to } => format!("rename-{from}-to-{to}"),
            Perturbation::AppendRows { sheet, count } => format!("append-{count}-rows-to-{sheet}"),
            Perturbation::ScaleLiterals {
                sheet,
                range,
                factor,
            } => format!("scale-{sheet}-{}-by-{factor}", range.to_a1()),
        }
    }

    /// Whether this changes what the correct answer is, so the task's
    /// value-based expectations have to be recomputed.
    pub fn changes_values(&self) -> bool {
        matches!(
            self,
            Perturbation::AppendRows { .. } | Perturbation::ScaleLiterals { .. }
        )
    }

    /// Change the starting workbook.
    fn apply(&self, engine: &mut Engine) -> Result<(), EnvError> {
        let actions = match self {
            Perturbation::Insert {
                sheet,
                axis,
                at,
                count,
                fill,
            } => insert_actions(engine, sheet, *axis, *at, *count, fill.as_ref())?,
            Perturbation::RenameSheet { from, to } => vec![Action::SheetRename {
                from: from.clone(),
                to: to.clone(),
            }],
            Perturbation::AppendRows { sheet, count } => repeat_last_row(engine, sheet, *count)?,
            Perturbation::ScaleLiterals {
                sheet,
                range,
                factor,
            } => scale_literals(engine, sheet, *range, *factor)?,
        };
        for action in &actions {
            engine
                .apply(action)
                .map_err(|e| EnvError::Perturbation(format!("{}: {e}", self.label())))?;
        }
        Ok(())
    }

    /// Rewrite a recorded action so it means the same thing on the variant.
    ///
    /// `engine` is the workbook *after* the perturbation, which is what the
    /// sheet-name lookup in a formula rewrite has to resolve against.
    fn remap_action(&self, engine: &Engine, action: &Action) -> Action {
        match self {
            Perturbation::Insert {
                sheet,
                axis,
                at,
                count,
                ..
            } => shift_action(engine, action, sheet, *axis, *at, *count),
            Perturbation::RenameSheet { from, to } => rename_in_action(action, from, to),
            // Neither of these moves anything, so the recorded actions still
            // land where they did. `AppendRows` adds new ones instead.
            Perturbation::AppendRows { .. } | Perturbation::ScaleLiterals { .. } => action.clone(),
        }
    }

    /// Actions the perturbation adds to the demonstration.
    ///
    /// Only `AppendRows` has any: longer data needs the filled columns
    /// continued, and continuing them is a guess about what the
    /// demonstration meant — which is precisely what the grader gate is for.
    fn extend_actions(&self, before: &Engine, actions: &[Action]) -> Vec<Action> {
        let Perturbation::AppendRows { sheet, count } = self else {
            return actions.to_vec();
        };
        let Some(last) = before
            .wb
            .sheet_by_name(sheet)
            .and_then(|s| s.used_range())
            .map(|r| r.end.row)
        else {
            return actions.to_vec();
        };

        let mut out = actions.to_vec();
        // A column the demonstration wrote all the way down to the old last
        // row is a filled column; anything else was a one-off (a grand total,
        // a label) and continuing it would be wrong.
        let mut by_col: std::collections::BTreeMap<(String, u32), Vec<(u32, String)>> =
            std::collections::BTreeMap::new();
        for action in actions {
            if let Action::CellEdit {
                sheet: s,
                addr,
                input,
            } = action
            {
                if s == sheet {
                    by_col
                        .entry((s.clone(), addr.col))
                        .or_default()
                        .push((addr.row, input.clone()));
                }
            }
        }
        for ((s, col), mut rows) in by_col {
            rows.sort();
            let Some((bottom, input)) = rows.last().cloned() else {
                continue;
            };
            if bottom != last || rows.len() < 2 {
                continue;
            }
            // Continue it only if it really is one repeated shape.
            let shape = crate::observe::relative_shape(&input, CellAddr::new(bottom, col));
            if !rows
                .iter()
                .all(|(r, i)| crate::observe::relative_shape(i, CellAddr::new(*r, col)) == shape)
            {
                continue;
            }
            for n in 1..=*count {
                let at = CellAddr::new(bottom + n, col);
                out.push(Action::CellEdit {
                    sheet: s.clone(),
                    addr: at,
                    input: rewrite_formula_by_offset(&input, CellAddr::new(bottom, col), at),
                });
            }
        }
        out
    }

    /// Rewrite a check so it asks the same question of the variant.
    fn remap_check(&self, before: &Engine, check: &Check) -> Check {
        match self {
            Perturbation::Insert {
                sheet,
                axis,
                at,
                count,
                ..
            } => map_check_refs(
                check,
                &|text| shift_a1(text, sheet, *axis, *at, *count),
                &|expect, at_text| {
                    let (qualifier, _) = split_qualified(at_text);
                    let on = qualifier
                        .map(|q| q.trim_matches('\'').to_string())
                        .unwrap_or_else(|| default_sheet_name(before));
                    shift_formula(before, expect, &on, sheet, *axis, *at, *count)
                },
            ),
            Perturbation::RenameSheet { from, to } => {
                let renamed =
                    map_check_refs(check, &|text| rename_in_a1(text, from, to), &|expect, _| {
                        rename_in_a1(expect, from, to)
                    });
                match renamed {
                    Check::SheetsExist { names } => Check::SheetsExist {
                        names: names
                            .into_iter()
                            .map(|n| if n == *from { to.clone() } else { n })
                            .collect(),
                    },
                    other => other,
                }
            }
            Perturbation::AppendRows { sheet, count } => {
                let Some(last) = before
                    .wb
                    .sheet_by_name(sheet)
                    .and_then(|s| s.used_range())
                    .map(|r| r.end.row)
                else {
                    return check.clone();
                };
                // A range that ended at the bottom of the data still means
                // "to the bottom of the data".
                // A formula's own references are left alone: a `=SUM(D2:D6)`
                // whose range grew is handled by the range remap above, and
                // this perturbation moves nothing else.
                map_check_refs(
                    check,
                    &|text| extend_a1(text, sheet, last, *count),
                    &|expect, _| extend_formula(expect, last, *count),
                )
            }
            Perturbation::ScaleLiterals { .. } => check.clone(),
        }
    }
}

/// A named sequence of perturbations, applied in order.
///
/// A sequence rather than one at a time because the combinations are where
/// the value is: a policy can memorise "the table starts at A1" or "the sheet
/// is called Sheet1" and still fail when both are false at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub label: String,
    pub steps: Vec<Perturbation>,
}

impl Recipe {
    pub fn new(label: impl Into<String>, steps: Vec<Perturbation>) -> Self {
        Recipe {
            label: label.into(),
            steps,
        }
    }

    fn changes_values(&self) -> bool {
        self.steps.iter().any(|s| s.changes_values())
    }
}

/// A variant that survived the gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub recipe: String,
    pub task: TaskSpec,
    pub trajectory: Trajectory,
}

/// A variant that did not, and why. Kept and reported: a perturbation that
/// never survives is a bug in the generator, and quietly producing fewer
/// variants is how nobody finds out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    pub recipe: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AugmentReport {
    pub accepted: Vec<Variant>,
    pub rejected: Vec<Rejection>,
}

impl AugmentReport {
    pub fn acceptance_rate(&self) -> f64 {
        let total = self.accepted.len() + self.rejected.len();
        if total == 0 {
            return 0.0;
        }
        self.accepted.len() as f64 / total as f64
    }
}

/// Generate variants of a validated demonstration.
///
/// The environment is threaded through and handed back so thousands of
/// variants share one snapshot store: every variant that happens to produce
/// an identical workbook costs nothing to keep.
pub fn augment(
    mut env: Env,
    source: &Trajectory,
    task: &TaskSpec,
    recipes: &[Recipe],
) -> Result<(Env, AugmentReport), EnvError> {
    let mut report = AugmentReport::default();

    for recipe in recipes {
        match one(&mut env, source, task, recipe) {
            Ok(Ok(variant)) => report.accepted.push(variant),
            Ok(Err(reason)) => report.rejected.push(Rejection {
                recipe: recipe.label.clone(),
                reason,
            }),
            // A perturbation the engine refused outright — an insert past the
            // sheet limit, a rename onto an existing name. Reported the same
            // way, because from the dataset's side it is the same fact: no
            // variant came out of this recipe.
            Err(e) => report.rejected.push(Rejection {
                recipe: recipe.label.clone(),
                reason: e.to_string(),
            }),
        }
    }
    Ok((env, report))
}

/// `Ok(Ok(v))` accepted, `Ok(Err(why))` rejected by the gate, `Err` broken.
fn one(
    env: &mut Env,
    source: &Trajectory,
    task: &TaskSpec,
    recipe: &Recipe,
) -> Result<Result<Variant, String>, EnvError> {
    let mut engine = env.store().load(&task.initial_snapshot)?;
    let mut actions: Vec<Action> = source.actions().cloned().collect();
    let mut checks = task.checks.clone();

    for step in &recipe.steps {
        // The order matters: `extend_actions` and `remap_check` read the
        // workbook as it was *before* this step, because "the last row of the
        // data" means the last row before rows were appended to it.
        actions = step.extend_actions(&engine, &actions);
        checks = checks
            .iter()
            .map(|c| step.remap_check(&engine, c))
            .collect();
        step.apply(&mut engine)?;
        actions = actions
            .iter()
            .map(|a| step.remap_action(&engine, a))
            .collect();
    }

    let start_sheet = recipe
        .steps
        .iter()
        .fold(task.start_sheet.clone(), |s, step| match (s, step) {
            (Some(name), Perturbation::RenameSheet { from, to }) if name == *from => {
                Some(to.clone())
            }
            (other, _) => other,
        });

    let snapshot: SnapshotId = env.store_mut().put(&engine.wb)?;
    let mut variant_task = TaskSpec {
        id: format!("{}::{}", task.id, recipe.label),
        instruction: task.instruction.clone(),
        initial_snapshot: snapshot,
        checks,
        start_sheet,
        // Headroom, not a glove. A budget set to exactly the recorded
        // demonstration's length is a budget only that demonstration can
        // meet: an agent that writes a column header before the column, or
        // takes one exploratory step, runs out on the last row and fails a
        // task it had solved. The budget exists to stop a runaway loop, and
        // twice the known-sufficient length still does that.
        max_steps: task
            .max_steps
            .max(actions.len().saturating_mul(2) as u32 + 16),
        origin: Some(format!("augmented:{}", recipe.label)),
    };

    // Replay the demonstration against the variant.
    let taken = std::mem::replace(env, Env::in_memory());
    let mut recorder = Recorder::start_task(
        taken,
        format!("{}::{}", source.id, recipe.label),
        &variant_task,
        Source::Augmented {
            from: source.id.clone(),
            perturbation: recipe.label.clone(),
        },
    )?
    // One observation, not one per step. A variant is a replay of a
    // demonstration whose every-step observations are already recorded, and
    // the rest are recoverable by replaying the variant — worth doing,
    // because they are far and away the largest thing in the file. On the
    // committed corpus it is the difference between 431K of variants and
    // 102K of them.
    .with_observations(crate::trajectory::ObservationPolicy::First);
    let mut run_result = Ok(());
    for action in &actions {
        if let Err(e) = recorder.step(action) {
            run_result = Err(e);
            break;
        }
    }

    // A perturbation that changes the inputs changes the right answer, so the
    // task's value expectations are recomputed from the replayed
    // demonstration. Those checks stop being independent evidence — the
    // demonstration is being trusted, which is why only a *validated* one is
    // ever augmented — and the structural checks are what still have teeth.
    // A variant with nothing but recomputed expectations proves nothing and
    // is refused below.
    if recipe.changes_values() && run_result.is_ok() {
        let engine = recorder.env().engine()?;
        variant_task.checks = variant_task
            .checks
            .iter()
            .map(|c| crate::task::retarget(c, engine))
            .collect();
    }

    let termination = match &run_result {
        Ok(()) => Termination::Done,
        Err(e) => Termination::Failed {
            reason: e.to_string(),
        },
    };
    let (trajectory, returned) = recorder.finish_with_env(termination, Some(&variant_task))?;
    *env = returned;

    if let Err(e) = run_result {
        return Ok(Err(e.to_string()));
    }
    let grade = trajectory
        .grade
        .clone()
        .expect("finish grades when a task is given");

    if recipe.changes_values() && !variant_task.checks.iter().any(is_independent) {
        return Ok(Err(
            "every check would have been recomputed from the demonstration, so the variant proves nothing"
                .into(),
        ));
    }
    if !grade.passed {
        let why: Vec<String> = grade.failures().map(|c| c.detail.clone()).collect();
        return Ok(Err(why.join("; ")));
    }

    Ok(Ok(Variant {
        recipe: recipe.label.clone(),
        task: variant_task,
        trajectory,
    }))
}

/// Whether a check still tests something after value expectations have been
/// recomputed from the demonstration.
fn is_independent(check: &Check) -> bool {
    !matches!(
        check,
        Check::CellNumber { .. } | Check::CellDisplays { .. } | Check::SumEquals { .. }
    )
}

// --- perturbation mechanics -------------------------------------------------

/// The insert itself, plus whatever fills the new column.
///
/// The rows filled are the ones the sheet already used, taken *before* the
/// insert — the insert moves columns, not rows, so they are still the right
/// rows afterwards, and reading them from the sheet after the insert would
/// give the same answer more confusingly.
fn insert_actions(
    engine: &Engine,
    sheet: &str,
    axis: Axis,
    at: u32,
    count: u32,
    fill: Option<&InsertFill>,
) -> Result<Vec<Action>, EnvError> {
    let mut actions = vec![match axis {
        Axis::Row => Action::RowInsert {
            sheet: sheet.to_string(),
            at,
            count,
        },
        Axis::Col => Action::ColInsert {
            sheet: sheet.to_string(),
            at,
            count,
        },
    }];

    let (Axis::Col, Some(fill)) = (axis, fill) else {
        return Ok(actions);
    };
    let s = engine
        .wb
        .sheet_by_name(sheet)
        .ok_or_else(|| EnvError::UnknownSheet(sheet.to_string()))?;
    let Some(used) = s.used_range() else {
        return Ok(actions);
    };
    for row in used.start.row..=used.end.row {
        let input = if row == used.start.row {
            &fill.header
        } else {
            &fill.value
        };
        for c in 0..count {
            actions.push(Action::CellEdit {
                sheet: sheet.to_string(),
                addr: CellAddr::new(row, at + c),
                input: input.clone(),
            });
        }
    }
    Ok(actions)
}

/// Actions that repeat a sheet's last populated row `count` more times.
fn repeat_last_row(engine: &Engine, sheet: &str, count: u32) -> Result<Vec<Action>, EnvError> {
    let s = engine
        .wb
        .sheet_by_name(sheet)
        .ok_or_else(|| EnvError::UnknownSheet(sheet.to_string()))?;
    let Some(used) = s.used_range() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for col in used.start.col..=used.end.col {
        let from = CellAddr::new(used.end.row, col);
        let Some(cell) = s.cells.get(&from) else {
            continue;
        };
        // Literals only. A formula in the bottom row is the demonstration's
        // work, and copying it down would hand the agent the answer.
        if cell.is_formula() {
            continue;
        }
        for n in 1..=count {
            out.push(Action::CellEdit {
                sheet: sheet.to_string(),
                addr: CellAddr::new(used.end.row + n, col),
                input: cell.input(),
            });
        }
    }
    Ok(out)
}

/// Actions that multiply every numeric literal in a range.
fn scale_literals(
    engine: &Engine,
    sheet: &str,
    range: RangeAddr,
    factor: f64,
) -> Result<Vec<Action>, EnvError> {
    let s = engine
        .wb
        .sheet_by_name(sheet)
        .ok_or_else(|| EnvError::UnknownSheet(sheet.to_string()))?;
    let mut out = Vec::new();
    for row in range.start.row..=range.end.row {
        for col in range.start.col..=range.end.col {
            let addr = CellAddr::new(row, col);
            let Some(cell) = s.cells.get(&addr) else {
                continue;
            };
            if cell.is_formula() {
                continue;
            }
            let engine::Value::Number(n) = cell.value() else {
                continue;
            };
            out.push(Action::CellEdit {
                sheet: sheet.to_string(),
                addr,
                input: engine::value::format_number_general(n * factor),
            });
        }
    }
    Ok(out)
}

// --- address remapping ------------------------------------------------------

fn shift_index(at: u32, count: u32, i: u32) -> u32 {
    if i >= at {
        i + count
    } else {
        i
    }
}

fn shift_addr(axis: Axis, at: u32, count: u32, a: CellAddr) -> CellAddr {
    match axis {
        Axis::Row => CellAddr::new(shift_index(at, count, a.row), a.col),
        Axis::Col => CellAddr::new(a.row, shift_index(at, count, a.col)),
    }
}

fn shift_range(axis: Axis, at: u32, count: u32, r: RangeAddr) -> RangeAddr {
    RangeAddr::new(
        shift_addr(axis, at, count, r.start),
        shift_addr(axis, at, count, r.end),
    )
}

/// Move a recorded action to where it belongs on the shifted sheet, rewriting
/// any formula it carries with the engine's own structural rewriter.
fn shift_action(
    engine: &Engine,
    action: &Action,
    sheet: &str,
    axis: Axis,
    at: u32,
    count: u32,
) -> Action {
    let mut out = action.clone();
    let on = |s: &str| s == sheet;
    match &mut out {
        Action::CellEdit {
            sheet: s,
            addr,
            input,
        } => {
            *input = shift_formula(engine, input, s, sheet, axis, at, count);
            if on(s) {
                *addr = shift_addr(axis, at, count, *addr);
            }
        }
        Action::CellClear { sheet: s, addr } => {
            if on(s) {
                *addr = shift_addr(axis, at, count, *addr);
            }
        }
        Action::RangeClear { sheet: s, range }
        | Action::MergeApply { sheet: s, range }
        | Action::MergeClear { sheet: s, range }
        | Action::FormatApply {
            sheet: s, range, ..
        }
        | Action::FormatClear { sheet: s, range }
        | Action::CondClear { sheet: s, range }
        | Action::SortApply {
            sheet: s, range, ..
        } => {
            if on(s) {
                *range = shift_range(axis, at, count, *range);
            }
        }
        Action::FillApply {
            sheet: s,
            source,
            target,
        } => {
            if on(s) {
                *source = shift_range(axis, at, count, *source);
                *target = shift_range(axis, at, count, *target);
            }
        }
        Action::RangePaste {
            source_sheet,
            source,
            target_sheet,
            target,
            ..
        } => {
            if on(source_sheet) {
                *source = shift_range(axis, at, count, *source);
            }
            if on(target_sheet) {
                *target = shift_range(axis, at, count, *target);
            }
        }
        Action::RowInsert {
            sheet: s, at: i, ..
        }
        | Action::RowDelete {
            sheet: s, at: i, ..
        } => {
            if on(s) && axis == Axis::Row {
                *i = shift_index(at, count, *i);
            }
        }
        Action::ColInsert {
            sheet: s, at: i, ..
        }
        | Action::ColDelete {
            sheet: s, at: i, ..
        } => {
            if on(s) && axis == Axis::Col {
                *i = shift_index(at, count, *i);
            }
        }
        Action::FindReplace {
            sheet: s,
            range: Some(range),
            ..
        } => {
            if on(s) {
                *range = shift_range(axis, at, count, *range);
            }
        }
        // Everything else names no address, or names one this does not know
        // how to move — a defined name's `refers_to`, a conditional rule's
        // range. Those recipes are rejected by the gate rather than guessed
        // at, which is the whole arrangement working as intended.
        _ => {}
    }
    out
}

/// Rewrite a formula's references for an insert, using `refs::structural` —
/// the same code path the engine itself uses when a row appears.
fn shift_formula(
    engine: &Engine,
    input: &str,
    formula_sheet: &str,
    shifted_sheet: &str,
    axis: Axis,
    at: u32,
    count: u32,
) -> String {
    let Some(body) = input.strip_prefix('=') else {
        return input.to_string();
    };
    let (Some(current), Some(target)) = (
        engine.wb.sheet_id_by_name(formula_sheet),
        engine.wb.sheet_id_by_name(shifted_sheet),
    ) else {
        return input.to_string();
    };
    let Ok(ast) = engine::parser::parse_formula(body) else {
        return input.to_string();
    };
    let shift = engine::refs::StructuralShift {
        sheet: target,
        axis,
        at,
        count,
        insert: true,
    };
    let rewritten =
        engine::refs::structural(&ast, &shift, current, &|n| engine.wb.sheet_id_by_name(n));
    format!("={}", rewritten.to_formula())
}

/// Move a formula from one cell to another as copy-and-paste would.
fn rewrite_formula_by_offset(input: &str, from: CellAddr, to: CellAddr) -> String {
    let Some(body) = input.strip_prefix('=') else {
        return input.to_string();
    };
    let Ok(ast) = engine::parser::parse_formula(body) else {
        return input.to_string();
    };
    let shifted = engine::refs::offset(
        &ast,
        to.row as i64 - from.row as i64,
        to.col as i64 - from.col as i64,
    );
    format!("={}", shifted.to_formula())
}

fn rename_in_action(action: &Action, from: &str, to: &str) -> Action {
    let mut out = action.clone();
    let swap = |s: &mut String| {
        if s == from {
            *s = to.to_string();
        }
    };
    match &mut out {
        Action::CellEdit { sheet, .. }
        | Action::CellClear { sheet, .. }
        | Action::RangeClear { sheet, .. }
        | Action::FillApply { sheet, .. }
        | Action::RowInsert { sheet, .. }
        | Action::RowDelete { sheet, .. }
        | Action::ColInsert { sheet, .. }
        | Action::ColDelete { sheet, .. }
        | Action::SortApply { sheet, .. }
        | Action::FilterApply { sheet, .. }
        | Action::FilterClear { sheet }
        | Action::MergeApply { sheet, .. }
        | Action::MergeClear { sheet, .. }
        | Action::FormatApply { sheet, .. }
        | Action::FormatClear { sheet, .. }
        | Action::FindReplace { sheet, .. }
        | Action::Resize { sheet, .. }
        | Action::CondAdd { sheet, .. }
        | Action::CondClear { sheet, .. }
        | Action::FreezePanes { sheet, .. } => swap(sheet),
        Action::RangePaste {
            source_sheet,
            target_sheet,
            ..
        } => {
            swap(source_sheet);
            swap(target_sheet);
        }
        Action::SheetRename { from: f, to: t } => {
            swap(f);
            swap(t);
        }
        Action::SheetAdd { name } | Action::SheetDelete { name } => swap(name),
        Action::NameDefine { refers_to, .. } => {
            *refers_to = rename_in_a1(refers_to, from, to);
        }
        Action::NameDelete { .. } | Action::Undo | Action::Redo => {}
    }
    // A cross-sheet formula names the sheet in its text.
    if let Action::CellEdit { input, .. } = &mut out {
        *input = rename_in_a1(input, from, to);
    }
    out
}

// --- A1 text remapping ------------------------------------------------------

/// Apply `f` to every address-shaped string a check carries.
fn map_check_refs(
    check: &Check,
    f: &dyn Fn(&str) -> String,
    formula: &dyn Fn(&str, &str) -> String,
) -> Check {
    match check.clone() {
        Check::CellDisplays { at, expect } => Check::CellDisplays { at: f(&at), expect },
        Check::CellNumber {
            at,
            expect,
            tolerance,
        } => Check::CellNumber {
            at: f(&at),
            expect,
            tolerance,
        },
        // `expect` is a formula written as it would appear at `at`, so it has
        // to move with `at`. Moving the address alone leaves the check asking
        // for a formula that would now be wrong there — which is not a
        // failing variant but a broken one, and the difference is invisible
        // from the outside.
        Check::CellFormula { at, expect } => Check::CellFormula {
            expect: formula(&expect, &at),
            at: f(&at),
        },
        Check::RangeFilled { range } => Check::RangeFilled { range: f(&range) },
        Check::NoErrors { range } => Check::NoErrors { range: f(&range) },
        Check::SumEquals {
            range,
            expect,
            tolerance,
        } => Check::SumEquals {
            range: f(&range),
            expect,
            tolerance,
        },
        Check::SumsMatch {
            left,
            right,
            tolerance,
        } => Check::SumsMatch {
            left: f(&left),
            right: f(&right),
            tolerance,
        },
        Check::Unchanged { ranges } => Check::Unchanged {
            ranges: ranges.iter().map(|r| f(r)).collect(),
        },
        Check::SheetsExist { names } => Check::SheetsExist { names },
        Check::NameRefersTo { name, refers_to } => Check::NameRefersTo {
            name,
            refers_to: f(&refers_to),
        },
    }
}

/// Split `Sheet!A1:B2` into its parts, keeping the qualifier's exact spelling.
fn split_qualified(text: &str) -> (Option<&str>, &str) {
    match text.split_once('!') {
        Some((sheet, rest)) => (Some(sheet), rest),
        None => (None, text),
    }
}

fn requalify(sheet: Option<&str>, rest: String) -> String {
    match sheet {
        Some(s) => format!("{s}!{rest}"),
        None => rest,
    }
}

fn on_sheet(qualifier: Option<&str>, sheet: &str) -> bool {
    // Unqualified means the workbook's first sheet, which is what every task
    // in this corpus perturbs; a qualifier has to match exactly.
    match qualifier {
        None => true,
        Some(q) => q.trim_matches('\'') == sheet,
    }
}

fn shift_a1(text: &str, sheet: &str, axis: Axis, at: u32, count: u32) -> String {
    let (qualifier, rest) = split_qualified(text);
    if !on_sheet(qualifier, sheet) {
        return text.to_string();
    }
    if let Some(r) = RangeAddr::parse_a1(rest) {
        return requalify(qualifier, shift_range(axis, at, count, r).to_a1());
    }
    if let Some(a) = CellAddr::parse_a1(rest) {
        return requalify(qualifier, shift_addr(axis, at, count, a).to_a1());
    }
    text.to_string()
}

/// Grow a range whose bottom sat on the last row of the data.
fn extend_a1(text: &str, sheet: &str, last_row: u32, count: u32) -> String {
    let (qualifier, rest) = split_qualified(text);
    if !on_sheet(qualifier, sheet) {
        return text.to_string();
    }
    let Some(r) = RangeAddr::parse_a1(rest) else {
        return text.to_string();
    };
    if r.end.row != last_row {
        return text.to_string();
    }
    let grown = RangeAddr::new(r.start, CellAddr::new(r.end.row + count, r.end.col));
    requalify(qualifier, grown.to_a1())
}

/// The sheet an unqualified reference means.
fn default_sheet_name(engine: &Engine) -> String {
    engine
        .wb
        .sheets
        .first()
        .map(|s| s.name.clone())
        .unwrap_or_default()
}

/// Grow a formula's ranges whose bottom sat on the last row of the data.
///
/// The `=SUM(D2:D6)` a task expects under a five-row table becomes
/// `=SUM(D2:D31)` under a thirty-row one. Only the bottom edge moves, and
/// only when it was exactly on the old last row — a range that stopped short
/// of the data meant something else and is left alone.
fn extend_formula(input: &str, last_row: u32, count: u32) -> String {
    let Some(body) = input.strip_prefix('=') else {
        return input.to_string();
    };
    let Ok(ast) = engine::parser::parse_formula(body) else {
        return input.to_string();
    };
    let grown = engine::refs::map_refs(&ast, &mut |r| match r {
        engine::refs::RefKind::Range(rr) if rr.end.row == last_row => {
            let mut end = rr.end;
            end.row += count;
            Some(engine::ast::Expr::Range(engine::ast::RangeRef {
                sheet: rr.sheet.clone(),
                start: rr.start,
                end,
            }))
        }
        _ => None,
    });
    format!("={}", grown.to_formula())
}

/// Rename a sheet inside any A1 text, including a formula's references.
fn rename_in_a1(text: &str, from: &str, to: &str) -> String {
    let quoted_from = format!("'{from}'!");
    let plain_from = format!("{from}!");
    let replacement = if to.chars().all(|c| c.is_alphanumeric() || c == '_') {
        format!("{to}!")
    } else {
        format!("'{to}'!")
    };
    text.replace(&quoted_from, &replacement)
        .replace(&plain_from, &replacement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::SnapshotStore;
    use crate::trajectory::ObservationPolicy;

    fn edit(sheet: &str, a1: &str, input: &str) -> Action {
        Action::CellEdit {
            sheet: sheet.into(),
            addr: CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        }
    }

    /// A three-row ledger with an empty Total column.
    fn base_workbook() -> Engine {
        let mut e = Engine::new();
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
            ("A4", "Washer"),
            ("B4", "2"),
            ("C4", "1.25"),
        ] {
            e.apply(&edit("Sheet1", a1, input)).unwrap();
        }
        e
    }

    /// The validated demonstration everything here is generated from.
    fn demonstration() -> (Env, TaskSpec, Trajectory) {
        let mut store = SnapshotStore::in_memory();
        let id = store.put(&base_workbook().wb).unwrap();
        let task = TaskSpec {
            id: "totals".into(),
            instruction: "Fill the Total column with quantity times price.".into(),
            initial_snapshot: id,
            checks: vec![
                Check::RangeFilled {
                    range: "D2:D4".into(),
                },
                Check::NoErrors {
                    range: "A1:D4".into(),
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
            max_steps: 16,
            origin: None,
        };
        let mut rec = Recorder::start_task(Env::new(store), "demo", &task, Source::Human)
            .unwrap()
            .with_observations(ObservationPolicy::None);
        for row in 2..=4 {
            rec.step(&edit(
                "Sheet1",
                &format!("D{row}"),
                &format!("=B{row}*C{row}"),
            ))
            .unwrap();
        }
        let trajectory = rec.finish(Termination::Done, Some(&task)).unwrap();
        assert!(trajectory.is_demonstration(), "the source must be valid");

        // Rebuild an environment holding the same starting snapshot.
        let mut store = SnapshotStore::in_memory();
        store.put(&base_workbook().wb).unwrap();
        (Env::new(store), task, trajectory)
    }

    fn run(recipes: Vec<Recipe>) -> AugmentReport {
        let (env, task, source) = demonstration();
        let (_, report) = augment(env, &source, &task, &recipes).unwrap();
        report
    }

    #[test]
    fn moving_the_table_down_and_right_produces_a_variant_that_passes() {
        let report = run(vec![Recipe::new(
            "moved",
            vec![
                Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: Axis::Row,
                    at: 0,
                    count: 3,
                    fill: None,
                },
                Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: Axis::Col,
                    at: 0,
                    count: 2,
                    fill: None,
                },
            ],
        )]);
        assert!(
            report.rejected.is_empty(),
            "rejected: {:?}",
            report.rejected
        );
        let v = &report.accepted[0];
        // The task moved with the table.
        assert!(
            v.task.checks.contains(&Check::RangeFilled {
                range: "F5:F7".into()
            }),
            "{:?}",
            v.task.checks
        );
        // ...and so did the demonstration, formulas included.
        assert_eq!(v.trajectory.steps[0].action, edit("Sheet1", "F5", "=D5*E5"));
    }

    #[test]
    fn an_irrelevant_column_in_the_middle_does_not_break_the_demonstration() {
        // The one that catches a policy which learned "the answer goes in
        // column D" rather than "the answer goes next to Price".
        let report = run(vec![Recipe::new(
            "distractor",
            vec![Perturbation::Insert {
                sheet: "Sheet1".into(),
                axis: Axis::Col,
                at: 1,
                count: 1,
                fill: None,
            }],
        )]);
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        let v = &report.accepted[0];
        assert_eq!(v.trajectory.steps[0].action, edit("Sheet1", "E2", "=C2*D2"));
    }

    #[test]
    fn renaming_the_sheet_carries_the_task_and_the_demonstration_with_it() {
        let report = run(vec![Recipe::new(
            "renamed",
            vec![Perturbation::RenameSheet {
                from: "Sheet1".into(),
                to: "Ledger".into(),
            }],
        )]);
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        let v = &report.accepted[0];
        assert_eq!(v.trajectory.steps[0].action, edit("Ledger", "D2", "=B2*C2"));
    }

    #[test]
    fn more_rows_of_data_extends_the_fill_and_the_ranges() {
        // The perturbation the whole exercise is for: a policy that memorised
        // three rows has learned nothing.
        let report = run(vec![Recipe::new(
            "longer",
            vec![Perturbation::AppendRows {
                sheet: "Sheet1".into(),
                count: 3,
            }],
        )]);
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        let v = &report.accepted[0];
        assert_eq!(
            v.trajectory.steps.len(),
            6,
            "the demonstration had to grow with the data"
        );
        assert!(v.task.checks.contains(&Check::RangeFilled {
            range: "D2:D7".into()
        }));
        assert_eq!(v.trajectory.steps[5].action, edit("Sheet1", "D7", "=B7*C7"));
    }

    #[test]
    fn different_numbers_produce_a_variant_with_a_different_answer() {
        let report = run(vec![Recipe::new(
            "doubled",
            vec![Perturbation::ScaleLiterals {
                sheet: "Sheet1".into(),
                range: RangeAddr::parse_a1("B2:B4").unwrap(),
                factor: 3.0,
            }],
        )]);
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        let v = &report.accepted[0];
        let sum = v
            .task
            .checks
            .iter()
            .find_map(|c| match c {
                Check::SumEquals { expect, .. } => Some(*expect),
                _ => None,
            })
            .unwrap();
        assert_eq!(sum, 51.0, "the expectation had to follow the inputs");
    }

    #[test]
    fn a_formula_check_moves_its_expected_formula_as_well_as_its_address() {
        // The regression: `CellFormula.expect` is written as it would appear
        // at `at`, so moving `at` without moving `expect` leaves the check
        // asking for a formula that would be wrong there. Every variant of a
        // task with a total row was being rejected for that, and a rejection
        // that comes from the generator rather than the perturbation is
        // invisible from the outside — it just looks like a hard task.
        let mut store = SnapshotStore::in_memory();
        let id = store.put(&base_workbook().wb).unwrap();
        let task = TaskSpec {
            id: "grand-total".into(),
            instruction: "Total the column and sum it underneath.".into(),
            initial_snapshot: id,
            checks: vec![Check::CellFormula {
                at: "D5".into(),
                expect: "=SUM(D2:D4)".into(),
            }],
            start_sheet: None,
            max_steps: 8,
            origin: None,
        };
        let mut rec = Recorder::start_task(Env::new(store), "demo", &task, Source::Human)
            .unwrap()
            .with_observations(ObservationPolicy::None);
        for row in 2..=4 {
            rec.step(&edit(
                "Sheet1",
                &format!("D{row}"),
                &format!("=B{row}*C{row}"),
            ))
            .unwrap();
        }
        rec.step(&edit("Sheet1", "D5", "=SUM(D2:D4)")).unwrap();
        let source = rec.finish(Termination::Done, Some(&task)).unwrap();
        assert!(source.is_demonstration());

        let mut store = SnapshotStore::in_memory();
        store.put(&base_workbook().wb).unwrap();
        let (_, report) = augment(
            Env::new(store),
            &source,
            &task,
            &[Recipe::new(
                "moved",
                vec![Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: Axis::Row,
                    at: 0,
                    count: 3,
                    fill: None,
                }],
            )],
        )
        .unwrap();

        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        assert!(report.accepted[0]
            .task
            .checks
            .contains(&Check::CellFormula {
                at: "D8".into(),
                expect: "=SUM(D5:D7)".into(),
            }));
    }

    #[test]
    fn a_filled_distractor_column_keeps_the_table_one_table() {
        // The bug this pins: an unfilled column insert leaves a blank
        // column, table detection stops at it — correctly — and the variant
        // becomes a *narrower table* rather than one with something
        // irrelevant in the middle. The demonstration replay passes either
        // way, because fixed addresses do not care where the table ends, so
        // the gate could not catch it. Only an agent that finds its own
        // table notices, and by then the variant is in the corpus.
        let (env, task, source) = demonstration();
        let (env, report) = augment(
            env,
            &source,
            &task,
            &[Recipe::new(
                "distractor",
                vec![Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: Axis::Col,
                    at: 1,
                    count: 1,
                    fill: Some(InsertFill {
                        header: "Bin".into(),
                        value: "A-12".into(),
                    }),
                }],
            )],
        )
        .unwrap();
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);

        let mut check = Env::new(env.into_store());
        check
            .reset(&report.accepted[0].task.initial_snapshot)
            .unwrap();
        let obs = check.observe().unwrap();
        let headers: Vec<&str> = obs.tables[0]
            .columns
            .iter()
            .map(|c| c.header.as_str())
            .collect();
        assert_eq!(
            headers,
            ["Item", "Bin", "Qty", "Price", "Total"],
            "the distractor split the table in two"
        );
    }

    #[test]
    fn an_unfilled_column_insert_really_does_split_the_table() {
        // The other half, so the reason for `fill` is written down as a
        // fact rather than as a claim in a doc comment.
        let (env, task, source) = demonstration();
        let (env, report) = augment(
            env,
            &source,
            &task,
            &[Recipe::new(
                "gap",
                vec![Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: Axis::Col,
                    at: 1,
                    count: 1,
                    fill: None,
                }],
            )],
        )
        .unwrap();
        assert!(report.rejected.is_empty(), "the gate cannot see this");

        let mut check = Env::new(env.into_store());
        check
            .reset(&report.accepted[0].task.initial_snapshot)
            .unwrap();
        let obs = check.observe().unwrap();
        assert_eq!(
            obs.tables[0].columns.len(),
            1,
            "a blank column should stop table detection at it"
        );
    }

    #[test]
    fn a_perturbation_the_engine_refuses_is_reported_not_swallowed() {
        let (env, task, source) = demonstration();
        let recipe = Recipe::new(
            "onto-a-sheet-that-is-not-there",
            vec![Perturbation::Insert {
                sheet: "Nonexistent".into(),
                axis: Axis::Col,
                at: 0,
                count: 1,
                fill: None,
            }],
        );
        let (_, report) = augment(env, &source, &task, &[recipe]).unwrap();
        assert!(report.accepted.is_empty());
        assert_eq!(report.rejected.len(), 1);
        assert!(report.acceptance_rate() < 1.0);
    }

    #[test]
    fn the_gate_catches_a_variant_the_generator_got_wrong() {
        // The test that decides whether any of this is safe. Extending a
        // filled column when rows are appended is a *guess* about what the
        // demonstration meant, and here the guess is wrong: this
        // demonstration also writes a grand total directly under the data, so
        // appending rows puts new data where the grand total goes. Nothing in
        // the generator notices. The grader does.
        let mut store = SnapshotStore::in_memory();
        let id = store.put(&base_workbook().wb).unwrap();
        let task = TaskSpec {
            id: "totals-and-a-grand-total".into(),
            instruction: "Fill the Total column and put a grand total underneath.".into(),
            initial_snapshot: id,
            checks: vec![
                Check::RangeFilled {
                    range: "D2:D4".into(),
                },
                Check::CellFormula {
                    at: "D5".into(),
                    expect: "=SUM(D2:D4)".into(),
                },
            ],
            start_sheet: None,
            max_steps: 16,
            origin: None,
        };
        let mut rec = Recorder::start_task(Env::new(store), "demo", &task, Source::Human)
            .unwrap()
            .with_observations(ObservationPolicy::None);
        for row in 2..=4 {
            rec.step(&edit(
                "Sheet1",
                &format!("D{row}"),
                &format!("=B{row}*C{row}"),
            ))
            .unwrap();
        }
        rec.step(&edit("Sheet1", "D5", "=SUM(D2:D4)")).unwrap();
        let source = rec.finish(Termination::Done, Some(&task)).unwrap();
        assert!(source.is_demonstration(), "the source itself must be valid");

        let mut store = SnapshotStore::in_memory();
        store.put(&base_workbook().wb).unwrap();
        let (_, report) = augment(
            Env::new(store),
            &source,
            &task,
            &[Recipe::new(
                "longer",
                vec![Perturbation::AppendRows {
                    sheet: "Sheet1".into(),
                    count: 2,
                }],
            )],
        )
        .unwrap();

        assert!(
            report.accepted.is_empty(),
            "a variant the generator mangled was shipped anyway"
        );
        assert_eq!(report.rejected.len(), 1);
        assert!(
            report.rejected[0].reason.contains("D5"),
            "rejected for the wrong reason: {}",
            report.rejected[0].reason
        );
    }

    #[test]
    fn a_longer_variant_really_does_hold_longer_data() {
        // Checking the artifact rather than the report. A generator that
        // produced an empty workbook and a task whose checks happened to pass
        // on it would look identical from the outside.
        let (env, task, source) = demonstration();
        let (env, report) = augment(
            env,
            &source,
            &task,
            &[Recipe::new(
                "longer",
                vec![Perturbation::AppendRows {
                    sheet: "Sheet1".into(),
                    count: 2,
                }],
            )],
        )
        .unwrap();
        let v = &report.accepted[0];

        let mut check_env = Env::new(env.into_store());
        check_env.reset(&v.task.initial_snapshot).unwrap();
        let e = check_env.engine().unwrap();
        // The appended rows carry the last row's data...
        assert_eq!(e.value_at("Sheet1", "B5"), engine::Value::Number(2.0));
        assert_eq!(e.value_at("Sheet1", "C6"), engine::Value::Number(1.25));
        // ...and the Total column is still the agent's job.
        assert_eq!(e.value_at("Sheet1", "D5"), engine::Value::Empty);

        // Then replaying the extended demonstration fills all five rows.
        for action in v.trajectory.actions() {
            check_env.step(action).unwrap();
        }
        assert_eq!(
            check_env.engine().unwrap().value_at("Sheet1", "D6"),
            engine::Value::Number(2.5)
        );
    }

    #[test]
    fn every_accepted_variant_replays_faithfully() {
        // Belt and braces: the variants are recorded trajectories too, and
        // they have to satisfy the same replay invariant as anything else in
        // the dataset.
        let (env, task, source) = demonstration();
        let recipes = vec![
            Recipe::new(
                "moved",
                vec![Perturbation::Insert {
                    sheet: "Sheet1".into(),
                    axis: Axis::Row,
                    at: 0,
                    count: 2,
                    fill: None,
                }],
            ),
            Recipe::new(
                "renamed",
                vec![Perturbation::RenameSheet {
                    from: "Sheet1".into(),
                    to: "Data".into(),
                }],
            ),
        ];
        let (env, report) = augment(env, &source, &task, &recipes).unwrap();
        assert_eq!(report.accepted.len(), 2, "{:?}", report.rejected);
        let store = env.into_store();
        for v in &report.accepted {
            let r = crate::trajectory::replay(store.clone(), &v.trajectory, Some(&v.task)).unwrap();
            assert!(r.faithful, "{:?}", r);
            assert!(r.grade.unwrap().passed);
        }
    }

    #[test]
    fn combining_perturbations_still_produces_one_coherent_variant() {
        // Where the value is: a policy can memorise one fact and still fail
        // when two are false at once.
        let report = run(vec![Recipe::new(
            "everything",
            vec![
                Perturbation::RenameSheet {
                    from: "Sheet1".into(),
                    to: "Q3 Ledger".into(),
                },
                Perturbation::Insert {
                    sheet: "Q3 Ledger".into(),
                    axis: Axis::Row,
                    at: 0,
                    count: 2,
                    fill: None,
                },
                Perturbation::Insert {
                    sheet: "Q3 Ledger".into(),
                    axis: Axis::Col,
                    at: 1,
                    count: 1,
                    fill: None,
                },
                Perturbation::AppendRows {
                    sheet: "Q3 Ledger".into(),
                    count: 2,
                },
            ],
        )]);
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        let v = &report.accepted[0];
        assert_eq!(v.trajectory.steps.len(), 5);
        assert!(v.trajectory.is_demonstration());
    }
}
