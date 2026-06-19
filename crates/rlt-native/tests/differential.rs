//! P1 gate: the native engine, running on a tagger dictionary **dumped from nlprule's own lexicon**,
//! must reproduce nlprule's tokenization + tagging over LanguageTool's example corpus.
//!
//! Bootstrapping the dictionary from nlprule (rather than current-LT data) is deliberate: it isolates
//! *engine-code* correctness (segmentation, tokenization, fst round-trip, lookup + case fallback) from
//! *data* differences. Swapping in current-LT data is P2; disambiguation — which narrows nlprule's
//! tag sets and explains the residual tag gap here — is P3.
//!
//! Skips (does not fail) when the nlprule binary or the LT grammar checkout is absent, so the suite
//! stays green on a fresh clone. Run the full gate with `cargo xtask fetch-engine && cargo xtask
//! fetch-lt` in place.

// Ratios of token counts (< 2^24) to f64 — precision-safe; the lint is noise here.
#![allow(clippy::cast_precision_loss)]

use std::collections::BTreeMap;
use std::path::Path;

use rlt_convert::{DEFAULT_GRAMMAR, extract_examples};
use rlt_core::{Analysis, Engine};
use rlt_engine::{DEFAULT_TOKENIZER_BIN, VendoredEngine};
use rlt_native::{NativeEngine, Segmenter, Tagger, WordData, build_artifact, segment_tokenize};

const SEGMENT_SRX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/segment.srx");

/// Resolve a workspace-root-relative path (the `rlt_engine`/`rlt_convert` defaults are relative to the
/// workspace root, but a `cargo test -p` CWD is the crate directory).
fn root(rel: &str) -> std::path::PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join(rel)
}

/// Load the three artifacts the gate needs, or `None` (→ skip) if any is missing.
fn fixtures() -> Option<(VendoredEngine, Segmenter, Vec<String>)> {
    let srx = match std::fs::read_to_string(SEGMENT_SRX) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skip: segment.srx: {e}");
            return None;
        }
    };
    let nlprule = match VendoredEngine::from_path(&root(DEFAULT_TOKENIZER_BIN)) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skip: en_tokenizer.bin: {e}");
            return None;
        }
    };
    let texts: Vec<String> = match extract_examples(&root(DEFAULT_GRAMMAR)) {
        Ok(ex) => ex.into_iter().map(|e| e.text).collect(),
        Err(e) => {
            eprintln!("skip: grammar.xml: {e}");
            return None;
        }
    };
    (!texts.is_empty()).then_some((nlprule, Segmenter::from_srx(&srx, "en").unwrap(), texts))
}

/// Bootstrap a tagger by dumping nlprule's raw analyses for every surface the native tokenizer emits
/// over `texts`. Keys are lower-cased (the native lexicon is case-folded; the engine's exact-then-
/// lower-case lookup resolves sentence-initial caps), so the dump is keyed off nlprule's lookup of the
/// original surface — capturing exactly the analyses the differential then checks for.
fn bootstrap_tagger(nlprule: &VendoredEngine, segmenter: &Segmenter, texts: &[String]) -> Tagger {
    let mut words: BTreeMap<String, Vec<WordData>> = BTreeMap::new();
    for text in texts {
        for token in segment_tokenize(segmenter, text) {
            let key = token.text.to_lowercase();
            if key.is_empty() || words.contains_key(&key) {
                continue;
            }
            let data: Vec<WordData> = nlprule
                .word_data(&token.text)
                .into_iter()
                .map(|(lemma, tag)| WordData { lemma, tag })
                .collect();
            if !data.is_empty() {
                words.insert(key, data);
            }
        }
    }
    Tagger::from_bytes(&build_artifact(&words).unwrap()).unwrap()
}

/// Tags nlprule supplies from its tokenizer heuristics + XML disambiguation rather than morphological
/// lexicon lookup: sentence/paragraph boundaries, the punctuation class, the OOV marker, and the
/// capitalization/digit-driven retags. The native engine grows these in P3 (disambiguation); excluding
/// them isolates the P1 question — does *lexical* tag lookup match nlprule exactly?
const STRUCTURAL_TAGS: &[&str] = &[
    "PCT",
    "SENT_START",
    "SENT_END",
    "PARA_END",
    "RB_SENT",
    "UNKNOWN",
    "NNP",
    "NNPS",
    "CD",
    "ORD",
];

fn is_structural(tag: &str) -> bool {
    STRUCTURAL_TAGS.contains(&tag)
}

/// Per-token agreement of `native` against the nlprule `reference` analysis of the same text.
#[derive(Default)]
struct Tally {
    ref_tokens: usize,
    span_hits: usize,
    tag_hits: usize, // all reference tags contained (informational; structural tags drag this down)
    lex_hits: usize, // all *non-structural* reference tags contained (the P1 invariant)
    missing: BTreeMap<String, usize>, // non-structural tags nlprule had but native lacked
}

fn compare(reference: &Analysis, native: &Analysis, t: &mut Tally) {
    for r in &reference.tokens {
        t.ref_tokens += 1;
        let Some(n) = native
            .tokens
            .iter()
            .find(|n| n.span == r.span && n.text == r.text)
        else {
            continue; // tokenization mismatch (contraction/hyphen/abbreviation split)
        };
        t.span_hits += 1;
        if r.tags.iter().all(|tag| n.tags.contains(tag)) {
            t.tag_hits += 1;
        }
        // The P1 invariant: nlprule's post-disambiguation tags are a *superset only by structural
        // tags*; every lexical (morphological) tag it assigns must be in the native raw lexicon set.
        let lexical_missing: Vec<&String> = r
            .tags
            .iter()
            .filter(|tag| !is_structural(tag) && !n.tags.contains(*tag))
            .collect();
        if lexical_missing.is_empty() {
            t.lex_hits += 1;
        } else {
            for tag in lexical_missing {
                *t.missing.entry(tag.clone()).or_default() += 1;
            }
        }
    }
}

#[test]
fn native_engine_reproduces_nlprule_over_example_corpus() {
    let Some((nlprule, segmenter, texts)) = fixtures() else {
        eprintln!(
            "skip: native differential needs resources/segment.srx + en_tokenizer.bin + grammar.xml"
        );
        return;
    };
    let tagger = bootstrap_tagger(&nlprule, &segmenter, &texts);
    let native = NativeEngine::new(segmenter, tagger);

    let mut t = Tally::default();
    for text in &texts {
        compare(&nlprule.analyze(text), &native.analyze(text), &mut t);
    }

    let span = t.span_hits as f64 / t.ref_tokens as f64;
    let tag = t.tag_hits as f64 / t.span_hits as f64;
    let lex = t.lex_hits as f64 / t.span_hits as f64;
    eprintln!(
        "native vs nlprule over {} examples: tokenization {:.1}% ({}/{}), lexical-tag {:.2}% ({}/{}), \
         all-tag {:.1}% (structural tags = P3)",
        texts.len(),
        span * 100.0,
        t.span_hits,
        t.ref_tokens,
        lex * 100.0,
        t.lex_hits,
        t.span_hits,
        tag * 100.0,
    );

    if !t.missing.is_empty() {
        let mut top: Vec<(&String, &usize)> = t.missing.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        eprintln!(
            "residual lexical-tag misses: {:?}",
            &top[..top.len().min(15)]
        );
    }

    // Tokenization parity is the engine-code gate; the residual is contraction/abbreviation splitting
    // (a documented later refinement). Lexical-tag containment near-perfect confirms the fst dictionary
    // round-trips and lookup + case-fallback reproduce nlprule's morphological lexicon exactly on every
    // shared token — the structural tags (PCT/SENT_END/NNP/CD/…) are P3 disambiguation, tracked apart.
    assert!(span >= 0.94, "tokenization agreement {span:.3} < 0.94");
    assert!(lex >= 0.99, "lexical-tag containment {lex:.3} < 0.99");
}
