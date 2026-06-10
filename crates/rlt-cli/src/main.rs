//! `rlt` — the command-line surface over [`rlt_core`].
//!
//! Subcommands: `check` (lint a file; `--matcher nlprule|ir` selects the L2 backend), `convert`
//! (run the offline LT → rkyv conversion, sharing one codepath with the `rlt-convert` binary) and
//! `tokens` (print the engine's tokenize + POS-tag analysis of a sentence).

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use rlt_core::{
    Checker, Composite, ConfusionChecker, Diagnostic, Engine, GrammarChecker, IrMatcher,
    WithConfusion, WithGrammar,
};
use rlt_engine::VendoredEngine;
use rlt_tagger::{RtenTagSource, Tagger};

/// Which L2 grammar backend to use.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Matcher {
    /// nlprule's bundled LanguageTool v5.2 rules (broad coverage; works after `fetch-engine`).
    Nlprule,
    /// Our matcher over LanguageTool v6.7 rules compiled to `resources/en.rkyv` (the on-thesis
    /// path; needs `cargo xtask build-blob` first).
    Ir,
}

/// Which engine supplies token analysis (POS tags/lemmas) for the oracle.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AnalysisEngine {
    /// nlprule's bundled LanguageTool v5.2 tagger (the baseline; needs `fetch-engine`).
    Nlprule,
    /// The native engine over current-LT tags (`tagger.rkyv` + `segment.srx`; needs `build-tagger`).
    Native,
}

/// Local, web-native grammar and spell checker built on LanguageTool's open rule corpus.
#[derive(Debug, Parser)]
#[command(name = "rlt", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Lint a text file and print diagnostics (`file:line:col CODE "x" -> "y"`).
    Check {
        /// Path to the UTF-8 text file to check.
        file: PathBuf,
        /// Grammar backend for L2.
        #[arg(long, value_enum, default_value_t = Matcher::Nlprule)]
        matcher: Matcher,
    },
    /// Convert a LanguageTool grammar.xml into the runtime rkyv artifact.
    Convert {
        /// Path to LanguageTool's English `grammar.xml`.
        #[arg(long, default_value = rlt_convert::DEFAULT_GRAMMAR)]
        grammar: PathBuf,
        /// Output artifact path.
        #[arg(long, default_value = rlt_convert::DEFAULT_OUT)]
        out: PathBuf,
    },
    /// Build the L3 confusion model from LT's confusion sets + Norvig's n-gram counts.
    BuildConfusion {
        /// LanguageTool confusion sets.
        #[arg(long, default_value = rlt_convert::DEFAULT_CONFUSION_SETS)]
        confusion_sets: PathBuf,
        /// Norvig unigram counts.
        #[arg(long, default_value = rlt_convert::DEFAULT_UNIGRAMS)]
        unigrams: PathBuf,
        /// Norvig bigram counts.
        #[arg(long, default_value = rlt_convert::DEFAULT_BIGRAMS)]
        bigrams: PathBuf,
        /// Output model path.
        #[arg(long, default_value = rlt_convert::DEFAULT_CONFUSION_OUT)]
        out: PathBuf,
    },
    /// Tokenize + POS-tag a sentence and print the engine's analysis (engine smoke test).
    Tokens {
        /// The sentence to analyze.
        text: String,
    },
    /// Score the IR matcher against LT's `<example>` corpus and print the numbers (no asserts).
    /// `--json` feeds the adaptability sweep; works on any LT version's grammar/blob.
    ScoreOracle {
        /// Which engine supplies token analysis: the nlprule baseline or the native current-LT tagger.
        #[arg(long, value_enum, default_value_t = AnalysisEngine::Nlprule)]
        engine: AnalysisEngine,
        /// nlprule tokenizer binary (used by `--engine nlprule`).
        #[arg(long, default_value = rlt_engine::DEFAULT_TOKENIZER_BIN)]
        tokenizer: PathBuf,
        /// SRX segmentation rules (used by `--engine native`).
        #[arg(long, default_value = "resources/segment.srx")]
        segment_srx: PathBuf,
        /// Native POS tagger artifact (used by `--engine native`).
        #[arg(long, default_value = "resources/tagger.rkyv")]
        tagger: PathBuf,
        /// Compiled IR rkyv blob.
        #[arg(long, default_value = rlt_convert::DEFAULT_OUT)]
        blob: PathBuf,
        /// LanguageTool `grammar.xml`.
        #[arg(long, default_value = rlt_convert::DEFAULT_GRAMMAR)]
        grammar: PathBuf,
        /// Emit JSON instead of a human-readable summary.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Check { file, matcher } => run_check(&file, matcher),
        Command::Convert { grammar, out } => {
            let report = rlt_convert::convert(&grammar, &out)?;
            tracing::info!(
                rules_total = report.rules_total,
                rules_opaque = report.rules_opaque,
                covered = format!("{:.1}%", report.covered_fraction() * 100.0),
                "conversion complete",
            );
            Ok(())
        }
        Command::BuildConfusion {
            confusion_sets,
            unigrams,
            bigrams,
            out,
        } => {
            // The POS-context aggregation needs the tagger; load the engine for it.
            let tok = rlt_engine::DEFAULT_TOKENIZER_BIN;
            let engine =
                VendoredEngine::from_path(std::path::Path::new(tok)).with_context(|| {
                    format!("loading engine from {tok} (run `cargo xtask fetch-engine`?)")
                })?;
            let report = rlt_convert::build_confusion_model(
                &confusion_sets,
                &unigrams,
                &bigrams,
                &out,
                |w| engine.pos_tags(w),
            )?;
            tracing::info!(
                pairs = report.pairs,
                bigrams = report.bigrams,
                "confusion model built"
            );
            Ok(())
        }
        Command::Tokens { text } => run_tokens(&text),
        Command::ScoreOracle {
            engine,
            tokenizer,
            segment_srx,
            tagger,
            blob,
            grammar,
            json,
        } => {
            let report = match engine {
                AnalysisEngine::Nlprule => {
                    rlt_cli::oracle_score::score_ir(&tokenizer, &blob, &grammar)?
                }
                AnalysisEngine::Native => {
                    rlt_cli::oracle_score::score_ir_native(&segment_srx, &tagger, &blob, &grammar)?
                }
            };
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!(
                    "reproduced {}/{} ({:.1}%); false positives {}/{} ({:.1}%)",
                    report.reproduced,
                    report.positive_total,
                    report.reproduced_pct,
                    report.false_positives,
                    report.negative_total,
                    report.false_positive_pct,
                );
            }
            Ok(())
        }
    }
}

/// Load the L3 confusion model if present, else an empty (no-op) checker.
fn load_confusion() -> ConfusionChecker {
    let path = rlt_convert::DEFAULT_CONFUSION_OUT;
    std::fs::read(path)
        .ok()
        .and_then(|b| ConfusionChecker::from_rkyv_bytes(&b).ok())
        .unwrap_or_else(|| {
            tracing::warn!("{path} not found — L3 disabled (run `cargo xtask build-confusion`)");
            ConfusionChecker::empty()
        })
}

/// Load the L4 neural tagger from `resources/l4/` if present, else `None` (L4 disabled).
fn load_tagger() -> Option<Tagger<RtenTagSource>> {
    let dir = std::path::Path::new("resources/l4");
    if !dir.join("model.int8.onnx").exists() {
        tracing::warn!("resources/l4 not found — L4 disabled (run `cargo xtask build-l4`)");
        return None;
    }
    match Tagger::from_dir(dir) {
        Ok(tagger) => Some(tagger),
        Err(e) => {
            tracing::warn!("L4 disabled: {e}");
            None
        }
    }
}

/// Stack L3 confusion (always) and L4 neural tagging (when present) onto an L1/L2 backend and run
/// the full cascade. Generic so both `--matcher` backends share one composition path.
fn check_with_layers<B: Engine + GrammarChecker>(
    backend: B,
    confusion: ConfusionChecker,
    tagger: Option<Tagger<RtenTagSource>>,
    text: &str,
) -> Vec<Diagnostic> {
    let confused = WithConfusion::new(backend, confusion);
    match tagger {
        Some(t) => Checker::new(WithGrammar::new(confused, t)).check(text),
        None => Checker::new(confused).check(text),
    }
}

/// Load the engine and print the tokenize + POS-tag analysis of `text`.
fn run_tokens(text: &str) -> Result<()> {
    let bin = rlt_engine::DEFAULT_TOKENIZER_BIN;
    let engine = VendoredEngine::from_path(std::path::Path::new(bin))
        .with_context(|| format!("loading engine from {bin} (run `cargo xtask fetch-engine`?)"))?;
    for token in rlt_core::Engine::analyze(&engine, text).tokens {
        println!(
            "{:>3}..{:<3} {:<14} [{}]",
            token.span.start,
            token.span.end,
            token.text,
            token.tags.join(", "),
        );
    }
    Ok(())
}

fn run_check(file: &std::path::Path, matcher: Matcher) -> Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;

    let tok = rlt_engine::DEFAULT_TOKENIZER_BIN;
    let engine = VendoredEngine::from_path(std::path::Path::new(tok))
        .with_context(|| format!("loading engine from {tok} (run `cargo xtask fetch-engine`?)"))?;

    // L3 confusion (no-op when absent) and L4 neural tagging (skipped when absent) wrap either backend.
    let confusion = load_confusion();
    let tagger = load_tagger();

    // L1 spelling runs for both backends; L2 grammar differs by `matcher`; L3 + L4 wrap both.
    let diagnostics = match matcher {
        Matcher::Nlprule => {
            let rules = rlt_engine::DEFAULT_RULES_BIN;
            let engine = if std::path::Path::new(rules).exists() {
                engine
                    .with_rules_path(std::path::Path::new(rules))
                    .with_context(|| format!("loading grammar rules from {rules}"))?
            } else {
                tracing::warn!(
                    "{rules} not found — spelling only (run `cargo xtask fetch-engine`)"
                );
                engine
            };
            check_with_layers(engine, confusion, tagger, &text)
        }
        Matcher::Ir => {
            let blob = rlt_convert::DEFAULT_OUT;
            let bytes = std::fs::read(blob)
                .with_context(|| format!("reading {blob} (run `cargo xtask build-blob`?)"))?;
            let ir = IrMatcher::from_rkyv_bytes(&bytes)
                .map_err(|e| anyhow!("compiling IR rules from {blob}: {e}"))?;
            check_with_layers(Composite::new(engine, ir), confusion, tagger, &text)
        }
    };

    for d in &diagnostics {
        print_diagnostic(file, &text, d);
    }
    tracing::info!(count = diagnostics.len(), "check complete");
    Ok(())
}

/// Render one diagnostic as `path:line:col CODE "span text" -> "suggestion"`.
fn print_diagnostic(file: &std::path::Path, text: &str, d: &Diagnostic) {
    let (line, col) = line_col(text, d.span.start);
    let span_text = text.get(d.span.start..d.span.end).unwrap_or("");
    let fix = d
        .suggestions
        .first()
        .map(|s| format!(" -> {:?}", s.replacement))
        .unwrap_or_default();
    println!(
        "{}:{}:{}  {}  {:?}{}",
        file.display(),
        line,
        col,
        d.code,
        span_text,
        fix,
    );
}

/// 1-based (line, column) for a byte offset, counting columns in characters.
fn line_col(text: &str, byte: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in text.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
