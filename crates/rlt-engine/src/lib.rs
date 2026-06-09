//! Linguistic analysis engine — the concrete side of `rlt-core`'s [`rlt_core::Engine`] seam.
//!
//! **M2 replaces the body** of this crate with a dependency-bumped vendoring of nlprule's
//! tokenizer / tagger / chunker / disambiguator, kept entirely behind [`VendoredEngine`] so the
//! rest of the workspace never names an nlprule type. The trait boundary is what makes a later
//! custom-engine swap a drop-in.
//!
//! For M0 this is a placeholder whitespace tokenizer with empty POS tags — just enough for the
//! CLI and WASM surfaces to instantiate a real [`rlt_core::Engine`] and run end to end.

#![forbid(unsafe_code)]

use rlt_core::{Analysis, Engine, Span, Token};

/// The vendored-nlprule analysis engine (placeholder implementation until M2).
#[derive(Debug, Default, Clone)]
pub struct VendoredEngine {
    _private: (),
}

impl VendoredEngine {
    /// Construct the engine. M2 will load the rkyv-archived tagger/dictionary artifact here.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Engine for VendoredEngine {
    fn analyze(&self, text: &str) -> Analysis {
        // Placeholder: split on Unicode whitespace, recording byte spans. M2 swaps this for the
        // real nlprule pipeline (proper tokenization, POS tags, chunks, disambiguation).
        let tokens = text
            .split_whitespace()
            .map(|word| {
                // `split_whitespace` borrows from `text`, so pointer arithmetic recovers the span.
                let start = word.as_ptr() as usize - text.as_ptr() as usize;
                Token {
                    text: word.to_owned(),
                    span: Span {
                        start,
                        end: start + word.len(),
                    },
                    tags: Vec::new(),
                }
            })
            .collect();
        Analysis { tokens }
    }
}
