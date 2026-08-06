//! The thing that says no.
//!
//! Between compiling a step and running it, every action is checked against
//! the scope the step declared and against a set of limits the task supplies.
//! The checks are cheap and they are all of the form "this action would do
//! something nobody asked for":
//!
//! * write outside the range the step said it would touch
//! * make a structural change — insert or delete rows, columns or sheets —
//!   that the task did not ask for
//! * write a formula containing a reference that cannot resolve
//! * overwrite a formula with a literal
//! * change more cells, counting recalculation, than the limit allows
//!
//! None of these is a correctness check. A plan can pass every one of them
//! and still be wrong, and the grader is what catches that. What the
//! validator catches is a different and more dangerous class: work that is
//! *plausible* and destroys something. An agent that fills the right column
//! and also flattens the one next to it scores well on task completion and
//! is worse than useless, and by the time a grader notices, the damage is
//! in the workbook.
//!
//! The blast-radius estimate counts recalculation deliberately. Editing one
//! input cell that four hundred formulas read is a four-hundred-cell change,
//! and an agent allowed to make it because "it only wrote one cell" has been
//! measured by the wrong number.

use std::collections::HashSet;

use engine::{Action, CellAddr, CellKey, Engine, RangeAddr};
use serde::{Deserialize, Serialize};

use crate::compile::Compiled;

/// What a step is allowed to do.
///
/// Set by the task rather than by the agent — a policy that could widen its
/// own limits does not have limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// The most cells one step may change, counting cells that only change
    /// by recalculation.
    pub max_cells: u32,
    /// Whether rows or columns may be inserted or deleted.
    pub allow_structural: bool,
    /// Whether sheets may be added, renamed or deleted. Separate from
    /// `allow_structural` because deleting a sheet is the single most
    /// destructive thing in the vocabulary and nothing should enable it by
    /// accident.
    pub allow_sheet_changes: bool,
    /// Whether an existing formula may be replaced by a literal.
    ///
    /// Off by default, because it is what a policy does when it computes the
    /// answer itself instead of writing the formula — the output looks
    /// right, the sheet stops working, and no value-based check notices.
    pub allow_overwriting_formulas: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_cells: 5_000,
            allow_structural: false,
            allow_sheet_changes: false,
            allow_overwriting_formulas: false,
        }
    }
}

impl Limits {
    /// Limits for a task that genuinely is about restructuring the sheet.
    pub fn permissive() -> Self {
        Limits {
            max_cells: 200_000,
            allow_structural: true,
            allow_sheet_changes: true,
            allow_overwriting_formulas: true,
        }
    }
}

/// Why an action was refused. Each names the action and what it would have
/// done, because a refusal a policy cannot learn from is just a wall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "refusal", rename_all = "snake_case")]
pub enum Refusal {
    /// Wrote somewhere the step did not say it would.
    OutsideScope {
        action: usize,
        at: String,
        declared: Vec<String>,
    },
    /// A structural or sheet-level change the task did not ask for.
    NotPermitted { action: usize, what: String },
    /// A formula that cannot resolve: a `#REF!`, an unknown sheet, or a cell
    /// referring to itself.
    InvalidReference {
        action: usize,
        at: String,
        formula: String,
        why: String,
    },
    /// Replacing a formula with a literal.
    OverwritesFormula {
        action: usize,
        at: String,
        existing: String,
    },
    /// More cells than allowed, counting recalculation.
    TooManyCells { estimated: u32, limit: u32 },
}

impl Refusal {
    pub fn explain(&self) -> String {
        match self {
            Refusal::OutsideScope { at, declared, .. } => {
                format!("{at} is outside the declared scope {declared:?}")
            }
            Refusal::NotPermitted { what, .. } => format!("{what} was not asked for"),
            Refusal::InvalidReference {
                at, formula, why, ..
            } => format!("{at} would hold {formula:?}, which {why}"),
            Refusal::OverwritesFormula { at, existing, .. } => {
                format!("{at} already holds the formula {existing:?}")
            }
            Refusal::TooManyCells { estimated, limit } => {
                format!("would change about {estimated} cells, over the limit of {limit}")
            }
        }
    }
}

/// What a step would cost if it ran.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    /// Cells written directly.
    pub written: u32,
    /// Cells that would change, including by recalculation. The number the
    /// limit is measured against.
    pub touched: u32,
    /// True when the dependency walk hit its bound, so `touched` is a floor
    /// rather than an exact figure. Said out loud, because a limit checked
    /// against a silently truncated estimate is not a limit.
    pub truncated: bool,
}

/// How far the blast-radius walk will go before it gives up and says so.
const RADIUS_BUDGET: usize = 100_000;

/// Check a compiled step. `Ok` carries what it would cost.
pub fn validate(
    compiled: &Compiled,
    engine: &Engine,
    limits: &Limits,
) -> Result<Estimate, Vec<Refusal>> {
    let mut refusals = Vec::new();
    let mut seeds: Vec<CellKey> = Vec::new();
    let mut written = 0u32;

    for (i, action) in compiled.actions.iter().enumerate() {
        match action {
            Action::CellEdit { sheet, addr, input } => {
                check_scope(&mut refusals, compiled, i, sheet, *addr);
                check_formula(&mut refusals, engine, i, sheet, *addr, input);
                check_not_clobbering(&mut refusals, engine, limits, i, sheet, *addr, input);
                written += 1;
                if let Some(key) = key_of(engine, sheet, *addr) {
                    seeds.push(key);
                }
            }
            Action::CellClear { sheet, addr } => {
                check_scope(&mut refusals, compiled, i, sheet, *addr);
                written += 1;
                if let Some(key) = key_of(engine, sheet, *addr) {
                    seeds.push(key);
                }
            }
            Action::RangeClear { sheet, range }
            | Action::FillApply {
                sheet,
                target: range,
                ..
            }
            | Action::FormatApply { sheet, range, .. }
            | Action::FormatClear { sheet, range }
            | Action::SortApply { sheet, range, .. } => {
                check_range_scope(&mut refusals, compiled, i, sheet, *range);
                written += range.cell_count().min(u32::MAX as u64) as u32;
                for addr in cells_of(*range) {
                    if let Some(key) = key_of(engine, sheet, addr) {
                        seeds.push(key);
                    }
                }
            }
            // A filter hides rows. It changes no value, so it has no blast
            // radius — but it is still scoped, because hiding rows outside
            // the table the step named is not what the step said it would do.
            Action::FilterApply { sheet, spec } => {
                check_range_scope(&mut refusals, compiled, i, sheet, spec.range);
            }
            Action::FilterClear { .. } => {}
            Action::RowInsert { .. }
            | Action::RowDelete { .. }
            | Action::ColInsert { .. }
            | Action::ColDelete { .. } => {
                if !limits.allow_structural {
                    refusals.push(Refusal::NotPermitted {
                        action: i,
                        what: describe(action),
                    });
                }
            }
            Action::SheetAdd { .. } | Action::SheetRename { .. } | Action::SheetDelete { .. } => {
                if !limits.allow_sheet_changes {
                    refusals.push(Refusal::NotPermitted {
                        action: i,
                        what: describe(action),
                    });
                }
            }
            // Undo and redo would let a step reach back past its own scope
            // into whatever came before it, which is not something a plan
            // step is ever entitled to do.
            Action::Undo | Action::Redo => {
                refusals.push(Refusal::NotPermitted {
                    action: i,
                    what: describe(action),
                });
            }
            _ => {}
        }
    }

    let (touched, truncated) = blast_radius(engine, &seeds);
    let touched = touched.max(written);
    if touched > limits.max_cells {
        refusals.push(Refusal::TooManyCells {
            estimated: touched,
            limit: limits.max_cells,
        });
    }

    if refusals.is_empty() {
        Ok(Estimate {
            written,
            touched,
            truncated,
        })
    } else {
        Err(refusals)
    }
}

fn key_of(engine: &Engine, sheet: &str, addr: CellAddr) -> Option<CellKey> {
    Some(CellKey {
        sheet: engine.wb.sheet_id_by_name(sheet)?,
        addr,
    })
}

fn cells_of(r: RangeAddr) -> impl Iterator<Item = CellAddr> {
    (r.start.row..=r.end.row)
        .flat_map(move |row| (r.start.col..=r.end.col).map(move |col| CellAddr::new(row, col)))
}

fn check_scope(out: &mut Vec<Refusal>, compiled: &Compiled, i: usize, sheet: &str, addr: CellAddr) {
    if compiled
        .scope
        .iter()
        .any(|(s, r)| s == sheet && r.contains(addr))
    {
        return;
    }
    out.push(Refusal::OutsideScope {
        action: i,
        at: format!("{sheet}!{}", addr.to_a1()),
        declared: declared(compiled),
    });
}

fn check_range_scope(
    out: &mut Vec<Refusal>,
    compiled: &Compiled,
    i: usize,
    sheet: &str,
    range: RangeAddr,
) {
    let inside = compiled
        .scope
        .iter()
        .any(|(s, r)| s == sheet && r.contains(range.start) && r.contains(range.end));
    if inside {
        return;
    }
    out.push(Refusal::OutsideScope {
        action: i,
        at: format!("{sheet}!{}", range.to_a1()),
        declared: declared(compiled),
    });
}

fn declared(compiled: &Compiled) -> Vec<String> {
    compiled
        .scope
        .iter()
        .map(|(s, r)| format!("{s}!{}", r.to_a1()))
        .collect()
}

/// A formula whose references cannot resolve.
///
/// Checked before it runs rather than after, because the engine will happily
/// store `=#REF!+1` and answer `#REF!` forever — a cell that looks filled in
/// and is broken, which is worse than an empty one.
fn check_formula(
    out: &mut Vec<Refusal>,
    engine: &Engine,
    i: usize,
    sheet: &str,
    addr: CellAddr,
    input: &str,
) {
    let Some(body) = input.strip_prefix('=') else {
        return;
    };
    let at = format!("{sheet}!{}", addr.to_a1());
    let Ok(ast) = engine::parser::parse_formula(body) else {
        out.push(Refusal::InvalidReference {
            action: i,
            at,
            formula: input.to_string(),
            why: "does not parse".into(),
        });
        return;
    };

    let mut why: Option<String> = None;
    ast.visit_refs(&mut |r| {
        let (named_sheet, hits_self) = match r {
            engine::ast::RefVisit::Cell(c) => {
                (c.sheet.clone(), c.sheet.is_none() && c.r.addr() == addr)
            }
            engine::ast::RefVisit::Range(rr) => (
                rr.sheet.clone(),
                rr.sheet.is_none() && RangeAddr::new(rr.start.addr(), rr.end.addr()).contains(addr),
            ),
        };
        if let Some(name) = &named_sheet {
            if engine.wb.sheet_by_name(name).is_none() && why.is_none() {
                why = Some(format!(
                    "refers to a sheet named {name:?} that does not exist"
                ));
            }
        }
        if hits_self && why.is_none() {
            why = Some("refers to the cell it is written in".into());
        }
    });

    // `#REF!` reaches the AST as an error literal, which is how a reference
    // that was rewritten out of existence survives being written back.
    if why.is_none() && contains_ref_error(&ast) {
        why = Some("contains a #REF!".into());
    }

    if let Some(why) = why {
        out.push(Refusal::InvalidReference {
            action: i,
            at,
            formula: input.to_string(),
            why,
        });
    }
}

fn contains_ref_error(e: &engine::ast::Expr) -> bool {
    use engine::ast::Expr;
    match e {
        Expr::Error(engine::ErrorKind::Ref) => true,
        Expr::Func(_, args) => args.iter().any(contains_ref_error),
        Expr::Binary(_, l, r) => contains_ref_error(l) || contains_ref_error(r),
        Expr::Neg(x) | Expr::Pos(x) | Expr::Percent(x) => contains_ref_error(x),
        _ => false,
    }
}

/// Replacing a formula with a literal.
///
/// The failure this exists for: a policy that computes the answer itself and
/// pastes the number. Every value-based check passes, the column stops
/// updating, and nobody finds out until the inputs change.
fn check_not_clobbering(
    out: &mut Vec<Refusal>,
    engine: &Engine,
    limits: &Limits,
    i: usize,
    sheet: &str,
    addr: CellAddr,
    input: &str,
) {
    if limits.allow_overwriting_formulas || input.starts_with('=') {
        return;
    }
    let Some(existing) = engine
        .wb
        .sheet_by_name(sheet)
        .and_then(|s| s.cells.get(&addr))
        .filter(|c| c.is_formula())
    else {
        return;
    };
    out.push(Refusal::OverwritesFormula {
        action: i,
        at: format!("{sheet}!{}", addr.to_a1()),
        existing: existing.input(),
    });
}

/// How many cells a set of edits would change, following the dependency
/// graph outward.
///
/// A floor, not a ceiling: it counts cells that *read* the seeds, which is
/// what recalculation will visit. A dependent whose value happens not to
/// change is still counted, because the alternative is evaluating the
/// workbook to find out — and this runs before every step.
fn blast_radius(engine: &Engine, seeds: &[CellKey]) -> (u32, bool) {
    let mut seen: HashSet<CellKey> = seeds.iter().copied().collect();
    let mut frontier: Vec<CellKey> = seeds.to_vec();
    let mut truncated = false;
    while let Some(key) = frontier.pop() {
        if seen.len() >= RADIUS_BUDGET {
            truncated = true;
            break;
        }
        for dependent in engine.dependents_of(key) {
            if seen.insert(dependent) {
                frontier.push(dependent);
            }
        }
    }
    (seen.len() as u32, truncated)
}

fn describe(action: &Action) -> String {
    match action {
        Action::RowInsert { sheet, at, count } => {
            format!("inserting {count} row(s) at {at} on {sheet}")
        }
        Action::RowDelete { sheet, at, count } => {
            format!("deleting {count} row(s) at {at} on {sheet}")
        }
        Action::ColInsert { sheet, at, count } => {
            format!("inserting {count} column(s) at {at} on {sheet}")
        }
        Action::ColDelete { sheet, at, count } => {
            format!("deleting {count} column(s) at {at} on {sheet}")
        }
        Action::SheetAdd { name } => format!("adding the sheet {name}"),
        Action::SheetRename { from, to } => format!("renaming {from} to {to}"),
        Action::SheetDelete { name } => format!("deleting the sheet {name}"),
        Action::Undo => "undoing".into(),
        Action::Redo => "redoing".into(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::Subject;

    fn edit(sheet: &str, a1: &str, input: &str) -> Action {
        Action::CellEdit {
            sheet: sheet.into(),
            addr: CellAddr::parse_a1(a1).unwrap(),
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
        ] {
            e.apply(&edit("Sheet1", a1, input)).unwrap();
        }
        e
    }

    fn scoped(actions: Vec<Action>, scope: &str) -> Compiled {
        Compiled {
            actions,
            scope: vec![("Sheet1".into(), RangeAddr::parse_a1(scope).unwrap())],
            subject: None,
        }
    }

    fn only(r: Result<Estimate, Vec<Refusal>>) -> Refusal {
        let mut v = r.unwrap_err();
        assert_eq!(v.len(), 1, "expected exactly one refusal: {v:?}");
        v.remove(0)
    }

    #[test]
    fn work_inside_the_declared_scope_passes() {
        let e = ledger();
        let c = scoped(
            vec![
                edit("Sheet1", "D1", "Total"),
                edit("Sheet1", "D2", "=B2*C2"),
            ],
            "D1:D3",
        );
        let estimate = validate(&c, &e, &Limits::default()).unwrap();
        assert_eq!(estimate.written, 2);
        assert!(!estimate.truncated);
    }

    #[test]
    fn one_cell_outside_the_scope_is_refused_and_named() {
        // The failure mode this whole file exists for: a step that does its
        // job and also touches something nobody asked about.
        let e = ledger();
        let c = scoped(
            vec![
                edit("Sheet1", "D2", "=B2*C2"),
                edit("Sheet1", "A2", "Screw"),
            ],
            "D1:D3",
        );
        match only(validate(&c, &e, &Limits::default())) {
            Refusal::OutsideScope { at, action, .. } => {
                assert_eq!(at, "Sheet1!A2");
                assert_eq!(action, 1);
            }
            other => panic!("wrong refusal: {other:?}"),
        }
    }

    #[test]
    fn a_range_action_must_lie_entirely_inside_the_scope() {
        // Half in is out. A fill that starts inside the declared column and
        // runs off the end of it is exactly the accident worth catching.
        let e = ledger();
        let c = scoped(
            vec![Action::FillApply {
                sheet: "Sheet1".into(),
                source: RangeAddr::parse_a1("D2:D2").unwrap(),
                target: RangeAddr::parse_a1("D2:D9").unwrap(),
            }],
            "D1:D3",
        );
        assert!(matches!(
            only(validate(&c, &e, &Limits::default())),
            Refusal::OutsideScope { .. }
        ));
    }

    #[test]
    fn deleting_rows_is_refused_unless_the_task_asked_for_it() {
        let e = ledger();
        let c = scoped(
            vec![Action::RowDelete {
                sheet: "Sheet1".into(),
                at: 1,
                count: 2,
            }],
            "A1:D9",
        );
        assert!(matches!(
            only(validate(&c, &e, &Limits::default())),
            Refusal::NotPermitted { .. }
        ));
        assert!(validate(&c, &e, &Limits::permissive()).is_ok());
    }

    #[test]
    fn deleting_a_sheet_needs_its_own_permission() {
        // The most destructive thing in the vocabulary. Enabling structural
        // edits must not quietly enable this too.
        let e = ledger();
        let c = scoped(
            vec![Action::SheetDelete {
                name: "Sheet1".into(),
            }],
            "A1:D9",
        );
        let structural_only = Limits {
            allow_structural: true,
            ..Limits::default()
        };
        assert!(matches!(
            only(validate(&c, &e, &structural_only)),
            Refusal::NotPermitted { .. }
        ));
    }

    #[test]
    fn undo_is_never_a_step_a_plan_may_take() {
        // It would reach back past the step's own scope into whatever came
        // before it, which no scope declaration can describe.
        let e = ledger();
        let c = scoped(vec![Action::Undo], "A1:D9");
        assert!(matches!(
            only(validate(&c, &e, &Limits::permissive())),
            Refusal::NotPermitted { .. }
        ));
    }

    #[test]
    fn a_formula_naming_a_sheet_that_does_not_exist_is_refused() {
        let e = ledger();
        let c = scoped(vec![edit("Sheet1", "D2", "=Summary!A1*2")], "D1:D3");
        match only(validate(&c, &e, &Limits::default())) {
            Refusal::InvalidReference { why, .. } => assert!(why.contains("Summary"), "{why}"),
            other => panic!("wrong refusal: {other:?}"),
        }
    }

    #[test]
    fn a_formula_that_refers_to_its_own_cell_is_refused_before_it_becomes_a_cycle() {
        let e = ledger();
        let c = scoped(vec![edit("Sheet1", "D2", "=SUM(D1:D3)")], "D1:D3");
        match only(validate(&c, &e, &Limits::default())) {
            Refusal::InvalidReference { why, .. } => assert!(
                why.contains("itself") || why.contains("written in"),
                "{why}"
            ),
            other => panic!("wrong refusal: {other:?}"),
        }
    }

    #[test]
    fn a_formula_carrying_a_ref_error_is_refused() {
        // The engine would store it and answer #REF! forever: a cell that
        // looks filled in and is broken, which is worse than an empty one.
        let e = ledger();
        let c = scoped(vec![edit("Sheet1", "D2", "=#REF!+1")], "D1:D3");
        assert!(matches!(
            only(validate(&c, &e, &Limits::default())),
            Refusal::InvalidReference { .. }
        ));
    }

    #[test]
    fn an_ordinary_cross_sheet_formula_is_fine() {
        let mut e = ledger();
        e.apply(&Action::SheetAdd {
            name: "Summary".into(),
        })
        .unwrap();
        let c = scoped(vec![edit("Sheet1", "D2", "=Summary!A1*2")], "D1:D3");
        assert!(validate(&c, &e, &Limits::default()).is_ok());
    }

    #[test]
    fn replacing_a_formula_with_a_literal_is_refused_by_default() {
        // A policy that computes the answer itself and pastes the number
        // passes every value check while quietly breaking the sheet.
        let mut e = ledger();
        e.apply(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        let c = scoped(vec![edit("Sheet1", "D2", "10")], "D1:D3");
        match only(validate(&c, &e, &Limits::default())) {
            Refusal::OverwritesFormula { existing, .. } => assert_eq!(existing, "=B2*C2"),
            other => panic!("wrong refusal: {other:?}"),
        }
        assert!(validate(&c, &e, &Limits::permissive()).is_ok());
    }

    #[test]
    fn replacing_a_formula_with_a_better_formula_is_fine() {
        let mut e = ledger();
        e.apply(&edit("Sheet1", "D2", "=B2*C2")).unwrap();
        let c = scoped(vec![edit("Sheet1", "D2", "=B2*C2*1.2")], "D1:D3");
        assert!(validate(&c, &e, &Limits::default()).is_ok());
    }

    #[test]
    fn the_impact_estimate_counts_cells_that_only_recalculate() {
        // Editing one input that four hundred formulas read is a
        // four-hundred-cell change. An agent measured on "it wrote one cell"
        // has been measured by the wrong number.
        let mut e = ledger();
        for row in 2..=3 {
            e.apply(&edit(
                "Sheet1",
                &format!("D{row}"),
                &format!("=B{row}*C{row}"),
            ))
            .unwrap();
        }
        e.apply(&edit("Sheet1", "D5", "=SUM(D2:D3)")).unwrap();

        let c = scoped(vec![edit("Sheet1", "B2", "40")], "B2:B2");
        let estimate = validate(&c, &e, &Limits::default()).unwrap();
        assert_eq!(estimate.written, 1);
        assert_eq!(
            estimate.touched, 3,
            "B2, the D2 that reads it, and the D5 that reads D2"
        );
    }

    #[test]
    fn a_step_over_the_cell_limit_is_refused() {
        let e = ledger();
        let actions: Vec<Action> = (1..=50)
            .map(|row| edit("Sheet1", &format!("D{row}"), "1"))
            .collect();
        let c = scoped(actions, "D1:D50");
        let tight = Limits {
            max_cells: 10,
            ..Limits::default()
        };
        match only(validate(&c, &e, &tight)) {
            Refusal::TooManyCells { estimated, limit } => {
                assert_eq!(estimated, 50);
                assert_eq!(limit, 10);
            }
            other => panic!("wrong refusal: {other:?}"),
        }
    }

    #[test]
    fn every_refusal_can_be_explained_to_whoever_has_to_fix_it() {
        let e = ledger();
        let c = scoped(vec![edit("Sheet1", "Z9", "1")], "D1:D3");
        let refusal = only(validate(&c, &e, &Limits::default()));
        let text = refusal.explain();
        assert!(text.contains("Z9"), "{text}");
        assert!(text.contains("D1:D3"), "{text}");
    }

    #[test]
    fn several_problems_are_all_reported_not_just_the_first() {
        // A policy told about one problem at a time takes one round trip per
        // problem, and a human reviewing a refusal wants the whole list.
        let e = ledger();
        let c = scoped(
            vec![
                edit("Sheet1", "Z9", "1"),
                edit("Sheet1", "D2", "=Nowhere!A1"),
            ],
            "D1:D3",
        );
        let refusals = validate(&c, &e, &Limits::default()).unwrap_err();
        assert_eq!(refusals.len(), 2, "{refusals:?}");
    }

    #[test]
    fn a_compiled_step_from_the_real_compiler_validates() {
        // The two halves have to agree about what a scope is; a test of each
        // alone would not notice if they drifted.
        let e = ledger();
        let obs = env::observe::observe(&e, "Sheet1", "A1", &[], "h".into());
        let subject: Subject = crate::compile(
            &crate::Step::LocateTable {
                sheet: None,
                must_have: vec!["Qty".into()],
            },
            &e,
            &obs,
            None,
        )
        .unwrap()
        .subject
        .unwrap();
        let compiled = crate::compile(
            &crate::Step::CreateDerivedColumn {
                header: "Total".into(),
                at: crate::ColumnRef::NextFree,
                formula: crate::FormulaTemplate::new("={Qty}*{Price}"),
                rows: crate::RowRange::TableBody,
            },
            &e,
            &obs,
            Some(&subject),
        )
        .unwrap();
        assert!(validate(&compiled, &e, &Limits::default()).is_ok());
    }
}
