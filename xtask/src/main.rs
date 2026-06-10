//! Workspace task runner (the `cargo xtask` pattern).
//!
//! Keeps build orchestration in Rust rather than ad-hoc shell scripts:
//! - `fetch-lt` — resumable sparse checkout of just the English resources + XSD schemas at the
//!   pinned LanguageTool tag (NOT the 274 MB full tree).
//! - `gen-schema` — regenerate the committed `xsd-parser` bindings from LT's `rules.xsd`.
//! - `fetch-engine` — download nlprule's prebuilt English tokenizer/rules binaries (resumable).
//! - `build-blob` — run the offline converter to produce the runtime rkyv artifact.
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

/// Sparse paths to pull from the LT monorepo: the core rule schemas + English language resources.
const SPARSE_PATHS: &[&str] = &[
    "languagetool-core/src/main/resources/org/languagetool/rules",
    "languagetool-language-modules/en/src/main/resources/org/languagetool",
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
    /// Run the offline converter to (re)build the runtime rkyv artifact.
    BuildBlob,
    /// Download Norvig's n-gram subsets for the L3 confusion model (resumable).
    FetchNgrams,
    /// Build the L3 confusion model from LT's confusion sets + the n-gram subsets.
    BuildConfusion,
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
}

fn main() -> Result<()> {
    match Cli::parse().task {
        Task::FetchLt => fetch_lt(&lt_version()),
        Task::GenSchema => gen_schema(),
        Task::FetchEngine => fetch_engine(),
        Task::BuildBlob => run("cargo", &["run", "-p", "rlt-convert"]),
        Task::FetchNgrams => fetch_ngrams(),
        Task::BuildConfusion => run("cargo", &["run", "-p", "rlt-cli", "--", "build-confusion"]),
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
