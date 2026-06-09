//! Workspace task runner (the `cargo xtask` pattern).
//!
//! Keeps build orchestration in Rust rather than ad-hoc shell scripts:
//! - `fetch-lt` — resumable sparse checkout of just the English resources + XSD schemas at the
//!   pinned LanguageTool tag (NOT the 274 MB full tree).
//! - `gen-schema` — regenerate the committed `xsd-parser` bindings from LT's `rules.xsd`.
//! - `fetch-engine` — download nlprule's prebuilt English tokenizer/rules binaries (resumable).
//! - `build-blob` — run the offline converter to produce the runtime rkyv artifact.
//! - `build-wasm` — package the WASM surface via `wasm-pack` (Node target).
//! - `run-oracle` — run the `<example>` differential-oracle test suite.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use xsd_parser::config::{
    GeneratorFlags, InterpreterFlags, OptimizerFlags, ParserFlags, Resolver, Schema,
};
use xsd_parser::{Config, generate};

/// The LanguageTool release we track. Bump this and re-run `fetch-lt` + `gen-schema` to retarget;
/// the example oracle then reports exactly which rules drifted.
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
    /// Package the WASM surface with wasm-pack (Node target).
    BuildWasm,
    /// Run the differential `<example>` oracle test suite.
    RunOracle,
}

fn main() -> Result<()> {
    match Cli::parse().task {
        Task::FetchLt => fetch_lt(),
        Task::GenSchema => gen_schema(),
        Task::FetchEngine => fetch_engine(),
        Task::BuildBlob => run("cargo", &["run", "-p", "rlt-convert"]),
        Task::BuildWasm => run(
            "wasm-pack",
            &[
                "build",
                "crates/rlt-wasm",
                "--target",
                "nodejs",
                "--out-dir",
                "pkg",
            ],
        ),
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
    }
}

/// Resumable sparse checkout: skips the clone if the repo already exists, just refreshing the
/// working tree. Keeps only [`SPARSE_PATHS`] so the on-disk footprint is the English subset.
fn fetch_lt() -> Result<()> {
    let repo_dir = Path::new(LT_DEST).join("_repo");
    if repo_dir.join(".git").exists() {
        println!(
            "LT checkout exists at {} — refreshing (resume)",
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
                LT_VERSION,
                LT_REPO,
                repo_dir.to_str().context("non-utf8 path")?,
            ],
        )?;
    }

    let repo = repo_dir.to_str().context("non-utf8 path")?;
    let mut sparse = vec!["-C", repo, "sparse-checkout", "set"];
    sparse.extend_from_slice(SPARSE_PATHS);
    run("git", &sparse)?;
    run("git", &["-C", repo, "checkout", LT_VERSION])?;

    println!(
        "fetched LT {LT_VERSION} resources into {}",
        repo_dir.display()
    );
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
