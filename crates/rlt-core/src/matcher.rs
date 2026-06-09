//! L2 — matching `rlt-ir` rules (compiled from *current* LanguageTool) over the token graph.
//!
//! This is the on-thesis differentiator: instead of nlprule's bundled v5.2 rules, it runs the
//! rules our converter compiled from LT v6.7. It is scored by the same example oracle as the
//! nlprule baseline, so the two are directly comparable.
//!
//! Scope: literal/regex/POS token matching with `inflected`, `negate`, `case_sensitive`,
//! `<exception>`s, `min`/`max`/`skip` quantifiers, `<or>`/`<and>` groups, `<marker>` spans,
//! `<antipattern>` suppression, rule-level `<regexp>` rules (matched over text), and suggestion
//! rendering — literal text + `<match no>`/`\N` token copies with case conversion and
//! `regexp_replace` transforms. `<phraseref>`s are inlined at convert time. Suggestions needing
//! morphological synthesis (`postag_replace`) are dropped, and rules left with an unsupported
//! construct (`<unify>` — unused in English — or `<filter>`) are skipped rather than matched wrong.

use std::collections::HashMap;

use regex::Regex;
use rlt_ir::{Case, Construct, ExceptionPat, Rule, SugPart, Suggestion as IrSuggestion, TokenPat};

use crate::{Analysis, Diagnostic, GrammarChecker, Source, Span, Suggestion, Token};

/// Cap for unbounded (`skip="-1"` / `max="-1"`) quantifiers, to bound backtracking.
const UNBOUNDED_CAP: usize = 64;

/// The IR-rule grammar checker: compiles a rule set once, then matches it over each analysis.
pub struct IrMatcher {
    rules: Vec<CompiledRule>,
    /// Indices of rules whose first element is a plain literal word → only tried where it occurs.
    by_first_literal: HashMap<String, Vec<usize>>,
    /// Indices of rules whose first element is not a plain literal → tried at every position.
    general: Vec<usize>,
    /// Rule-level `<regexp>` rules, matched as a regex over the sentence text.
    regexp_rules: Vec<CompiledRegexpRule>,
}

impl IrMatcher {
    /// Compile a rule set (skipping rules that cannot be matched faithfully).
    #[must_use]
    pub fn new(rules: &[Rule]) -> Self {
        let mut compiled = Vec::new();
        let mut by_first_literal: HashMap<String, Vec<usize>> = HashMap::new();
        let mut general = Vec::new();
        let mut regexp_rules = Vec::new();
        for r in rules {
            if r.pattern
                .iter()
                .any(|c| matches!(c, Construct::Regexp { .. }))
            {
                regexp_rules.extend(compile_regexp_rule(r));
                continue;
            }
            let Some(rule) = compile_rule(r) else {
                continue;
            };
            let idx = compiled.len();
            match rule.first_literal() {
                Some(word) => by_first_literal.entry(word).or_default().push(idx),
                None => general.push(idx),
            }
            compiled.push(rule);
        }
        Self {
            rules: compiled,
            by_first_literal,
            general,
            regexp_rules,
        }
    }

    /// Compile a rule set from the converter's rkyv artifact bytes.
    ///
    /// # Errors
    /// Returns an error if `bytes` is not a valid archived `Vec<Rule>`.
    pub fn from_rkyv_bytes(bytes: &[u8]) -> Result<Self, rkyv::rancor::Error> {
        Ok(Self::new(&rlt_ir::deserialize_rules(bytes)?))
    }

    /// Number of rules successfully compiled (the matchable subset, including regexp rules).
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len() + self.regexp_rules.len()
    }

    fn diagnostics(&self, text: &str, tokens: &[Token]) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = Vec::new();
        // Lower-case each surface once for the first-literal index probe, not once per start.
        let lower: Vec<String> = tokens.iter().map(|t| t.text.to_ascii_lowercase()).collect();
        // Reused across every (start, rule) pair so the per-element capture slots are not
        // reallocated for each of the thousands of rule attempts per token.
        let mut captures: Vec<Option<(usize, usize)>> = Vec::new();
        for (start, key) in lower.iter().enumerate() {
            let literal = self
                .by_first_literal
                .get(key)
                .map_or(&[][..], Vec::as_slice);
            for &ri in self.general.iter().chain(literal) {
                let rule = &self.rules[ri];
                captures.clear();
                captures.resize(rule.elements.len(), None);
                if let Some(end) = match_elements(&rule.elements, 0, tokens, start, &mut captures) {
                    if suppressed(rule, tokens, start, end) {
                        continue;
                    }
                    if let Some(d) = render(rule, text, tokens, start, end, &captures) {
                        out.push(d);
                    }
                }
            }
        }
        for rr in &self.regexp_rules {
            rr.diagnostics(text, &mut out);
        }
        // De-duplicate identical (span, rule) hits produced at overlapping start positions.
        out.sort_by(|a, b| {
            (a.span.start, a.span.end, &a.code).cmp(&(b.span.start, b.span.end, &b.code))
        });
        out.dedup_by(|a, b| a.span == b.span && a.code == b.code);
        out
    }
}

impl GrammarChecker for IrMatcher {
    fn grammar_diagnostics(&self, text: &str, analysis: &Analysis) -> Vec<Diagnostic> {
        self.diagnostics(text, &analysis.tokens)
    }
}

/// A compiled rule: pattern elements, the marker element-range, correction templates, and the
/// antipatterns that suppress it.
struct CompiledRule {
    id: String,
    message: String,
    elements: Vec<Elem>,
    marker: Option<(usize, usize)>,
    suggestions: Vec<CompiledSuggestion>,
    /// Antipattern element sequences; if any matches overlapping the rule's match, suppress it.
    antipatterns: Vec<Vec<Elem>>,
}

/// A correction template with any `regexp_replace` transforms pre-compiled.
struct CompiledSuggestion {
    parts: Vec<CompiledPart>,
}

enum CompiledPart {
    Text(String),
    /// Copy the Nth matched token's surface, apply the optional regex substitution, then `case`.
    Token {
        no: usize,
        case: Case,
        regex: Option<(Regex, String)>,
    },
}

impl CompiledRule {
    /// The first element's literal lower-cased word, if it is an un-negated literal-text token —
    /// used to index the rule so it is only attempted where that word occurs.
    fn first_literal(&self) -> Option<String> {
        let Matcher::One(m) = &self.elements.first()?.matcher else {
            return None; // or/and groups are tried at every position
        };
        match (&m.text, m.negate, &m.postag) {
            (Some(TextMatch::Literal { value, .. }), false, None) if !m.inflected => {
                Some(value.to_ascii_lowercase())
            }
            _ => None,
        }
    }
}

/// One pattern element: a token matcher plus its `min`/`max`/`skip` quantifiers.
struct Elem {
    matcher: Matcher,
    min: usize,
    max: usize,
    skip: usize,
}

/// How a single token position is matched.
enum Matcher {
    /// A single `<token>` constraint.
    One(TokenMatcher),
    /// `<or>`: the position matches if **any** alternative matches.
    Or(Vec<TokenMatcher>),
    /// `<and>`: the position matches if **all** constraints hold on the token.
    And(Vec<TokenMatcher>),
}

/// A single token constraint: text and/or POS, with `inflected`/`negate`/exceptions.
struct TokenMatcher {
    text: Option<TextMatch>,
    postag: Option<TagMatch>,
    inflected: bool,
    negate: bool,
    exceptions: Vec<TokenMatcher>,
}

enum TextMatch {
    Literal { value: String, case_sensitive: bool },
    Regex(Regex),
}

enum TagMatch {
    Literal(String),
    Regex(Regex),
}

/// Compile a rule, or `None` if it cannot be matched faithfully (unsupported construct, no
/// suggestions, empty pattern, or an uncompilable main regex).
fn compile_rule(r: &Rule) -> Option<CompiledRule> {
    // Compile suggestions up front (pre-compiling regex transforms); a rule with no renderable
    // suggestion cannot produce a correction, so skip it.
    let suggestions = compile_suggestions(&r.suggestions)?;
    let mut elements = Vec::new();
    let mut marker_start = None;
    let mut marker = None;
    for c in &r.pattern {
        match c {
            Construct::MarkerStart => marker_start = Some(elements.len()),
            Construct::MarkerEnd => marker = marker_start.map(|s| (s, elements.len())),
            Construct::Token(_) | Construct::Or(_) | Construct::And(_) => {
                elements.push(compile_construct(c)?);
            }
            // Unsupported/Opaque (and any future construct) can't be matched faithfully — skip.
            _ => return None,
        }
    }
    if elements.is_empty() {
        return None;
    }
    // Compile antipatterns; drop (rather than skip the rule for) any that cannot be matched —
    // a missing antipattern only costs precision, whereas dropping the rule costs recall.
    let antipatterns = r
        .antipatterns
        .iter()
        .filter_map(|ap| compile_elements(ap))
        .collect();
    Some(CompiledRule {
        id: r.id.clone(),
        message: r.message.clone(),
        elements,
        marker,
        suggestions,
        antipatterns,
    })
}

/// Pre-compile a rule's suggestion templates, or `None` if none render — a rule with no renderable
/// correction cannot produce a fix, so the caller skips it.
fn compile_suggestions(suggestions: &[IrSuggestion]) -> Option<Vec<CompiledSuggestion>> {
    let compiled: Vec<CompiledSuggestion> =
        suggestions.iter().filter_map(compile_suggestion).collect();
    (!compiled.is_empty()).then_some(compiled)
}

/// Pre-compile a suggestion's parts (compiling any `regexp_replace`); `None` if a transform regex
/// or an unknown part makes it unrenderable.
fn compile_suggestion(s: &IrSuggestion) -> Option<CompiledSuggestion> {
    let mut parts = Vec::new();
    for p in &s.parts {
        match p {
            SugPart::Text(t) => parts.push(CompiledPart::Text(t.clone())),
            SugPart::Token {
                no,
                case,
                transform,
            } => {
                let regex = match transform {
                    Some((m, r)) => Some((Regex::new(m).ok()?, r.clone())),
                    None => None,
                };
                parts.push(CompiledPart::Token {
                    no: *no,
                    case: *case,
                    regex,
                });
            }
            _ => return None,
        }
    }
    Some(CompiledSuggestion { parts })
}

/// A compiled rule-level `<regexp>` rule, matched as a regex over the sentence text.
struct CompiledRegexpRule {
    id: String,
    message: String,
    regex: Regex,
    mark: Option<usize>,
    suggestions: Vec<CompiledSuggestion>,
}

impl CompiledRegexpRule {
    /// Append a diagnostic for each regex match over `text`.
    fn diagnostics(&self, text: &str, out: &mut Vec<Diagnostic>) {
        for caps in self.regex.captures_iter(text) {
            let Some(m) = self.mark.and_then(|g| caps.get(g)).or_else(|| caps.get(0)) else {
                continue;
            };
            let suggestions = dedup_suggestions(
                self.suggestions
                    .iter()
                    .map(|s| render_regexp_suggestion(s, &caps)),
            );
            if suggestions.is_empty() {
                continue;
            }
            out.push(Diagnostic {
                span: Span {
                    start: m.start(),
                    end: m.end(),
                },
                code: self.id.clone(),
                message: self.message.clone(),
                suggestions,
                source: Source::Grammar,
            });
        }
    }
}

/// Compile a rule whose pattern is a rule-level `<regexp>`; `None` if the regex does not compile or
/// no suggestion renders.
fn compile_regexp_rule(r: &Rule) -> Option<CompiledRegexpRule> {
    let (pattern, mark, case_sensitive) = r.pattern.iter().find_map(|c| match c {
        Construct::Regexp {
            pattern,
            mark,
            case_sensitive,
        } => Some((pattern, *mark, *case_sensitive)),
        _ => None,
    })?;
    let src = if case_sensitive {
        pattern.clone()
    } else {
        format!("(?i){pattern}")
    };
    let regex = Regex::new(&src).ok()?;
    let suggestions = compile_suggestions(&r.suggestions)?;
    Some(CompiledRegexpRule {
        id: r.id.clone(),
        message: r.message.clone(),
        regex,
        mark,
        suggestions,
    })
}

/// Render a suggestion against regex capture groups (`<match no="N">` / `\N` → group N).
fn render_regexp_suggestion(sug: &CompiledSuggestion, caps: &regex::Captures) -> Option<String> {
    let mut out = String::new();
    for part in &sug.parts {
        match part {
            CompiledPart::Text(t) => out.push_str(t),
            CompiledPart::Token { no, case, regex } => {
                let surface = caps.get(*no)?.as_str();
                let transformed = match regex {
                    Some((re, rep)) => re.replace_all(surface, rep.as_str()).into_owned(),
                    None => surface.to_owned(),
                };
                out.push_str(&apply_case(&transformed, *case));
            }
        }
    }
    Some(out)
}

/// Collect rendered suggestion strings into [`Suggestion`]s, dropping empties and duplicates
/// (order-preserving) — the same templates can render to the same correction.
fn dedup_suggestions(rendered: impl Iterator<Item = Option<String>>) -> Vec<Suggestion> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for rep in rendered.flatten() {
        if !rep.is_empty() && !seen.contains(&rep) {
            seen.push(rep.clone());
            out.push(Suggestion { replacement: rep });
        }
    }
    out
}

/// Compile a construct list (antipattern body) into matchable elements; `None` if it contains an
/// unsupported construct or an uncompilable regex (so it cannot suppress). Markers are ignored.
fn compile_elements(constructs: &[Construct]) -> Option<Vec<Elem>> {
    let mut elements = Vec::new();
    for c in constructs {
        match c {
            Construct::Token(_) | Construct::Or(_) | Construct::And(_) => {
                elements.push(compile_construct(c)?);
            }
            Construct::MarkerStart | Construct::MarkerEnd => {}
            _ => return None,
        }
    }
    (!elements.is_empty()).then_some(elements)
}

/// Compile a single-position construct (`<token>`/`<or>`/`<and>`) into an [`Elem`]; `None` if a
/// regex fails to compile. Group elements (`<or>`/`<and>`) occupy exactly one token position.
fn compile_construct(c: &Construct) -> Option<Elem> {
    let (matcher, quant) = match c {
        Construct::Token(t) => (Matcher::One(compile_token(t)?), Some(t)),
        Construct::Or(alts) => (
            Matcher::Or(alts.iter().map(compile_token).collect::<Option<_>>()?),
            None,
        ),
        Construct::And(alts) => (
            Matcher::And(alts.iter().map(compile_token).collect::<Option<_>>()?),
            None,
        ),
        _ => return None,
    };
    let (min, max, skip) = quant.map_or((1, 1, 0), quantifiers);
    Some(Elem {
        matcher,
        min,
        max,
        skip,
    })
}

/// The `min`/`max`/`skip` quantifiers of a token (unbounded `-1` capped at [`UNBOUNDED_CAP`]).
fn quantifiers(t: &TokenPat) -> (usize, usize, usize) {
    let min = usize::try_from(t.min.unwrap_or(1)).unwrap_or(1);
    let max = match t.max {
        Some(-1) => UNBOUNDED_CAP,
        Some(m) => usize::try_from(m).unwrap_or(1).max(min),
        None => min.max(1),
    };
    let skip = match t.skip {
        Some(-1) => UNBOUNDED_CAP,
        Some(s) => usize::try_from(s).unwrap_or(0),
        None => 0,
    };
    (min, max, skip)
}

/// Compile a [`TokenPat`] into a single [`TokenMatcher`]; `None` if a regex does not compile.
fn compile_token(t: &TokenPat) -> Option<TokenMatcher> {
    let (text, postag) = compile_constraint(
        t.text.as_deref(),
        t.postag.as_deref(),
        t.regexp,
        t.postag_regexp,
        t.case_sensitive,
    )?;
    // Exceptions: drop any whose regex fails to compile rather than discard the whole rule.
    let exceptions = t.exceptions.iter().filter_map(compile_exception).collect();
    Some(TokenMatcher {
        text,
        postag,
        inflected: t.inflected,
        negate: t.negate,
        exceptions,
    })
}

fn compile_exception(e: &ExceptionPat) -> Option<TokenMatcher> {
    let (text, postag) = compile_constraint(
        e.text.as_deref(),
        e.postag.as_deref(),
        e.regexp,
        e.postag_regexp,
        e.case_sensitive,
    )?;
    Some(TokenMatcher {
        text,
        postag,
        inflected: e.inflected,
        negate: e.negate,
        exceptions: Vec::new(),
    })
}

/// Compile the text + POS-tag constraints shared by tokens and exceptions. `text`/`postag` are
/// regexes when their respective `regexp` flag is set (the converter carries `postag_regexp`
/// explicitly, so the runtime never guesses). `None` if a regex fails to compile.
fn compile_constraint(
    text: Option<&str>,
    postag: Option<&str>,
    regexp: bool,
    postag_regexp: bool,
    case_sensitive: bool,
) -> Option<(Option<TextMatch>, Option<TagMatch>)> {
    let text = match text {
        Some(s) if regexp => Some(TextMatch::Regex(anchored(s, !case_sensitive)?)),
        Some(s) => Some(TextMatch::Literal {
            value: s.to_owned(),
            case_sensitive,
        }),
        None => None,
    };
    let postag = match postag {
        Some(p) if postag_regexp => Some(TagMatch::Regex(anchored(p, false)?)),
        Some(p) => Some(TagMatch::Literal(p.to_owned())),
        None => None,
    };
    Some((text, postag))
}

/// Compile a whole-token-anchored regex (optionally case-insensitive). `None` on regex error.
fn anchored(pattern: &str, case_insensitive: bool) -> Option<Regex> {
    let prefix = if case_insensitive { "(?i)" } else { "" };
    Regex::new(&format!("{prefix}^(?:{pattern})$")).ok()
}

impl Matcher {
    /// Whether this position-matcher accepts `token`.
    fn matches(&self, token: &Token) -> bool {
        match self {
            Matcher::One(m) => m.matches(token),
            Matcher::Or(ms) => ms.iter().any(|m| m.matches(token)),
            Matcher::And(ms) => ms.iter().all(|m| m.matches(token)),
        }
    }
}

impl TokenMatcher {
    /// Whether this matcher accepts `token` (applying negation and exceptions).
    fn matches(&self, token: &Token) -> bool {
        let mut hit = self.constraint(token);
        if self.negate {
            hit = !hit;
        }
        if !hit {
            return false;
        }
        // Disqualified if any exception matches.
        !self.exceptions.iter().any(|e| {
            let mut m = e.constraint(token);
            if e.negate {
                m = !m;
            }
            m
        })
    }

    /// The raw text-AND-POS constraint, ignoring negation/exceptions.
    fn constraint(&self, token: &Token) -> bool {
        let text_ok = match &self.text {
            None => true,
            Some(TextMatch::Regex(re)) => re.is_match(&token.text),
            Some(TextMatch::Literal {
                value,
                case_sensitive,
            }) => {
                if self.inflected {
                    token.lemmas.iter().any(|l| eq(l, value, *case_sensitive))
                } else {
                    eq(&token.text, value, *case_sensitive)
                }
            }
        };
        let tag_ok = match &self.postag {
            None => true,
            Some(TagMatch::Regex(re)) => token.tags.iter().any(|t| re.is_match(t)),
            Some(TagMatch::Literal(p)) => token.tags.iter().any(|t| t == p),
        };
        text_ok && tag_ok
    }
}

/// String equality, optionally case-insensitive (ASCII-fold).
fn eq(a: &str, b: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        a == b
    } else {
        a.eq_ignore_ascii_case(b)
    }
}

/// Backtracking match of `elements[ei..]` against `tokens[ti..]`; returns the end token index on
/// success and records each element's captured token range in `captures`.
fn match_elements(
    elements: &[Elem],
    ei: usize,
    tokens: &[Token],
    ti: usize,
    captures: &mut [Option<(usize, usize)>],
) -> Option<usize> {
    if ei == elements.len() {
        return Some(ti);
    }
    let elem = &elements[ei];
    let remaining = tokens.len() - ti;
    let max = elem.max.min(remaining);

    for count in elem.min..=max {
        if count > 0 && !(ti..ti + count).all(|k| elem.matcher.matches(&tokens[k])) {
            break; // contiguous run broken — larger counts cannot match either
        }
        captures[ei] = (count > 0).then_some((ti, ti + count));
        let base = ti + count;
        let skip_end = (base + elem.skip).min(tokens.len());
        for next in base..=skip_end {
            if let Some(end) = match_elements(elements, ei + 1, tokens, next, captures) {
                return Some(end);
            }
        }
    }
    captures[ei] = None;
    None
}

/// Whether any of `rule`'s antipatterns matches a token range overlapping the rule's match
/// `[start, end)` — in which case the rule is suppressed.
fn suppressed(rule: &CompiledRule, tokens: &[Token], start: usize, end: usize) -> bool {
    rule.antipatterns.iter().any(|ap| {
        let mut caps = vec![None; ap.len()];
        // Overlap requires the antipattern to start before the rule match ends.
        (0..end).any(|from| {
            caps.fill(None);
            matches!(
                match_elements(ap, 0, tokens, from, &mut caps),
                Some(stop) if stop > from && start < stop
            )
        })
    })
}

/// Build a diagnostic from a successful match, or `None` if the marker is empty or no suggestion
/// renders.
fn render(
    rule: &CompiledRule,
    text: &str,
    tokens: &[Token],
    start: usize,
    end: usize,
    captures: &[Option<(usize, usize)>],
) -> Option<Diagnostic> {
    let (mtok_start, mtok_end) = marker_token_range(rule, captures, start, end)?;
    let span = Span {
        start: tokens[mtok_start].span.start,
        end: tokens[mtok_end - 1].span.end,
    };

    let suggestions = dedup_suggestions(
        rule.suggestions
            .iter()
            .map(|s| render_suggestion(s, text, tokens, captures)),
    );
    if suggestions.is_empty() {
        return None;
    }
    Some(Diagnostic {
        span,
        code: rule.id.clone(),
        message: rule.message.clone(),
        suggestions,
        source: Source::Grammar,
    })
}

/// Token-index range `[start, end)` the diagnostic span covers: the marked elements' captures, or
/// the whole match when there is no `<marker>`.
fn marker_token_range(
    rule: &CompiledRule,
    captures: &[Option<(usize, usize)>],
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let Some((mi_start, mi_end)) = rule.marker else {
        return (end > start).then_some((start, end));
    };
    let mut lo = usize::MAX;
    let mut hi = 0;
    for cap in captures.iter().take(mi_end).skip(mi_start).flatten() {
        lo = lo.min(cap.0);
        hi = hi.max(cap.1);
    }
    (hi > lo).then_some((lo, hi))
}

/// Render one suggestion template against the captured tokens, or `None` if a token reference is
/// missing.
fn render_suggestion(
    sug: &CompiledSuggestion,
    text: &str,
    tokens: &[Token],
    captures: &[Option<(usize, usize)>],
) -> Option<String> {
    let mut out = String::new();
    for part in &sug.parts {
        match part {
            CompiledPart::Text(t) => out.push_str(t),
            CompiledPart::Token { no, case, regex } => {
                let (ts, te) = captures.get(no.checked_sub(1)?).copied().flatten()?;
                let surface = text.get(tokens[ts].span.start..tokens[te - 1].span.end)?;
                let transformed = match regex {
                    Some((re, rep)) => re.replace_all(surface, rep.as_str()).into_owned(),
                    None => surface.to_owned(),
                };
                out.push_str(&apply_case(&transformed, *case));
            }
        }
    }
    Some(out)
}

/// Apply a [`Case`] transform to a copied token surface.
fn apply_case(s: &str, case: Case) -> String {
    match case {
        Case::Keep => s.to_owned(),
        Case::Upper => s.to_uppercase(),
        Case::Lower => s.to_lowercase(),
        Case::StartUpper => crate::capitalize_first(s),
    }
}

#[cfg(test)]
mod tests {
    use rlt_ir::TokenPat;

    use super::*;

    /// Build an analysis by splitting on spaces, with given per-token (tags, lemmas).
    fn analyze(text: &str, tagged: &[(&str, &[&str], &[&str])]) -> Analysis {
        let mut tokens = Vec::new();
        let mut pos = 0;
        for (i, raw) in text.split(' ').enumerate() {
            let start = pos;
            pos += raw.len() + 1; // +1 for the space
            let (_, tags, lemmas) = tagged[i];
            tokens.push(Token {
                text: raw.to_owned(),
                span: Span {
                    start,
                    end: start + raw.len(),
                },
                tags: tags.iter().map(|s| (*s).to_owned()).collect(),
                lemmas: lemmas.iter().map(|s| (*s).to_owned()).collect(),
            });
        }
        Analysis { tokens }
    }

    fn lit(word: &str) -> Construct {
        Construct::Token(TokenPat {
            text: Some(word.to_owned()),
            ..Default::default()
        })
    }

    #[test]
    fn matches_should_of_and_renders_literal_suggestion() {
        // Rule: <marker>should of</marker> -> "should have"
        let rule = Rule {
            id: "MODAL_OF".to_owned(),
            pattern: vec![
                Construct::MarkerStart,
                lit("should"),
                lit("of"),
                Construct::MarkerEnd,
            ],
            antipatterns: vec![],
            message: "Did you mean “should have”?".to_owned(),
            suggestions: vec![IrSuggestion {
                parts: vec![SugPart::Text("should have".to_owned())],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);

        let text = "It should of worked";
        let analysis = analyze(
            text,
            &[
                ("It", &["PRP"], &["it"]),
                ("should", &["MD"], &["should"]),
                ("of", &["IN"], &["of"]),
                ("worked", &["VBD"], &["work"]),
            ],
        );
        let diags = matcher.grammar_diagnostics(text, &analysis);

        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert_eq!(diags[0].code, "MODAL_OF");
        assert_eq!(diags[0].suggestions[0].replacement, "should have");
        // Span covers exactly "should of".
        assert_eq!(&text[diags[0].span.start..diags[0].span.end], "should of");
    }

    #[test]
    fn inflected_and_match_reference_render() {
        // Rule: <marker><token inflected>be</token></marker> <token postag=DT/> -> match copy.
        let rule = Rule {
            id: "DEMO".to_owned(),
            pattern: vec![
                Construct::MarkerStart,
                Construct::Token(TokenPat {
                    text: Some("be".to_owned()),
                    inflected: true,
                    ..Default::default()
                }),
                Construct::MarkerEnd,
                Construct::Token(TokenPat {
                    postag: Some("DT".to_owned()),
                    ..Default::default()
                }),
            ],
            antipatterns: vec![],
            message: String::new(),
            suggestions: vec![IrSuggestion {
                parts: vec![SugPart::Token {
                    no: 1,
                    case: Case::StartUpper,
                    transform: None,
                }],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);

        let text = "were the best";
        let analysis = analyze(
            text,
            &[
                ("were", &["VBD"], &["be"]),
                ("the", &["DT"], &["the"]),
                ("best", &["JJS"], &["best"]),
            ],
        );
        let diags = matcher.grammar_diagnostics(text, &analysis);
        assert_eq!(
            diags.len(),
            1,
            "inflected `were`→lemma `be` should match: {diags:?}"
        );
        // <match no=1> copies the first token surface ("were"), StartUpper → "Were".
        assert_eq!(diags[0].suggestions[0].replacement, "Were");
    }

    #[test]
    fn exception_suppresses_match() {
        // Token matches any "run" EXCEPT when tagged NN.
        let rule = Rule {
            id: "EXC".to_owned(),
            pattern: vec![Construct::Token(TokenPat {
                text: Some("run".to_owned()),
                exceptions: vec![rlt_ir::ExceptionPat {
                    postag: Some("NN".to_owned()),
                    ..Default::default()
                }],
                ..Default::default()
            })],
            antipatterns: vec![],
            message: String::new(),
            suggestions: vec![IrSuggestion {
                parts: vec![SugPart::Text("x".to_owned())],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);

        let noun = analyze("run", &[("run", &["NN"], &["run"])]);
        assert!(
            matcher.grammar_diagnostics("run", &noun).is_empty(),
            "NN exception suppresses"
        );

        let verb = analyze("run", &[("run", &["VB"], &["run"])]);
        assert_eq!(
            matcher.grammar_diagnostics("run", &verb).len(),
            1,
            "VB is not excepted"
        );
    }

    #[test]
    fn antipattern_suppresses_overlapping_match() {
        // Rule fires on the word "lead"; antipattern "lead singer" suppresses it.
        let rule = Rule {
            id: "LEAD".to_owned(),
            pattern: vec![lit("lead")],
            antipatterns: vec![vec![lit("lead"), lit("singer")]],
            message: String::new(),
            suggestions: vec![IrSuggestion {
                parts: vec![SugPart::Text("led".to_owned())],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);

        // "lead role" → fires (antipattern doesn't match).
        let fires_on = analyze(
            "lead role",
            &[("lead", &["NN"], &["lead"]), ("role", &["NN"], &["role"])],
        );
        assert_eq!(matcher.grammar_diagnostics("lead role", &fires_on).len(), 1);

        // "lead singer" → suppressed by the antipattern.
        let singer = analyze(
            "lead singer",
            &[
                ("lead", &["NN"], &["lead"]),
                ("singer", &["NN"], &["singer"]),
            ],
        );
        assert!(
            matcher
                .grammar_diagnostics("lead singer", &singer)
                .is_empty(),
            "antipattern suppresses"
        );
    }

    #[test]
    fn or_matches_any_alternative() {
        // <marker><or>cat|dog</or></marker> -> "pet"
        let rule = Rule {
            id: "OR".to_owned(),
            pattern: vec![
                Construct::MarkerStart,
                Construct::Or(vec![
                    TokenPat {
                        text: Some("cat".to_owned()),
                        ..Default::default()
                    },
                    TokenPat {
                        text: Some("dog".to_owned()),
                        ..Default::default()
                    },
                ]),
                Construct::MarkerEnd,
            ],
            antipatterns: vec![],
            message: String::new(),
            suggestions: vec![IrSuggestion {
                parts: vec![SugPart::Text("pet".to_owned())],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);
        assert_eq!(
            matcher
                .grammar_diagnostics("dog", &analyze("dog", &[("dog", &[], &[])]))
                .len(),
            1
        );
        assert_eq!(
            matcher
                .grammar_diagnostics("cat", &analyze("cat", &[("cat", &[], &[])]))
                .len(),
            1
        );
        assert!(
            matcher
                .grammar_diagnostics("fish", &analyze("fish", &[("fish", &[], &[])]))
                .is_empty()
        );
    }

    #[test]
    fn match_regexp_replace_transforms_surface() {
        // <marker>cats</marker> -> <match no="1" regexp_match="s$" regexp_replace=""/> (strip plural)
        let rule = Rule {
            id: "DEPLURAL".to_owned(),
            pattern: vec![
                Construct::MarkerStart,
                Construct::Token(TokenPat {
                    text: Some(r"\w+s".to_owned()),
                    regexp: true,
                    ..Default::default()
                }),
                Construct::MarkerEnd,
            ],
            antipatterns: vec![],
            message: String::new(),
            suggestions: vec![IrSuggestion {
                parts: vec![SugPart::Token {
                    no: 1,
                    case: Case::Keep,
                    transform: Some(("s$".to_owned(), String::new())),
                }],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);
        let diags =
            matcher.grammar_diagnostics("cats", &analyze("cats", &[("cats", &["NNS"], &[])]));
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "cat");
    }

    #[test]
    fn regexp_rule_matches_over_text_and_renders_groups() {
        // Rule-level <regexp> "(\d+) dollars" → suggest "$<group 1>".
        let rule = Rule {
            id: "DOLLARS".to_owned(),
            pattern: vec![Construct::Regexp {
                pattern: r"(\d+) dollars".to_owned(),
                mark: None,
                case_sensitive: false,
            }],
            antipatterns: vec![],
            message: "Use the $ sign.".to_owned(),
            suggestions: vec![IrSuggestion {
                parts: vec![
                    SugPart::Text("$".to_owned()),
                    SugPart::Token {
                        no: 1,
                        case: Case::Keep,
                        transform: None,
                    },
                ],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);
        let text = "I have 50 dollars left";
        // Regexp rules match over text, not tokens, so the token graph is irrelevant.
        let diags = matcher.grammar_diagnostics(text, &Analysis { tokens: vec![] });
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].suggestions[0].replacement, "$50");
        assert_eq!(&text[diags[0].span.start..diags[0].span.end], "50 dollars");
    }

    #[test]
    fn and_requires_all_constraints() {
        // <and>: token must be "run" AND tagged VB (not the noun "run").
        let rule = Rule {
            id: "AND".to_owned(),
            pattern: vec![Construct::And(vec![
                TokenPat {
                    text: Some("run".to_owned()),
                    ..Default::default()
                },
                TokenPat {
                    postag: Some("VB".to_owned()),
                    ..Default::default()
                },
            ])],
            antipatterns: vec![],
            message: String::new(),
            suggestions: vec![IrSuggestion {
                parts: vec![SugPart::Text("x".to_owned())],
            }],
        };
        let matcher = IrMatcher::new(&[rule]);
        // "run" tagged VB → matches; tagged NN → does not.
        assert_eq!(
            matcher
                .grammar_diagnostics("run", &analyze("run", &[("run", &["VB"], &[])]))
                .len(),
            1
        );
        assert!(
            matcher
                .grammar_diagnostics("run", &analyze("run", &[("run", &["NN"], &[])]))
                .is_empty()
        );
    }
}
