//! File import and export: xlsx and CSV.
//!
//! Importers never touch the model directly: every imported cell is replayed
//! through `Engine::apply`, so the engine recalculates the workbook itself and
//! an import is indistinguishable from the user typing the file in.
//!
//! Anything we cannot model is reported as an `ImportWarning` rather than
//! dropped silently, and (for xlsx) the original zip package is retained so
//! export can write unmodeled parts back untouched.

pub mod csv;
pub(crate) mod styles;
pub mod xlsx;

use crate::addr::CellAddr;
use crate::engine::{Action, ApplyError, Engine};

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("xlsx read error: {0}")]
    XlsxRead(String),
    #[error("xlsx write error: {0}")]
    XlsxWrite(String),
    #[error("xml error: {0}")]
    Xml(String),
    #[error("csv error: {0}")]
    Csv(#[from] ::csv::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sheet not found: {0}")]
    SheetNotFound(String),
    #[error("malformed workbook: {0}")]
    Malformed(String),
    /// The workbook changed in a way the preserved package cannot express.
    #[error("cannot patch the original package: {0}")]
    Unrepresentable(String),
    /// The engine rejected imported content (bad address, duplicate sheet...).
    #[error("engine rejected imported content: {0}")]
    Apply(String),
}

impl From<calamine::XlsxError> for IoError {
    fn from(e: calamine::XlsxError) -> Self {
        IoError::XlsxRead(e.to_string())
    }
}

impl From<quick_xml::Error> for IoError {
    fn from(e: quick_xml::Error) -> Self {
        IoError::Xml(e.to_string())
    }
}

impl From<rust_xlsxwriter::XlsxError> for IoError {
    fn from(e: rust_xlsxwriter::XlsxError) -> Self {
        IoError::XlsxWrite(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportWarningKind {
    /// A workbook feature Gridline does not model (charts, pivots, VBA...).
    UnsupportedFeature,
    /// A formula our parser rejected; kept as text.
    UnparseableFormula,
    /// A sheet that is not a plain worksheet (chartsheet, macro sheet...).
    UnsupportedSheetType,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportWarning {
    pub kind: ImportWarningKind,
    pub detail: String,
}

impl ImportWarning {
    pub(crate) fn new(kind: ImportWarningKind, detail: impl Into<String>) -> Self {
        ImportWarning {
            kind,
            detail: detail.into(),
        }
    }
}

pub struct ImportResult {
    pub engine: Engine,
    pub warnings: Vec<ImportWarning>,
}

/// Replay one imported cell through the engine. A formula our parser rejects
/// is kept verbatim as text (leading apostrophe forces the literal) and
/// reported, so nothing is dropped silently.
pub(crate) fn apply_cell(
    engine: &mut Engine,
    sheet: &str,
    addr: CellAddr,
    input: &str,
    warnings: &mut Vec<ImportWarning>,
) -> Result<(), IoError> {
    let edit = |input: String| Action::CellEdit {
        sheet: sheet.to_string(),
        addr,
        input,
    };
    match engine.apply(&edit(input.to_string())) {
        Ok(_) => Ok(()),
        Err(ApplyError::Formula(e)) => {
            warnings.push(ImportWarning::new(
                ImportWarningKind::UnparseableFormula,
                format!(
                    "{}!{}: {} ({}); kept as text",
                    sheet,
                    addr.to_a1(),
                    input,
                    e
                ),
            ));
            engine
                .apply(&edit(format!("'{}", input)))
                .map_err(|e| IoError::Apply(e.to_string()))?;
            Ok(())
        }
        Err(e) => Err(IoError::Apply(e.to_string())),
    }
}

/// Rename the workbook's default sheet and add the imported ones in order,
/// leaving the workbook holding exactly `names`.
pub(crate) fn install_sheets(engine: &mut Engine, names: &[String]) -> Result<(), IoError> {
    let default = engine.wb.sheets[0].name.clone();
    // The placeholder must not collide with any incoming name, since the
    // engine refuses duplicates (case-insensitively).
    let mut placeholder = String::from("__gridline_import__");
    while names.iter().any(|n| n.eq_ignore_ascii_case(&placeholder)) {
        placeholder.push('_');
    }
    let apply = |engine: &mut Engine, a: Action| -> Result<(), IoError> {
        engine.apply(&a).map(|_| ()).map_err(|e| match e {
            ApplyError::DuplicateSheet(n) => {
                IoError::Malformed(format!("duplicate sheet name '{}'", n))
            }
            other => IoError::Apply(other.to_string()),
        })
    };
    apply(
        engine,
        Action::SheetRename {
            from: default,
            to: placeholder.clone(),
        },
    )?;
    for name in names {
        apply(engine, Action::SheetAdd { name: name.clone() })?;
    }
    if names.is_empty() {
        // Nothing to import into: keep a usable single sheet.
        apply(
            engine,
            Action::SheetRename {
                from: placeholder,
                to: "Sheet1".to_string(),
            },
        )?;
    } else {
        apply(engine, Action::SheetDelete { name: placeholder })?;
    }
    Ok(())
}
