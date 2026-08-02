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
pub mod functions;
pub mod io;
pub mod model;
pub mod ops;
pub mod parser;
pub mod refs;
pub mod serial;
pub mod value;

pub use addr::{CellAddr, RangeAddr};
pub use engine::{Action, ApplyError, Engine, Event, FilterSpec, PasteMode, SortKey};
pub use model::{Cell, CellContent, CellKey, Sheet, SheetId, Workbook};
pub use value::{ErrorKind, Value};

pub fn engine_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
