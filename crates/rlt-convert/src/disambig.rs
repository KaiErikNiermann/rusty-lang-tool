//! Lower LanguageTool's `disambiguation.xml` into [`rlt_ir::DisambigRule`]s.
//!
//! `disambiguation.xml` uses the same pattern vocabulary as `grammar.xml`, but it has no XSD (LT
//! validates it against a DTD), so the generated `lt_schema` bindings don't cover its `<disambig>`
//! tag-action element. Rather than codegen a schema, we deserialize it with a small hand-written
//! `serde` model (via quick-xml) and lower it to the shared IR — reusing [`rlt_ir::Construct`] /
//! [`rlt_ir::TokenPat`] so the runtime matcher can run these rules with the exact same engine it uses
//! for grammar rules.
//!
//! Constructs we can't faithfully run become [`TagAction::Unsupported`] (a `<match>` postag-synthesis,
//! `<unify>`, `filterall`, `ignore_spelling`, or a `chunk_re` token needing a chunker we don't have):
//! the rule is kept and named, but inert — so coverage stays a computed number, not a silent drop.

use anyhow::{Context, Result, anyhow};
use rlt_ir::{Construct, DisambigRule, ExceptionPat, TagAction, TokenPat};
use serde::Deserialize;

use crate::expand_entities;

/// Default location of LT's English `disambiguation.xml` after `cargo xtask fetch-lt`.
pub const DEFAULT_DISAMBIGUATION: &str = "resources/lt/_repo/languagetool-language-modules/en/src/main/resources/org/languagetool/resource/en/disambiguation.xml";

/// Summary of a disambiguation lowering run.
#[derive(Debug, Clone)]
pub struct DisambigReport {
    /// Every lowered rule (including the inert [`TagAction::Unsupported`] tail).
    pub rules: Vec<DisambigRule>,
    /// How many rules carry an applicable (non-`Unsupported`) action.
    pub applicable: usize,
}

/// Parse and lower `disambiguation.xml` at `path` into IR rules.
///
/// # Errors
/// Returns an error if the file cannot be read or the XML cannot be parsed.
pub fn lower_disambiguation(path: &std::path::Path) -> Result<DisambigReport> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let xml = expand_entities(&raw, path.parent())
        .with_context(|| format!("expanding entities in {}", path.display()))?;
    let parsed: XRules = quick_xml::de::from_str(&xml)
        .with_context(|| format!("deserializing {}", path.display()))?;

    let mut rules = Vec::new();
    for group in &parsed.rulegroup {
        for rule in &group.rule {
            rules.push(lower_rule(rule, group.id.as_deref()));
        }
    }
    for rule in &parsed.rule {
        rules.push(lower_rule(rule, None));
    }
    let applicable = rules.iter().filter(|r| !r.action.is_unsupported()).count();
    Ok(DisambigReport { rules, applicable })
}

/// Lower `disambiguation.xml` and serialize the rules to an rkyv artifact at `out`.
///
/// # Errors
/// Returns an error if the XML can't be parsed or the artifact can't be written.
pub fn convert_disambiguation(
    disambig_path: &std::path::Path,
    out: &std::path::Path,
) -> Result<DisambigReport> {
    let report = lower_disambiguation(disambig_path)?;
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&report.rules)
        .map_err(|e| anyhow!("serializing disambiguation rules: {e}"))?;
    std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
    Ok(report)
}

/// Lower one `<rule>` into a [`DisambigRule`]. The action is [`TagAction::Unsupported`] if the pattern
/// uses a construct we can't run (a `<match>`/`chunk_re` token, `<unify>`) or the `<disambig>` action
/// isn't one we apply.
fn lower_rule(rule: &XRule, group_id: Option<&str>) -> DisambigRule {
    let id = rule
        .id
        .as_deref()
        .or(group_id)
        .unwrap_or("DISAMBIG")
        .to_owned();
    let mut pattern = Vec::new();
    let supported_pattern = match &rule.pattern {
        Some(p) => {
            let ok = lower_items(&p.items, &mut pattern);
            // `<pattern case_sensitive="yes">` applies to all the pattern's tokens; the serde model
            // doesn't thread it down, so stamp it on after lowering (98 such patterns in en, 284 de).
            if ok && is_yes(p.case_sensitive.as_deref()) {
                crate::force_case_sensitive(&mut pattern);
            }
            ok
        }
        None => false,
    };
    let action = if supported_pattern && !pattern.is_empty() {
        rule.disambig
            .as_ref()
            .map_or(TagAction::Unsupported, lower_action)
    } else {
        TagAction::Unsupported
    };
    // Antipatterns suppress the action in excluded contexts. Drop (don't fail the rule for) any with an
    // unmatchable token — a missing antipattern only costs precision, mirroring the grammar matcher.
    let antipatterns = rule
        .antipattern
        .iter()
        .filter_map(|a| {
            let mut ap = Vec::new();
            (lower_items(&a.items, &mut ap) && !ap.is_empty()).then(|| {
                if is_yes(a.case_sensitive.as_deref()) {
                    crate::force_case_sensitive(&mut ap);
                }
                ap
            })
        })
        .collect();
    DisambigRule {
        id,
        pattern,
        antipatterns,
        action,
    }
}

/// Lower an ordered list of pattern items into constructs, returning `false` if any is unsupported
/// (a `<match>`/`chunk_re` token, `<unify>`) — in which case the rule must not run.
fn lower_items(items: &[XPatItem], out: &mut Vec<Construct>) -> bool {
    for item in items {
        match item {
            XPatItem::Token(t) => match lower_token(t) {
                Some(p) => out.push(Construct::Token(p)),
                None => return false,
            },
            XPatItem::Marker(m) => {
                out.push(Construct::MarkerStart);
                if !lower_items(&m.items, out) {
                    return false;
                }
                out.push(Construct::MarkerEnd);
            }
            XPatItem::And(g) => match lower_group(g) {
                Some(alts) => out.push(Construct::And(alts)),
                None => return false,
            },
            XPatItem::Or(g) => match lower_group(g) {
                Some(alts) => out.push(Construct::Or(alts)),
                None => return false,
            },
            // No chunker / unification support — the rule can't run faithfully.
            XPatItem::Unify(_) | XPatItem::Feature(_) => return false,
        }
    }
    true
}

/// Lower an `<and>`/`<or>` group's tokens, or `None` if any token is unsupported.
fn lower_group(g: &XGroup) -> Option<Vec<TokenPat>> {
    let alts: Option<Vec<TokenPat>> = g.token.iter().map(lower_token).collect();
    alts.filter(|a| !a.is_empty())
}

/// Lower a `<token>` to a [`TokenPat`], or `None` if it carries a `<match>` back-reference or a
/// `chunk`/`chunk_re` constraint we can't evaluate.
fn lower_token(t: &XToken) -> Option<TokenPat> {
    if !t.r#match.is_empty() || t.chunk_re.is_some() || t.chunk.is_some() {
        return None;
    }
    Some(TokenPat {
        text: trimmed(t.text.as_deref()),
        postag: t.postag.clone(),
        regexp: is_yes(t.regexp.as_deref()),
        postag_regexp: is_yes(t.postag_regexp.as_deref()),
        negate: is_yes(t.negate.as_deref()),
        inflected: is_yes(t.inflected.as_deref()),
        min: t.min,
        max: t.max,
        skip: t.skip,
        case_sensitive: is_yes(t.case_sensitive.as_deref()),
        space_before: None, // disambiguation.xml's hand-written model doesn't parse spacebefore (rare there)
        exceptions: t.exception.iter().map(lower_exception).collect(),
    })
}

fn lower_exception(e: &XException) -> ExceptionPat {
    ExceptionPat {
        text: trimmed(e.text.as_deref()),
        postag: e.postag.clone(),
        regexp: is_yes(e.regexp.as_deref()),
        postag_regexp: is_yes(e.postag_regexp.as_deref()),
        inflected: is_yes(e.inflected.as_deref()),
        negate: is_yes(e.negate.as_deref()),
        case_sensitive: is_yes(e.case_sensitive.as_deref()),
    }
}

/// Lower a `<disambig>` element into a [`TagAction`]. A `<match>` child means postag synthesis we
/// don't support; `filterall`/`unify`/`ignore_spelling` aren't applied.
fn lower_action(d: &XDisambig) -> TagAction {
    if !d.r#match.is_empty() {
        return TagAction::Unsupported;
    }
    // Readings: the `<disambig postag=…>` shorthand and each `<wd pos=… lemma=…>` child.
    let mut postags: Vec<String> = d.postag.iter().cloned().collect();
    let mut lemmas = Vec::new();
    for wd in &d.wd {
        if let Some(p) = &wd.pos {
            postags.push(p.clone());
        }
        if let Some(l) = &wd.lemma {
            lemmas.push(l.clone());
        }
    }
    let postag_regexp = is_yes(d.postag_regexp.as_deref());
    match d.action.as_deref().unwrap_or("replace") {
        "replace" => TagAction::Replace { postags, lemmas },
        "add" => TagAction::Add { postags, lemmas },
        "remove" => TagAction::Remove {
            postags,
            lemmas,
            postag_regexp,
        },
        "filter" => TagAction::Filter {
            postags,
            postag_regexp,
        },
        _ => TagAction::Unsupported, // filterall, unify, ignore_spelling
    }
}

/// A trimmed, non-empty string.
fn trimmed(text: Option<&str>) -> Option<String> {
    text.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// LT's `yes`/`no` attribute → bool (absent = false).
fn is_yes(v: Option<&str>) -> bool {
    v.is_some_and(|s| s.eq_ignore_ascii_case("yes"))
}

// ---- The serde model mirroring disambiguation.xml (only the bits we lower) --------------------

#[derive(Debug, Deserialize)]
struct XRules {
    #[serde(default)]
    rulegroup: Vec<XRulegroup>,
    #[serde(default)]
    rule: Vec<XRule>,
}

#[derive(Debug, Deserialize)]
struct XRulegroup {
    #[serde(rename = "@id")]
    id: Option<String>,
    #[serde(default)]
    rule: Vec<XRule>,
}

#[derive(Debug, Deserialize)]
struct XRule {
    #[serde(rename = "@id")]
    id: Option<String>,
    pattern: Option<XPattern>,
    #[serde(default)]
    antipattern: Vec<XPattern>,
    disambig: Option<XDisambig>,
}

#[derive(Debug, Deserialize)]
struct XPattern {
    #[serde(rename = "@case_sensitive")]
    case_sensitive: Option<String>,
    #[serde(rename = "$value", default)]
    items: Vec<XPatItem>,
}

#[derive(Debug, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "transient parse type; boxing would complicate the serde derive for no runtime benefit"
)]
enum XPatItem {
    #[serde(rename = "token")]
    Token(XToken),
    #[serde(rename = "marker")]
    Marker(XMarker),
    #[serde(rename = "and")]
    And(XGroup),
    #[serde(rename = "or")]
    Or(XGroup),
    #[serde(rename = "unify")]
    Unify(serde::de::IgnoredAny),
    #[serde(rename = "feature")]
    Feature(serde::de::IgnoredAny),
}

#[derive(Debug, Deserialize)]
struct XMarker {
    #[serde(rename = "$value", default)]
    items: Vec<XPatItem>,
}

#[derive(Debug, Deserialize)]
struct XGroup {
    #[serde(default)]
    token: Vec<XToken>,
}

#[derive(Debug, Default, Deserialize)]
struct XToken {
    #[serde(rename = "@postag")]
    postag: Option<String>,
    #[serde(rename = "@postag_regexp")]
    postag_regexp: Option<String>,
    #[serde(rename = "@regexp")]
    regexp: Option<String>,
    #[serde(rename = "@negate")]
    negate: Option<String>,
    #[serde(rename = "@inflected")]
    inflected: Option<String>,
    #[serde(rename = "@min")]
    min: Option<i32>,
    #[serde(rename = "@max")]
    max: Option<i32>,
    #[serde(rename = "@skip")]
    skip: Option<i32>,
    #[serde(rename = "@case_sensitive")]
    case_sensitive: Option<String>,
    #[serde(rename = "@chunk")]
    chunk: Option<String>,
    #[serde(rename = "@chunk_re")]
    chunk_re: Option<String>,
    #[serde(rename = "$text")]
    text: Option<String>,
    #[serde(default)]
    exception: Vec<XException>,
    #[serde(default, rename = "match")]
    r#match: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, Default, Deserialize)]
struct XException {
    #[serde(rename = "@postag")]
    postag: Option<String>,
    #[serde(rename = "@postag_regexp")]
    postag_regexp: Option<String>,
    #[serde(rename = "@regexp")]
    regexp: Option<String>,
    #[serde(rename = "@negate")]
    negate: Option<String>,
    #[serde(rename = "@inflected")]
    inflected: Option<String>,
    #[serde(rename = "@case_sensitive")]
    case_sensitive: Option<String>,
    #[serde(rename = "$text")]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XDisambig {
    #[serde(rename = "@action")]
    action: Option<String>,
    #[serde(rename = "@postag")]
    postag: Option<String>,
    #[serde(rename = "@postag_regexp")]
    postag_regexp: Option<String>,
    #[serde(default)]
    wd: Vec<XWd>,
    #[serde(default, rename = "match")]
    r#match: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, Deserialize)]
struct XWd {
    #[serde(rename = "@pos")]
    pos: Option<String>,
    #[serde(rename = "@lemma")]
    lemma: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `<pattern case_sensitive="yes">` in disambiguation.xml must make its tokens
    /// case-sensitive (98 such patterns in en, 284 in de) — else uppercase regexes mis-tag.
    #[test]
    fn pattern_case_sensitive_propagates_in_disambig() {
        let xml = "<rules><rule id=\"X\"><pattern case_sensitive=\"yes\">\
                   <token regexp=\"yes\">[A-Z]+</token></pattern>\
                   <disambig postag=\"NNP\"/></rule></rules>";
        let parsed: XRules = quick_xml::de::from_str(xml).expect("parse");
        let rule = lower_rule(&parsed.rule[0], None);
        assert!(
            rule.pattern
                .iter()
                .any(|c| matches!(c, Construct::Token(t) if t.case_sensitive)),
            "disambig pattern-level case_sensitive was not propagated to the token",
        );
    }

    #[test]
    fn lowers_real_disambiguation_xml() {
        let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../"))
            .join(DEFAULT_DISAMBIGUATION);
        if !path.exists() {
            eprintln!("skip: {} not fetched", path.display());
            return;
        }
        let report = lower_disambiguation(&path).expect("lower disambiguation.xml");
        // The file has ~1061 <disambig> actions; most should lower to an applicable action.
        let n = report.rules.len();
        let applied = report.applicable;
        let action_mix =
            |f: fn(&TagAction) -> bool| report.rules.iter().filter(|r| f(&r.action)).count();
        eprintln!(
            "disambig: {n} rules, {applied} applicable | replace={} add={} remove={} filter={} unsupported={}",
            action_mix(|a| matches!(a, TagAction::Replace { .. })),
            action_mix(|a| matches!(a, TagAction::Add { .. })),
            action_mix(|a| matches!(a, TagAction::Remove { .. })),
            action_mix(|a| matches!(a, TagAction::Filter { .. })),
            action_mix(TagAction::is_unsupported),
        );
        assert!(n > 800, "expected ~1000 rules, got {n}");
        assert!(
            applied > 500,
            "expected most rules applicable, got {applied}"
        );
    }
}
