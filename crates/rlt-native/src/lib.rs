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

mod compound;
mod tagger;

pub use tagger::{Tagger, TaggerError, WordData, build_artifact, build_from_triples};

use std::ops::Range;
use std::path::Path;

use rlt_core::{Analysis, Disambiguator, Engine, Span, Token};
use rlt_lang::{Compounding, LangConfig, TagSet};

/// Errors constructing the engine from its artifacts.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// `segment.srx` could not be parsed.
    #[error("parsing segment.srx: {0}")]
    Srx(String),
    /// The tagger artifact could not be loaded.
    #[error(transparent)]
    Tagger(#[from] TaggerError),
    /// The disambiguation artifact could not be loaded.
    #[error("loading disambiguation rules: {0}")]
    Disambig(String),
    /// An artifact file could not be read.
    #[error("reading artifact: {0}")]
    Io(#[from] std::io::Error),
}

/// The native analysis engine: sentence segmentation + tokenization + FST POS tagging + (optional)
/// disambiguation behind [`rlt_core::Engine`]. A drop-in replacement for the vendored nlprule engine.
pub struct NativeEngine {
    segmenter: Segmenter,
    tagger: Tagger,
    disambiguator: Option<Disambiguator>,
    /// The structural tagset this language's grammar anchors on (Penn vs STTS).
    tagset: &'static TagSet,
    /// Compound-word splitting rules, if the language compounds (German); `None` otherwise.
    compounds: Option<&'static Compounding>,
}

impl NativeEngine {
    /// Assemble from a loaded segmenter + tagger, using the English tagset (no disambiguation). Use
    /// [`with_tagset`](Self::with_tagset) for another language and [`with_disambiguator`](Self::with_disambiguator)
    /// to add disambiguation.
    #[must_use]
    pub fn new(segmenter: Segmenter, tagger: Tagger) -> Self {
        Self {
            segmenter,
            tagger,
            disambiguator: None,
            tagset: &rlt_lang::EN.tagset,
            compounds: None,
        }
    }

    /// Use a specific language's structural tagset.
    #[must_use]
    pub fn with_tagset(mut self, tagset: &'static TagSet) -> Self {
        self.tagset = tagset;
        self
    }

    /// Attach a disambiguation pass (run after tagging, before the result is returned).
    #[must_use]
    pub fn with_disambiguator(mut self, disambiguator: Disambiguator) -> Self {
        self.disambiguator = Some(disambiguator);
        self
    }

    /// The distinct POS tags the lexicon assigns to `word` (deduplicated) — used at build time to
    /// derive the L3 confusion model's POS-context statistics. Empty for unknown words.
    #[must_use]
    pub fn pos_tags(&self, word: &str) -> Vec<String> {
        let mut tags = Vec::new();
        if let Some(analyses) = self.tagger.lookup(word) {
            for wd in analyses {
                push_tag(&mut tags, &wd.tag);
            }
        }
        tags
    }

    /// Load from in-memory bytes — the wasm path. `segment_srx` is the SRX XML; `tagger` is the rkyv
    /// tagger artifact; `cfg` selects the SRX language + the structural tagset. Disambiguation is
    /// attached via [`with_disambiguator`](Self::with_disambiguator).
    ///
    /// # Errors
    /// Returns [`NativeError`] if either artifact is malformed.
    pub fn from_bytes(
        cfg: &'static LangConfig,
        segment_srx: &str,
        tagger: &[u8],
    ) -> Result<Self, NativeError> {
        Ok(Self {
            segmenter: Segmenter::from_srx(segment_srx, cfg.code)?,
            tagger: Tagger::from_bytes(tagger)?,
            disambiguator: None,
            tagset: &cfg.tagset,
            compounds: cfg.compounds.as_ref(),
        })
    }

    /// Load from files on disk — the native path. `disambig` (the `disambig.rkyv` artifact) is
    /// optional; without it the engine emits raw, un-disambiguated lexicon tags.
    ///
    /// # Errors
    /// Returns [`NativeError`] if a file is missing or an artifact is malformed.
    pub fn from_paths(
        cfg: &'static LangConfig,
        segment_srx: &Path,
        tagger: &Path,
        disambig: Option<&Path>,
    ) -> Result<Self, NativeError> {
        let mut engine =
            Self::from_bytes(cfg, &std::fs::read_to_string(segment_srx)?, &std::fs::read(tagger)?)?;
        if let Some(path) = disambig {
            engine.disambiguator = Some(
                Disambiguator::from_rkyv_bytes(&std::fs::read(path)?)
                    .map_err(|e| NativeError::Disambig(e.to_string()))?,
            );
        }
        Ok(engine)
    }
}

impl Engine for NativeEngine {
    fn analyze(&self, text: &str) -> Analysis {
        let mut tokens = Vec::new();
        for range in self.segmenter.sentence_ranges(text) {
            // A zero-width SENT_START sentinel opens each sentence — LanguageTool authors its rules
            // (979 reference SENT_START) assuming this boundary token exists at position 0; without
            // it, every position-anchored pattern mis-aligns.
            let mut sentence = vec![Token {
                text: String::new(),
                span: Span { start: range.start, end: range.start },
                tags: vec![self.tagset.sent_start.to_owned()],
                lemmas: Vec::new(),
            }];
            tokenize_into(&text[range.clone()], range.start, &mut sentence);
            for token in &mut sentence {
                self.tagger.tag_token(token);
                // Out-of-lexicon word + a compounding language → try a compound split, taking the head
                // constituent's analyses (so e.g. German `Haustür` is tagged, not flagged unknown).
                if token.tags.is_empty() {
                    if let Some(rules) = self.compounds {
                        if let Some(analyses) = compound::analyze_compound(&token.text, &self.tagger, rules) {
                            for wd in &analyses {
                                push_tag(&mut token.tags, &wd.tag);
                                push_tag(&mut token.lemmas, &wd.lemma);
                            }
                        }
                    }
                }
                push_structural_tags(token, self.tagset);
            }
            // SENT_END marks the sentence's final token — 331 grammar rules anchor on it.
            if let Some(last) = sentence.last_mut() {
                push_tag(&mut last.tags, self.tagset.sent_end);
            }
            // Disambiguation runs per sentence (LT's rules don't cross boundaries; the sentinels
            // bound them), narrowing/fixing tags before the L2 matcher sees them.
            if let Some(disambiguator) = &self.disambiguator {
                disambiguator.disambiguate(&mut sentence);
            }
            tokens.append(&mut sentence);
        }
        Analysis { tokens }
    }

    fn is_known(&self, word: &str) -> bool {
        self.tagger.is_known(word)
            || self
                .compounds
                .is_some_and(|rules| compound::is_compound(word, &self.tagger, rules))
    }
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

/// Add the tokenizer-level **structural** tags LanguageTool's tagger assigns by token *shape* — the
/// ones grammar rules anchor on heavily but the morphological lexicon doesn't carry. The tag *strings*
/// come from the language's [`TagSet`] (English Penn `CD`/`PCT`/`NNP` vs German STTS `ZAL`/…/`EIG`):
/// - all-digit token → `digit_tag` (a replacement, per `<token regexp>\d+</token>`);
/// - punctuation → `punctuation_tag`, plus the per-character literal class;
/// - an out-of-lexicon word → `proper_noun_tag` if capitalized, else `oov_tag`.
fn push_structural_tags(token: &mut Token, tagset: &TagSet) {
    let text = token.text.as_str();
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        token.tags.clear();
        token.lemmas.clear();
        token.tags.push(tagset.digit_tag.to_owned());
        return;
    }
    if !text.is_empty() && text.chars().all(|c| tagset.punctuation_chars.contains(&c)) {
        push_tag(&mut token.tags, tagset.punctuation_tag);
        if let Some((_, class)) = tagset.punctuation_classes.iter().find(|(ch, _)| *ch == text) {
            push_tag(&mut token.tags, class);
        }
        return;
    }
    if token.tags.is_empty() {
        let capitalized = text.chars().next().is_some_and(char::is_uppercase);
        push_tag(&mut token.tags, if capitalized { tagset.proper_noun_tag } else { tagset.oov_tag });
    }
}

/// Append `tag` to `tags` if not already present (order-preserving unique).
fn push_tag(tags: &mut Vec<String>, tag: &str) {
    if !tags.iter().any(|t| t == tag) {
        tags.push(tag.to_owned());
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

    #[test]
    fn analyze_segments_tokenizes_and_tags() {
        use std::collections::BTreeMap;

        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/segment.srx");
        let Ok(xml) = std::fs::read_to_string(path) else {
            eprintln!("skip: {path} not fetched");
            return;
        };
        let segmenter = Segmenter::from_srx(&xml, "en").expect("parse segment.srx");

        let mut words = BTreeMap::new();
        words.insert("the".to_owned(), vec![WordData { lemma: "the".to_owned(), tag: "DT".to_owned() }]);
        words.insert("cat".to_owned(), vec![WordData { lemma: "cat".to_owned(), tag: "NN".to_owned() }]);
        let tagger = Tagger::from_bytes(&build_artifact(&words).unwrap()).unwrap();

        let engine = NativeEngine::new(segmenter, tagger);
        let analysis = engine.analyze("The cat.");
        let toks: Vec<(&str, Vec<&str>)> = analysis
            .tokens
            .iter()
            .map(|t| (t.text.as_str(), t.tags.iter().map(String::as_str).collect()))
            .collect();
        assert_eq!(toks, vec![
            ("", vec!["SENT_START"]),             // zero-width sentence-start sentinel
            ("The", vec!["DT"]),                  // lower-case fallback resolves the sentence-initial cap
            ("cat", vec!["NN"]),
            (".", vec!["PCT", ".", "SENT_END"]),  // structural: punctuation class + sentence-final
        ]);
        assert!(engine.is_known("The") && !engine.is_known("zzz"));
    }
}
