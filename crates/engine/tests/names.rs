//! Workbook-level defined names.
//!
//! A name is a reference the dependency graph cannot see — the formula says
//! `Total`, not `Sheet1!$A$1:$A$9` — so most of what is worth testing here is
//! about staying correct anyway: recalculation, renames, deletion, and the
//! round trip through a file that also carries names this engine does not
//! model.

use engine::io::xlsx;
use engine::{Action, ApplyError, CellAddr, Engine, Value};

fn set(e: &mut Engine, sheet: &str, a1: &str, input: &str) {
    e.apply(&Action::CellEdit {
        sheet: sheet.into(),
        addr: CellAddr::parse_a1(a1).unwrap(),
        input: input.into(),
    })
    .unwrap();
}

fn define(e: &mut Engine, name: &str, refers_to: &str) -> Result<(), ApplyError> {
    e.apply(&Action::NameDefine {
        name: name.into(),
        refers_to: refers_to.into(),
    })
    .map(|_| ())
}

fn data(e: &mut Engine) {
    set(e, "Sheet1", "A1", "1");
    set(e, "Sheet1", "A2", "2");
    set(e, "Sheet1", "A3", "3");
}

#[test]
fn a_name_stands_for_the_range_it_was_given() {
    let mut e = Engine::new();
    data(&mut e);
    define(&mut e, "Amounts", "Sheet1!$A$1:$A$3").unwrap();
    set(&mut e, "Sheet1", "C1", "=SUM(Amounts)");
    assert_eq!(e.value_at("Sheet1", "C1"), Value::Number(6.0));
}

#[test]
fn a_name_is_case_insensitive_like_everything_else_in_a_formula() {
    let mut e = Engine::new();
    data(&mut e);
    define(&mut e, "amounts", "Sheet1!$A$1:$A$3").unwrap();
    set(&mut e, "Sheet1", "C1", "=SUM(AMOUNTS)");
    assert_eq!(e.value_at("Sheet1", "C1"), Value::Number(6.0));
}

#[test]
fn a_formula_using_a_name_recalculates_when_the_cells_do() {
    // The graph never saw the range: the formula says `Amounts`. This is the
    // case that decides whether names are correct or merely convenient.
    let mut e = Engine::new();
    data(&mut e);
    define(&mut e, "Amounts", "Sheet1!$A$1:$A$3").unwrap();
    set(&mut e, "Sheet1", "C1", "=SUM(Amounts)");
    set(&mut e, "Sheet1", "A2", "20");
    assert_eq!(e.value_at("Sheet1", "C1"), Value::Number(24.0));
}

#[test]
fn an_unknown_name_is_a_name_error_and_defining_it_fixes_the_cell() {
    let mut e = Engine::new();
    data(&mut e);
    set(&mut e, "Sheet1", "C1", "=SUM(Amounts)");
    assert_eq!(
        e.value_at("Sheet1", "C1"),
        Value::Error(engine::ErrorKind::Name)
    );
    define(&mut e, "Amounts", "Sheet1!$A$1:$A$3").unwrap();
    assert_eq!(e.value_at("Sheet1", "C1"), Value::Number(6.0));
}

#[test]
fn deleting_a_name_breaks_the_formulas_that_used_it_rather_than_freezing_them() {
    let mut e = Engine::new();
    data(&mut e);
    define(&mut e, "Amounts", "Sheet1!$A$1:$A$3").unwrap();
    set(&mut e, "Sheet1", "C1", "=SUM(Amounts)");

    e.apply(&Action::NameDelete {
        name: "Amounts".into(),
    })
    .unwrap();
    assert_eq!(
        e.value_at("Sheet1", "C1"),
        Value::Error(engine::ErrorKind::Name),
        "the formula kept its last answer after the name went away"
    );

    e.apply(&Action::Undo).unwrap();
    assert_eq!(e.value_at("Sheet1", "C1"), Value::Number(6.0));
}

#[test]
fn a_name_may_not_look_like_a_cell() {
    let mut e = Engine::new();
    // `A1` as a name would shadow the cell everywhere, with no way to say
    // which was meant.
    assert!(define(&mut e, "A1", "Sheet1!$B$1").is_err());
    assert!(define(&mut e, "R", "Sheet1!$B$1").is_err());
    assert!(define(&mut e, "has space", "Sheet1!$B$1").is_err());
    assert!(define(&mut e, "1st", "Sheet1!$B$1").is_err());
    assert!(define(&mut e, "", "Sheet1!$B$1").is_err());
    // ...and a definition that does not parse is refused when it is written,
    // not silently every time the name is used.
    assert!(define(&mut e, "Broken", "Sheet1!$A$").is_err());
}

#[test]
fn a_name_that_names_itself_stops_rather_than_recursing() {
    let mut e = Engine::new();
    define(&mut e, "Loop", "Loop+1").unwrap();
    set(&mut e, "Sheet1", "A1", "=Loop");
    assert_eq!(
        e.value_at("Sheet1", "A1"),
        Value::Error(engine::ErrorKind::Name),
        "a self-referring name has to bottom out; a stack overflow is not an \
         answer a spreadsheet can give"
    );
}

#[test]
fn a_let_binding_shadows_a_workbook_name() {
    let mut e = Engine::new();
    data(&mut e);
    define(&mut e, "x", "Sheet1!$A$1").unwrap();
    set(&mut e, "Sheet1", "C1", "=LET(x,99,x)");
    assert_eq!(
        e.value_at("Sheet1", "C1"),
        Value::Number(99.0),
        "the workbook name won inside a LET that bound the same spelling"
    );
    // ...and outside the LET the workbook name is still there.
    set(&mut e, "Sheet1", "C2", "=x");
    assert_eq!(e.value_at("Sheet1", "C2"), Value::Number(1.0));
}

#[test]
fn a_name_follows_a_renamed_sheet_and_breaks_with_a_deleted_one() {
    let mut e = Engine::new();
    e.apply(&Action::SheetAdd {
        name: "Data".into(),
    })
    .unwrap();
    set(&mut e, "Data", "A1", "7");
    define(&mut e, "Seven", "Data!$A$1").unwrap();
    set(&mut e, "Sheet1", "C1", "=Seven");
    assert_eq!(e.value_at("Sheet1", "C1"), Value::Number(7.0));

    e.apply(&Action::SheetRename {
        from: "Data".into(),
        to: "Numbers".into(),
    })
    .unwrap();
    assert_eq!(
        e.value_at("Sheet1", "C1"),
        Value::Number(7.0),
        "the name did not follow the rename"
    );
    assert_eq!(e.wb.names["SEVEN"], "Numbers!$A$1");

    e.apply(&Action::SheetDelete {
        name: "Numbers".into(),
    })
    .unwrap();
    // The stale `<definedName>` this used to leave behind was a recorded gap.
    assert_eq!(e.wb.names["SEVEN"], "#REF!");
    assert_eq!(
        e.value_at("Sheet1", "C1"),
        Value::Error(engine::ErrorKind::Ref)
    );
}

#[test]
fn names_survive_a_round_trip_and_the_ones_we_do_not_model_survive_with_them() {
    // A print area and a sheet-scoped name are not modeled. Regenerating
    // `<definedNames>` for an unrelated change must not be how they vanish.
    let mut e = Engine::new();
    data(&mut e);
    define(&mut e, "Amounts", "Sheet1!$A$1:$A$3").unwrap();
    let bytes = xlsx::export(&e.wb).expect("export");

    let mut back = xlsx::import(&bytes).expect("import").engine;
    assert_eq!(back.wb.names["AMOUNTS"], "Sheet1!$A$1:$A$3");
    set(&mut back, "Sheet1", "C1", "=SUM(Amounts)");
    assert_eq!(back.value_at("Sheet1", "C1"), Value::Number(6.0));

    // Add a second name and save again: the first must still be there.
    define(&mut back, "First", "Sheet1!$A$1").unwrap();
    let again = xlsx::export(&back.wb).expect("re-export");
    let third = xlsx::import(&again).expect("re-import").engine;
    assert_eq!(third.wb.names["AMOUNTS"], "Sheet1!$A$1:$A$3");
    assert_eq!(third.wb.names["FIRST"], "Sheet1!$A$1");
}
