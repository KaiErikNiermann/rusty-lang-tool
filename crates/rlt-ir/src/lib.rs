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
//! M1 lowers the rule *structure* (pattern → tokens/markers, filters → `Opaque`) and captures the
//! attributes needed for counting and serialization. The full token *matching semantics* (against
//! a tagged token graph) are built out in M4, driven by the example oracle.

#![forbid(unsafe_code)]

use rkyv::{Archive, Deserialize, Serialize};

/// A single compiled grammar rule: an ordered pattern plus its identity.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
pub struct Rule {
    /// Stable LT rule id (e.g. `"A_INFINITIVE"`); falls back to the enclosing group id for
    /// anonymous rules in a `<rulegroup>`. Used as the diagnostic's machine-readable code.
    pub id: String,
    /// The ordered sequence of pattern elements this rule matches against the token graph.
    pub pattern: Vec<Construct>,
}

impl Rule {
    /// Whether this rule depends on a `<filter>` (or otherwise unsupported) construct — i.e. its
    /// pattern contains an [`Construct::Opaque`] node anywhere.
    #[must_use]
    pub fn is_opaque(&self) -> bool {
        self.pattern.iter().any(Construct::is_opaque)
    }
}

/// One element of a rule's pattern. Known constructs get explicit variants; the `<filter>` escape
/// hatch and not-yet-lowered constructs land in [`Construct::Opaque`] (the coverage-metric tail).
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum Construct {
    /// A `<token>` matcher.
    Token(TokenPat),
    /// Opening boundary of a `<marker>…</marker>` (the span a diagnostic applies to). Markers
    /// delimit a contiguous run, so a flat start/end pair represents them without recursion.
    MarkerStart,
    /// Closing boundary of a `<marker>`. See [`Construct::MarkerStart`].
    MarkerEnd,
    /// A structurally-recognized construct whose matching semantics are not yet lowered
    /// (`<and>`, `<or>`, `<unify>`, `<phraseref>`, rule-level `<regexp>`). The `kind` is the LT
    /// element name, preserved so coverage gaps are named rather than silent.
    Unsupported {
        /// The LT element name this stands in for.
        kind: String,
    },
    /// The `<filter class="…" args="…">` escape hatch, or any construct deferred to a shim.
    /// Carrying the class + raw args keeps coverage countable and the rule shimmable later.
    Opaque {
        /// The Java filter class name (e.g. `"FindSuggestionsFilter"`).
        class: String,
        /// Raw, un-interpreted `args` attribute, preserved verbatim for a future shim.
        args: String,
    },
}

impl Construct {
    /// Whether this construct is the `<filter>`/unsupported escape hatch.
    #[must_use]
    pub fn is_opaque(&self) -> bool {
        matches!(self, Construct::Opaque { .. })
    }
}

/// A `<token>` pattern matcher: the attributes that select which token(s) it matches.
///
/// M1 captures the declarative attributes and the literal/regex text; M4 interprets them against
/// the engine's tagged token graph.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
pub struct TokenPat {
    /// The token's literal surface text, or — when [`regexp`](Self::regexp) is set — a regex over
    /// the surface form. `None` for tokens matched purely by POS tag.
    pub text: Option<String>,
    /// A part-of-speech constraint (LT tagset); may itself be a regex when the source token had
    /// `postag_regexp="yes"`.
    pub postag: Option<String>,
    /// `regexp="yes"`: [`text`](Self::text) is a regular expression, not a literal.
    pub regexp: bool,
    /// `negate="yes"`: the token matches when it does *not* satisfy the constraint.
    pub negate: bool,
    /// `inflected="yes"`: match any inflected form of [`text`](Self::text) as a lemma.
    pub inflected: bool,
    /// `min`: minimum number of consecutive tokens this element matches.
    pub min: Option<i32>,
    /// `max`: maximum number of consecutive tokens this element matches.
    pub max: Option<i32>,
    /// `skip`: how many tokens may be skipped before the next element must match.
    pub skip: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rkyv_round_trips_rules() {
        let rules = vec![Rule {
            id: "TEST_RULE".to_owned(),
            pattern: vec![
                Construct::MarkerStart,
                Construct::Token(TokenPat {
                    text: Some("colour".to_owned()),
                    postag: None,
                    regexp: false,
                    negate: false,
                    inflected: false,
                    min: None,
                    max: None,
                    skip: None,
                }),
                Construct::MarkerEnd,
                Construct::Opaque {
                    class: "FindSuggestionsFilter".to_owned(),
                    args: "field:foo".to_owned(),
                },
            ],
        }];

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&rules).expect("serialize");
        let back = rkyv::from_bytes::<Vec<Rule>, rkyv::rancor::Error>(&bytes).expect("deserialize");

        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "TEST_RULE");
        assert!(back[0].is_opaque(), "filter rule must count as opaque");
    }
}
