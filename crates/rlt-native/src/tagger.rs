//! The POS tagger: an FST mapping each (lower-cased) surface form to its morphological analyses.
//!
//! Artifact layout: an `fst::Map<word → entry index>` plus an rkyv side-table of analyses (the fst
//! can only store a `u64` value, so it stores an index into the table). A word's analyses are the
//! set of `(lemma, tag)` pairs LanguageTool's dictionary records for it — e.g. `left → {(leave,VBD),
//! (leave,VBN), (left,JJ), (left,NN)}`. The same artifact backs `is_known` (membership) and tagging.
//!
//! The offline build (`build_artifact`) is fed by the P1 bootstrap (nlprule's tags) and, later, the
//! AGID/Moby dictionary build.

use std::collections::BTreeMap;

use fst::{Map, MapBuilder};
use rkyv::{Archive, Deserialize, Serialize};

use rlt_core::Token;

/// One morphological analysis of a word: its base form and POS tag (LT tagset).
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
pub struct WordData {
    /// Base form.
    pub lemma: String,
    /// POS tag (LT/Penn tagset, e.g. `VBD`, `NN`, `PRP$`).
    pub tag: String,
}

/// The serialized tagger artifact: the fst bytes + the per-word analysis table.
#[derive(Debug, Archive, Serialize, Deserialize)]
struct TaggerData {
    /// Serialized `fst::Map` bytes: lower-cased surface → index into `entries`.
    fst_bytes: Vec<u8>,
    /// `entries[i]` = the analyses for the word whose fst value is `i`.
    entries: Vec<Vec<WordData>>,
}

/// Errors loading or building the tagger.
#[derive(Debug, thiserror::Error)]
pub enum TaggerError {
    /// The rkyv side-table could not be deserialized.
    #[error("tagger artifact: {0}")]
    Rkyv(String),
    /// The fst bytes are not a valid finite-state map.
    #[error("tagger fst: {0}")]
    Fst(String),
}

/// A loaded POS tagger.
pub struct Tagger {
    fst: Map<Vec<u8>>,
    entries: Vec<Vec<WordData>>,
}

impl Tagger {
    /// Load from the rkyv artifact bytes (native + wasm).
    ///
    /// # Errors
    /// Returns [`TaggerError`] if the artifact or its embedded fst is malformed.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TaggerError> {
        let data = rkyv::from_bytes::<TaggerData, rkyv::rancor::Error>(bytes)
            .map_err(|e| TaggerError::Rkyv(e.to_string()))?;
        let fst = Map::new(data.fst_bytes).map_err(|e| TaggerError::Fst(e.to_string()))?;
        Ok(Self {
            fst,
            entries: data.entries,
        })
    }

    /// The analyses for `word` — exact match first, then a lower-cased fallback (so sentence-initial
    /// capitalization still resolves). `None` if the word is unknown.
    #[must_use]
    pub fn lookup(&self, word: &str) -> Option<&[WordData]> {
        let idx = self
            .fst
            .get(word.as_bytes())
            .or_else(|| self.fst.get(word.to_lowercase().as_bytes()))?;
        self.entries.get(usize::try_from(idx).ok()?).map(Vec::as_slice)
    }

    /// Whether `word` is in the lexicon (the L1 spelling membership oracle).
    #[must_use]
    pub fn is_known(&self, word: &str) -> bool {
        self.lookup(word).is_some()
    }

    /// Fill `token.tags` and `token.lemmas` from its analyses (deduplicated, order-preserving —
    /// matching the vendored engine's behaviour).
    pub fn tag_token(&self, token: &mut Token) {
        if let Some(analyses) = self.lookup(&token.text) {
            for wd in analyses {
                push_unique(&mut token.tags, &wd.tag);
                push_unique(&mut token.lemmas, &wd.lemma);
            }
        }
    }
}

/// Append `value` to `out` iff non-empty and not already present (order-preserving unique).
fn push_unique(out: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !out.iter().any(|v| v == value) {
        out.push(value.to_owned());
    }
}

/// Build the tagger artifact bytes from a word → analyses map (offline). Keys are lower-cased; the
/// `BTreeMap` gives the sorted order the fst requires.
///
/// # Errors
/// Returns [`TaggerError`] if the fst cannot be built or the table cannot be serialized.
pub fn build_artifact(words: &BTreeMap<String, Vec<WordData>>) -> Result<Vec<u8>, TaggerError> {
    let mut builder = MapBuilder::memory();
    let mut entries: Vec<Vec<WordData>> = Vec::with_capacity(words.len());
    for (word, analyses) in words {
        builder
            .insert(word.as_bytes(), entries.len() as u64)
            .map_err(|e| TaggerError::Fst(e.to_string()))?;
        entries.push(analyses.clone());
    }
    let fst_bytes = builder
        .into_inner()
        .map_err(|e| TaggerError::Fst(e.to_string()))?;
    let data = TaggerData { fst_bytes, entries };
    rkyv::to_bytes::<rkyv::rancor::Error>(&data)
        .map(|b| b.to_vec())
        .map_err(|e| TaggerError::Rkyv(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wd(lemma: &str, tag: &str) -> WordData {
        WordData {
            lemma: lemma.to_owned(),
            tag: tag.to_owned(),
        }
    }

    fn fixture() -> Tagger {
        // Keys lower-cased + sorted (BTreeMap handles the sort).
        let mut words = BTreeMap::new();
        words.insert("cat".to_owned(), vec![wd("cat", "NN")]);
        words.insert(
            "left".to_owned(),
            vec![wd("leave", "VBD"), wd("leave", "VBN"), wd("left", "JJ")],
        );
        let bytes = build_artifact(&words).expect("build");
        Tagger::from_bytes(&bytes).expect("load")
    }

    #[test]
    fn looks_up_multiple_analyses() {
        let t = fixture();
        let left = t.lookup("left").expect("known");
        assert_eq!(left.len(), 3);
        assert_eq!(left[0].lemma, "leave");
    }

    #[test]
    fn lowercase_fallback_for_sentence_initial_caps() {
        assert!(fixture().is_known("Cat"));
        assert!(!fixture().is_known("Catt"));
    }

    #[test]
    fn tags_token_with_deduped_tags_and_lemmas() {
        let mut tok = Token {
            text: "left".to_owned(),
            ..Default::default()
        };
        fixture().tag_token(&mut tok);
        // leave appears twice (VBD, VBN) -> one lemma; left -> the JJ analysis.
        assert_eq!(tok.tags, vec!["VBD", "VBN", "JJ"]);
        assert_eq!(tok.lemmas, vec!["leave", "left"]);
    }
}
