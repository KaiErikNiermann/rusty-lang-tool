//! Offline LanguageTool → `rlt-ir` converter.
//!
//! This is the piece the plan identifies as the heart of the project (and what rotted in nlprule).
//! The M1 build will: (1) generate typed XML structs from LT's `pattern.xsd` / `rules.xsd` /
//! `disambiguation.xsd` via `xsd-parser` in `build.rs`, (2) parse `grammar.xml` with `quick-xml`,
//! (3) lower into [`rlt_ir::Rule`]s — tagging filter/unsupported constructs as
//! [`rlt_ir::Construct::Opaque`] — and (4) serialize to a zero-copy `rkyv` blob.
//!
//! M0 establishes the entry point shared by the standalone `rlt-convert` binary and the
//! `rlt convert` CLI subcommand, plus the coverage-reporting shape.

#![forbid(unsafe_code)]

use std::path::Path;

use anyhow::Result;

/// Outcome of a conversion run: the counts the converter prints and the oracle later tracks.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConversionReport {
    /// Total rules parsed from the LT source.
    pub rules_total: usize,
    /// Rules whose pattern contains at least one [`rlt_ir::Construct::Opaque`] node.
    pub rules_opaque: usize,
}

impl ConversionReport {
    /// Fraction of rules fully represented by typed (non-`Opaque`) constructs, in `[0.0, 1.0]`.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "rule counts are in the thousands; far below f64's 2^52 exact-integer range"
    )]
    pub fn covered_fraction(&self) -> f64 {
        if self.rules_total == 0 {
            return 1.0;
        }
        let covered = self.rules_total - self.rules_opaque;
        covered as f64 / self.rules_total as f64
    }
}

/// Convert an unpacked LanguageTool language directory at `lt_dir` into an rkyv artifact at `out`.
///
/// M0: validates the entry-point shape and returns an empty report. M1 fills in schema codegen,
/// XML parsing, IR lowering and serialization.
///
/// # Errors
/// Returns an error if the LT source directory is missing or the artifact cannot be written.
pub fn convert(lt_dir: &Path, out: &Path) -> Result<ConversionReport> {
    tracing::info!(lt_dir = %lt_dir.display(), out = %out.display(), "conversion requested (M0 stub)");
    Ok(ConversionReport::default())
}
