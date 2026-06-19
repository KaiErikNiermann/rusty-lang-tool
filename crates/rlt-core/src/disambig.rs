//! L3 — statistical confusion-pair disambiguation (real-word errors).
//!
//! For each token that is a member of a confusion pair (their/there, affect/effect, …), this
//! compares the contextual probability of the token vs. its alternative under a bigram language
//! model (Norvig's Google-corpus n-gram subset, pruned to confusion words). When the alternative
//! is sufficiently more probable it is suggested — catching real-word errors L1 (the word is valid)
//! and L2 (no rule fires) miss.
//!
//! The decision combines two bigram log-likelihood ratios `log P(alt | context) − log P(word |
//! context)` with add-one smoothing — one over the neighbours' surface words, one over their
//! context-disambiguated POS (which generalises when the exact word bigram is unseen, e.g.
//! "the/to before a noun"). No fetchable trigram data exists, so the POS view is the available
//! widening of context. The combined ratio is compared against a threshold derived from LT's
//! per-pair `factor`: LT's factors are calibrated for its richer model so they do not transfer
//! literally; we map them *log-relatively* — aggressive pairs (low factor) need only a modest
//! ratio, conservative ones (high factor, e.g. the/to) a large one.

use std::collections::HashMap;

use rlt_ir::ConfusionModel;

use crate::{Analysis, Diagnostic, GrammarChecker, Source, Suggestion, recase};

/// LanguageTool `factor` exponents (`log10`) span ~1e3..1e12; clamp to this for the threshold map.
const LF_MIN: f64 = 3.0;
const LF_MAX: f64 = 12.0;
/// Corresponding log-likelihood-ratio thresholds (word + POS context) the model can plausibly
/// reach: aggressive pairs need the alternative clearly favoured, conservative ones (high factor,
/// e.g. the/to) far more strongly.
const LOGR_MIN: f64 = 2.0;
const LOGR_MAX: f64 = 6.5;
/// Minimum bigram evidence (summed alt-context counts) to fire — never decide on smoothing alone.
const MIN_EVIDENCE: u32 = 5000;
/// Add-one (Laplace) smoothing for bigram and unigram counts.
const SMOOTH: f64 = 1.0;

/// L3 confusion-pair checker, compiled from a [`ConfusionModel`]. The count tables are keyed by
/// `u32` indices into a shared `vocab` (the artifact's interned side-table), so a surface word is
/// resolved to its index once and the heavy bigram table stays a packed, binary-searchable `Vec`
/// rather than a string-keyed hash map.
pub struct ConfusionChecker {
    /// Confusion word (lower-cased) → alternatives to test (with each pair's factor) when it occurs.
    alternatives: HashMap<String, Vec<(String, f32)>>,
    /// Interned token (word or POS tag) → its index in the count tables.
    vocab: HashMap<String, u32>,
    unigrams: HashMap<u32, u32>,
    /// `(w1_idx, w2_idx, count)`, sorted by the index pair for binary-search lookup.
    bigrams: Vec<(u32, u32, u32)>,
    /// `(leftPOS_idx, member_idx)` → count, and `(member_idx, rightPOS_idx)` → count.
    left_pos: HashMap<(u32, u32), u32>,
    right_pos: HashMap<(u32, u32), u32>,
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
        // The artifact builder already sorts `bigrams`; sort defensively so binary search is sound
        // even for a hand-built model (cheap: a single near-sorted pass at load time).
        let mut bigrams = model.bigrams.clone();
        bigrams.sort_unstable();
        Self {
            alternatives,
            // `vocab.len()` is bounded by the artifact builder's `u32` indices, so the cast is total.
            vocab: model
                .vocab
                .iter()
                .enumerate()
                .map(|(i, s)| (s.clone(), u32::try_from(i).unwrap_or(u32::MAX)))
                .collect(),
            unigrams: model.unigrams.iter().copied().collect(),
            bigrams,
            left_pos: model
                .left_pos
                .iter()
                .map(|&(p, m, c)| ((p, m), c))
                .collect(),
            right_pos: model
                .right_pos
                .iter()
                .map(|&(m, p, c)| ((m, p), c))
                .collect(),
        }
    }

    /// An empty checker that produces no diagnostics — used when no confusion model is available,
    /// so the cascade can always wrap with L3 without branching.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            alternatives: HashMap::new(),
            vocab: HashMap::new(),
            unigrams: HashMap::new(),
            bigrams: Vec::new(),
            left_pos: HashMap::new(),
            right_pos: HashMap::new(),
        }
    }

    /// Build a checker from the rkyv confusion-model artifact.
    ///
    /// # Errors
    /// Returns an error if `bytes` is not a valid archived [`ConfusionModel`].
    pub fn from_rkyv_bytes(bytes: &[u8]) -> Result<Self, rkyv::rancor::Error> {
        Ok(Self::new(&rlt_ir::deserialize_confusion(bytes)?))
    }

    /// Resolve a surface token to its vocab index (`None` ⇒ unseen ⇒ count 0 everywhere).
    fn idx(&self, w: &str) -> Option<u32> {
        self.vocab.get(w).copied()
    }

    fn bigram(&self, a: &str, b: &str) -> u32 {
        let (Some(ia), Some(ib)) = (self.idx(a), self.idx(b)) else {
            return 0;
        };
        self.bigrams
            .binary_search_by(|&(x, y, _)| (x, y).cmp(&(ia, ib)))
            .map_or(0, |i| self.bigrams[i].2)
    }

    fn unigram_smoothed(&self, w: &str) -> f64 {
        let count = self
            .idx(w)
            .and_then(|i| self.unigrams.get(&i))
            .copied()
            .unwrap_or(0);
        f64::from(count) + SMOOTH
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

    /// POS-context log-ratio: the same `log P(alt) − log P(word)` form but over the neighbours'
    /// (context-disambiguated) POS instead of their surface words — so it still discriminates even
    /// when the exact word bigram was unseen. Generalises e.g. "the/to before a noun".
    fn pos_log_ratio(&self, lpos: Option<&str>, word: &str, rpos: Option<&str>, alt: &str) -> f64 {
        // `(a, b)` → count via the interned indices; an unseen token (or POS) ⇒ 0.
        let count = |map: &HashMap<(u32, u32), u32>, a: &str, b: &str| -> f64 {
            let (Some(ia), Some(ib)) = (self.idx(a), self.idx(b)) else {
                return 0.0;
            };
            f64::from(map.get(&(ia, ib)).copied().unwrap_or(0))
        };
        let mut lr = 0.0;
        if let Some(p) = lpos {
            lr += (count(&self.left_pos, p, alt) + SMOOTH).ln()
                - (count(&self.left_pos, p, word) + SMOOTH).ln();
        }
        if let Some(p) = rpos {
            lr += (count(&self.right_pos, alt, p) + SMOOTH).ln()
                - (count(&self.right_pos, word, p) + SMOOTH).ln();
        }
        lr
    }
}

impl GrammarChecker for ConfusionChecker {
    fn grammar_diagnostics(&self, _text: &str, analysis: &Analysis) -> Vec<Diagnostic> {
        let tokens = &analysis.tokens;
        // Lower-case every surface once; each token is otherwise re-lowercased up to three times
        // (as the current word, then as the next token's left neighbour, then the right neighbour).
        let lower: Vec<String> = tokens.iter().map(|t| t.text.to_lowercase()).collect();
        let mut out = Vec::new();
        for i in 0..tokens.len() {
            let word = &lower[i];
            let Some(alts) = self.alternatives.get(word) else {
                continue;
            };
            // Only plain words, and skip contraction heads (the "they" of a "they"+"'re" split).
            if !is_word(word) || tokens.get(i + 1).is_some_and(|t| t.text.starts_with('\'')) {
                continue;
            }
            // Word-only neighbours (punctuation contributes no usable word context).
            let left = i
                .checked_sub(1)
                .map(|j| lower[j].as_str())
                .filter(|w| is_word(w));
            let right = (i + 1 < lower.len())
                .then(|| lower[i + 1].as_str())
                .filter(|w| is_word(w));
            // Neighbours' context-disambiguated primary POS, for the POS-generalised score.
            let lpos = i
                .checked_sub(1)
                .and_then(|j| tokens[j].tags.first())
                .map(String::as_str);
            let rpos = tokens
                .get(i + 1)
                .and_then(|t| t.tags.first())
                .map(String::as_str);

            // Evidence-gate first: most candidates lack the bigram support to fire, and the gate is
            // far cheaper than the two log-ratios — so skip those before scoring them.
            let best = alts
                .iter()
                .filter_map(|(alt, factor)| {
                    if self.evidence(left, alt, right) < MIN_EVIDENCE {
                        return None;
                    }
                    let lr = self.log_ratio(left, word, right, alt)
                        + self.pos_log_ratio(lpos, word, rpos, alt);
                    (lr >= log_threshold(*factor)).then_some((alt, lr))
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));

            if let Some((alt, _)) = best {
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

/// Whether a token is a plain alphabetic word (usable as confusion context). Unicode-aware so it
/// accepts non-Latin scripts (Cyrillic etc.); for ASCII text this is identical to the old
/// `is_ascii_alphabetic` gate, and confusion decisions are gated by the pair table regardless.
fn is_word(w: &str) -> bool {
    !w.is_empty() && w.chars().all(char::is_alphabetic)
}

/// Threshold on the log-likelihood ratio, mapped log-relatively from LT's `factor`.
fn log_threshold(factor: f32) -> f64 {
    let lf = f64::from(factor).log10().clamp(LF_MIN, LF_MAX);
    let t = (lf - LF_MIN) / (LF_MAX - LF_MIN);
    LOGR_MIN + t * (LOGR_MAX - LOGR_MIN)
}

#[cfg(test)]
mod tests {
    use rlt_ir::ConfusionPair;

    use super::*;
    use crate::{Span, Token};

    fn model() -> ConfusionModel {
        // vocab indices: their=0, there=1, over=2, car=3.
        ConfusionModel {
            pairs: vec![ConfusionPair {
                a: "their".to_owned(),
                b: "there".to_owned(),
                factor: 10.0,
                symmetric: true,
            }],
            vocab: vec![
                "their".to_owned(),
                "there".to_owned(),
                "over".to_owned(),
                "car".to_owned(),
            ],
            unigrams: vec![(0, 5_000_000), (1, 5_000_000)],
            // "over there" is common; "over their" is not — context favours "there".
            bigrams: vec![(0, 3, 40000), (2, 1, 50000)],
            left_pos: vec![],
            right_pos: vec![],
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
