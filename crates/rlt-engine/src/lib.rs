//! Linguistic analysis engine — the concrete side of `rlt-core`'s [`rlt_core::Engine`] seam.
//!
//! The baseline engine wraps [`nlprule`]'s tokenizer (tokenization + POS tagging + chunking +
//! disambiguation), loaded from the prebuilt `en_tokenizer.bin` nlprule distributes on its GitHub
//! releases (derived from LanguageTool v5.2, LGPL-2.1). It is kept entirely behind
//! [`VendoredEngine`] so the rest of the workspace never names an nlprule type — the trait
//! boundary is what makes a later custom-engine swap (consuming current-LT data) a drop-in.
//!
//! nlprule is pulled in with `default-features = false, features = ["regex-fancy"]` so it stays
//! pure-Rust and compiles to `wasm32` (its `regex-onig` default needs a C library).

#![forbid(unsafe_code)]

use std::io::Read;
use std::path::Path;

use nlprule::Tokenizer;
use rlt_core::{Analysis, Engine, Span, Token};

/// Default on-disk location of the nlprule English tokenizer binary (`cargo xtask fetch-engine`).
pub const DEFAULT_TOKENIZER_BIN: &str = "resources/en_tokenizer.bin";

/// Errors constructing a [`VendoredEngine`].
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The nlprule tokenizer binary could not be opened or deserialized.
    #[error("loading nlprule tokenizer: {0}")]
    Load(#[from] nlprule::Error),
    /// The tokenizer binary path could not be read.
    #[error("reading tokenizer binary: {0}")]
    Io(#[from] std::io::Error),
}

/// The baseline nlprule-backed analysis engine.
pub struct VendoredEngine {
    tokenizer: Tokenizer,
}

impl VendoredEngine {
    /// Load the engine from an `en_tokenizer.bin` on disk (the native path).
    ///
    /// # Errors
    /// Returns [`EngineError`] if the file is missing or not a valid nlprule tokenizer binary.
    pub fn from_path(path: &Path) -> Result<Self, EngineError> {
        Ok(Self {
            tokenizer: Tokenizer::new(path)?,
        })
    }

    /// Load the engine from an in-memory `en_tokenizer.bin` (the wasm path: bytes supplied by JS).
    ///
    /// # Errors
    /// Returns [`EngineError`] if the bytes are not a valid nlprule tokenizer binary.
    pub fn from_reader<R: Read>(reader: R) -> Result<Self, EngineError> {
        Ok(Self {
            tokenizer: Tokenizer::from_reader(reader)?,
        })
    }
}

impl Engine for VendoredEngine {
    fn analyze(&self, text: &str) -> Analysis {
        let mut tokens = Vec::new();
        for sentence in self.tokenizer.pipe(text) {
            for token in sentence.iter() {
                let word = token.word();
                let surface = word.as_str();
                if surface.is_empty() {
                    continue; // skip nlprule's sentence-boundary sentinels
                }
                let byte = token.span().byte();
                let tags = word
                    .tags()
                    .iter()
                    .map(|d| d.pos().as_str().to_owned())
                    .filter(|p| !p.is_empty())
                    .collect();
                tokens.push(Token {
                    text: surface.to_owned(),
                    span: Span {
                        start: byte.start,
                        end: byte.end,
                    },
                    tags,
                });
            }
        }
        Analysis { tokens }
    }
}
