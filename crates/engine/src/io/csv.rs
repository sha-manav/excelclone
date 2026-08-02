//! CSV import and export.
//!
//! Import treats every row as data (no header row) and replays each field
//! through `Engine::apply`; a field starting with `=` is a formula. Export
//! writes the computed values of a sheet's used range, never formula text.

use super::{apply_cell, install_sheets, ImportResult, IoError};
use crate::addr::CellAddr;
use crate::engine::Engine;
use crate::model::{SheetId, Workbook};

/// Read CSV bytes into a single-sheet workbook named `sheet_name`.
pub fn import(bytes: &[u8], sheet_name: &str) -> Result<ImportResult, IoError> {
    let name = if sheet_name.trim().is_empty() {
        "Sheet1"
    } else {
        sheet_name
    };
    let mut engine = Engine::new();
    install_sheets(&mut engine, std::slice::from_ref(&name.to_string()))?;

    let mut warnings = Vec::new();
    let mut reader = ::csv::ReaderBuilder::new()
        .has_headers(false)
        // Ragged rows are common in the wild and are not an error here.
        .flexible(true)
        .from_reader(bytes);
    for (row, record) in reader.records().enumerate() {
        let record = record?;
        let Ok(row) = u32::try_from(row) else {
            return Err(IoError::Malformed("csv has too many rows".into()));
        };
        for (col, field) in record.iter().enumerate() {
            if field.is_empty() {
                continue;
            }
            let Ok(col) = u32::try_from(col) else {
                return Err(IoError::Malformed("csv row has too many fields".into()));
            };
            let addr = CellAddr::new(row, col);
            if !addr.is_valid() {
                return Err(IoError::Malformed(format!(
                    "csv cell at row {} col {} is outside the grid",
                    row + 1,
                    col + 1
                )));
            }
            apply_cell(&mut engine, name, addr, field, &mut warnings)?;
        }
    }
    Ok(ImportResult { engine, warnings })
}

/// Write one sheet's used range as CSV, using computed values.
pub fn export(wb: &Workbook, sheet: SheetId) -> Result<Vec<u8>, IoError> {
    let sheet = wb
        .sheet(sheet)
        .ok_or_else(|| IoError::SheetNotFound(format!("{:?}", sheet)))?;
    let mut writer = ::csv::WriterBuilder::new().from_writer(Vec::new());
    if let Some(range) = sheet.used_range() {
        // Rows hidden by the active filter are still data: CSV export dumps
        // the sheet, not the view, as Excel does.
        for row in range.start.row..=range.end.row {
            let fields: Vec<String> = (range.start.col..=range.end.col)
                .map(|col| sheet.value(CellAddr::new(row, col)).display())
                .collect();
            writer.write_record(&fields)?;
        }
    }
    writer.flush()?;
    writer
        .into_inner()
        .map_err(|e| IoError::Malformed(e.to_string()))
}
