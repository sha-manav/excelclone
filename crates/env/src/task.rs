//! Task specifications and the graders that decide whether one was done.
//!
//! Every grader here is deterministic and reads the *workbook*, never the
//! observation. That separation is the point: an observation is a lossy
//! summary a policy is allowed to be misled by, and a score has to be
//! something a summarization bug cannot move.
//!
//! A task's checks are a conjunction — all of them or it did not pass — and
//! the result names each one, because "failed" is not actionable and "the
//! total is 8880 where 8900 was expected, and D7 was modified when it should
//! not have been" is.
//!
//! The forbidden-range check exists because of a specific failure mode: a
//! policy that completes more tasks while quietly rewriting cells nobody
//! asked it to touch is *worse* than one that completes fewer. Average reward
//! cannot see that. A hard check can.

use std::collections::BTreeMap;

use engine::{CellAddr, Engine, RangeAddr, Value};
use serde::{Deserialize, Serialize};

use crate::snapshot::SnapshotId;

/// One thing a task requires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum Check {
    /// A cell must display exactly this. Compared on the *displayed* string
    /// so a task can require "$1,234.00" rather than 1234, which is often
    /// what was actually asked for.
    CellDisplays { at: String, expect: String },
    /// A cell must hold this number, within `tolerance`. Separate from
    /// `CellDisplays` because a total is a number and its formatting is not
    /// usually part of the task.
    CellNumber {
        at: String,
        expect: f64,
        #[serde(default)]
        tolerance: f64,
    },
    /// A cell must contain a formula, and its *relative shape* must match —
    /// so "put =B2*C2 in D2 and fill down" is one check rather than four
    /// hundred, and a policy that hard-coded `=B2*C2` in every row fails it.
    CellFormula {
        at: String,
        /// The expected formula written as it would appear at `at`.
        expect: String,
    },
    /// Every cell in the range must hold a formula of the same shape as the
    /// one in the range's first cell. This is what "fill it down" means.
    RangeFilled { range: String },
    /// Two ranges must sum to the same number: the debits-equal-credits
    /// shape, and the most common real-world invariant there is.
    SumsMatch {
        left: String,
        right: String,
        #[serde(default)]
        tolerance: f64,
    },
    /// A range must sum to a number.
    SumEquals {
        range: String,
        expect: f64,
        #[serde(default)]
        tolerance: f64,
    },
    /// No cell in the range may hold an error.
    NoErrors { range: String },
    /// Nothing in these ranges may differ from the initial snapshot. The
    /// check that stops "completed the task and broke the sheet".
    Unchanged { ranges: Vec<String> },
    /// The workbook must still have these sheets, under these names.
    SheetsExist { names: Vec<String> },
    /// A defined name must exist and point here.
    NameRefersTo { name: String, refers_to: String },
}

/// A task: what to do, where to start, and how to know it was done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    /// What a human would be told to do.
    pub instruction: String,
    pub initial_snapshot: SnapshotId,
    pub checks: Vec<Check>,
    /// The sheet a policy starts on.
    #[serde(default)]
    pub start_sheet: Option<String>,
    /// A budget: a policy that has not finished by here has failed. Stops a
    /// looping agent from running forever and makes "how many steps did this
    /// take" a comparable number across policies.
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    /// Where this task came from — a human trajectory, or which perturbation
    /// generated it. Kept so a score can be broken down by provenance.
    #[serde(default)]
    pub origin: Option<String>,
}

fn default_max_steps() -> u32 {
    64
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub check: Check,
    pub passed: bool,
    /// What was actually there, when it failed. Always populated on failure:
    /// a grader that only says "no" cannot be debugged.
    pub detail: String,
}

/// What grading a task produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeResult {
    pub task_id: String,
    pub passed: bool,
    pub checks: Vec<CheckResult>,
    /// Cells changed since the initial snapshot that no check asked for.
    ///
    /// Not a pass/fail on its own — a task may legitimately touch cells its
    /// checks do not name — but it is reported on every grade, because it is
    /// the number that separates "did the task" from "did the task and
    /// nothing else".
    pub incidental_changes: u32,
}

impl GradeResult {
    pub fn failures(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|c| !c.passed)
    }
}

/// Grade a workbook against a task. `initial` is the state the task started
/// from, needed by the checks that ask what changed.
pub fn grade(task: &TaskSpec, engine: &Engine, initial: &Engine) -> GradeResult {
    let checks: Vec<CheckResult> = task
        .checks
        .iter()
        .map(|c| run_check(c, engine, initial))
        .collect();
    let named = named_cells(engine, task);
    let incidental = changed_cells(initial, engine)
        .into_iter()
        .filter(|k| !named.contains(k))
        .count() as u32;

    GradeResult {
        task_id: task.id.clone(),
        passed: checks.iter().all(|c| c.passed),
        checks,
        incidental_changes: incidental,
    }
}

fn run_check(check: &Check, engine: &Engine, initial: &Engine) -> CheckResult {
    let (passed, detail) = match check {
        Check::CellDisplays { at, expect } => match resolve(engine, at) {
            None => (false, format!("{at} is not an address")),
            Some((sheet, addr)) => {
                let got = display_at(engine, &sheet, addr);
                (got == *expect, format!("{at} shows {got:?}"))
            }
        },
        Check::CellNumber {
            at,
            expect,
            tolerance,
        } => match resolve(engine, at) {
            None => (false, format!("{at} is not an address")),
            Some((sheet, addr)) => match value_at(engine, &sheet, addr) {
                Value::Number(n) => (
                    close_enough(n, *expect, *tolerance),
                    format!("{at} holds {n}"),
                ),
                other => (
                    false,
                    format!("{at} holds {} , not a number", other.display()),
                ),
            },
        },
        Check::CellFormula { at, expect } => match resolve(engine, at) {
            None => (false, format!("{at} is not an address")),
            Some((sheet, addr)) => match formula_at(engine, &sheet, addr) {
                None => (false, format!("{at} holds no formula")),
                Some(got) => (
                    same_formula(&got, expect, addr, addr),
                    format!("{at} holds {got:?}"),
                ),
            },
        },
        Check::RangeFilled { range } => filled_check(engine, range),
        Check::SumsMatch {
            left,
            right,
            tolerance,
        } => {
            let a = sum_of(engine, left);
            let b = sum_of(engine, right);
            match (a, b) {
                (Some(a), Some(b)) => (
                    close_enough(a, b, *tolerance),
                    format!("{left} sums to {a}, {right} to {b}"),
                ),
                _ => (false, format!("{left} or {right} is not a range")),
            }
        }
        Check::SumEquals {
            range,
            expect,
            tolerance,
        } => match sum_of(engine, range) {
            None => (false, format!("{range} is not a range")),
            Some(got) => (
                close_enough(got, *expect, *tolerance),
                format!("{range} sums to {got}"),
            ),
        },
        Check::NoErrors { range } => match parse_range(engine, range) {
            None => (false, format!("{range} is not a range")),
            Some((sheet, r)) => {
                let bad: Vec<String> = cells_of(r)
                    .filter(|a| matches!(value_at(engine, &sheet, *a), Value::Error(_)))
                    .map(|a| a.to_a1())
                    .take(5)
                    .collect();
                (
                    bad.is_empty(),
                    if bad.is_empty() {
                        format!("{range} is clean")
                    } else {
                        format!("errors at {}", bad.join(", "))
                    },
                )
            }
        },
        Check::Unchanged { ranges } => {
            let mut touched = Vec::new();
            for range in ranges {
                let Some((sheet, r)) = parse_range(engine, range) else {
                    return CheckResult {
                        check: check.clone(),
                        passed: false,
                        detail: format!("{range} is not a range"),
                    };
                };
                for addr in cells_of(r) {
                    if value_at(initial, &sheet, addr) != value_at(engine, &sheet, addr)
                        || formula_at(initial, &sheet, addr) != formula_at(engine, &sheet, addr)
                    {
                        touched.push(format!("{sheet}!{}", addr.to_a1()));
                    }
                }
            }
            let shown: Vec<String> = touched.iter().take(5).cloned().collect();
            (
                touched.is_empty(),
                if touched.is_empty() {
                    "untouched".into()
                } else {
                    format!("{} cell(s) changed: {}", touched.len(), shown.join(", "))
                },
            )
        }
        Check::SheetsExist { names } => {
            let missing: Vec<&String> = names
                .iter()
                .filter(|n| engine.wb.sheet_by_name(n).is_none())
                .collect();
            (
                missing.is_empty(),
                if missing.is_empty() {
                    "all present".into()
                } else {
                    format!("missing {missing:?}")
                },
            )
        }
        Check::NameRefersTo { name, refers_to } => {
            let key = name.to_ascii_uppercase();
            match engine.wb.names.get(&key) {
                None => (false, format!("no name {name}")),
                Some(got) => (
                    got.eq_ignore_ascii_case(refers_to),
                    format!("{name} refers to {got}"),
                ),
            }
        }
    };
    CheckResult {
        check: check.clone(),
        passed,
        detail,
    }
}

/// Whether two numbers count as the same answer.
///
/// An explicit `tolerance` wins when the task sets one — "within a penny" is
/// a real requirement and only the task knows it. With no tolerance the
/// fallback is the engine's own rule: agreement to fifteen significant
/// digits, which is what Excel considers equal and what every other
/// comparison in this codebase uses.
///
/// Not exact equality, and this is the whole reason the function exists:
/// `0.1 + 0.2` is 0.30000000000000004, and a grader that failed a correct
/// column of sums over the last bit would be teaching a policy to avoid
/// correct answers.
fn close_enough(got: f64, expect: f64, tolerance: f64) -> bool {
    if tolerance > 0.0 {
        return (got - expect).abs() <= tolerance;
    }
    engine::eval::agree_to_15_digits(got, expect)
}

/// Every cell of the range holds a formula shaped like the first one's.
fn filled_check(engine: &Engine, range: &str) -> (bool, String) {
    let Some((sheet, r)) = parse_range(engine, range) else {
        return (false, format!("{range} is not a range"));
    };
    let first = r.start;
    let Some(template) = formula_at(engine, &sheet, first) else {
        return (
            false,
            format!("{}!{} holds no formula", sheet, first.to_a1()),
        );
    };
    for addr in cells_of(r) {
        let Some(got) = formula_at(engine, &sheet, addr) else {
            return (
                false,
                format!("{}!{} holds no formula", sheet, addr.to_a1()),
            );
        };
        if !same_formula(&got, &template, addr, first) {
            return (
                false,
                format!(
                    "{}!{} holds {got:?}, which is not {template:?} filled down",
                    sheet,
                    addr.to_a1()
                ),
            );
        }
    }
    (true, format!("{range} is filled with {template:?}"))
}

/// Whether `got` at `at` is `expect` written at `from`, filled.
///
/// Comparing shapes rather than text is what lets a check say "fill this
/// down" once. Two formulas match when shifting the template from its own
/// cell to this one reproduces it — so `=B2*C2` in D2 and `=B3*C3` in D3 are
/// the same formula, and `=B2*C2` in both is not.
fn same_formula(got: &str, expect: &str, at: CellAddr, from: CellAddr) -> bool {
    crate::observe::relative_shape(got, at) == crate::observe::relative_shape(expect, from)
}

/// Cells whose value or formula differs between two workbooks.
pub fn changed_cells(before: &Engine, after: &Engine) -> Vec<(String, CellAddr)> {
    let mut out = Vec::new();
    let names: Vec<String> = after.wb.sheets.iter().map(|s| s.name.clone()).collect();
    for name in names {
        let Some(now) = after.wb.sheet_by_name(&name) else {
            continue;
        };
        let then = before.wb.sheet_by_name(&name);
        let mut addrs: std::collections::BTreeSet<CellAddr> = now.cells.keys().copied().collect();
        if let Some(then) = then {
            addrs.extend(then.cells.keys().copied());
        }
        for addr in addrs {
            let a = then.map(|s| s.value(addr)).unwrap_or(Value::Empty);
            let b = now.value(addr);
            let fa = then
                .and_then(|s| s.cells.get(&addr))
                .filter(|c| c.is_formula())
                .map(|c| c.input());
            let fb = now
                .cells
                .get(&addr)
                .filter(|c| c.is_formula())
                .map(|c| c.input());
            if a != b || fa != fb {
                out.push((name.clone(), addr));
            }
        }
    }
    out
}

/// Every cell a task's checks name, so the rest can be counted as incidental.
fn named_cells(engine: &Engine, task: &TaskSpec) -> std::collections::HashSet<(String, CellAddr)> {
    let mut out = std::collections::HashSet::new();
    let mut add_range = |text: &str| {
        if let Some((sheet, r)) = split_ref(text) {
            // Unqualified means the first sheet — the same rule `resolve`
            // uses. Leaving it blank here would put `("", D2)` in the set
            // while `changed_cells` reports `("Sheet1", D2)`, so nothing
            // would ever match and every task would look like it had
            // scribbled outside the lines.
            let sheet = default_sheet(engine, sheet);
            if let Some(r) = RangeAddr::parse_a1(r) {
                for addr in cells_of(r) {
                    out.insert((sheet.clone(), addr));
                }
            } else if let Some(a) = CellAddr::parse_a1(r) {
                out.insert((sheet, a));
            }
        }
    };
    for check in &task.checks {
        match check {
            Check::CellDisplays { at, .. }
            | Check::CellNumber { at, .. }
            | Check::CellFormula { at, .. } => add_range(at),
            Check::RangeFilled { range }
            | Check::NoErrors { range }
            | Check::SumEquals { range, .. } => add_range(range),
            Check::SumsMatch { left, right, .. } => {
                add_range(left);
                add_range(right);
            }
            // `Unchanged` names cells that must *not* change; counting them
            // as expected would make the incidental figure meaningless.
            Check::Unchanged { .. } | Check::SheetsExist { .. } | Check::NameRefersTo { .. } => {}
        }
    }
    out
}

// --- address plumbing -------------------------------------------------------

/// `Sheet1!A1` or `A1`, the latter meaning the workbook's first sheet.
fn split_ref(text: &str) -> Option<(String, &str)> {
    match text.split_once('!') {
        Some((sheet, rest)) => Some((sheet.trim_matches('\'').to_string(), rest)),
        None => Some((String::new(), text)),
    }
}

fn resolve(engine: &Engine, text: &str) -> Option<(String, CellAddr)> {
    let (sheet, rest) = split_ref(text)?;
    let sheet = default_sheet(engine, sheet);
    Some((sheet, CellAddr::parse_a1(rest)?))
}

fn parse_range(engine: &Engine, text: &str) -> Option<(String, RangeAddr)> {
    let (sheet, rest) = split_ref(text)?;
    let sheet = default_sheet(engine, sheet);
    let range =
        RangeAddr::parse_a1(rest).or_else(|| CellAddr::parse_a1(rest).map(RangeAddr::single))?;
    Some((sheet, range))
}

fn default_sheet(engine: &Engine, sheet: String) -> String {
    if sheet.is_empty() {
        engine
            .wb
            .sheets
            .first()
            .map(|s| s.name.clone())
            .unwrap_or_default()
    } else {
        sheet
    }
}

fn cells_of(r: RangeAddr) -> impl Iterator<Item = CellAddr> {
    (r.start.row..=r.end.row)
        .flat_map(move |row| (r.start.col..=r.end.col).map(move |col| CellAddr::new(row, col)))
}

fn value_at(engine: &Engine, sheet: &str, addr: CellAddr) -> Value {
    engine
        .wb
        .sheet_by_name(sheet)
        .map(|s| s.value(addr))
        .unwrap_or(Value::Empty)
}

fn display_at(engine: &Engine, sheet: &str, addr: CellAddr) -> String {
    let Some(s) = engine.wb.sheet_by_name(sheet) else {
        return String::new();
    };
    let format = engine.wb.formats.resolve(s.format_id(addr));
    let value = s.value(addr);
    match &format.number_format {
        // A format code the engine cannot apply falls back to the plain
        // display rather than to an error string: the check is about the
        // value, and a bad format code is the workbook's problem, not the
        // policy's.
        Some(code) => engine::functions::numfmt::format_value(&value, code)
            .unwrap_or_else(|_| value.display()),
        None => value.display(),
    }
}

fn formula_at(engine: &Engine, sheet: &str, addr: CellAddr) -> Option<String> {
    engine
        .wb
        .sheet_by_name(sheet)?
        .cells
        .get(&addr)
        .filter(|c| c.is_formula())
        .map(|c| c.input())
}

fn sum_of(engine: &Engine, range: &str) -> Option<f64> {
    let (sheet, r) = parse_range(engine, range)?;
    let total: f64 = cells_of(r)
        .filter_map(|a| match value_at(engine, &sheet, a) {
            Value::Number(n) => Some(n),
            _ => None,
        })
        .sum();
    // Rust's `Sum for f64` folds from -0.0, so an empty range reports "-0" in
    // the detail string. Equal to zero, confusing to read.
    Some(total + 0.0)
}

/// Rewrite a check's *expected value* to what the workbook currently holds.
///
/// Only for generated variants whose inputs changed: doubling every quantity
/// changes what the Total column should sum to, and a variant that kept the
/// original expectation would be unpassable for reasons that have nothing to
/// do with the policy.
///
/// This makes the retargeted check tautological against the run it was taken
/// from, which is exactly why only a *validated* demonstration is ever
/// augmented and why `augment` refuses a variant whose checks are all
/// retargeted. Structural checks — is it filled, are there errors, did the
/// inputs survive — are untouched and are what still has teeth.
pub fn retarget(check: &Check, engine: &Engine) -> Check {
    match check {
        Check::CellNumber {
            at,
            expect,
            tolerance,
        } => match resolve(engine, at).map(|(s, a)| value_at(engine, &s, a)) {
            Some(Value::Number(n)) => Check::CellNumber {
                at: at.clone(),
                expect: n,
                tolerance: *tolerance,
            },
            // Not a number any more: leave the expectation alone and let the
            // grader fail the variant, which is the honest outcome.
            _ => Check::CellNumber {
                at: at.clone(),
                expect: *expect,
                tolerance: *tolerance,
            },
        },
        Check::CellDisplays { at, .. } => match resolve(engine, at) {
            Some((sheet, addr)) => Check::CellDisplays {
                at: at.clone(),
                expect: display_at(engine, &sheet, addr),
            },
            None => check.clone(),
        },
        Check::SumEquals {
            range,
            expect,
            tolerance,
        } => Check::SumEquals {
            range: range.clone(),
            expect: sum_of(engine, range).unwrap_or(*expect),
            tolerance: *tolerance,
        },
        other => other.clone(),
    }
}

/// Task specs as a TOML-free JSON list, for the CLI and the eval corpus.
pub fn load_tasks(path: &std::path::Path) -> Result<Vec<TaskSpec>, crate::EnvError> {
    let text = std::fs::read_to_string(path).map_err(crate::EnvError::Io)?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        out.push(serde_json::from_str(line).map_err(crate::EnvError::Serde)?);
    }
    Ok(out)
}

/// Every distinct sheet a task's checks mention, so a perturbation knows what
/// it has to rename in the checks as well as in the workbook.
pub fn sheets_mentioned(task: &TaskSpec) -> BTreeMap<String, u32> {
    let mut out: BTreeMap<String, u32> = BTreeMap::new();
    fn note(out: &mut BTreeMap<String, u32>, text: &str) {
        if let Some((sheet, _)) = split_ref(text) {
            if !sheet.is_empty() {
                *out.entry(sheet).or_default() += 1;
            }
        }
    }
    for check in &task.checks {
        match check {
            Check::CellDisplays { at, .. }
            | Check::CellNumber { at, .. }
            | Check::CellFormula { at, .. } => note(&mut out, at),
            Check::RangeFilled { range }
            | Check::NoErrors { range }
            | Check::SumEquals { range, .. } => note(&mut out, range),
            Check::SumsMatch { left, right, .. } => {
                note(&mut out, left);
                note(&mut out, right);
            }
            Check::Unchanged { ranges } => {
                for r in ranges {
                    note(&mut out, r);
                }
            }
            Check::SheetsExist { names } => {
                for n in names {
                    *out.entry(n.clone()).or_default() += 1;
                }
            }
            Check::NameRefersTo { refers_to, .. } => note(&mut out, refers_to),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Action;

    fn edit(e: &mut Engine, sheet: &str, a1: &str, input: &str) {
        e.apply(&Action::CellEdit {
            sheet: sheet.into(),
            addr: CellAddr::parse_a1(a1).unwrap(),
            input: input.into(),
        })
        .unwrap();
    }

    /// Qty, Price, and an empty Total column waiting to be filled.
    fn start() -> Engine {
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
            edit(&mut e, "Sheet1", a1, input);
        }
        e.clear_history();
        e
    }

    fn spec(checks: Vec<Check>) -> TaskSpec {
        TaskSpec {
            id: "t".into(),
            instruction: "fill in the totals".into(),
            initial_snapshot: SnapshotId("x".into()),
            checks,
            start_sheet: None,
            max_steps: 64,
            origin: None,
        }
    }

    fn filled() -> Engine {
        let mut e = start();
        for row in 2..=4 {
            edit(
                &mut e,
                "Sheet1",
                &format!("D{row}"),
                &format!("=B{row}*C{row}"),
            );
        }
        e
    }

    #[test]
    fn a_correctly_filled_column_passes_every_way_of_asking() {
        let done = filled();
        let task = spec(vec![
            Check::RangeFilled {
                range: "D2:D4".into(),
            },
            Check::CellFormula {
                at: "D3".into(),
                expect: "=B3*C3".into(),
            },
            Check::CellNumber {
                at: "D2".into(),
                expect: 10.0,
                tolerance: 0.0,
            },
            Check::SumEquals {
                range: "D2:D4".into(),
                expect: 17.0,
                tolerance: 0.0,
            },
            Check::NoErrors {
                range: "A1:D4".into(),
            },
            Check::Unchanged {
                ranges: vec!["A1:C4".into()],
            },
        ]);
        let result = grade(&task, &done, &start());
        assert!(
            result.passed,
            "correct work was marked wrong: {:?}",
            result.failures().collect::<Vec<_>>()
        );
    }

    #[test]
    fn hard_coded_literals_do_not_pass_a_fill_check() {
        // The failure this check exists for: the right numbers, no formulas.
        // Grading on values alone would call this a success and train a
        // policy to paste constants into computed columns.
        let mut e = start();
        for (a1, v) in [("D2", "10"), ("D3", "4.5"), ("D4", "2.5")] {
            edit(&mut e, "Sheet1", a1, v);
        }
        let task = spec(vec![Check::RangeFilled {
            range: "D2:D4".into(),
        }]);
        assert!(!grade(&task, &e, &start()).passed);
    }

    #[test]
    fn the_same_formula_repeated_without_shifting_fails_a_fill_check() {
        // =B2*C2 in all three rows gives the wrong answer twice, but every
        // cell does hold a formula. Only shape comparison catches it.
        let mut e = start();
        for row in 2..=4 {
            edit(&mut e, "Sheet1", &format!("D{row}"), "=B2*C2");
        }
        let task = spec(vec![Check::RangeFilled {
            range: "D2:D4".into(),
        }]);
        let result = grade(&task, &e, &start());
        assert!(!result.passed, "an unshifted fill was accepted");
    }

    #[test]
    fn a_formula_check_accepts_the_shape_not_the_letters() {
        // The check names `=B2*C2`; the cell it names is D3. What must match
        // is the shape at that cell, so a task written once still grades a
        // table that moved down a row.
        let done = filled();
        let task = spec(vec![Check::CellFormula {
            at: "D3".into(),
            expect: "=B3*C3".into(),
        }]);
        assert!(grade(&task, &done, &start()).passed);
    }

    #[test]
    fn a_formula_check_rejects_a_different_formula_that_reads_up_and_left() {
        // The regression that matters: normalizing by shifting toward the
        // origin turned every up-and-left reference into #REF!, so
        // `=B3*C3` and `=A3+B2` both normalized to the same string and this
        // check passed anything.
        let mut e = start();
        edit(&mut e, "Sheet1", "D3", "=A3+B3");
        let task = spec(vec![Check::CellFormula {
            at: "D3".into(),
            expect: "=B3*C3".into(),
        }]);
        let result = grade(&task, &e, &start());
        assert!(
            !result.passed,
            "a different formula passed: {:?}",
            result.checks[0].detail
        );
    }

    #[test]
    fn an_absolute_reference_is_not_the_same_shape_as_a_relative_one() {
        // `=$B$2*C2` filled down is a different intent from `=B2*C2`, and a
        // grader that could not tell would accept a column that is wrong in
        // every row but the first.
        let mut e = start();
        edit(&mut e, "Sheet1", "D2", "=$B$2*C2");
        let task = spec(vec![Check::CellFormula {
            at: "D2".into(),
            expect: "=B2*C2".into(),
        }]);
        assert!(!grade(&task, &e, &start()).passed);
    }

    #[test]
    fn a_sum_that_is_right_to_fifteen_digits_passes_without_a_tolerance() {
        // 0.1 + 0.2 is 0.30000000000000004. Failing that would train a
        // policy away from the correct answer.
        let mut e = Engine::new();
        edit(&mut e, "Sheet1", "A1", "0.1");
        edit(&mut e, "Sheet1", "A2", "0.2");
        edit(&mut e, "Sheet1", "A3", "=A1+A2");
        let task = spec(vec![Check::CellNumber {
            at: "A3".into(),
            expect: 0.3,
            tolerance: 0.0,
        }]);
        let result = grade(&task, &e, &Engine::new());
        assert!(result.passed, "{}", result.checks[0].detail);
    }

    #[test]
    fn a_number_that_is_merely_close_still_fails_without_a_tolerance() {
        // The other half of the previous test: floating-point slack must not
        // become a free pass for an answer that is actually wrong.
        let mut e = Engine::new();
        edit(&mut e, "Sheet1", "A1", "0.3001");
        let task = spec(vec![Check::CellNumber {
            at: "A1".into(),
            expect: 0.3,
            tolerance: 0.0,
        }]);
        assert!(!grade(&task, &e, &Engine::new()).passed);
        let lenient = spec(vec![Check::CellNumber {
            at: "A1".into(),
            expect: 0.3,
            tolerance: 0.001,
        }]);
        assert!(grade(&lenient, &e, &Engine::new()).passed);
    }

    #[test]
    fn changing_a_cell_the_task_forbade_fails_even_when_the_answer_is_right() {
        let mut e = filled();
        edit(&mut e, "Sheet1", "B2", "40");
        let task = spec(vec![
            Check::RangeFilled {
                range: "D2:D4".into(),
            },
            Check::Unchanged {
                ranges: vec!["A1:C4".into()],
            },
        ]);
        let result = grade(&task, &e, &start());
        assert!(!result.passed);
        let failed: Vec<&CheckResult> = result.failures().collect();
        assert_eq!(failed.len(), 1);
        assert!(failed[0].detail.contains("B2"), "{}", failed[0].detail);
    }

    #[test]
    fn work_confined_to_the_cells_the_task_named_reports_no_incidental_changes() {
        // The regression this pins: unqualified check addresses were stored
        // against an empty sheet name while the diff reported "Sheet1", so
        // nothing ever matched and clean work looked like vandalism.
        let task = spec(vec![Check::RangeFilled {
            range: "D2:D4".into(),
        }]);
        let result = grade(&task, &filled(), &start());
        assert_eq!(
            result.incidental_changes, 0,
            "the three cells the task asked for were counted as collateral"
        );
    }

    #[test]
    fn a_cell_nobody_asked_about_is_counted_as_incidental() {
        let mut e = filled();
        edit(&mut e, "Sheet1", "F9", "scratch");
        let task = spec(vec![Check::RangeFilled {
            range: "D2:D4".into(),
        }]);
        let result = grade(&task, &e, &start());
        assert!(result.passed, "the task itself was still done");
        assert_eq!(
            result.incidental_changes, 1,
            "passing while touching unrelated cells has to be visible"
        );
    }

    #[test]
    fn a_recalculated_cell_counts_as_a_change() {
        // Editing B2 changes D2 without anyone typing in D2. A diff that
        // only saw typed cells would miss the blast radius entirely.
        let before = filled();
        let mut after = before.clone();
        edit(&mut after, "Sheet1", "B2", "40");
        let changed: Vec<String> = changed_cells(&before, &after)
            .into_iter()
            .map(|(s, a)| format!("{s}!{}", a.to_a1()))
            .collect();
        assert!(changed.contains(&"Sheet1!B2".to_string()));
        assert!(changed.contains(&"Sheet1!D2".to_string()), "{changed:?}");
    }

    #[test]
    fn debits_equal_credits_is_expressible_and_catches_an_imbalance() {
        let mut e = Engine::new();
        for (a1, v) in [
            ("A1", "Debit"),
            ("B1", "Credit"),
            ("A2", "100"),
            ("B2", "60"),
            ("A3", "50"),
            ("B3", "90"),
        ] {
            edit(&mut e, "Sheet1", a1, v);
        }
        let balanced = spec(vec![Check::SumsMatch {
            left: "A2:A3".into(),
            right: "B2:B3".into(),
            tolerance: 0.0,
        }]);
        assert!(grade(&balanced, &e, &Engine::new()).passed);

        edit(&mut e, "Sheet1", "B3", "89");
        let result = grade(&balanced, &e, &Engine::new());
        assert!(!result.passed);
        assert!(
            result.checks[0].detail.contains("149"),
            "{}",
            result.checks[0].detail
        );
    }

    #[test]
    fn a_check_naming_an_address_that_does_not_exist_fails_rather_than_passing_vacuously() {
        // The dangerous default. A grader that shrugs at a typo in its own
        // spec reports success for a task nobody verified.
        let task = spec(vec![
            Check::CellNumber {
                at: "not an address".into(),
                expect: 1.0,
                tolerance: 0.0,
            },
            Check::NoErrors {
                range: "also not a range".into(),
            },
            Check::SumEquals {
                range: "??".into(),
                expect: 1.0,
                tolerance: 0.0,
            },
        ]);
        let result = grade(&task, &start(), &start());
        assert!(!result.passed);
        assert_eq!(result.failures().count(), 3);
    }

    #[test]
    fn an_empty_range_does_not_pass_a_fill_check() {
        // Nothing there is not "filled", however tempting the vacuous truth.
        let task = spec(vec![Check::RangeFilled {
            range: "D2:D4".into(),
        }]);
        assert!(!grade(&task, &start(), &start()).passed);
    }

    #[test]
    fn every_failing_check_says_what_was_actually_there() {
        let task = spec(vec![Check::CellNumber {
            at: "D2".into(),
            expect: 10.0,
            tolerance: 0.0,
        }]);
        let result = grade(&task, &start(), &start());
        assert!(!result.passed);
        assert!(
            !result.checks[0].detail.is_empty(),
            "a refusal with no explanation cannot be debugged"
        );
    }

    #[test]
    fn a_task_round_trips_through_jsonl() {
        let task = spec(vec![
            Check::RangeFilled {
                range: "D2:D4".into(),
            },
            Check::Unchanged {
                ranges: vec!["A1:C4".into()],
            },
        ]);
        let line = serde_json::to_string(&task).unwrap();
        assert!(!line.contains('\n'), "a JSONL record must be one line");
        let back: TaskSpec = serde_json::from_str(&line).unwrap();
        assert_eq!(task, back);
    }

    #[test]
    fn a_task_records_which_sheets_its_checks_depend_on() {
        // Augmentation renames sheets; it needs to know which checks follow.
        let task = spec(vec![
            Check::SumEquals {
                range: "Ledger!D2:D4".into(),
                expect: 17.0,
                tolerance: 0.0,
            },
            Check::SheetsExist {
                names: vec!["Summary".into()],
            },
        ]);
        let sheets = sheets_mentioned(&task);
        assert_eq!(sheets.get("Ledger"), Some(&1));
        assert_eq!(sheets.get("Summary"), Some(&1));
    }
}
