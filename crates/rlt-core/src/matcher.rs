//! L2 — matching `rlt-ir` rules (compiled from *current* LanguageTool) over the token graph.
//!
//! This is the on-thesis differentiator: instead of nlprule's bundled v5.2 rules, it runs the
//! rules our converter compiled from LT v6.7. It is scored by the same example oracle as the
//! nlprule baseline, so the two are directly comparable.
//!
//! Scope: literal/regex/POS token matching with `inflected`, `negate`, `case_sensitive`,
//! `<exception>`s, `min`/`max`/`skip` quantifiers, `<or>`/`<and>` groups, `<marker>` spans,
//! `<antipattern>` suppression and suggestion rendering (literal text + `<match no>`/`\N` token
//! copies with case conversion). `<phraseref>`s are inlined at convert time. Rules whose pattern
//! still contains an unsupported construct (`<unify>` — unused in English — or `<filter>`) are
//! skipped rather than matched wrongly.

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
}

impl IrMatcher {
    /// Compile a rule set (skipping rules that cannot be matched faithfully).
    #[must_use]
    pub fn new(rules: &[Rule]) -> Self {
        let mut compiled = Vec::new();
        let mut by_first_literal: HashMap<String, Vec<usize>> = HashMap::new();
        let mut general = Vec::new();
        for r in rules {
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
        }
    }

    /// Compile a rule set from the converter's rkyv artifact bytes.
    ///
    /// # Errors
    /// Returns an error if `bytes` is not a valid archived `Vec<Rule>`.
    pub fn from_rkyv_bytes(bytes: &[u8]) -> Result<Self, rkyv::rancor::Error> {
        Ok(Self::new(&rlt_ir::deserialize_rules(bytes)?))
    }

    /// Number of rules successfully compiled (the matchable subset).
    #[must_use]
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    fn diagnostics(&self, text: &str, tokens: &[Token]) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = Vec::new();
        for start in 0..tokens.len() {
            let key = tokens[start].text.to_ascii_lowercase();
            let literal = self
                .by_first_literal
                .get(&key)
                .map_or(&[][..], Vec::as_slice);
            for &ri in self.general.iter().chain(literal) {
                let rule = &self.rules[ri];
                let mut captures = vec![None; rule.elements.len()];
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
    suggestions: Vec<IrSuggestion>,
    /// Antipattern element sequences; if any matches overlapping the rule's match, suppress it.
    antipatterns: Vec<Vec<Elem>>,
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
    if r.suggestions.is_empty() {
        return None;
    }
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
        suggestions: r.suggestions.clone(),
        antipatterns,
    })
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
    let text = match t.text.as_deref() {
        Some(s) if t.regexp => Some(TextMatch::Regex(anchored(s, !t.case_sensitive)?)),
        Some(s) => Some(TextMatch::Literal {
            value: s.to_owned(),
            case_sensitive: t.case_sensitive,
        }),
        None => None,
    };
    // postag_regexp is folded into the source `postag` string; treat any regex metachar as a regex.
    let postag = match t.postag.as_deref() {
        Some(p) if is_regexish(p) => Some(TagMatch::Regex(anchored(p, false)?)),
        Some(p) => Some(TagMatch::Literal(p.to_owned())),
        None => None,
    };
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
    let text = match e.text.as_deref() {
        Some(t) if e.regexp => Some(TextMatch::Regex(anchored(t, !e.case_sensitive)?)),
        Some(t) => Some(TextMatch::Literal {
            value: t.to_owned(),
            case_sensitive: e.case_sensitive,
        }),
        None => None,
    };
    let postag = match e.postag.as_deref() {
        Some(p) if is_regexish(p) => Some(TagMatch::Regex(anchored(p, false)?)),
        Some(p) => Some(TagMatch::Literal(p.to_owned())),
        None => None,
    };
    Some(TokenMatcher {
        text,
        postag,
        inflected: e.inflected,
        negate: e.negate,
        exceptions: Vec::new(),
    })
}

/// Compile a whole-token-anchored regex (optionally case-insensitive). `None` on regex error.
fn anchored(pattern: &str, case_insensitive: bool) -> Option<Regex> {
    let prefix = if case_insensitive { "(?i)" } else { "" };
    Regex::new(&format!("{prefix}^(?:{pattern})$")).ok()
}

/// Heuristic: does this POS string look like a regex (so it should be compiled, not compared)?
fn is_regexish(p: &str) -> bool {
    p.bytes().any(|b| {
        matches!(
            b,
            b'.' | b'*' | b'+' | b'?' | b'(' | b')' | b'[' | b']' | b'|' | b'^' | b'$' | b'\\'
        )
    })
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

    let mut seen = Vec::new();
    let mut suggestions = Vec::new();
    for s in &rule.suggestions {
        if let Some(r) = render_suggestion(s, text, tokens, captures) {
            if !r.is_empty() && !seen.contains(&r) {
                seen.push(r.clone());
                suggestions.push(Suggestion { replacement: r });
            }
        }
    }
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
    sug: &IrSuggestion,
    text: &str,
    tokens: &[Token],
    captures: &[Option<(usize, usize)>],
) -> Option<String> {
    let mut out = String::new();
    for part in &sug.parts {
        match part {
            SugPart::Text(t) => out.push_str(t),
            SugPart::Token { no, case } => {
                let (ts, te) = captures.get(no.checked_sub(1)?).copied().flatten()?;
                let surface = text.get(tokens[ts].span.start..tokens[te - 1].span.end)?;
                out.push_str(&apply_case(surface, *case));
            }
            // An unknown suggestion part can't be rendered — drop the whole suggestion.
            _ => return None,
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
        Case::StartUpper => {
            let mut chars = s.chars();
            chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().collect::<String>() + chars.as_str()
            })
        }
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
