//! Round-trip fidelity: what opening and saving a file costs.
//!
//! Two separate claims, measured separately because they fail separately:
//!
//! * **State** — the workbook Gridline reads back is the workbook it wrote.
//!   A file that survives the trip but recalculates differently has not
//!   survived it.
//! * **Untouched parts** — every zip entry Gridline does not model comes back
//!   byte-identical. This is the promise that opening a file in Gridline
//!   cannot destroy the parts of it Gridline does not understand, and it is
//!   the one users cannot check for themselves.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use engine::io::xlsx;

#[derive(Debug, Clone)]
pub struct RoundTrip {
    pub file: String,
    /// Parts that came back exactly as they went in.
    pub preserved: usize,
    /// Parts that changed. `xl/worksheets/*` and `xl/styles.xml` are expected
    /// to: they are what export regenerates.
    pub rewritten: Vec<String>,
    /// Parts that vanished. Always a defect.
    pub lost: Vec<String>,
    /// Whether the state snapshot survived unchanged.
    pub state_identical: bool,
    /// Anything that stopped the trip entirely.
    pub error: Option<String>,
}

impl RoundTrip {
    /// A trip is clean when the state survived and nothing was lost. Parts
    /// being *rewritten* is not a fault by itself — the sheet XML has to be.
    pub fn clean(&self) -> bool {
        self.error.is_none() && self.state_identical && self.lost.is_empty()
    }
}

/// Open every xlsx in a directory, save it, and open it again.
pub fn run(dir: &Path) -> Vec<RoundTrip> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "xlsx"))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    files.iter().map(|p| one(p)).collect()
}

fn one(path: &Path) -> RoundTrip {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut result = RoundTrip {
        file: name,
        preserved: 0,
        rewritten: Vec::new(),
        lost: Vec::new(),
        state_identical: false,
        error: None,
    };

    let original = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            result.error = Some(e.to_string());
            return result;
        }
    };
    let imported = match xlsx::import(&original) {
        Ok(r) => r,
        Err(e) => {
            result.error = Some(format!("import: {e}"));
            return result;
        }
    };
    let before = imported.engine.wb.state_snapshot();
    let saved = match xlsx::export(&imported.engine.wb) {
        Ok(b) => b,
        Err(e) => {
            result.error = Some(format!("export: {e}"));
            return result;
        }
    };
    let reopened = match xlsx::import(&saved) {
        Ok(r) => r,
        Err(e) => {
            result.error = Some(format!("re-import: {e}"));
            return result;
        }
    };
    result.state_identical = reopened.engine.wb.state_snapshot() == before;

    let (old_parts, new_parts) = (parts(&original), parts(&saved));
    for (name, bytes) in &old_parts {
        match new_parts.iter().find(|(n, _)| n == name) {
            None => result.lost.push(name.clone()),
            Some((_, after)) if after == bytes => result.preserved += 1,
            Some(_) => result.rewritten.push(name.clone()),
        }
    }
    result
}

fn parts(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let Ok(mut f) = zip.by_index(i) else { continue };
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_string();
        let mut data = Vec::new();
        if f.read_to_end(&mut data).is_ok() {
            out.push((name, data));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("fixtures")
    }

    #[test]
    fn the_committed_fixtures_round_trip_cleanly() {
        let results = run(&fixtures());
        assert!(!results.is_empty(), "no fixtures found to measure");
        for r in &results {
            assert!(
                r.clean(),
                "{} did not survive: state_identical={} lost={:?} error={:?}",
                r.file,
                r.state_identical,
                r.lost,
                r.error
            );
        }
    }

    #[test]
    fn only_the_parts_we_regenerate_are_rewritten() {
        // The preservation promise in one assertion: if anything outside the
        // sheet XML and the style sheet comes back different, opening and
        // saving a file changed something it had no business changing.
        for r in run(&fixtures()) {
            for part in &r.rewritten {
                assert!(
                    part.starts_with("xl/worksheets/") || part == "xl/styles.xml",
                    "{} rewrote {part}, which export does not regenerate",
                    r.file
                );
            }
        }
    }
}
