//! M1 integration tests: recalc ordering, cycles, error semantics,
//! function behavior with Excel-verified expected values.

use engine::{Action, Engine, ErrorKind, Value};

fn set(e: &mut Engine, a1: &str, input: &str) {
    e.apply(&Action::CellEdit {
        sheet: "Sheet1".into(),
        addr: engine::CellAddr::parse_a1(a1).unwrap(),
        input: input.into(),
    })
    .unwrap();
}

fn val(e: &Engine, a1: &str) -> Value {
    e.value_at("Sheet1", a1)
}

fn num(e: &Engine, a1: &str) -> f64 {
    match val(e, a1) {
        Value::Number(n) => n,
        other => panic!("expected number at {a1}, got {other:?}"),
    }
}

fn err(e: &Engine, a1: &str) -> ErrorKind {
    match val(e, a1) {
        Value::Error(k) => k,
        other => panic!("expected error at {a1}, got {other:?}"),
    }
}

#[test]
fn literal_inputs() {
    let mut e = Engine::new();
    set(&mut e, "A1", "42");
    set(&mut e, "A2", "3.5");
    set(&mut e, "A3", "hello");
    set(&mut e, "A4", "TRUE");
    set(&mut e, "A5", "'123");
    set(&mut e, "A6", "50%");
    set(&mut e, "A7", "#N/A");
    assert_eq!(val(&e, "A1"), Value::Number(42.0));
    assert_eq!(val(&e, "A2"), Value::Number(3.5));
    assert_eq!(val(&e, "A3"), Value::Text("hello".into()));
    assert_eq!(val(&e, "A4"), Value::Bool(true));
    assert_eq!(val(&e, "A5"), Value::Text("123".into()));
    assert_eq!(val(&e, "A6"), Value::Number(0.5));
    assert_eq!(err(&e, "A7"), ErrorKind::NA);
    assert_eq!(val(&e, "Z99"), Value::Empty);
}

#[test]
fn basic_formula_and_recalc_chain() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "A2", "=A1+1");
    set(&mut e, "A3", "=A2+1");
    set(&mut e, "A4", "=A3+1");
    assert_eq!(num(&e, "A4"), 4.0);
    // Editing the root recalculates the whole chain incrementally.
    set(&mut e, "A1", "10");
    assert_eq!(num(&e, "A2"), 11.0);
    assert_eq!(num(&e, "A3"), 12.0);
    assert_eq!(num(&e, "A4"), 13.0);
}

#[test]
fn recalc_through_ranges() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "A2", "2");
    set(&mut e, "A3", "3");
    set(&mut e, "B1", "=SUM(A1:A10)");
    assert_eq!(num(&e, "B1"), 6.0);
    set(&mut e, "A9", "10");
    assert_eq!(num(&e, "B1"), 16.0);
    // Clearing a cell inside the watched range recalcs too.
    e.apply(&Action::CellClear {
        sheet: "Sheet1".into(),
        addr: engine::CellAddr::parse_a1("A9").unwrap(),
    })
    .unwrap();
    assert_eq!(num(&e, "B1"), 6.0);
}

#[test]
fn diamond_dependency_evaluates_once_correctly() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "B1", "=A1*2");
    set(&mut e, "B2", "=A1*3");
    set(&mut e, "C1", "=B1+B2");
    assert_eq!(num(&e, "C1"), 5.0);
    set(&mut e, "A1", "2");
    assert_eq!(num(&e, "C1"), 10.0);
}

#[test]
fn cycles_marked_circ_without_hanging() {
    let mut e = Engine::new();
    set(&mut e, "A1", "=B1");
    set(&mut e, "B1", "=A1");
    assert_eq!(err(&e, "A1"), ErrorKind::Circ);
    assert_eq!(err(&e, "B1"), ErrorKind::Circ);
    // Self-reference.
    set(&mut e, "C1", "=C1+1");
    assert_eq!(err(&e, "C1"), ErrorKind::Circ);
    // Downstream of a cycle propagates #CIRC!.
    set(&mut e, "D1", "=A1+1");
    assert_eq!(err(&e, "D1"), ErrorKind::Circ);
    // Breaking the cycle recovers.
    set(&mut e, "B1", "5");
    assert_eq!(num(&e, "A1"), 5.0);
    assert_eq!(num(&e, "D1"), 6.0);
}

#[test]
fn range_self_overlap_is_cycle() {
    let mut e = Engine::new();
    set(&mut e, "A5", "=SUM(A1:A10)");
    assert_eq!(err(&e, "A5"), ErrorKind::Circ);
}

#[test]
fn error_semantics() {
    let mut e = Engine::new();
    set(&mut e, "A1", "=1/0");
    assert_eq!(err(&e, "A1"), ErrorKind::Div0);
    // Errors propagate through operators and aggregates.
    set(&mut e, "A2", "=A1+1");
    assert_eq!(err(&e, "A2"), ErrorKind::Div0);
    set(&mut e, "A3", "=SUM(A1:A2)");
    assert_eq!(err(&e, "A3"), ErrorKind::Div0);
    // #NAME? for unknown functions and identifiers.
    set(&mut e, "B1", "=NOSUCHFUNC(1)");
    assert_eq!(err(&e, "B1"), ErrorKind::Name);
    set(&mut e, "B2", "=unknown_name");
    assert_eq!(err(&e, "B2"), ErrorKind::Name);
    // #VALUE! for non-numeric text in arithmetic.
    set(&mut e, "C1", "abc");
    set(&mut e, "C2", "=C1+1");
    assert_eq!(err(&e, "C2"), ErrorKind::Value);
    // Numeric text coerces.
    set(&mut e, "D1", "'3");
    set(&mut e, "D2", "=D1+1");
    assert_eq!(num(&e, "D2"), 4.0);
    // Empty coerces to 0 in arithmetic, "" in concat.
    set(&mut e, "E1", "=Z1+5");
    assert_eq!(num(&e, "E1"), 5.0);
    set(&mut e, "E2", "=\"x\"&Z1");
    assert_eq!(val(&e, "E2"), Value::Text("x".into()));
}

#[test]
fn operator_semantics_excel_verified() {
    let mut e = Engine::new();
    // Excel: -2^2 = 4 (unary minus binds tighter than ^).
    set(&mut e, "A1", "=-2^2");
    assert_eq!(num(&e, "A1"), 4.0);
    // Excel: 2^3^2 = 64 (left associative).
    set(&mut e, "A2", "=2^3^2");
    assert_eq!(num(&e, "A2"), 64.0);
    // Percent.
    set(&mut e, "A3", "=50%*10");
    assert_eq!(num(&e, "A3"), 5.0);
    // Concat coerces numbers via General format.
    set(&mut e, "A4", "=\"v\"&1.5");
    assert_eq!(val(&e, "A4"), Value::Text("v1.5".into()));
    // Comparisons: number < text < bool.
    set(&mut e, "B1", "=\"a\"<TRUE");
    assert_eq!(val(&e, "B1"), Value::Bool(true));
    set(&mut e, "B2", "=99999<\"a\"");
    assert_eq!(val(&e, "B2"), Value::Bool(true));
    set(&mut e, "B3", "=\"ABC\"=\"abc\"");
    assert_eq!(val(&e, "B3"), Value::Bool(true));
    // TRUE coerces to 1 in arithmetic.
    set(&mut e, "B4", "=TRUE+1");
    assert_eq!(num(&e, "B4"), 2.0);
    // 0^0 is #NUM!.
    set(&mut e, "B5", "=0^0");
    assert_eq!(err(&e, "B5"), ErrorKind::Num);
}

#[test]
fn math_functions_excel_verified() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "A2", "2");
    set(&mut e, "A3", "3");
    set(&mut e, "A4", "text");
    set(&mut e, "A5", "TRUE");

    // SUM ignores text/bools that come from ranges.
    set(&mut e, "C1", "=SUM(A1:A5)");
    assert_eq!(num(&e, "C1"), 6.0);
    // ...but coerces direct arguments: SUM("3", TRUE) = 4.
    set(&mut e, "C2", "=SUM(\"3\",TRUE)");
    assert_eq!(num(&e, "C2"), 4.0);
    set(&mut e, "C3", "=AVERAGE(A1:A3)");
    assert_eq!(num(&e, "C3"), 2.0);
    set(&mut e, "C4", "=AVERAGE(A4)");
    assert_eq!(err(&e, "C4"), ErrorKind::Div0);
    set(&mut e, "C5", "=MIN(A1:A5)");
    assert_eq!(num(&e, "C5"), 1.0);
    set(&mut e, "C6", "=MAX(A1:A5)");
    assert_eq!(num(&e, "C6"), 3.0);
    set(&mut e, "C7", "=MIN(B1:B9)");
    assert_eq!(num(&e, "C7"), 0.0);
    set(&mut e, "C8", "=COUNT(A1:A5)");
    assert_eq!(num(&e, "C8"), 3.0);
    set(&mut e, "C9", "=COUNTA(A1:A5)");
    assert_eq!(num(&e, "C9"), 5.0);
    set(&mut e, "C10", "=COUNTBLANK(A1:A10)");
    assert_eq!(num(&e, "C10"), 5.0);
    set(&mut e, "C11", "=PRODUCT(A1:A3)");
    assert_eq!(num(&e, "C11"), 6.0);

    // Excel-verified rounding.
    set(&mut e, "D1", "=ROUND(2.5,0)");
    assert_eq!(num(&e, "D1"), 3.0);
    set(&mut e, "D2", "=ROUND(-2.5,0)");
    assert_eq!(num(&e, "D2"), -3.0);
    set(&mut e, "D3", "=ROUND(2.675,2)");
    assert_eq!(num(&e, "D3"), 2.68);
    set(&mut e, "D4", "=ROUND(1234.5678,-2)");
    assert_eq!(num(&e, "D4"), 1200.0);
    set(&mut e, "D5", "=ROUNDUP(3.2,0)");
    assert_eq!(num(&e, "D5"), 4.0);
    set(&mut e, "D6", "=ROUNDUP(-3.2,0)");
    assert_eq!(num(&e, "D6"), -4.0);
    set(&mut e, "D7", "=ROUNDDOWN(3.9,0)");
    assert_eq!(num(&e, "D7"), 3.0);
    set(&mut e, "D8", "=ROUNDDOWN(-3.9,0)");
    assert_eq!(num(&e, "D8"), -3.0);

    set(&mut e, "E1", "=ABS(-4)");
    assert_eq!(num(&e, "E1"), 4.0);
    set(&mut e, "E2", "=INT(-1.5)");
    assert_eq!(num(&e, "E2"), -2.0);
    set(&mut e, "E3", "=INT(8.9)");
    assert_eq!(num(&e, "E3"), 8.0);
    // Excel: MOD(-3, 2) = 1 (sign of divisor).
    set(&mut e, "E4", "=MOD(-3,2)");
    assert_eq!(num(&e, "E4"), 1.0);
    set(&mut e, "E5", "=MOD(3,-2)");
    assert_eq!(num(&e, "E5"), -1.0);
    set(&mut e, "E6", "=MOD(3,0)");
    assert_eq!(err(&e, "E6"), ErrorKind::Div0);
    set(&mut e, "E7", "=POWER(2,10)");
    assert_eq!(num(&e, "E7"), 1024.0);
    set(&mut e, "E8", "=SQRT(16)");
    assert_eq!(num(&e, "E8"), 4.0);
    set(&mut e, "E9", "=SQRT(-1)");
    assert_eq!(err(&e, "E9"), ErrorKind::Num);
}

#[test]
fn logic_functions_excel_verified() {
    let mut e = Engine::new();
    set(&mut e, "A1", "5");
    set(&mut e, "B1", "=IF(A1>3,\"big\",\"small\")");
    assert_eq!(val(&e, "B1"), Value::Text("big".into()));
    set(&mut e, "B2", "=IF(A1>10,\"big\")");
    assert_eq!(val(&e, "B2"), Value::Bool(false));
    // Lazy branches: the untaken error branch does not propagate.
    set(&mut e, "B3", "=IF(TRUE,1,1/0)");
    assert_eq!(num(&e, "B3"), 1.0);
    set(&mut e, "B4", "=IFS(A1>10,\"a\",A1>3,\"b\")");
    assert_eq!(val(&e, "B4"), Value::Text("b".into()));
    set(&mut e, "B5", "=IFS(A1>10,\"a\")");
    assert_eq!(err(&e, "B5"), ErrorKind::NA);
    set(&mut e, "C1", "=AND(TRUE,1)");
    assert_eq!(val(&e, "C1"), Value::Bool(true));
    set(&mut e, "C2", "=AND(TRUE,0)");
    assert_eq!(val(&e, "C2"), Value::Bool(false));
    set(&mut e, "C3", "=OR(FALSE,0,1)");
    assert_eq!(val(&e, "C3"), Value::Bool(true));
    set(&mut e, "C4", "=NOT(0)");
    assert_eq!(val(&e, "C4"), Value::Bool(true));
    set(&mut e, "C5", "=AND(\"x\")");
    assert_eq!(err(&e, "C5"), ErrorKind::Value);
    set(&mut e, "D1", "=IFERROR(1/0,\"fallback\")");
    assert_eq!(val(&e, "D1"), Value::Text("fallback".into()));
    set(&mut e, "D2", "=IFERROR(7,\"fallback\")");
    assert_eq!(num(&e, "D2"), 7.0);
    set(&mut e, "E1", "=ISBLANK(Z1)");
    assert_eq!(val(&e, "E1"), Value::Bool(true));
    set(&mut e, "E2", "=ISBLANK(\"\")");
    assert_eq!(val(&e, "E2"), Value::Bool(false));
    set(&mut e, "E3", "=ISNUMBER(A1)");
    assert_eq!(val(&e, "E3"), Value::Bool(true));
    set(&mut e, "E4", "=ISTEXT(\"x\")");
    assert_eq!(val(&e, "E4"), Value::Bool(true));
    set(&mut e, "E5", "=ISERROR(1/0)");
    assert_eq!(val(&e, "E5"), Value::Bool(true));
}

#[test]
fn cross_sheet_refs_and_sheet_ops() {
    let mut e = Engine::new();
    e.apply(&Action::SheetAdd {
        name: "Data".into(),
    })
    .unwrap();
    e.apply(&Action::CellEdit {
        sheet: "Data".into(),
        addr: engine::CellAddr::parse_a1("A1").unwrap(),
        input: "42".into(),
    })
    .unwrap();
    set(&mut e, "A1", "=Data!A1*2");
    assert_eq!(num(&e, "A1"), 84.0);

    // Editing the other sheet recalcs dependents here.
    e.apply(&Action::CellEdit {
        sheet: "Data".into(),
        addr: engine::CellAddr::parse_a1("A1").unwrap(),
        input: "10".into(),
    })
    .unwrap();
    assert_eq!(num(&e, "A1"), 20.0);

    // Rename rewrites the formula text.
    e.apply(&Action::SheetRename {
        from: "Data".into(),
        to: "Numbers".into(),
    })
    .unwrap();
    match val(&e, "A1") {
        Value::Number(n) => assert_eq!(n, 20.0),
        other => panic!("{other:?}"),
    }
    let cell_input =
        e.wb.sheet_by_name("Sheet1")
            .unwrap()
            .cells
            .get(&engine::CellAddr::parse_a1("A1").unwrap())
            .unwrap()
            .input();
    assert_eq!(cell_input, "=Numbers!A1*2");

    // Delete: refs become #REF!.
    e.apply(&Action::SheetDelete {
        name: "Numbers".into(),
    })
    .unwrap();
    assert_eq!(err(&e, "A1"), ErrorKind::Ref);

    // Unknown sheet ref is #REF!.
    set(&mut e, "B1", "=Nope!A1");
    assert_eq!(err(&e, "B1"), ErrorKind::Ref);
}

#[test]
fn replay_reproduces_state() {
    // The flagship invariant in miniature: applying the same action list to
    // a fresh engine yields an identical state snapshot.
    let actions = vec![
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: engine::CellAddr::parse_a1("A1").unwrap(),
            input: "5".into(),
        },
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: engine::CellAddr::parse_a1("A2").unwrap(),
            input: "=A1*3".into(),
        },
        Action::SheetAdd {
            name: "Other".into(),
        },
        Action::CellEdit {
            sheet: "Other".into(),
            addr: engine::CellAddr::parse_a1("B2").unwrap(),
            input: "=Sheet1!A2+1".into(),
        },
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: engine::CellAddr::parse_a1("A1").unwrap(),
            input: "7".into(),
        },
        Action::CellClear {
            sheet: "Sheet1".into(),
            addr: engine::CellAddr::parse_a1("A1").unwrap(),
        },
    ];
    let mut live = Engine::new();
    for a in &actions {
        live.apply(a).unwrap();
    }
    let mut replayed = Engine::new();
    for a in &actions {
        replayed.apply(a).unwrap();
    }
    assert_eq!(
        serde_json::to_string(&live.wb.state_snapshot()).unwrap(),
        serde_json::to_string(&replayed.wb.state_snapshot()).unwrap()
    );
}

#[test]
fn apply_rejects_invalid() {
    let mut e = Engine::new();
    assert!(e
        .apply(&Action::CellEdit {
            sheet: "Nope".into(),
            addr: engine::CellAddr::parse_a1("A1").unwrap(),
            input: "1".into(),
        })
        .is_err());
    assert!(e
        .apply(&Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: engine::CellAddr::parse_a1("A1").unwrap(),
            input: "=1+".into(),
        })
        .is_err());
    assert!(e
        .apply(&Action::SheetDelete {
            name: "Sheet1".into()
        })
        .is_err());
    assert!(e
        .apply(&Action::SheetAdd {
            name: "sheet1".into()
        })
        .is_err());
}
