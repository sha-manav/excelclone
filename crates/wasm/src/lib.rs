//! wasm-bindgen bindings over the Gridline engine.
//!
//! The browser never mutates workbook state directly: it sends an `Action`
//! as JSON, receives the resulting `Event`s back, and re-reads whatever it
//! needs to paint. That keeps the single-mutation-path invariant intact
//! across the language boundary, and means the event stream the capture
//! pipeline records is exactly the one the engine produced.

use engine::functions::numfmt;
use engine::io;
use engine::{Action, CellAddr, CellFormat, Engine, RangeAddr, Value};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Cell kinds, used by the renderer to pick alignment and colour without
/// having to parse the display string.
const KIND_EMPTY: u8 = 0;
const KIND_NUMBER: u8 = 1;
const KIND_TEXT: u8 = 2;
const KIND_BOOL: u8 = 3;
const KIND_ERROR: u8 = 4;

/// A cell's display text under its number format.
///
/// `numfmt` fails soft by design — an unrecognised code renders as General
/// rather than erroring — so a format we cannot parse shows the plain value
/// instead of blocking the paint.
fn display_with_format(v: &Value, f: &CellFormat) -> String {
    match &f.number_format {
        None => v.display(),
        Some(code) => numfmt::format_value(v, code).unwrap_or_else(|_| v.display()),
    }
}

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
    /// `rows * cols` display strings, already run through each cell's number
    /// format. The grid draws text; deciding what the text says is the
    /// engine's job, and doing it here keeps one implementation of Excel's
    /// format codes rather than a second one in TypeScript.
    values: Vec<String>,
    /// `rows * cols` kind tags.
    kinds: Vec<u8>,
    /// Formula cells, so the grid can mark them.
    formulas: Vec<bool>,
    /// `rows * cols` indices into `palette`. Sent as indices rather than
    /// objects because a formatted block is overwhelmingly repetitive: a bold
    /// header row is one palette entry and N small integers.
    styles: Vec<u32>,
    /// Distinct formats used in this block. Entry 0 is always the default.
    palette: Vec<CellFormat>,
}

#[derive(Serialize)]
struct SheetInfo {
    name: String,
    /// Extent of the *data*: what Ctrl+Down should reach.
    used_rows: u32,
    used_cols: u32,
    /// Extent of everything that has to be drawn, which is larger when cells
    /// carry formatting or merges but no value.
    painted_rows: u32,
    painted_cols: u32,
    hidden_rows: Vec<u32>,
    merged: Vec<String>,
    /// Non-default column widths and row heights in pixels, as
    /// `[index, pixels]` pairs. Pairs rather than an object because a JSON
    /// object would key them by string and the grid wants numbers.
    col_widths: Vec<(u32, f64)>,
    row_heights: Vec<(u32, f64)>,
    frozen_rows: u32,
    frozen_cols: u32,
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

/// Apply a rule's attributes over a cell's own, which is what "differential"
/// means: the rule wins where it says anything and is silent everywhere else.
fn layer(base: &mut CellFormat, over: &CellFormat) {
    if over.bold {
        base.bold = true;
    }
    if over.italic {
        base.italic = true;
    }
    if over.font_color.is_some() {
        base.font_color = over.font_color.clone();
    }
    if over.fill_color.is_some() {
        base.fill_color = over.fill_color.clone();
    }
    if !over.borders.is_none() {
        base.borders = over.borders;
    }
    if over.number_format.is_some() {
        base.number_format = over.number_format.clone();
    }
    if over.align.is_some() {
        base.align = over.align;
    }
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

    /// Apply several actions as one user-visible gesture, returning every
    /// event produced.
    ///
    /// One undo step, not one per action: a routine that took five actions to
    /// express is still one thing the user asked for. A failure part-way
    /// through leaves the earlier actions applied — the caller decides
    /// whether to undo, exactly as a user would.
    #[wasm_bindgen(js_name = applyBatchJson)]
    pub fn apply_batch_json(&mut self, actions_json: &str) -> Result<String, JsValue> {
        let actions: Vec<Action> = serde_json::from_str(actions_json).map_err(js_err)?;
        let events = self
            .engine
            .apply_batch(&actions, "run routine")
            .map_err(js_err)?;
        serde_json::to_string(&events).map_err(js_err)
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
        let mut styles = Vec::with_capacity(n);
        let mut palette = vec![CellFormat::default()];
        // Format id in the workbook table -> index in this viewport's palette.
        let mut seen: Vec<(u32, u32)> = Vec::new();

        for r in row0..row0 + rows {
            for c in col0..col0 + cols {
                let addr = CellAddr::new(r, c);
                let mut resolved = match s.format_id(addr) {
                    None => 0,
                    Some(id) => match seen.iter().find(|(k, _)| *k == id) {
                        Some((_, i)) => *i,
                        None => {
                            palette.push(self.engine.wb.formats.resolve(Some(id)));
                            let i = (palette.len() - 1) as u32;
                            seen.push((id, i));
                            i
                        }
                    },
                };
                // A rule's format is differential: it layers over whatever the
                // cell already had, and the result is a one-off palette entry
                // rather than an interned format, because it is derived state
                // that must not reach the workbook's format table.
                if let Some(cond) = s.cond_formats.get(&addr) {
                    let mut merged = palette[resolved as usize].clone();
                    layer(&mut merged, cond);
                    palette.push(merged);
                    resolved = (palette.len() - 1) as u32;
                }
                let style = resolved;
                styles.push(style);
                match s.cells.get(&addr) {
                    None => {
                        // A cell with no `Cell` behind it may still be showing
                        // a value: a dynamic array spilled into it. Reading
                        // only `cells` here is how a block would compute
                        // correctly and draw as blank.
                        match s.spill.get(&addr) {
                            Some((_, v)) => {
                                values.push(display_with_format(v, &palette[style as usize]));
                                kinds.push(kind_of(v));
                                // Not a formula: there is nothing to open in
                                // the formula bar, and saying otherwise would
                                // let the user edit a cell that does not exist.
                                formulas.push(false);
                            }
                            None => {
                                values.push(String::new());
                                kinds.push(KIND_EMPTY);
                                formulas.push(false);
                            }
                        }
                    }
                    Some(cell) => {
                        let v = cell.value();
                        values.push(display_with_format(v, &palette[style as usize]));
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
            styles,
            palette,
        })
    }

    /// The formula-bar text for a cell: the formula if there is one, else the
    /// literal as typed.
    #[wasm_bindgen(js_name = cellInput)]
    pub fn cell_input(&self, sheet: &str, row: u32, col: u32) -> String {
        let addr = CellAddr::new(row, col);
        let Some(s) = self.engine.wb.sheet_by_name(sheet) else {
            return String::new();
        };
        if let Some(cell) = s.cells.get(&addr) {
            return cell.input();
        }
        // A spilled cell has no formula of its own. Excel shows the anchor's,
        // greyed; this shows the value, because the formula bar here is an
        // editable field and offering the formula would invite the user to
        // press Enter and end up with a second copy of it. Typing over the
        // value breaks the block, which is what Excel does too.
        s.spill
            .get(&addr)
            .map(|(_, v)| v.display())
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

    /// Every defined name, as `[name, refers_to]` pairs in name order.
    #[wasm_bindgen(js_name = definedNames)]
    pub fn defined_names(&self) -> Result<JsValue, JsValue> {
        let pairs: Vec<(String, String)> = self
            .engine
            .wb
            .names
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        to_js(&pairs)
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
                let painted = s.painted_range();
                SheetInfo {
                    name: s.name.clone(),
                    used_rows: used.map(|r| r.end.row + 1).unwrap_or(0),
                    used_cols: used.map(|r| r.end.col + 1).unwrap_or(0),
                    painted_rows: painted.map(|r| r.end.row + 1).unwrap_or(0),
                    painted_cols: painted.map(|r| r.end.col + 1).unwrap_or(0),
                    hidden_rows: s.hidden_rows.clone(),
                    merged: s.merged.iter().map(|m| m.to_a1()).collect(),
                    col_widths: s.col_widths.iter().map(|(i, px)| (*i, *px)).collect(),
                    row_heights: s.row_heights.iter().map(|(i, px)| (*i, *px)).collect(),
                    frozen_rows: s.frozen_rows,
                    frozen_cols: s.frozen_cols,
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

    /// Addresses matching a search term, in reading order.
    ///
    /// Read-only, and it goes through the engine's own matcher so "find next"
    /// walks exactly the cells "replace all" would rewrite. A second matcher
    /// in TypeScript would eventually disagree with the first, and the
    /// disagreement would show up as a replacement the user never saw coming.
    #[wasm_bindgen(js_name = findMatches)]
    pub fn find_matches(
        &self,
        sheet: &str,
        find: &str,
        match_case: bool,
        whole_cell: bool,
    ) -> Result<JsValue, JsValue> {
        let hits: Vec<String> = self
            .engine
            .find_matches(sheet, None, find, match_case, whole_cell)
            .iter()
            .map(|a| a.to_a1())
            .collect();
        to_js(&hits)
    }

    /// The resolved format of one cell, for the toolbar's pressed states.
    #[wasm_bindgen(js_name = cellFormat)]
    pub fn cell_format(&self, sheet: &str, row: u32, col: u32) -> Result<JsValue, JsValue> {
        let f = self
            .engine
            .wb
            .sheet_by_name(sheet)
            .and_then(|s| s.format_id(CellAddr::new(row, col)))
            .map(|id| self.engine.wb.formats.resolve(Some(id)))
            .unwrap_or_default();
        to_js(&f)
    }

    /// What a routine would change if it ran at `(row, col)`, without
    /// changing anything.
    #[wasm_bindgen(js_name = previewRoutine)]
    pub fn preview_routine(
        &self,
        body_json: &str,
        sheet: &str,
        row: u32,
        col: u32,
    ) -> Result<String, JsValue> {
        let routine: engine::Routine = serde_json::from_str(body_json).map_err(js_err)?;
        let preview =
            engine::routine::dry_run(&self.engine, &routine, sheet, CellAddr::new(row, col));
        serde_json::to_string(&preview).map_err(js_err)
    }

    /// The actions a routine would apply at `(row, col)`.
    ///
    /// Deliberately *not* a `runRoutine` that applies them here. Handing the
    /// actions back lets the client push them through the same `applyBatch`
    /// every other gesture uses, which means the capture pipeline sees them
    /// without anything being taught about routines. A second execution path
    /// inside the engine would be exactly the side door the single-mutation
    /// rule exists to forbid.
    #[wasm_bindgen(js_name = routineActions)]
    pub fn routine_actions(
        &self,
        body_json: &str,
        sheet: &str,
        row: u32,
        col: u32,
    ) -> Result<String, JsValue> {
        let routine: engine::Routine = serde_json::from_str(body_json).map_err(js_err)?;
        let actions = routine.actions_at(sheet, CellAddr::new(row, col));
        serde_json::to_string(&actions).map_err(js_err)
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

/// Redact a label that stays a plain string on the wire, such as the sheet
/// name carried in every envelope's `context`.
#[wasm_bindgen(js_name = redactLabel)]
pub fn redact_label(text: &str, mode: &str, salt: &str) -> Result<String, JsValue> {
    let mode = engine::PrivacyMode::parse(mode)
        .ok_or_else(|| JsValue::from_str("unknown privacy mode"))?;
    Ok(engine::telemetry::redact_label_text(text, mode, salt))
}

/// The documented action vocabulary, for the transparency page.
#[wasm_bindgen(js_name = actionVocabulary)]
pub fn action_vocabulary() -> Vec<String> {
    engine::telemetry::ACTION_VOCABULARY
        .iter()
        .map(|s| s.to_string())
        .collect()
}
