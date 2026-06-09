//! Differential oracle over LanguageTool's bundled `<example>` sentences.
//!
//! Each LT grammar rule ships positive examples (`correction="…"`) and negative ones. This runs
//! the positive examples through the full checker and reports the share whose expected correction
//! the checker reproduces — the self-maintaining health metric: on an LT bump, regenerate and the
//! number moves. It currently measures the **nlprule (LT v5.2) baseline** against LT v6.7 examples,
//! so reproduction is partial by construction; the on-thesis matcher over our converted v6.7 rules
//! (the rkyv IR) will be scored by this same harness and is expected to climb.
//!
//! Requires the engine binaries and the fetched grammar; skips (not fails) when they are absent.
//! Run with `cargo xtask run-oracle` (or `cargo test -p rlt-cli --test oracle -- --nocapture`).

use std::path::{Path, PathBuf};

use rlt_core::{Checker, Source};
use rlt_engine::VendoredEngine;

/// Resolve a workspace-root-relative path from this crate's manifest dir.
fn root(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

#[test]
#[ignore = "slow (~45s) and needs fetched data; run via `cargo xtask run-oracle`"]
fn reproduces_example_corrections() {
    let tokenizer = root("resources/en_tokenizer.bin");
    let rules = root("resources/en_rules.bin");
    let grammar = root(rlt_convert::DEFAULT_GRAMMAR);

    for (label, p) in [
        ("tokenizer", &tokenizer),
        ("rules", &rules),
        ("grammar", &grammar),
    ] {
        if !p.exists() {
            eprintln!(
                "skipping oracle: {label} missing at {} (run fetch-lt + fetch-engine)",
                p.display()
            );
            return;
        }
    }

    let engine = VendoredEngine::from_path(&tokenizer)
        .and_then(|e| e.with_rules_path(&rules))
        .expect("load engine + rules");
    let checker = Checker::new(engine);

    let examples = rlt_convert::extract_examples(&grammar).expect("extract examples");
    let positives: Vec<_> = examples
        .into_iter()
        .filter(|e| !e.corrections.is_empty())
        .collect();

    let mut reproduced = 0usize;
    for ex in &positives {
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

    let total = positives.len();
    #[allow(clippy::cast_precision_loss)]
    let pct = if total == 0 {
        0.0
    } else {
        reproduced as f64 / total as f64 * 100.0
    };
    eprintln!("ORACLE: reproduced {reproduced}/{total} positive examples ({pct:.1}%)");

    // Regression floor just below the measured baseline (4751 with nlprule 0.6.4 vs LT v6.7).
    // Deterministic given the pinned versions; catches regressions without being brittle.
    assert!(
        reproduced >= 4500,
        "oracle reproduced only {reproduced}/{total}; expected >= 4500"
    );
}
