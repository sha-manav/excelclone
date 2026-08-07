//! Whole-column and whole-row references: `A:C`, `1:5`.
//!
//! These are rectangles like any other, with one axis supplied by the sheet
//! rather than the author. That single difference is where every interesting
//! case lives: printing has to give the axis back unwritten, structural edits
//! must not shift an axis nobody chose, and evaluation must not densify a
//! million rows to add up ten.

use engine::{Action, CellAddr, Engine, RangeAddr, Value};

fn a1(s: &str) -> CellAddr {
    CellAddr::parse_a1(s).unwrap()
}

/// A sheet with 1, 3 and 4 down column A and 10, 20 down column B.
fn seeded() -> Engine {
    let mut e = Engine::new();
    for (cell, input) in [
        ("A1", "1"),
        ("A2", "3"),
        ("A3", "4"),
        ("B1", "10"),
        ("B2", "20"),
    ] {
        put(&mut e, cell, input);
    }
    e
}

fn put(e: &mut Engine, cell: &str, input: &str) {
    e.apply(&Action::CellEdit {
        sheet: "Sheet1".into(),
        addr: a1(cell),
        input: input.into(),
    })
    .unwrap();
}

fn value(e: &Engine, cell: &str) -> Value {
    e.value_at("Sheet1", cell)
}

/// What the formula bar would show — the reference as the user wrote it.
fn input(e: &Engine, cell: &str) -> String {
    e.wb.sheet_by_name("Sheet1")
        .unwrap()
        .cells
        .get(&a1(cell))
        .map(|c| c.input())
        .unwrap_or_default()
}

#[test]
fn a_whole_column_sums_what_is_in_it() {
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:A)");
    assert_eq!(value(&e, "D1"), Value::Number(8.0));
}

#[test]
fn a_span_of_columns_covers_all_of_them() {
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:B)");
    assert_eq!(value(&e, "D1"), Value::Number(38.0));
}

#[test]
fn whole_rows_work_the_same_way() {
    let mut e = seeded();
    put(&mut e, "D5", "=SUM(1:2)");
    assert_eq!(value(&e, "D5"), Value::Number(34.0));
}

#[test]
fn an_empty_column_sums_to_zero_rather_than_erroring() {
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(Z:Z)");
    assert_eq!(value(&e, "D1"), Value::Number(0.0));
}

#[test]
fn an_open_reference_comes_back_out_the_way_it_went_in() {
    // The formula bar shows what the user typed, not `A1:A1048576`.
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:A)");
    put(&mut e, "D2", "=SUM(1:5)");
    put(&mut e, "D3", "=SUM($A:$C)");
    assert_eq!(input(&e, "D1"), "=SUM(A:A)");
    assert_eq!(input(&e, "D2"), "=SUM(1:5)");
    assert_eq!(input(&e, "D3"), "=SUM($A:$C)");
}

#[test]
fn a_column_reference_follows_new_data_below_the_used_range() {
    // Evaluation narrows to the used range for speed; the *dependency* keeps
    // the whole column, or a formula would stop noticing new rows — which is
    // the single reason anybody writes `A:A` instead of `A1:A100`.
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:A)");
    assert_eq!(value(&e, "D1"), Value::Number(8.0));
    put(&mut e, "A50", "90");
    assert_eq!(value(&e, "D1"), Value::Number(98.0));
}

#[test]
fn inserting_a_row_leaves_a_whole_column_reference_alone() {
    // The row axis was never written down, so there is nothing to shift — and
    // shifting it would push the bottom past the last row and produce #REF!.
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:A)");
    e.apply(&Action::RowInsert {
        sheet: "Sheet1".into(),
        at: 0,
        count: 2,
    })
    .unwrap();
    assert_eq!(input(&e, "D3"), "=SUM(A:A)");
    assert_eq!(value(&e, "D3"), Value::Number(8.0));
}

#[test]
fn inserting_a_column_still_moves_a_whole_column_reference() {
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:A)");
    e.apply(&Action::ColInsert {
        sheet: "Sheet1".into(),
        at: 0,
        count: 1,
    })
    .unwrap();
    assert_eq!(input(&e, "E1"), "=SUM(B:B)");
    assert_eq!(value(&e, "E1"), Value::Number(8.0));
}

#[test]
fn inserting_a_row_still_moves_a_whole_row_reference() {
    let mut e = seeded();
    put(&mut e, "D5", "=SUM(1:2)");
    e.apply(&Action::RowInsert {
        sheet: "Sheet1".into(),
        at: 0,
        count: 1,
    })
    .unwrap();
    assert_eq!(input(&e, "D6"), "=SUM(2:3)");
}

#[test]
fn copying_shifts_only_the_axis_the_formula_named() {
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:A)");
    e.apply(&Action::RangePaste {
        source_sheet: "Sheet1".into(),
        source: RangeAddr::new(a1("D1"), a1("D1")),
        target_sheet: "Sheet1".into(),
        target: RangeAddr::new(a1("E5"), a1("E5")),
        mode: engine::PasteMode::Formulas,
        cut: false,
    })
    .unwrap();
    // One column right and four rows down: the columns move, the rows have
    // nothing to move.
    assert_eq!(input(&e, "E5"), "=SUM(B:B)");
}

#[test]
fn a_bare_column_letter_is_still_a_name_without_a_colon() {
    // `A` alone has to keep meaning what it meant, or every LET binding named
    // after a column stops working.
    let ast = engine::parser::parse_formula("LET(A,2,A*3)").unwrap();
    assert_eq!(ast.to_formula(), "LET(A,2,A*3)");
    let mut e = Engine::new();
    put(&mut e, "A5", "=LET(A,2,A*3)");
    assert_eq!(value(&e, "A5"), Value::Number(6.0));
}

#[test]
fn a_number_range_is_not_confused_with_arithmetic() {
    // `1:5` is a range; `1` and `5` on their own are still numbers, and
    // nothing that looks like a colon-free expression may change meaning.
    assert_eq!(
        engine::parser::parse_formula("1+5").unwrap().to_formula(),
        "1+5"
    );
    assert!(engine::parser::parse_formula("1.5:2").is_err());
    assert!(engine::parser::parse_formula("0:3").is_err());
}

#[test]
fn open_ranges_survive_a_cross_sheet_reference() {
    let mut e = seeded();
    e.apply(&Action::SheetAdd {
        name: "Data".into(),
    })
    .unwrap();
    e.apply(&Action::CellEdit {
        sheet: "Data".into(),
        addr: a1("A1"),
        input: "7".into(),
    })
    .unwrap();
    put(&mut e, "D1", "=SUM(Data!A:A)");
    assert_eq!(value(&e, "D1"), Value::Number(7.0));
    assert_eq!(input(&e, "D1"), "=SUM(Data!A:A)");
}

#[test]
fn counting_a_whole_column_counts_what_is_there() {
    let mut e = seeded();
    put(&mut e, "D1", "=COUNT(A:A)");
    put(&mut e, "D2", "=COUNTA(A:A)");
    assert_eq!(value(&e, "D1"), Value::Number(3.0));
    assert_eq!(value(&e, "D2"), Value::Number(3.0));
}

#[test]
fn a_lookup_over_a_whole_column_finds_its_row() {
    let mut e = Engine::new();
    for (cell, input) in [
        ("A1", "apple"),
        ("B1", "3"),
        ("A2", "pear"),
        ("B2", "5"),
        ("A3", "plum"),
        ("B3", "9"),
    ] {
        put(&mut e, cell, input);
    }
    put(&mut e, "D1", "=VLOOKUP(\"pear\",A:B,2,FALSE)");
    assert_eq!(value(&e, "D1"), Value::Number(5.0));
}

#[test]
fn an_open_reference_survives_a_round_trip_through_xlsx() {
    // Formulas go out as text and come back through the same parser, so this
    // is really a test that both directions agree — and the direction that
    // would fail silently is the write, which is the one that reaches a file
    // somebody else opens.
    let mut e = seeded();
    put(&mut e, "D1", "=SUM(A:A)");
    // Rows 2 and 3, clear of both row 1 (where D1's own result sits) and row 5
    // (where this formula does) — a formula inside the band it reads is
    // circular, which is tested on purpose below.
    put(&mut e, "D5", "=SUM(2:3)");
    let bytes = engine::io::xlsx::export(&e.wb).unwrap();

    let back = engine::io::xlsx::import(&bytes).unwrap().engine;
    let back = {
        let mut b = back;
        b.recalc_all();
        b
    };
    assert_eq!(input(&back, "D1"), "=SUM(A:A)");
    assert_eq!(input(&back, "D5"), "=SUM(2:3)");
    assert_eq!(value(&back, "D1"), Value::Number(8.0));
    assert_eq!(value(&back, "D5"), Value::Number(27.0));
}

#[test]
fn a_whole_column_reference_does_not_densify_the_column() {
    // The point of narrowing to the used range. Without it this builds a
    // million-element vector per formula and the test does not finish.
    let mut e = seeded();
    for i in 0..200 {
        put(&mut e, &format!("D{}", i + 1), "=SUM(A:A)+COUNTA(B:B)");
    }
    assert_eq!(value(&e, "D200"), Value::Number(10.0));
}

#[test]
fn a_formula_inside_the_band_it_reads_is_circular() {
    // `=SUM(A:A)` written *in* column A reads itself. The dependency graph
    // sees the whole column — it is only evaluation that narrows — so this is
    // caught rather than quietly computing a number that depends on itself.
    let mut e = seeded();
    put(&mut e, "A9", "=SUM(A:A)");
    assert_eq!(value(&e, "A9"), Value::Error(engine::ErrorKind::Circ));

    put(&mut e, "D2", "=SUM(1:2)");
    assert_eq!(value(&e, "D2"), Value::Error(engine::ErrorKind::Circ));
}

#[test]
fn adding_the_span_marker_did_not_move_an_ordinary_workbook_hash() {
    // Snapshots are content-addressed over the workbook, and the AST is in
    // there. Serializing `span` on every range would have changed the hash of
    // every workbook containing one — every committed trajectory would have
    // stopped replaying, which is how this was found. The default stays out of
    // the JSON, so only a workbook that actually uses an open range differs.
    let ast = engine::parser::parse_formula("SUM(A1:A3)").unwrap();
    let json = serde_json::to_string(&ast).unwrap();
    assert!(!json.contains("span"), "{json}");

    let open = engine::parser::parse_formula("SUM(A:A)").unwrap();
    let json = serde_json::to_string(&open).unwrap();
    assert!(json.contains("\"span\":\"cols\""), "{json}");
}
