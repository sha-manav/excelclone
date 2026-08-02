//! Excel-verified integration tests for the text and conditional-aggregation
//! families.

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

fn text(e: &Engine, cell: &str) -> String {
    match val(e, cell) {
        Value::Text(s) => s,
        other => panic!("expected text at {cell}, got {other:?}"),
    }
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

#[test]
fn text_extraction_and_case() {
    let mut e = Engine::new();
    set(&mut e, "A1", "Hello World");
    set(&mut e, "B1", "=LEFT(A1,5)");
    assert_eq!(text(&e, "B1"), "Hello");
    set(&mut e, "B2", "=RIGHT(A1,5)");
    assert_eq!(text(&e, "B2"), "World");
    set(&mut e, "B3", "=MID(A1,7,5)");
    assert_eq!(text(&e, "B3"), "World");
    set(&mut e, "B4", "=LEN(A1)");
    assert_eq!(num(&e, "B4"), 11.0);
    set(&mut e, "B5", "=UPPER(A1)");
    assert_eq!(text(&e, "B5"), "HELLO WORLD");
    set(&mut e, "B6", "=LOWER(A1)");
    assert_eq!(text(&e, "B6"), "hello world");
    set(&mut e, "B7", "=PROPER(\"hello world\")");
    assert_eq!(text(&e, "B7"), "Hello World");
    // Excel's TRIM also collapses internal runs of spaces.
    set(&mut e, "B8", "=TRIM(\"  a   b  \")");
    assert_eq!(text(&e, "B8"), "a b");
    // n beyond the length returns the whole string; negative is #VALUE!.
    set(&mut e, "B9", "=LEFT(A1,99)");
    assert_eq!(text(&e, "B9"), "Hello World");
    set(&mut e, "B10", "=LEFT(A1,-1)");
    assert_eq!(err(&e, "B10"), ErrorKind::Value);
    // Non-ASCII must not panic or mis-slice.
    set(&mut e, "C1", "café ☕");
    set(&mut e, "C2", "=LEN(C1)");
    assert_eq!(num(&e, "C2"), 6.0);
    set(&mut e, "C3", "=LEFT(C1,4)");
    assert_eq!(text(&e, "C3"), "café");
}

#[test]
fn text_joining() {
    let mut e = Engine::new();
    set(&mut e, "A1", "a");
    set(&mut e, "A2", "b");
    set(&mut e, "A3", "c");
    set(&mut e, "B1", "=CONCAT(A1:A3)");
    assert_eq!(text(&e, "B1"), "abc");
    set(&mut e, "B2", "=CONCATENATE(A1,A2,\"!\")");
    assert_eq!(text(&e, "B2"), "ab!");
    set(&mut e, "B3", "=TEXTJOIN(\"-\",TRUE,A1:A3)");
    assert_eq!(text(&e, "B3"), "a-b-c");
    // ignore_empty FALSE keeps a slot for the blank cell.
    set(&mut e, "D1", "x");
    set(&mut e, "D3", "y");
    set(&mut e, "B4", "=TEXTJOIN(\",\",FALSE,D1:D3)");
    assert_eq!(text(&e, "B4"), "x,,y");
    set(&mut e, "B5", "=TEXTJOIN(\",\",TRUE,D1:D3)");
    assert_eq!(text(&e, "B5"), "x,y");
}

#[test]
fn text_search_and_substitution() {
    let mut e = Engine::new();
    set(&mut e, "A1", "abcABC");
    // FIND is case-sensitive; SEARCH is not.
    set(&mut e, "B1", "=FIND(\"A\",A1)");
    assert_eq!(num(&e, "B1"), 4.0);
    set(&mut e, "B2", "=SEARCH(\"A\",A1)");
    assert_eq!(num(&e, "B2"), 1.0);
    set(&mut e, "B3", "=FIND(\"z\",A1)");
    assert_eq!(err(&e, "B3"), ErrorKind::Value);
    // SEARCH supports wildcards.
    set(&mut e, "B4", "=SEARCH(\"b*B\",A1)");
    assert_eq!(num(&e, "B4"), 2.0);

    set(&mut e, "C1", "a-b-c");
    set(&mut e, "C2", "=SUBSTITUTE(C1,\"-\",\"+\")");
    assert_eq!(text(&e, "C2"), "a+b+c");
    // The instance argument replaces only the Nth occurrence.
    set(&mut e, "C3", "=SUBSTITUTE(C1,\"-\",\"+\",2)");
    assert_eq!(text(&e, "C3"), "a-b+c");
    set(&mut e, "C4", "=REPLACE(\"abcdef\",2,3,\"XY\")");
    assert_eq!(text(&e, "C4"), "aXYef");
}

#[test]
fn text_and_value_conversion() {
    let mut e = Engine::new();
    set(&mut e, "A1", "=TEXT(1234.567,\"#,##0.00\")");
    assert_eq!(text(&e, "A1"), "1,234.57");
    set(&mut e, "A2", "=TEXT(0.25,\"0%\")");
    assert_eq!(text(&e, "A2"), "25%");
    set(&mut e, "A3", "=TEXT(DATE(2024,3,7),\"yyyy-mm-dd\")");
    assert_eq!(text(&e, "A3"), "2024-03-07");
    set(&mut e, "A4", "=TEXT(2.675,\"0.00\")");
    assert_eq!(text(&e, "A4"), "2.68");

    set(&mut e, "B1", "=VALUE(\"1234\")");
    assert_eq!(num(&e, "B1"), 1234.0);
    set(&mut e, "B2", "=VALUE(\"12%\")");
    assert!((num(&e, "B2") - 0.12).abs() < 1e-12);
    set(&mut e, "B3", "=VALUE(\"nope\")");
    assert_eq!(err(&e, "B3"), ErrorKind::Value);
}

/// A small ledger in A1:C6 used by the conditional-aggregation tests.
fn ledger(e: &mut Engine) {
    let rows = [
        ("apple", "10", "north"),
        ("banana", "20", "south"),
        ("apple", "30", "south"),
        ("cherry", "40", "north"),
        ("apple", "50", "north"),
    ];
    set(e, "A1", "fruit");
    set(e, "B1", "qty");
    set(e, "C1", "region");
    for (i, (a, b, c)) in rows.iter().enumerate() {
        let r = i + 2;
        set(e, &format!("A{r}"), a);
        set(e, &format!("B{r}"), b);
        set(e, &format!("C{r}"), c);
    }
}

#[test]
fn countif_and_sumif() {
    let mut e = Engine::new();
    ledger(&mut e);

    set(&mut e, "E1", "=COUNTIF(A2:A6,\"apple\")");
    assert_eq!(num(&e, "E1"), 3.0);
    // Criteria matching is case-insensitive.
    set(&mut e, "E2", "=COUNTIF(A2:A6,\"APPLE\")");
    assert_eq!(num(&e, "E2"), 3.0);
    set(&mut e, "E3", "=COUNTIF(B2:B6,\">25\")");
    assert_eq!(num(&e, "E3"), 3.0);
    set(&mut e, "E4", "=COUNTIF(A2:A6,\"<>apple\")");
    assert_eq!(num(&e, "E4"), 2.0);
    // Wildcards.
    set(&mut e, "E5", "=COUNTIF(A2:A6,\"*rr*\")");
    assert_eq!(num(&e, "E5"), 1.0);
    set(&mut e, "E6", "=COUNTIF(A2:A6,\"?anana\")");
    assert_eq!(num(&e, "E6"), 1.0);

    set(&mut e, "F1", "=SUMIF(A2:A6,\"apple\",B2:B6)");
    assert_eq!(num(&e, "F1"), 90.0);
    set(&mut e, "F2", "=SUMIF(B2:B6,\">=30\")");
    assert_eq!(num(&e, "F2"), 120.0);
    set(&mut e, "F3", "=AVERAGEIF(A2:A6,\"apple\",B2:B6)");
    assert_eq!(num(&e, "F3"), 30.0);
    // No matches at all is #DIV/0! for the averaging variants.
    set(&mut e, "F4", "=AVERAGEIF(A2:A6,\"kiwi\",B2:B6)");
    assert_eq!(err(&e, "F4"), ErrorKind::Div0);
}

#[test]
fn multi_criteria_variants() {
    let mut e = Engine::new();
    ledger(&mut e);

    set(&mut e, "E1", "=COUNTIFS(A2:A6,\"apple\",C2:C6,\"north\")");
    assert_eq!(num(&e, "E1"), 2.0);
    set(
        &mut e,
        "E2",
        "=SUMIFS(B2:B6,A2:A6,\"apple\",C2:C6,\"north\")",
    );
    assert_eq!(num(&e, "E2"), 60.0);
    set(
        &mut e,
        "E3",
        "=AVERAGEIFS(B2:B6,A2:A6,\"apple\",C2:C6,\"north\")",
    );
    assert_eq!(num(&e, "E3"), 30.0);
    set(&mut e, "E4", "=SUMIFS(B2:B6,B2:B6,\">15\",B2:B6,\"<45\")");
    assert_eq!(num(&e, "E4"), 90.0);
    // Mismatched range shapes fail loudly.
    set(&mut e, "E5", "=COUNTIFS(A2:A6,\"apple\",C2:C4,\"north\")");
    assert_eq!(err(&e, "E5"), ErrorKind::Value);
}

#[test]
fn criteria_can_come_from_a_cell() {
    let mut e = Engine::new();
    ledger(&mut e);
    set(&mut e, "E1", "apple");
    set(&mut e, "E2", "=COUNTIF(A2:A6,E1)");
    assert_eq!(num(&e, "E2"), 3.0);
    set(&mut e, "E3", ">25");
    set(&mut e, "E4", "=COUNTIF(B2:B6,E3)");
    assert_eq!(num(&e, "E4"), 3.0);
}

#[test]
fn conditional_aggregates_recalc_with_their_ranges() {
    let mut e = Engine::new();
    ledger(&mut e);
    set(&mut e, "E1", "=SUMIF(A2:A6,\"apple\",B2:B6)");
    assert_eq!(num(&e, "E1"), 90.0);
    // Editing a cell inside a watched range must refresh the aggregate.
    set(&mut e, "B2", "100");
    assert_eq!(num(&e, "E1"), 180.0);
    set(&mut e, "A3", "apple");
    assert_eq!(num(&e, "E1"), 200.0);
}
