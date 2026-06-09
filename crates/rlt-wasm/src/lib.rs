//! WebAssembly surface for the checker.
//!
//! Compiles to `wasm32-unknown-unknown` via `wasm-pack`. The runtime loads its rkyv artifact with
//! zero-copy views (no deserialization pass), which is what makes browser cold-start cheap — the
//! property nlprule lacked. M6 adds the Node smoke test (`scripts/smoke_node.mjs`) that drives
//! [`Checker::check`] through this boundary.

#![forbid(unsafe_code)]

use rlt_core::Checker;
use rlt_engine::VendoredEngine;
use wasm_bindgen::prelude::*;

/// A reusable checker handle held across calls from JS, so the engine/artifact load once.
#[wasm_bindgen]
pub struct RltChecker {
    inner: Checker<VendoredEngine>,
}

#[wasm_bindgen]
impl RltChecker {
    /// Construct a checker from the bytes of nlprule's `en_tokenizer.bin` and `en_rules.bin`
    /// (supplied by JS, e.g. fetched or bundled). Installs a panic hook so Rust panics surface as
    /// JS console errors.
    ///
    /// # Errors
    /// Returns a JS error if either buffer is not a valid nlprule binary.
    #[wasm_bindgen(constructor)]
    pub fn new(tokenizer_bin: &[u8], rules_bin: &[u8]) -> Result<RltChecker, JsValue> {
        console_error_panic_hook::set_once();
        let engine = VendoredEngine::from_reader(tokenizer_bin)
            .and_then(|e| e.with_rules_reader(rules_bin))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self {
            inner: Checker::new(engine),
        })
    }

    /// Check `text` and return the diagnostics as a JS array of plain objects.
    ///
    /// # Errors
    /// Returns a JS error if the diagnostics cannot be serialized to a `JsValue`.
    pub fn check(&self, text: &str) -> Result<JsValue, JsValue> {
        let diagnostics = self.inner.check(text);
        serde_wasm_bindgen::to_value(&diagnostics).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
