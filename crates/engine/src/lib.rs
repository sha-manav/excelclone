//! Gridline spreadsheet engine: pure library crate, no I/O.
//!
//! All state mutation flows through `Engine::apply(Action) -> Vec<Event>`.
//! The action log is the source of truth; replaying it from an empty
//! workbook must reproduce the exact final state.

pub mod addr;
pub mod ast;
pub mod deps;
pub mod engine;
pub mod eval;
pub mod format;
pub mod functions;
pub mod io;
pub mod model;
pub mod ops;
pub mod parser;
pub mod refs;
pub mod routine;
pub mod serial;
pub mod telemetry;
pub mod value;

pub use addr::{CellAddr, RangeAddr};
pub use engine::{Action, ApplyError, Engine, Event, FilterSpec, PasteMode, SortKey};
pub use format::{BorderPreset, Borders, CellFormat, FormatId, FormatPatch, FormatTable, HAlign};
pub use model::{Cell, CellContent, CellKey, Sheet, SheetId, Workbook};
pub use routine::{CellChange, DryRun, Requirement, Routine};
pub use telemetry::{EventEnvelope, PrivacyMode};
pub use value::{ErrorKind, Value};

pub fn engine_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
