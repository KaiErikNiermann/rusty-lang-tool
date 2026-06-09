//! Workspace task runner (the `cargo xtask` pattern).
//!
//! Keeps build orchestration in Rust rather than ad-hoc shell scripts:
//! - `fetch-lt` — resumable sparse checkout of just the English resources + XSD schemas at the
//!   pinned LanguageTool tag (NOT the 274 MB full tree).
//! - `build-blob` — run the offline converter to produce the runtime rkyv artifact.
//! - `build-wasm` — package the WASM surface via `wasm-pack` (Node target).
//! - `run-oracle` — run the `<example>` differential-oracle test suite.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

/// The LanguageTool release we track. Bump this and re-run `fetch-lt` to retarget; the converter's
/// schema codegen + the example oracle then report exactly what drifted.
const LT_VERSION: &str = "v6.7";
const LT_REPO: &str = "https://github.com/languagetool-org/languagetool.git";
const LT_DEST: &str = "resources/lt";

/// Sparse paths to pull from the LT monorepo: the core rule schemas + English language resources.
/// (Adjusted/verified for real in M1 when the converter first consumes them.)
const SPARSE_PATHS: &[&str] = &[
    "languagetool-core/src/main/resources/org/languagetool/rules",
    "languagetool-language-modules/en/src/main/resources/org/languagetool",
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
        Task::RunOracle => run("cargo", &["test", "-p", "rlt-core", "--", "--nocapture"]),
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
