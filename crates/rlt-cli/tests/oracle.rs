//! Differential oracle over LanguageTool's bundled `<example>` sentences.
//!
//! Each LT grammar rule ships positive examples (`correction="…"`). This runs them through the
//! checker and reports the share whose expected correction is reproduced — the self-maintaining
//! health metric. Two backends are scored with the identical harness:
//! - the **nlprule (LT v5.2) baseline**, and
//! - the **on-thesis IR matcher** over our converter's LT v6.7 rules (`resources/en.rkyv`).
//!
//! Requires the engine binaries and the fetched grammar (and, for the IR test, the rkyv blob);
//! each test skips (not fails) when its inputs are absent. Run `cargo xtask run-oracle`.

use std::path::{Path, PathBuf};

use rlt_convert::Example;
use rlt_core::{Checker, Composite, Engine, GrammarChecker, IrMatcher, Source};
use rlt_engine::VendoredEngine;

/// Resolve a workspace-root-relative path from this crate's manifest dir.
fn root(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// `true` (and a heads-up) if any required input is missing — caller should skip.
fn missing(paths: &[(&str, &PathBuf)]) -> bool {
    for (label, p) in paths {
        if !p.exists() {
            eprintln!(
                "skipping oracle: {label} missing at {} (run fetch-lt + fetch-engine)",
                p.display()
            );
            return true;
        }
    }
    false
}

/// Load LT's positive (`correction`-bearing) examples.
fn positive_examples(grammar: &Path) -> Vec<Example> {
    rlt_convert::extract_examples(grammar)
        .expect("extract examples")
        .into_iter()
        .filter(|e| !e.corrections.is_empty())
        .collect()
}

/// Count examples whose expected correction the checker reproduces, and print the rate.
fn score<B: Engine + GrammarChecker>(
    label: &str,
    checker: &Checker<B>,
    examples: &[Example],
) -> usize {
    let mut reproduced = 0usize;
    for ex in examples {
        let produced: Vec<String> = checker
            .check(&ex.text)
            .into_iter()
            .filter(|d| d.source == Source::Grammar)
            .flat_map(|d| d.suggestions.into_iter().map(|s| s.replacement))
            .collect();
        if ex
            .corrections
            .iter()
            .any(|c| produced.iter().any(|p| p == c))
        {
            reproduced += 1;
        }
    }
    let total = examples.len();
    #[allow(clippy::cast_precision_loss)]
    let pct = if total == 0 {
        0.0
    } else {
        reproduced as f64 / total as f64 * 100.0
    };
    eprintln!("ORACLE [{label}]: reproduced {reproduced}/{total} ({pct:.1}%)");
    reproduced
}

#[test]
#[ignore = "slow (~45s) and needs fetched data; run via `cargo xtask run-oracle`"]
fn nlprule_baseline_reproduces_examples() {
    let tokenizer = root("resources/en_tokenizer.bin");
    let rules = root("resources/en_rules.bin");
    let grammar = root(rlt_convert::DEFAULT_GRAMMAR);
    if missing(&[
        ("tokenizer", &tokenizer),
        ("rules", &rules),
        ("grammar", &grammar),
    ]) {
        return;
    }

    let engine = VendoredEngine::from_path(&tokenizer)
        .and_then(|e| e.with_rules_path(&rules))
        .expect("load engine + rules");
    let checker = Checker::new(engine);

    let reproduced = score("nlprule v5.2", &checker, &positive_examples(&grammar));
    // Regression floor just below the measured baseline (4751 with nlprule 0.6.4 vs LT v6.7).
    assert!(
        reproduced >= 4500,
        "nlprule oracle reproduced only {reproduced}; expected >= 4500"
    );
}

#[test]
#[ignore = "slow and needs fetched data + en.rkyv; run via `cargo xtask run-oracle`"]
fn ir_matcher_reproduces_examples() {
    let tokenizer = root("resources/en_tokenizer.bin");
    let blob = root("resources/en.rkyv");
    let grammar = root(rlt_convert::DEFAULT_GRAMMAR);
    if missing(&[
        ("tokenizer", &tokenizer),
        ("en.rkyv", &blob),
        ("grammar", &grammar),
    ]) {
        return;
    }

    let engine = VendoredEngine::from_path(&tokenizer).expect("load tokenizer");
    let bytes = std::fs::read(&blob).expect("read en.rkyv");
    let matcher = IrMatcher::from_rkyv_bytes(&bytes).expect("compile IR rules");
    eprintln!(
        "IR matcher compiled {} matchable rules",
        matcher.rule_count()
    );
    let checker = Checker::new(Composite::new(engine, matcher));

    let reproduced = score("ir v6.7", &checker, &positive_examples(&grammar));
    // Floor just below the measured value (4982 = 55.3%, ahead of the nlprule v5.2 baseline's
    // 52.8%). Raise as the matcher gains construct coverage (antipatterns, and/or/unify, …).
    assert!(
        reproduced >= 4800,
        "IR matcher reproduced only {reproduced}; expected >= 4800"
    );
}
