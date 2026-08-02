//! The engine half of the 60fps target.
//!
//! The grid makes exactly one viewport call per repaint, so that call has to
//! be cheap enough to leave the frame budget to painting. A 60fps frame is
//! 16.6ms; we assert well under that on a 50k-cell sheet, with enough
//! headroom that this fails on a real regression rather than on CI noise.

use engine::{Action, CellAddr, Engine};
use std::time::Instant;

/// 50k populated cells: 5000 rows x 10 columns, every fifth column a formula.
fn big_sheet() -> Engine {
    let mut e = Engine::new();
    for row in 0..5000u32 {
        for col in 0..10u32 {
            let input = match col {
                4 => format!("=A{}+B{}", row + 1, row + 1),
                9 => format!("=SUM(A{}:D{})", row + 1, row + 1),
                _ => ((row * 10 + col) % 997).to_string(),
            };
            e.apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::new(row, col),
                input,
            })
            .expect("edit applies");
        }
    }
    e
}

/// Mirrors what the wasm `viewport` binding does per repaint.
fn read_viewport(e: &Engine, row0: u32, col0: u32, rows: u32, cols: u32) -> usize {
    let sheet = e.wb.sheet_by_name("Sheet1").expect("sheet exists");
    let mut painted = 0usize;
    for r in row0..row0 + rows {
        for c in col0..col0 + cols {
            if let Some(cell) = sheet.cells.get(&CellAddr::new(r, c)) {
                // The display string is the actual work the binding does.
                let _ = cell.value().display();
                painted += 1;
            }
        }
    }
    painted
}

#[test]
fn viewport_reads_are_well_inside_a_frame_budget() {
    let e = big_sheet();
    assert_eq!(e.wb.sheet_by_name("Sheet1").unwrap().cells.len(), 50_000);

    // A generous viewport: 60 rows x 20 columns, larger than a typical screen.
    let mut worst = std::time::Duration::ZERO;
    for row0 in (0..4_900).step_by(377) {
        let start = Instant::now();
        let painted = read_viewport(&e, row0, 0, 60, 20);
        worst = worst.max(start.elapsed());
        assert!(painted > 0, "viewport at row {row0} was empty");
    }

    // 4ms leaves three quarters of a 60fps frame for painting. Debug builds
    // are several times slower than the release wasm the browser runs, so
    // this is a loose bound that still catches an accidental full scan.
    assert!(
        worst.as_millis() < 4,
        "worst viewport read took {worst:?}, over the 4ms budget"
    );
}

#[test]
fn editing_one_cell_does_not_rescan_the_sheet() {
    let mut e = big_sheet();
    // An edit whose dependents are a single formula must not cost anything
    // like a full recalculation of 50k cells.
    let start = Instant::now();
    e.apply(&Action::CellEdit {
        sheet: "Sheet1".into(),
        addr: CellAddr::new(2500, 0),
        input: "12345".into(),
    })
    .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 50,
        "single edit took {elapsed:?} on a 50k-cell sheet"
    );
}
