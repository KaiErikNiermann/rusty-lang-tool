//! Native analysis engine — the on-thesis replacement for the vendored nlprule engine.
//!
//! Pipeline (pure Rust, wasm-capable): `text → srx sentence segmentation → word tokenization → FST
//! POS tagging → disambiguation → Analysis`. It implements [`rlt_core::Engine`], so it drops in
//! behind the same seam nlprule sits behind today, but produces **current-LT** tags/lemmas (the
//! lever on the L2 oracle) instead of nlprule's LT-v5.2 ones.
//!
//! This module currently provides the segmentation + tokenization front of the pipeline (spans into
//! the source text); the FST tagger and disambiguation land in the following milestones.

#![forbid(unsafe_code)]

use std::ops::Range;

use rlt_core::{Span, Token};

/// Errors constructing the engine from its artifacts.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// `segment.srx` could not be parsed.
    #[error("parsing segment.srx: {0}")]
    Srx(String),
}

/// Sentence segmenter driven by LanguageTool's `segment.srx` (via the `srx` crate), specialized to
/// one language's rules.
pub struct Segmenter {
    rules: srx::Rules,
}

impl Segmenter {
    /// Build from the contents of `segment.srx`, selecting the rules for `lang` (e.g. `"en"`).
    ///
    /// # Errors
    /// Returns [`NativeError::Srx`] if the SRX XML does not parse.
    pub fn from_srx(srx_xml: &str, lang: &str) -> Result<Self, NativeError> {
        let srx: srx::SRX = srx_xml
            .parse()
            .map_err(|e| NativeError::Srx(format!("{e:?}")))?;
        Ok(Self {
            rules: srx.language_rules(lang),
        })
    }

    /// Byte ranges of the sentences in `text`.
    #[must_use]
    pub fn sentence_ranges(&self, text: &str) -> Vec<Range<usize>> {
        self.rules.split_ranges(text)
    }
}

/// Whether `c` belongs to a word token (vs. a standalone punctuation/symbol token). Apostrophe is
/// included so contractions stay one token for now; the differential test against nlprule will guide
/// refinements (hyphens, contraction splitting, …).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\''
}

/// Tokenize `sentence` into word + punctuation tokens (whitespace is a boundary, not a token),
/// appending to `out` with byte spans **absolute** into the source text (offset by `base`). Tags and
/// lemmas are left empty here — the tagger fills them.
pub fn tokenize_into(sentence: &str, base: usize, out: &mut Vec<Token>) {
    let mut word_start: Option<usize> = None;
    for (i, c) in sentence.char_indices() {
        if is_word_char(c) {
            word_start.get_or_insert(i);
            continue;
        }
        if let Some(s) = word_start.take() {
            out.push(make_token(sentence, s, i, base));
        }
        if !c.is_whitespace() {
            out.push(make_token(sentence, i, i + c.len_utf8(), base));
        }
    }
    if let Some(s) = word_start {
        out.push(make_token(sentence, s, sentence.len(), base));
    }
}

fn make_token(sentence: &str, start: usize, end: usize, base: usize) -> Token {
    Token {
        text: sentence[start..end].to_owned(),
        span: Span {
            start: base + start,
            end: base + end,
        },
        tags: Vec::new(),
        lemmas: Vec::new(),
    }
}

/// Segment `text` into sentences and tokenize each, returning all tokens (untagged) in source order.
#[must_use]
pub fn segment_tokenize(segmenter: &Segmenter, text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    for range in segmenter.sentence_ranges(text) {
        tokenize_into(&text[range.clone()], range.start, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surfaces(text: &str) -> Vec<(usize, usize, String)> {
        let mut out = Vec::new();
        tokenize_into(text, 0, &mut out);
        out.into_iter()
            .map(|t| (t.span.start, t.span.end, t.text))
            .collect()
    }

    #[test]
    fn tokenizes_words_and_punctuation_with_spans() {
        let toks = surfaces("Hello, world!");
        assert_eq!(toks, vec![
            (0, 5, "Hello".to_owned()),
            (5, 6, ",".to_owned()),
            (7, 12, "world".to_owned()),
            (12, 13, "!".to_owned()),
        ]);
    }

    #[test]
    fn keeps_contraction_as_one_token() {
        let toks = surfaces("don't");
        assert_eq!(toks, vec![(0, 5, "don't".to_owned())]);
    }

    #[test]
    fn spans_are_absolute_with_base_offset() {
        let mut out = Vec::new();
        tokenize_into("cat.", 10, &mut out);
        assert_eq!(out[0].span, Span { start: 10, end: 13 });
        assert_eq!(out[1].span, Span { start: 13, end: 14 });
    }

    #[test]
    fn handles_unicode_spans() {
        // "café." — é is 2 bytes, so the word span is 0..5 and the period 5..6.
        let toks = surfaces("café.");
        assert_eq!(toks, vec![
            (0, 5, "café".to_owned()),
            (5, 6, ".".to_owned()),
        ]);
    }

    #[test]
    fn segmenter_splits_with_real_lt_srx() {
        // Exercise the actual LanguageTool segment.srx (fetched to resources/); skip if absent.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/segment.srx");
        let Ok(xml) = std::fs::read_to_string(path) else {
            eprintln!("skip: {path} not fetched");
            return;
        };
        let seg = Segmenter::from_srx(&xml, "en").expect("parse segment.srx");
        let text = "He went home. She stayed.";
        let sents: Vec<String> = seg
            .sentence_ranges(text)
            .iter()
            .map(|r| text[r.clone()].to_owned())
            .collect();
        assert_eq!(sents.len(), 2, "{sents:?}");
        assert!(sents[0].contains("home") && sents[1].contains("stayed"), "{sents:?}");
        // Spans must tile the text exactly.
        assert_eq!(sents.concat(), text);
    }
}
