//! WebAssembly surface for the checker.
//!
//! Compiles to `wasm32-unknown-unknown` via `wasm-pack`. The runtime loads its rkyv artifact with
//! zero-copy views (no deserialization pass), which is what makes browser cold-start cheap — the
//! property nlprule lacked. `RltChecker::new` runs L1+L2; `RltChecker::with_neural` additionally
//! loads the L4 neural tagger (a pure-Rust `rten` int8 model) from in-memory bytes.

#![forbid(unsafe_code)]

use rlt_core::{
    Checker, Composite, Diagnostic, Disambiguator, Engine, GrammarChecker, IrMatcher, WithGrammar,
};
#[cfg(feature = "nlprule")]
use rlt_engine::VendoredEngine;
use rlt_native::NativeEngine;
use rlt_tagger::Tagger;
use wasm_bindgen::prelude::*;

/// Object-safe handle over a concrete `Checker<B>`, so a checker with or without the L4 layer (which
/// have different concrete types) can be held behind one `RltChecker`.
trait CheckerHandle {
    fn run(&self, text: &str) -> Vec<Diagnostic>;
}

impl<B: Engine + GrammarChecker> CheckerHandle for Checker<B> {
    fn run(&self, text: &str) -> Vec<Diagnostic> {
        self.check(text)
    }
}

/// A reusable checker handle held across calls from JS, so the engine/artifacts load once.
#[wasm_bindgen]
pub struct RltChecker {
    inner: Box<dyn CheckerHandle>,
}

#[wasm_bindgen]
impl RltChecker {
    /// Construct an L1+L2 checker from the bytes of nlprule's `en_tokenizer.bin` and `en_rules.bin`
    /// (supplied by JS). Installs a panic hook so Rust panics surface as JS console errors.
    ///
    /// # Errors
    /// Returns a JS error if either buffer is not a valid nlprule binary.
    #[cfg(feature = "nlprule")]
    #[wasm_bindgen(constructor)]
    pub fn new(tokenizer_bin: &[u8], rules_bin: &[u8]) -> Result<RltChecker, JsValue> {
        console_error_panic_hook::set_once();
        let engine = load_engine(tokenizer_bin, rules_bin)?;
        Ok(Self {
            inner: Box::new(Checker::new(engine)),
        })
    }

    /// Construct a checker that also runs the **L4 neural tagger**, loading its artifact tuple
    /// (`model.int8.onnx` / `tokenizer.json` / `labels.json` / `meta.json` / `verb-form-vocab.txt`)
    /// from in-memory bytes. The tagger composes on top of L1+L2 via `WithGrammar`.
    ///
    /// # Errors
    /// Returns a JS error if any engine or L4 artifact buffer is invalid.
    #[cfg(feature = "nlprule")]
    pub fn with_neural(
        tokenizer_bin: &[u8],
        rules_bin: &[u8],
        l4_model: Vec<u8>,
        l4_tokenizer: &[u8],
        l4_labels: &[u8],
        l4_meta: &[u8],
        l4_verb: &[u8],
    ) -> Result<RltChecker, JsValue> {
        console_error_panic_hook::set_once();
        let engine = load_engine(tokenizer_bin, rules_bin)?;
        let tagger = Tagger::from_bytes(l4_model, l4_tokenizer, l4_labels, l4_meta, l4_verb)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self {
            inner: Box::new(Checker::new(WithGrammar::new(engine, tagger))),
        })
    }

    /// Construct a **fully-native** L1+L2 checker — the pure-Rust pipeline that needs no nlprule
    /// binary. `segment_srx` is LanguageTool's `segment.srx` text; `tagger` is the `tagger.rkyv`
    /// artifact; `disambig` is `disambig.rkyv` (pass an empty buffer to skip disambiguation);
    /// `ir_blob` is the converter's `en.rkyv` grammar artifact.
    ///
    /// # Errors
    /// Returns a JS error if any artifact buffer is invalid.
    pub fn with_native(
        lang: &str,
        segment_srx: &str,
        tagger: &[u8],
        disambig: &[u8],
        ir_blob: &[u8],
    ) -> Result<RltChecker, JsValue> {
        console_error_panic_hook::set_once();
        let engine = load_native_engine(lang, segment_srx, tagger, disambig)?;
        let ir = load_ir(ir_blob)?;
        Ok(Self {
            inner: Box::new(Checker::new(Composite::new(engine, ir))),
        })
    }

    /// Like [`with_native`](Self::with_native) but also runs the **L4 neural tagger** on top.
    ///
    /// # Errors
    /// Returns a JS error if any engine or L4 artifact buffer is invalid.
    #[allow(clippy::too_many_arguments, reason = "wasm-bindgen flattens the artifact tuple")]
    pub fn with_native_neural(
        lang: &str,
        segment_srx: &str,
        tagger: &[u8],
        disambig: &[u8],
        ir_blob: &[u8],
        l4_model: Vec<u8>,
        l4_tokenizer: &[u8],
        l4_labels: &[u8],
        l4_meta: &[u8],
        l4_verb: &[u8],
    ) -> Result<RltChecker, JsValue> {
        console_error_panic_hook::set_once();
        let engine = load_native_engine(lang, segment_srx, tagger, disambig)?;
        let ir = load_ir(ir_blob)?;
        let l4 = Tagger::from_bytes(l4_model, l4_tokenizer, l4_labels, l4_meta, l4_verb)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self {
            inner: Box::new(Checker::new(WithGrammar::new(Composite::new(engine, ir), l4))),
        })
    }

    /// Check `text` and return the diagnostics as a JS array of plain objects.
    ///
    /// # Errors
    /// Returns a JS error if the diagnostics cannot be serialized to a `JsValue`.
    pub fn check(&self, text: &str) -> Result<JsValue, JsValue> {
        let diagnostics = self.inner.run(text);
        serde_wasm_bindgen::to_value(&diagnostics).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

/// Build the nlprule engine (L0/L1 + L2 rules) from in-memory binaries.
#[cfg(feature = "nlprule")]
fn load_engine(tokenizer_bin: &[u8], rules_bin: &[u8]) -> Result<VendoredEngine, JsValue> {
    VendoredEngine::from_reader(tokenizer_bin)
        .and_then(|e| e.with_rules_reader(rules_bin))
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Build the native engine from in-memory artifacts (disambiguation skipped if `disambig` is empty).
fn load_native_engine(
    lang: &str,
    segment_srx: &str,
    tagger: &[u8],
    disambig: &[u8],
) -> Result<NativeEngine, JsValue> {
    let cfg = rlt_lang::config(lang)
        .ok_or_else(|| JsValue::from_str(&format!("unknown language {lang:?}")))?;
    let mut engine = NativeEngine::from_bytes(cfg, segment_srx, tagger)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    if !disambig.is_empty() {
        let d = Disambiguator::from_rkyv_bytes(disambig)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        engine = engine.with_disambiguator(d);
    }
    Ok(engine)
}

/// Compile the IR grammar matcher from the `en.rkyv` artifact bytes.
fn load_ir(ir_blob: &[u8]) -> Result<IrMatcher, JsValue> {
    IrMatcher::from_rkyv_bytes(ir_blob).map_err(|e| JsValue::from_str(&e.to_string()))
}
