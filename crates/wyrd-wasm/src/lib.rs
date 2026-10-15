//! Browser entry points. The Studio's offline bundle mode runs the same
//! correlator the daemon uses, compiled to WebAssembly.

use wasm_bindgen::prelude::*;

/// Correlate a session given as a JSON array of ledger events; returns the
/// causal graph as JSON.
#[wasm_bindgen]
pub fn correlate(events_json: &str) -> Result<String, JsError> {
    let events: Vec<wyrd_schema::Event> = serde_json::from_str(events_json)?;
    Ok(serde_json::to_string(&wyrd_correlator::correlate(&events))?)
}

/// Event schema identifier this build understands.
#[wasm_bindgen]
pub fn schema_id() -> String {
    wyrd_schema::SCHEMA_ID.to_string()
}
