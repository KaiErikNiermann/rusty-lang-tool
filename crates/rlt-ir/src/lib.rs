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

/// A single compiled grammar rule: an ordered pattern plus the message/corrections it emits.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Rule {
    /// Stable LT rule id (e.g. `"A_INFINITIVE"`); falls back to the enclosing group id for
    /// anonymous rules in a `<rulegroup>`. Used as the diagnostic's machine-readable code.
    pub id: String,
    /// The ordered sequence of pattern elements this rule matches against the token graph.
    pub pattern: Vec<Construct>,
    /// `<antipattern>`s: token sequences that, when one matches overlapping the rule's match,
    /// suppress the rule (LT's exception-by-context mechanism). Each is its own construct list.
    /// Includes the enclosing `<rulegroup>`'s antipatterns, which apply to every member rule.
    pub antipatterns: Vec<Vec<Construct>>,
    /// Human-readable message shown when the rule fires (plain text; embedded markup dropped).
    pub message: String,
    /// Correction templates rendered against the matched tokens to produce replacements.
    pub suggestions: Vec<Suggestion>,
}

/// A correction template: an ordered sequence of literal text and back-references to matched
/// tokens, rendered into a replacement string when the rule fires.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Suggestion {
    /// The parts concatenated (after rendering token references) to form the replacement.
    pub parts: Vec<SugPart>,
}

/// One piece of a [`Suggestion`].
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub enum SugPart {
    /// Literal text.
    Text(String),
    /// `<match no="N"/>` — copy the Nth matched pattern token's surface form (1-indexed over the
    /// pattern's tokens), applying `case` (and an optional regex substitution first).
    Token {
        /// 1-based index into the pattern's tokens.
        no: usize,
        /// Case transform applied to the copied surface.
        case: Case,
        /// `(regexp_match, regexp_replace)` applied to the copied surface before `case`, if any.
        transform: Option<(String, String)>,
    },
}

/// Case transform applied when rendering a [`SugPart::Token`] (LT `case_conversion`).
#[derive(
    Debug, Clone, Copy, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum Case {
    /// Copy verbatim.
    Keep,
    /// Upper-case the whole token.
    Upper,
    /// Lower-case the whole token.
    Lower,
    /// Upper-case the first character only.
    StartUpper,
}

impl Rule {
    /// Whether this rule depends on a `<filter>` (or otherwise unsupported) construct — i.e. its
    /// pattern contains an [`Construct::Opaque`] node anywhere.
    #[must_use]
    pub fn is_opaque(&self) -> bool {
        self.pattern.iter().any(Construct::is_opaque)
    }
}

/// Copy possibly-unaligned `bytes` into a 16-byte-aligned buffer.
///
/// rkyv's validated `from_bytes` requires the buffer to meet the archive's alignment, but a `&[u8]`
/// from `std::fs::read`, a JS/wasm buffer, or a sub-slice only guarantees byte alignment. Production
/// allocators over-align large buffers and hide this; a mis-aligned slice (or Miri's minimal-alignment
/// allocator) surfaces it as an "unaligned pointer" error. Every loader routes through here so loading
/// is correct regardless of the source allocation.
#[must_use]
pub fn align_bytes(bytes: &[u8]) -> rkyv::util::AlignedVec<16> {
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
    aligned.extend_from_slice(bytes);
    aligned
}

/// Deserialize a `Vec<Rule>` from the rkyv artifact the converter produced.
///
/// # Errors
/// Returns an error if `bytes` is not a valid archived `Vec<Rule>`.
pub fn deserialize_rules(bytes: &[u8]) -> Result<Vec<Rule>, rkyv::rancor::Error> {
    rkyv::from_bytes::<Vec<Rule>, rkyv::rancor::Error>(&align_bytes(bytes))
}

/// The L3 confusion-pair model: easily-confused word pairs plus the pruned n-gram counts used to
/// pick the contextually-more-probable member (real-word error detection, e.g. their/there).
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct ConfusionModel {
    /// Easily-confused word pairs (from LanguageTool's `confusion_sets.txt`).
    pub pairs: Vec<ConfusionPair>,
    /// Interned string table for the count tables below: every word (lower-cased) and POS tag is
    /// stored once here and referenced by its `u32` index elsewhere — the same side-table trick the
    /// tagger uses. Ordered by descending reference frequency, so the hottest tokens get the
    /// smallest indices (mostly-zero `u32`s ⇒ the artifact gzips well).
    pub vocab: Vec<String>,
    /// Unigram counts as `(word_idx, count)` — context-free backoff for confusion words.
    pub unigrams: Vec<(u32, u32)>,
    /// Bigram counts as `(w1_idx, w2_idx, count)`, pruned to those touching a confusion word.
    /// Sorted by `(w1_idx, w2_idx)` so a count can be found by binary search without a hash map.
    pub bigrams: Vec<(u32, u32, u32)>,
    /// Left-POS context as `(pos_idx, member_idx, count)`: summed count of bigrams whose left word
    /// has that primary POS and whose right word is the confusion member. Sorted; generalises
    /// sparse word bigrams.
    pub left_pos: Vec<(u32, u32, u32)>,
    /// Right-POS context as `(member_idx, pos_idx, count)`: summed count of bigrams whose left word
    /// is the member and whose right word has that primary POS. Sorted.
    pub right_pos: Vec<(u32, u32, u32)>,
}

/// One easily-confused pair. `symmetric` pairs are checked both ways; directional ones only `a→b`.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct ConfusionPair {
    /// The first (or, for directional pairs, the "from") word — lower-cased.
    pub a: String,
    /// The second (or "to") word — lower-cased.
    pub b: String,
    /// LanguageTool's confidence factor: how much more probable the alternative must be.
    pub factor: f32,
    /// Whether the pair is bidirectional (`a; b`) rather than directional (`a -> b`).
    pub symmetric: bool,
}

/// Deserialize a [`ConfusionModel`] from its rkyv artifact.
///
/// # Errors
/// Returns an error if `bytes` is not a valid archived [`ConfusionModel`].
pub fn deserialize_confusion(bytes: &[u8]) -> Result<ConfusionModel, rkyv::rancor::Error> {
    rkyv::from_bytes::<ConfusionModel, rkyv::rancor::Error>(&align_bytes(bytes))
}

/// One element of a rule's pattern. Known constructs get explicit variants; the `<filter>` escape
/// hatch and not-yet-lowered constructs land in [`Construct::Opaque`] (the coverage-metric tail).
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub enum Construct {
    /// A `<token>` matcher.
    Token(TokenPat),
    /// An `<or>` group: one token position that matches if **any** alternative matches.
    Or(Vec<TokenPat>),
    /// An `<and>` group: one token position that matches if **all** constraints hold on it.
    And(Vec<TokenPat>),
    /// Opening boundary of a `<marker>…</marker>` (the span a diagnostic applies to). Markers
    /// delimit a contiguous run, so a flat start/end pair represents them without recursion.
    MarkerStart,
    /// Closing boundary of a `<marker>`. See [`Construct::MarkerStart`].
    MarkerEnd,
    /// A rule-level `<regexp>`: the whole rule matches a regex over the sentence text (rather than
    /// the token sequence). `mark` is the 1-based capture group delimiting the error span (the whole
    /// match when `None`); suggestions reference capture groups by `\N`.
    Regexp {
        /// The regular expression source.
        pattern: String,
        /// The capture group to mark as the error span (1-based), or the whole match.
        mark: Option<usize>,
        /// `case_sensitive="yes"`.
        case_sensitive: bool,
    },
    /// A structurally-recognized construct whose matching semantics are not yet lowered
    /// (`<unify>`, `<phraseref>` to an undefined phrase). The `kind` is the LT element name,
    /// preserved so coverage gaps are named rather than silent.
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
#[derive(
    Debug, Clone, Default, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize,
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "fields mirror LT's token attributes 1:1"
)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct TokenPat {
    /// The token's literal surface text, or — when [`regexp`](Self::regexp) is set — a regex over
    /// the surface form. `None` for tokens matched purely by POS tag.
    pub text: Option<String>,
    /// A part-of-speech constraint (LT tagset); a regex when [`postag_regexp`](Self::postag_regexp)
    /// is set, otherwise a literal tag.
    pub postag: Option<String>,
    /// `regexp="yes"`: [`text`](Self::text) is a regular expression, not a literal.
    pub regexp: bool,
    /// `postag_regexp="yes"`: [`postag`](Self::postag) is a regular expression, not a literal tag.
    pub postag_regexp: bool,
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
    /// `case_sensitive="yes"`: match [`text`](Self::text) case-sensitively (default is insensitive).
    pub case_sensitive: bool,
    /// `spacebefore`: whether whitespace must (`Some(true)`) or must not (`Some(false)`) precede this
    /// token; `None` (LT's `ignore`, the default) places no constraint. Drives spacing rules like
    /// `ESPACIO_DESPUES_DE_PUNTO` — without it they fire on already-correct text and emit no-op fixes.
    pub space_before: Option<bool>,
    /// `<exception>` children: the token does *not* match if any exception matches it.
    pub exceptions: Vec<ExceptionPat>,
}

/// A `<token>`'s `<exception>`: a lighter token-like matcher that, when it matches the candidate,
/// disqualifies the enclosing token from matching.
#[derive(
    Debug, Clone, Default, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize,
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "fields mirror LT's exception attributes 1:1"
)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct ExceptionPat {
    /// Literal surface text, or a regex when [`regexp`](Self::regexp) is set.
    pub text: Option<String>,
    /// A part-of-speech constraint; a regex when [`postag_regexp`](Self::postag_regexp) is set.
    pub postag: Option<String>,
    /// `regexp="yes"`: [`text`](Self::text) is a regular expression.
    pub regexp: bool,
    /// `postag_regexp="yes"`: [`postag`](Self::postag) is a regular expression, not a literal tag.
    pub postag_regexp: bool,
    /// `inflected="yes"`: match [`text`](Self::text) against the candidate's lemmas.
    pub inflected: bool,
    /// `negate="yes"`: the exception is satisfied when it does *not* match.
    pub negate: bool,
    /// `case_sensitive="yes"`: match text case-sensitively.
    pub case_sensitive: bool,
}

/// A disambiguation rule: a pattern (with an optional `<marker>` delimiting the affected tokens) plus
/// the tag action to apply to the marked tokens when it matches. Lowered from `disambiguation.xml`,
/// which uses the same pattern vocabulary as `grammar.xml`. Run after tagging, before the L2 matcher,
/// to narrow/fix the over-generated raw-lexicon tags the grammar rules then key on.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct DisambigRule {
    /// LT rule id (or enclosing group id), for debugging.
    pub id: String,
    /// The ordered pattern, with `MarkerStart`/`MarkerEnd` bounding the tokens the action mutates
    /// (the whole match when there is no marker).
    pub pattern: Vec<Construct>,
    /// `<antipattern>`s: if any matches overlapping the rule's match, the action is suppressed (LT's
    /// exception-by-context mechanism). Without these a disambig rule over-applies — mutating tags in
    /// contexts the antipattern was meant to carve out.
    pub antipatterns: Vec<Vec<Construct>>,
    /// What to do to the marked tokens' tags/lemmas on a match.
    pub action: TagAction,
}

/// What a matched [`DisambigRule`] does to the marked tokens. Operates on the token's flattened,
/// deduplicated `tags`/`lemmas` lists (the engine models analyses as separate tag + lemma lists, not
/// paired readings), which captures the disambiguation effect the L2 matcher keys on.
#[derive(Debug, Clone, Archive, Serialize, Deserialize, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub enum TagAction {
    /// `action="replace"` (LT's default): replace the marked tokens' readings with these.
    Replace {
        /// Postags to set.
        postags: Vec<String>,
        /// Lemmas to set (empty = leave the surface-derived lemmas).
        lemmas: Vec<String>,
    },
    /// `action="add"`: add these as additional readings.
    Add {
        /// Postags to add.
        postags: Vec<String>,
        /// Lemmas to add.
        lemmas: Vec<String>,
    },
    /// `action="remove"`: drop readings whose postag (or lemma) matches one of these.
    Remove {
        /// Postags to remove (regex patterns when `postag_regexp`).
        postags: Vec<String>,
        /// Lemmas to remove.
        lemmas: Vec<String>,
        /// Whether `postags` are regexes (`postag_regexp="yes"`).
        postag_regexp: bool,
    },
    /// `action="filter"`: keep only the postags matching one of these patterns.
    Filter {
        /// Postag patterns to keep (regex when `postag_regexp`).
        postags: Vec<String>,
        /// Whether `postags` are regexes (`postag_regexp="yes"`).
        postag_regexp: bool,
    },
    /// `action="unify"/"filterall"/"ignore_spelling"`, `<match>` postag synthesis, or a `chunk_re`
    /// token (no chunker) — recognized but not applied. The rule is kept (named) but inert.
    Unsupported,
}

impl TagAction {
    /// Whether this action is recognized but not applied (the coverage tail).
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(self, TagAction::Unsupported)
    }
}

/// Deserialize a `Vec<DisambigRule>` from its rkyv artifact.
///
/// # Errors
/// Returns an error if `bytes` is not a valid archived `Vec<DisambigRule>`.
pub fn deserialize_disambig(bytes: &[u8]) -> Result<Vec<DisambigRule>, rkyv::rancor::Error> {
    rkyv::from_bytes::<Vec<DisambigRule>, rkyv::rancor::Error>(&align_bytes(bytes))
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
                    ..Default::default()
                }),
                Construct::MarkerEnd,
                Construct::Opaque {
                    class: "FindSuggestionsFilter".to_owned(),
                    args: "field:foo".to_owned(),
                },
            ],
            antipatterns: vec![vec![Construct::Token(TokenPat {
                text: Some("colour".to_owned()),
                ..Default::default()
            })]],
            message: "Use the American spelling.".to_owned(),
            suggestions: vec![Suggestion {
                parts: vec![SugPart::Text("color".to_owned())],
            }],
        }];

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&rules).expect("serialize");
        let back = rkyv::from_bytes::<Vec<Rule>, rkyv::rancor::Error>(&bytes).expect("deserialize");

        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "TEST_RULE");
        assert!(back[0].is_opaque(), "filter rule must count as opaque");
    }
}
