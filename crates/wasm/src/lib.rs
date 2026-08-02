//! wasm-bindgen bindings over the Gridline engine.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn engine_version() -> String {
    engine::engine_version().to_string()
}
