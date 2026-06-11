//! The neural [`TagSource`]: runs the int8 GECToR ONNX graph in `rten` and reduces its per-subword
//! logits to the per-word [`WordPred`]s the decoder consumes.
//!
//! Contract (verified against the exported artifact): the words are encoded as one **pre-tokenized**
//! sequence with `"$START"` prepended; the model emits per-subword `logits_labels` `[1, seq, 5001]`
//! and `logits_d` `[1, seq, 2]`. Each word's prediction is read at its **first subword position**
//! (via the tokenizer's `word_ids`). `rten` has no i64 tensor input, so ids are passed as i32.

use std::path::Path;

use rten::Model;
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};
use serde::Deserialize;
use tokenizers::Tokenizer;

use crate::{Labels, TagSource, Tagger, TaggerConfig, VerbDict, WordPred, WordSpan};

/// The indices the Rust side must agree with the exported graph on (subset of `meta.json`).
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    /// Label-logit index of `$KEEP`.
    pub keep_label_index: usize,
    /// Detect-head logit index of `$INCORRECT`.
    pub detect_incorrect_index: usize,
    /// The virtual start-of-sentence word prepended before encoding.
    #[serde(default = "default_start_token")]
    pub start_token: String,
}

fn default_start_token() -> String {
    "$START".to_owned()
}

/// Errors loading the L4 artifact tuple or running inference.
#[derive(Debug, thiserror::Error)]
pub enum TaggerError {
    /// An artifact file could not be read.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// `labels.json` / `meta.json` could not be parsed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The model/tokenizer backend failed (load, tokenize, or inference).
    #[error("model backend: {0}")]
    Backend(String),
}

/// Wrap any backend (rten / tokenizers) error as a [`TaggerError::Backend`].
fn backend<E: std::fmt::Debug>(e: E) -> TaggerError {
    TaggerError::Backend(format!("{e:?}"))
}

/// A keep-everything fallback prediction (used when a word has no subword or inference fails).
const KEEP: WordPred = WordPred {
    edit_label: 0,
    edit_prob: 0.0,
    keep_prob: 1.0,
    error_prob: 0.0,
};

/// An `rten`-backed GECToR tag predictor.
pub struct RtenTagSource {
    model: Model,
    tokenizer: Tokenizer,
    meta: Meta,
}

impl RtenTagSource {
    /// Assemble from a loaded model, tokenizer, and the agreed indices.
    #[must_use]
    pub fn new(model: Model, tokenizer: Tokenizer, meta: Meta) -> Self {
        Self {
            model,
            tokenizer,
            meta,
        }
    }

    /// Run the graph and reduce to per-word predictions; `Err` on any backend failure.
    fn infer(&self, words: &[WordSpan], text: &str) -> Result<Vec<WordPred>, TaggerError> {
        // "$START" + the surface words, as one pre-tokenized sequence.
        let mut input: Vec<&str> = Vec::with_capacity(words.len() + 1);
        input.push(self.meta.start_token.as_str());
        input.extend(words.iter().map(|w| &text[w.start..w.end]));

        let enc = self.tokenizer.encode(input, true).map_err(backend)?;
        // Token ids fit in i32 (vocab ~50k); rten has no i64 tensor input.
        let ids: Vec<i32> = enc
            .get_ids()
            .iter()
            .map(|&id| i32::try_from(id).unwrap_or(0))
            .collect();
        let n = ids.len();
        let word_ids = enc.get_word_ids();

        let input_ids = Tensor::from_data(&[1, n], ids);
        let attention_mask = Tensor::<i32>::full(&[1, n], 1);
        let inputs = vec![
            (
                self.model.node_id("input_ids").map_err(backend)?,
                input_ids.view().into(),
            ),
            (
                self.model.node_id("attention_mask").map_err(backend)?,
                attention_mask.view().into(),
            ),
        ];
        let [labels, dtags] = self
            .model
            .run_n(
                inputs,
                [
                    self.model.node_id("logits_labels").map_err(backend)?,
                    self.model.node_id("logits_d").map_err(backend)?,
                ],
                None,
            )
            .map_err(backend)?;
        let labels: NdTensor<f32, 3> = labels.try_into().map_err(backend)?;
        let dtags: NdTensor<f32, 3> = dtags.try_into().map_err(backend)?;

        let mut preds = Vec::with_capacity(words.len());
        for i in 0..words.len() {
            // Model word index = i + 1 ($START is word 0); read its first subword position.
            let model_word = u32::try_from(i + 1).unwrap_or(u32::MAX);
            match word_ids.iter().position(|w| *w == Some(model_word)) {
                Some(p) => preds.push(reduce_word(&labels, &dtags, p, &self.meta)),
                None => preds.push(KEEP),
            }
        }
        Ok(preds)
    }
}

impl TagSource for RtenTagSource {
    fn predict(&self, words: &[WordSpan], text: &str) -> Vec<WordPred> {
        // Fail closed: an inference error yields no L4 suggestions rather than crashing the cascade.
        self.infer(words, text)
            .unwrap_or_else(|_| vec![KEEP; words.len()])
    }
}

/// Reduce the label + detect logit rows at subword position `p` to a [`WordPred`] (softmax, keep
/// probability, best non-keep edit, detect error probability). Element-indexed to avoid depending on
/// tensor-slice surface details.
fn reduce_word(
    labels: &NdTensor<f32, 3>,
    dtags: &NdTensor<f32, 3>,
    p: usize,
    meta: &Meta,
) -> WordPred {
    let n_labels = labels.shape()[2];
    let max = (0..n_labels).fold(f32::NEG_INFINITY, |m, c| m.max(labels[[0, p, c]]));
    let mut sum = 0.0f32;
    let mut keep_exp = 0.0f32;
    let (mut best_label, mut best_exp) = (0usize, -1.0f32);
    for c in 0..n_labels {
        let e = (labels[[0, p, c]] - max).exp();
        sum += e;
        if c == meta.keep_label_index {
            keep_exp = e;
        } else if e > best_exp {
            best_exp = e;
            best_label = c;
        }
    }
    let (keep_prob, edit_prob) = if sum > 0.0 {
        (keep_exp / sum, best_exp.max(0.0) / sum)
    } else {
        (0.0, 0.0)
    };

    let n_d = dtags.shape()[2];
    let dmax = (0..n_d).fold(f32::NEG_INFINITY, |m, c| m.max(dtags[[0, p, c]]));
    let mut dsum = 0.0f32;
    let mut incorrect = 0.0f32;
    for c in 0..n_d {
        let e = (dtags[[0, p, c]] - dmax).exp();
        dsum += e;
        if c == meta.detect_incorrect_index {
            incorrect = e;
        }
    }
    let error_prob = if dsum > 0.0 { incorrect / dsum } else { 0.0 };

    WordPred {
        edit_label: best_label,
        edit_prob,
        keep_prob,
        error_prob,
    }
}

impl Tagger<RtenTagSource> {
    /// Load the L4 artifact tuple from in-memory bytes — the wasm path (no filesystem). `verb_dict`
    /// may be empty (then `$TRANSFORM_VERB_*` tags are skipped).
    ///
    /// # Errors
    /// Returns [`TaggerError`] if the model/tokenizer is invalid or `labels`/`meta` won't parse.
    pub fn from_bytes(
        model: Vec<u8>,
        tokenizer_json: &[u8],
        labels_json: &[u8],
        meta_json: &[u8],
        verb_dict: &[u8],
    ) -> Result<Self, TaggerError> {
        let model = Model::load(model).map_err(backend)?;
        let tokenizer = Tokenizer::from_bytes(tokenizer_json).map_err(backend)?;
        let labels: Vec<String> = serde_json::from_slice(labels_json)?;
        let meta: Meta = serde_json::from_slice(meta_json)?;
        let verb_dict = VerbDict::parse(&String::from_utf8_lossy(verb_dict));
        Ok(Tagger::new(
            RtenTagSource::new(model, tokenizer, meta),
            Labels::new(labels),
            TaggerConfig::default(),
        )
        .with_verb_dict(verb_dict))
    }

    /// Load the L4 artifact tuple from a directory (`model.int8.onnx` / `tokenizer.json` /
    /// `labels.json` / `meta.json` / `verb-form-vocab.txt`) — the native path.
    ///
    /// # Errors
    /// Returns [`TaggerError`] if any required artifact is missing or malformed.
    pub fn from_dir(dir: &Path) -> Result<Self, TaggerError> {
        Self::from_bytes(
            std::fs::read(dir.join("model.int8.onnx"))?,
            &std::fs::read(dir.join("tokenizer.json"))?,
            &std::fs::read(dir.join("labels.json"))?,
            &std::fs::read(dir.join("meta.json"))?,
            &std::fs::read(dir.join("verb-form-vocab.txt")).unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rlt_core::{Analysis, GrammarChecker, Source};

    use super::*;

    fn artifact_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources/en/l4")
    }

    #[test]
    #[ignore = "needs resources/l4 artifact; produce it with the pipeline, run via xtask run-l4-oracle"]
    fn neural_correction_fires_end_to_end() {
        let dir = artifact_dir();
        if !dir.join("model.int8.onnx").exists() {
            eprintln!("skipping: no L4 artifact at {}", dir.display());
            return;
        }
        let tagger = Tagger::from_dir(&dir).expect("load L4 artifact");
        let text = "They was very happy .";
        let diags = tagger.grammar_diagnostics(text, &Analysis::default());
        // Subject-verb agreement "They was" -> "They were" clears the calibrated default threshold.
        assert!(
            diags.iter().any(|d| {
                d.source == Source::Neural
                    && text.get(d.span.start..d.span.end) == Some("was")
                    && d.suggestions.iter().any(|s| s.replacement == "were")
            }),
            "expected was->were neural correction, got {diags:?}"
        );
    }
}
