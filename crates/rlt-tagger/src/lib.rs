//! L4 — neural edit-tagger (GECToR-style), the last cascade stage.
//!
//! A GECToR tagger predicts, per input word, an **edit tag** (`$KEEP` / `$DELETE` / `$APPEND_x` /
//! `$REPLACE_x` / `$TRANSFORM_*`) plus a binary *detect* signal (is this token erroneous?). This
//! crate owns the **decoding** half — turning those per-word predictions into [`Diagnostic`]s with
//! byte spans and replacements — behind a backend-agnostic [`TagSource`] seam. The neural model that
//! actually produces the predictions (an `rten` int8 ONNX graph + a RoBERTa tokenizer) is a separate
//! [`TagSource`] implementation, kept out of the decoder so the edit logic is unit-testable without a
//! model and so the same decoder serves native and wasm.
//!
//! Decoding mirrors GECToR inference: a sentence-level *error gate* (skip everything unless some
//! token's error probability clears [`TaggerConfig::min_error_probability`]) and a per-word *keep
//! bias* ([`TaggerConfig::keep_confidence`] added to `$KEEP` before the edit wins) — the two knobs
//! that trade recall for the precision a writer-facing tool needs. Single pass for now (GECToR's
//! iterative refinement is a later refinement).

#![forbid(unsafe_code)]

use rlt_core::{Analysis, Diagnostic, GrammarChecker, Source, Span, Suggestion};

/// A whitespace-delimited input word, as a byte range into the source text (GECToR operates on
/// space-tokenized words, not the engine's linguistic tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordSpan {
    /// Byte offset of the word's first byte.
    pub start: usize,
    /// Byte offset one past the word's last byte.
    pub end: usize,
}

/// The per-word prediction a [`TagSource`] yields, reduced to what decoding needs: the winning
/// non-keep edit (label index + its probability), the `$KEEP` probability, and the detect head's
/// error probability. Keeping this small (rather than a full ~5k-way distribution) lets the decoder
/// be tested with hand-written predictions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WordPred {
    /// Label index of the most probable **non-`$KEEP`** edit for this word.
    pub edit_label: usize,
    /// Probability mass on that edit.
    pub edit_prob: f32,
    /// Probability mass on `$KEEP` (no change).
    pub keep_prob: f32,
    /// Detect-head probability that this word is erroneous.
    pub error_prob: f32,
}

/// Produces a [`WordPred`] for each input word. The neural `rten` backend is one implementor; tests
/// use a scripted mock. `predict` must return one entry per word in `words` (order-aligned).
pub trait TagSource {
    /// Predict an edit/keep/error signal for every word in `words` (drawn from `text`).
    fn predict(&self, words: &[WordSpan], text: &str) -> Vec<WordPred>;
}

/// The edit-tag vocabulary (index → tag string), e.g. loaded from the artifact's `labels.json`.
#[derive(Debug, Clone)]
pub struct Labels {
    tags: Vec<String>,
}

impl Labels {
    /// Build from the ordered tag vocabulary (index = label id).
    #[must_use]
    pub fn new(tags: Vec<String>) -> Self {
        Self { tags }
    }

    /// The tag string for a label index, if in range.
    #[must_use]
    pub fn tag(&self, index: usize) -> Option<&str> {
        self.tags.get(index).map(String::as_str)
    }

    /// Parse the label at `index` into an [`Edit`]; unknown/unsupported tags decode to
    /// [`Edit::Unsupported`] so they are simply skipped rather than misapplied.
    #[must_use]
    pub fn edit(&self, index: usize) -> Edit {
        self.tag(index).map_or(Edit::Unsupported, parse_tag)
    }
}

/// A decoded GECToR edit. The transform/merge/split families that need external dictionaries
/// ([`Edit::Unsupported`]) are deferred; the core text edits are handled directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// `$KEEP` — leave the word unchanged.
    Keep,
    /// `$DELETE` — remove the word.
    Delete,
    /// `$APPEND_x` — insert token `x` after the word.
    Append(String),
    /// `$REPLACE_x` — replace the word with token `x`.
    Replace(String),
    /// `$TRANSFORM_CASE_*` — recase the word.
    Case(CaseOp),
    /// A recognised-but-not-yet-handled tag (`$TRANSFORM_VERB_*`, `$MERGE_*`, padding, …).
    Unsupported,
}

/// A `$TRANSFORM_CASE_*` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseOp {
    /// Lower-case the whole word.
    Lower,
    /// Upper-case the whole word.
    Upper,
    /// Upper-case the first character only.
    Capital,
}

/// Parse a single GECToR tag string into an [`Edit`].
fn parse_tag(tag: &str) -> Edit {
    match tag {
        "$KEEP" => Edit::Keep,
        "$DELETE" => Edit::Delete,
        "$TRANSFORM_CASE_LOWER" => Edit::Case(CaseOp::Lower),
        "$TRANSFORM_CASE_UPPER" => Edit::Case(CaseOp::Upper),
        "$TRANSFORM_CASE_CAPITAL" => Edit::Case(CaseOp::Capital),
        _ => {
            if let Some(t) = tag.strip_prefix("$APPEND_") {
                Edit::Append(t.to_owned())
            } else if let Some(t) = tag.strip_prefix("$REPLACE_") {
                Edit::Replace(t.to_owned())
            } else {
                Edit::Unsupported
            }
        }
    }
}

/// Decoder knobs (GECToR inference parameters), tuned **after** quantization against the precision
/// target. Defaults bias toward precision (a writer hates false positives more than misses).
#[derive(Debug, Clone, Copy)]
pub struct TaggerConfig {
    /// Added to each word's `$KEEP` probability before an edit can win — higher = fewer edits.
    pub keep_confidence: f32,
    /// A sentence is only edited if some word's detect error probability reaches this.
    pub min_error_probability: f32,
}

impl Default for TaggerConfig {
    fn default() -> Self {
        // Conservative starting point; M7.7 calibrates these to the precision@recall target.
        Self {
            keep_confidence: 0.0,
            min_error_probability: 0.5,
        }
    }
}

/// The L4 tagger: a [`TagSource`] backend plus the label vocabulary and decoding config. Implements
/// [`GrammarChecker`] so it stacks onto the cascade via `rlt_core::WithGrammar`.
pub struct Tagger<S: TagSource> {
    source: S,
    labels: Labels,
    config: TaggerConfig,
}

impl<S: TagSource> Tagger<S> {
    /// Assemble a tagger from a prediction backend, its label vocabulary, and decoding config.
    pub fn new(source: S, labels: Labels, config: TaggerConfig) -> Self {
        Self {
            source,
            labels,
            config,
        }
    }

    /// Decode predictions into diagnostics — the pure half, independent of how `predict` is backed.
    fn decode(&self, text: &str) -> Vec<Diagnostic> {
        let words = split_words(text);
        if words.is_empty() {
            return Vec::new();
        }
        let preds = self.source.predict(&words, text);
        // Sentence-level error gate: unless some word is confidently erroneous, emit nothing.
        if !preds
            .iter()
            .any(|p| p.error_prob >= self.config.min_error_probability)
        {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (word, pred) in words.iter().zip(&preds) {
            // Keep bias: the edit must beat `$KEEP` by the configured margin to fire.
            if pred.edit_prob <= pred.keep_prob + self.config.keep_confidence {
                continue;
            }
            if let Some(diag) = edit_to_diagnostic(&self.labels.edit(pred.edit_label), *word, text) {
                out.push(diag);
            }
        }
        out
    }
}

impl<S: TagSource> GrammarChecker for Tagger<S> {
    fn grammar_diagnostics(&self, text: &str, _analysis: &Analysis) -> Vec<Diagnostic> {
        self.decode(text)
    }
}

/// Turn one decoded edit on `word` into a [`Diagnostic`], or `None` for no-ops/unsupported tags.
fn edit_to_diagnostic(edit: &Edit, word: WordSpan, text: &str) -> Option<Diagnostic> {
    let surface = text.get(word.start..word.end)?;
    let replacement = match edit {
        Edit::Keep | Edit::Unsupported => return None,
        Edit::Delete => String::new(),
        Edit::Append(t) => format!("{surface} {t}"),
        Edit::Replace(t) => t.clone(),
        Edit::Case(op) => apply_case(surface, *op),
    };
    if replacement == surface {
        return None;
    }
    Some(Diagnostic {
        span: Span {
            start: word.start,
            end: word.end,
        },
        code: "NEURAL".to_owned(),
        message: "Possible grammatical error.".to_owned(),
        suggestions: vec![Suggestion { replacement }],
        source: Source::Neural,
    })
}

/// Apply a `$TRANSFORM_CASE_*` op to a word surface.
fn apply_case(surface: &str, op: CaseOp) -> String {
    match op {
        CaseOp::Lower => surface.to_lowercase(),
        CaseOp::Upper => surface.to_uppercase(),
        CaseOp::Capital => {
            let mut chars = surface.chars();
            chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().collect::<String>() + chars.as_str()
            })
        }
    }
}

/// Split `text` into whitespace-delimited words with their byte ranges (GECToR's pre-tokenization).
#[must_use]
pub fn split_words(text: &str) -> Vec<WordSpan> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                words.push(WordSpan { start: s, end: i });
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        words.push(WordSpan {
            start: s,
            end: text.len(),
        });
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted [`TagSource`] returning fixed predictions — lets us test the decoder with no model.
    struct MockSource(Vec<WordPred>);
    impl TagSource for MockSource {
        fn predict(&self, _words: &[WordSpan], _text: &str) -> Vec<WordPred> {
            self.0.clone()
        }
    }

    /// Vocabulary used across the decoder tests.
    fn labels() -> Labels {
        Labels::new(vec![
            "$KEEP".to_owned(),               // 0
            "$DELETE".to_owned(),             // 1
            "$REPLACE_believe".to_owned(),    // 2
            "$APPEND_the".to_owned(),         // 3
            "$TRANSFORM_CASE_CAPITAL".to_owned(), // 4
            "$TRANSFORM_VERB_VB_VBZ".to_owned(),  // 5 (unsupported for now)
        ])
    }

    fn keep() -> WordPred {
        WordPred {
            edit_label: 0,
            edit_prob: 0.01,
            keep_prob: 0.99,
            error_prob: 0.0,
        }
    }

    fn edit(label: usize) -> WordPred {
        WordPred {
            edit_label: label,
            edit_prob: 0.95,
            keep_prob: 0.05,
            error_prob: 0.95,
        }
    }

    fn run(text: &str, preds: Vec<WordPred>, config: TaggerConfig) -> Vec<Diagnostic> {
        Tagger::new(MockSource(preds), labels(), config)
            .grammar_diagnostics(text, &Analysis::default())
    }

    #[test]
    fn splits_words_with_byte_spans() {
        let spans = split_words("  hi  world ");
        assert_eq!(spans, vec![
            WordSpan { start: 2, end: 4 },
            WordSpan { start: 6, end: 11 },
        ]);
    }

    #[test]
    fn replace_edit_yields_span_and_suggestion() {
        // "I beleive it" → replace word 1 with "believe".
        let text = "I beleive it";
        let diags = run(text, vec![keep(), edit(2), keep()], TaggerConfig::default());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(&text[diags[0].span.start..diags[0].span.end], "beleive");
        assert_eq!(diags[0].suggestions[0].replacement, "believe");
        assert_eq!(diags[0].source, Source::Neural);
    }

    #[test]
    fn append_edit_expands_to_self_contained_suggestion() {
        // "in morning" → append "the" after "in" → "in the".
        let diags = run("in morning", vec![edit(3), keep()], TaggerConfig::default());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "in the");
    }

    #[test]
    fn delete_edit_suggests_empty_replacement() {
        let diags = run("the the cat", vec![keep(), edit(1), keep()], TaggerConfig::default());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "");
    }

    #[test]
    fn case_transform_capitalizes() {
        let diags = run("i went", vec![edit(4), keep()], TaggerConfig::default());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "I");
    }

    #[test]
    fn unsupported_tag_is_skipped() {
        // Verb-transform tag isn't handled yet → no diagnostic even though it "fires".
        let diags = run("she run", vec![keep(), edit(5)], TaggerConfig::default());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn keep_bias_suppresses_low_margin_edits() {
        // Edit barely beats keep; a keep_confidence margin suppresses it.
        let pred = WordPred {
            edit_label: 2,
            edit_prob: 0.55,
            keep_prob: 0.45,
            error_prob: 0.95,
        };
        let cfg = TaggerConfig {
            keep_confidence: 0.3,
            ..TaggerConfig::default()
        };
        assert!(run("I beleive it", vec![keep(), pred, keep()], cfg).is_empty());
    }

    #[test]
    fn sentence_error_gate_blocks_when_no_word_is_erroneous() {
        // A confident edit, but the detect head says nothing is wrong → emit nothing.
        let pred = WordPred {
            edit_label: 2,
            edit_prob: 0.95,
            keep_prob: 0.05,
            error_prob: 0.1, // below min_error_probability
        };
        assert!(run("I beleive it", vec![keep(), pred, keep()], TaggerConfig::default()).is_empty());
    }
}
