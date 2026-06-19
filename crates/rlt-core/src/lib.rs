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

mod disambig;
mod matcher;
mod spell;
#[doc(hidden)]
pub use spell::fuzz_edits1;

pub use disambig::ConfusionChecker;
pub use matcher::{Disambiguator, IrMatcher};

use serde::{Deserialize, Serialize};

/// Upper-case the first character of `s`, leaving the rest unchanged (`receive` → `Receive`).
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// Append `value` to `out` iff it is non-empty and not already present (order-preserving unique) —
/// taggers yield the same POS/lemma across a word's analyses, and downstream wants each once.
pub fn push_unique(out: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !out.iter().any(|v| v == value) {
        out.push(value.to_owned());
    }
}

/// Re-case `candidate` to match `source`'s leading capitalization (so `Recieve` → `Receive`, not
/// `receive`); leaves it untouched when `source` starts lower-case.
pub(crate) fn recase(source: &str, candidate: &str) -> String {
    if source.chars().next().is_some_and(char::is_uppercase) {
        capitalize_first(candidate)
    } else {
        candidate.to_owned()
    }
}

/// A half-open byte range `[start, end)` into the checked text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
    /// L3 — statistical confusion-pair disambiguation (real-word errors, e.g. their/there).
    Statistical,
    /// L4 — neural edit-tagger (GECToR-style; the long-tail grammatical errors L1–L3 miss).
    Neural,
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

/// A single analysed token: surface text, its span, POS tags and lemmas.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Token {
    /// The token's surface form as it appears in the source text.
    pub text: String,
    /// Where the token sits in the source text.
    pub span: Span,
    /// Part-of-speech tags from the engine's tagger (LT tagset).
    pub tags: Vec<String>,
    /// Lemmas (base forms) the tagger assigned — used by L2 `inflected` token matching.
    pub lemmas: Vec<String>,
}

/// The result of running an [`Engine`] over a piece of text: the token graph downstream layers walk.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
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
    /// Produce grammar/style diagnostics for `text`, given its already-computed [`Analysis`] (the
    /// IR matcher walks the token graph; the nlprule baseline uses `text` directly).
    fn grammar_diagnostics(&self, text: &str, analysis: &Analysis) -> Vec<Diagnostic>;
}

/// Wires the L1 spelling and L2 grammar layers and runs the cascade over text.
///
/// Generic over a backend providing both seams so the concrete type is a compile-time choice at
/// the binary, with no dynamic dispatch on the hot path.
pub struct Checker<B: Engine + GrammarChecker> {
    backend: B,
    /// The script's lower-case alphabet, driving L1 spell-check membership + edit generation.
    /// Defaults to [`spell::ASCII_ALPHABET`] (en/de); the CLI/wasm pass the language's alphabet.
    alphabet: &'static str,
    /// The L1 spelling-diagnostic message, in the checked language (LanguageTool's localized
    /// `spelling` string). Defaults to English; the CLI/wasm pass the language's variant.
    spell_message: &'static str,
}

/// English default for the L1 spelling message when no language-specific one is supplied (the nlprule
/// baseline path); the native path passes the localized [`rlt_lang::SpellConfig::message`].
const DEFAULT_SPELL_MESSAGE: &str = "Possible spelling mistake found.";

impl<B: Engine + GrammarChecker> Checker<B> {
    /// Construct a checker over the given analysis + grammar backend, spell-checking against the
    /// ASCII alphabet (the historical English/German default).
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            alphabet: spell::ASCII_ALPHABET,
            spell_message: DEFAULT_SPELL_MESSAGE,
        }
    }

    /// Construct a checker whose L1 spell-checker uses the active language's `alphabet` (e.g. Cyrillic
    /// for Russian) and localized `spell_message` — both from `rlt_lang::SpellConfig`.
    pub fn with_spell(backend: B, alphabet: &'static str, spell_message: &'static str) -> Self {
        Self {
            backend,
            alphabet,
            spell_message,
        }
    }

    /// Run the full cascade (L1 spelling + L2 grammar) over `text` and return all diagnostics,
    /// ordered by start position.
    #[must_use]
    pub fn check(&self, text: &str) -> Vec<Diagnostic> {
        let analysis = self.backend.analyze(text);
        let mut diagnostics = spell::spelling_diagnostics(
            &self.backend,
            &analysis,
            self.alphabet,
            self.spell_message,
        );
        diagnostics.extend(self.backend.grammar_diagnostics(text, &analysis));
        strip_noop_suggestions(text, &mut diagnostics);
        diagnostics.sort_by_key(|d| d.span.start);
        diagnostics
    }
}

/// Drop "fixes" that would replace a span with the text it already contains. Such **no-op suggestions**
/// are the formal signature of a *cyclic correction*: the user applies the fix, the text is unchanged,
/// the same diagnostic re-fires, and the editor offers the identical no-op again. They arise from
/// false-positive matches (e.g. a spacing rule firing on already-correct text and reconstructing it) or
/// a suggestion template that rebuilds its own input.
///
/// Enforcing "every surfaced suggestion strictly changes its span" makes the cyclic state
/// *unrepresentable* at this single chokepoint (every concrete checker funnels through here): applying
/// any offered fix is guaranteed to change the text, so it always makes progress. A diagnostic whose
/// suggestions were *all* no-ops has no real change to offer and is itself spurious, so it is removed;
/// a diagnostic that never carried a suggestion is informational (nothing to apply, cannot cycle) and
/// is kept.
fn strip_noop_suggestions(text: &str, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.retain_mut(|d| {
        if d.suggestions.is_empty() {
            return true;
        }
        let spanned = text.get(d.span.start..d.span.end).unwrap_or_default();
        d.suggestions.retain(|s| s.replacement != spanned);
        !d.suggestions.is_empty()
    });
}

/// Composes a separate [`Engine`] and [`GrammarChecker`] into one backend — e.g. nlprule for
/// analysis (L0/L1) plus the IR matcher for L2 — so [`Checker`] can drive both.
pub struct Composite<E: Engine, G: GrammarChecker> {
    engine: E,
    grammar: G,
}

impl<E: Engine, G: GrammarChecker> Composite<E, G> {
    /// Combine an analysis engine with a grammar checker.
    pub fn new(engine: E, grammar: G) -> Self {
        Self { engine, grammar }
    }
}

impl<E: Engine, G: GrammarChecker> Engine for Composite<E, G> {
    fn analyze(&self, text: &str) -> Analysis {
        self.engine.analyze(text)
    }
    fn is_known(&self, word: &str) -> bool {
        self.engine.is_known(word)
    }
}

impl<E: Engine, G: GrammarChecker> GrammarChecker for Composite<E, G> {
    fn grammar_diagnostics(&self, text: &str, analysis: &Analysis) -> Vec<Diagnostic> {
        self.grammar.grammar_diagnostics(text, analysis)
    }
}

/// Stacks an additional [`GrammarChecker`] `G` onto an [`Engine`] + [`GrammarChecker`] backend `B`,
/// **concatenating** both layers' diagnostics — additive composition where `G` never overrides `B`.
/// This is the seam every new cascade layer slots onto: analysis (`Engine`) is delegated to `B`,
/// and `grammar_diagnostics` runs `B` first, then appends `G`'s findings. L3 confusion ([`WithConfusion`])
/// and the L4 neural tagger are both just specialisations of this.
pub struct WithGrammar<B: Engine + GrammarChecker, G: GrammarChecker> {
    inner: B,
    extra: G,
}

impl<B: Engine + GrammarChecker, G: GrammarChecker> WithGrammar<B, G> {
    /// Wrap `inner`, appending `extra`'s diagnostics to the cascade.
    pub fn new(inner: B, extra: G) -> Self {
        Self { inner, extra }
    }
}

impl<B: Engine + GrammarChecker, G: GrammarChecker> Engine for WithGrammar<B, G> {
    fn analyze(&self, text: &str) -> Analysis {
        self.inner.analyze(text)
    }
    fn is_known(&self, word: &str) -> bool {
        self.inner.is_known(word)
    }
}

impl<B: Engine + GrammarChecker, G: GrammarChecker> GrammarChecker for WithGrammar<B, G> {
    fn grammar_diagnostics(&self, text: &str, analysis: &Analysis) -> Vec<Diagnostic> {
        let mut diagnostics = self.inner.grammar_diagnostics(text, analysis);
        diagnostics.extend(self.extra.grammar_diagnostics(text, analysis));
        diagnostics
    }
}

/// L3 real-word-error detection stacked onto any backend — the [`WithGrammar`] specialisation for
/// [`ConfusionChecker`]. With an empty model it is a no-op. Construct via `WithConfusion::new`.
pub type WithConfusion<B> = WithGrammar<B, ConfusionChecker>;

#[cfg(test)]
mod noop_guard_tests {
    use super::*;

    fn diag(start: usize, end: usize, reps: &[&str]) -> Diagnostic {
        Diagnostic {
            span: Span { start, end },
            code: "TEST".to_owned(),
            message: String::new(),
            suggestions: reps
                .iter()
                .map(|r| Suggestion {
                    replacement: (*r).to_owned(),
                })
                .collect(),
            source: Source::Grammar,
        }
    }

    #[test]
    fn drops_diagnostic_whose_only_suggestion_is_identity() {
        // ESPACIO firing on correctly-spaced text: span ". No" suggested as ". No".
        let text = "pan. No se";
        let mut d = vec![diag(3, 7, &[". No"])];
        strip_noop_suggestions(text, &mut d);
        assert!(
            d.is_empty(),
            "a sole no-op suggestion → drop the spurious diagnostic"
        );
    }

    #[test]
    fn keeps_real_fix_that_changes_the_span() {
        // ESPACIO firing on genuinely-missing space: span ".No" suggested as ". No".
        let text = "pan.No se";
        let mut d = vec![diag(3, 6, &[". No"])];
        strip_noop_suggestions(text, &mut d);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].suggestions.len(), 1);
    }

    #[test]
    fn strips_only_the_noop_when_mixed() {
        let text = "teh cat";
        let mut d = vec![diag(0, 3, &["teh", "the"])]; // identity + real
        strip_noop_suggestions(text, &mut d);
        assert_eq!(d.len(), 1);
        assert_eq!(
            d[0].suggestions,
            vec![Suggestion {
                replacement: "the".to_owned()
            }]
        );
    }

    #[test]
    fn keeps_informational_diagnostic_with_no_suggestions() {
        // A spell flag with no candidate (e.g. an OOV with no edit-1 hit) cannot cycle → keep it.
        let text = "xyzzy";
        let mut d = vec![diag(0, 5, &[])];
        strip_noop_suggestions(text, &mut d);
        assert_eq!(d.len(), 1);
    }
}
