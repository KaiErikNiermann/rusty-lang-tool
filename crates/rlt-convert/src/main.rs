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

    // M0: fixed default paths; M1/M5 add real argument parsing.
    let lt_dir = PathBuf::from("resources/lt/en");
    let out = PathBuf::from("resources/en.rkyv");

    let report = rlt_convert::convert(&lt_dir, &out)?;
    tracing::info!(
        rules_total = report.rules_total,
        rules_opaque = report.rules_opaque,
        covered = format!("{:.1}%", report.covered_fraction() * 100.0),
        "conversion complete",
    );
    Ok(())
}
