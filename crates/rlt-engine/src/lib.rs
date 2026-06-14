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

use nlprule::{Rules, Tokenizer};
use rlt_core::{
    Analysis, Diagnostic, Engine, GrammarChecker, Source, Span, Suggestion, Token, push_unique,
};

// The nlprule baseline runs its own pipeline from `text`, so it ignores the precomputed analysis.

/// Default on-disk location of the nlprule English tokenizer binary (`cargo xtask fetch-engine`).
pub const DEFAULT_TOKENIZER_BIN: &str = "resources/en_tokenizer.bin";
/// Default on-disk location of the nlprule English grammar-rules binary.
pub const DEFAULT_RULES_BIN: &str = "resources/en_rules.bin";

/// Errors constructing a [`VendoredEngine`].
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// An nlprule binary could not be opened or deserialized.
    #[error("loading nlprule binary: {0}")]
    Load(#[from] nlprule::Error),
    /// An nlprule binary path could not be read.
    #[error("reading nlprule binary: {0}")]
    Io(#[from] std::io::Error),
}

/// The baseline nlprule-backed engine: tokenizer (L0/L1 analysis) plus optional grammar rules (L2).
///
/// Grammar rules are optional so analysis-only uses (`rlt tokens`, the tokenizer test) need not
/// load the ~7.5 MB rules binary; [`GrammarChecker`] yields nothing until they are attached.
pub struct VendoredEngine {
    tokenizer: Tokenizer,
    rules: Option<Rules>,
}

impl VendoredEngine {
    /// Load the analysis engine from an `en_tokenizer.bin` on disk (the native path).
    ///
    /// # Errors
    /// Returns [`EngineError`] if the file is missing or not a valid nlprule tokenizer binary.
    pub fn from_path(path: &Path) -> Result<Self, EngineError> {
        Ok(Self {
            tokenizer: Tokenizer::new(path)?,
            rules: None,
        })
    }

    /// Load the analysis engine from an in-memory `en_tokenizer.bin` (the wasm path).
    ///
    /// # Errors
    /// Returns [`EngineError`] if the bytes are not a valid nlprule tokenizer binary.
    pub fn from_reader<R: Read>(reader: R) -> Result<Self, EngineError> {
        Ok(Self {
            tokenizer: Tokenizer::from_reader(reader)?,
            rules: None,
        })
    }

    /// Attach grammar rules from an `en_rules.bin` on disk (enables L2).
    ///
    /// # Errors
    /// Returns [`EngineError`] if the file is missing or not a valid nlprule rules binary.
    pub fn with_rules_path(mut self, path: &Path) -> Result<Self, EngineError> {
        self.rules = Some(Rules::new(path)?);
        Ok(self)
    }

    /// Attach grammar rules from in-memory `en_rules.bin` bytes (the wasm path; enables L2).
    ///
    /// # Errors
    /// Returns [`EngineError`] if the bytes are not a valid nlprule rules binary.
    pub fn with_rules_reader<R: Read>(mut self, reader: R) -> Result<Self, EngineError> {
        self.rules = Some(Rules::from_reader(reader)?);
        Ok(self)
    }

    /// The distinct lexical POS tags the tagger assigns to `word` (the first is the primary form).
    /// Used at build time to derive the L3 POS-context statistics; empty for unknown words.
    #[must_use]
    pub fn pos_tags(&self, word: &str) -> Vec<String> {
        let mut tags = Vec::new();
        for d in self.tokenizer.tagger().get_tags(word) {
            push_unique(&mut tags, d.pos().as_str());
        }
        tags
    }

    /// The raw `(lemma, pos)` analyses nlprule's tagger records for `word`, in lexicon order — the
    /// context-free dictionary lookup (no disambiguation). Feeds the native engine's P1 bootstrap
    /// dictionary (a direct dump of nlprule's tagger) so its differential test isolates engine-code
    /// bugs from data differences. Empty for unknown words.
    #[must_use]
    pub fn word_data(&self, word: &str) -> Vec<(String, String)> {
        self.tokenizer
            .tagger()
            .get_tags(word)
            .map(|d| (d.lemma().as_str().to_owned(), d.pos().as_str().to_owned()))
            .collect()
    }
}

impl GrammarChecker for VendoredEngine {
    fn grammar_diagnostics(&self, text: &str, _analysis: &Analysis) -> Vec<Diagnostic> {
        let Some(rules) = &self.rules else {
            return Vec::new();
        };
        rules
            .suggest(text, &self.tokenizer)
            .into_iter()
            .map(|s| Diagnostic {
                span: Span {
                    start: s.span().byte().start,
                    end: s.span().byte().end,
                },
                code: s.source().to_owned(),
                message: s.message().to_owned(),
                suggestions: s
                    .replacements()
                    .iter()
                    .map(|r| Suggestion {
                        replacement: r.clone(),
                    })
                    .collect(),
                source: Source::Grammar,
            })
            .collect()
    }
}

impl Engine for VendoredEngine {
    fn is_known(&self, word: &str) -> bool {
        // get_tags does the lexicon lookup (trying lower-case and a compound-split heuristic
        // internally) and yields nothing for genuinely-unknown words; the pipeline's UNKNOWN tag
        // is added downstream, not here.
        self.tokenizer.tagger().get_tags(word).next().is_some()
    }

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
                let mut tags = Vec::new();
                let mut lemmas = Vec::new();
                for d in word.tags() {
                    push_unique(&mut tags, d.pos().as_str());
                    push_unique(&mut lemmas, d.lemma().as_str());
                }
                tokens.push(Token {
                    text: surface.to_owned(),
                    span: Span {
                        start: byte.start,
                        end: byte.end,
                    },
                    tags,
                    lemmas,
                });
            }
        }
        Analysis { tokens }
    }
}
