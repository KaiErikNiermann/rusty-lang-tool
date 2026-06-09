//! Intermediate representation for LanguageTool rule constructs.
//!
//! This crate is the contract between the offline converter ([`rlt-convert`], which lowers LT's
//! `grammar.xml` / `disambiguation.xml` into these types) and the runtime ([`rlt-core`], which
//! walks them over a token graph). It is intentionally dependency-light: just the data shapes,
//! `serde` for debugging dumps, and `rkyv` for the zero-copy runtime artifact.
//!
//! # Design: the `Opaque` tail
//!
//! Every *known* LT construct is modelled as an explicit variant. The single [`Construct::Opaque`]
//! variant captures the `<filter class="…">` escape hatch (and any not-yet-supported construct),
//! so "what we cannot yet convert" is a *computed number* — the count of rules whose IR contains
//! an `Opaque` node — rather than a silent drop. The enums are `#[non_exhaustive]` and matched
//! exhaustively in the engine, so adding a construct is a compile error everywhere until handled.
//!
//! The real variant set is built out in milestone M1; this is the M0 skeleton establishing the
//! crate boundary and the `Opaque`-as-coverage-metric discipline.

#![forbid(unsafe_code)]

use rkyv::{Archive, Deserialize, Serialize};

/// A single compiled grammar rule: an ordered pattern plus the message/suggestions it emits.
///
/// Fleshed out in M1. The fields here are placeholders that establish the archived shape.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Rule {
    /// Stable LT rule id (e.g. `"A_INFINITIVE"`), used as the diagnostic's machine-readable code.
    pub id: String,
    /// The ordered sequence of pattern elements this rule matches against the token graph.
    pub pattern: Vec<Construct>,
}

/// One element of a rule's pattern. Known constructs get explicit variants; everything we cannot
/// yet represent declaratively lands in [`Construct::Opaque`] (the coverage-metric tail).
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum Construct {
    /// The `<filter class="…">` escape hatch, or any construct not yet lowered to a typed variant.
    /// Carrying the original class + raw args means coverage is countable and shimmable later.
    Opaque {
        /// The Java filter class name (or a synthetic tag for unsupported constructs).
        class: String,
        /// Raw, un-interpreted arguments, preserved verbatim for a future hand/LLM-written shim.
        args: Vec<(String, String)>,
    },
}

/// Count of rules whose pattern contains at least one [`Construct::Opaque`] node.
///
/// This is the headline coverage metric the converter prints and the oracle tracks.
#[must_use]
pub fn opaque_rule_count(rules: &[Rule]) -> usize {
    rules
        .iter()
        .filter(|r| {
            r.pattern
                .iter()
                .any(|c| matches!(c, Construct::Opaque { .. }))
        })
        .count()
}
