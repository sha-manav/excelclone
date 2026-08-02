//! Gridline spreadsheet engine: pure library crate, no I/O.
//!
//! All state mutation flows through `Engine::apply(Action) -> Vec<Event>`.
//! The event log is the source of truth; replaying it from an empty workbook
//! must reproduce the exact final state.

pub fn engine_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::engine_version().is_empty());
    }
}
