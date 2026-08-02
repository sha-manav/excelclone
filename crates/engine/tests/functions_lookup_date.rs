//! Excel-verified integration tests for the lookup and date/time families.

use engine::{Action, CellAddr, Engine, ErrorKind, Value};

fn set(e: &mut Engine, cell: &str, input: &str) {
    e.apply(&Action::CellEdit {
        sheet: "Sheet1".into(),
        addr: CellAddr::parse_a1(cell).unwrap(),
        input: input.into(),
    })
    .unwrap();
}

fn val(e: &Engine, cell: &str) -> Value {
    e.value_at("Sheet1", cell)
}

fn num(e: &Engine, cell: &str) -> f64 {
    match val(e, cell) {
        Value::Number(n) => n,
        other => panic!("expected number at {cell}, got {other:?}"),
    }
}

fn err(e: &Engine, cell: &str) -> ErrorKind {
    match val(e, cell) {
        Value::Error(k) => k,
        other => panic!("expected error at {cell}, got {other:?}"),
    }
}

/// A small lookup table in A1:C4.
fn table(e: &mut Engine) {
    let rows = [
        ("A1", "10", "apple", "100"),
        ("A2", "20", "banana", "200"),
        ("A3", "30", "cherry", "300"),
        ("A4", "40", "date", "400"),
    ];
    for (i, (_, a, b, c)) in rows.iter().enumerate() {
        let r = i + 1;
        set(e, &format!("A{r}"), a);
        set(e, &format!("B{r}"), b);
        set(e, &format!("C{r}"), c);
    }
}

#[test]
fn vlookup_exact_and_approximate() {
    let mut e = Engine::new();
    table(&mut e);

    set(&mut e, "E1", "=VLOOKUP(20,A1:C4,2,FALSE)");
    assert_eq!(val(&e, "E1"), Value::Text("banana".into()));
    set(&mut e, "E2", "=VLOOKUP(20,A1:C4,3,FALSE)");
    assert_eq!(num(&e, "E2"), 200.0);
    // Exact mode: no match is #N/A.
    set(&mut e, "E3", "=VLOOKUP(25,A1:C4,2,FALSE)");
    assert_eq!(err(&e, "E3"), ErrorKind::NA);
    // Approximate mode falls back to the largest value <= lookup.
    set(&mut e, "E4", "=VLOOKUP(25,A1:C4,2,TRUE)");
    assert_eq!(val(&e, "E4"), Value::Text("banana".into()));
    // Below the first entry is #N/A even in approximate mode.
    set(&mut e, "E5", "=VLOOKUP(5,A1:C4,2,TRUE)");
    assert_eq!(err(&e, "E5"), ErrorKind::NA);
    // Column index out of range.
    set(&mut e, "E6", "=VLOOKUP(10,A1:C4,9,FALSE)");
    assert_eq!(err(&e, "E6"), ErrorKind::Ref);
    set(&mut e, "E7", "=VLOOKUP(10,A1:C4,0,FALSE)");
    assert_eq!(err(&e, "E7"), ErrorKind::Value);
}

#[test]
fn vlookup_text_and_wildcards() {
    let mut e = Engine::new();
    set(&mut e, "A1", "apple");
    set(&mut e, "B1", "1");
    set(&mut e, "A2", "banana");
    set(&mut e, "B2", "2");

    // Text lookup is case-insensitive.
    set(&mut e, "D1", "=VLOOKUP(\"APPLE\",A1:B2,2,FALSE)");
    assert_eq!(num(&e, "D1"), 1.0);
    // Wildcards work in exact mode.
    set(&mut e, "D2", "=VLOOKUP(\"ban*\",A1:B2,2,FALSE)");
    assert_eq!(num(&e, "D2"), 2.0);
    set(&mut e, "D3", "=VLOOKUP(\"?anana\",A1:B2,2,FALSE)");
    assert_eq!(num(&e, "D3"), 2.0);
}

#[test]
fn hlookup_scans_the_first_row() {
    let mut e = Engine::new();
    set(&mut e, "A1", "q1");
    set(&mut e, "B1", "q2");
    set(&mut e, "C1", "q3");
    set(&mut e, "A2", "10");
    set(&mut e, "B2", "20");
    set(&mut e, "C2", "30");
    set(&mut e, "A4", "=HLOOKUP(\"q2\",A1:C2,2,FALSE)");
    assert_eq!(num(&e, "A4"), 20.0);
    set(&mut e, "A5", "=HLOOKUP(\"q9\",A1:C2,2,FALSE)");
    assert_eq!(err(&e, "A5"), ErrorKind::NA);
}

#[test]
fn index_and_match() {
    let mut e = Engine::new();
    table(&mut e);

    set(&mut e, "E1", "=INDEX(A1:C4,2,2)");
    assert_eq!(val(&e, "E1"), Value::Text("banana".into()));
    set(&mut e, "E2", "=INDEX(B1:B4,3)");
    assert_eq!(val(&e, "E2"), Value::Text("cherry".into()));
    // Out of range.
    set(&mut e, "E3", "=INDEX(A1:C4,9,1)");
    assert_eq!(err(&e, "E3"), ErrorKind::Ref);

    set(&mut e, "F1", "=MATCH(30,A1:A4,0)");
    assert_eq!(num(&e, "F1"), 3.0);
    set(&mut e, "F2", "=MATCH(\"cherry\",B1:B4,0)");
    assert_eq!(num(&e, "F2"), 3.0);
    set(&mut e, "F3", "=MATCH(25,A1:A4,1)");
    assert_eq!(num(&e, "F3"), 2.0);
    set(&mut e, "F4", "=MATCH(99,A1:A4,0)");
    assert_eq!(err(&e, "F4"), ErrorKind::NA);

    // The classic INDEX/MATCH pairing.
    set(&mut e, "G1", "=INDEX(C1:C4,MATCH(\"cherry\",B1:B4,0))");
    assert_eq!(num(&e, "G1"), 300.0);
}

#[test]
fn xlookup_exact_modes() {
    let mut e = Engine::new();
    table(&mut e);

    set(&mut e, "E1", "=XLOOKUP(30,A1:A4,B1:B4)");
    assert_eq!(val(&e, "E1"), Value::Text("cherry".into()));
    // if_not_found beats #N/A.
    set(&mut e, "E2", "=XLOOKUP(99,A1:A4,B1:B4,\"none\")");
    assert_eq!(val(&e, "E2"), Value::Text("none".into()));
    set(&mut e, "E3", "=XLOOKUP(99,A1:A4,B1:B4)");
    assert_eq!(err(&e, "E3"), ErrorKind::NA);
    // Wildcard mode.
    set(&mut e, "E4", "=XLOOKUP(\"che*\",B1:B4,C1:C4,,2)");
    assert_eq!(num(&e, "E4"), 300.0);
    // Approximate modes are not implemented yet and must fail loudly.
    set(&mut e, "E5", "=XLOOKUP(25,A1:A4,B1:B4,,-1)");
    assert_eq!(err(&e, "E5"), ErrorKind::Value);
    // Mismatched array lengths.
    set(&mut e, "E6", "=XLOOKUP(10,A1:A4,B1:B2)");
    assert_eq!(err(&e, "E6"), ErrorKind::Value);
}

#[test]
fn choose_is_lazy() {
    let mut e = Engine::new();
    set(&mut e, "A1", "=CHOOSE(2,\"a\",\"b\",\"c\")");
    assert_eq!(val(&e, "A1"), Value::Text("b".into()));
    set(&mut e, "A2", "=CHOOSE(4,\"a\",\"b\")");
    assert_eq!(err(&e, "A2"), ErrorKind::Value);
    // Unselected branches are never evaluated, so their errors do not leak.
    set(&mut e, "A3", "=CHOOSE(1,10,1/0)");
    assert_eq!(num(&e, "A3"), 10.0);
}

#[test]
fn date_construction_and_parts() {
    let mut e = Engine::new();
    // Excel: DATE(2020,1,1) is serial 43831.
    set(&mut e, "A1", "=DATE(2020,1,1)");
    assert_eq!(num(&e, "A1"), 43831.0);
    // Out-of-range months and days roll over.
    set(&mut e, "A2", "=YEAR(DATE(2020,13,1))");
    assert_eq!(num(&e, "A2"), 2021.0);
    set(&mut e, "A3", "=DATE(2020,1,0)");
    set(&mut e, "A4", "=YEAR(A3)&\"-\"&MONTH(A3)&\"-\"&DAY(A3)");
    assert_eq!(val(&e, "A4"), Value::Text("2019-12-31".into()));

    set(&mut e, "B1", "=YEAR(DATE(2024,2,29))");
    assert_eq!(num(&e, "B1"), 2024.0);
    set(&mut e, "B2", "=MONTH(DATE(2024,2,29))");
    assert_eq!(num(&e, "B2"), 2.0);
    set(&mut e, "B3", "=DAY(DATE(2024,2,29))");
    assert_eq!(num(&e, "B3"), 29.0);
}

#[test]
fn eomonth_and_datedif_and_weekday() {
    let mut e = Engine::new();
    // Excel: EOMONTH(DATE(2024,1,31),1) is 2024-02-29.
    set(&mut e, "A1", "=DAY(EOMONTH(DATE(2024,1,31),1))");
    assert_eq!(num(&e, "A1"), 29.0);
    set(&mut e, "A2", "=MONTH(EOMONTH(DATE(2024,1,15),-1))");
    assert_eq!(num(&e, "A2"), 12.0);

    set(
        &mut e,
        "B1",
        "=DATEDIF(DATE(1969,7,16),DATE(2020,1,1),\"Y\")",
    );
    assert_eq!(num(&e, "B1"), 50.0);
    set(
        &mut e,
        "B2",
        "=DATEDIF(DATE(2020,1,1),DATE(2020,3,1),\"M\")",
    );
    assert_eq!(num(&e, "B2"), 2.0);
    set(
        &mut e,
        "B3",
        "=DATEDIF(DATE(2020,1,1),DATE(2020,1,31),\"D\")",
    );
    assert_eq!(num(&e, "B3"), 30.0);
    // Reversed arguments fail loudly.
    set(
        &mut e,
        "B4",
        "=DATEDIF(DATE(2020,1,2),DATE(2020,1,1),\"D\")",
    );
    assert_eq!(err(&e, "B4"), ErrorKind::Num);

    // 2024-01-01 was a Monday.
    set(&mut e, "C1", "=WEEKDAY(DATE(2024,1,1))");
    assert_eq!(num(&e, "C1"), 2.0);
    set(&mut e, "C2", "=WEEKDAY(DATE(2024,1,1),2)");
    assert_eq!(num(&e, "C2"), 1.0);
    set(&mut e, "C3", "=WEEKDAY(DATE(2024,1,1),3)");
    assert_eq!(num(&e, "C3"), 0.0);
    set(&mut e, "C4", "=WEEKDAY(DATE(2024,1,1),9)");
    assert_eq!(err(&e, "C4"), ErrorKind::Num);
}

#[test]
fn today_and_now_use_the_injected_clock() {
    let mut e = Engine::new();
    // 2024-01-01T12:00:00Z.
    e.now_ms = 1_704_110_400_000;
    set(&mut e, "A1", "=TODAY()");
    set(&mut e, "A2", "=NOW()");
    set(&mut e, "A3", "=YEAR(TODAY())");
    assert_eq!(num(&e, "A1"), 45292.0);
    assert!((num(&e, "A2") - 45292.5).abs() < 1e-6);
    assert_eq!(num(&e, "A3"), 2024.0);
}

#[test]
fn volatile_functions_recalc_on_every_edit() {
    let mut e = Engine::new();
    e.now_ms = 1_704_110_400_000;
    set(&mut e, "A1", "=TODAY()");
    assert_eq!(num(&e, "A1"), 45292.0);
    // Advance the clock a day; an unrelated edit must refresh TODAY().
    e.now_ms += 86_400_000;
    set(&mut e, "B1", "trigger");
    assert_eq!(num(&e, "A1"), 45293.0);
}

#[test]
fn random_functions_stay_in_range_and_are_replayable() {
    let mut e = Engine::new();
    e.now_ms = 1_704_110_400_000;
    set(&mut e, "A1", "=RANDBETWEEN(1,6)");
    set(&mut e, "A2", "=RAND()");
    let a1 = num(&e, "A1");
    assert!((1.0..=6.0).contains(&a1), "RANDBETWEEN out of range: {a1}");
    assert_eq!(a1, a1.trunc());
    let a2 = num(&e, "A2");
    assert!((0.0..1.0).contains(&a2), "RAND out of range: {a2}");

    // Replaying the same actions against the same clock reproduces the
    // exact same values — the property the whole event log depends on.
    let mut replay = Engine::new();
    replay.now_ms = 1_704_110_400_000;
    set(&mut replay, "A1", "=RANDBETWEEN(1,6)");
    set(&mut replay, "A2", "=RAND()");
    assert_eq!(replay.wb.state_snapshot(), e.wb.state_snapshot());
}
