//! Workspace task runner (the `cargo xtask` pattern).
//!
//! Keeps build orchestration in Rust rather than ad-hoc shell scripts:
//! - `fetch-lt` — resumable sparse checkout of just the English resources + XSD schemas at the
//!   pinned LanguageTool tag (NOT the 274 MB full tree).
//! - `gen-schema` — regenerate the committed `xsd-parser` bindings from LT's `rules.xsd`.
//! - `fetch-engine` — download nlprule's prebuilt English tokenizer/rules binaries (resumable).
//! - `build-blob` — run the offline converter to produce the runtime rkyv artifact.
//! - `build-tagger` — build the native engine's POS dictionary from AGID + LT's `remap.awk` (resumable).
//! - `bench` — criterion benchmark of the native engine vs the nlprule baseline.
//! - `build-wasm` — package the WASM surface via `wasm-pack` (Node target) + run the Node smoke test.
//! - `run-oracle` — run the `<example>` differential-oracle test suite.
//! - `build-l4` — build the L4 neural model artifact via the offline `pipeline/` (uv + Python).
//! - `run-l4-oracle` — run the L4 end-to-end / oracle tests (need `resources/l4/`).
//! - `eval-l4` — ERRANT F0.5 eval of the int8 L4 model vs BEA-2019 dev → `resources/l4/metrics.json`.
//! - `adapt-sweep` — run the codegen + converter + oracle across past LT releases (adaptability gauge).
//! - `fuzz` — run a libFuzzer target via `cargo-fuzz` (thin passthrough; lists targets with no arg).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use xsd_parser::config::{
    GeneratorFlags, InterpreterFlags, OptimizerFlags, ParserFlags, Resolver, Schema,
};
use xsd_parser::{Config, generate};

/// The LanguageTool release we track. Bump this and re-run `fetch-lt` + `gen-schema` to retarget;
/// the example oracle then reports exactly which rules drifted. Overridable per-invocation via the
/// `$RLT_LT_VERSION` env var (used by `adapt-sweep` to run the harness across past releases).
const LT_VERSION: &str = "v6.7";
const LT_REPO: &str = "https://github.com/languagetool-org/languagetool.git";
const LT_DEST: &str = "resources/lt";

/// Sparse paths to pull from the LT monorepo: the core rule schemas + each supported language's
/// resources (one entry per `rlt_lang` config; add a language → add its module here).
const SPARSE_PATHS: &[&str] = &[
    "languagetool-core/src/main/resources/org/languagetool/rules",
    "languagetool-language-modules/en/src/main/resources/org/languagetool",
    "languagetool-language-modules/de/src/main/resources/org/languagetool",
    "languagetool-language-modules/ru/src/main/resources/org/languagetool",
    "languagetool-language-modules/ar/src/main/resources/org/languagetool",
    "languagetool-language-modules/fr/src/main/resources/org/languagetool",
    "languagetool-language-modules/es/src/main/resources/org/languagetool",
    "languagetool-language-modules/it/src/main/resources/org/languagetool",
];

/// LT's top-level rules schema (it `xs:include`s `pattern.xsd`); the entry point for codegen.
const RULES_XSD: &str =
    "resources/lt/_repo/languagetool-core/src/main/resources/org/languagetool/rules/rules.xsd";
/// Where the generated bindings are committed (consumed by `rlt-convert`).
const SCHEMA_OUT: &str = "crates/rlt-convert/src/lt_schema.rs";

/// nlprule release whose prebuilt English binaries the baseline engine loads (LT v5.2-derived).
const NLPRULE_VERSION: &str = "0.6.4";
/// Binaries to fetch (gzipped on the release) into `resources/`.
const ENGINE_BINARIES: &[&str] = &["en_tokenizer.bin", "en_rules.bin"];
/// Directory the engine binaries land in (gitignored; the converter artifact lives here too).
const RESOURCES_DIR: &str = "resources";

/// AGID (Automatically Generated Inflection Database) — the inflection source LanguageTool's English
/// POS dictionary is built from. The maintained mirror of Kevin Atkinson's `infl.txt`.
const AGID_INFL: &str = "resources/agid-infl.txt";
const AGID_URL: &str = "https://raw.githubusercontent.com/en-wl/wordlist/master/agid/infl.txt";
/// Kevin Atkinson's Moby/WordNet part-of-speech database — the closed-class half (determiners,
/// prepositions, pronouns, conjunctions) that AGID's inflection list (open-class V/N/A only) lacks.
/// `remap.awk` has rules for both formats; it must see `infl.txt` first (the pos rules reference the
/// JJR/NNS arrays the infl rules populate).
const AGID_POS: &str = "resources/agid-pos.txt";
const AGID_POS_URL: &str =
    "https://raw.githubusercontent.com/en-wl/wordlist/master/pos/part-of-speech.txt";
/// LT's English resource directory (the `remap.awk` build script + `added`/`removed` supplements live
/// here after `fetch-lt`).
const LT_EN_RESOURCE: &str = "resources/lt/_repo/languagetool-language-modules/en/src/main/resources/org/languagetool/resource/en";
/// Hand-authored closed-class supplement (committed; license-clean). Overrides the awk output for the
/// high-frequency function words AGID lacks and `remap.awk` mistags (the, a, is, and, to, pronouns…).
const CLOSED_CLASS: &str = "data/en-closed-class.tsv";

/// Norvig's Google-corpus n-gram subsets (small, fetchable) backing the L3 confusion model.
const NGRAM_DIR: &str = "resources/ngrams";
const NGRAM_FILES: &[(&str, &str)] = &[
    ("count_1w.txt", "https://norvig.com/ngrams/count_1w.txt"),
    ("count_2w.txt", "https://norvig.com/ngrams/count_2w.txt"),
];

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "rusty-lang-tool build tasks")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Sparse-checkout the pinned LT release's English resources + schemas (resumable).
    FetchLt,
    /// Regenerate the committed schema bindings from LT's rules.xsd (run after an LT bump).
    GenSchema,
    /// Download nlprule's prebuilt English tokenizer/rules binaries into resources/ (resumable).
    FetchEngine,
    /// Run the offline converter to (re)build the grammar rkyv artifact for `--lang`.
    BuildBlob {
        /// Language ISO code (e.g. `en`, `de`).
        #[arg(long, default_value = "en")]
        lang: String,
    },
    /// Build the native engine's POS tagger dictionary (`resources/<lang>/tagger.rkyv`) for `--lang`
    /// from LanguageTool's morfologik `.dict` (auto-fetched from the `*-pos-dict` Maven jar), falling
    /// back to AGID + `remap.awk` (English only). Needs `fetch-lt` for the supplements.
    BuildTagger {
        /// Language ISO code (e.g. `en`, `de`).
        #[arg(long, default_value = "en")]
        lang: String,
    },
    /// Lower LanguageTool's `disambiguation.xml` for `--lang` into `resources/<lang>/disambig.rkyv`.
    BuildDisambig {
        /// Language ISO code (e.g. `en`, `de`).
        #[arg(long, default_value = "en")]
        lang: String,
    },
    /// Build all per-language artifacts for `--lang` (tagger + disambig + grammar blob).
    BuildLang {
        /// Language ISO code (e.g. `en`, `de`).
        #[arg(long, default_value = "en")]
        lang: String,
    },
    /// Run the criterion benchmark comparing the native engine to the nlprule baseline (analyze,
    /// is_known, load time). Needs `build-tagger` + `fetch-engine` for the head-to-head.
    Bench,
    /// Download Norvig's n-gram subsets for the L3 confusion model (resumable).
    FetchNgrams,
    /// Build the L3 confusion model from LT's confusion sets + the n-gram subsets.
    BuildConfusion {
        /// Language ISO code (`en` uses Norvig via `rlt convert`; others use an n-gram `--source`).
        #[arg(long, default_value = "en")]
        lang: String,
        /// Non-English n-gram source: `lt-ngrams` (LanguageTool's tuned Lucene data via the JVM
        /// extractor — best recall) or `leipzig` (pure-Rust corpus count — no JVM).
        #[arg(long, default_value = "lt-ngrams")]
        source: String,
    },
    /// Package the WASM surface with wasm-pack (Node target).
    BuildWasm,
    /// Run the differential `<example>` oracle test suite.
    RunOracle,
    /// Build the L4 neural model artifact (`resources/l4/`) via the offline `pipeline/` (uv +
    /// Python 3.12): export the GECToR checkpoint to ONNX, int8-quantize, fetch the verb dict.
    BuildL4,
    /// Run the L4 end-to-end / oracle tests (need the `resources/l4/` artifact).
    RunL4Oracle,
    /// Evaluate the int8 L4 model with ERRANT F0.5 against the BEA-2019 dev set, writing
    /// `resources/l4/metrics.json` (the promotion gate). Needs `build-l4` first.
    EvalL4,
    /// Codegen-adaptability gauge: run fetch-lt -> gen-schema -> build-blob -> score-oracle across
    /// past LT releases and write `docs/adaptability.md`. Resumable; restores the pinned version.
    AdaptSweep {
        /// First LT tag to sweep (inclusive), e.g. `v5.4`. Defaults to the earliest known.
        #[arg(long)]
        from: Option<String>,
        /// Last LT tag to sweep (inclusive), e.g. `v6.8`. Defaults to the pinned version.
        #[arg(long)]
        to: Option<String>,
        /// Re-run versions already recorded in `resources/adaptability/results.json`.
        #[arg(long)]
        force: bool,
        /// Skip sweeping; just regenerate `docs/adaptability.md` from the saved results.
        #[arg(long)]
        report_only: bool,
    },
    /// Run a libFuzzer target via cargo-fuzz (`cargo install cargo-fuzz` first). With no target,
    /// lists the available targets. Args after `--` are forwarded to libFuzzer, e.g.
    /// `cargo xtask fuzz ir_match -- -max_total_time=60`.
    Fuzz {
        /// The fuzz target to run (omit to list available targets).
        target: Option<String>,
        /// Extra arguments forwarded to libFuzzer (after `--`).
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Read-only: dump everything needed to author a `LangConfig` + structural tagset for a language
    /// from the fetched LT checkout (FSA version, .info, dict tag stats, vocalized verdict, candidate
    /// tags, confusion-pair count). Works before the language has a config — pass `--code`/`--lt-module`.
    LangInspect {
        /// ISO code (labels + the `resources/<code>/` artifact dir; usually == lt-module).
        #[arg(long)]
        code: String,
        /// LT module dir under `languagetool-language-modules/` (defaults to `--code`).
        #[arg(long)]
        lt_module: Option<String>,
        /// Override the `.dict` path (default: the repo `resource/<m>/<m_name>.dict`).
        #[arg(long)]
        dict: Option<String>,
        /// Override the `.info` path.
        #[arg(long)]
        info: Option<String>,
    },
    /// Compare the currently-fetched upstream inputs against the committed `lang-manifests/<code>.json`
    /// and report per-file drift (unchanged / changed / missing) + pinned-vs-manifest LT version.
    /// Exits non-zero on drift, so it can gate CI. Detects *linguistic* upstream changes, not just
    /// version bumps (bump `LT_VERSION` → `fetch-lt` → `lang-status` to see what changed).
    LangStatus {
        /// Limit to one language (default: all configured languages).
        #[arg(long)]
        lang: Option<String>,
    },
    /// (Re)write `lang-manifests/<code>.json` from the currently-fetched upstream inputs — run once
    /// after a build validates (oracle green) to pin the content we built from.
    LangManifest {
        /// Language ISO code.
        #[arg(long)]
        lang: String,
    },
    /// Verify every configured language (the canonical `rlt_lang::LANGUAGES`) is wired into all the
    /// sites that *can't* share that Rust const — the per-language manifest, the sparse-checkout paths,
    /// the nightly oracle matrix, and the convert/oracle test names — so adding a language can't leave
    /// one system out of sync. Exits non-zero on any required gap.
    LangCoherence,
    /// Print the canonical language codes (space-separated, from `rlt_lang::LANGUAGES`) so shell/CI can
    /// derive the language set + count dynamically instead of hardcoding a list that can drift.
    LangCodes,
}

fn main() -> Result<()> {
    match Cli::parse().task {
        Task::FetchLt => fetch_lt(&lt_version()),
        Task::GenSchema => gen_schema(),
        Task::FetchEngine => fetch_engine(),
        Task::BuildBlob { lang } => build_blob(lang_cfg(&lang)?),
        Task::BuildTagger { lang } => build_tagger(lang_cfg(&lang)?),
        Task::BuildDisambig { lang } => build_disambig(lang_cfg(&lang)?),
        Task::BuildLang { lang } => {
            let cfg = lang_cfg(&lang)?;
            build_tagger(cfg)?;
            build_disambig(cfg)?;
            build_blob(cfg)
        }
        Task::Bench => run("cargo", &["bench", "-p", "rlt-native", "--bench", "engine"]),
        Task::FetchNgrams => fetch_ngrams(),
        Task::BuildConfusion { lang, .. } if lang == "en" => {
            run("cargo", &["run", "-p", "rlt-cli", "--", "build-confusion"])
        }
        Task::BuildConfusion { lang, source } => build_confusion(lang_cfg(&lang)?, &source),
        Task::BuildWasm => build_wasm(),
        Task::RunOracle => run(
            "cargo",
            &[
                "test",
                "-p",
                "rlt-cli",
                "--test",
                "oracle",
                "--",
                "--ignored",
                "--nocapture",
            ],
        ),
        Task::LangInspect {
            code,
            lt_module,
            dict,
            info,
        } => lang_inspect(&code, lt_module.as_deref(), dict.as_deref(), info.as_deref()),
        Task::LangStatus { lang } => lang_status(lang.as_deref()),
        Task::LangManifest { lang } => lang_manifest(lang_cfg(&lang)?),
        Task::LangCoherence => lang_coherence(),
        Task::LangCodes => {
            println!("{}", rlt_lang::LANGUAGES.iter().map(|c| c.code).collect::<Vec<_>>().join(" "));
            Ok(())
        }
        Task::BuildL4 => build_l4(),
        Task::RunL4Oracle => run(
            "cargo",
            &[
                "test",
                "-p",
                "rlt-cli",
                "--release",
                "--test",
                "oracle",
                "--",
                "--ignored",
                "l4",
                "--nocapture",
            ],
        ),
        Task::EvalL4 => run(
            "uv",
            &["run", "--project", "pipeline", "python", "-m", "rlt_pipeline.evaluate"],
        ),
        Task::AdaptSweep {
            from,
            to,
            force,
            report_only,
        } => adapt_sweep(from.as_deref(), to.as_deref(), force, report_only),
        Task::Fuzz { target, args } => run_fuzz(target.as_deref(), &args),
    }
}

/// Build the L4 artifact via the offline `pipeline/` (uv + Python 3.12). Each step is resumable: the
/// Python scripts skip work whose output already exists.
fn build_l4() -> Result<()> {
    run("uv", &["sync", "--project", "pipeline"])?;
    for module in [
        "rlt_pipeline.export",
        "rlt_pipeline.quantize",
        "rlt_pipeline.fetch",
    ] {
        run(
            "uv",
            &["run", "--project", "pipeline", "python", "-m", module],
        )?;
    }
    Ok(())
}

/// Thin wrapper over `cargo fuzz`: `run <target> [-- <libfuzzer args>]`, or `list` when no target
/// is given. Requires `cargo install cargo-fuzz` and a nightly toolchain.
fn run_fuzz(target: Option<&str>, extra: &[String]) -> Result<()> {
    let Some(target) = target else {
        return run("cargo", &["fuzz", "list"]);
    };
    let mut args = vec!["fuzz", "run", target];
    if !extra.is_empty() {
        args.push("--");
        args.extend(extra.iter().map(String::as_str));
    }
    run("cargo", &args)
}

// ---- Adaptability sweep -------------------------------------------------------------------------

/// LT release tags in chronological order (v6.3 is a *branch*, not a tag — excluded).
const LT_VERSIONS: &[&str] = &[
    "v5.4", "v5.5", "v5.6", "v5.7", "v5.8", "v5.9", "v6.0", "v6.1", "v6.2", "v6.4", "v6.5", "v6.6",
    "v6.7", "v6.8",
];
const ADAPT_DIR: &str = "resources/adaptability";
const ADAPT_RESULTS: &str = "resources/adaptability/results.json";
const ADAPT_REPORT: &str = "docs/adaptability.md";

/// One LT version's row in the adaptability matrix.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AdaptResult {
    version: String,
    fetch_ok: bool,
    gen_schema_ok: bool,
    /// Did `rlt-convert` compile + run against this version's regenerated bindings? (The key signal:
    /// a renamed/removed element the lowering matches on becomes a compile error here.)
    convert_compiles: bool,
    rules_total: Option<usize>,
    rules_opaque: Option<usize>,
    reproduced: Option<usize>,
    positive_total: Option<usize>,
    reproduced_pct: Option<f64>,
    false_positives: Option<usize>,
    negative_total: Option<usize>,
    note: String,
}

/// Mirrors `rlt_cli::oracle_score::ScoreReport`'s JSON (avoids an xtask → rlt-cli dependency).
#[derive(serde::Deserialize)]
struct ScoreJson {
    reproduced: usize,
    positive_total: usize,
    reproduced_pct: f64,
    false_positives: usize,
    negative_total: usize,
}

/// Output of a captured child process (never bails — the sweep records failures).
struct Captured {
    ok: bool,
    stdout: String,
    stderr: String,
}

fn capture(program: &str, args: &[&str]) -> Captured {
    println!("$ {program} {}", args.join(" "));
    match Command::new(program).args(args).output() {
        Ok(o) => Captured {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Captured {
            ok: false,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}

/// Parse the integer immediately following `marker` (e.g. `"rules="` → `5343`).
fn num_after(haystack: &str, marker: &str) -> Option<usize> {
    haystack
        .split(marker)
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// Strip ANSI CSI escapes (tracing colours the captured `key=value` fields, splitting the `=`).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip the CSI sequence up to and including its final letter byte.
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Last `n` bytes of `s` (trimmed, on a char boundary) for compact error notes.
fn tail(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.len() <= n {
        return s.to_owned();
    }
    let start = (s.len() - n..s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(s.len());
    format!("…{}", &s[start..])
}

/// The `[from, to]` inclusive slice of [`LT_VERSIONS`] (defaults: earliest .. pinned).
fn version_range(from: Option<&str>, to: Option<&str>) -> Result<Vec<String>> {
    let idx = |v: &str| LT_VERSIONS.iter().position(|x| *x == v);
    let lo = from.map_or(Ok(0), |v| idx(v).with_context(|| format!("unknown LT tag {v}")))?;
    let hi = match to {
        Some(v) => idx(v).with_context(|| format!("unknown LT tag {v}"))?,
        None => idx(LT_VERSION).unwrap_or(LT_VERSIONS.len() - 1),
    };
    if lo > hi {
        bail!("--from is after --to");
    }
    Ok(LT_VERSIONS[lo..=hi].iter().map(|s| (*s).to_owned()).collect())
}

fn load_results() -> Vec<AdaptResult> {
    std::fs::read_to_string(ADAPT_RESULTS)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_results(results: &[AdaptResult]) -> Result<()> {
    std::fs::create_dir_all(ADAPT_DIR).context("creating adaptability dir")?;
    std::fs::write(ADAPT_RESULTS, serde_json::to_string_pretty(results)?)
        .context("writing results.json")
}

/// Run the whole pipeline for one LT version and record the outcome (never panics/bails).
fn sweep_one(version: &str) -> AdaptResult {
    let mut r = AdaptResult {
        version: version.to_owned(),
        fetch_ok: false,
        gen_schema_ok: false,
        convert_compiles: false,
        rules_total: None,
        rules_opaque: None,
        reproduced: None,
        positive_total: None,
        reproduced_pct: None,
        false_positives: None,
        negative_total: None,
        note: String::new(),
    };
    if let Err(e) = fetch_lt(version) {
        r.note = format!("fetch-lt failed: {e}");
        return r;
    }
    r.fetch_ok = true;
    if let Err(e) = gen_schema() {
        r.note = format!("gen-schema failed: {e}");
        return r;
    }
    r.gen_schema_ok = true;

    // build-blob recompiles rlt-convert against the regenerated bindings — a compile failure here is
    // the sharpest adaptability signal.
    let blob = capture("cargo", &["run", "-q", "-p", "rlt-convert"]);
    if !blob.ok {
        r.note = format!("converter compile/run failed: {}", tail(&blob.stderr, 600));
        return r;
    }
    r.convert_compiles = true;
    // The converter logs its report via tracing (to stdout), with the `key=value` fields coloured.
    let blob_log = strip_ansi(&format!("{}\n{}", blob.stdout, blob.stderr));
    r.rules_total = num_after(&blob_log, "rules=");
    r.rules_opaque = num_after(&blob_log, "opaque=");

    let score = capture(
        "cargo",
        &["run", "-q", "-p", "rlt-cli", "--", "score-oracle", "--json"],
    );
    if !score.ok {
        r.note = format!("score-oracle failed: {}", tail(&score.stderr, 600));
        return r;
    }
    match score
        .stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<ScoreJson>(l).ok())
    {
        Some(s) => {
            r.reproduced = Some(s.reproduced);
            r.positive_total = Some(s.positive_total);
            r.reproduced_pct = Some(s.reproduced_pct);
            r.false_positives = Some(s.false_positives);
            r.negative_total = Some(s.negative_total);
        }
        None => r.note = "score-oracle produced no parseable JSON".into(),
    }
    r
}

/// Restore the pinned [`LT_VERSION`] working tree after the sweep (committed bindings + v6.7 blob).
fn restore_pinned() -> Result<()> {
    println!("== restoring pinned {LT_VERSION} ==");
    run("git", &["checkout", "--", SCHEMA_OUT])?;
    fetch_lt(LT_VERSION)?;
    run("cargo", &["run", "-q", "-p", "rlt-convert"])
}

fn adapt_sweep(from: Option<&str>, to: Option<&str>, force: bool, report_only: bool) -> Result<()> {
    if report_only {
        return write_report(&mut load_results());
    }
    let versions = version_range(from, to)?;
    println!(
        "adaptability sweep over {} version(s): {}",
        versions.len(),
        versions.join(" ")
    );
    let mut results = load_results();
    for v in &versions {
        if !force && results.iter().any(|r| &r.version == v) {
            println!("== {v}: already recorded — skipping (use --force) ==");
            continue;
        }
        println!("== sweeping {v} ==");
        let result = sweep_one(v);
        if result.convert_compiles {
            println!(
                "== {v}: compiles; reproduced {:?}/{:?} ==",
                result.reproduced, result.positive_total
            );
        } else {
            println!("== {v}: DID NOT COMPILE/RUN — {} ==", result.note);
        }
        results.retain(|r| &r.version != v);
        results.push(result);
        save_results(&results)?; // incremental → the sweep is resumable
    }
    restore_pinned()?;
    write_report(&mut results)
}

/// Write `docs/adaptability.md` — the committed live gauge.
fn write_report(results: &mut [AdaptResult]) -> Result<()> {
    use std::fmt::Write as _;
    results.sort_by_key(|r| {
        LT_VERSIONS
            .iter()
            .position(|v| *v == r.version)
            .unwrap_or(usize::MAX)
    });
    let yn = |b: bool| if b { "✅" } else { "❌" };
    let mut md = String::new();
    md.push_str("# LanguageTool codegen-adaptability gauge\n\n");
    md.push_str(
        "Generated by `cargo xtask adapt-sweep`. For each LT release we regenerate the XSD bindings, \
         recompile the converter, build the rkyv blob, and score the IR matcher against that \
         version's own `<example>` corpus — a proxy for how cleanly the self-maintaining converter \
         will absorb *future* releases. **Converter compiles** is the load-bearing column: a \
         renamed/removed XML element the lowering matches on shows up as a compile error.\n\n",
    );

    // Computed headline: the range that compiles + scores, and where it breaks.
    let compiling: Vec<&AdaptResult> = results.iter().filter(|r| r.convert_compiles).collect();
    let failing: Vec<&str> = results
        .iter()
        .filter(|r| r.fetch_ok && !r.convert_compiles)
        .map(|r| r.version.as_str())
        .collect();
    let pcts: Vec<f64> = compiling.iter().filter_map(|r| r.reproduced_pct).collect();
    if let (Some(first), Some(last)) = (compiling.first(), compiling.last()) {
        let lo = pcts.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = pcts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let _ = write!(
            md,
            "**Summary.** The converter (authored for `{LT_VERSION}`) compiles and scores cleanly \
             from **{}** through **{}** — {lo:.1}–{hi:.1}% of each release's own examples reproduced. ",
            first.version, last.version,
        );
        if failing.is_empty() {
            md.push_str("It compiles on every release swept.\n\n");
        } else {
            let _ = writeln!(
                md,
                "It does **not** compile on **{}**, where the older schema lacks an element/attribute \
                 the lowering matches on (exact incompatibilities below) — so the break point is a \
                 *compile error*, never silent drift.\n",
                failing.join(", "),
            );
        }
    }

    md.push_str(
        "| LT | fetch | gen-schema | converter compiles | rules / opaque | reproduced | repro % | FP |\n\
         |---|:-:|:-:|:-:|---|---|--:|---|\n",
    );
    for r in results.iter() {
        let pair = |a: Option<usize>, b: Option<usize>| match (a, b) {
            (Some(x), Some(y)) => format!("{x} / {y}"),
            _ => "—".to_owned(),
        };
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            r.version,
            yn(r.fetch_ok),
            yn(r.gen_schema_ok),
            yn(r.convert_compiles),
            pair(r.rules_total, r.rules_opaque),
            pair(r.reproduced, r.positive_total),
            r.reproduced_pct.map_or("—".to_owned(), |p| format!("{p:.1}%")),
            pair(r.false_positives, r.negative_total),
        );
    }
    if results.iter().any(|r| !r.note.is_empty()) {
        md.push_str("\n## Why the failures fail\n\n");
        for r in results.iter().filter(|r| !r.note.is_empty()) {
            let _ = writeln!(md, "- **{}**: {}", r.version, salient(&r.note));
        }
    }
    std::fs::create_dir_all("docs").context("creating docs dir")?;
    std::fs::write(ADAPT_REPORT, md).context("writing adaptability.md")?;
    println!("wrote {ADAPT_REPORT}");
    Ok(())
}

/// Pull the crux line out of a captured failure note (the rustc `not found` / `error[` line),
/// stripping the `| ^^^^ ` caret decoration rustc prefixes it with.
fn salient(note: &str) -> &str {
    let pick = note
        .lines()
        .find(|l| l.contains("not found"))
        .or_else(|| note.lines().find(|l| l.trim_start().starts_with("error[")))
        .or_else(|| note.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or(note);
    pick.trim_start_matches(|c: char| c == '|' || c == '^' || c.is_whitespace())
}

/// The LT release to operate on: `$RLT_LT_VERSION` if set, else the pinned [`LT_VERSION`].
fn lt_version() -> String {
    std::env::var("RLT_LT_VERSION").unwrap_or_else(|_| LT_VERSION.to_owned())
}

fn fetch_lt(version: &str) -> Result<()> {
    let repo_dir = Path::new(LT_DEST).join("_repo");
    if repo_dir.join(".git").exists() {
        println!(
            "LT checkout exists at {} — fetching {version} (resume)",
            repo_dir.display()
        );
    } else {
        std::fs::create_dir_all(LT_DEST).context("creating LT destination dir")?;
        run(
            "git",
            &[
                "clone",
                "--filter=blob:none",
                "--no-checkout",
                "--depth",
                "1",
                "--branch",
                version,
                LT_REPO,
                repo_dir.to_str().context("non-utf8 path")?,
            ],
        )?;
    }

    let repo = repo_dir.to_str().context("non-utf8 path")?;
    // Bring in the requested tag's objects — needed to switch versions in the shallow clone (the
    // adaptability sweep checks out a different tag each iteration).
    run(
        "git",
        &["-C", repo, "fetch", "--depth", "1", "origin", "tag", version],
    )?;
    let mut sparse = vec!["-C", repo, "sparse-checkout", "set"];
    sparse.extend_from_slice(SPARSE_PATHS);
    run("git", &sparse)?;
    run("git", &["-C", repo, "checkout", version])?;

    // Provision the shared multilingual segmenter. `segment.srx` lives in `languagetool-core`'s
    // *resource* dir, which the sparse checkout (rules-only) excludes — so extract the blob straight
    // from git into resources/segment.srx. (Previously this had to be copied by hand on a fresh clone.)
    let srx_dest = "resources/segment.srx";
    if !Path::new(srx_dest).exists() {
        let blob = format!(
            "{version}:languagetool-core/src/main/resources/org/languagetool/resource/segment.srx"
        );
        let out = Command::new("git")
            .args(["-C", repo, "show", &blob])
            .output()
            .context("extracting segment.srx from the LT checkout")?;
        if !out.status.success() {
            bail!("git show {blob} failed: {}", String::from_utf8_lossy(&out.stderr));
        }
        std::fs::write(srx_dest, &out.stdout).with_context(|| format!("writing {srx_dest}"))?;
        println!("provisioned {srx_dest} ({} bytes)", out.stdout.len());
    }

    println!("fetched LT {version} resources into {}", repo_dir.display());
    println!("next: `cargo xtask build-blob` to compile the rkyv artifact");
    Ok(())
}

/// Regenerate `rlt-convert`'s typed XML bindings from LT's `rules.xsd`.
///
/// Flags were derived empirically against LT v6.7 (see the M1 design notes):
/// - `WITH_NUM_BIG_INT` off — LT uses only small ints; avoids a `num-bigint` dependency.
/// - `SIMPLIFY_MIXED_TYPES` off — required so `<message>`/`<suggestion>` mixed content (text
///   interleaved with optional `<match>`) round-trips instead of forcing a mandatory `<match>`.
/// - `REMOVE_DUPLICATES` off — recommended by upstream; avoids miscompiles on some schemas.
fn gen_schema() -> Result<()> {
    let input = Path::new(RULES_XSD)
        .canonicalize()
        .with_context(|| format!("{RULES_XSD} not found — run `cargo xtask fetch-lt` first"))?;

    let mut config = Config::default().with_quick_xml_deserialize_config(true);
    config.parser.resolver = vec![Resolver::File];
    config.parser.flags = ParserFlags::all();
    config.parser.schemas = vec![Schema::File(input)];
    config.interpreter.flags = InterpreterFlags::all() - InterpreterFlags::WITH_NUM_BIG_INT;
    config.optimizer.flags = OptimizerFlags::all()
        - OptimizerFlags::REMOVE_DUPLICATES
        - OptimizerFlags::SIMPLIFY_MIXED_TYPES;
    config.generator.flags = GeneratorFlags::all();

    let code = generate(config)?.to_string();
    let header = "// @generated by `cargo xtask gen-schema` from LanguageTool's rules.xsd.\n\
                  // Do not edit by hand. Regenerate after bumping LT_VERSION.\n\
                  #![allow(warnings, clippy::all, clippy::pedantic)]\n\n";
    std::fs::write(SCHEMA_OUT, format!("{header}{code}"))
        .with_context(|| format!("writing {SCHEMA_OUT}"))?;

    // Format in place so the committed bindings are reviewable and diff cleanly on the next bump.
    run("rustfmt", &["--edition", "2024", SCHEMA_OUT])?;
    println!("wrote {} ({} bytes pre-format)", SCHEMA_OUT, code.len());
    Ok(())
}

/// Download nlprule's prebuilt English binaries (gzipped) into `resources/` and decompress them.
///
/// Resumable: each binary that already exists is skipped. The binaries are LT v5.2-derived and
/// LGPL-2.1; they back the baseline engine until a custom engine consuming current-LT data lands.
fn fetch_engine() -> Result<()> {
    std::fs::create_dir_all(RESOURCES_DIR).context("creating resources dir")?;
    for name in ENGINE_BINARIES {
        let dest = format!("{RESOURCES_DIR}/{name}");
        if Path::new(&dest).exists() {
            println!("{dest} exists — skipping (resume)");
            continue;
        }
        let url = format!(
            "https://github.com/bminixhofer/nlprule/releases/download/{NLPRULE_VERSION}/{name}.gz"
        );
        let gz = format!("{dest}.gz");
        run("curl", &["-sSL", "-o", &gz, &url])?;
        run("gunzip", &["-f", &gz])?;
        println!("fetched {dest}");
    }
    println!("engine binaries ready in {RESOURCES_DIR}/");
    Ok(())
}

/// Package the WASM surface (nodejs target) and run the Node smoke test against it.
fn build_wasm() -> Result<()> {
    run(
        "wasm-pack",
        &[
            "build",
            "crates/rlt-wasm",
            "--target",
            "nodejs",
            "--out-dir",
            "pkg",
        ],
    )?;
    run("node", &["scripts/smoke_node.mjs"])
}

/// Download Norvig's unigram/bigram count files into `resources/ngrams/` (resumable, ~10 MB).
fn fetch_ngrams() -> Result<()> {
    std::fs::create_dir_all(NGRAM_DIR).context("creating ngram dir")?;
    for (name, url) in NGRAM_FILES {
        let dest = format!("{NGRAM_DIR}/{name}");
        if Path::new(&dest).exists() {
            println!("{dest} exists — skipping (resume)");
            continue;
        }
        run("curl", &["-sSL", "-o", &dest, url])?;
        println!("fetched {dest}");
    }
    println!("next: `cargo xtask build-confusion` to compile the L3 model");
    Ok(())
}

/// Build the native engine's POS tagger dictionary, applying the `added.txt`/`removed.txt`
/// supplements, then serializing the fst artifact. The dictionary comes from **LanguageTool's own
/// morfologik `.dict`** (read directly — its tagset/coverage is exactly LT's) when available, falling
/// back to reconstructing it from AGID via LT's `remap.awk` otherwise.
fn build_tagger(cfg: &'static rlt_lang::LangConfig) -> Result<()> {
    std::fs::create_dir_all(cfg.resource_dir())?;
    let (mut triples, source) = match morfologik_triples(cfg)? {
        Some(t) => (t, "LT morfologik .dict".to_owned()),
        None if cfg.sources.uses_agid => {
            (agid_triples()?, "AGID/POS + remap.awk + closed-class".to_owned())
        }
        None => bail!("no morfologik POS dict for {} and no AGID fallback", cfg.code),
    };
    let from_dict = triples.len();

    // Supplements: + added.txt, − removed.txt (both `fullform⇥baseform⇥postag`, `#` comments) — LT's
    // additions-to / removals-from the binary dict. Apply when present (every language ships them).
    let res = cfg.lt_resource_dir();
    let added = read_triple_file_opt(&format!("{res}/added.txt"))?;
    let removed: std::collections::HashSet<(String, String, String)> =
        read_triple_file_opt(&format!("{res}/removed.txt"))?.into_iter().collect();
    let n_added = added.len();
    triples.extend(added);
    let before = triples.len();
    triples.retain(|t| !removed.contains(t));
    let n_removed = before - triples.len();

    let out = cfg.tagger_path();
    let bytes = rlt_native::build_from_triples(triples)
        .map_err(|e| anyhow::anyhow!("building tagger artifact: {e}"))?;
    std::fs::write(&out, &bytes).with_context(|| format!("writing {out}"))?;
    println!(
        "wrote {out}: {from_dict} triples from {source} + {n_added} added − {n_removed} removed, \
         {} bytes",
        bytes.len(),
    );
    Ok(())
}

/// Build the grammar IR blob for `cfg` (grammar.xml → `resources/<lang>/grammar.rkyv`) via `rlt convert`.
fn build_blob(cfg: &'static rlt_lang::LangConfig) -> Result<()> {
    std::fs::create_dir_all(cfg.resource_dir())?;
    run("cargo", &[
        "run", "-p", "rlt-cli", "--", "convert",
        "--grammar", &cfg.grammar_xml_path(),
        "--out", &cfg.grammar_blob_path(),
    ])
}

/// Read triples straight from LanguageTool's morfologik POS dictionary for `cfg`, fetching the
/// `*-pos-dict` jar if needed. Returns `None` (→ AGID fallback) only if the dict can't be obtained.
fn morfologik_triples(
    cfg: &'static rlt_lang::LangConfig,
) -> Result<Option<Vec<(String, String, String)>>> {
    let dict = cfg.pos_dict_local();
    if !Path::new(&dict).exists() {
        if let Err(e) = fetch_pos_dict(cfg) {
            println!("morfologik POS dict unavailable ({e}) — falling back to AGID");
            return Ok(None);
        }
    }
    let meta = rlt_convert::parse_info(&std::fs::read_to_string(cfg.pos_info_local())?)?;
    let triples = rlt_convert::read_triples(&std::fs::read(&dict)?, &meta)
        .with_context(|| format!("reading {dict}"))?;
    println!("read {} triples from {dict}", triples.len());
    Ok(Some(triples))
}

/// Download `cfg`'s `*-pos-dict` jar and extract its `.dict` + `.info` into `resources/<lang>/`.
/// Only valid for [`PosDict::Maven`] languages; repo-shipped dicts are already present after
/// `fetch-lt` (their `pos_dict_local()` points straight into the checkout).
fn fetch_pos_dict(cfg: &'static rlt_lang::LangConfig) -> Result<()> {
    let rlt_lang::PosDict::Maven {
        jar_dict_path,
        jar_info_path,
        ..
    } = cfg.pos_dict
    else {
        bail!(
            "{} ships its POS dict in the LT repo — run `cargo xtask fetch-lt`",
            cfg.code
        );
    };
    std::fs::create_dir_all(cfg.resource_dir())?;
    let jar = cfg.pos_jar_local();
    let url = cfg.pos_dict.jar_url().expect("Maven dict has a jar URL");
    fetch_if_absent(&jar, &url)?;
    // jars are zip archives; extract the two files flat into resources/<lang>/, renaming to pos.*.
    run("unzip", &["-o", "-j", &jar, jar_dict_path, jar_info_path, "-d", &cfg.resource_dir()])?;
    // unzip -j strips the dir but keeps the basename (english.dict / german.dict); rename to pos.*.
    let dir = cfg.resource_dir();
    let dict_name = jar_dict_path.rsplit('/').next().unwrap_or("pos.dict");
    let info_name = jar_info_path.rsplit('/').next().unwrap_or("pos.info");
    std::fs::rename(format!("{dir}/{dict_name}"), cfg.pos_dict_local())?;
    std::fs::rename(format!("{dir}/{info_name}"), cfg.pos_info_local())?;
    Ok(())
}

/// Reconstruct the POS dictionary from AGID + the Moby/WordNet part-of-speech list via LT's own
/// `remap.awk`, plus the hand-authored closed-class supplement. The English-only fallback when LT's
/// binary dict isn't available.
fn agid_triples() -> Result<Vec<(String, String, String)>> {
    fetch_if_absent(AGID_INFL, AGID_URL)?;
    fetch_if_absent(AGID_POS, AGID_POS_URL)?;

    let remap = format!("{LT_EN_RESOURCE}/remap.awk");
    if !Path::new(&remap).exists() {
        bail!("missing {remap} — run `cargo xtask fetch-lt` first");
    }
    // remap.awk over infl.txt THEN part-of-speech.txt (order matters: the pos rules cross-reference
    // arrays the infl rules build).
    println!("$ gawk -f {remap} {AGID_INFL} {AGID_POS}");
    let out = Command::new("gawk")
        .args(["-f", &remap, AGID_INFL, AGID_POS])
        .output()
        .context("running remap.awk (is `gawk` installed?)")?;
    if !out.status.success() {
        bail!("remap.awk failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let mut triples = parse_triples(&String::from_utf8_lossy(&out.stdout));

    // Closed-class override: replace the awk output for the curated function words (awk mistags/omits
    // them). Drop every awk triple whose surface is overridden, then add ours.
    let overrides = read_triple_file(CLOSED_CLASS)?;
    let overridden: std::collections::HashSet<String> =
        overrides.iter().map(|(surface, ..)| surface.clone()).collect();
    triples.retain(|(surface, ..)| !overridden.contains(surface));
    triples.extend(overrides);
    Ok(triples)
}

/// Lower `cfg`'s `disambiguation.xml` into `resources/<lang>/disambig.rkyv`.
fn build_disambig(cfg: &'static rlt_lang::LangConfig) -> Result<()> {
    std::fs::create_dir_all(cfg.resource_dir())?;
    let src = cfg.disambiguation_xml_path();
    if !Path::new(&src).exists() {
        bail!("missing {src} — run `cargo xtask fetch-lt` first");
    }
    let out = cfg.disambig_path();
    let report = rlt_convert::convert_disambiguation(Path::new(&src), Path::new(&out))?;
    println!(
        "wrote {out}: {} rules ({} applicable, {} unsupported)",
        report.rules.len(),
        report.applicable,
        report.rules.len() - report.applicable,
    );
    Ok(())
}

/// Resolve an ISO language code to its static config.
fn lang_cfg(code: &str) -> Result<&'static rlt_lang::LangConfig> {
    rlt_lang::config(code)
        .ok_or_else(|| anyhow::anyhow!("unknown language {code:?} (known: {})", rlt_lang::known()))
}

/// Read-only accelerator: dump everything a human needs to author a `LangConfig` + derive the
/// structural tagset for `code`, from the fetched LT checkout. Reuses the morfologik reader and the
/// repo path layout; works before the language has a config (so it can guide that config). The grep
/// of `grammar.xml`/`disambiguation.xml` surfaces the most-referenced postags so the author picks the
/// `proper_noun_tag`/`punctuation_tag`/`digit_tag` that match the most rules.
#[allow(clippy::too_many_lines, reason = "a single linear diagnostic dump; splitting hurts readability")]
fn lang_inspect(
    code: &str,
    lt_module: Option<&str>,
    dict: Option<&str>,
    info: Option<&str>,
) -> Result<()> {
    use std::collections::BTreeMap;
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

    let m = lt_module.unwrap_or(code);
    let resource = format!(
        "resources/lt/_repo/languagetool-language-modules/{m}/src/main/resources/org/languagetool/resource/{m}"
    );
    let rules = format!(
        "resources/lt/_repo/languagetool-language-modules/{m}/src/main/resources/org/languagetool/rules/{m}"
    );
    if !Path::new(&resource).exists() {
        bail!("{resource} not found — add {m:?} to SPARSE_PATHS and run `cargo xtask fetch-lt`");
    }

    // Locate the POS dict: an explicit override; else the repo `.dict` (preferring the analyzer over
    // a `*_synth.dict` generator); else — if a `LangConfig` already names a Maven dict for this code —
    // auto-fetch it (so authoring the Maven coords is enough, no manual `build-tagger` round-trip).
    let (dict_path, info_path) = match (dict, info) {
        (Some(d), Some(i)) => (d.to_owned(), i.to_owned()),
        _ => match find_repo_dict(&resource) {
            Ok(pair) => pair,
            Err(repo_err) => {
                let Ok(cfg) = lang_cfg(code) else {
                    return Err(repo_err.context(format!(
                        "no repo .dict under {resource} and no LangConfig for {code:?} — pass --dict/--info"
                    )));
                };
                if !matches!(cfg.pos_dict, rlt_lang::PosDict::Maven { .. }) {
                    return Err(repo_err.context(format!("no repo .dict under {resource}")));
                }
                if !Path::new(&cfg.pos_dict_local()).exists() {
                    println!("  fetching Maven POS dict for {code}…");
                    fetch_pos_dict(cfg)?;
                }
                (cfg.pos_dict_local(), cfg.pos_info_local())
            }
        },
    };
    println!("lang-inspect {code} (module={m})");
    println!("  dict: {dict_path}");

    let bytes = std::fs::read(&dict_path).with_context(|| format!("reading {dict_path}"))?;
    let version = bytes.get(4).copied().unwrap_or(0);
    let fsa = match version {
        0xc6 => "CFSA2(0xc6)",
        0x05 => "FSA5(0x05)",
        _ => "unknown FSA version — UNSUPPORTED",
    };
    let meta = rlt_convert::parse_info(
        &std::fs::read_to_string(&info_path).with_context(|| format!("reading {info_path}"))?,
    )?;
    println!(
        "  fsa: {fsa}  sep={:?} encoder={:?} encoding={}",
        meta.separator as char,
        meta.encoder,
        meta.encoding.map_or("utf-8", |e| e.name()),
    );

    let triples = rlt_convert::read_triples(&bytes, &meta)
        .with_context(|| format!("reading {dict_path}"))?;
    // Top-level tag = the part before the first ':' or ';' (the word-class group across tagsets).
    let top_level = |tag: &str| tag.split([':', ';']).next().unwrap_or(tag).to_owned();
    let mut tag_freq: BTreeMap<String, u64> = BTreeMap::new();
    let mut marks = 0u64;
    let mut letters: std::collections::BTreeSet<char> = std::collections::BTreeSet::new();
    for (inflected, _, tag) in &triples {
        *tag_freq.entry(top_level(tag)).or_default() += 1;
        if inflected.chars().any(|c| c.general_category() == GeneralCategory::NonspacingMark) {
            marks += 1;
        }
        // The spell alphabet: distinct lower-case base letters (skip marks/digits/punct).
        for c in inflected.chars().filter(|c| c.is_alphabetic()) {
            letters.extend(c.to_lowercase());
        }
    }
    let mut top: Vec<_> = tag_freq.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    println!("  triples: {}   distinct top-level tags: {}", triples.len(), tag_freq.len());
    print!("    top tags:");
    for (tag, n) in top.iter().take(10) {
        print!(" {tag}({n})");
    }
    println!();
    for (inflected, lemma, tag) in triples.iter().take(3) {
        println!("    sample: {inflected} | {lemma} | {tag}");
    }
    let alphabet: String = letters.iter().collect();
    println!("  spell.alphabet ({} letters): {alphabet}", letters.len());
    println!(
        "  dict keys: {marks}/{} carry combining marks → {}",
        triples.len(),
        if marks > 0 {
            "VOCALIZED dict → Normalization::None (preserve input marks to match keys)"
        } else {
            "UNVOCALIZED dict → Normalization::StripCombiningMarks if the script's input can carry \
             combining marks (Arabic tashkeel, Hebrew niqqud); else None"
        },
    );

    // Most-referenced postags in the grammar/disambiguation rules — the structural-tag candidates.
    for (label, path) in [
        ("grammar.xml", format!("{rules}/grammar.xml")),
        ("disambiguation.xml", format!("{resource}/disambiguation.xml")),
    ] {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let examples = text.matches("<example").count();
            let mut refs: BTreeMap<String, u64> = BTreeMap::new();
            for after in text.split("postag=\"").skip(1) {
                if let Some(tag) = after.split('"').next() {
                    *refs.entry(tag.to_owned()).or_default() += 1;
                }
            }
            let mut r: Vec<_> = refs.iter().collect();
            r.sort_by(|a, b| b.1.cmp(a.1));
            print!("  {label}: {examples} examples; top postags:");
            for (tag, n) in r.iter().take(8) {
                print!(" {tag}({n})");
            }
            println!();
        }
    }

    let confusion = format!("{resource}/confusion_sets.txt");
    let pairs = confusion_words(&confusion).map_or(0, |w| w.len());
    println!(
        "  confusion_sets.txt: {pairs} words → {}",
        if pairs == 0 { "L3 skip (confusion:false)" } else { "L3 available (confusion:true)" },
    );
    Ok(())
}

/// Find a morfologik `(.dict, .info)` pair under `dir`, preferring the analyzer dict over a
/// `*_synth.dict` generator and skipping the hunspell subdir.
fn find_repo_dict(dir: &str) -> Result<(String, String)> {
    let mut best: Option<String> = None;
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Some(stem) = name.strip_suffix(".dict") {
            let info = path.with_file_name(format!("{stem}.info"));
            if info.exists() {
                let p = path.to_string_lossy().into_owned();
                // Prefer the non-synth dict; take the first otherwise.
                if !name.contains("synth") {
                    return Ok((p.clone(), info.to_string_lossy().into_owned()));
                }
                best.get_or_insert(p);
            }
        }
    }
    let dict = best.context("no .dict with a sibling .info")?;
    Ok((dict.clone(), dict.replace(".dict", ".info")))
}

/// The configured ISO codes — the default set for `lang-status`, derived from the canonical
/// [`rlt_lang::LANGUAGES`] so it can never list a different set than the engine actually knows.
fn configured_langs() -> Vec<&'static str> {
    rlt_lang::LANGUAGES.iter().map(|c| c.code).collect()
}

/// One hashed upstream input in a language manifest.
#[derive(serde::Serialize, serde::Deserialize)]
struct ManifestInput {
    /// Hex SHA-256 of the file's bytes; `None` if the (optional) input is absent.
    sha256: Option<String>,
    /// Byte length (0 if absent).
    bytes: u64,
    /// Which built artifact this input feeds.
    feeds: String,
    /// Whether the input is optional (absence is not drift).
    optional: bool,
}

/// A committed record of the *content* (not just the LT version) of every upstream input that feeds
/// a language's artifacts, so linguistic drift is detectable. Lives at `lang-manifests/<code>.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct LangManifest {
    code: String,
    lt_version: String,
    inputs: std::collections::BTreeMap<String, ManifestInput>,
    /// The structural-tag strings chosen in the `LangConfig`, pinned for review.
    tagset_values: std::collections::BTreeMap<String, String>,
}

/// The upstream inputs that feed a language's artifacts: `(key, path, feeds, optional)`. One place so
/// `lang-manifest` and `lang-status` agree on the set. Paths come from the `LangConfig` getters.
fn manifest_inputs(cfg: &'static rlt_lang::LangConfig) -> Vec<(&'static str, String, &'static str, bool)> {
    let res = cfg.lt_resource_dir();
    vec![
        ("grammar.xml", cfg.grammar_xml_path(), "grammar.rkyv", false),
        ("disambiguation.xml", cfg.disambiguation_xml_path(), "disambig.rkyv", false),
        // Optional: en reconstructs its tagger from AGID (no morfologik dict); de/ru/ar ship one.
        ("pos.dict", cfg.pos_dict_local(), "tagger.rkyv", true),
        ("pos.info", cfg.pos_info_local(), "tagger.rkyv", true),
        ("added.txt", format!("{res}/added.txt"), "tagger.rkyv", true),
        ("removed.txt", format!("{res}/removed.txt"), "tagger.rkyv", true),
        ("confusion_sets.txt", format!("{res}/confusion_sets.txt"), "confusion.rkyv", true),
    ]
}

/// Hex SHA-256 + byte length of a file, or `None` if it doesn't exist.
fn sha256_file(path: &str) -> Option<(String, u64)> {
    use std::fmt::Write as _;

    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    let hash = Sha256::digest(&bytes);
    let hex = hash.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    Some((hex, bytes.len() as u64))
}

/// Write `lang-manifests/<code>.json` from the currently-fetched upstream inputs (run after a build
/// validates). Required inputs must be present; optional ones are recorded as absent.
fn lang_manifest(cfg: &'static rlt_lang::LangConfig) -> Result<()> {
    let mut inputs = std::collections::BTreeMap::new();
    for (key, path, feeds, optional) in manifest_inputs(cfg) {
        let (sha256, bytes) = match sha256_file(&path) {
            Some((s, b)) => (Some(s), b),
            None if optional => (None, 0),
            None => bail!("required input {key} missing at {path} — run `build-lang --lang {}` first", cfg.code),
        };
        inputs.insert(key.to_owned(), ManifestInput { sha256, bytes, feeds: feeds.to_owned(), optional });
    }
    let tagset_values = [
        ("digit_tag", cfg.tagset.digit_tag),
        ("punctuation_tag", cfg.tagset.punctuation_tag),
        ("proper_noun_tag", cfg.tagset.proper_noun_tag),
        ("oov_tag", cfg.tagset.oov_tag),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let manifest = LangManifest {
        code: cfg.code.to_owned(),
        lt_version: lt_version(),
        inputs,
        tagset_values,
    };
    std::fs::create_dir_all("lang-manifests")?;
    let path = format!("lang-manifests/{}.json", cfg.code);
    std::fs::write(&path, serde_json::to_string_pretty(&manifest)? + "\n")?;
    println!(
        "wrote {path}: {} inputs, lt_version {}",
        manifest.inputs.len(),
        manifest.lt_version
    );
    Ok(())
}

/// Compare current upstream inputs to each committed manifest and report per-file drift. Exits
/// non-zero if anything changed since the manifest was written (so CI can gate on it).
fn lang_status(opt_lang: Option<&str>) -> Result<()> {
    let codes: Vec<&str> = match opt_lang {
        Some(l) => vec![l],
        None => configured_langs(),
    };
    let mut drifted = false;
    for code in codes {
        let cfg = lang_cfg(code)?;
        let manifest_path = format!("lang-manifests/{code}.json");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            println!("{code}: no manifest — run `cargo xtask lang-manifest --lang {code}`");
            drifted = true;
            continue;
        };
        let manifest: LangManifest = serde_json::from_str(&text)
            .with_context(|| format!("parsing {manifest_path}"))?;
        let pinned = lt_version();
        if pinned == manifest.lt_version {
            println!("{code}: LT {pinned}");
        } else {
            println!("{code}: LT pinned {pinned} ≠ manifest {} — re-validate + re-pin", manifest.lt_version);
        }
        for (key, path, _feeds, optional) in manifest_inputs(cfg) {
            let current = sha256_file(&path).map(|(s, _)| s);
            let recorded = manifest.inputs.get(key).and_then(|i| i.sha256.clone());
            match (current.as_deref(), recorded.as_deref()) {
                (Some(c), Some(r)) if c == r => {}
                (Some(c), Some(r)) => {
                    println!("  CHANGED  {key}: {}… → {}…", &r[..12], &c[..12]);
                    drifted = true;
                }
                (Some(c), None) => {
                    println!("  NEW      {key}: {}… (not in manifest)", &c[..12]);
                    drifted = true;
                }
                (None, Some(r)) => {
                    println!("  MISSING  {key}: was {}…", &r[..12]);
                    drifted = true;
                }
                (None, None) => {
                    if !optional {
                        println!("  ABSENT   {key} (required, not fetched — run `fetch-lt`)");
                    }
                }
            }
        }
    }
    if drifted {
        bail!("upstream drift (or missing manifest) detected — review above");
    }
    println!("all upstream inputs unchanged");
    Ok(())
}

/// Whether a coherence requirement gates CI or is only surfaced for review.
enum Severity {
    /// A genuine wiring gap — the language is not fully incorporated; fails the build.
    Required,
    /// A soft expectation a language may legitimately not meet (e.g. ru has L3 enabled but no
    /// precision/recall floor — its corpus is too small); surfaced as a warning, never fatal.
    Recommended,
}

/// One coherence requirement evaluated for one language: `result` is `Ok` if the site includes the
/// language, or `Err(fix-hint)` describing exactly what to add.
struct Check {
    /// Short label of the validated site.
    label: &'static str,
    /// Whether a failure gates CI.
    severity: Severity,
    /// `Ok(())` if the language is wired into this site, else `Err(hint)`.
    result: Result<(), String>,
}

impl Check {
    fn new(label: &'static str, severity: Severity, ok: bool, hint: String) -> Self {
        Self { label, severity, result: if ok { Ok(()) } else { Err(hint) } }
    }
    fn required(label: &'static str, ok: bool, hint: String) -> Self {
        Self::new(label, Severity::Required, ok, hint)
    }
    fn recommended(label: &'static str, ok: bool, hint: String) -> Self {
        Self::new(label, Severity::Recommended, ok, hint)
    }
}

/// Parse the `lang: [en, de, …]` matrix from the nightly oracle job (the one site that lists every
/// language as data, not via the Rust const). Returns the codes, or empty if the matrix isn't found.
fn nightly_oracle_langs(yaml: &str) -> Vec<String> {
    yaml.lines()
        .find_map(|l| l.trim().strip_prefix("lang:").map(str::trim))
        .and_then(|v| v.strip_prefix('[')?.strip_suffix(']'))
        .map(|inner| inner.split(',').map(|s| s.trim().to_owned()).collect())
        .unwrap_or_default()
}

/// Verify every configured language is wired into the non-Rust / cross-file sites that can't derive
/// from [`rlt_lang::LANGUAGES`]. Required failures gate CI; recommended ones only warn. Closes with an
/// informational sweep of every numeric "N langs" mention in the tree so stale counts surface.
fn lang_coherence() -> Result<()> {
    let nightly = std::fs::read_to_string(".github/workflows/nightly.yml")
        .context("reading .github/workflows/nightly.yml")?;
    let oracle = std::fs::read_to_string("crates/rlt-cli/tests/oracle.rs")
        .context("reading crates/rlt-cli/tests/oracle.rs")?;
    let morfologik = std::fs::read_to_string("crates/rlt-convert/src/morfologik.rs")
        .context("reading crates/rlt-convert/src/morfologik.rs")?;
    let oracle_matrix = nightly_oracle_langs(&nightly);

    let count = rlt_lang::LANGUAGES.len();
    println!("language coherence — {count} configured: {}\n", rlt_lang::known());

    let mut failures = 0usize;
    for cfg in rlt_lang::LANGUAGES {
        let code = cfg.code;
        let mut checks = vec![
            Check::required(
                "manifest",
                Path::new(&format!("lang-manifests/{code}.json")).exists(),
                format!("run `cargo xtask lang-manifest --lang {code}` to write lang-manifests/{code}.json"),
            ),
            Check::required(
                "sparse-checkout path",
                SPARSE_PATHS.contains(&cfg.lt_sparse_path().as_str()),
                format!("add {:?} to SPARSE_PATHS in xtask/src/main.rs", cfg.lt_sparse_path()),
            ),
            Check::required(
                "nightly oracle matrix",
                oracle_matrix.iter().any(|c| c == code),
                format!("add `{code}` to the oracle `lang:` matrix in .github/workflows/nightly.yml"),
            ),
            Check::required(
                "morfologik dict test",
                morfologik.contains(&format!("fn reads_real_languagetool_{}_dict", cfg.name)),
                format!(
                    "add `reads_real_languagetool_{}_dict` to crates/rlt-convert/src/morfologik.rs",
                    cfg.name
                ),
            ),
        ];

        // Native L2 oracle. English's grammar path is exercised by the nlprule + IR-matcher pair rather
        // than a `<code>_native_reproduces_examples` test, so it has its own expectation.
        if code == "en" {
            checks.push(Check::required(
                "native oracle test",
                oracle.contains("fn nlprule_baseline_reproduces_examples")
                    && oracle.contains("fn ir_matcher_reproduces_examples"),
                "restore en's nlprule_baseline_reproduces_examples + ir_matcher_reproduces_examples in crates/rlt-cli/tests/oracle.rs".to_owned(),
            ));
        } else {
            checks.push(Check::required(
                "native oracle test",
                oracle.contains(&format!("fn {code}_native_reproduces_examples")),
                format!("add `{code}_native_reproduces_examples` to crates/rlt-cli/tests/oracle.rs"),
            ));
        }

        // L3 confusion — only languages that enable it must be buildable; the scored floor is a soft
        // expectation (ru enables L3 but its corpus is too small for a meaningful precision/recall floor).
        if cfg.sources.confusion {
            checks.push(Check::required(
                "L3 confusion build",
                code == "en" || confusion_corpus(code).is_some(),
                format!("add a `{code}` arm to confusion_corpus() in xtask/src/main.rs"),
            ));
            let l3_test = if code == "en" {
                "l3_confusion_precision_recall".to_owned()
            } else {
                format!("{code}_l3_confusion_precision_recall")
            };
            checks.push(Check::recommended(
                "L3 oracle floor",
                oracle.contains(&format!("fn {l3_test}")),
                format!("add `{l3_test}` to crates/rlt-cli/tests/oracle.rs, or accept no floor (tiny corpus)"),
            ));
        }

        let lang_failed = checks
            .iter()
            .any(|c| matches!(c.severity, Severity::Required) && c.result.is_err());
        println!("{} {code} ({})", if lang_failed { '✗' } else { '✓' }, cfg.name);
        for c in &checks {
            match (&c.result, &c.severity) {
                (Ok(()), _) => println!("    PASS  {}", c.label),
                (Err(hint), Severity::Required) => {
                    println!("    FAIL  {} — {hint}", c.label);
                    failures += 1;
                }
                (Err(hint), Severity::Recommended) => println!("    WARN  {} — {hint}", c.label),
            }
        }
        println!();
    }

    if failures > 0 {
        bail!("{failures} required coherence check(s) failed — a language is not fully wired in");
    }
    println!("all required coherence checks passed");
    Ok(())
}

/// Leipzig Corpora Collection — a clean, fetchable German news corpus (tagged sentences) used as the
/// L3 n-gram source. LanguageTool's own German n-grams are a Java Lucene index (no Rust reader), and
/// our English L3 already uses a non-LT corpus (Norvig), so a non-LT German corpus is consistent.
const LEIPZIG_DE_URL: &str =
    "https://downloads.wortschatz-leipzig.de/corpora/deu_news_2021_1M.tar.gz";

/// Leipzig Russian news corpus — the L3 n-gram source for Russian (no prebuilt LT n-grams exist for
/// ru; LT ships tuned Lucene n-grams only for en/de/fr/es).
const LEIPZIG_RU_URL: &str =
    "https://downloads.wortschatz-leipzig.de/corpora/rus_news_2022_1M.tar.gz";

/// Leipzig fallback corpora for French / Spanish (used only if the JVM/lt-ngrams path is unavailable).
const LEIPZIG_FR_URL: &str =
    "https://downloads.wortschatz-leipzig.de/corpora/fra_news_2022_1M.tar.gz";
const LEIPZIG_ES_URL: &str =
    "https://downloads.wortschatz-leipzig.de/corpora/spa_news_2022_1M.tar.gz";

/// LanguageTool's own tuned n-gram datasets (Java Lucene indexes). The preferred L3 source — extracted
/// to TSV via `tools/NgramDump.java`; far better recall than Leipzig (more coverage + the confusion
/// `factor` thresholds were calibrated against them). LT publishes these for en/de/fr/es only. ~1.6–1.8
/// GB each; build-time only.
const LT_NGRAM_DE_URL: &str = "https://languagetool.org/download/ngram-data/ngrams-de-20150819.zip";
const LT_NGRAM_FR_URL: &str = "https://languagetool.org/download/ngram-data/ngrams-fr-20150913.zip";
const LT_NGRAM_ES_URL: &str = "https://languagetool.org/download/ngram-data/ngrams-es-20150915.zip";

/// The n-gram corpora for a non-English L3 confusion build: `(LanguageTool tuned n-grams or `None`,
/// Leipzig fallback)`. de/fr/es have LT's own tuned Lucene n-grams (best recall); ru has Leipzig only.
/// `None` for a language with no L3 build configured (English builds via the dedicated Norvig path).
/// The single source of truth for "which non-en languages can build L3" — the coherence checker reads it.
fn confusion_corpus(code: &str) -> Option<(Option<&'static str>, &'static str)> {
    match code {
        "de" => Some((Some(LT_NGRAM_DE_URL), LEIPZIG_DE_URL)),
        "fr" => Some((Some(LT_NGRAM_FR_URL), LEIPZIG_FR_URL)),
        "es" => Some((Some(LT_NGRAM_ES_URL), LEIPZIG_ES_URL)),
        "ru" => Some((None, LEIPZIG_RU_URL)),
        _ => None,
    }
}

/// Build a language's L3 confusion model, dispatching by language. de/fr/es have LanguageTool's own
/// tuned Lucene n-grams (best recall, via the JVM extractor) with a Leipzig fallback; ru uses Leipzig
/// only (no LT n-grams exist for it). `source == "leipzig"` forces the corpus path (no JVM).
fn build_confusion(cfg: &'static rlt_lang::LangConfig, source: &str) -> Result<()> {
    let (lt_ngram_url, leipzig_url) = confusion_corpus(cfg.code)
        .with_context(|| format!("no L3 confusion build configured for {:?}", cfg.code))?;
    if source != "leipzig" {
        if let Some(url) = lt_ngram_url {
            match build_confusion_lt_ngrams(cfg, url) {
                Ok(()) => return Ok(()),
                Err(e) => println!("lt-ngrams source unavailable ({e}) — falling back to Leipzig"),
            }
        }
    }
    build_confusion_leipzig(cfg, leipzig_url)
}

/// Build the model from already-counted Norvig-format TSVs, using the native engine's POS tags.
fn build_confusion_from_counts(
    cfg: &'static rlt_lang::LangConfig,
    count_1w: &str,
    count_2w: &str,
) -> Result<()> {
    let confusion_sets = format!("{}/confusion_sets.txt", cfg.lt_resource_dir());
    let engine = rlt_native::NativeEngine::from_paths(
        cfg,
        Path::new(cfg.segment_srx_path()),
        &std::path::PathBuf::from(cfg.tagger_path()),
        None,
    )
    .map_err(|e| anyhow::anyhow!("loading {} native engine: {e}", cfg.code))?;
    let out = cfg.confusion_path();
    let report = rlt_convert::build_confusion_model(
        Path::new(&confusion_sets),
        Path::new(count_1w),
        Path::new(count_2w),
        Path::new(&out),
        |w| engine.pos_tags(w),
    )?;
    println!("wrote {out}: {} pairs, {} bigrams", report.pairs, report.bigrams);
    Ok(())
}

/// L3 source: LanguageTool's own (Lucene) n-grams for `cfg`, downloaded from `url` and dumped to TSV
/// by the JVM extractor. Generic over the language (the Lucene index is laid out under `cfg.lt_module`).
fn build_confusion_lt_ngrams(cfg: &'static rlt_lang::LangConfig, url: &str) -> Result<()> {
    if Command::new("java").arg("-version").output().is_err() {
        bail!("no JDK on PATH (needed to read LT's Lucene n-grams)");
    }
    let dir = format!("{}/ngrams", cfg.resource_dir());
    std::fs::create_dir_all(&dir)?;

    // 1. Fetch + extract LT's 1-gram and 2-gram Lucene indexes (resumable; ~1.6–1.8 GB).
    let index_dir = format!("{dir}/lt-index");
    if !Path::new(&format!("{index_dir}/{lt}/2grams", lt = cfg.lt_module)).exists() {
        let zip = format!("{dir}/lt-ngrams.zip");
        fetch_if_absent(&zip, url)?;
        let one = format!("{}/1grams/*", cfg.lt_module);
        let two = format!("{}/2grams/*", cfg.lt_module);
        run("unzip", &["-o", "-q", &zip, &one, &two, "-d", &index_dir])?;
    }

    // 2. Compile the extractor (if needed), fetching the Lucene 6.x jars (read the 2015 5.0 index).
    if !Path::new("tools/out/NgramDump.class").exists() {
        std::fs::create_dir_all("tools/lib")?;
        for jar in ["lucene-core", "lucene-backward-codecs"] {
            fetch_if_absent(
                &format!("tools/lib/{jar}-6.6.6.jar"),
                &format!("https://repo1.maven.org/maven2/org/apache/lucene/{jar}/6.6.6/{jar}-6.6.6.jar"),
            )?;
        }
        std::fs::create_dir_all("tools/out")?;
        run("javac", &["-cp", "tools/lib/*", "-d", "tools/out", "tools/NgramDump.java"])?;
    }
    // 3. Dump each index to a Norvig-format TSV (resumable — the dump is the slow step, so skip it
    //    when both TSVs are already present and non-empty).
    let count_1w = format!("{dir}/count_1w.txt");
    let count_2w = format!("{dir}/count_2w.txt");
    let cached = |p: &str| Path::new(p).metadata().is_ok_and(|m| m.len() > 0);
    if !(cached(&count_1w) && cached(&count_2w)) {
        let cp = "tools/lib/*:tools/out";
        run("java", &["-cp", cp, "NgramDump", &format!("{index_dir}/{}/1grams", cfg.lt_module), &count_1w])?;
        run("java", &["-cp", cp, "NgramDump", &format!("{index_dir}/{}/2grams", cfg.lt_module), &count_2w])?;
    }

    build_confusion_from_counts(cfg, &count_1w, &count_2w)
}

/// L3 source: a Leipzig corpus counted in pure Rust (no JVM). Lower recall than LT's tuned n-grams.
fn build_confusion_leipzig(cfg: &'static rlt_lang::LangConfig, url: &str) -> Result<()> {
    let dir = format!("{}/ngrams", cfg.resource_dir());
    std::fs::create_dir_all(&dir)?;
    let sentences = format!("{dir}/sentences.txt");
    if !Path::new(&sentences).exists() {
        let tar = format!("{dir}/leipzig.tar.gz");
        fetch_if_absent(&tar, url)?;
        // The plain `*-sentences.txt` (`id<TAB>sentence`) is present in every Leipzig corpus; the
        // counter strips any `|TAG` itself, so tagged sentences aren't needed.
        run("tar", &["xzf", &tar, "-C", &dir, "--strip-components=1", "--wildcards", "*-sentences.txt"])?;
        let extracted = std::fs::read_dir(&dir)?
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().ends_with("-sentences.txt"))
            .context("Leipzig tar had no *-sentences.txt")?;
        std::fs::rename(extracted, &sentences)?;
    }
    let words = confusion_words(&format!("{}/confusion_sets.txt", cfg.lt_resource_dir()))?;
    let count_1w = format!("{dir}/count_1w.txt");
    let count_2w = format!("{dir}/count_2w.txt");
    count_corpus(&sentences, &words, &count_1w, &count_2w)?;
    build_confusion_from_counts(cfg, &count_1w, &count_2w)
}

/// The lower-cased word set of a `confusion_sets.txt` (the prune set for n-gram counting).
fn confusion_words(confusion_sets: &str) -> Result<std::collections::HashSet<String>> {
    let text = std::fs::read_to_string(confusion_sets)
        .with_context(|| format!("reading {confusion_sets}"))?;
    let mut words = std::collections::HashSet::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        // Format: `a -> b; factor` or `a; b; factor` — collect the word tokens before the `;`.
        let head = line.split(';').next().unwrap_or("");
        for w in head.split("->").flat_map(|s| s.split_whitespace()) {
            words.insert(w.trim().to_lowercase());
        }
    }
    Ok(words)
}

/// Stream Leipzig tagged sentences (`id⇥word|POS word|POS …`), counting unigrams (all) + bigrams that
/// touch a confusion word, writing Norvig-format TSVs (`word⇥count`, `w1 w2⇥count`).
fn count_corpus(
    sentences: &str,
    confusion_words: &std::collections::HashSet<String>,
    out_1w: &str,
    out_2w: &str,
) -> Result<()> {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, BufWriter, Write};

    let reader = BufReader::new(std::fs::File::open(sentences)?);
    let mut uni: HashMap<String, u32> = HashMap::new();
    let mut bi: HashMap<String, u32> = HashMap::new();
    for line in reader.lines() {
        let line = line?;
        let Some((_, rest)) = line.split_once('\t') else {
            continue;
        };
        let mut prev: Option<String> = None;
        for tok in rest.split(' ') {
            let word = tok.split('|').next().unwrap_or(tok);
            if word.is_empty() {
                continue;
            }
            let w = word.to_lowercase();
            *uni.entry(w.clone()).or_default() += 1;
            if let Some(p) = prev.take() {
                if confusion_words.contains(&p) || confusion_words.contains(&w) {
                    *bi.entry(format!("{p} {w}")).or_default() += 1;
                }
            }
            prev = Some(w);
        }
    }
    // count_1w: prune to count ≥ 2 to bound the file (singletons add only noise).
    let mut w1 = BufWriter::new(std::fs::File::create(out_1w)?);
    for (w, c) in &uni {
        if *c >= 2 {
            writeln!(w1, "{w}\t{c}")?;
        }
    }
    let mut w2 = BufWriter::new(std::fs::File::create(out_2w)?);
    for (g, c) in &bi {
        writeln!(w2, "{g}\t{c}")?;
    }
    println!("counted {} unigrams, {} confusion-touching bigrams", uni.len(), bi.len());
    Ok(())
}

/// Download `url` to `dest` unless a non-empty file is already there (resumable).
fn fetch_if_absent(dest: &str, url: &str) -> Result<()> {
    if Path::new(dest).exists() && std::fs::metadata(dest)?.len() > 0 {
        println!("{dest} exists — skipping download (resume)");
    } else {
        run("curl", &["-sSL", "-o", dest, url])?;
        println!("fetched {dest}");
    }
    Ok(())
}

/// Parse `remap.awk`'s stdout into `(inflected, lemma, tag)` triples — tab-separated, exactly three
/// non-empty fields (awk occasionally emits a stray blank line).
fn parse_triples(text: &str) -> Vec<(String, String, String)> {
    text.lines().filter_map(split_triple).collect()
}

/// Read an LT supplement file (`added.txt`/`removed.txt`) into triples, skipping `#` comments + blanks.
fn read_triple_file(path: &str) -> Result<Vec<(String, String, String)>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    Ok(text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(split_triple)
        .collect())
}

/// Like [`read_triple_file`] but returns an empty vec if the file is absent (not every language ships
/// every supplement).
fn read_triple_file_opt(path: &str) -> Result<Vec<(String, String, String)>> {
    if Path::new(path).exists() {
        read_triple_file(path)
    } else {
        Ok(Vec::new())
    }
}

/// Split one tab-separated line into a non-empty `(inflected, lemma, tag)` triple, or `None`.
fn split_triple(line: &str) -> Option<(String, String, String)> {
    let mut f = line.split('\t');
    match (f.next(), f.next(), f.next()) {
        (Some(i), Some(l), Some(t)) if !i.is_empty() && !t.is_empty() => {
            Some((i.to_owned(), l.to_owned(), t.to_owned()))
        }
        _ => None,
    }
}

/// Run an external command, inheriting stdio, failing loudly on non-zero exit.
fn run(program: &str, args: &[&str]) -> Result<()> {
    println!("$ {program} {}", args.join(" "));
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("spawning `{program}` (is it installed?)"))?;
    if !status.success() {
        bail!("`{program}` exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nightly_matrix_parses_the_lang_list() {
        let yaml = "  strategy:\n    matrix:\n      lang: [en, de, ru, ar, fr, es, it]\n";
        assert_eq!(nightly_oracle_langs(yaml), ["en", "de", "ru", "ar", "fr", "es", "it"]);
    }

    #[test]
    fn nightly_matrix_absent_yields_empty() {
        assert!(nightly_oracle_langs("jobs:\n  build:\n").is_empty());
    }

    #[test]
    fn lang_codes_output_matches_the_canonical_list() {
        // The string `lang-codes` emits (and CI iterates) must be exactly the canonical codes.
        let codes = rlt_lang::LANGUAGES.iter().map(|c| c.code).collect::<Vec<_>>();
        assert_eq!(codes.join(" ").split_whitespace().collect::<Vec<_>>(), codes);
    }

    #[test]
    fn sparse_paths_cover_every_configured_language() {
        // The one in-binary const the checker can't make underivable: prove it stays in lockstep.
        for cfg in rlt_lang::LANGUAGES {
            assert!(
                SPARSE_PATHS.contains(&cfg.lt_sparse_path().as_str()),
                "SPARSE_PATHS missing the {} sparse-checkout path",
                cfg.code,
            );
        }
    }

    #[test]
    fn known_lists_exactly_the_canonical_codes() {
        let expected = rlt_lang::LANGUAGES.iter().map(|c| c.code).collect::<Vec<_>>().join(", ");
        assert_eq!(rlt_lang::known(), expected);
    }
}
