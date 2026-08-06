//! M2 integration tests: copy/cut/paste, fill, insert/delete, sort, filter,
//! merge, and undo/redo. Expected values are Excel-verified.

use engine::{Action, Axis, CellAddr, Engine, FilterSpec, PasteMode, RangeAddr, SortKey, Value};
use std::collections::BTreeMap;

fn a1(s: &str) -> CellAddr {
    CellAddr::parse_a1(s).unwrap()
}

fn r(s: &str) -> RangeAddr {
    RangeAddr::parse_a1(s).unwrap()
}

fn set(e: &mut Engine, cell: &str, input: &str) {
    e.apply(&Action::CellEdit {
        sheet: "Sheet1".into(),
        addr: a1(cell),
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

/// The formula-bar text of a cell.
fn input(e: &Engine, cell: &str) -> String {
    e.wb.sheet_by_name("Sheet1")
        .unwrap()
        .cells
        .get(&a1(cell))
        .map(|c| c.input())
        .unwrap_or_default()
}

fn paste(e: &mut Engine, src: &str, dst: &str, mode: PasteMode, cut: bool) {
    e.apply(&Action::RangePaste {
        source_sheet: "Sheet1".into(),
        source: r(src),
        target_sheet: "Sheet1".into(),
        target: r(dst),
        mode,
        cut,
    })
    .unwrap();
}

#[test]
fn copy_paste_adjusts_relative_keeps_absolute() {
    let mut e = Engine::new();
    set(&mut e, "A1", "10");
    set(&mut e, "A2", "20");
    set(&mut e, "B1", "=A1*2");
    set(&mut e, "B2", "=$A$1*2");

    paste(&mut e, "B1:B2", "C1:C2", PasteMode::Formulas, false);
    assert_eq!(input(&e, "C1"), "=B1*2");
    assert_eq!(input(&e, "C2"), "=$A$1*2");
    // C1 reads B1 (=20), C2 still reads A1 (=10).
    assert_eq!(num(&e, "C1"), 40.0);
    assert_eq!(num(&e, "C2"), 20.0);
}

#[test]
fn paste_values_freezes_results() {
    let mut e = Engine::new();
    set(&mut e, "A1", "5");
    set(&mut e, "B1", "=A1*3");
    assert_eq!(num(&e, "B1"), 15.0);

    paste(&mut e, "B1", "C1", PasteMode::Values, false);
    assert_eq!(input(&e, "C1"), "15");
    // Changing the source leaves the pasted value untouched.
    set(&mut e, "A1", "100");
    assert_eq!(num(&e, "B1"), 300.0);
    assert_eq!(num(&e, "C1"), 15.0);
}

#[test]
fn paste_tiles_into_exact_multiples() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "A2", "2");
    paste(&mut e, "A1:A2", "B1:B6", PasteMode::Formulas, false);
    let got: Vec<f64> = ["B1", "B2", "B3", "B4", "B5", "B6"]
        .iter()
        .map(|c| num(&e, c))
        .collect();
    assert_eq!(got, vec![1.0, 2.0, 1.0, 2.0, 1.0, 2.0]);
}

#[test]
fn cut_paste_moves_cells_and_retargets_refs() {
    let mut e = Engine::new();
    set(&mut e, "A1", "7");
    set(&mut e, "B1", "=A1+1");
    set(&mut e, "C1", "=A1*10");

    // Move A1 to E1: the cell moves, and both formulas follow it.
    paste(&mut e, "A1", "E1", PasteMode::Formulas, true);
    assert_eq!(val(&e, "A1"), Value::Empty);
    assert_eq!(num(&e, "E1"), 7.0);
    assert_eq!(input(&e, "B1"), "=E1+1");
    assert_eq!(input(&e, "C1"), "=E1*10");
    assert_eq!(num(&e, "B1"), 8.0);
    assert_eq!(num(&e, "C1"), 70.0);
}

#[test]
fn cut_paste_keeps_formula_refs_verbatim() {
    let mut e = Engine::new();
    set(&mut e, "A1", "3");
    set(&mut e, "B1", "=A1*2");
    // Moving the formula itself must not re-point it.
    paste(&mut e, "B1", "B5", PasteMode::Formulas, true);
    assert_eq!(input(&e, "B5"), "=A1*2");
    assert_eq!(num(&e, "B5"), 6.0);
}

#[test]
fn fill_down_extends_formulas_and_series() {
    let mut e = Engine::new();
    for (i, v) in ["10", "20", "30", "40"].iter().enumerate() {
        set(&mut e, &format!("A{}", i + 1), v);
    }
    set(&mut e, "B1", "=A1*2");
    e.apply(&Action::FillApply {
        sheet: "Sheet1".into(),
        source: r("B1"),
        target: r("B1:B4"),
    })
    .unwrap();
    assert_eq!(input(&e, "B4"), "=A4*2");
    assert_eq!(num(&e, "B4"), 80.0);

    // Two numeric seeds establish a step.
    set(&mut e, "C1", "1");
    set(&mut e, "C2", "3");
    e.apply(&Action::FillApply {
        sheet: "Sheet1".into(),
        source: r("C1:C2"),
        target: r("C1:C5"),
    })
    .unwrap();
    assert_eq!(num(&e, "C3"), 5.0);
    assert_eq!(num(&e, "C4"), 7.0);
    assert_eq!(num(&e, "C5"), 9.0);

    // A single number is copied, not incremented (Excel needs two cells).
    set(&mut e, "D1", "42");
    e.apply(&Action::FillApply {
        sheet: "Sheet1".into(),
        source: r("D1"),
        target: r("D1:D3"),
    })
    .unwrap();
    assert_eq!(num(&e, "D3"), 42.0);

    // Text with a trailing integer increments from a single cell.
    set(&mut e, "E1", "Item 7");
    e.apply(&Action::FillApply {
        sheet: "Sheet1".into(),
        source: r("E1"),
        target: r("E1:E3"),
    })
    .unwrap();
    assert_eq!(val(&e, "E2"), Value::Text("Item 8".into()));
    assert_eq!(val(&e, "E3"), Value::Text("Item 9".into()));
}

#[test]
fn fill_right_extends_across_columns() {
    let mut e = Engine::new();
    set(&mut e, "A1", "5");
    set(&mut e, "A2", "=A1*2");
    e.apply(&Action::FillApply {
        sheet: "Sheet1".into(),
        source: r("A2"),
        target: r("A2:C2"),
    })
    .unwrap();
    assert_eq!(input(&e, "C2"), "=C1*2");
}

#[test]
fn insert_rows_shifts_cells_and_refs() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "A2", "2");
    set(&mut e, "A3", "3");
    set(&mut e, "B1", "=SUM(A1:A3)");
    assert_eq!(num(&e, "B1"), 6.0);

    // Insert one row at row 2 (0-based index 1).
    e.apply(&Action::RowInsert {
        sheet: "Sheet1".into(),
        at: 1,
        count: 1,
    })
    .unwrap();
    assert_eq!(num(&e, "A1"), 1.0);
    assert_eq!(val(&e, "A2"), Value::Empty);
    assert_eq!(num(&e, "A3"), 2.0);
    assert_eq!(num(&e, "A4"), 3.0);
    // The range widened to cover the inserted row.
    assert_eq!(input(&e, "B1"), "=SUM(A1:A4)");
    assert_eq!(num(&e, "B1"), 6.0);
}

#[test]
fn delete_rows_breaks_refs_to_deleted_targets() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "A2", "2");
    set(&mut e, "A3", "3");
    // Both observers live in row 1 so they survive the deletion themselves.
    set(&mut e, "C1", "=A2*10");
    set(&mut e, "D1", "=SUM(A1:A3)");

    // Delete row 2 (0-based index 1).
    e.apply(&Action::RowDelete {
        sheet: "Sheet1".into(),
        at: 1,
        count: 1,
    })
    .unwrap();
    assert_eq!(num(&e, "A2"), 3.0);
    // The direct ref to the deleted row is #REF!.
    assert_eq!(input(&e, "C1"), "=#REF!*10");
    assert_eq!(val(&e, "C1"), Value::Error(engine::ErrorKind::Ref));
    // The range shrank but survives.
    assert_eq!(input(&e, "D1"), "=SUM(A1:A2)");
    assert_eq!(num(&e, "D1"), 4.0);
}

#[test]
fn insert_and_delete_columns() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "B1", "2");
    set(&mut e, "C1", "=A1+B1");
    e.apply(&Action::ColInsert {
        sheet: "Sheet1".into(),
        at: 1,
        count: 1,
    })
    .unwrap();
    // B shifted to C, the formula moved to D and follows its inputs.
    assert_eq!(num(&e, "C1"), 2.0);
    assert_eq!(input(&e, "D1"), "=A1+C1");

    e.apply(&Action::ColDelete {
        sheet: "Sheet1".into(),
        at: 1,
        count: 1,
    })
    .unwrap();
    assert_eq!(input(&e, "C1"), "=A1+B1");
    assert_eq!(num(&e, "C1"), 3.0);
}

#[test]
fn sort_single_and_multi_key() {
    let mut e = Engine::new();
    // Header + three data rows.
    set(&mut e, "A1", "name");
    set(&mut e, "B1", "score");
    set(&mut e, "A2", "carol");
    set(&mut e, "B2", "3");
    set(&mut e, "A3", "alice");
    set(&mut e, "B3", "5");
    set(&mut e, "A4", "bob");
    set(&mut e, "B4", "1");

    e.apply(&Action::SortApply {
        sheet: "Sheet1".into(),
        range: r("A1:B4"),
        keys: vec![SortKey {
            column: 0,
            ascending: true,
        }],
        has_header: true,
    })
    .unwrap();
    assert_eq!(val(&e, "A1"), Value::Text("name".into()));
    assert_eq!(val(&e, "A2"), Value::Text("alice".into()));
    assert_eq!(val(&e, "A3"), Value::Text("bob".into()));
    assert_eq!(val(&e, "A4"), Value::Text("carol".into()));
    // The whole row travels with its key.
    assert_eq!(num(&e, "B2"), 5.0);
    assert_eq!(num(&e, "B4"), 3.0);

    // Descending by score.
    e.apply(&Action::SortApply {
        sheet: "Sheet1".into(),
        range: r("A1:B4"),
        keys: vec![SortKey {
            column: 1,
            ascending: false,
        }],
        has_header: true,
    })
    .unwrap();
    assert_eq!(num(&e, "B2"), 5.0);
    assert_eq!(num(&e, "B3"), 3.0);
    assert_eq!(num(&e, "B4"), 1.0);
}

#[test]
fn sort_puts_blanks_last() {
    let mut e = Engine::new();
    set(&mut e, "A1", "b");
    set(&mut e, "A3", "a");
    e.apply(&Action::SortApply {
        sheet: "Sheet1".into(),
        range: r("A1:A3"),
        keys: vec![SortKey {
            column: 0,
            ascending: true,
        }],
        has_header: false,
    })
    .unwrap();
    assert_eq!(val(&e, "A1"), Value::Text("a".into()));
    assert_eq!(val(&e, "A2"), Value::Text("b".into()));
    assert_eq!(val(&e, "A3"), Value::Empty);
}

#[test]
fn filter_hides_rows_without_changing_values() {
    let mut e = Engine::new();
    set(&mut e, "A1", "status");
    set(&mut e, "A2", "open");
    set(&mut e, "A3", "closed");
    set(&mut e, "A4", "open");

    e.apply(&Action::FilterApply {
        sheet: "Sheet1".into(),
        spec: FilterSpec {
            range: r("A1:A4"),
            column: 0,
            allowed: vec!["open".into()],
        },
    })
    .unwrap();
    let hidden = &e.wb.sheet_by_name("Sheet1").unwrap().hidden_rows;
    assert_eq!(hidden, &vec![2]); // 0-based row 2 == A3
    assert_eq!(val(&e, "A3"), Value::Text("closed".into()));

    e.apply(&Action::FilterClear {
        sheet: "Sheet1".into(),
    })
    .unwrap();
    assert!(e.wb.sheet_by_name("Sheet1").unwrap().hidden_rows.is_empty());
}

#[test]
fn merge_keeps_anchor_and_clears_the_rest() {
    let mut e = Engine::new();
    set(&mut e, "A1", "title");
    set(&mut e, "B1", "gone");
    e.apply(&Action::MergeApply {
        sheet: "Sheet1".into(),
        range: r("A1:B1"),
    })
    .unwrap();
    assert_eq!(val(&e, "A1"), Value::Text("title".into()));
    assert_eq!(val(&e, "B1"), Value::Empty);
    assert_eq!(e.wb.sheet_by_name("Sheet1").unwrap().merged.len(), 1);

    e.apply(&Action::MergeClear {
        sheet: "Sheet1".into(),
        range: r("A1:B1"),
    })
    .unwrap();
    assert!(e.wb.sheet_by_name("Sheet1").unwrap().merged.is_empty());
}

#[test]
fn undo_redo_round_trips_every_operation() {
    let ops: Vec<Action> = vec![
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: a1("A1"),
            input: "1".into(),
        },
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: a1("A2"),
            input: "2".into(),
        },
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: a1("B1"),
            input: "=SUM(A1:A2)".into(),
        },
        Action::RangePaste {
            source_sheet: "Sheet1".into(),
            source: r("B1"),
            target_sheet: "Sheet1".into(),
            target: r("C1"),
            mode: PasteMode::Formulas,
            cut: false,
        },
        Action::FillApply {
            sheet: "Sheet1".into(),
            source: r("A1:A2"),
            target: r("A1:A6"),
        },
        Action::RowInsert {
            sheet: "Sheet1".into(),
            at: 0,
            count: 2,
        },
        Action::RowDelete {
            sheet: "Sheet1".into(),
            at: 0,
            count: 1,
        },
        Action::SortApply {
            sheet: "Sheet1".into(),
            range: r("A1:A8"),
            keys: vec![SortKey {
                column: 0,
                ascending: false,
            }],
            has_header: false,
        },
        Action::MergeApply {
            sheet: "Sheet1".into(),
            range: r("D1:E1"),
        },
        Action::SheetAdd { name: "Two".into() },
        Action::CellClear {
            sheet: "Sheet1".into(),
            addr: a1("A1"),
        },
    ];

    // Undoing everything must return to an empty workbook, and redoing
    // everything must return to the final state.
    let mut e = Engine::new();
    let empty = e.wb.state_snapshot();
    let mut checkpoints = Vec::new();
    for op in &ops {
        e.apply(op).unwrap();
        checkpoints.push(e.wb.state_snapshot());
    }
    let final_state = e.wb.state_snapshot();

    for i in (0..ops.len()).rev() {
        e.apply(&Action::Undo).unwrap();
        let expected = if i == 0 {
            empty.clone()
        } else {
            checkpoints[i - 1].clone()
        };
        assert_eq!(
            e.wb.state_snapshot(),
            expected,
            "state after undoing operation {i} ({:?})",
            ops[i]
        );
    }
    assert!(!e.can_undo());

    for (i, cp) in checkpoints.iter().enumerate() {
        e.apply(&Action::Redo).unwrap();
        assert_eq!(e.wb.state_snapshot(), *cp, "state after redoing op {i}");
    }
    assert_eq!(e.wb.state_snapshot(), final_state);
    assert!(!e.can_redo());
}

#[test]
fn paste_then_undo_is_identity() {
    let mut e = Engine::new();
    set(&mut e, "A1", "=1+1");
    set(&mut e, "B1", "seed");
    let before = e.wb.state_snapshot();
    paste(&mut e, "A1:B1", "A5:B5", PasteMode::Formulas, false);
    e.apply(&Action::Undo).unwrap();
    assert_eq!(e.wb.state_snapshot(), before);
}

#[test]
fn new_action_clears_redo_history() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    e.apply(&Action::Undo).unwrap();
    assert!(e.can_redo());
    set(&mut e, "A1", "2");
    assert!(!e.can_redo());
    assert!(e.apply(&Action::Redo).is_err());
}

#[test]
fn undo_on_empty_stack_errors() {
    let mut e = Engine::new();
    assert!(e.apply(&Action::Undo).is_err());
    assert!(e.apply(&Action::Redo).is_err());
}

#[test]
fn structural_actions_replay_deterministically() {
    let ops: Vec<Action> = vec![
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: a1("A1"),
            input: "5".into(),
        },
        Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: a1("A2"),
            input: "=A1*2".into(),
        },
        Action::FillApply {
            sheet: "Sheet1".into(),
            source: r("A2"),
            target: r("A2:A6"),
        },
        Action::RangePaste {
            source_sheet: "Sheet1".into(),
            source: r("A1:A6"),
            target_sheet: "Sheet1".into(),
            target: r("C1"),
            mode: PasteMode::Formulas,
            cut: false,
        },
        Action::RowInsert {
            sheet: "Sheet1".into(),
            at: 2,
            count: 3,
        },
        Action::ColDelete {
            sheet: "Sheet1".into(),
            at: 1,
            count: 1,
        },
        Action::Undo,
        Action::Redo,
        Action::SortApply {
            sheet: "Sheet1".into(),
            range: r("A1:A9"),
            keys: vec![SortKey {
                column: 0,
                ascending: true,
            }],
            has_header: false,
        },
    ];
    let mut live = Engine::new();
    for op in &ops {
        live.apply(op).unwrap();
    }
    let mut replayed = Engine::new();
    for op in &ops {
        replayed.apply(op).unwrap();
    }
    assert_eq!(live.wb.state_snapshot(), replayed.wb.state_snapshot());
}

// ---------------------------------------------------------------------------
// Resize
// ---------------------------------------------------------------------------

fn resize(
    e: &mut Engine,
    axis: Axis,
    at: u32,
    count: u32,
    size: Option<f64>,
) -> Vec<engine::Event> {
    e.apply(&Action::Resize {
        sheet: "Sheet1".into(),
        axis,
        at,
        count,
        size,
    })
    .unwrap()
}

fn widths(e: &Engine) -> BTreeMap<u32, f64> {
    e.wb.sheet_by_name("Sheet1").unwrap().col_widths.clone()
}

#[test]
fn a_resize_covers_the_run_it_names_and_undoes_as_one_step() {
    let mut e = Engine::new();
    resize(&mut e, Axis::Col, 1, 3, Some(150.0));
    assert_eq!(
        widths(&e),
        BTreeMap::from([(1, 150.0), (2, 150.0), (3, 150.0)])
    );

    // Dragging three column borders at once is one gesture, so one Ctrl+Z.
    e.apply(&Action::Undo).unwrap();
    assert!(widths(&e).is_empty());
    e.apply(&Action::Redo).unwrap();
    assert_eq!(widths(&e).len(), 3);
}

#[test]
fn clearing_a_size_restores_the_default_rather_than_recording_one() {
    let mut e = Engine::new();
    resize(&mut e, Axis::Col, 0, 1, Some(150.0));
    resize(&mut e, Axis::Col, 0, 1, None);
    // Not "the default width, written down": absent. A file written from this
    // sheet must carry no <col> for column A at all.
    assert!(widths(&e).is_empty());
    e.apply(&Action::Undo).unwrap();
    assert_eq!(widths(&e).get(&0), Some(&150.0));
}

#[test]
fn a_resize_that_changes_nothing_is_not_an_undo_step() {
    let mut e = Engine::new();
    set(&mut e, "A1", "keep me");
    // Clearing a size that was never set, and setting the size a column
    // already has. Neither is a change, and neither may bury the cell edit
    // under a no-op the next Ctrl+Z would spend itself on.
    assert!(resize(&mut e, Axis::Col, 0, 1, None).is_empty());
    resize(&mut e, Axis::Col, 0, 1, Some(150.0));
    assert!(resize(&mut e, Axis::Col, 0, 1, Some(150.0)).is_empty());

    e.apply(&Action::Undo).unwrap();
    assert!(widths(&e).is_empty(), "undo skipped past the resize");
    e.apply(&Action::Undo).unwrap();
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Empty);
}

#[test]
fn a_size_must_be_a_size() {
    let mut e = Engine::new();
    for bad in [0.0, -10.0, f64::NAN, f64::INFINITY] {
        assert!(
            e.apply(&Action::Resize {
                sheet: "Sheet1".into(),
                axis: Axis::Col,
                at: 0,
                count: 1,
                size: Some(bad),
            })
            .is_err(),
            "{bad} was accepted as a width"
        );
    }
    // Hiding a column is a separate feature; a zero width must not become the
    // back door into it.
    assert!(widths(&e).is_empty());
}

#[test]
fn widths_travel_with_the_columns_they_belong_to() {
    let mut e = Engine::new();
    resize(&mut e, Axis::Col, 2, 1, Some(150.0));
    e.apply(&Action::ColInsert {
        sheet: "Sheet1".into(),
        at: 0,
        count: 2,
    })
    .unwrap();
    assert_eq!(
        widths(&e),
        BTreeMap::from([(4, 150.0)]),
        "the width stayed behind on the wrong column"
    );

    // And a deleted column takes its width with it rather than leaving it for
    // whoever slides into the slot.
    e.apply(&Action::ColDelete {
        sheet: "Sheet1".into(),
        at: 4,
        count: 1,
    })
    .unwrap();
    assert!(widths(&e).is_empty());
}

#[test]
fn row_heights_shift_with_inserted_rows() {
    let mut e = Engine::new();
    resize(&mut e, Axis::Row, 5, 1, Some(40.0));
    e.apply(&Action::RowInsert {
        sheet: "Sheet1".into(),
        at: 0,
        count: 3,
    })
    .unwrap();
    let heights = &e.wb.sheet_by_name("Sheet1").unwrap().row_heights;
    assert_eq!(*heights, BTreeMap::from([(8, 40.0)]));
    // The columns were not the axis, so they were not touched.
    assert!(widths(&e).is_empty());
}
