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

mod infer;

pub use infer::{Meta, RtenTagSource, TaggerError};

use std::collections::HashMap;

use rlt_core::{Analysis, Diagnostic, GrammarChecker, Source, Span, Suggestion};

/// Grammarly's verb-form transform dictionary, keyed `"{word}_{from_tag}_{to_tag}"` → target form
/// (e.g. `"go_VB_VBZ"` → `"goes"`), used to apply `$TRANSFORM_VERB_*` tags.
#[derive(Debug, Clone, Default)]
pub struct VerbDict(HashMap<String, String>);

impl VerbDict {
    /// Parse `verb-form-vocab.txt` (`word1_word2:TAG1_TAG2` lines) into the decode map. First
    /// mapping for a key wins (matching gector's loader).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut map: HashMap<String, String> = HashMap::new();
        for line in text.lines() {
            let Some((words, tags)) = line.split_once(':') else {
                continue;
            };
            let Some((from_word, to_word)) = words.split_once('_') else {
                continue;
            };
            // decode key = "{word1}_{TAG1}_{TAG2}" = "{from_word}_{tags}".
            map.entry(format!("{from_word}_{}", tags.trim()))
                .or_insert_with(|| to_word.to_owned());
        }
        Self(map)
    }

    /// Resolve a `$TRANSFORM_VERB_{tags}` for `surface` (`tags` like `"VB_VBZ"`).
    fn apply(&self, surface: &str, tags: &str) -> Option<&str> {
        self.0.get(&format!("{surface}_{tags}")).map(String::as_str)
    }
}

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

/// A decoded GECToR edit. `$MERGE_*` (needs the following word) and padding/OOV tags decode to
/// [`Edit::Unsupported`] and are skipped; everything else is applied directly.
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
    /// `$TRANSFORM_AGREEMENT_PLURAL` — append `s`.
    Pluralize,
    /// `$TRANSFORM_AGREEMENT_SINGULAR` — drop the trailing character.
    Singularize,
    /// `$TRANSFORM_SPLIT_HYPHEN` — replace hyphens with spaces.
    SplitHyphen,
    /// `$TRANSFORM_VERB_{from}_{to}` — verb-form change resolved via the [`VerbDict`] (the `tags`
    /// suffix, e.g. `"VB_VBZ"`).
    Verb(String),
    /// A recognised-but-not-applied tag (`$MERGE_*`, `<PAD>`, `<OOV>`, …).
    Unsupported,
}

/// A `$TRANSFORM_CASE_*` operation (semantics match gector's `g_transform_processer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseOp {
    /// Lower-case the whole word.
    Lower,
    /// Upper-case the whole word.
    Upper,
    /// Capitalize: first char upper, the rest lower-cased (`$TRANSFORM_CASE_CAPITAL`).
    Capital,
    /// Keep the first char, capitalize the remainder (`$TRANSFORM_CASE_CAPITAL_1`).
    CapitalRest,
}

/// Parse a single GECToR tag string into an [`Edit`].
fn parse_tag(tag: &str) -> Edit {
    match tag {
        "$KEEP" => Edit::Keep,
        "$DELETE" => Edit::Delete,
        "$TRANSFORM_CASE_LOWER" => Edit::Case(CaseOp::Lower),
        "$TRANSFORM_CASE_UPPER" => Edit::Case(CaseOp::Upper),
        "$TRANSFORM_CASE_CAPITAL" => Edit::Case(CaseOp::Capital),
        "$TRANSFORM_CASE_CAPITAL_1" => Edit::Case(CaseOp::CapitalRest),
        "$TRANSFORM_AGREEMENT_PLURAL" => Edit::Pluralize,
        "$TRANSFORM_AGREEMENT_SINGULAR" => Edit::Singularize,
        "$TRANSFORM_SPLIT_HYPHEN" => Edit::SplitHyphen,
        _ => {
            if let Some(t) = tag.strip_prefix("$APPEND_") {
                Edit::Append(t.to_owned())
            } else if let Some(t) = tag.strip_prefix("$REPLACE_") {
                Edit::Replace(t.to_owned())
            } else if let Some(tags) = tag.strip_prefix("$TRANSFORM_VERB_") {
                Edit::Verb(tags.to_owned())
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
    /// Confidence floor used as both the sentence gate (some word's detect error probability must
    /// reach this) and the token gate (an edit's own probability must reach this) — GECToR semantics.
    pub min_error_probability: f32,
    /// Max refinement passes (GECToR re-tags its own output to catch cascading errors). 1 = single
    /// pass. Each pass re-runs inference, so this trades latency for recall.
    pub max_iterations: usize,
}

impl Default for TaggerConfig {
    fn default() -> Self {
        // Calibrated post-quantization on BEA-2019 dev (pipeline ERRANT sweep): this combo maximized
        // F0.5 (precision-leaning), which is what an additive, writer-facing layer wants.
        Self {
            keep_confidence: 0.2,
            min_error_probability: 0.66,
            max_iterations: 5,
        }
    }
}

/// The L4 tagger: a [`TagSource`] backend plus the label vocabulary and decoding config. Implements
/// [`GrammarChecker`] so it stacks onto the cascade via `rlt_core::WithGrammar`.
pub struct Tagger<S: TagSource> {
    source: S,
    labels: Labels,
    config: TaggerConfig,
    verb_dict: VerbDict,
}

impl<S: TagSource> Tagger<S> {
    /// Assemble a tagger from a prediction backend, its label vocabulary, and decoding config.
    /// `$TRANSFORM_VERB_*` tags are no-ops until a [`VerbDict`] is attached with [`Self::with_verb_dict`].
    pub fn new(source: S, labels: Labels, config: TaggerConfig) -> Self {
        Self {
            source,
            labels,
            config,
            verb_dict: VerbDict::default(),
        }
    }

    /// Attach the verb-form dictionary so `$TRANSFORM_VERB_*` tags resolve.
    #[must_use]
    pub fn with_verb_dict(mut self, verb_dict: VerbDict) -> Self {
        self.verb_dict = verb_dict;
        self
    }

    /// Iteratively correct `text` (re-tagging its own output, GECToR-style, until it converges or
    /// hits `max_iterations`), then diff the result against the original to emit span-accurate
    /// diagnostics into `text`.
    fn decode(&self, text: &str) -> Vec<Diagnostic> {
        let words = split_words(text);
        if words.is_empty() {
            return Vec::new();
        }
        let original: Vec<String> = words.iter().map(|w| text[w.start..w.end].to_owned()).collect();

        let mut tokens = original.clone();
        for _ in 0..self.config.max_iterations.max(1) {
            let preds = self.predict_tokens(&tokens);
            let next = self.apply_edits(&tokens, &preds);
            if next == tokens {
                break; // converged — no more corrections
            }
            tokens = next;
        }
        diff_to_diagnostics(&words, &original, &tokens)
    }

    /// Predict over the current token sequence by reconstructing a synthetic text + spans the
    /// backend can tokenize (tokens never contain whitespace, so the split round-trips).
    fn predict_tokens(&self, tokens: &[String]) -> Vec<WordPred> {
        if tokens.is_empty() {
            return Vec::new();
        }
        let text = tokens.join(" ");
        let words = split_words(&text);
        self.source.predict(&words, &text)
    }

    /// Apply one pass of edits to `tokens` (GECToR's `edit_src_by_tags`): gated per the config, with
    /// `$DELETE` dropping a token and append/replace/split producing one or more.
    fn apply_edits(&self, tokens: &[String], preds: &[WordPred]) -> Vec<String> {
        // Sentence-level error gate: unless some token is confidently erroneous, change nothing.
        if !preds.iter().any(|p| p.error_prob >= self.config.min_error_probability) {
            return tokens.to_vec();
        }
        let mut out: Vec<String> = Vec::with_capacity(tokens.len());
        for (tok, pred) in tokens.iter().zip(preds) {
            // Keep unless the edit beats $KEEP by the margin AND clears the token confidence floor.
            let keep = pred.edit_prob <= pred.keep_prob + self.config.keep_confidence
                || pred.edit_prob < self.config.min_error_probability;
            if keep {
                out.push(tok.clone());
                continue;
            }
            match self.labels.edit(pred.edit_label) {
                Edit::Delete => {}
                Edit::Append(t) => {
                    out.push(tok.clone());
                    out.extend(t.split_whitespace().map(str::to_owned));
                }
                Edit::Replace(t) => out.extend(t.split_whitespace().map(str::to_owned)),
                Edit::Case(op) => out.push(apply_case(tok, op)),
                Edit::Pluralize => out.push(format!("{tok}s")),
                Edit::Singularize => {
                    let mut s = tok.clone();
                    s.pop();
                    out.push(s);
                }
                Edit::SplitHyphen => out.extend(tok.split('-').map(str::to_owned)),
                Edit::Verb(tags) => {
                    out.push(self.verb_dict.apply(tok, &tags).unwrap_or(tok.as_str()).to_owned());
                }
                Edit::Keep | Edit::Unsupported => out.push(tok.clone()),
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

/// Diff the original words against the corrected token sequence and emit one diagnostic per edit
/// region, with byte spans into the original text. Insertions fold into the adjacent original word
/// so each suggestion is a self-contained span replacement.
fn diff_to_diagnostics(
    words: &[WordSpan],
    original: &[String],
    corrected: &[String],
) -> Vec<Diagnostic> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};

    let mut out = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, original, corrected) {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete {
                old_index, old_len, ..
            } => push_edit(&mut out, words, original, old_index, old_len, String::new()),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let repl = corrected[new_index..new_index + new_len].join(" ");
                push_edit(&mut out, words, original, old_index, old_len, repl);
            }
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => {
                let inserted = corrected[new_index..new_index + new_len].join(" ");
                if old_index == 0 {
                    let repl = format!("{inserted} {}", original[0]);
                    push_edit(&mut out, words, original, 0, 1, repl);
                } else {
                    let repl = format!("{} {inserted}", original[old_index - 1]);
                    push_edit(&mut out, words, original, old_index - 1, 1, repl);
                }
            }
        }
    }
    out
}

/// Push a diagnostic spanning original words `[start, start + len)` with `replacement`, unless it is
/// a no-op (the replacement already equals the original span text).
fn push_edit(
    out: &mut Vec<Diagnostic>,
    words: &[WordSpan],
    original: &[String],
    start: usize,
    len: usize,
    replacement: String,
) {
    if len == 0 || replacement == original[start..start + len].join(" ") {
        return;
    }
    out.push(Diagnostic {
        span: Span {
            start: words[start].start,
            end: words[start + len - 1].end,
        },
        code: "NEURAL".to_owned(),
        message: "Possible grammatical error.".to_owned(),
        suggestions: vec![Suggestion { replacement }],
        source: Source::Neural,
    });
}

/// Apply a `$TRANSFORM_CASE_*` op to a word surface (matching gector's semantics).
fn apply_case(surface: &str, op: CaseOp) -> String {
    match op {
        CaseOp::Lower => surface.to_lowercase(),
        CaseOp::Upper => surface.to_uppercase(),
        CaseOp::Capital => capitalize(surface),
        CaseOp::CapitalRest => {
            // Keep the first char, capitalize the remainder (`token[0] + token[1:].capitalize()`).
            let mut chars = surface.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_string() + &capitalize(chars.as_str())
            })
        }
    }
}

/// First character upper-cased, the rest lower-cased (Python `str.capitalize`).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
    })
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
    use std::cell::Cell;

    use super::*;

    /// A keep-everything prediction.
    const KEEP: WordPred = WordPred {
        edit_label: 0,
        edit_prob: 0.0,
        keep_prob: 1.0,
        error_prob: 0.0,
    };

    /// A scripted [`TagSource`]: returns the given predictions on the first pass, then all-keep so
    /// the iterative decoder converges (a real model keep-tags its already-corrected output).
    struct MockSource {
        preds: Vec<WordPred>,
        calls: Cell<usize>,
    }
    impl MockSource {
        fn new(preds: Vec<WordPred>) -> Self {
            Self {
                preds,
                calls: Cell::new(0),
            }
        }
    }
    impl TagSource for MockSource {
        fn predict(&self, words: &[WordSpan], _text: &str) -> Vec<WordPred> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            if call == 0 && words.len() == self.preds.len() {
                self.preds.clone()
            } else {
                vec![KEEP; words.len()]
            }
        }
    }

    /// Vocabulary used across the decoder tests.
    fn labels() -> Labels {
        Labels::new(vec![
            "$KEEP".to_owned(),                       // 0
            "$DELETE".to_owned(),                     // 1
            "$REPLACE_believe".to_owned(),            // 2
            "$APPEND_the".to_owned(),                 // 3
            "$TRANSFORM_CASE_CAPITAL".to_owned(),     // 4
            "$TRANSFORM_VERB_VB_VBZ".to_owned(),      // 5 (needs the verb dict)
            "$TRANSFORM_AGREEMENT_PLURAL".to_owned(), // 6
            "$TRANSFORM_AGREEMENT_SINGULAR".to_owned(), // 7
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
        Tagger::new(MockSource::new(preds), labels(), config)
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
    fn verb_transform_without_dict_is_skipped() {
        // A $TRANSFORM_VERB tag with no verb dict attached can't resolve → no diagnostic.
        let diags = run("she run", vec![keep(), edit(5)], TaggerConfig::default());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn verb_transform_resolves_via_dict() {
        // "She go" → $TRANSFORM_VERB_VB_VBZ on "go" → "goes" via the verb dict.
        let vd = VerbDict::parse("go_goes:VB_VBZ\nrun_runs:VB_VBZ\n");
        let tagger =
            Tagger::new(MockSource::new(vec![keep(), edit(5)]), labels(), TaggerConfig::default())
                .with_verb_dict(vd);
        let diags = tagger.grammar_diagnostics("She go", &Analysis::default());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "goes");
    }

    #[test]
    fn agreement_transforms_apply() {
        // PLURAL appends 's'; SINGULAR drops the last char.
        let plural = run("one cat", vec![keep(), edit(6)], TaggerConfig::default());
        assert_eq!(plural[0].suggestions[0].replacement, "cats");
        let singular = run("two cats", vec![keep(), edit(7)], TaggerConfig::default());
        assert_eq!(singular[0].suggestions[0].replacement, "cat");
    }

    /// A [`TagSource`] scripted per pass (one prediction set per call), then all-keep.
    struct IterSource {
        scripts: Vec<Vec<WordPred>>,
        calls: Cell<usize>,
    }
    impl TagSource for IterSource {
        fn predict(&self, words: &[WordSpan], _text: &str) -> Vec<WordPred> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            match self.scripts.get(call) {
                Some(s) if s.len() == words.len() => s.clone(),
                _ => vec![KEEP; words.len()],
            }
        }
    }

    #[test]
    fn iterative_refinement_applies_cascading_edits() {
        // Pass 1 capitalizes "i"; pass 2 — on the now-corrected sentence — fixes "beleive". The
        // unchanged "am" between them keeps the two edits as separate diagnostic regions.
        let source = IterSource {
            scripts: vec![
                vec![edit(4), keep(), keep()], // iter 1: $TRANSFORM_CASE_CAPITAL on "i" -> "I"
                vec![keep(), keep(), edit(2)], // iter 2: $REPLACE_believe on "beleive"
            ],
            calls: Cell::new(0),
        };
        let diags = Tagger::new(source, labels(), TaggerConfig::default())
            .grammar_diagnostics("i am beleive", &Analysis::default());
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "I");
        assert_eq!(diags[1].suggestions[0].replacement, "believe");
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
