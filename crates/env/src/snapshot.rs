//! Content-addressed workbook snapshots.
//!
//! A trajectory names its starting state by hash rather than carrying it. Ten
//! thousand generated variants of one task share one snapshot when they start
//! from the same workbook, and the dataset stays a file somebody can read.
//!
//! The hash is over the *canonical* serialization, which matters more than it
//! sounds: two workbooks that compute the same thing must get the same id, or
//! the store fills up with duplicates that are only accidentally different.
//! `Workbook`'s derived state — spills, conditional formats, the interned
//! format table's ordering — is excluded by construction, because none of it
//! is serialized; what is left is the inputs, and the inputs are what a
//! snapshot means.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine::{Engine, Workbook};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EnvError;

/// A snapshot's identity: the hash of its canonical bytes, hex, truncated to
/// something a human can compare at a glance and still long enough that a
/// collision is not a thing that happens.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

impl SnapshotId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Canonical bytes for a workbook, and their id.
///
/// `serde_json` over `Workbook` is already deterministic here: every map in
/// the model that reaches the wire is a `BTreeMap`, and `cells` — the one
/// `HashMap` — is converted on the way out. A `HashMap` serialized directly
/// would order by hash seed and give a different id for the same workbook on
/// every run, which is exactly the bug this comment exists to prevent
/// somebody reintroducing.
pub fn canonical(wb: &Workbook) -> Result<(SnapshotId, Vec<u8>), EnvError> {
    let value = serde_json::to_value(wb).map_err(EnvError::Serde)?;
    let sorted = sort_maps(value);
    let bytes = serde_json::to_vec(&sorted).map_err(EnvError::Serde)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let id = SnapshotId(hex::encode(hasher.finalize())[..32].to_string());
    Ok((id, bytes))
}

/// Recursively sort every object's keys, so serialization order cannot depend
/// on the order a map happened to iterate in.
fn sort_maps(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, serde_json::Value> =
                map.into_iter().map(|(k, v)| (k, sort_maps(v))).collect();
            serde_json::Value::Object(sorted.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_maps).collect())
        }
        other => other,
    }
}

/// Where snapshots live. In memory by default; backed by a directory when the
/// dataset has to outlive the process.
#[derive(Debug, Default, Clone)]
pub struct SnapshotStore {
    memory: HashMap<SnapshotId, Vec<u8>>,
    dir: Option<PathBuf>,
}

impl SnapshotStore {
    pub fn in_memory() -> Self {
        SnapshotStore::default()
    }

    /// A store that also writes to `dir`, creating it if needed.
    pub fn at(dir: impl Into<PathBuf>) -> Result<Self, EnvError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(EnvError::Io)?;
        Ok(SnapshotStore {
            memory: HashMap::new(),
            dir: Some(dir),
        })
    }

    /// Store a workbook and return its id. Storing the same workbook twice is
    /// free and returns the same id — that is the whole point of addressing
    /// by content.
    pub fn put(&mut self, wb: &Workbook) -> Result<SnapshotId, EnvError> {
        let (id, bytes) = canonical(wb)?;
        if self.memory.contains_key(&id) {
            return Ok(id);
        }
        if let Some(dir) = &self.dir {
            let path = self.path_for(dir, &id);
            if !path.exists() {
                std::fs::write(&path, &bytes).map_err(EnvError::Io)?;
            }
        }
        self.memory.insert(id.clone(), bytes);
        Ok(id)
    }

    /// Load a snapshot as a *recalculated* engine.
    ///
    /// Recalculated rather than trusted: cached values, spilled blocks and
    /// conditional formats are all derived, and a snapshot that carried them
    /// could disagree with what the engine would compute. Recomputing on the
    /// way in is what makes `reset` mean "this exact starting state" even if
    /// the file on disk was written by an older build.
    pub fn load(&self, id: &SnapshotId) -> Result<Engine, EnvError> {
        let bytes = match self.memory.get(id) {
            Some(b) => b.clone(),
            None => {
                let dir = self
                    .dir
                    .as_ref()
                    .ok_or_else(|| EnvError::UnknownSnapshot(id.clone()))?;
                std::fs::read(self.path_for(dir, id))
                    .map_err(|_| EnvError::UnknownSnapshot(id.clone()))?
            }
        };
        let wb: Workbook = serde_json::from_slice(&bytes).map_err(EnvError::Serde)?;
        let mut engine = Engine::new();
        engine.wb = wb;
        engine.recalc_all();
        // A reset is a starting point, not an edit: nothing to undo.
        engine.clear_history();
        Ok(engine)
    }

    pub fn len(&self) -> usize {
        self.memory.len()
    }

    pub fn is_empty(&self) -> bool {
        self.memory.is_empty()
    }

    fn path_for(&self, dir: &Path, id: &SnapshotId) -> PathBuf {
        dir.join(format!("{}.json", id.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Action, CellAddr};

    fn workbook_with(cells: &[(&str, &str)]) -> Workbook {
        let mut e = Engine::new();
        for (a1, input) in cells {
            e.apply(&Action::CellEdit {
                sheet: "Sheet1".into(),
                addr: CellAddr::parse_a1(a1).unwrap(),
                input: (*input).into(),
            })
            .unwrap();
        }
        e.wb
    }

    #[test]
    fn the_same_workbook_hashes_the_same_way_every_time() {
        // The id is the dataset's join key. If it moved between runs, every
        // trajectory recorded yesterday would point at nothing today.
        let a = workbook_with(&[("A1", "1"), ("B2", "hello"), ("C3", "=A1+1")]);
        let b = workbook_with(&[("C3", "=A1+1"), ("A1", "1"), ("B2", "hello")]);
        assert_eq!(canonical(&a).unwrap().0, canonical(&b).unwrap().0);
    }

    #[test]
    fn a_different_workbook_hashes_differently() {
        let a = workbook_with(&[("A1", "1")]);
        let b = workbook_with(&[("A1", "2")]);
        assert_ne!(canonical(&a).unwrap().0, canonical(&b).unwrap().0);
    }

    #[test]
    fn storing_the_same_snapshot_twice_costs_nothing() {
        let mut store = SnapshotStore::in_memory();
        let wb = workbook_with(&[("A1", "1")]);
        let first = store.put(&wb).unwrap();
        let second = store.put(&wb).unwrap();
        assert_eq!(first, second);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_loaded_snapshot_has_recomputed_its_formulas() {
        let mut store = SnapshotStore::in_memory();
        let id = store
            .put(&workbook_with(&[("A1", "2"), ("A2", "=A1*21")]))
            .unwrap();
        let e = store.load(&id).unwrap();
        assert_eq!(e.value_at("Sheet1", "A2"), engine::Value::Number(42.0));
        // ...and nothing to undo, because loading is not editing.
        assert!(!e.can_undo());
    }

    #[test]
    fn a_snapshot_survives_a_trip_through_a_directory() {
        let dir = std::env::temp_dir().join(format!("gridline-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let id = {
            let mut store = SnapshotStore::at(&dir).unwrap();
            store.put(&workbook_with(&[("A1", "7")])).unwrap()
        };
        // A fresh store with an empty cache has to find it on disk.
        let store = SnapshotStore::at(&dir).unwrap();
        let e = store.load(&id).unwrap();
        assert_eq!(e.value_at("Sheet1", "A1"), engine::Value::Number(7.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_id_is_an_error_rather_than_an_empty_workbook() {
        let store = SnapshotStore::in_memory();
        assert!(store.load(&SnapshotId("nope".into())).is_err());
    }
}
