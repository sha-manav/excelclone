//! wasm-bindgen bindings over the Gridline engine.
//!
//! The browser never mutates workbook state directly: it sends an `Action`
//! as JSON, receives the resulting `Event`s back, and re-reads whatever it
//! needs to paint. That keeps the single-mutation-path invariant intact
//! across the language boundary, and means the event stream the capture
//! pipeline records is exactly the one the engine produced.

use engine::io;
use engine::{Action, CellAddr, Engine, RangeAddr, Value};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Cell kinds, used by the renderer to pick alignment and colour without
/// having to parse the display string.
const KIND_EMPTY: u8 = 0;
const KIND_NUMBER: u8 = 1;
const KIND_TEXT: u8 = 2;
const KIND_BOOL: u8 = 3;
const KIND_ERROR: u8 = 4;

fn kind_of(v: &Value) -> u8 {
    match v {
        Value::Empty => KIND_EMPTY,
        Value::Number(_) => KIND_NUMBER,
        Value::Text(_) => KIND_TEXT,
        Value::Bool(_) => KIND_BOOL,
        Value::Error(_) => KIND_ERROR,
    }
}

/// A rectangular block of display data, flattened row-major so the JS side
/// can index it without allocating per-cell objects.
#[derive(Serialize)]
struct Viewport {
    row0: u32,
    col0: u32,
    rows: u32,
    cols: u32,
    /// `rows * cols` display strings.
    values: Vec<String>,
    /// `rows * cols` kind tags.
    kinds: Vec<u8>,
    /// Formula cells, so the grid can mark them.
    formulas: Vec<bool>,
}

#[derive(Serialize)]
struct SheetInfo {
    name: String,
    used_rows: u32,
    used_cols: u32,
    hidden_rows: Vec<u32>,
    merged: Vec<String>,
}

#[derive(Serialize)]
struct ImportOutcome {
    warnings: Vec<ImportWarningJs>,
    sheets: Vec<String>,
}

#[derive(Serialize)]
struct ImportWarningJs {
    kind: String,
    detail: String,
}

fn js_err(e: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&e.to_string())
}

fn to_js<T: Serialize>(v: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(v).map_err(js_err)
}

#[wasm_bindgen]
pub struct Gridline {
    engine: Engine,
}

impl Default for Gridline {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl Gridline {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Gridline {
        Gridline {
            engine: Engine::new(),
        }
    }

    /// Apply one action, returning the emitted events as JSON.
    ///
    /// This is the only way to change anything.
    #[wasm_bindgen(js_name = applyJson)]
    pub fn apply_json(&mut self, action_json: &str) -> Result<String, JsValue> {
        let action: Action = serde_json::from_str(action_json).map_err(js_err)?;
        let events = self.engine.apply(&action).map_err(js_err)?;
        serde_json::to_string(&events).map_err(js_err)
    }

    /// Apply several actions as one unit, returning every event produced.
    /// A failure part-way through leaves the earlier actions applied — the
    /// caller decides whether to undo, exactly as a user would.
    #[wasm_bindgen(js_name = applyBatchJson)]
    pub fn apply_batch_json(&mut self, actions_json: &str) -> Result<String, JsValue> {
        let actions: Vec<Action> = serde_json::from_str(actions_json).map_err(js_err)?;
        let mut all = Vec::new();
        for a in &actions {
            all.extend(self.engine.apply(a).map_err(js_err)?);
        }
        serde_json::to_string(&all).map_err(js_err)
    }

    /// Display data for a visible block of cells.
    pub fn viewport(
        &self,
        sheet: &str,
        row0: u32,
        col0: u32,
        rows: u32,
        cols: u32,
    ) -> Result<JsValue, JsValue> {
        let s = self
            .engine
            .wb
            .sheet_by_name(sheet)
            .ok_or_else(|| JsValue::from_str("unknown sheet"))?;
        let n = (rows as usize) * (cols as usize);
        let mut values = Vec::with_capacity(n);
        let mut kinds = Vec::with_capacity(n);
        let mut formulas = Vec::with_capacity(n);
        for r in row0..row0 + rows {
            for c in col0..col0 + cols {
                let addr = CellAddr::new(r, c);
                match s.cells.get(&addr) {
                    None => {
                        values.push(String::new());
                        kinds.push(KIND_EMPTY);
                        formulas.push(false);
                    }
                    Some(cell) => {
                        let v = cell.value();
                        values.push(v.display());
                        kinds.push(kind_of(v));
                        formulas.push(cell.is_formula());
                    }
                }
            }
        }
        to_js(&Viewport {
            row0,
            col0,
            rows,
            cols,
            values,
            kinds,
            formulas,
        })
    }

    /// The formula-bar text for a cell: the formula if there is one, else the
    /// literal as typed.
    #[wasm_bindgen(js_name = cellInput)]
    pub fn cell_input(&self, sheet: &str, row: u32, col: u32) -> String {
        self.engine
            .wb
            .sheet_by_name(sheet)
            .and_then(|s| s.cells.get(&CellAddr::new(row, col)))
            .map(|c| c.input())
            .unwrap_or_default()
    }

    /// The computed display value of a cell.
    #[wasm_bindgen(js_name = cellValue)]
    pub fn cell_value(&self, sheet: &str, row: u32, col: u32) -> String {
        self.engine
            .wb
            .sheet_by_name(sheet)
            .map(|s| s.value(CellAddr::new(row, col)).display())
            .unwrap_or_default()
    }

    /// Names, extents, hidden rows and merges for every sheet.
    pub fn sheets(&self) -> Result<JsValue, JsValue> {
        let infos: Vec<SheetInfo> = self
            .engine
            .wb
            .sheets
            .iter()
            .map(|s| {
                let used = s.used_range();
                SheetInfo {
                    name: s.name.clone(),
                    used_rows: used.map(|r| r.end.row + 1).unwrap_or(0),
                    used_cols: used.map(|r| r.end.col + 1).unwrap_or(0),
                    hidden_rows: s.hidden_rows.clone(),
                    merged: s.merged.iter().map(|m| m.to_a1()).collect(),
                }
            })
            .collect();
        to_js(&infos)
    }

    /// Distinct display values in one column of a range, for the filter menu.
    #[wasm_bindgen(js_name = columnValues)]
    pub fn column_values(&self, sheet: &str, range: &str, col: u32) -> Result<JsValue, JsValue> {
        let s = self
            .engine
            .wb
            .sheet_by_name(sheet)
            .ok_or_else(|| JsValue::from_str("unknown sheet"))?;
        let r = RangeAddr::parse_a1(range).ok_or_else(|| JsValue::from_str("bad range"))?;
        let mut seen: Vec<String> = Vec::new();
        // Skip the header row, which the filter never hides.
        for row in r.start.row + 1..=r.end.row {
            let v = s.value(CellAddr::new(row, col)).display();
            if !seen.contains(&v) {
                seen.push(v);
            }
        }
        seen.sort();
        to_js(&seen)
    }

    /// Inject the wall clock so NOW/TODAY stay replayable.
    #[wasm_bindgen(js_name = setNowMs)]
    pub fn set_now_ms(&mut self, ms: f64) {
        self.engine.now_ms = ms as i64;
    }

    #[wasm_bindgen(js_name = canUndo)]
    pub fn can_undo(&self) -> bool {
        self.engine.can_undo()
    }

    #[wasm_bindgen(js_name = canRedo)]
    pub fn can_redo(&self) -> bool {
        self.engine.can_redo()
    }

    /// Deterministic snapshot of computed state — the same one the replay
    /// tests compare, exposed so the client can checksum what it renders.
    #[wasm_bindgen(js_name = stateSnapshot)]
    pub fn state_snapshot(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.engine.wb.state_snapshot()).map_err(js_err)
    }

    #[wasm_bindgen(js_name = importXlsx)]
    pub fn import_xlsx(&mut self, bytes: &[u8]) -> Result<JsValue, JsValue> {
        let result = io::xlsx::import(bytes).map_err(js_err)?;
        self.adopt(result)
    }

    #[wasm_bindgen(js_name = importCsv)]
    pub fn import_csv(&mut self, bytes: &[u8], sheet_name: &str) -> Result<JsValue, JsValue> {
        let result = io::csv::import(bytes, sheet_name).map_err(js_err)?;
        self.adopt(result)
    }

    fn adopt(&mut self, result: io::ImportResult) -> Result<JsValue, JsValue> {
        let now = self.engine.now_ms;
        self.engine = result.engine;
        self.engine.now_ms = now;
        self.engine.recalc_all();
        let outcome = ImportOutcome {
            warnings: result
                .warnings
                .iter()
                .map(|w| ImportWarningJs {
                    kind: format!("{:?}", w.kind),
                    detail: w.detail.clone(),
                })
                .collect(),
            sheets: self
                .engine
                .wb
                .sheets
                .iter()
                .map(|s| s.name.clone())
                .collect(),
        };
        to_js(&outcome)
    }

    #[wasm_bindgen(js_name = exportXlsx)]
    pub fn export_xlsx(&self) -> Result<Vec<u8>, JsValue> {
        io::xlsx::export(&self.engine.wb).map_err(js_err)
    }

    #[wasm_bindgen(js_name = exportCsv)]
    pub fn export_csv(&self, sheet: &str) -> Result<Vec<u8>, JsValue> {
        let id = self
            .engine
            .wb
            .sheet_id_by_name(sheet)
            .ok_or_else(|| JsValue::from_str("unknown sheet"))?;
        io::csv::export(&self.engine.wb, id).map_err(js_err)
    }
}

#[wasm_bindgen(js_name = engineVersion)]
pub fn engine_version() -> String {
    engine::engine_version().to_string()
}

/// Turn an action into its vocabulary name and redacted payload.
///
/// Redaction happens here, in the same Rust the tests cover, rather than in
/// JavaScript: a second implementation of a privacy guarantee is a second
/// chance to get it wrong. Returns `{"action": name, "payload": {...}}`.
#[wasm_bindgen(js_name = describeAction)]
pub fn describe_action(action_json: &str, mode: &str, salt: &str) -> Result<String, JsValue> {
    let action: Action = serde_json::from_str(action_json).map_err(js_err)?;
    let mode = engine::PrivacyMode::parse(mode)
        .ok_or_else(|| JsValue::from_str("unknown privacy mode"))?;
    let (name, payload) = engine::telemetry::describe(&action, mode, salt);
    serde_json::to_string(&serde_json::json!({ "action": name, "payload": payload }))
        .map_err(js_err)
}

/// The documented action vocabulary, for the transparency page.
#[wasm_bindgen(js_name = actionVocabulary)]
pub fn action_vocabulary() -> Vec<String> {
    engine::telemetry::ACTION_VOCABULARY
        .iter()
        .map(|s| s.to_string())
        .collect()
}
