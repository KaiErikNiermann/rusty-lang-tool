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
        /// Language (ISO code, e.g. `en`, `de`).
        #[arg(long, default_value = "en")]
        lang: String,
        /// Grammar backend for L2 (default: our IR matcher over current LT rules).
        #[arg(long, value_enum, default_value_t = Matcher::Ir)]
        matcher: Matcher,
        /// Analysis engine for the IR matcher: the native current-LT pipeline (default) or nlprule.
        /// Ignored for `--matcher nlprule` (which bundles its own analysis).
        #[arg(long, value_enum, default_value_t = AnalysisEngine::Native)]
        engine: AnalysisEngine,
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
        /// Language (ISO code, e.g. `en`, `de`, `ru`) — selects the native engine + artifacts.
        #[arg(long, default_value = "en")]
        lang: String,
    },
    /// Score the IR matcher against LT's `<example>` corpus and print the numbers (no asserts).
    /// `--json` feeds the adaptability sweep; works on any LT version's grammar/blob.
    ScoreOracle {
        /// Language (ISO code, e.g. `en`, `de`) — supplies the default artifact/grammar paths.
        #[arg(long, default_value = "en")]
        lang: String,
        /// Which engine supplies token analysis: the nlprule baseline or the native current-LT tagger.
        #[arg(long, value_enum, default_value_t = AnalysisEngine::Nlprule)]
        engine: AnalysisEngine,
        /// nlprule tokenizer binary (used by `--engine nlprule`).
        #[arg(long, default_value = rlt_engine::DEFAULT_TOKENIZER_BIN)]
        tokenizer: PathBuf,
        /// SRX segmentation rules (used by `--engine native`; defaults to the shared file).
        #[arg(long)]
        segment_srx: Option<PathBuf>,
        /// Native POS tagger artifact (used by `--engine native`; defaults from `--lang`).
        #[arg(long)]
        tagger: Option<PathBuf>,
        /// Native disambiguation artifact (used by `--engine native`; defaults from `--lang`).
        #[arg(long)]
        disambig: Option<PathBuf>,
        /// Disable the native disambiguation pass even if the disambig artifact exists.
        #[arg(long)]
        no_disambig: bool,
        /// Compiled IR rkyv blob (defaults from `--lang`).
        #[arg(long)]
        blob: Option<PathBuf>,
        /// LanguageTool `grammar.xml` (defaults from `--lang`).
        #[arg(long)]
        grammar: Option<PathBuf>,
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
        Command::Check {
            file,
            lang,
            matcher,
            engine,
        } => run_check(&file, matcher, engine, resolve_lang(&lang)?),
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
        Command::Tokens { text, lang } => run_tokens(&text, resolve_lang(&lang)?),
        Command::ScoreOracle {
            lang,
            engine,
            tokenizer,
            segment_srx,
            tagger,
            disambig,
            no_disambig,
            blob,
            grammar,
            json,
        } => {
            let cfg = resolve_lang(&lang)?;
            // Path flags default from the language config when not given explicitly.
            let blob = blob.unwrap_or_else(|| cfg.grammar_blob_path().into());
            let grammar = grammar.unwrap_or_else(|| cfg.grammar_xml_path().into());
            let report = match engine {
                AnalysisEngine::Nlprule => {
                    rlt_cli::oracle_score::score_ir(&tokenizer, &blob, &grammar)?
                }
                AnalysisEngine::Native => {
                    let segment_srx = segment_srx.unwrap_or_else(|| cfg.segment_srx_path().into());
                    let tagger = tagger.unwrap_or_else(|| cfg.tagger_path().into());
                    let disambig = disambig.unwrap_or_else(|| cfg.disambig_path().into());
                    // Use the disambiguation artifact when present (unless explicitly disabled).
                    let disambig = (!no_disambig && disambig.exists()).then_some(disambig);
                    rlt_cli::oracle_score::score_ir_native(
                        cfg,
                        &segment_srx,
                        &tagger,
                        disambig.as_deref(),
                        &blob,
                        &grammar,
                    )?
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

/// Resolve an ISO language code to its static config, erroring on unknown codes.
fn resolve_lang(code: &str) -> Result<&'static rlt_lang::LangConfig> {
    rlt_lang::config(code)
        .ok_or_else(|| anyhow!("unknown language {code:?} (known: {})", rlt_lang::known()))
}

/// Load the nlprule analysis engine (no grammar rules attached).
fn load_nlprule_engine() -> Result<VendoredEngine> {
    let tok = rlt_engine::DEFAULT_TOKENIZER_BIN;
    VendoredEngine::from_path(std::path::Path::new(tok))
        .with_context(|| format!("loading engine from {tok} (run `cargo xtask fetch-engine`?)"))
}

/// Load the native analysis engine for `cfg` (`segment.srx` + `resources/<lang>/tagger.rkyv`, with
/// `disambig.rkyv` when present), using the language's structural tagset.
fn load_native_engine(cfg: &'static rlt_lang::LangConfig) -> Result<rlt_native::NativeEngine> {
    let srx = PathBuf::from(cfg.segment_srx_path());
    let tagger = PathBuf::from(cfg.tagger_path());
    let disambig = PathBuf::from(cfg.disambig_path());
    rlt_native::NativeEngine::from_paths(
        cfg,
        &srx,
        &tagger,
        disambig.exists().then_some(disambig.as_path()),
    )
    .with_context(|| {
        format!(
            "loading native engine — needs {} (cargo xtask fetch-lt) and {} \
             (cargo xtask build-tagger --lang {}); disambig via build-disambig",
            srx.display(),
            tagger.display(),
            cfg.code,
        )
    })
}

/// Load + compile the IR grammar matcher from `cfg`'s rkyv blob.
fn load_ir_matcher(cfg: &rlt_lang::LangConfig) -> Result<IrMatcher> {
    let blob = cfg.grammar_blob_path();
    let bytes = std::fs::read(&blob)
        .with_context(|| format!("reading {blob} (run `cargo xtask build-blob --lang {}`?)", cfg.code))?;
    IrMatcher::from_rkyv_bytes(&bytes).map_err(|e| anyhow!("compiling IR rules from {blob}: {e}"))
}

/// Load the L3 confusion model if `cfg` enables it and it's present, else an empty (no-op) checker.
fn load_confusion(cfg: &rlt_lang::LangConfig) -> ConfusionChecker {
    if !cfg.sources.confusion {
        return ConfusionChecker::empty();
    }
    let path = cfg.confusion_path();
    std::fs::read(&path)
        .ok()
        .and_then(|b| ConfusionChecker::from_rkyv_bytes(&b).ok())
        .unwrap_or_else(|| {
            tracing::warn!("{path} not found — L3 disabled (run `cargo xtask build-confusion`)");
            ConfusionChecker::empty()
        })
}

/// Load the L4 neural tagger if `cfg` enables it and `resources/<lang>/l4/` is present, else `None`.
fn load_tagger(cfg: &rlt_lang::LangConfig) -> Option<Tagger<RtenTagSource>> {
    if !cfg.sources.neural_l4 {
        return None;
    }
    let dir = PathBuf::from(cfg.l4_dir());
    if !dir.join("model.int8.onnx").exists() {
        tracing::warn!("{} not found — L4 disabled (run `cargo xtask build-l4`)", dir.display());
        return None;
    }
    match Tagger::from_dir(&dir) {
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
    alphabet: &'static str,
    text: &str,
) -> Vec<Diagnostic> {
    let confused = WithConfusion::new(backend, confusion);
    match tagger {
        Some(t) => Checker::with_spell(WithGrammar::new(confused, t), alphabet).check(text),
        None => Checker::with_spell(confused, alphabet).check(text),
    }
}

/// Load the engine and print the tokenize + POS-tag analysis of `text`.
fn run_tokens(text: &str, cfg: &'static rlt_lang::LangConfig) -> Result<()> {
    let engine = load_native_engine(cfg)?;
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

fn run_check(
    file: &std::path::Path,
    matcher: Matcher,
    engine: AnalysisEngine,
    cfg: &'static rlt_lang::LangConfig,
) -> Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;

    // L3 confusion (no-op when absent) and L4 neural tagging (skipped when absent) wrap either backend.
    let confusion = load_confusion(cfg);
    let tagger = load_tagger(cfg);

    // L1 spelling runs for every path; L2 grammar differs by `matcher`; for the IR matcher the
    // analysis engine differs by `engine`; L3 + L4 wrap all of them.
    let diagnostics = match matcher {
        Matcher::Nlprule => {
            // Pure nlprule: it bundles analysis + grammar (the `--engine` flag does not apply).
            let mut nlprule = load_nlprule_engine()?;
            let rules = rlt_engine::DEFAULT_RULES_BIN;
            if std::path::Path::new(rules).exists() {
                nlprule = nlprule
                    .with_rules_path(std::path::Path::new(rules))
                    .with_context(|| format!("loading grammar rules from {rules}"))?;
            } else {
                tracing::warn!("{rules} not found — spelling only (run `cargo xtask fetch-engine`)");
            }
            check_with_layers(nlprule, confusion, tagger, cfg.spell.alphabet, &text)
        }
        Matcher::Ir => {
            let ir = load_ir_matcher(cfg)?;
            match engine {
                AnalysisEngine::Native => {
                    let native = load_native_engine(cfg)?;
                    check_with_layers(Composite::new(native, ir), confusion, tagger, cfg.spell.alphabet, &text)
                }
                AnalysisEngine::Nlprule => {
                    let nlprule = load_nlprule_engine()?;
                    check_with_layers(Composite::new(nlprule, ir), confusion, tagger, cfg.spell.alphabet, &text)
                }
            }
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
