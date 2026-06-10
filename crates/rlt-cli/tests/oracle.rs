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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rlt_convert::Example;
use rlt_core::{Checker, Composite, ConfusionChecker, Engine, GrammarChecker, IrMatcher, Source};
use rlt_engine::VendoredEngine;
use rlt_tagger::Tagger;

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

/// Load LT's negative examples (no `correction`) — sentences on which the owning rule must NOT
/// fire. Used as a precision check for the IR matcher, whose diagnostic `code` is the rule id.
fn negative_examples(grammar: &Path) -> Vec<Example> {
    rlt_convert::extract_examples(grammar)
        .expect("extract examples")
        .into_iter()
        .filter(|e| e.corrections.is_empty())
        .collect()
}

/// Count negative examples on which the owning rule wrongly fires (self-flag = false positive),
/// and print the rate. Lower is better.
fn false_positive_rate<B: Engine + GrammarChecker>(
    checker: &Checker<B>,
    examples: &[Example],
) -> usize {
    let mut flagged = 0usize;
    for ex in examples {
        let fires = checker
            .check(&ex.text)
            .into_iter()
            .any(|d| d.source == Source::Grammar && d.code == ex.rule_id);
        if fires {
            flagged += 1;
        }
    }
    let total = examples.len();
    #[allow(clippy::cast_precision_loss)]
    let pct = if total == 0 {
        0.0
    } else {
        flagged as f64 / total as f64 * 100.0
    };
    eprintln!(
        "ORACLE [ir v6.7] false positives: {flagged}/{total} negative examples self-flag ({pct:.1}%)"
    );
    flagged
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
    let false_positives = false_positive_rate(&checker, &negative_examples(&grammar));

    // Recall floor: measured 4985/8527 = 58.5% — ahead of the nlprule v5.2 baseline (55.3%). The
    // jump from 53.3% is the <match> `regexp_replace` transforms rendering correctly (440 uses)
    // plus rule-level <regexp> rules. Raise as further coverage grows.
    assert!(
        reproduced >= 4800,
        "IR matcher reproduced only {reproduced}; expected >= 4800"
    );
    // Precision ceiling: measured 718/11211 = 6.4% of negatives self-flag. Antipatterns + skipping
    // disabled/back-ref rules keep this down; lower it as precision improves.
    assert!(
        false_positives <= 900,
        "IR matcher self-flagged {false_positives} negatives; expected <= 900"
    );
}

/// Correct English sentences from LT's examples: negative examples verbatim, positive examples
/// with their correction applied at the marker.
fn correct_sentences(grammar: &Path) -> Vec<String> {
    rlt_convert::extract_examples(grammar)
        .expect("extract examples")
        .into_iter()
        .filter_map(|e| {
            if e.corrections.is_empty() {
                return Some(e.text);
            }
            let (start, end) = e.marker?;
            let mut s = e.text;
            if s.is_char_boundary(start) && s.is_char_boundary(end) && end <= s.len() {
                s.replace_range(start..end, &e.corrections[0]);
                Some(s)
            } else {
                None
            }
        })
        .collect()
}

/// L3 quality via synthetic perturbation: take correct sentences, swap each confusion-set word for
/// a plausible alternative (introducing a real-word error), and measure whether L3 recovers the
/// original (recall) while not flagging the unperturbed correct words (precision).
#[test]
#[ignore = "slow and needs fetched data + confusion.rkyv; run via `cargo xtask run-oracle`"]
fn l3_confusion_precision_recall() {
    let tokenizer = root("resources/en_tokenizer.bin");
    let model_path = root("resources/confusion.rkyv");
    let grammar = root(rlt_convert::DEFAULT_GRAMMAR);
    if missing(&[
        ("tokenizer", &tokenizer),
        ("confusion.rkyv", &model_path),
        ("grammar", &grammar),
    ]) {
        return;
    }

    let engine = VendoredEngine::from_path(&tokenizer).expect("load tokenizer");
    let model_bytes = std::fs::read(&model_path).expect("read confusion model");
    let model = rlt_ir::deserialize_confusion(&model_bytes).expect("deserialize model");
    let checker = ConfusionChecker::new(&model);

    // reverse: correct word → error word(s) L3 should map back to it.
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for p in &model.pairs {
        reverse.entry(p.b.clone()).or_default().push(p.a.clone());
        if p.symmetric {
            reverse.entry(p.a.clone()).or_default().push(p.b.clone());
        }
    }

    let sentences = correct_sentences(&grammar);
    let (mut tp, mut fneg, mut fp, mut perturbations) = (0usize, 0usize, 0usize, 0usize);

    for s in &sentences {
        let analysis = engine.analyze(s);
        // Precision: any L3 flag on an already-correct sentence is a false positive.
        fp += checker.grammar_diagnostics(s, &analysis).len();

        for i in 0..analysis.tokens.len() {
            let word = analysis.tokens[i].text.to_ascii_lowercase();
            let Some(errors) = reverse.get(&word) else {
                continue;
            };
            let span = analysis.tokens[i].span;
            for err in errors {
                perturbations += 1;
                let mut perturbed = analysis.clone();
                perturbed.tokens[i].text.clone_from(err);
                let recovered = checker.grammar_diagnostics(s, &perturbed).iter().any(|d| {
                    d.span == span
                        && d.suggestions
                            .iter()
                            .any(|sg| sg.replacement.eq_ignore_ascii_case(&word))
                });
                if recovered {
                    tp += 1;
                } else {
                    fneg += 1;
                }
            }
        }
    }

    let total = tp + fneg;
    #[allow(clippy::cast_precision_loss)]
    let recall = if total == 0 {
        0.0
    } else {
        tp as f64 / total as f64 * 100.0
    };
    eprintln!(
        "ORACLE [l3]: recall {tp}/{total} ({recall:.1}%) over {} sentences / {perturbations} perturbations; {fp} false positives on correct text",
        sentences.len(),
    );
    // Measured (word + POS context): 94850/114808 = 82.6% recall, 4305 false positives. The
    // residual FP rate is bigram-structural — no fetchable trigram data exists, and the POS-context
    // generalisation only nudges the frontier. Floors/ceilings guard against regressions.
    assert!(
        tp >= 90_000,
        "L3 recall regressed: recovered {tp}; expected >= 90000"
    );
    assert!(
        fp <= 5000,
        "L3 false positives regressed: {fp}; expected <= 5000"
    );
}

/// L4 quality smoke/regression: a curated set of grammatical errors the neural tagger should fix,
/// and correct sentences it must leave untouched. A floor that guards against decode/quantization
/// regressions — *not* a full GEC benchmark (that is the pipeline's offline ERRANT F0.5 gate). The
/// tagger reads text directly, so no engine is needed. Requires `resources/l4/`.
#[test]
#[ignore = "needs resources/l4 artifact; run via `cargo xtask run-l4-oracle`"]
fn l4_edit_tagger_precision_recall() {
    let dir = root("resources/l4");
    if !dir.join("model.int8.onnx").exists() {
        eprintln!(
            "skipping L4 oracle: {} missing (run `cargo xtask build-l4`)",
            dir.display()
        );
        return;
    }
    let tagger = Tagger::from_dir(&dir).expect("load L4 artifact");

    // (sentence, acceptable replacements on the error word — verbs have >1 valid form).
    let errors: &[(&str, &[&str])] = &[
        ("I have a apple .", &["an"]),
        ("They was very happy .", &["were"]),
        ("He do not like it .", &["did", "does"]),
        ("I seen it yesterday .", &["saw"]),
        ("She go to school .", &["went", "goes"]),
    ];
    let mut recalled = 0usize;
    for (text, fixes) in errors {
        let diags = tagger.grammar_diagnostics(text, &rlt_core::Analysis::default());
        let hit = diags.iter().any(|d| {
            d.source == Source::Neural
                && d.suggestions
                    .iter()
                    .any(|s| fixes.contains(&s.replacement.as_str()))
        });
        if hit {
            recalled += 1;
        } else {
            eprintln!("L4 missed {text:?}: {diags:?}");
        }
    }

    let clean = [
        "She goes to school every day .",
        "I have an apple .",
        "The quick brown fox jumps over the lazy dog .",
        "We are very happy today .",
    ];
    let mut false_positives = 0usize;
    for text in clean {
        let n = tagger
            .grammar_diagnostics(text, &rlt_core::Analysis::default())
            .iter()
            .filter(|d| d.source == Source::Neural)
            .count();
        if n > 0 {
            eprintln!("L4 false positive on {text:?}");
        }
        false_positives += n;
    }

    eprintln!(
        "ORACLE [l4]: recall {recalled}/{}, false positives {false_positives}/{}",
        errors.len(),
        clean.len()
    );
    assert!(
        recalled >= 4,
        "L4 recall regressed: {recalled}/{}",
        errors.len()
    );
    assert!(false_positives == 0, "L4 false positives: {false_positives}");
}
