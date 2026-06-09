//! L3 — statistical confusion-pair disambiguation (real-word errors).
//!
//! For each token that is a member of a confusion pair (their/there, affect/effect, …), this
//! compares the contextual probability of the token vs. its alternative under a bigram language
//! model (Norvig's Google-corpus n-gram subset, pruned to confusion words). When the alternative
//! is sufficiently more probable it is suggested — catching real-word errors L1 (the word is valid)
//! and L2 (no rule fires) miss.
//!
//! The decision is a bigram log-likelihood ratio `log P(alt | context) − log P(word | context)`
//! with add-one smoothing, compared against a threshold derived from LanguageTool's per-pair
//! `factor`. LT's factors are calibrated for its richer (trigram, full-corpus) model, so they do
//! not transfer literally; we map them *log-relatively* — aggressive pairs (low factor) need only
//! a modest ratio, conservative ones (high factor, e.g. the/to) need a large one — preserving their
//! relative aggressiveness while staying in the regime bigram counts can express.

use std::collections::HashMap;

use rlt_ir::ConfusionModel;

use crate::{Analysis, Diagnostic, GrammarChecker, Source, Suggestion};

/// LanguageTool `factor` exponents (`log10`) span ~1e3..1e12; clamp to this for the threshold map.
const LF_MIN: f64 = 3.0;
const LF_MAX: f64 = 12.0;
/// Corresponding log-likelihood-ratio thresholds the bigram model can plausibly reach: aggressive
/// pairs need the alternative ~3x more probable (`ln 3`), conservative ones ~100x (`ln 100`).
const LOGR_MIN: f64 = 1.099;
const LOGR_MAX: f64 = 4.605;
/// Minimum bigram evidence (summed alt-context counts) to fire — never decide on smoothing alone.
const MIN_EVIDENCE: u32 = 5000;
/// Add-one (Laplace) smoothing for bigram and unigram counts.
const SMOOTH: f64 = 1.0;

/// L3 confusion-pair checker, compiled from a [`ConfusionModel`].
pub struct ConfusionChecker {
    /// Confusion word (lower-cased) → alternatives to test (with each pair's factor) when it occurs.
    alternatives: HashMap<String, Vec<(String, f32)>>,
    unigrams: HashMap<String, u32>,
    bigrams: HashMap<String, u32>,
}

impl ConfusionChecker {
    /// Build a checker from a confusion model.
    #[must_use]
    pub fn new(model: &ConfusionModel) -> Self {
        let mut alternatives: HashMap<String, Vec<(String, f32)>> = HashMap::new();
        for p in &model.pairs {
            // `a -> b` (and the `a` side of symmetric pairs) suggests `b` when `a` occurs.
            alternatives
                .entry(p.a.clone())
                .or_default()
                .push((p.b.clone(), p.factor));
            if p.symmetric {
                alternatives
                    .entry(p.b.clone())
                    .or_default()
                    .push((p.a.clone(), p.factor));
            }
        }
        Self {
            alternatives,
            unigrams: model.unigrams.iter().cloned().collect(),
            bigrams: model.bigrams.iter().cloned().collect(),
        }
    }

    /// An empty checker that produces no diagnostics — used when no confusion model is available,
    /// so the cascade can always wrap with L3 without branching.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            alternatives: HashMap::new(),
            unigrams: HashMap::new(),
            bigrams: HashMap::new(),
        }
    }

    /// Build a checker from the rkyv confusion-model artifact.
    ///
    /// # Errors
    /// Returns an error if `bytes` is not a valid archived [`ConfusionModel`].
    pub fn from_rkyv_bytes(bytes: &[u8]) -> Result<Self, rkyv::rancor::Error> {
        Ok(Self::new(&rlt_ir::deserialize_confusion(bytes)?))
    }

    fn bigram(&self, a: &str, b: &str) -> u32 {
        *self.bigrams.get(&format!("{a} {b}")).unwrap_or(&0)
    }

    fn unigram_smoothed(&self, w: &str) -> f64 {
        f64::from(*self.unigrams.get(w).unwrap_or(&0)) + SMOOTH
    }

    /// `log P(alt | context) − log P(word | context)` under a bigram model with add-one smoothing.
    /// On the left side `count(left)` cancels; on the right side the conditionals normalise by the
    /// candidates' unigram counts (so a generally-common word does not win on raw frequency).
    fn log_ratio(&self, left: Option<&str>, word: &str, right: Option<&str>, alt: &str) -> f64 {
        let mut lr = 0.0;
        if let Some(l) = left {
            lr += (f64::from(self.bigram(l, alt)) + SMOOTH).ln()
                - (f64::from(self.bigram(l, word)) + SMOOTH).ln();
        }
        if let Some(r) = right {
            lr += ((f64::from(self.bigram(alt, r)) + SMOOTH) / self.unigram_smoothed(alt)).ln()
                - ((f64::from(self.bigram(word, r)) + SMOOTH) / self.unigram_smoothed(word)).ln();
        }
        lr
    }

    /// Raw bigram count of the alternative actually appearing in this context.
    fn evidence(&self, left: Option<&str>, alt: &str, right: Option<&str>) -> u32 {
        left.map_or(0, |l| self.bigram(l, alt))
            .saturating_add(right.map_or(0, |r| self.bigram(alt, r)))
    }
}

impl GrammarChecker for ConfusionChecker {
    fn grammar_diagnostics(&self, _text: &str, analysis: &Analysis) -> Vec<Diagnostic> {
        let tokens = &analysis.tokens;
        let mut out = Vec::new();
        for i in 0..tokens.len() {
            let word = tokens[i].text.to_ascii_lowercase();
            let Some(alts) = self.alternatives.get(&word) else {
                continue;
            };
            // Only plain words, and skip contraction heads (the "they" of a "they"+"'re" split).
            if !is_word(&word) || tokens.get(i + 1).is_some_and(|t| t.text.starts_with('\'')) {
                continue;
            }
            // Word-only neighbours (punctuation contributes no usable context).
            let left = i
                .checked_sub(1)
                .map(|j| tokens[j].text.to_ascii_lowercase())
                .filter(|w| is_word(w));
            let right = tokens
                .get(i + 1)
                .map(|t| t.text.to_ascii_lowercase())
                .filter(|w| is_word(w));
            let (left, right) = (left.as_deref(), right.as_deref());

            let best = alts
                .iter()
                .map(|(alt, factor)| {
                    (
                        alt,
                        self.log_ratio(left, &word, right, alt),
                        self.evidence(left, alt, right),
                        log_threshold(*factor),
                    )
                })
                .filter(|&(_, lr, ev, thr)| ev >= MIN_EVIDENCE && lr >= thr)
                .max_by(|a, b| a.1.total_cmp(&b.1));

            if let Some((alt, _, _, _)) = best {
                out.push(Diagnostic {
                    span: tokens[i].span,
                    code: "CONFUSION".to_owned(),
                    message: format!("Did you mean “{alt}” instead of “{}”?", tokens[i].text),
                    suggestions: vec![Suggestion {
                        replacement: recase(&tokens[i].text, alt),
                    }],
                    source: Source::Statistical,
                });
            }
        }
        out
    }
}

/// Whether a token is a plain alphabetic word (usable as confusion context).
fn is_word(w: &str) -> bool {
    !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Threshold on the log-likelihood ratio, mapped log-relatively from LT's `factor`.
fn log_threshold(factor: f32) -> f64 {
    let lf = f64::from(factor).log10().clamp(LF_MIN, LF_MAX);
    let t = (lf - LF_MIN) / (LF_MAX - LF_MIN);
    LOGR_MIN + t * (LOGR_MAX - LOGR_MIN)
}

/// Apply `source`'s leading capitalization to `candidate`.
fn recase(source: &str, candidate: &str) -> String {
    if source.chars().next().is_some_and(char::is_uppercase) {
        let mut c = candidate.chars();
        c.next().map_or_else(String::new, |first| {
            first.to_uppercase().collect::<String>() + c.as_str()
        })
    } else {
        candidate.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use rlt_ir::ConfusionPair;

    use super::*;
    use crate::{Span, Token};

    fn model() -> ConfusionModel {
        ConfusionModel {
            pairs: vec![ConfusionPair {
                a: "their".to_owned(),
                b: "there".to_owned(),
                factor: 10.0,
                symmetric: true,
            }],
            unigrams: vec![
                ("their".to_owned(), 5_000_000),
                ("there".to_owned(), 5_000_000),
            ],
            // "over there" is common; "over their" is not — context favours "there".
            bigrams: vec![
                ("over there".to_owned(), 50000),
                ("their car".to_owned(), 40000),
            ],
        }
    }

    fn analysis(words: &[&str]) -> Analysis {
        let mut tokens = Vec::new();
        let mut pos = 0;
        for w in words {
            tokens.push(Token {
                text: (*w).to_owned(),
                span: Span {
                    start: pos,
                    end: pos + w.len(),
                },
                ..Default::default()
            });
            pos += w.len() + 1;
        }
        Analysis { tokens }
    }

    #[test]
    fn flags_real_word_error_from_context() {
        let checker = ConfusionChecker::new(&model());
        // "over their" → "over there" (bigram "over there" dominates).
        let diags = checker.grammar_diagnostics("", &analysis(&["over", "their"]));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "there");
        assert_eq!(diags[0].source, Source::Statistical);
    }

    #[test]
    fn leaves_contextually_correct_word() {
        let checker = ConfusionChecker::new(&model());
        // "their car" is attested; no suggestion.
        assert!(
            checker
                .grammar_diagnostics("", &analysis(&["their", "car"]))
                .is_empty()
        );
    }

    #[test]
    fn no_evidence_no_flag() {
        let checker = ConfusionChecker::new(&model());
        // Neither "blorp their" nor "blorp there" is attested → no decision.
        assert!(
            checker
                .grammar_diagnostics("", &analysis(&["blorp", "their"]))
                .is_empty()
        );
    }
}
