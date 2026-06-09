//! L3 — statistical confusion-pair disambiguation (real-word errors).
//!
//! For each token that is a member of a confusion pair (their/there, affect/effect, …), this
//! compares how well the token vs. its alternative fits the local context, using bigram counts
//! pruned to confusion words (Norvig's Google-corpus subset). When the alternative is sufficiently
//! more probable in context it is suggested. This catches errors L1 (valid word) and L2 (no rule)
//! miss.
//!
//! v1 uses a context-bigram ratio test rather than LanguageTool's full probabilistic n-gram model,
//! so the per-pair `factor` thresholds (calibrated for that model) are approximated by a global
//! ratio. It fires only when bigram evidence clearly favours the alternative — favouring precision.

use std::collections::HashMap;

use rlt_ir::ConfusionModel;

use crate::{Analysis, Diagnostic, GrammarChecker, Source, Suggestion};

/// Floor / ceiling on how many times more probable (in context) the alternative must be. The
/// actual ratio is scaled from LT's per-pair `factor` between these bounds: conservative pairs
/// (high factor, error-prone like the/to) demand much stronger evidence.
const MIN_RATIO: f64 = 10.0;
const MAX_RATIO: f64 = 1000.0;
/// Divisor mapping LT's probabilistic `factor` onto a raw-count ratio threshold.
const FACTOR_SCALE: f64 = 1_000_000.0;
/// Minimum bigram evidence for the alternative, to avoid firing on sparse/noisy contexts.
const MIN_EVIDENCE: u32 = 5000;

/// L3 confusion-pair checker, compiled from a [`ConfusionModel`].
pub struct ConfusionChecker {
    /// Confusion word (lower-cased) → the alternatives to test (with each pair's factor) when it
    /// occurs.
    alternatives: HashMap<String, Vec<(String, f32)>>,
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
            bigrams: model.bigrams.iter().cloned().collect(),
        }
    }

    /// An empty checker that produces no diagnostics — used when no confusion model is available,
    /// so the cascade can always wrap with L3 without branching.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            alternatives: HashMap::new(),
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

    /// Context-fit score for `mid` given its lower-cased neighbours: the bigram counts joining it
    /// to each *alphabetic* neighbour (punctuation neighbours are skipped — their bigrams are noisy
    /// and dilute the signal). 0 when a context bigram is absent.
    fn score(&self, left: Option<&str>, mid: &str, right: Option<&str>) -> u32 {
        let mut s = 0u32;
        if let Some(l) = left.filter(|w| is_word(w)) {
            s = s.saturating_add(*self.bigrams.get(&format!("{l} {mid}")).unwrap_or(&0));
        }
        if let Some(r) = right.filter(|w| is_word(w)) {
            s = s.saturating_add(*self.bigrams.get(&format!("{mid} {r}")).unwrap_or(&0));
        }
        s
    }
}

/// Whether a token is a plain alphabetic word (usable as confusion context).
fn is_word(w: &str) -> bool {
    !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Map LT's per-pair `factor` to a raw-count ratio threshold, clamped to `[MIN_RATIO, MAX_RATIO]`.
fn required_ratio(factor: f32) -> f64 {
    (f64::from(factor) / FACTOR_SCALE).clamp(MIN_RATIO, MAX_RATIO)
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
            // Only consider purely-alphabetic tokens, and skip ones that are the head of a
            // contraction (e.g. the "they" of "they're", split off by the tokenizer).
            if !is_word(&word) || tokens.get(i + 1).is_some_and(|t| t.text.starts_with('\'')) {
                continue;
            }
            let left = i
                .checked_sub(1)
                .map(|j| tokens[j].text.to_ascii_lowercase());
            let right = tokens.get(i + 1).map(|t| t.text.to_ascii_lowercase());
            let left = left.as_deref();
            let right = right.as_deref();

            let s_word = self.score(left, &word, right);
            // Suggest the best-scoring alternative that beats the original by the pair's
            // factor-scaled ratio in context.
            let best = alts
                .iter()
                .map(|(alt, factor)| (alt, self.score(left, alt, right), required_ratio(*factor)))
                .filter(|&(_, s, ratio)| {
                    s >= MIN_EVIDENCE && f64::from(s) > f64::from(s_word) * ratio
                })
                .max_by_key(|&(_, s, _)| s);

            if let Some((alt, _, _)) = best {
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
            unigrams: vec![("their".to_owned(), 100), ("there".to_owned(), 100)],
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
}
