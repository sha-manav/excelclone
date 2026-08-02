//! Dependency graph: which cells feed which formulas.

use crate::addr::RangeAddr;
use crate::model::{CellKey, SheetId};
use std::collections::{HashMap, HashSet};

/// What a formula cell reads: exact cell precedents plus watched ranges.
pub type Precedents = (Vec<CellKey>, Vec<(SheetId, RangeAddr)>);

#[derive(Debug, Default, Clone)]
pub struct DepGraph {
    /// precedent cell -> formula cells that read it directly.
    dependents: HashMap<CellKey, HashSet<CellKey>>,
    /// (sheet, range, formula cell reading the range).
    range_watchers: Vec<(SheetId, RangeAddr, CellKey)>,
    /// formula cell -> its precedents. Used to unregister on edit and to
    /// order recalc.
    forward: HashMap<CellKey, Precedents>,
}

impl DepGraph {
    pub fn set_precedents(
        &mut self,
        key: CellKey,
        exact: Vec<CellKey>,
        ranges: Vec<(SheetId, RangeAddr)>,
    ) {
        self.clear(key);
        for p in &exact {
            self.dependents.entry(*p).or_default().insert(key);
        }
        for (sid, r) in &ranges {
            self.range_watchers.push((*sid, *r, key));
        }
        self.forward.insert(key, (exact, ranges));
    }

    pub fn clear(&mut self, key: CellKey) {
        if let Some((exact, _)) = self.forward.remove(&key) {
            for p in exact {
                if let Some(s) = self.dependents.get_mut(&p) {
                    s.remove(&key);
                    if s.is_empty() {
                        self.dependents.remove(&p);
                    }
                }
            }
            self.range_watchers.retain(|(_, _, k)| *k != key);
        }
    }

    /// All formula cells that directly read `key` (exact refs + ranges).
    pub fn dependents_of(&self, key: CellKey) -> Vec<CellKey> {
        let mut out: HashSet<CellKey> = self
            .dependents
            .get(&key)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        for (sid, range, watcher) in &self.range_watchers {
            if *sid == key.sheet && range.contains(key.addr) {
                out.insert(*watcher);
            }
        }
        let mut v: Vec<CellKey> = out.into_iter().collect();
        v.sort();
        v
    }

    pub fn precedents_of(&self, key: CellKey) -> Option<&Precedents> {
        self.forward.get(&key)
    }

    /// All registered formula cells (used to rebuild after sheet changes).
    pub fn formula_cells(&self) -> Vec<CellKey> {
        let mut v: Vec<CellKey> = self.forward.keys().copied().collect();
        v.sort();
        v
    }

    pub fn reset(&mut self) {
        self.dependents.clear();
        self.range_watchers.clear();
        self.forward.clear();
    }
}
