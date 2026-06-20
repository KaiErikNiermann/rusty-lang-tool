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
    /// Human-readable message shown when the rule fires. Inline `<suggestion>`/`<match>` are lowered
    /// to `\N` token backreferences (the runtime substitutes the matched surface when rendering).
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

// ── L3 confusion artifact codec ──────────────────────────────────────────────────────────────
//
// The confusion model is dominated by `bigrams` (≈170k `(u32,u32,u32)` triples for en); stored as
// rkyv's contiguous little-endian struct array it brotli'd at only ~2.9× (the worst of any artifact).
// rkyv buys nothing here at runtime — the loader fully deserializes into owned `Vec`s/`HashMap`s (see
// `ConfusionChecker::new`), never zero-copy `access` — so we use a compact custom encoding instead:
// columnar (struct-of-arrays) + delta on the sorted index columns + LEB128 varints throughout. That
// shrinks the brotli'd artifact ~22% (and proportionally more on the heavy-confusion languages).
//
// Format (little-endian, versioned): MAGIC, VERSION, then for each section a varint count followed by
// its columns. Sorted index columns are delta-encoded; counts are raw varints. The MAGIC lets a stale
// rkyv artifact (or fuzz garbage) be rejected cleanly instead of mis-parsed.

const CONFUSION_MAGIC: &[u8; 4] = b"RLTC";
const CONFUSION_VERSION: u8 = 1;

/// Malformed confusion artifact — wrapped into the `rkyv::rancor::Error` the public API already
/// returns, so callers are unchanged.
#[derive(Debug)]
struct ConfusionDecodeError(&'static str);

impl core::fmt::Display for ConfusionDecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "malformed confusion artifact: {}", self.0)
    }
}

impl core::error::Error for ConfusionDecodeError {}

/// Append `v` as an LEB128 unsigned varint.
fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_len(out: &mut Vec<u8>, len: usize) {
    write_varint(out, len as u64);
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    write_len(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

/// Encode a sorted `(idx, count)` array: idx column delta-encoded, count column raw — both varint.
fn write_unigrams(out: &mut Vec<u8>, rows: &[(u32, u32)]) {
    write_len(out, rows.len());
    let mut prev = 0u32;
    for &(idx, _) in rows {
        write_varint(out, u64::from(idx.wrapping_sub(prev)));
        prev = idx;
    }
    for &(_, count) in rows {
        write_varint(out, u64::from(count));
    }
}

/// Encode a `(a, b, count)` array sorted by `(a, b)`: column `a` delta-encoded; column `b`
/// delta-encoded *within* each equal-`a` run (it strictly increases there) and raw at run boundaries;
/// column `count` raw. Columnar so brotli sees three homogeneous streams.
fn write_triples(out: &mut Vec<u8>, rows: &[(u32, u32, u32)]) {
    write_len(out, rows.len());
    let mut prev_a = 0u32;
    for &(a, _, _) in rows {
        write_varint(out, u64::from(a.wrapping_sub(prev_a)));
        prev_a = a;
    }
    for (i, &(a, b, _)) in rows.iter().enumerate() {
        if i > 0 && rows[i - 1].0 == a {
            write_varint(out, u64::from(b.wrapping_sub(rows[i - 1].1)));
        } else {
            write_varint(out, u64::from(b));
        }
    }
    for &(_, _, count) in rows {
        write_varint(out, u64::from(count));
    }
}

/// Serialize a [`ConfusionModel`] to the compact columnar/varint artifact (see module note above).
#[must_use]
pub fn serialize_confusion(model: &ConfusionModel) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(CONFUSION_MAGIC);
    out.push(CONFUSION_VERSION);
    write_len(&mut out, model.pairs.len());
    for p in &model.pairs {
        write_str(&mut out, &p.a);
        write_str(&mut out, &p.b);
        out.extend_from_slice(&p.factor.to_le_bytes());
        out.push(u8::from(p.symmetric));
    }
    write_len(&mut out, model.vocab.len());
    for s in &model.vocab {
        write_str(&mut out, s);
    }
    write_unigrams(&mut out, &model.unigrams);
    write_triples(&mut out, &model.bigrams);
    write_triples(&mut out, &model.left_pos);
    write_triples(&mut out, &model.right_pos);
    out
}

/// Bounds-checked cursor over the artifact bytes — every read returns `Err` rather than panicking, so
/// arbitrary/fuzz input fails gracefully.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn byte(&mut self) -> Result<u8, ConfusionDecodeError> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or(ConfusionDecodeError("unexpected end of input"))?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ConfusionDecodeError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(ConfusionDecodeError("length overflow"))?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(ConfusionDecodeError("unexpected end of input"))?;
        self.pos = end;
        Ok(slice)
    }

    fn varint(&mut self) -> Result<u64, ConfusionDecodeError> {
        let mut result = 0u64;
        for i in 0..10u32 {
            let b = self.byte()?;
            result |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(result);
            }
        }
        Err(ConfusionDecodeError("varint too long"))
    }

    fn u32v(&mut self) -> Result<u32, ConfusionDecodeError> {
        u32::try_from(self.varint()?).map_err(|_| ConfusionDecodeError("u32 out of range"))
    }

    /// A length, capped at `remaining()` so a corrupt huge count can't trigger a giant pre-allocation
    /// (each element consumes ≥1 byte, so the true count never exceeds the bytes left).
    fn count(&mut self) -> Result<usize, ConfusionDecodeError> {
        let n =
            usize::try_from(self.varint()?).map_err(|_| ConfusionDecodeError("count overflow"))?;
        Ok(n)
    }

    fn cap(&self, n: usize) -> usize {
        n.min(self.remaining())
    }

    fn f32(&mut self) -> Result<f32, ConfusionDecodeError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn string(&mut self) -> Result<String, ConfusionDecodeError> {
        let n = self.count()?;
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ConfusionDecodeError("invalid utf-8"))
    }
}

fn read_unigrams(r: &mut Reader<'_>) -> Result<Vec<(u32, u32)>, ConfusionDecodeError> {
    let n = r.count()?;
    let mut idx = Vec::with_capacity(r.cap(n));
    let mut prev = 0u32;
    for _ in 0..n {
        prev = prev.wrapping_add(r.u32v()?);
        idx.push(prev);
    }
    let mut out = Vec::with_capacity(r.cap(n));
    for &i in &idx {
        out.push((i, r.u32v()?));
    }
    Ok(out)
}

fn read_triples(reader: &mut Reader<'_>) -> Result<Vec<(u32, u32, u32)>, ConfusionDecodeError> {
    let n = reader.count()?;
    let mut col_a = Vec::with_capacity(reader.cap(n));
    let mut prev = 0u32;
    for _ in 0..n {
        prev = prev.wrapping_add(reader.u32v()?);
        col_a.push(prev);
    }
    let mut col_b: Vec<u32> = Vec::with_capacity(reader.cap(n));
    for i in 0..n {
        let delta = reader.u32v()?;
        let value = if i > 0 && col_a[i] == col_a[i - 1] {
            col_b[i - 1].wrapping_add(delta)
        } else {
            delta
        };
        col_b.push(value);
    }
    let mut out = Vec::with_capacity(reader.cap(n));
    for i in 0..n {
        out.push((col_a[i], col_b[i], reader.u32v()?));
    }
    Ok(out)
}

fn decode_confusion(bytes: &[u8]) -> Result<ConfusionModel, ConfusionDecodeError> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != CONFUSION_MAGIC || r.byte()? != CONFUSION_VERSION {
        return Err(ConfusionDecodeError("bad magic or version"));
    }
    let n_pairs = r.count()?;
    let mut pairs = Vec::with_capacity(r.cap(n_pairs));
    for _ in 0..n_pairs {
        pairs.push(ConfusionPair {
            a: r.string()?,
            b: r.string()?,
            factor: r.f32()?,
            symmetric: r.byte()? != 0,
        });
    }
    let n_vocab = r.count()?;
    let mut vocab = Vec::with_capacity(r.cap(n_vocab));
    for _ in 0..n_vocab {
        vocab.push(r.string()?);
    }
    Ok(ConfusionModel {
        pairs,
        vocab,
        unigrams: read_unigrams(&mut r)?,
        bigrams: read_triples(&mut r)?,
        left_pos: read_triples(&mut r)?,
        right_pos: read_triples(&mut r)?,
    })
}

/// Deserialize a [`ConfusionModel`] from its compact columnar/varint artifact (see [`serialize_confusion`]).
///
/// # Errors
/// Returns an error if `bytes` is not a valid confusion artifact (bad magic/version, truncated, or
/// malformed) — never panics, so it is safe on untrusted input.
pub fn deserialize_confusion(bytes: &[u8]) -> Result<ConfusionModel, rkyv::rancor::Error> {
    decode_confusion(bytes).map_err(rkyv::rancor::Source::new)
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

    fn sample_confusion() -> ConfusionModel {
        ConfusionModel {
            pairs: vec![
                ConfusionPair {
                    a: "their".to_owned(),
                    b: "there".to_owned(),
                    factor: 10.0,
                    symmetric: true,
                },
                ConfusionPair {
                    a: "affect".to_owned(),
                    b: "effect".to_owned(),
                    factor: 2.5,
                    symmetric: false,
                },
            ],
            vocab: vec!["the".to_owned(), "their".to_owned(), "there".to_owned()],
            unigrams: vec![(0, 9000), (1, 42), (2, 37)],
            // Sorted by (a, b); two share a=0 (exercises within-run b delta) and one starts a new run.
            bigrams: vec![(0, 1, 5), (0, 2, 8), (1, 2, 3)],
            left_pos: vec![(0, 0, 1), (0, 5, 2), (3, 1, 7)],
            right_pos: vec![(1, 4, 6)],
        }
    }

    fn assert_confusion_eq(a: &ConfusionModel, b: &ConfusionModel) {
        assert_eq!(a.vocab, b.vocab);
        assert_eq!(a.unigrams, b.unigrams);
        assert_eq!(a.bigrams, b.bigrams);
        assert_eq!(a.left_pos, b.left_pos);
        assert_eq!(a.right_pos, b.right_pos);
        assert_eq!(a.pairs.len(), b.pairs.len());
        for (x, y) in a.pairs.iter().zip(&b.pairs) {
            assert_eq!(
                (&x.a, &x.b, x.factor, x.symmetric),
                (&y.a, &y.b, y.factor, y.symmetric)
            );
        }
    }

    #[test]
    fn confusion_round_trips() {
        let model = sample_confusion();
        let bytes = serialize_confusion(&model);
        let back = deserialize_confusion(&bytes).expect("deserialize");
        assert_confusion_eq(&model, &back);
    }

    #[test]
    fn confusion_round_trips_empty() {
        let model = ConfusionModel {
            pairs: vec![],
            vocab: vec![],
            unigrams: vec![],
            bigrams: vec![],
            left_pos: vec![],
            right_pos: vec![],
        };
        let bytes = serialize_confusion(&model);
        assert_confusion_eq(&model, &deserialize_confusion(&bytes).expect("deserialize"));
    }

    #[test]
    fn confusion_rejects_garbage_without_panicking() {
        // Bad magic, truncation, and arbitrary bytes must all error cleanly (fuzz contract).
        assert!(deserialize_confusion(b"").is_err());
        assert!(deserialize_confusion(b"not-an-artifact").is_err());
        assert!(deserialize_confusion(b"RLTC\x01\xff\xff\xff").is_err());
        let good = serialize_confusion(&sample_confusion());
        for cut in 0..good.len() {
            // Every truncation of a valid artifact must error, never panic.
            let _ = deserialize_confusion(&good[..cut]);
        }
    }
}
