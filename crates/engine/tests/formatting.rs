//! M5 integration tests: per-cell formatting and find/replace.
//!
//! The interesting cases are the ones where formatting and contents have to
//! move together — or deliberately not together. Sorting a table must carry a
//! bold total row along; pressing Delete must not.

use engine::{
    Action, BorderPreset, CellAddr, CellFormat, Engine, FormatPatch, HAlign, PasteMode, RangeAddr,
    SortKey, Value,
};

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

fn fmt(e: &mut Engine, range: &str, patches: Vec<FormatPatch>) {
    e.apply(&Action::FormatApply {
        sheet: "Sheet1".into(),
        range: r(range),
        patches,
    })
    .unwrap();
}

/// The resolved format at a cell — the default when it has none.
fn format_at(e: &Engine, cell: &str) -> CellFormat {
    let s = e.wb.sheet_by_name("Sheet1").unwrap();
    e.wb.formats.resolve(s.format_id(a1(cell)))
}

fn input(e: &Engine, cell: &str) -> String {
    e.wb.sheet_by_name("Sheet1")
        .unwrap()
        .cells
        .get(&a1(cell))
        .map(|c| c.input())
        .unwrap_or_default()
}

/* ------------------------------------------------------------- basic apply */

#[test]
fn patches_compose_rather_than_replace() {
    let mut e = Engine::new();
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    fmt(
        &mut e,
        "A1",
        vec![FormatPatch::FillColor(Some("#ff0000".into()))],
    );
    let f = format_at(&e, "A1");
    assert!(f.bold, "the second patch cleared the first");
    assert_eq!(f.fill_color.as_deref(), Some("#ff0000"));
}

#[test]
fn a_format_needs_no_cell_underneath_it() {
    let mut e = Engine::new();
    fmt(&mut e, "C7", vec![FormatPatch::Italic(true)]);
    assert!(format_at(&e, "C7").italic);
    // ...and it does not conjure a cell into existence, which would show up
    // in the used range, in CSV export, and in COUNTA.
    let s = e.wb.sheet_by_name("Sheet1").unwrap();
    assert!(s.cells.is_empty());
    assert_eq!(s.used_range(), None);
    assert_eq!(s.painted_range(), Some(r("C7")));
}

#[test]
fn returning_to_the_default_drops_the_entry() {
    let mut e = Engine::new();
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    assert_eq!(e.wb.sheet_by_name("Sheet1").unwrap().formats.len(), 1);
    fmt(&mut e, "A1", vec![FormatPatch::Bold(false)]);
    assert!(
        e.wb.sheet_by_name("Sheet1").unwrap().formats.is_empty(),
        "an unformatted cell should hold no entry at all"
    );
}

#[test]
fn identical_formats_across_cells_share_one_table_entry() {
    let mut e = Engine::new();
    fmt(&mut e, "A1:A100", vec![FormatPatch::Bold(true)]);
    assert_eq!(e.wb.sheet_by_name("Sheet1").unwrap().formats.len(), 100);
    assert_eq!(e.wb.formats.len(), 1, "100 cells should intern to 1 format");
}

#[test]
fn outline_borders_only_touch_the_perimeter() {
    let mut e = Engine::new();
    fmt(
        &mut e,
        "B2:D4",
        vec![FormatPatch::Border(BorderPreset::All)],
    );
    assert!(format_at(&e, "C3").borders.top);
    fmt(
        &mut e,
        "B2:D4",
        vec![FormatPatch::Border(BorderPreset::Outline)],
    );
    // The interior is reset by the same gesture that draws the perimeter.
    assert!(format_at(&e, "C3").borders.is_none());
    assert!(format_at(&e, "B2").borders.top && format_at(&e, "B2").borders.left);
    assert!(format_at(&e, "D4").borders.bottom && format_at(&e, "D4").borders.right);
}

#[test]
fn formatting_a_huge_range_is_refused_rather_than_attempted() {
    let mut e = Engine::new();
    let err = e
        .apply(&Action::FormatApply {
            sheet: "Sheet1".into(),
            range: RangeAddr::new(CellAddr::new(0, 0), CellAddr::new(1_000_000, 0)),
            patches: vec![FormatPatch::Bold(true)],
        })
        .unwrap_err();
    assert!(
        err.to_string().contains("exceeds"),
        "expected a loud refusal, got {err}"
    );
    assert!(e.wb.sheet_by_name("Sheet1").unwrap().formats.is_empty());
}

#[test]
fn an_empty_patch_list_is_refused() {
    let mut e = Engine::new();
    assert!(e
        .apply(&Action::FormatApply {
            sheet: "Sheet1".into(),
            range: r("A1"),
            patches: vec![],
        })
        .is_err());
}

/* ------------------------------------------------ contents versus dressing */

#[test]
fn delete_clears_contents_and_keeps_formatting() {
    // Excel's semantics, and the reason "Clear Formats" is a separate menu
    // item: retyping into a formatted cell must not lose its formatting.
    let mut e = Engine::new();
    set(&mut e, "A1", "42");
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    e.apply(&Action::RangeClear {
        sheet: "Sheet1".into(),
        range: r("A1"),
    })
    .unwrap();
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Empty);
    assert!(format_at(&e, "A1").bold);
}

#[test]
fn clear_formatting_keeps_contents() {
    let mut e = Engine::new();
    set(&mut e, "A1", "42");
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    e.apply(&Action::FormatClear {
        sheet: "Sheet1".into(),
        range: r("A1"),
    })
    .unwrap();
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Number(42.0));
    assert!(format_at(&e, "A1").is_default());
}

#[test]
fn a_number_format_does_not_change_the_value() {
    // Formatting is display only: SUM must not start summing formatted text.
    let mut e = Engine::new();
    set(&mut e, "A1", "0.5");
    set(&mut e, "A2", "=A1*2");
    fmt(
        &mut e,
        "A1",
        vec![FormatPatch::NumberFormat(Some("0.00%".into()))],
    );
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Number(0.5));
    assert_eq!(e.value_at("Sheet1", "A2"), Value::Number(1.0));
}

/* --------------------------------------------------------- moving with data */

#[test]
fn sorting_carries_formatting_with_the_row() {
    let mut e = Engine::new();
    for (i, name) in ["Zoe", "Ada", "Mia"].iter().enumerate() {
        set(&mut e, &format!("A{}", i + 1), name);
    }
    // Mark the row that must stay marked wherever it lands.
    fmt(
        &mut e,
        "A1",
        vec![FormatPatch::FillColor(Some("#ffff00".into()))],
    );
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
    assert_eq!(input(&e, "A3"), "Zoe");
    assert_eq!(
        format_at(&e, "A3").fill_color.as_deref(),
        Some("#ffff00"),
        "the highlight stayed at the old position instead of following Zoe"
    );
    assert!(format_at(&e, "A1").is_default());
}

#[test]
fn inserting_rows_shifts_formatting_too() {
    let mut e = Engine::new();
    fmt(&mut e, "A5", vec![FormatPatch::Bold(true)]);
    e.apply(&Action::RowInsert {
        sheet: "Sheet1".into(),
        at: 0,
        count: 2,
    })
    .unwrap();
    assert!(format_at(&e, "A7").bold);
    assert!(format_at(&e, "A5").is_default());
}

#[test]
fn deleting_a_column_takes_its_formatting_with_it() {
    let mut e = Engine::new();
    fmt(&mut e, "B1", vec![FormatPatch::Bold(true)]);
    fmt(&mut e, "C1", vec![FormatPatch::Italic(true)]);
    e.apply(&Action::ColDelete {
        sheet: "Sheet1".into(),
        at: 1,
        count: 1,
    })
    .unwrap();
    assert!(format_at(&e, "B1").italic, "C should have slid into B");
    assert!(!format_at(&e, "B1").bold, "B's own format should be gone");
}

#[test]
fn paste_carries_formatting_but_paste_values_does_not() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);

    e.apply(&Action::RangePaste {
        source_sheet: "Sheet1".into(),
        source: r("A1"),
        target_sheet: "Sheet1".into(),
        target: r("B1"),
        mode: PasteMode::Formulas,
        cut: false,
    })
    .unwrap();
    assert!(format_at(&e, "B1").bold);

    e.apply(&Action::RangePaste {
        source_sheet: "Sheet1".into(),
        source: r("A1"),
        target_sheet: "Sheet1".into(),
        target: r("C1"),
        mode: PasteMode::Values,
        cut: false,
    })
    .unwrap();
    assert!(
        format_at(&e, "C1").is_default(),
        "paste-values should paste the value without the dressing"
    );
}

#[test]
fn a_cut_leaves_no_formatting_behind() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    e.apply(&Action::RangePaste {
        source_sheet: "Sheet1".into(),
        source: r("A1"),
        target_sheet: "Sheet1".into(),
        target: r("B1"),
        mode: PasteMode::Formulas,
        cut: true,
    })
    .unwrap();
    assert!(format_at(&e, "B1").bold);
    assert!(format_at(&e, "A1").is_default());
}

#[test]
fn a_fill_drags_the_seed_formatting_down() {
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    set(&mut e, "A2", "2");
    fmt(
        &mut e,
        "A1:A2",
        vec![FormatPatch::NumberFormat(Some("0.00".into()))],
    );
    e.apply(&Action::FillApply {
        sheet: "Sheet1".into(),
        source: r("A1:A2"),
        target: r("A1:A6"),
    })
    .unwrap();
    assert_eq!(e.value_at("Sheet1", "A6"), Value::Number(6.0));
    for cell in ["A3", "A4", "A5", "A6"] {
        assert_eq!(
            format_at(&e, cell).number_format.as_deref(),
            Some("0.00"),
            "{cell} did not inherit the seed's number format"
        );
    }
}

#[test]
fn a_fill_over_formatted_cells_replaces_their_formatting() {
    // The seed wins: filling an unformatted seed over a formatted target
    // clears the target, exactly as dragging in Excel does.
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    fmt(&mut e, "A3", vec![FormatPatch::Bold(true)]);
    e.apply(&Action::FillApply {
        sheet: "Sheet1".into(),
        source: r("A1"),
        target: r("A1:A3"),
    })
    .unwrap();
    assert!(format_at(&e, "A3").is_default());
}

/* ------------------------------------------------------------------- undo */

#[test]
fn undo_restores_formatting_without_touching_contents() {
    let mut e = Engine::new();
    set(&mut e, "A1", "42");
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    e.apply(&Action::Undo).unwrap();
    assert!(format_at(&e, "A1").is_default());
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Number(42.0));
    e.apply(&Action::Redo).unwrap();
    assert!(format_at(&e, "A1").bold);
}

#[test]
fn undoing_a_paste_restores_both_halves_together() {
    // A paste is one gesture, so its undo must not leave the pasted
    // formatting standing over restored contents.
    let mut e = Engine::new();
    set(&mut e, "A1", "1");
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    set(&mut e, "B1", "9");
    fmt(&mut e, "B1", vec![FormatPatch::Italic(true)]);

    e.apply(&Action::RangePaste {
        source_sheet: "Sheet1".into(),
        source: r("A1"),
        target_sheet: "Sheet1".into(),
        target: r("B1"),
        mode: PasteMode::Formulas,
        cut: false,
    })
    .unwrap();
    e.apply(&Action::Undo).unwrap();
    assert_eq!(e.value_at("Sheet1", "B1"), Value::Number(9.0));
    let f = format_at(&e, "B1");
    assert!(f.italic && !f.bold, "B1 kept the pasted formatting: {f:?}");
}

#[test]
fn undoing_a_sort_puts_formatting_back_where_it_started() {
    let mut e = Engine::new();
    set(&mut e, "A1", "Zoe");
    set(&mut e, "A2", "Ada");
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    e.apply(&Action::SortApply {
        sheet: "Sheet1".into(),
        range: r("A1:A2"),
        keys: vec![SortKey {
            column: 0,
            ascending: true,
        }],
        has_header: false,
    })
    .unwrap();
    assert!(format_at(&e, "A2").bold);
    e.apply(&Action::Undo).unwrap();
    assert_eq!(input(&e, "A1"), "Zoe");
    assert!(format_at(&e, "A1").bold);
    assert!(format_at(&e, "A2").is_default());
}

/* --------------------------------------------------------- find & replace */

fn replace(e: &mut Engine, find: &str, to: &str, match_case: bool, whole_cell: bool) -> u32 {
    let events = e
        .apply(&Action::FindReplace {
            sheet: "Sheet1".into(),
            range: None,
            find: find.into(),
            replace: to.into(),
            match_case,
            whole_cell,
        })
        .unwrap();
    events
        .iter()
        .find_map(|ev| match ev {
            engine::Event::Replaced { cells, .. } => Some(*cells),
            _ => None,
        })
        .expect("a Replaced event")
}

#[test]
fn replace_rewrites_formula_source_not_results() {
    let mut e = Engine::new();
    set(&mut e, "A1", "2");
    set(&mut e, "A2", "3");
    set(&mut e, "B1", "=SUM(A1:A2)");
    assert_eq!(replace(&mut e, "SUM", "MAX", true, false), 1);
    assert_eq!(input(&e, "B1"), "=MAX(A1:A2)");
    assert_eq!(e.value_at("Sheet1", "B1"), Value::Number(3.0));
}

#[test]
fn a_replacement_can_turn_a_literal_into_a_formula() {
    let mut e = Engine::new();
    set(&mut e, "A1", "7");
    set(&mut e, "A2", "TOTAL");
    assert_eq!(replace(&mut e, "TOTAL", "=A1*2", true, true), 1);
    assert_eq!(e.value_at("Sheet1", "A2"), Value::Number(14.0));
}

#[test]
fn replacing_with_nothing_clears_the_cell() {
    let mut e = Engine::new();
    set(&mut e, "A1", "draft");
    set(&mut e, "B1", "=COUNTBLANK(A1:A1)");
    assert_eq!(replace(&mut e, "draft", "", true, true), 1);
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Empty);
    // Cleared, not blanked with an empty string: COUNTBLANK must see it.
    assert_eq!(e.value_at("Sheet1", "B1"), Value::Number(1.0));
}

#[test]
fn whole_cell_matching_does_not_match_substrings() {
    let mut e = Engine::new();
    set(&mut e, "A1", "pro");
    set(&mut e, "A2", "professional");
    assert_eq!(replace(&mut e, "pro", "premium", false, true), 1);
    assert_eq!(input(&e, "A1"), "premium");
    assert_eq!(input(&e, "A2"), "professional");
}

#[test]
fn substring_matching_replaces_every_occurrence_in_a_cell() {
    let mut e = Engine::new();
    set(&mut e, "A1", "ab-ab-ab");
    assert_eq!(replace(&mut e, "ab", "x", true, false), 1);
    assert_eq!(input(&e, "A1"), "x-x-x");
}

#[test]
fn case_sensitivity_is_honoured_in_both_directions() {
    let mut e = Engine::new();
    set(&mut e, "A1", "Draft");
    assert_eq!(replace(&mut e, "draft", "final", true, false), 0);
    assert_eq!(input(&e, "A1"), "Draft");
    assert_eq!(replace(&mut e, "draft", "final", false, false), 1);
    assert_eq!(input(&e, "A1"), "final");
}

#[test]
fn a_case_insensitive_replacement_preserves_surrounding_text_exactly() {
    // The failure mode this guards: finding offsets in a folded copy of the
    // string and splicing them into the original. Any length change between
    // the two corrupts the text around the match.
    let mut e = Engine::new();
    set(&mut e, "A1", "café DRAFT café");
    assert_eq!(replace(&mut e, "draft", "final", false, false), 1);
    assert_eq!(input(&e, "A1"), "café final café");
}

#[test]
fn replacing_nothing_reports_nothing() {
    let mut e = Engine::new();
    set(&mut e, "A1", "hello");
    assert_eq!(replace(&mut e, "zzz", "x", true, false), 0);
    assert_eq!(input(&e, "A1"), "hello");
}

#[test]
fn an_empty_search_term_is_refused() {
    let mut e = Engine::new();
    assert!(e
        .apply(&Action::FindReplace {
            sheet: "Sheet1".into(),
            range: None,
            find: String::new(),
            replace: "x".into(),
            match_case: false,
            whole_cell: false,
        })
        .is_err());
}

#[test]
fn a_replacement_that_would_not_parse_leaves_that_cell_alone() {
    let mut e = Engine::new();
    set(&mut e, "A1", "ok");
    set(&mut e, "A2", "ok");
    // "=1+" is not a formula; A1 must survive while A2 still gets its edit.
    let replaced = replace(&mut e, "ok", "=1+", true, true);
    assert_eq!(replaced, 0, "neither cell should have changed");
    assert_eq!(input(&e, "A1"), "ok");
    assert_eq!(input(&e, "A2"), "ok");
}

#[test]
fn replace_is_scoped_to_its_range_when_given_one() {
    let mut e = Engine::new();
    set(&mut e, "A1", "x");
    set(&mut e, "B1", "x");
    e.apply(&Action::FindReplace {
        sheet: "Sheet1".into(),
        range: Some(r("A1")),
        find: "x".into(),
        replace: "y".into(),
        match_case: true,
        whole_cell: true,
    })
    .unwrap();
    assert_eq!(input(&e, "A1"), "y");
    assert_eq!(input(&e, "B1"), "x");
}

#[test]
fn replace_all_undoes_as_one_step() {
    let mut e = Engine::new();
    for i in 1..=5 {
        set(&mut e, &format!("A{i}"), "draft");
    }
    assert_eq!(replace(&mut e, "draft", "final", true, true), 5);
    e.apply(&Action::Undo).unwrap();
    for i in 1..=5 {
        assert_eq!(input(&e, &format!("A{i}")), "draft", "row {i}");
    }
}

#[test]
fn find_matches_are_returned_in_reading_order() {
    let mut e = Engine::new();
    for cell in ["B2", "A1", "C3", "A3"] {
        set(&mut e, cell, "hit");
    }
    let found = e.find_matches("Sheet1", None, "HIT", false, true);
    let as_a1: Vec<String> = found.iter().map(|a| a.to_a1()).collect();
    assert_eq!(as_a1, vec!["A1", "B2", "A3", "C3"]);
}

#[test]
fn find_uses_the_same_rules_replace_does() {
    let mut e = Engine::new();
    set(&mut e, "A1", "Draft");
    assert!(e
        .find_matches("Sheet1", None, "draft", true, false)
        .is_empty());
    assert_eq!(
        e.find_matches("Sheet1", None, "draft", false, false).len(),
        1
    );
}

/* ----------------------------------------------------------------- events */

#[test]
fn format_events_report_the_attributes_touched() {
    let mut e = Engine::new();
    let events = e
        .apply(&Action::FormatApply {
            sheet: "Sheet1".into(),
            range: r("A1:B2"),
            patches: vec![
                FormatPatch::Bold(true),
                FormatPatch::Align(Some(HAlign::Right)),
            ],
        })
        .unwrap();
    match &events[0] {
        engine::Event::FormatApplied {
            attributes, cells, ..
        } => {
            assert_eq!(attributes, &["bold".to_string(), "align".to_string()]);
            assert_eq!(*cells, 4);
        }
        other => panic!("expected FormatApplied, got {other:?}"),
    }
}

#[test]
fn reapplying_the_same_format_reports_no_cells_changed() {
    let mut e = Engine::new();
    fmt(&mut e, "A1", vec![FormatPatch::Bold(true)]);
    let events = e
        .apply(&Action::FormatApply {
            sheet: "Sheet1".into(),
            range: r("A1"),
            patches: vec![FormatPatch::Bold(true)],
        })
        .unwrap();
    match &events[0] {
        engine::Event::FormatApplied { cells, .. } => assert_eq!(*cells, 0),
        other => panic!("expected FormatApplied, got {other:?}"),
    }
}

/* --------------------------------------------------------------- batches */

#[test]
fn a_batch_undoes_as_one_gesture() {
    // The bug this pins: applying a batch one action at a time pushes one
    // undo entry per action, so a five-step routine took five Ctrl+Z to
    // reject. Nobody would use a suggestion that costs that much to refuse.
    let mut e = Engine::new();
    set(&mut e, "A1", "seed");
    e.apply_batch(
        &[
            Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: a1("B1"),
                input: "=A1&\"!\"".into(),
            },
            Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: a1("C1"),
                input: "=B1&\"?\"".into(),
            },
            Action::FormatApply {
                sheet: "Sheet1".into(),
                range: r("B1:C1"),
                patches: vec![FormatPatch::Bold(true)],
            },
        ],
        "run routine",
    )
    .unwrap();
    assert_eq!(input(&e, "C1"), "=B1&\"?\"");
    assert!(format_at(&e, "B1").bold);

    e.apply(&Action::Undo).unwrap();
    assert_eq!(input(&e, "B1"), "", "one undo should take the whole batch");
    assert_eq!(input(&e, "C1"), "");
    assert!(format_at(&e, "B1").is_default());
    // ...and no further, so the work before the batch survives.
    assert_eq!(input(&e, "A1"), "seed");

    // Redo brings all of it back, also in one step.
    e.apply(&Action::Redo).unwrap();
    assert_eq!(input(&e, "C1"), "=B1&\"?\"");
    assert!(format_at(&e, "B1").bold);
}

#[test]
fn a_single_action_batch_is_still_one_undo_step() {
    let mut e = Engine::new();
    e.apply_batch(
        &[Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: a1("A1"),
            input: "1".into(),
        }],
        "run routine",
    )
    .unwrap();
    e.apply(&Action::Undo).unwrap();
    assert_eq!(input(&e, "A1"), "");
    assert!(!e.can_undo());
}

#[test]
fn a_batch_that_fails_part_way_leaves_what_it_did() {
    // Documented behaviour: the caller is looking at a partial result and
    // should be able to step back through it, so those entries stay separate.
    let mut e = Engine::new();
    let err = e.apply_batch(
        &[
            Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: a1("A1"),
                input: "1".into(),
            },
            Action::CellEdit {
                sheet: "NoSuchSheet".into(),
                addr: a1("A1"),
                input: "2".into(),
            },
        ],
        "run routine",
    );
    assert!(err.is_err());
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Number(1.0));
    e.apply(&Action::Undo).unwrap();
    assert_eq!(e.value_at("Sheet1", "A1"), Value::Empty);
}
