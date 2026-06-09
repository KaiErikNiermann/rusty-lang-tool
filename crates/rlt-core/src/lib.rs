//! Runtime checker core.
//!
//! This crate owns the **abstractions** every surface (CLI, WASM) and every cascade layer agree
//! on, but it does *not* depend on any concrete engine — the engine is injected. That dependency
//! inversion is the seam described in the plan: [`Engine`] is implemented by `rlt-engine` (a
//! vendored nlprule) today and can be swapped for a custom implementation later without touching
//! callers.
//!
//! ## Layout
//! - [`Engine`] — linguistic analysis seam (tokenize/tag/chunk → [`Analysis`]).
//! - [`Diagnostic`] & friends — the uniform output every layer emits.
//! - [`Checker`] — wires an [`Engine`] + L1 spelling + L2 rule matching and runs the cascade.
//!
//! M0 establishes these shapes; L1/L2 bodies arrive in M3/M4.

#![forbid(unsafe_code)]

mod spell;

use serde::{Deserialize, Serialize};

/// A half-open byte range `[start, end)` into the checked text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Byte offset of the first byte in the span.
    pub start: usize,
    /// Byte offset one past the last byte in the span.
    pub end: usize,
}

/// A proposed replacement for the text under a diagnostic's span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    /// The text to substitute for the span.
    pub replacement: String,
}

/// Which cascade layer produced a diagnostic. Lets the UI and tests attribute findings, and lets
/// later layers compose with (rather than override) earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// L1 — FSA dictionary spell check.
    Spelling,
    /// L2 — LanguageTool rule grammar.
    Grammar,
}

/// One finding: a span, the rule/source that flagged it, a human message, and ordered fixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The text region the finding applies to.
    pub span: Span,
    /// Machine-readable code: an LT rule id for [`Source::Grammar`], or `"SPELL"` for spelling.
    pub code: String,
    /// Human-readable explanation of the issue.
    pub message: String,
    /// Ordered candidate fixes (best first); may be empty.
    pub suggestions: Vec<Suggestion>,
    /// Which cascade layer produced this finding.
    pub source: Source,
}

/// A single analysed token: surface text, its span, and the POS tags assigned to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The token's surface form as it appears in the source text.
    pub text: String,
    /// Where the token sits in the source text.
    pub span: Span,
    /// Part-of-speech tags from the engine's tagger (LT tagset).
    pub tags: Vec<String>,
}

/// The result of running an [`Engine`] over a piece of text: the token graph downstream layers walk.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// Tokens in source order.
    pub tokens: Vec<Token>,
}

/// Linguistic analysis seam: tokenization, POS tagging, chunking and disambiguation.
///
/// The vendored nlprule lives behind this trait (`rlt-engine`); a future custom engine implements
/// the same trait and drops in unchanged. Nothing in `rlt-core` names a concrete engine type.
pub trait Engine {
    /// Tokenize, tag, chunk and disambiguate `text` into the token graph the cascade walks.
    fn analyze(&self, text: &str) -> Analysis;

    /// Whether `word` is in the engine's lexicon (any inflected form / casing). This is the L1
    /// spelling membership oracle and the validity filter for correction candidates. The future
    /// custom engine answers it from its own FSA dictionary.
    fn is_known(&self, word: &str) -> bool;
}

/// L2 — rule-based grammar/style checking seam.
///
/// The baseline (`rlt-engine`) wraps nlprule's rule engine; the on-thesis swap walks `rlt-ir`
/// rules (compiled from current LT) over the token graph. Either way it emits [`Source::Grammar`]
/// diagnostics, so the cascade and the example oracle are agnostic to which backs it.
pub trait GrammarChecker {
    /// Produce grammar/style diagnostics for `text`.
    fn grammar_diagnostics(&self, text: &str) -> Vec<Diagnostic>;
}

/// Wires the L1 spelling and L2 grammar layers and runs the cascade over text.
///
/// Generic over a backend providing both seams so the concrete type is a compile-time choice at
/// the binary, with no dynamic dispatch on the hot path.
pub struct Checker<B: Engine + GrammarChecker> {
    backend: B,
}

impl<B: Engine + GrammarChecker> Checker<B> {
    /// Construct a checker over the given analysis + grammar backend.
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Run the full cascade (L1 spelling + L2 grammar) over `text` and return all diagnostics,
    /// ordered by start position.
    #[must_use]
    pub fn check(&self, text: &str) -> Vec<Diagnostic> {
        let analysis = self.backend.analyze(text);
        let mut diagnostics = spell::spelling_diagnostics(&self.backend, &analysis);
        diagnostics.extend(self.backend.grammar_diagnostics(text));
        diagnostics.sort_by_key(|d| d.span.start);
        diagnostics
    }
}
