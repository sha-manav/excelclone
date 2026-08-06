//! Dynamic arrays landing on the grid.
//!
//! The parity corpus reads one cell, so it can only ask what a block's
//! elements are. These ask the question the corpus cannot: *where* they went,
//! what happens when something is in the way, and whether the rest of the
//! engine — dependencies, undo, structural edits, export — knows about cells
//! that have no `Cell` behind them.

use engine::{Action, CellAddr, Engine, RangeAddr, Value};

fn set(e: &mut Engine, a1: &str, input: &str) {
    e.apply(&Action::CellEdit {
        sheet: "Sheet1".into(),
        addr: CellAddr::parse_a1(a1).unwrap(),
        input: input.into(),
    })
    .unwrap();
}

fn clear(e: &mut Engine, a1: &str) {
    e.apply(&Action::CellClear {
        sheet: "Sheet1".into(),
        addr: CellAddr::parse_a1(a1).unwrap(),
    })
    .unwrap();
}

fn at(e: &Engine, a1: &str) -> Value {
    e.value_at("Sheet1", a1)
}

/// Three values, two of them the same.
fn source(e: &mut Engine) {
    set(e, "A1", "b");
    set(e, "A2", "a");
    set(e, "A3", "b");
}

#[test]
fn a_block_fills_the_cells_below_its_formula() {
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");

    assert_eq!(at(&e, "C1"), Value::Text("b".into()));
    assert_eq!(at(&e, "C2"), Value::Text("a".into()));
    // Two distinct values, so nothing lands in the third row.
    assert_eq!(at(&e, "C3"), Value::Empty);

    // The spilled cell is not a cell: there is nothing to edit, and the
    // formula bar has nothing to show.
    let sheet = e.wb.sheet_by_name("Sheet1").unwrap();
    assert!(!sheet.cells.contains_key(&CellAddr::parse_a1("C2").unwrap()));
    assert_eq!(
        sheet.spill_anchor(CellAddr::parse_a1("C2").unwrap()),
        Some(CellAddr::parse_a1("C1").unwrap())
    );
}

#[test]
fn a_block_fills_across_as_well_as_down() {
    let mut e = Engine::new();
    set(&mut e, "A1", "=SEQUENCE(2,3)");
    for (cell, n) in [
        ("A1", 1.0),
        ("B1", 2.0),
        ("C1", 3.0),
        ("A2", 4.0),
        ("B2", 5.0),
        ("C2", 6.0),
    ] {
        assert_eq!(at(&e, cell), Value::Number(n), "{cell}");
    }
}

#[test]
fn something_in_the_way_is_a_spill_error_and_nothing_lands() {
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C2", "in the way");
    set(&mut e, "C1", "=UNIQUE(A1:A3)");

    assert_eq!(at(&e, "C1"), Value::Error(engine::ErrorKind::Spill));
    // Half a block would be worse than none: the user could not tell which
    // half was the formula's.
    assert_eq!(at(&e, "C2"), Value::Text("in the way".into()));
    assert!(e.wb.sheet_by_name("Sheet1").unwrap().spill.is_empty());

    // Clear the obstruction and the block lands without the formula being
    // touched.
    clear(&mut e, "C2");
    assert_eq!(at(&e, "C1"), Value::Text("b".into()));
    assert_eq!(at(&e, "C2"), Value::Text("a".into()));
}

#[test]
fn typing_into_a_spilled_cell_breaks_the_spill_rather_than_being_overwritten() {
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");
    assert_eq!(at(&e, "C2"), Value::Text("a".into()));

    set(&mut e, "C2", "mine");
    assert_eq!(at(&e, "C2"), Value::Text("mine".into()));
    assert_eq!(at(&e, "C1"), Value::Error(engine::ErrorKind::Spill));
}

#[test]
fn a_block_shrinks_and_grows_with_its_source() {
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");
    assert_eq!(at(&e, "C3"), Value::Empty);

    // A third distinct value makes the block one row taller.
    set(&mut e, "A3", "c");
    assert_eq!(at(&e, "C3"), Value::Text("c".into()));

    // And back again: the abandoned cell has to be cleared, not left holding
    // the value it had.
    set(&mut e, "A3", "b");
    assert_eq!(at(&e, "C3"), Value::Empty);
}

#[test]
fn a_formula_reading_a_spilled_cell_sees_it_change() {
    // The dependency graph knows nothing about C2: no cell was ever written
    // there. This is the case that makes spilling more than a display trick.
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");
    set(&mut e, "E1", "=C2&\"!\"");
    assert_eq!(at(&e, "E1"), Value::Text("a!".into()));

    set(&mut e, "A2", "z");
    assert_eq!(at(&e, "C2"), Value::Text("z".into()));
    assert_eq!(
        at(&e, "E1"),
        Value::Text("z!".into()),
        "the reader of a spilled cell was not recalculated"
    );
}

#[test]
fn a_block_reaching_the_bottom_of_the_grid_is_a_spill_error() {
    let mut e = Engine::new();
    set(&mut e, "A1048575", "=SEQUENCE(3)");
    assert_eq!(
        e.value_at("Sheet1", "A1048575"),
        Value::Error(engine::ErrorKind::Spill),
        "a block hanging off the end of the sheet has to say so"
    );
}

#[test]
fn undo_takes_a_block_away_with_the_formula_that_made_it() {
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");
    assert_eq!(at(&e, "C2"), Value::Text("a".into()));

    e.apply(&Action::Undo).unwrap();
    assert_eq!(at(&e, "C1"), Value::Empty);
    assert_eq!(
        at(&e, "C2"),
        Value::Empty,
        "the block outlived the formula that produced it"
    );

    e.apply(&Action::Redo).unwrap();
    assert_eq!(at(&e, "C2"), Value::Text("a".into()));
}

#[test]
fn deleting_the_formula_takes_its_block_with_it() {
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");
    clear(&mut e, "C1");
    assert_eq!(at(&e, "C2"), Value::Empty);
    assert!(e.wb.sheet_by_name("Sheet1").unwrap().spill.is_empty());
}

#[test]
fn spilled_cells_are_part_of_the_used_range() {
    // Ctrl+Down, CSV export and whole-sheet ranges all read the used range;
    // a block that stopped at its anchor would be invisible to all three.
    let mut e = Engine::new();
    set(&mut e, "A1", "=SEQUENCE(4)");
    let used = e.wb.sheet_by_name("Sheet1").unwrap().used_range();
    assert_eq!(used, Some(RangeAddr::parse_a1("A1:A4").unwrap()));
}

#[test]
fn two_blocks_place_the_same_way_whatever_order_they_were_written_in() {
    // Replay determinism, at the level spilling can break it: the layout must
    // come from the anchors' addresses, not from the order the formulas
    // happened to be evaluated in.
    let build = |reverse: bool| {
        let mut e = Engine::new();
        let steps: Vec<(&str, &str)> = vec![("A1", "=SEQUENCE(3)"), ("A3", "=SEQUENCE(3)")];
        let steps: Vec<(&str, &str)> = if reverse {
            steps.into_iter().rev().collect()
        } else {
            steps
        };
        for (cell, input) in steps {
            set(&mut e, cell, input);
        }
        e
    };
    let forward = build(false);
    let backward = build(true);
    assert_eq!(
        forward.wb.state_snapshot(),
        backward.wb.state_snapshot(),
        "the layout depended on the order the formulas were entered"
    );
    // A1 wants A1:A3 and A3 holds a real cell, so A1 is the one that cannot
    // land — a cell always beats a block, whichever was written first. A3's
    // own block has A3:A5 free and spills.
    assert_eq!(
        forward.value_at("Sheet1", "A1"),
        Value::Error(engine::ErrorKind::Spill)
    );
    assert_eq!(forward.value_at("Sheet1", "A4"), Value::Number(2.0));
}

#[test]
fn a_block_moves_with_an_inserted_row() {
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");
    e.apply(&Action::RowInsert {
        sheet: "Sheet1".into(),
        at: 0,
        count: 2,
    })
    .unwrap();
    // The formula went to C3 and its reference followed the data, so the
    // block is now C3:C4 and the old cells are empty.
    assert_eq!(at(&e, "C3"), Value::Text("b".into()));
    assert_eq!(at(&e, "C4"), Value::Text("a".into()));
    assert_eq!(at(&e, "C1"), Value::Empty);
    assert_eq!(at(&e, "C2"), Value::Empty);
}

#[test]
fn a_block_survives_a_save_as_recalculated_values() {
    // xlsx has no concept of our overlay, so what a spilled cell becomes on
    // disk is a written value. What must not happen is the block vanishing.
    use engine::io::xlsx;
    let mut e = Engine::new();
    source(&mut e);
    set(&mut e, "C1", "=UNIQUE(A1:A3)");

    let bytes = xlsx::export(&e.wb).expect("export");
    let back = xlsx::import(&bytes).expect("import").engine;
    assert_eq!(back.value_at("Sheet1", "C1"), Value::Text("b".into()));
    assert_eq!(
        back.value_at("Sheet1", "C2"),
        Value::Text("a".into()),
        "the spilled cell was lost on save"
    );
}
