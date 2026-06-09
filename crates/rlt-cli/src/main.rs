//! `rlt` — the command-line surface over [`rlt_core`].
//!
//! Two subcommands: `check` (lint a file, print diagnostics) and `convert` (run the offline LT →
//! rkyv conversion, delegating to [`rlt_convert::convert`] so there is exactly one conversion
//! codepath shared with the standalone `rlt-convert` binary).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rlt_core::{Checker, Diagnostic};
use rlt_engine::VendoredEngine;

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
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Check { file } => run_check(&file),
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
    }
}

fn run_check(file: &std::path::Path) -> Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;

    let checker = Checker::new(VendoredEngine::new());
    let diagnostics = checker.check(&text);

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
