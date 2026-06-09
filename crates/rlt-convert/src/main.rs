//! Standalone entry point for the offline converter. Thin wrapper over [`rlt_convert::convert`];
//! the same library function backs the `rlt convert` CLI subcommand (DRY: one codepath).

use std::path::PathBuf;

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Fixed default paths; the `rlt convert` CLI subcommand exposes overridable arguments.
    let grammar = PathBuf::from(rlt_convert::DEFAULT_GRAMMAR);
    let out = PathBuf::from(rlt_convert::DEFAULT_OUT);

    let report = rlt_convert::convert(&grammar, &out)?;
    tracing::info!(
        rules_total = report.rules_total,
        rules_opaque = report.rules_opaque,
        covered = format!("{:.1}%", report.covered_fraction() * 100.0),
        "conversion complete",
    );
    Ok(())
}
