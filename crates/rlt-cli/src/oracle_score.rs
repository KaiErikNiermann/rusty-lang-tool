//! Reusable scoring of the on-thesis IR matcher against LanguageTool's bundled `<example>` corpus.
//!
//! Shared by the differential-oracle integration test (which asserts version-specific regression
//! floors) and the `rlt score-oracle` subcommand (which the adaptability sweep calls to read the
//! numbers for *any* LT version). The functions here never assert — they just measure.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use rlt_convert::Example;
use rlt_core::{Checker, Composite, Engine, GrammarChecker, IrMatcher, Source};
use rlt_engine::VendoredEngine;
use serde::Serialize;

/// IR-matcher oracle numbers for one grammar/blob pairing. No asserts — works on any LT version.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreReport {
    /// Positive examples whose expected correction the matcher reproduced.
    pub reproduced: usize,
    /// Total positive (`correction`-bearing) examples.
    pub positive_total: usize,
    /// `reproduced / positive_total` as a percentage.
    pub reproduced_pct: f64,
    /// Negative examples the owning rule wrongly fired on (self-flagged).
    pub false_positives: usize,
    /// Total negative (no-`correction`) examples.
    pub negative_total: usize,
    /// `false_positives / negative_total` as a percentage.
    pub false_positive_pct: f64,
}

/// LanguageTool's positive (`correction`-bearing) examples.
///
/// # Errors
/// Returns an error if the grammar XML cannot be parsed.
pub fn positive_examples(grammar: &Path) -> Result<Vec<Example>> {
    Ok(rlt_convert::extract_examples(grammar)?
        .into_iter()
        .filter(|e| !e.corrections.is_empty())
        .collect())
}

/// LanguageTool's negative examples (no `correction`) — the owning rule must not fire on these.
///
/// # Errors
/// Returns an error if the grammar XML cannot be parsed.
pub fn negative_examples(grammar: &Path) -> Result<Vec<Example>> {
    Ok(rlt_convert::extract_examples(grammar)?
        .into_iter()
        .filter(|e| e.corrections.is_empty())
        .collect())
}

/// How many positive examples' expected correction the checker reproduces (an L2-grammar fix).
pub fn count_reproduced<B: Engine + GrammarChecker>(
    checker: &Checker<B>,
    examples: &[Example],
) -> usize {
    examples
        .iter()
        .filter(|ex| {
            let produced: Vec<String> = checker
                .check(&ex.text)
                .into_iter()
                .filter(|d| d.source == Source::Grammar)
                .flat_map(|d| d.suggestions.into_iter().map(|s| s.replacement))
                .collect();
            ex.corrections.iter().any(|c| produced.iter().any(|p| p == c))
        })
        .count()
}

/// How many negative examples the owning rule wrongly fires on (self-flag = false positive).
pub fn count_false_positives<B: Engine + GrammarChecker>(
    checker: &Checker<B>,
    examples: &[Example],
) -> usize {
    examples
        .iter()
        .filter(|ex| {
            checker
                .check(&ex.text)
                .into_iter()
                .any(|d| d.source == Source::Grammar && d.code == ex.rule_id)
        })
        .count()
}

#[allow(clippy::cast_precision_loss, reason = "corpus sizes are well within f64")]
fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 / total as f64 * 100.0
    }
}

/// Score the IR matcher (`Composite<VendoredEngine, IrMatcher>`) over the `<example>` corpus of a
/// given grammar, using the given tokenizer + compiled rkyv blob.
///
/// # Errors
/// Returns an error if the engine/blob can't load or the grammar can't be parsed.
pub fn score_ir(tokenizer: &Path, blob: &Path, grammar: &Path) -> Result<ScoreReport> {
    let engine = VendoredEngine::from_path(tokenizer)
        .with_context(|| format!("loading engine from {}", tokenizer.display()))?;
    let bytes = std::fs::read(blob).with_context(|| format!("reading {}", blob.display()))?;
    let ir = IrMatcher::from_rkyv_bytes(&bytes)
        .map_err(|e| anyhow!("compiling IR rules from {}: {e}", blob.display()))?;
    let checker = Checker::new(Composite::new(engine, ir));

    let positives = positive_examples(grammar)?;
    let negatives = negative_examples(grammar)?;
    let reproduced = count_reproduced(&checker, &positives);
    let false_positives = count_false_positives(&checker, &negatives);

    Ok(ScoreReport {
        reproduced,
        positive_total: positives.len(),
        reproduced_pct: pct(reproduced, positives.len()),
        false_positives,
        negative_total: negatives.len(),
        false_positive_pct: pct(false_positives, negatives.len()),
    })
}
