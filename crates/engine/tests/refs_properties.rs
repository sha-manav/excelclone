//! Property-based tests for reference rewriting and structural editing.
//!
//! These check the invariants that make the event log trustworthy: an
//! operation followed by its undo is the identity, references survive
//! round-trips, and no input sequence can panic the engine.

use engine::{Action, CellAddr, Engine, PasteMode, RangeAddr, SortKey};
use proptest::prelude::*;

/// A small grid keeps the search space tight while still exercising the
/// interesting overlaps between source, target and shifted regions.
const MAX_ROW: u32 = 7;
const MAX_COL: u32 = 5;

fn addr() -> impl Strategy<Value = CellAddr> {
    (0..MAX_ROW, 0..MAX_COL).prop_map(|(r, c)| CellAddr::new(r, c))
}

fn range() -> impl Strategy<Value = RangeAddr> {
    (addr(), addr()).prop_map(|(a, b)| RangeAddr::new(a, b))
}

/// Inputs that cover literals, formulas with every reference flavour, and
/// text — the cases whose rewriting rules differ.
fn cell_input() -> impl Strategy<Value = String> {
    prop_oneof![
        (0..100i64).prop_map(|n| n.to_string()),
        Just("=A1+B2".to_string()),
        Just("=$A$1*2".to_string()),
        Just("=$A1+A$1".to_string()),
        Just("=SUM(A1:B3)".to_string()),
        Just("=SUM(A1:A4)*C2".to_string()),
        Just("=IF(A1>2,B1,C1)".to_string()),
        Just("text".to_string()),
    ]
}

/// A workbook built from a handful of seeded cells.
fn seeded_engine(seeds: &[(CellAddr, String)]) -> Engine {
    let mut e = Engine::new();
    for (a, input) in seeds {
        // Some generated inputs are legitimately unparseable in context;
        // skipping them keeps the property about the ones that applied.
        let _ = e.apply(&Action::CellEdit {
            sheet: "Sheet1".into(),
            addr: *a,
            input: input.clone(),
        });
    }
    e
}

fn seeds() -> impl Strategy<Value = Vec<(CellAddr, String)>> {
    prop::collection::vec((addr(), cell_input()), 0..12)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// The flagship structural invariant: any operation followed by undo
    /// leaves the workbook exactly as it was.
    #[test]
    fn operation_then_undo_is_identity(
        seeds in seeds(),
        src in range(),
        dst_anchor in addr(),
        mode in prop_oneof![Just(PasteMode::Formulas), Just(PasteMode::Values)],
        cut in any::<bool>(),
        at in 0..MAX_ROW,
        count in 1..3u32,
        op in 0..7usize,
    ) {
        let mut e = seeded_engine(&seeds);
        let before = e.wb.state_snapshot();

        let dst = RangeAddr::single(dst_anchor);
        let action = match op {
            0 => Action::RangePaste {
                source_sheet: "Sheet1".into(),
                source: src,
                target_sheet: "Sheet1".into(),
                target: dst,
                mode,
                cut,
            },
            1 => Action::FillApply {
                sheet: "Sheet1".into(),
                // The fill target must contain the source.
                source: src,
                target: RangeAddr::new(
                    src.start,
                    CellAddr::new((src.end.row + count).min(MAX_ROW), src.end.col),
                ),
            },
            2 => Action::RowInsert { sheet: "Sheet1".into(), at, count },
            3 => Action::RowDelete { sheet: "Sheet1".into(), at, count },
            4 => Action::ColInsert { sheet: "Sheet1".into(), at: at % MAX_COL, count },
            5 => Action::ColDelete { sheet: "Sheet1".into(), at: at % MAX_COL, count },
            _ => Action::SortApply {
                sheet: "Sheet1".into(),
                range: src,
                keys: vec![SortKey { column: src.start.col, ascending: cut }],
                has_header: false,
            },
        };

        if e.apply(&action).is_ok() {
            e.apply(&Action::Undo).unwrap();
            prop_assert_eq!(e.wb.state_snapshot(), before);
        }
    }

    /// Replaying an action log from an empty workbook reproduces the exact
    /// same state — the property the whole product rests on.
    #[test]
    fn replay_reproduces_state(
        seeds in seeds(),
        src in range(),
        dst_anchor in addr(),
        at in 0..MAX_ROW,
        count in 1..3u32,
    ) {
        let mut log: Vec<Action> = seeds
            .iter()
            .map(|(a, input)| Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: *a,
                input: input.clone(),
            })
            .collect();
        log.push(Action::RangePaste {
            source_sheet: "Sheet1".into(),
            source: src,
            target_sheet: "Sheet1".into(),
            target: RangeAddr::single(dst_anchor),
            mode: PasteMode::Formulas,
            cut: false,
        });
        log.push(Action::RowInsert { sheet: "Sheet1".into(), at, count });
        log.push(Action::ColDelete { sheet: "Sheet1".into(), at: at % MAX_COL, count });
        log.push(Action::Undo);
        log.push(Action::Redo);

        let mut live = Engine::new();
        for a in &log {
            let _ = live.apply(a);
        }
        let mut replayed = Engine::new();
        for a in &log {
            let _ = replayed.apply(a);
        }
        prop_assert_eq!(live.wb.state_snapshot(), replayed.wb.state_snapshot());
    }

    /// Inserting rows and then deleting the same rows restores every
    /// reference, because nothing was destroyed in between.
    #[test]
    fn insert_then_delete_same_rows_restores_refs(
        seeds in seeds(),
        at in 0..MAX_ROW,
        count in 1..4u32,
    ) {
        let mut e = seeded_engine(&seeds);
        let before = e.wb.state_snapshot();
        e.apply(&Action::RowInsert { sheet: "Sheet1".into(), at, count }).unwrap();
        e.apply(&Action::RowDelete { sheet: "Sheet1".into(), at, count }).unwrap();
        prop_assert_eq!(e.wb.state_snapshot(), before);
    }

    /// The same for columns.
    #[test]
    fn insert_then_delete_same_cols_restores_refs(
        seeds in seeds(),
        at in 0..MAX_COL,
        count in 1..4u32,
    ) {
        let mut e = seeded_engine(&seeds);
        let before = e.wb.state_snapshot();
        e.apply(&Action::ColInsert { sheet: "Sheet1".into(), at, count }).unwrap();
        e.apply(&Action::ColDelete { sheet: "Sheet1".into(), at, count }).unwrap();
        prop_assert_eq!(e.wb.state_snapshot(), before);
    }

    /// No sequence of structural edits may panic or hang, and incremental
    /// recalculation must always agree with a full one — otherwise the same
    /// workbook would hold different values depending on how it was reached,
    /// and replaying a log would not reproduce the live state.
    #[test]
    fn incremental_and_full_recalc_agree(
        seeds in seeds(),
        ops in prop::collection::vec(
            (0..7usize, addr(), range(), 0..MAX_ROW, 1..3u32),
            0..8,
        ),
    ) {
        let mut e = seeded_engine(&seeds);
        for (op, anchor, rng, at, count) in ops {
            let action = match op {
                0 => Action::RangePaste {
                    source_sheet: "Sheet1".into(),
                    source: rng,
                    target_sheet: "Sheet1".into(),
                    target: RangeAddr::single(anchor),
                    mode: PasteMode::Formulas,
                    cut: false,
                },
                1 => Action::RangeClear { sheet: "Sheet1".into(), range: rng },
                2 => Action::RowInsert { sheet: "Sheet1".into(), at, count },
                3 => Action::RowDelete { sheet: "Sheet1".into(), at, count },
                4 => Action::ColInsert { sheet: "Sheet1".into(), at: at % MAX_COL, count },
                5 => Action::ColDelete { sheet: "Sheet1".into(), at: at % MAX_COL, count },
                _ => Action::Undo,
            };
            let _ = e.apply(&action);
        }
        let incremental = e.wb.state_snapshot();
        e.recalc_all();
        prop_assert_eq!(e.wb.state_snapshot(), incremental);
    }
}
