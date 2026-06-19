//! Offline LanguageTool → `rlt-ir` converter.
//!
//! The piece the plan identifies as the heart of the project (and what rotted in nlprule). The
//! pipeline:
//! 1. **Preprocess** — expand LT's internal-DTD general entities and strip the DOCTYPE so the XML
//!    is standalone (quick_xml does not expand DTD entities).
//! 2. **Deserialize** — parse `grammar.xml` into the typed tree generated from LT's own schemas
//!    (`src/lt_schema.rs`, produced by `cargo xtask gen-schema`).
//! 3. **Lower** — walk categories → rule(group)s into [`rlt_ir::Rule`]s, mapping `<filter>` (and
//!    not-yet-supported constructs) to [`rlt_ir::Construct::Opaque`] so coverage is countable.
//! 4. **Serialize** — write the IR as a zero-copy `rkyv` blob the runtime views without parsing.
//!
//! The full token *matching semantics* are built out in M4; M1 establishes the structural spine
//! and the headline coverage metric.

#![forbid(unsafe_code)]

mod disambig;
mod lt_schema;
mod morfologik;

pub use disambig::{
    DEFAULT_DISAMBIGUATION, DisambigReport, convert_disambiguation, lower_disambiguation,
};
pub use morfologik::{DictMeta, Encoder, parse_info, read_triples};

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use rlt_ir::{Case, Construct, ExceptionPat, Rule, SugPart, Suggestion, TokenPat};
use xsd_parser_types::quick_xml::{DeserializeSync, SliceReader, XmlReader};

use lt_schema::{pattern, rules};

/// Default location of LT's English `grammar.xml` after `cargo xtask fetch-lt`.
pub const DEFAULT_GRAMMAR: &str = "resources/lt/_repo/languagetool-language-modules/en/src/main/resources/org/languagetool/rules/en/grammar.xml";
/// Default output path for the compiled runtime artifact (English; see `rlt_lang` for per-language).
pub const DEFAULT_OUT: &str = "resources/en/grammar.rkyv";
/// Default location of LanguageTool's English confusion sets after `cargo xtask fetch-lt`.
pub const DEFAULT_CONFUSION_SETS: &str = "resources/lt/_repo/languagetool-language-modules/en/src/main/resources/org/languagetool/resource/en/confusion_sets.txt";
/// Default location of Norvig's unigram counts after `cargo xtask fetch-ngrams`.
pub const DEFAULT_UNIGRAMS: &str = "resources/ngrams/count_1w.txt";
/// Default location of Norvig's bigram counts after `cargo xtask fetch-ngrams`.
pub const DEFAULT_BIGRAMS: &str = "resources/ngrams/count_2w.txt";
/// Default output path for the compiled L3 confusion model (English).
pub const DEFAULT_CONFUSION_OUT: &str = "resources/en/confusion.rkyv";

/// Counts from building the L3 confusion model.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConfusionReport {
    /// Easily-confused pairs parsed from `confusion_sets.txt`.
    pub pairs: usize,
    /// Bigrams retained after pruning to confusion words.
    pub bigrams: usize,
}

/// Build the L3 confusion model: parse LanguageTool's confusion sets, prune Norvig's unigram and
/// bigram counts to those touching a confusion word, and serialize to an rkyv artifact at `out`.
///
/// # Errors
/// Returns an error if any input file is missing/unreadable or the artifact cannot be written.
pub fn build_confusion_model(
    confusion_sets: &Path,
    unigrams: &Path,
    bigrams: &Path,
    out: &Path,
    pos_tags: impl Fn(&str) -> Vec<String>,
) -> Result<ConfusionReport> {
    let pairs = parse_confusion_sets(confusion_sets)?;

    // The set of confusion words to prune the n-grams against.
    let words: std::collections::HashSet<String> = pairs
        .iter()
        .flat_map(|p| [p.a.clone(), p.b.clone()])
        .collect();

    let unigrams = prune_unigrams(unigrams, &words)
        .with_context(|| format!("reading {}", unigrams.display()))?;
    let Bigrams {
        counts: bigrams,
        left_pos,
        right_pos,
    } = prune_bigrams(bigrams, &words, &pos_tags)
        .with_context(|| format!("reading {}", bigrams.display()))?;

    let report = ConfusionReport {
        pairs: pairs.len(),
        bigrams: bigrams.len(),
    };
    let model = intern_confusion(pairs, &unigrams, &bigrams, &left_pos, &right_pos);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&model)
        .map_err(|e| anyhow!("rkyv serialize: {e}"))?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
    tracing::info!(pairs = report.pairs, bigrams = report.bigrams, out = %out.display(), "wrote confusion model");
    Ok(report)
}

/// Intern every word/POS string from the count tables into a single frequency-ranked `vocab`, then
/// re-express each count entry as `u32` indices into it (the tagger's side-table trick). Hot tokens
/// get the lowest indices and each table is sorted, so the artifact gzips well and the runtime can
/// binary-search bigrams without a string-keyed hash map.
fn intern_confusion(
    pairs: Vec<rlt_ir::ConfusionPair>,
    unigrams: &[(String, u32)],
    bigrams: &[(String, String, u32)],
    left_pos: &[(String, String, u32)],
    right_pos: &[(String, String, u32)],
) -> rlt_ir::ConfusionModel {
    use std::collections::HashMap;
    let mut freq: HashMap<&str, u64> = HashMap::new();
    for (w, _) in unigrams {
        *freq.entry(w.as_str()).or_default() += 1;
    }
    for (a, b, _) in bigrams.iter().chain(left_pos).chain(right_pos) {
        *freq.entry(a.as_str()).or_default() += 1;
        *freq.entry(b.as_str()).or_default() += 1;
    }

    let mut vocab: Vec<String> = freq.keys().map(|s| (*s).to_owned()).collect();
    vocab.sort_unstable_by(|a, b| {
        freq[b.as_str()]
            .cmp(&freq[a.as_str()])
            .then_with(|| a.cmp(b))
    });
    let idx: HashMap<&str, u32> = vocab
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), u32::try_from(i).unwrap_or(u32::MAX)))
        .collect();

    let mut unigrams: Vec<(u32, u32)> = unigrams
        .iter()
        .map(|(w, c)| (idx[w.as_str()], *c))
        .collect();
    unigrams.sort_unstable();
    let intern3 = |t: &[(String, String, u32)]| -> Vec<(u32, u32, u32)> {
        let mut v: Vec<(u32, u32, u32)> = t
            .iter()
            .map(|(a, b, c)| (idx[a.as_str()], idx[b.as_str()], *c))
            .collect();
        v.sort_unstable();
        v
    };
    rlt_ir::ConfusionModel {
        pairs,
        unigrams,
        bigrams: intern3(bigrams),
        left_pos: intern3(left_pos),
        right_pos: intern3(right_pos),
        vocab,
    }
}

/// Parse `confusion_sets.txt` into [`rlt_ir::ConfusionPair`]s. Lines are `a; b; factor` (symmetric)
/// or `a -> b; factor` (directional), with `#` comments; multi-word entries are skipped.
fn parse_confusion_sets(path: &Path) -> Result<Vec<rlt_ir::ConfusionPair>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut pairs = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let symmetric = !line.contains("->");
        let fields: Vec<String> = line
            .replace("->", ";")
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let [a, b, factor] = &fields[..] else {
            continue;
        };
        if a.contains(' ') || b.contains(' ') {
            continue; // single-word confusion pairs only
        }
        let Ok(factor) = factor.parse::<f32>() else {
            continue;
        };
        pairs.push(rlt_ir::ConfusionPair {
            a: a.to_ascii_lowercase(),
            b: b.to_ascii_lowercase(),
            factor,
            symmetric,
        });
    }
    Ok(pairs)
}

/// Keep `word\tcount` unigram lines whose word is a confusion word (lower-cased).
fn prune_unigrams(
    path: &Path,
    words: &std::collections::HashSet<String>,
) -> Result<Vec<(String, u32)>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((w, c)) = line.split_once('\t') else {
            continue;
        };
        let w = w.to_ascii_lowercase();
        if words.contains(&w) {
            if let Ok(count) = c.trim().parse::<u32>() {
                out.push((w, count));
            }
        }
    }
    Ok(out)
}

/// The pruned bigrams plus the POS-context aggregations derived from them, as un-interned string
/// components (`build_confusion_model` interns them into the artifact's shared vocab).
struct Bigrams {
    /// `(w1, w2, count)`.
    counts: Vec<(String, String, u32)>,
    /// `(left_pos, member, count)`.
    left_pos: Vec<(String, String, u32)>,
    /// `(member, right_pos, count)`.
    right_pos: Vec<(String, String, u32)>,
}

/// Keep `"w1 w2"\tcount` bigram lines touching a confusion word, and aggregate POS-context counts:
/// when the member is on the right, accumulate the left word's primary POS; when on the left,
/// the right word's primary POS. `pos_tags` is cached so each neighbour is tagged once.
fn prune_bigrams(
    path: &Path,
    words: &std::collections::HashSet<String>,
    pos_tags: &impl Fn(&str) -> Vec<String>,
) -> Result<Bigrams> {
    use std::collections::HashMap;
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    let mut left_pos: HashMap<(String, String), u32> = HashMap::new();
    let mut right_pos: HashMap<(String, String), u32> = HashMap::new();
    let mut pos_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut primary_pos = |w: &str| -> Option<String> {
        pos_cache
            .entry(w.to_owned())
            .or_insert_with(|| pos_tags(w).into_iter().next())
            .clone()
    };

    for line in text.lines() {
        let Some((gram, c)) = line.split_once('\t') else {
            continue;
        };
        let gram = gram.to_ascii_lowercase();
        let Some((w1, w2)) = gram.split_once(' ') else {
            continue;
        };
        let (w1, w2) = (w1.to_owned(), w2.to_owned());
        if !(words.contains(&w1) || words.contains(&w2)) {
            continue;
        }
        let Ok(count) = c.trim().parse::<u32>() else {
            continue;
        };
        if words.contains(&w2) {
            if let Some(p1) = primary_pos(&w1) {
                let e = left_pos.entry((p1, w2.clone())).or_default();
                *e = e.saturating_add(count);
            }
        }
        if words.contains(&w1) {
            if let Some(p2) = primary_pos(&w2) {
                let e = right_pos.entry((w1.clone(), p2)).or_default();
                *e = e.saturating_add(count);
            }
        }
        out.push((w1, w2, count));
    }
    Ok(Bigrams {
        counts: out,
        left_pos: left_pos.into_iter().map(|((p, m), c)| (p, m, c)).collect(),
        right_pos: right_pos.into_iter().map(|((m, p), c)| (m, p, c)).collect(),
    })
}

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

/// Convert a LanguageTool `grammar.xml` at `grammar_path` into an rkyv artifact at `out`.
///
/// # Errors
/// Returns an error if the grammar file is missing, fails to parse, or the artifact cannot be
/// serialized or written.
pub fn convert(grammar_path: &Path, out: &Path) -> Result<ConversionReport> {
    let doc = parse_grammar(grammar_path)?;
    let rules = lower_document(&doc);
    let rules_opaque = rules.iter().filter(|r| r.is_opaque()).count();
    let report = ConversionReport {
        rules_total: rules.len(),
        rules_opaque,
    };

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&rules)
        .map_err(|e| anyhow!("rkyv serialize: {e}"))?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    tracing::info!(
        rules = report.rules_total,
        opaque = report.rules_opaque,
        bytes = bytes.len(),
        out = %out.display(),
        "wrote rkyv artifact",
    );
    Ok(report)
}

/// Read, entity-expand and deserialize a `grammar.xml` into the typed tree.
fn parse_grammar(grammar_path: &Path) -> Result<rules::RulesElementType> {
    let raw = std::fs::read_to_string(grammar_path)
        .with_context(|| format!("reading {}", grammar_path.display()))?;
    let xml = expand_entities(&raw, grammar_path.parent())
        .with_context(|| format!("preprocessing {}", grammar_path.display()))?;
    let mut reader = SliceReader::new(&xml).with_error_info();
    DeserializeSync::deserialize(&mut reader)
        .map_err(|e| anyhow!("deserializing {}: {e:?}", grammar_path.display()))
}

/// A bundled `<example>` sentence: the differential-oracle unit. Positive examples carry the
/// expected `corrections` and a `marker` span; negative examples have neither.
#[derive(Debug, Clone)]
pub struct Example {
    /// The rule (or enclosing group) the example belongs to.
    pub rule_id: String,
    /// The plain sentence, with `<marker>` tags removed.
    pub text: String,
    /// Byte span of the marked (erroneous) region within [`text`](Self::text), if any.
    pub marker: Option<(usize, usize)>,
    /// Expected correction alternatives (from `correction="a|b"`); empty for negative examples.
    pub corrections: Vec<String>,
}

/// Extract every `<example>` sentence from a LanguageTool `grammar.xml` — the oracle corpus.
///
/// # Errors
/// Returns an error if the grammar file is missing or fails to parse.
pub fn extract_examples(grammar_path: &Path) -> Result<Vec<Example>> {
    let doc = parse_grammar(grammar_path)?;
    let mut out = Vec::new();
    for item in &doc.content {
        let rules::RulesElementTypeContent::Category(cat) = item else {
            continue;
        };
        for c in &cat.content {
            match c {
                rules::CategoryElementTypeContent::Rule(r) if rule_enabled(r.default.as_ref()) => {
                    collect_examples(r, None, &mut out);
                }
                rules::CategoryElementTypeContent::Rulegroup(g)
                    if group_enabled(g.default.as_ref()) =>
                {
                    for r in g.rule.iter().filter(|r| rule_enabled(r.default.as_ref())) {
                        collect_examples(r, Some(&g.id), &mut out);
                    }
                }
                // Disabled rules/groups don't fire by default — exclude their examples too.
                _ => {}
            }
        }
    }
    Ok(out)
}

/// Gather the `<example>`s of a single `<rule>` into `out`.
fn collect_examples(r: &rules::RuleElementType, group_id: Option<&str>, out: &mut Vec<Example>) {
    let rule_id =
        r.id.clone()
            .or_else(|| group_id.map(str::to_owned))
            .unwrap_or_else(|| "<anon>".to_owned());
    for c in &r.content {
        if let rules::RuleElementTypeContent::Example(ex) = c {
            out.push(lower_example(&rule_id, ex));
        }
    }
}

/// Reconstruct an example's plain text and marker span from its mixed content.
fn lower_example(rule_id: &str, ex: &rules::ExampleElementType) -> Example {
    let corrections = ex
        .correction
        .as_deref()
        .map(|c| c.split('|').map(str::to_owned).collect())
        .unwrap_or_default();

    let mut text = ex
        .text_before
        .as_ref()
        .map_or_else(String::new, |t| t.0.clone());
    let mut marker = None;
    for item in &ex.content {
        // `ExampleElementTypeContent` has a single `Marker` variant, so this is irrefutable.
        let rules::ExampleElementTypeContent::Marker(m) = item;
        let start = text.len();
        text.push_str(&m.value.content);
        marker = Some((start, text.len()));
        if let Some(after) = &m.text_after {
            text.push_str(&after.0);
        }
    }

    Example {
        rule_id: rule_id.to_owned(),
        text,
        marker,
        corrections,
    }
}

/// Named `<phrase>` definitions (`id` → lowered constructs), for resolving `<phraseref>`.
type Phrases = std::collections::HashMap<String, Vec<Construct>>;

/// Walk the parsed document into a flat list of lowered rules.
fn lower_document(doc: &rules::RulesElementType) -> Vec<Rule> {
    let phrases = collect_phrases(doc);
    let mut out = Vec::new();
    for item in &doc.content {
        let rules::RulesElementTypeContent::Category(cat) = item else {
            continue; // <unification> at the top level carries no rules.
        };
        for c in &cat.content {
            match c {
                rules::CategoryElementTypeContent::Rule(r) if rule_enabled(r.default.as_ref()) => {
                    out.push(lower_rule(r, None, &[], &phrases));
                }
                rules::CategoryElementTypeContent::Rulegroup(g)
                    if group_enabled(g.default.as_ref()) =>
                {
                    // Group-level antipatterns apply to every member rule.
                    let group_aps: Vec<Vec<Construct>> = g
                        .antipattern
                        .iter()
                        .map(|a| {
                            let mut ap = lower_antipattern(a, &phrases);
                            if a.case_sensitive.as_ref().is_some_and(is_yes) {
                                force_case_sensitive(&mut ap);
                            }
                            ap
                        })
                        .collect();
                    out.extend(
                        g.rule
                            .iter()
                            .filter(|r| rule_enabled(r.default.as_ref()))
                            .map(|r| lower_rule(r, Some(&g.id), &group_aps, &phrases)),
                    );
                }
                // `default="off"`/`"temp_off"` rules and groups are disabled in LT — skip them.
                _ => {}
            }
        }
    }
    out
}

/// Collect all top-level `<phrases>`/`<phrase>` definitions into a lookup map.
fn collect_phrases(doc: &rules::RulesElementType) -> Phrases {
    let mut phrases = Phrases::new();
    for item in &doc.content {
        if let rules::RulesElementTypeContent::Phrases(ps) = item {
            for p in &ps.phrase {
                phrases.insert(p.id.clone(), lower_phrase(p));
            }
        }
    }
    phrases
}

/// Lower a `<phrase>`'s body into constructs (token vocabulary; `<unify>`/`<includephrases>` become
/// `Unsupported`). Phrases do not contain `<phraseref>`s, so no recursion is needed.
fn lower_phrase(p: &pattern::PhraseElementType) -> Vec<Construct> {
    let mut out = Vec::new();
    for c in &p.content {
        match c {
            pattern::PhraseElementTypeContent::Token(t) => push_token(t, &mut out),
            pattern::PhraseElementTypeContent::And(a) => push_and(a, &mut out),
            pattern::PhraseElementTypeContent::Unify(_) => {
                out.push(Construct::Unsupported {
                    kind: "unify".to_owned(),
                });
            }
            pattern::PhraseElementTypeContent::Includephrases(_) => {
                out.push(Construct::Unsupported {
                    kind: "includephrases".to_owned(),
                });
            }
        }
    }
    out
}

/// Whether a `<rule>` is enabled (not `default="off"`/`"temp_off"`).
fn rule_enabled(default: Option<&rules::RuleDefaultType>) -> bool {
    !matches!(
        default,
        Some(rules::RuleDefaultType::Off | rules::RuleDefaultType::TempOff)
    )
}

/// Whether a `<rulegroup>` is enabled (not `default="off"`/`"temp_off"`).
fn group_enabled(default: Option<&rules::RulegroupDefaultType>) -> bool {
    !matches!(
        default,
        Some(rules::RulegroupDefaultType::Off | rules::RulegroupDefaultType::TempOff)
    )
}

/// Lower one `<rule>`; anonymous rules inside a `<rulegroup>` inherit the group id and its
/// `group_antipatterns`.
fn lower_rule(
    r: &rules::RuleElementType,
    group_id: Option<&str>,
    group_antipatterns: &[Vec<Construct>],
    phrases: &Phrases,
) -> Rule {
    let id =
        r.id.clone()
            .or_else(|| group_id.map(str::to_owned))
            .unwrap_or_else(|| "<anon>".to_owned());

    let mut pattern = Vec::new();
    let mut message = String::new();
    let mut suggestions = Vec::new();
    let mut antipatterns: Vec<Vec<Construct>> = group_antipatterns.to_vec();
    for c in &r.content {
        match c {
            rules::RuleElementTypeContent::Pattern(p) => {
                let start = pattern.len();
                lower_pattern(&p.content, &mut pattern, phrases);
                if p.case_sensitive.as_ref().is_some_and(is_yes) {
                    force_case_sensitive(&mut pattern[start..]);
                }
            }
            rules::RuleElementTypeContent::Antipattern(a) => {
                let mut ap = lower_antipattern(a, phrases);
                if a.case_sensitive.as_ref().is_some_and(is_yes) {
                    force_case_sensitive(&mut ap);
                }
                antipatterns.push(ap);
            }
            rules::RuleElementTypeContent::Filter(f) => pattern.push(Construct::Opaque {
                class: f.class.clone(),
                args: f.args.clone(),
            }),
            rules::RuleElementTypeContent::Regexp(re) => {
                pattern.push(Construct::Regexp {
                    pattern: re.content.clone(),
                    mark: re.mark.and_then(|m| usize::try_from(m).ok()),
                    case_sensitive: re.case_sensitive.as_ref().is_some_and(is_yes),
                });
            }
            rules::RuleElementTypeContent::Message(m) => {
                let (text, sugs) = lower_message(m);
                if message.is_empty() {
                    message = text;
                }
                suggestions.extend(sugs);
            }
            rules::RuleElementTypeContent::Suggestion(s) => suggestions.extend(lower_suggestion(s)),
            // Url/Short/Example are not part of the match-and-suggest path here.
            _ => {}
        }
    }
    Rule {
        id,
        pattern,
        antipatterns,
        message,
        suggestions,
    }
}

/// Lower an `<antipattern>` into a construct list (the same token vocabulary as a `<pattern>`;
/// `<example>` children and unsupported sub-constructs are dropped/kept as `Unsupported`).
fn lower_antipattern(a: &rules::AntipatternElementType, phrases: &Phrases) -> Vec<Construct> {
    let mut out = Vec::new();
    for item in &a.content {
        match item {
            rules::AntipatternElementTypeContent::Token(t) => {
                push_token(t, &mut out);
            }
            rules::AntipatternElementTypeContent::Marker(m) => {
                out.push(Construct::MarkerStart);
                lower_marker(&m.content, &mut out, phrases);
                out.push(Construct::MarkerEnd);
            }
            rules::AntipatternElementTypeContent::And(a) => push_and(a, &mut out),
            rules::AntipatternElementTypeContent::Unify(_) => {
                out.push(Construct::Unsupported {
                    kind: "unify".to_owned(),
                });
            }
            rules::AntipatternElementTypeContent::Example(_) => {}
        }
    }
    out
}

/// Lower a `<message>` into its display text plus the corrections (`<suggestion>`s) it embeds. Inline
/// `<suggestion>`/`<match>` are rendered into the text as `\N` token backreferences (the runtime
/// substitutes them with the matched surface) — otherwise the words are dropped and the message reads
/// "Use  or, in informal register, ." (the mangled-message bug).
fn lower_message(m: &rules::MessageElementType) -> (String, Vec<Suggestion>) {
    let mut text = m
        .text_before
        .as_ref()
        .map_or_else(String::new, |t| t.0.clone());
    let mut suggestions = Vec::new();
    for item in &m.content {
        match item {
            rules::MessageElementTypeContent::Suggestion(s) => {
                text.push_str(&inline_suggestion(&s.value));
                suggestions.extend(lower_suggestion(&s.value));
                if let Some(after) = &s.text_after {
                    text.push_str(&after.0);
                }
            }
            rules::MessageElementTypeContent::Match(mt) => {
                use std::fmt::Write as _;
                let _ = write!(text, "\\{}", mt.value.no);
                if let Some(after) = &mt.text_after {
                    text.push_str(&after.0);
                }
            }
        }
    }
    (text.trim().to_owned(), suggestions)
}

/// Render a message-embedded `<suggestion>` as literal text + `\N` token backreferences (its display
/// form). The precise correction — with case/regex transforms — is carried separately by
/// [`lower_suggestion`]; this is only what the human-readable message shows inline.
fn inline_suggestion(s: &rules::SuggestionElementType) -> String {
    use std::fmt::Write as _;
    let mut out = s
        .text_before
        .as_ref()
        .map_or_else(String::new, |t| t.0.clone());
    for item in &s.content {
        let _ = write!(out, "\\{}", item.match_.value.no);
        if let Some(after) = &item.match_.text_after {
            out.push_str(&after.0);
        }
    }
    out
}

/// Lower a `<suggestion>` into a correction template (literal text + `<match no>` token refs, with
/// optional `regexp_replace`). Returns `None` when a `<match>` needs morphological synthesis
/// (`postag_replace`), which we cannot perform — better to drop the suggestion than render it wrong.
fn lower_suggestion(s: &rules::SuggestionElementType) -> Option<Suggestion> {
    let mut parts = Vec::new();
    if let Some(t) = &s.text_before {
        push_text(&mut parts, &t.0);
    }
    for item in &s.content {
        let m = &item.match_.value;
        if m.postag_replace.is_some() {
            return None; // synthesis (generate an inflected form) — unsupported
        }
        let transform = match (m.regexp_match.as_ref(), m.regexp_replace.as_ref()) {
            (Some(rm), Some(rr)) => Some((rm.clone(), rr.clone())),
            _ => None,
        };
        parts.push(SugPart::Token {
            no: m.no,
            case: map_case(m.case_conversion.as_ref()),
            transform,
        });
        if let Some(after) = &item.match_.text_after {
            push_text(&mut parts, &after.0);
        }
    }
    Some(Suggestion { parts })
}

/// Append suggestion text, splitting LT's `\N` backreference shorthand (e.g. `\2`, equivalent to
/// `<match no="2"/>`) into [`SugPart::Token`] parts and preserving the literal text around them.
fn push_text(parts: &mut Vec<SugPart>, text: &str) {
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(d) if d.is_ascii_digit() => {
                    if !literal.is_empty() {
                        parts.push(SugPart::Text(std::mem::take(&mut literal)));
                    }
                    let no = (*d as u8 - b'0') as usize;
                    chars.next();
                    parts.push(SugPart::Token {
                        no,
                        case: Case::Keep,
                        transform: None,
                    });
                }
                // `\\` → literal backslash; any other escape → keep the next char literally.
                Some(_) => literal.push(chars.next().unwrap_or('\\')),
                None => literal.push('\\'),
            }
        } else {
            literal.push(c);
        }
    }
    if !literal.is_empty() {
        parts.push(SugPart::Text(literal));
    }
}

/// Map LT's `case_conversion` to the IR [`Case`].
fn map_case(c: Option<&pattern::MatchCaseConversionType>) -> Case {
    use pattern::MatchCaseConversionType as M;
    match c {
        Some(M::Allupper) => Case::Upper,
        Some(M::Alllower) => Case::Lower,
        Some(M::Startupper | M::Firstupper) => Case::StartUpper,
        _ => Case::Keep,
    }
}

/// Lower the contents of a `<pattern>` into IR constructs.
fn lower_pattern(
    content: &[rules::PatternElementTypeContent],
    out: &mut Vec<Construct>,
    phrases: &Phrases,
) {
    for item in content {
        match item {
            rules::PatternElementTypeContent::Token(t) => {
                push_token(t, out);
            }
            rules::PatternElementTypeContent::Marker(m) => {
                out.push(Construct::MarkerStart);
                lower_marker(&m.content, out, phrases);
                out.push(Construct::MarkerEnd);
            }
            rules::PatternElementTypeContent::Phraseref(p) => {
                push_phraseref(&p.idref, out, phrases);
            }
            rules::PatternElementTypeContent::And(a) => push_and(a, out),
            rules::PatternElementTypeContent::Or(o) => push_or(o, out),
            rules::PatternElementTypeContent::Unify(_) => {
                out.push(Construct::Unsupported {
                    kind: "unify".to_owned(),
                });
            }
        }
    }
}

/// Lower the contents of a `<marker>` (the same construct vocabulary as `<pattern>`) into `out`.
fn lower_marker(
    content: &[pattern::MarkerElementTypeContent],
    out: &mut Vec<Construct>,
    phrases: &Phrases,
) {
    for item in content {
        match item {
            pattern::MarkerElementTypeContent::Token(t) => {
                push_token(t, out);
            }
            pattern::MarkerElementTypeContent::Or(o) => push_or(o, out),
            pattern::MarkerElementTypeContent::And(a) => push_and(a, out),
            pattern::MarkerElementTypeContent::Phraseref(p) => {
                push_phraseref(&p.idref, out, phrases);
            }
            pattern::MarkerElementTypeContent::Unify(_) => {
                out.push(Construct::Unsupported {
                    kind: "unify".to_owned(),
                });
            }
            pattern::MarkerElementTypeContent::UnifyIgnore(_) => {
                out.push(Construct::Unsupported {
                    kind: "unify-ignore".to_owned(),
                });
            }
        }
    }
}

/// Resolve a `<phraseref idref="…">` by inlining the named phrase's constructs (or `Unsupported`
/// if the phrase is undefined).
fn push_phraseref(idref: &str, out: &mut Vec<Construct>, phrases: &Phrases) {
    match phrases.get(idref) {
        Some(constructs) => out.extend(constructs.iter().cloned()),
        None => out.push(Construct::Unsupported {
            kind: "phraseref".to_owned(),
        }),
    }
}

/// Push a `<token>` as a construct. A token containing a `<match>` child is a back-reference
/// matcher (its value depends on another captured token) we can't faithfully match, so it becomes
/// `Unsupported` — which makes the enclosing rule skip rather than match too loosely.
fn push_token(t: &pattern::TokenElementType, out: &mut Vec<Construct>) {
    if token_has_match(t) {
        out.push(Construct::Unsupported {
            kind: "token-match".to_owned(),
        });
    } else {
        out.push(Construct::Token(lower_token(t)));
    }
}

/// Whether a `<token>` carries a `<match>` back-reference child (which we cannot match faithfully).
fn token_has_match(t: &pattern::TokenElementType) -> bool {
    t.content
        .iter()
        .any(|c| matches!(c, pattern::TokenElementTypeContent::Match(_)))
}

/// Push an `<or>` as `Construct::Or` (alternatives), or `Unsupported` if any alternative is not a
/// plain token.
fn push_or(o: &pattern::OrElementType, out: &mut Vec<Construct>) {
    let mut alts = Vec::new();
    for c in &o.content {
        // `OrElementTypeContent` has a single `Token` variant, so this is irrefutable.
        let pattern::OrElementTypeContent::Token(t) = c;
        if token_has_match(t) {
            out.push(Construct::Unsupported {
                kind: "or".to_owned(),
            });
            return;
        }
        alts.push(lower_token(t));
    }
    out.push(if alts.is_empty() {
        Construct::Unsupported {
            kind: "or".to_owned(),
        }
    } else {
        Construct::Or(alts)
    });
}

/// Push an `<and>` as `Construct::And` (constraints that must all hold), or `Unsupported` if it
/// contains a non-token child (e.g. a nested `<marker>`).
fn push_and(a: &pattern::AndElementType, out: &mut Vec<Construct>) {
    let mut alts = Vec::new();
    for c in &a.content {
        match c {
            pattern::AndElementTypeContent::Token(t) if !token_has_match(t) => {
                alts.push(lower_token(t));
            }
            _ => {
                out.push(Construct::Unsupported {
                    kind: "and".to_owned(),
                });
                return;
            }
        }
    }
    out.push(if alts.is_empty() {
        Construct::Unsupported {
            kind: "and".to_owned(),
        }
    } else {
        Construct::And(alts)
    });
}

/// Lower a `<token>`'s declarative attributes, literal/regex text and `<exception>`s.
fn lower_token(t: &pattern::TokenElementType) -> TokenPat {
    let exceptions = t
        .content
        .iter()
        .filter_map(|c| match c {
            pattern::TokenElementTypeContent::Exception(e) => Some(lower_exception(&e.value)),
            pattern::TokenElementTypeContent::Match(_) => None,
        })
        .collect();
    TokenPat {
        text: token_literal(t),
        postag: t.postag.clone(),
        regexp: is_yes(&t.regexp),
        postag_regexp: is_yes(&t.postag_regexp),
        negate: is_yes(&t.negate),
        inflected: is_yes(&t.inflected),
        min: t.min,
        max: t.max,
        skip: t.skip,
        case_sensitive: t.case_sensitive.as_ref().is_some_and(is_yes),
        space_before: space_before(&t.spacebefore),
        exceptions,
    }
}

/// Lower LT's `spacebefore` (`yes`/`no`/`ignore`) to the IR's optional whitespace constraint.
fn space_before(s: &pattern::TokenSpacebeforeType) -> Option<bool> {
    match s {
        pattern::TokenSpacebeforeType::Yes => Some(true),
        pattern::TokenSpacebeforeType::No => Some(false),
        pattern::TokenSpacebeforeType::Ignore => None,
    }
}

/// A `<token>`'s literal (or regex, when `regexp="yes"`) surface text. LT writes it either as the
/// token's leading text (`<token>hell</token>`) **or** as the *tail* text after a child
/// `<exception>`/`<match>` (`<token><exception …>…</exception>hell</token>`) — the latter lands in the
/// child's `Mixed::text_after`, not `text_before`. Missing it silently turns the token into a wildcard
/// (it then matches any word), so gather every text fragment of the element, ignoring child contents.
fn token_literal(t: &pattern::TokenElementType) -> Option<String> {
    let mut text = t
        .text_before
        .as_ref()
        .map(|x| x.0.clone())
        .unwrap_or_default();
    for c in &t.content {
        let tail = match c {
            pattern::TokenElementTypeContent::Exception(e) => e.text_after.as_ref(),
            pattern::TokenElementTypeContent::Match(m) => m.text_after.as_ref(),
        };
        if let Some(tail) = tail {
            text.push_str(&tail.0);
        }
    }
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Lower a `<token>`'s `<exception>` into an [`ExceptionPat`].
fn lower_exception(e: &pattern::ExceptionElementType) -> ExceptionPat {
    ExceptionPat {
        text: trimmed(e.text.as_ref()),
        postag: e.postag.clone(),
        regexp: is_yes(&e.regexp),
        postag_regexp: is_yes(&e.postag_regexp),
        inflected: is_yes(&e.inflected),
        negate: is_yes(&e.negate),
        case_sensitive: e.case_sensitive.as_ref().is_some_and(is_yes),
    }
}

/// A trimmed, non-empty literal from an optional mixed-content text node.
fn trimmed(text: Option<&xsd_parser_types::xml::Text>) -> Option<String> {
    text.map(|x| x.0.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Force case-sensitive matching on every token (and its exceptions) and rule-level `<regexp>` in
/// `constructs`. `<pattern>`/`<antipattern>` carry a `case_sensitive="yes"` that applies to *all* their
/// tokens, but the lowering only sees the children, not that attribute — so the enclosing scope stamps
/// it on afterward. Without this, a case-sensitive uppercase regex (e.g. Russian `AllCaps`'s
/// `[А-ЯЁ]{4,4}…`) matches case-insensitively and fires on every capitalised word. Shared with the
/// disambiguation lowering, which has the same `<pattern case_sensitive>` (98 in en, 284 in de).
pub(crate) fn force_case_sensitive(constructs: &mut [Construct]) {
    fn mark(t: &mut TokenPat) {
        t.case_sensitive = true;
        for e in &mut t.exceptions {
            e.case_sensitive = true;
        }
    }
    for c in constructs {
        match c {
            Construct::Token(t) => mark(t),
            Construct::Or(ts) | Construct::And(ts) => ts.iter_mut().for_each(mark),
            Construct::Regexp { case_sensitive, .. } => *case_sensitive = true,
            _ => {}
        }
    }
}

/// LT's `yes`/`no` enum → bool.
fn is_yes(v: &pattern::BinaryYesNoType) -> bool {
    matches!(v, pattern::BinaryYesNoType::Yes)
}

/// Whether opening `name` begins a region whose `<token>`s the converter does *not* lower 1:1, so the
/// raw scan must skip them too: `<phrases>` definitions (expanded only at `<phraseref>`), `<unify>`
/// (unsupported), and `default="off"`/`"temp_off"` `<rule>`/`<rulegroup>`s (disabled — see
/// [`rule_enabled`]/[`group_enabled`]). Mirroring this is what keeps the fidelity audit free of
/// spurious "missing" literals.
fn opens_skip_region(name: &[u8], e: &quick_xml::events::BytesStart) -> Result<bool> {
    if matches!(name, b"phrases" | b"unify") {
        return Ok(true);
    }
    if matches!(name, b"rule" | b"rulegroup") {
        if let Some(attr) = e
            .try_get_attribute("default")
            .map_err(|err| anyhow!("reading default attr: {err}"))?
        {
            return Ok(matches!(attr.value.as_ref(), b"off" | b"temp_off"));
        }
    }
    Ok(false)
}

/// Independently extract every *enabled* `<token>`'s literal surface text from the (entity-expanded)
/// grammar via a raw event scan — only text that is a *direct* child of `<token>` (not inside an
/// `<exception>`/`<match>` child, which carry their own text). The genuinely-independent cross-check for
/// [`token_literal`]: it doesn't touch the xsd-parser path. Skips regions the converter doesn't lower
/// (see [`opens_skip_region`]).
fn raw_token_literals(xml: &str) -> Result<std::collections::HashSet<String>> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<(Box<[u8]>, bool)> = Vec::new(); // (element name, opened a skip region)
    let mut skip_depth = 0u32;
    let mut literal: Option<String> = None; // text being accumulated for the open <token>
    let mut out = std::collections::HashSet::new();
    loop {
        match reader
            .read_event()
            .map_err(|e| anyhow!("scanning token literals: {e}"))?
        {
            Event::Start(e) => {
                let name: Box<[u8]> = e.name().as_ref().into();
                let skip = opens_skip_region(&name, &e)?;
                if skip {
                    skip_depth += 1;
                }
                if &*name == b"token" && skip_depth == 0 {
                    literal = Some(String::new());
                }
                stack.push((name, skip));
            }
            Event::End(_) => {
                if let Some((name, skip)) = stack.pop() {
                    if &*name == b"token" {
                        if let Some(s) = literal.take() {
                            let s = s.trim();
                            if !s.is_empty() {
                                out.insert(s.to_owned());
                            }
                        }
                    }
                    if skip {
                        skip_depth = skip_depth.saturating_sub(1);
                    }
                }
            }
            Event::Text(t) => {
                // Only text whose immediate parent is the <token> itself is the token's literal.
                if let Some(acc) = literal.as_mut() {
                    if stack.last().is_some_and(|(n, _)| n.as_ref() == b"token") {
                        let s =
                            std::str::from_utf8(&t).map_err(|e| anyhow!("token text utf8: {e}"))?;
                        acc.push_str(
                            &quick_xml::escape::unescape(s)
                                .map_err(|e| anyhow!("unescape: {e}"))?,
                        );
                    }
                }
            }
            // quick-xml emits `&amp;` etc. as a separate ref event mid-text; resolve it into the literal
            // (else `,|and|or|&amp;` would read as `,|and|or|`, a spurious mismatch with the IR).
            Event::GeneralRef(r) => {
                if let Some(acc) = literal.as_mut() {
                    if stack.last().is_some_and(|(n, _)| n.as_ref() == b"token") {
                        if let Some(c) =
                            r.resolve_char_ref().map_err(|e| anyhow!("char ref: {e}"))?
                        {
                            acc.push(c);
                        } else {
                            let name = r.decode().map_err(|e| anyhow!("ref decode: {e}"))?;
                            acc.push_str(match name.as_ref() {
                                "amp" => "&",
                                "lt" => "<",
                                "gt" => ">",
                                "quot" => "\"",
                                "apos" => "'",
                                other => return Err(anyhow!("unresolved entity &{other};")),
                            });
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Gather the literal text of every `<token>` matcher in a construct list (incl. `<or>`/`<and>` groups).
fn collect_token_texts(constructs: &[Construct], out: &mut std::collections::HashSet<String>) {
    for c in constructs {
        match c {
            Construct::Token(t) => out.extend(t.text.clone()),
            Construct::Or(ts) | Construct::And(ts) => {
                out.extend(ts.iter().filter_map(|t| t.text.clone()));
            }
            _ => {}
        }
    }
}

/// Cross-check the converter's token-literal extraction against an independent raw scan: the source
/// token literals the lowered IR is **missing**. Each is a `<token>` whose surface text was dropped —
/// turning it into a wildcard that matches any word (the HELL bug). Empty ⇒ full fidelity.
///
/// # Errors
/// Propagates read / parse / scan errors.
pub fn token_literal_fidelity(grammar_path: &Path) -> Result<Vec<String>> {
    let raw = std::fs::read_to_string(grammar_path)
        .with_context(|| format!("reading {}", grammar_path.display()))?;
    let xml = expand_entities(&raw, grammar_path.parent())?;
    let source = raw_token_literals(&xml)?;

    let mut ir = std::collections::HashSet::new();
    for rule in lower_document(&parse_grammar(grammar_path)?) {
        collect_token_texts(&rule.pattern, &mut ir);
        for ap in &rule.antipatterns {
            collect_token_texts(ap, &mut ir);
        }
    }

    let mut missing: Vec<String> = source.into_iter().filter(|s| !ir.contains(s)).collect();
    missing.sort();
    Ok(missing)
}

/// Expand LT's internal-DTD general entities and strip the DOCTYPE, yielding standalone XML.
///
/// LT's `grammar.xml` declares ~40 `<!ENTITY name "value">` in an internal subset and references
/// them thousands of times. quick_xml does not expand DTD entities, so we do a textual pass:
/// remove the DOCTYPE, then iteratively substitute `&name;` until stable (entities may reference
/// other entities). Quote-bearing entity values appear only in element content in LT's grammar,
/// so textual substitution is safe here.
///
/// The Romance languages (es/fr/it) hoist their shared general entities into an **external** DTD
/// file pulled in by a parameter entity — `<!ENTITY % entities SYSTEM "…/entities.ent">` then
/// `%entities;`. quick_xml resolves no external subsets either, so when `base_dir` is given we read
/// each `SYSTEM "rel/path"` file (relative to the XML's own directory) and fold its `<!ENTITY …>`
/// declarations into the same substitution set as the inline ones. en/de/ru/ar have no parameter
/// entities, so this is a no-op for them (byte-identical output).
pub(crate) fn expand_entities(xml: &str, base_dir: Option<&Path>) -> Result<String> {
    let Some(doctype_start) = xml.find("<!DOCTYPE") else {
        return Ok(xml.to_owned()); // No internal subset — nothing to expand.
    };
    let subset_end = xml[doctype_start..]
        .find("]>")
        .map(|i| i + doctype_start)
        .ok_or_else(|| anyhow!("unterminated DOCTYPE internal subset"))?;
    let doctype = xml[doctype_start..subset_end].to_owned();

    // <!ENTITY name "value"> and <!ENTITY name 'value'> — the delimiter is respected so values
    // may contain the other quote (e.g. <!ENTITY quote '["…"]'>).
    let double_q = Regex::new(r#"<!ENTITY\s+(\w+)\s+"([^"]*)""#)?;
    let single_q = Regex::new(r"<!ENTITY\s+(\w+)\s+'([^']*)'")?;
    let parse_decls = |text: &str| -> Vec<(String, String)> {
        double_q
            .captures_iter(text)
            .chain(single_q.captures_iter(text))
            .map(|cap| (cap[1].to_owned(), cap[2].to_owned()))
            .collect()
    };

    // External parameter entities: `<!ENTITY % name SYSTEM "rel/path">`. Inline each referenced
    // file's declarations (es/fr/it `entities.ent`); silently skip if unreadable or no base dir.
    let mut decl_text = doctype.clone();
    if let Some(dir) = base_dir {
        let sys = Regex::new(r#"<!ENTITY\s+%\s+\w+\s+SYSTEM\s+"([^"]*)""#)?;
        for cap in sys.captures_iter(&doctype) {
            if let Ok(ext) = std::fs::read_to_string(dir.join(&cap[1])) {
                decl_text.push('\n');
                decl_text.push_str(&ext);
            }
        }
    }

    let entities: Vec<(String, String)> = parse_decls(&decl_text);

    let mut body = String::with_capacity(xml.len());
    body.push_str(&xml[..doctype_start]);
    body.push_str(&xml[subset_end + 2..]);

    // Iterate to resolve nested entity references; ~3 passes suffice for LT, 10 is a safe cap.
    for _ in 0..10 {
        let mut changed = false;
        for (name, value) in &entities {
            let needle = format!("&{name};");
            if body.contains(&needle) {
                body = body.replace(&needle, value);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::expand_entities;

    #[test]
    fn expands_nested_entities_and_strips_doctype() {
        let xml = "<?xml version=\"1.0\"?>\n\
                   <!DOCTYPE rules [\n\
                   <!ENTITY base \"foo\">\n\
                   <!ENTITY combo \"&base;-bar\">\n\
                   <!ENTITY q '[\"x\"]'>\n\
                   ]>\n\
                   <rules><a>&combo;</a><b>&q;</b></rules>";
        let out = expand_entities(xml, None).expect("expand");

        assert!(!out.contains("<!DOCTYPE"), "DOCTYPE not stripped: {out}");
        assert!(
            out.contains("<a>foo-bar</a>"),
            "nested entity not expanded: {out}"
        );
        assert!(
            out.contains("<b>[\"x\"]</b>"),
            "single-quoted entity not expanded: {out}"
        );
        assert!(
            !out.contains("&combo;") && !out.contains("&base;"),
            "leftover entity: {out}"
        );
    }

    #[test]
    fn passthrough_without_doctype() {
        let xml = "<rules><a/></rules>";
        assert_eq!(expand_entities(xml, None).unwrap(), xml);
    }

    /// Regression: a `<token>`'s literal text often follows its child `<exception>` (as XML tail text),
    /// e.g. the LT `HELL` rule's `<token><exception …>…</exception>hell</token>`. Dropping that tail
    /// turned the token into a wildcard that matched any word (HELL flagged "there" → "he'll"). The
    /// converted token must carry the literal "hell".
    #[test]
    fn token_literal_after_exception_is_not_a_wildcard() {
        let dir = std::env::temp_dir().join(format!("rlt-tok-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grammar.xml");
        std::fs::write(
            &path,
            "<?xml version=\"1.0\"?>\n\
             <rules lang=\"en\">\n\
               <category id=\"C\" name=\"C\">\n\
                 <rule id=\"HELLISH\" name=\"hellish\">\n\
                   <pattern>\n\
                     <token><exception scope=\"previous\" regexp=\"yes\">the|their</exception>hell</token>\n\
                     <token postag=\"VB\"/>\n\
                   </pattern>\n\
                   <message>Did you mean <suggestion>he'll</suggestion>?</message>\n\
                   <example correction=\"he'll\"><marker>hell</marker> be there.</example>\n\
                 </rule>\n\
               </category>\n\
             </rules>\n",
        )
        .unwrap();

        let doc = super::parse_grammar(&path).expect("parse minimal grammar");
        let token = super::lower_document(&doc)
            .iter()
            .flat_map(|r| r.pattern.clone())
            .find_map(|c| match c {
                super::Construct::Token(t) => Some(t),
                _ => None,
            })
            .expect("the rule has a token");
        assert_eq!(
            token.text.as_deref(),
            Some("hell"),
            "token literal after <exception> was dropped (wildcard regression)",
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression: `<pattern case_sensitive="yes">` must stamp case-sensitivity onto its tokens. Without
    /// it, an uppercase regex (ru `AllCaps`'s `[А-ЯЁ]{4,4}…`) matches lowercase and fires on every word.
    #[test]
    fn pattern_case_sensitive_propagates_to_tokens() {
        let dir = std::env::temp_dir().join(format!("rlt-cs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grammar.xml");
        std::fs::write(
            &path,
            "<?xml version=\"1.0\"?>\n\
             <rules lang=\"en\">\n\
               <category id=\"C\" name=\"C\">\n\
                 <rule id=\"CS\" name=\"cs\">\n\
                   <pattern case_sensitive=\"yes\">\n\
                     <token regexp=\"yes\">[A-Z]+</token>\n\
                   </pattern>\n\
                   <message>caps</message>\n\
                   <example correction=\"x\"><marker>ABC</marker></example>\n\
                 </rule>\n\
               </category>\n\
             </rules>\n",
        )
        .unwrap();
        let token = super::lower_document(&super::parse_grammar(&path).expect("parse"))
            .iter()
            .flat_map(|r| r.pattern.clone())
            .find_map(|c| match c {
                super::Construct::Token(t) => Some(t),
                _ => None,
            })
            .expect("the rule has a token");
        assert!(
            token.case_sensitive,
            "pattern-level case_sensitive was not propagated to the token"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Full-grammar fidelity gate (needs `fetch-lt`): every enabled `<token>` literal in the real LT
    /// English grammar must survive lowering — no token silently becomes a wildcard. `#[ignore]` since
    /// it needs the fetched resources (run in the engine CI job).
    #[test]
    #[ignore = "needs fetch-lt resources"]
    fn real_grammar_has_full_token_fidelity() {
        let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../"))
            .join(super::DEFAULT_GRAMMAR);
        if !path.exists() {
            eprintln!(
                "SKIP: {} missing — run `cargo xtask fetch-lt`",
                path.display()
            );
            return;
        }
        let missing = super::token_literal_fidelity(&path).expect("fidelity scan");
        assert!(
            missing.is_empty(),
            "dropped token literals (wildcard bugs): {missing:?}"
        );
    }

    /// en/de/ru/ar have only an *inline* internal subset — a `base_dir` must not change their
    /// output one byte (the byte-identical gate for the external-entity generalization).
    #[test]
    fn inline_subset_is_base_dir_invariant() {
        let xml = "<!DOCTYPE rules [\n\
                   <!ENTITY base \"foo\">\n\
                   ]>\n\
                   <rules><a>&base;</a></rules>";
        let without = expand_entities(xml, None).unwrap();
        let with = expand_entities(xml, Some(std::path::Path::new("/nonexistent"))).unwrap();
        assert_eq!(without, with, "base_dir altered inline-subset expansion");
        assert!(without.contains("<a>foo</a>"));
    }

    /// es/fr/it pull their general entities from an external `entities.ent` via a parameter entity
    /// (`<!ENTITY % … SYSTEM "rel.ent">` + `%entities;`). Declarations in that file must expand too.
    #[test]
    fn resolves_external_parameter_entity_file() {
        let dir = std::env::temp_dir().join(format!("rlt-ent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ent = dir.join("entities.ent");
        std::fs::write(&ent, "<!ENTITY shared \"hola\">\n").unwrap();

        let xml = "<!DOCTYPE rules [\n\
                   <!ENTITY % entities SYSTEM \"entities.ent\">\n\
                   %entities;\n\
                   <!ENTITY local \"adios\">\n\
                   ]>\n\
                   <rules><a>&shared;</a><b>&local;</b></rules>";
        let out = expand_entities(xml, Some(&dir)).unwrap();
        assert!(
            out.contains("<a>hola</a>"),
            "external entity not expanded: {out}"
        );
        assert!(out.contains("<b>adios</b>"), "inline entity lost: {out}");
        assert!(
            !out.contains("&shared;") && !out.contains("%entities;"),
            "leftover: {out}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
