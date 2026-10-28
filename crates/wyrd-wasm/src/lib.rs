//! Browser entry points. The Studio's offline mode runs the same
//! correlator the daemon uses and the same bundle reader as `wyrd verify`,
//! compiled to WebAssembly.
#![forbid(unsafe_code)]

use wasm_bindgen::prelude::*;

pub mod bundle;

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

/// A `.wyrd` bundle opened in the browser: verified with the same reader
/// as `wyrd verify` before anything in it is exposed.
#[wasm_bindgen]
pub struct BundleView {
    report: String,
    opened: Option<bundle::Opened>,
}

#[wasm_bindgen]
impl BundleView {
    /// Verify `bytes` (signature, file hashes, ledger chain). Never fails:
    /// the report says which check did.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> BundleView {
        let (report, opened) = bundle::open(bytes);
        BundleView {
            report: report.to_string(),
            opened,
        }
    }

    /// The verification report (`wyrd verify --json` without the path).
    pub fn report(&self) -> String {
        self.report.clone()
    }

    fn opened(&self) -> Result<&bundle::Opened, JsError> {
        self.opened
            .as_ref()
            .ok_or_else(|| JsError::new("the bundle did not verify"))
    }

    /// The verified events as a JSON array.
    pub fn events(&self) -> Result<String, JsError> {
        self.opened()?.events_json().map_err(|e| JsError::new(&e))
    }

    /// Changes between two checkpoint manifests (`b3:` hashes).
    pub fn diff(&self, a: &str, b: &str) -> Result<String, JsError> {
        self.opened()?.diff_json(a, b).map_err(|e| JsError::new(&e))
    }

    /// Both sides of one path at two checkpoint manifests.
    pub fn file(&self, a: &str, b: &str, path: &str) -> Result<String, JsError> {
        self.opened()?
            .file_json(a, b, path)
            .map_err(|e| JsError::new(&e))
    }
}
