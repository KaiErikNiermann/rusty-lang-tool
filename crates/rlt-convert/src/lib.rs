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

mod lt_schema;

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use rlt_ir::{Case, Construct, ExceptionPat, Rule, SugPart, Suggestion, TokenPat};
use xsd_parser_types::quick_xml::{DeserializeSync, SliceReader, XmlReader};

use lt_schema::{pattern, rules};

/// Default location of LT's English `grammar.xml` after `cargo xtask fetch-lt`.
pub const DEFAULT_GRAMMAR: &str = "resources/lt/_repo/languagetool-language-modules/en/src/main/resources/org/languagetool/rules/en/grammar.xml";
/// Default output path for the compiled runtime artifact.
pub const DEFAULT_OUT: &str = "resources/en.rkyv";

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
    let xml = expand_entities(&raw)
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
                rules::CategoryElementTypeContent::Rule(r) => collect_examples(r, None, &mut out),
                rules::CategoryElementTypeContent::Rulegroup(g) => {
                    for r in &g.rule {
                        collect_examples(r, Some(&g.id), &mut out);
                    }
                }
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

/// Walk the parsed document into a flat list of lowered rules.
fn lower_document(doc: &rules::RulesElementType) -> Vec<Rule> {
    let mut out = Vec::new();
    for item in &doc.content {
        let rules::RulesElementTypeContent::Category(cat) = item else {
            continue; // <unification>/<phrases> at the top level carry no rules.
        };
        for c in &cat.content {
            match c {
                rules::CategoryElementTypeContent::Rule(r) => out.push(lower_rule(r, None)),
                rules::CategoryElementTypeContent::Rulegroup(g) => {
                    out.extend(g.rule.iter().map(|r| lower_rule(r, Some(&g.id))));
                }
            }
        }
    }
    out
}

/// Lower one `<rule>`; anonymous rules inside a `<rulegroup>` inherit the group id.
fn lower_rule(r: &rules::RuleElementType, group_id: Option<&str>) -> Rule {
    let id =
        r.id.clone()
            .or_else(|| group_id.map(str::to_owned))
            .unwrap_or_else(|| "<anon>".to_owned());

    let mut pattern = Vec::new();
    let mut message = String::new();
    let mut suggestions = Vec::new();
    for c in &r.content {
        match c {
            rules::RuleElementTypeContent::Pattern(p) => lower_pattern(&p.content, &mut pattern),
            rules::RuleElementTypeContent::Filter(f) => pattern.push(Construct::Opaque {
                class: f.class.clone(),
                args: f.args.clone(),
            }),
            rules::RuleElementTypeContent::Regexp(_) => {
                pattern.push(Construct::Unsupported {
                    kind: "regexp".to_owned(),
                });
            }
            rules::RuleElementTypeContent::Message(m) => {
                let (text, sugs) = lower_message(m);
                if message.is_empty() {
                    message = text;
                }
                suggestions.extend(sugs);
            }
            rules::RuleElementTypeContent::Suggestion(s) => suggestions.push(lower_suggestion(s)),
            // Antipattern/Url/Short/Example are not part of the match-and-suggest path here.
            _ => {}
        }
    }
    Rule {
        id,
        pattern,
        message,
        suggestions,
    }
}

/// Lower a `<message>` into its display text plus the corrections (`<suggestion>`s) it embeds.
fn lower_message(m: &rules::MessageElementType) -> (String, Vec<Suggestion>) {
    let mut text = m
        .text_before
        .as_ref()
        .map_or_else(String::new, |t| t.0.clone());
    let mut suggestions = Vec::new();
    for item in &m.content {
        match item {
            rules::MessageElementTypeContent::Suggestion(s) => {
                suggestions.push(lower_suggestion(&s.value));
                if let Some(after) = &s.text_after {
                    text.push_str(&after.0);
                }
            }
            rules::MessageElementTypeContent::Match(mt) => {
                if let Some(after) = &mt.text_after {
                    text.push_str(&after.0);
                }
            }
        }
    }
    (text.trim().to_owned(), suggestions)
}

/// Lower a `<suggestion>` into a correction template (literal text + `<match no>` token refs).
fn lower_suggestion(s: &rules::SuggestionElementType) -> Suggestion {
    let mut parts = Vec::new();
    if let Some(t) = &s.text_before {
        push_text(&mut parts, &t.0);
    }
    for item in &s.content {
        let m = &item.match_.value;
        parts.push(SugPart::Token {
            no: m.no,
            case: map_case(m.case_conversion.as_ref()),
        });
        if let Some(after) = &item.match_.text_after {
            push_text(&mut parts, &after.0);
        }
    }
    Suggestion { parts }
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
fn lower_pattern(content: &[rules::PatternElementTypeContent], out: &mut Vec<Construct>) {
    for item in content {
        match item {
            rules::PatternElementTypeContent::Token(t) => {
                out.push(Construct::Token(lower_token(t)));
            }
            rules::PatternElementTypeContent::Marker(m) => {
                out.push(Construct::MarkerStart);
                lower_marker(&m.content, out);
                out.push(Construct::MarkerEnd);
            }
            rules::PatternElementTypeContent::Phraseref(_) => {
                out.push(Construct::Unsupported {
                    kind: "phraseref".to_owned(),
                });
            }
            rules::PatternElementTypeContent::And(_) => {
                out.push(Construct::Unsupported {
                    kind: "and".to_owned(),
                });
            }
            rules::PatternElementTypeContent::Or(_) => {
                out.push(Construct::Unsupported {
                    kind: "or".to_owned(),
                });
            }
            rules::PatternElementTypeContent::Unify(_) => {
                out.push(Construct::Unsupported {
                    kind: "unify".to_owned(),
                });
            }
        }
    }
}

/// Lower the contents of a `<marker>` (the same construct vocabulary as `<pattern>`) into `out`.
fn lower_marker(content: &[pattern::MarkerElementTypeContent], out: &mut Vec<Construct>) {
    for item in content {
        match item {
            pattern::MarkerElementTypeContent::Token(t) => {
                out.push(Construct::Token(lower_token(t)));
            }
            pattern::MarkerElementTypeContent::Or(_) => {
                out.push(Construct::Unsupported {
                    kind: "or".to_owned(),
                });
            }
            pattern::MarkerElementTypeContent::And(_) => {
                out.push(Construct::Unsupported {
                    kind: "and".to_owned(),
                });
            }
            pattern::MarkerElementTypeContent::Unify(_) => {
                out.push(Construct::Unsupported {
                    kind: "unify".to_owned(),
                });
            }
            pattern::MarkerElementTypeContent::Phraseref(_) => {
                out.push(Construct::Unsupported {
                    kind: "phraseref".to_owned(),
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
        text: trimmed(t.text_before.as_ref()),
        postag: t.postag.clone(),
        regexp: is_yes(&t.regexp),
        negate: is_yes(&t.negate),
        inflected: is_yes(&t.inflected),
        min: t.min,
        max: t.max,
        skip: t.skip,
        case_sensitive: t.case_sensitive.as_ref().is_some_and(is_yes),
        exceptions,
    }
}

/// Lower a `<token>`'s `<exception>` into an [`ExceptionPat`].
fn lower_exception(e: &pattern::ExceptionElementType) -> ExceptionPat {
    ExceptionPat {
        text: trimmed(e.text.as_ref()),
        postag: e.postag.clone(),
        regexp: is_yes(&e.regexp),
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

/// LT's `yes`/`no` enum → bool.
fn is_yes(v: &pattern::BinaryYesNoType) -> bool {
    matches!(v, pattern::BinaryYesNoType::Yes)
}

/// Expand LT's internal-DTD general entities and strip the DOCTYPE, yielding standalone XML.
///
/// LT's `grammar.xml` declares ~40 `<!ENTITY name "value">` in an internal subset and references
/// them thousands of times. quick_xml does not expand DTD entities, so we do a textual pass:
/// remove the DOCTYPE, then iteratively substitute `&name;` until stable (entities may reference
/// other entities). Quote-bearing entity values appear only in element content in LT's grammar,
/// so textual substitution is safe here.
fn expand_entities(xml: &str) -> Result<String> {
    let Some(doctype_start) = xml.find("<!DOCTYPE") else {
        return Ok(xml.to_owned()); // No internal subset — nothing to expand.
    };
    let subset_end = xml[doctype_start..]
        .find("]>")
        .map(|i| i + doctype_start)
        .ok_or_else(|| anyhow!("unterminated DOCTYPE internal subset"))?;
    let doctype = &xml[doctype_start..subset_end];

    // <!ENTITY name "value"> and <!ENTITY name 'value'> — the delimiter is respected so values
    // may contain the other quote (e.g. <!ENTITY quote '["…"]'>).
    let double_q = Regex::new(r#"<!ENTITY\s+(\w+)\s+"([^"]*)""#)?;
    let single_q = Regex::new(r"<!ENTITY\s+(\w+)\s+'([^']*)'")?;
    let entities: Vec<(String, String)> = double_q
        .captures_iter(doctype)
        .chain(single_q.captures_iter(doctype))
        .map(|cap| (cap[1].to_owned(), cap[2].to_owned()))
        .collect();

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
        let out = expand_entities(xml).expect("expand");

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
        assert_eq!(expand_entities(xml).unwrap(), xml);
    }
}
