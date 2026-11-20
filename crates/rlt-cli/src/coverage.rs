//! Coverage against LanguageTool itself: which `<example>`s LT reproduces that we do not, and why.
//!
//! [`oracle_score`](crate::oracle_score) counts how many examples we reproduce. This module compares
//! that with what LanguageTool reproduces on the same sentences, and attributes each gap to one cause
//! by running our matcher twice: once on our own analysis (the real pipeline) and once on LT's
//! tokens, tags and lemmas. An example we only reproduce on LT's analysis is lost to our tokenizer,
//! tagger or disambiguator; one we miss on both is lost to the converter or the matcher. LT's side
//! comes from `tools/lt-probe/LtProbe.java` (see `cargo xtask coverage`), as [`LtSentence`] rows.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rlt_convert::Example;
use rlt_core::{
    Analysis, Checker, Composite, Diagnostic, Engine, GrammarChecker, IrMatcher, Source, Span,
    Synthesizer, Token,
};
use rlt_ir::{Construct, Rule};
use serde::{Deserialize, Serialize};

/// One sentence as LanguageTool analysed and checked it (a line of the probe's output).
#[derive(Debug, Clone, Deserialize)]
pub struct LtSentence {
    /// The example's index, as sent to the probe.
    pub id: String,
    /// LT's matches with its default rule set.
    pub matches: Vec<LtMatch>,
    /// LT's tokens (whitespace excluded) with their readings after disambiguation.
    pub tokens: Vec<LtToken>,
}

/// A rule match reported by LanguageTool.
#[derive(Debug, Clone, Deserialize)]
pub struct LtMatch {
    /// The rule id (the group id for a rule inside a `<rulegroup>`).
    pub rule: String,
    /// `ID[n]` for pattern rules; the plain id for Java rules.
    pub full_id: String,
    /// Whether the rule is implemented in Java rather than as an XML pattern.
    pub java: bool,
    /// Suggested replacements.
    pub repl: Vec<String>,
}

/// A token of LanguageTool's analysis.
#[derive(Debug, Clone, Deserialize)]
pub struct LtToken {
    /// The surface text (empty for the sentence-start token).
    pub text: String,
    /// `lemma/TAG` readings; `null` stands for a missing lemma or tag.
    pub readings: Vec<String>,
}

/// Whether the matcher can run a converted rule, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RuleStatus {
    /// The matcher compiles and runs it.
    Compiled,
    /// The pattern has a `<filter>` (a Java class) we do not implement.
    Filter(String),
    /// The pattern has a construct the converter does not lower (named by its LT element).
    Unsupported(String),
    /// No suggestion survived lowering, so the rule can never produce a correction.
    NoSuggestion,
    /// Every suggestion needs inflected forms and no synthesizer is loaded for the language.
    NeedsSynthesizer,
    /// The matcher rejects it otherwise (in practice a regex the `regex` crate cannot compile).
    Uncompilable,
}

impl RuleStatus {
    /// The status of `rule` as the converter emitted it, for a matcher with `synthesizer`.
    #[must_use]
    pub fn of(rule: &Rule, synthesizer: Option<&Arc<dyn Synthesizer>>) -> Self {
        for c in &rule.pattern {
            match c {
                Construct::Opaque { class, .. } => return Self::Filter(short_class(class)),
                Construct::Unsupported { kind } => return Self::Unsupported(kind.clone()),
                _ => {}
            }
        }
        let compiles = |synthesizer: Option<Arc<dyn Synthesizer>>| {
            IrMatcher::with_synthesizer(std::slice::from_ref(rule), synthesizer).rule_count() == 1
        };
        if rule.suggestions.is_empty() {
            Self::NoSuggestion
        } else if compiles(synthesizer.cloned()) {
            Self::Compiled
        } else if synthesizer.is_none() && compiles(Some(Arc::new(NoForms))) {
            Self::NeedsSynthesizer
        } else {
            Self::Uncompilable
        }
    }
}

/// A synthesizer that finds no forms: it only tells [`RuleStatus::of`] that a rule would compile
/// with one.
struct NoForms;

impl Synthesizer for NoForms {
    fn synthesize(&self, _lemma: &str, _tag: &str) -> Vec<String> {
        Vec::new()
    }

    fn synthesize_matching(&self, _lemma: &str, _tags: &regex::Regex) -> Vec<String> {
        Vec::new()
    }
}

/// The last segment of a Java class name.
fn short_class(class: &str) -> String {
    class.rsplit('.').next().unwrap_or(class).to_owned()
}

/// Why an example that LT's `grammar.xml` rules reproduce is not reproduced by us.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum GapCause {
    /// The owning rule does not run at all.
    NotCompiled(RuleStatus),
    /// On LT's analysis the rule fires on the error but suggests the correction in another letter
    /// case (LT adjusts suggestion case to the matched text).
    SuggestionCase,
    /// On LT's analysis the rule fires on the error with another suggestion.
    SuggestionOther,
    /// On LT's analysis the rule fires on the error, but every suggestion equals the original text,
    /// so the checker drops it (typically a form LT would synthesize that we copy unchanged).
    SuggestionUnchanged,
    /// Reproduced on LT's analysis; our tokens differ from LT's.
    Tokenization,
    /// Reproduced on LT's analysis; same tokens, different tags.
    Tags,
    /// Reproduced on LT's analysis; same tokens and tags (so lemmas differ).
    AnalysisOther,
    /// Not even on LT's analysis does the owning rule fire on the error.
    NoMatch,
}

impl std::fmt::Display for RuleStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compiled => f.write_str("compiled"),
            Self::Filter(class) => write!(f, "filter {class}"),
            Self::Unsupported(kind) => write!(f, "unsupported <{kind}>"),
            Self::NoSuggestion => f.write_str("no suggestion survives lowering"),
            Self::NeedsSynthesizer => {
                f.write_str("every suggestion needs inflected forms and no synthesizer is loaded")
            }
            Self::Uncompilable => f.write_str("regex the matcher cannot compile"),
        }
    }
}

impl std::fmt::Display for GapCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCompiled(status) => write!(f, "rule not run: {status}"),
            Self::SuggestionCase => f.write_str("matcher: right place, suggestion differs in case"),
            Self::SuggestionOther => f.write_str("matcher: right place, other suggestion"),
            Self::SuggestionUnchanged => {
                f.write_str("matcher: right place, suggests the unchanged text")
            }
            Self::Tokenization => f.write_str("analysis: tokenization differs"),
            Self::Tags => f.write_str("analysis: tags differ"),
            Self::AnalysisOther => f.write_str("analysis: lemmas differ"),
            Self::NoMatch => f.write_str("matcher: no match even on LT's analysis"),
        }
    }
}

impl std::fmt::Display for FalsePositiveCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Matcher => "matcher: also fires on LT's analysis",
            Self::Analysis => "analysis: only on our analysis",
        })
    }
}

impl GapCause {
    /// Whether this gap would remain with LT's analysis (converter/matcher side).
    #[must_use]
    pub fn is_matcher_side(&self) -> bool {
        !matches!(self, Self::Tokenization | Self::Tags | Self::AnalysisOther)
    }
}

/// Why we fire on a correct example that LT leaves alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum FalsePositiveCause {
    /// We also fire on LT's analysis: the rule as converted or matched is too broad.
    Matcher,
    /// Only on our analysis.
    Analysis,
}

/// The verdict for one example.
#[derive(Debug, Clone, Serialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per pipeline compared; a bitset would hide them in the JSON"
)]
pub struct Outcome {
    /// Index into the example list.
    pub idx: usize,
    /// LT's full id of the owning rule.
    pub full_id: String,
    /// The example text.
    pub text: String,
    /// Positive (has corrections) or negative.
    pub positive: bool,
    /// Positive: LT's `grammar.xml` rules reproduce it. Negative: the owning rule fires in LT.
    pub lt: bool,
    /// Positive: any LT rule (Java and other XML files included) reproduces it.
    pub lt_any: bool,
    /// Positive: we reproduce it. Negative: the owning rule fires in our pipeline.
    pub ours: bool,
    /// The same on LT's analysis.
    pub ours_on_lt: bool,
    /// Set for a positive LT reproduces and we do not.
    pub gap: Option<GapCause>,
    /// Set for a negative we fire on and LT does not.
    pub false_positive: Option<FalsePositiveCause>,
}

/// Counts over the whole example corpus.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    /// Positive examples.
    pub positives: usize,
    /// Positives reproduced by LT's rules from the same `grammar.xml`.
    pub lt_grammar: usize,
    /// Positives reproduced by any LT rule.
    pub lt_any: usize,
    /// Positives we reproduce.
    pub ours: usize,
    /// Positives our matcher reproduces on LT's analysis.
    pub ours_on_lt: usize,
    /// Positives we reproduce and LT's `grammar.xml` rules do not.
    pub ours_not_lt: usize,
    /// Negative examples.
    pub negatives: usize,
    /// Negatives LT's owning rule fires on.
    pub lt_false_positives: usize,
    /// Negatives our owning rule fires on.
    pub ours_false_positives: usize,
    /// Negatives our owning rule fires on with LT's analysis.
    pub ours_on_lt_false_positives: usize,
}

/// The full comparison.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    /// Corpus-wide counts.
    pub totals: Totals,
    /// Gap causes with their example count and the rules contributing most, largest first.
    pub gaps: Vec<CauseCount<GapCause>>,
    /// False-positive causes, likewise.
    pub false_positives: Vec<CauseCount<FalsePositiveCause>>,
    /// Per-example verdicts.
    pub outcomes: Vec<Outcome>,
}

/// How many examples one cause accounts for, and the rules behind them.
#[derive(Debug, Clone, Serialize)]
pub struct CauseCount<C> {
    /// The cause.
    pub cause: C,
    /// Examples attributed to it.
    pub count: usize,
    /// Owning rule ids with their example counts, largest first.
    pub rules: Vec<(String, usize)>,
}

/// An [`Engine`] that serves analyses computed elsewhere (LT's), keyed by sentence text.
struct FixedAnalyses<'a>(&'a HashMap<String, Analysis>);

impl Engine for FixedAnalyses<'_> {
    fn analyze(&self, text: &str) -> Analysis {
        self.0
            .get(text)
            .cloned()
            .unwrap_or(Analysis { tokens: Vec::new() })
    }
    fn is_known(&self, _word: &str) -> bool {
        true // only grammar diagnostics are compared; keep the speller quiet
    }
}

/// Rebuild LT's analysis of `text` as our [`Analysis`]: spans are found left to right in `text`,
/// `null` lemmas and tags are dropped, and duplicate tags/lemmas are merged.
#[must_use]
pub fn lt_analysis(text: &str, lt: &LtSentence) -> Analysis {
    let mut pos = 0;
    let tokens = lt
        .tokens
        .iter()
        .map(|t| {
            let (mut tags, mut lemmas) = (Vec::new(), Vec::new());
            for r in &t.readings {
                let (lemma, tag) = r.rsplit_once('/').unwrap_or((r, "null"));
                if tag != "null" && !tags.iter().any(|x| x == tag) {
                    tags.push(tag.to_owned());
                }
                if lemma != "null" && !lemmas.iter().any(|x| x == lemma) {
                    lemmas.push(lemma.to_owned());
                }
            }
            let start = if t.text.is_empty() {
                pos
            } else {
                text.get(pos..)
                    .and_then(|rest| rest.find(&t.text))
                    .map_or(pos, |o| pos + o)
            };
            let end = (start + t.text.len()).min(text.len());
            pos = end;
            Token {
                text: t.text.clone(),
                span: Span { start, end },
                tags,
                lemmas,
            }
        })
        .collect();
    Analysis { tokens }
}

/// The grammar diagnostics of the oracle path (`Checker::check`, grammar source only).
fn grammar_only<B: Engine + GrammarChecker>(checker: &Checker<B>, text: &str) -> Vec<Diagnostic> {
    checker
        .check(text)
        .into_iter()
        .filter(|d| d.source == Source::Grammar)
        .collect()
}

fn reproduces(diags: &[Diagnostic], corrections: &[String]) -> bool {
    diags
        .iter()
        .flat_map(|d| &d.suggestions)
        .any(|s| corrections.contains(&s.replacement))
}

fn lt_reproduces<'a>(
    matches: impl IntoIterator<Item = &'a LtMatch>,
    corrections: &[String],
) -> bool {
    matches
        .into_iter()
        .flat_map(|m| &m.repl)
        .any(|r| corrections.contains(r))
}

/// Whether `d` overlaps the example's marked span (any position when unmarked).
fn on_marker(d: &Diagnostic, marker: Option<(usize, usize)>) -> bool {
    marker.is_none_or(|(s, e)| d.span.start < e && s < d.span.end)
}

/// Surface texts of the non-empty tokens.
fn surfaces(a: &Analysis) -> Vec<&str> {
    a.tokens
        .iter()
        .map(|t| t.text.as_str())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Per non-empty token, its tag set without the sentence/paragraph boundary tags.
fn tag_sets<'a>(a: &'a Analysis, boundary: &[&str]) -> Vec<HashSet<&'a str>> {
    a.tokens
        .iter()
        .filter(|t| !t.text.is_empty())
        .map(|t| {
            t.tags
                .iter()
                .map(String::as_str)
                .filter(|t| !boundary.contains(t))
                .collect()
        })
        .collect()
}

/// Inputs shared by every example's classification.
struct Context<'a> {
    rules: &'a [Rule],
    statuses: Vec<RuleStatus>,
    grammar_full_ids: HashSet<String>,
    boundary: [&'a str; 3],
}

/// What each pipeline produced for one example.
struct Runs<'a> {
    ex: &'a Example,
    lt: &'a LtSentence,
    ours: Vec<Diagnostic>,
    ours_on_lt: Vec<Diagnostic>,
    /// The matcher's own output on LT's analysis, before the checker drops no-op suggestions.
    raw_on_lt: Vec<Diagnostic>,
    ours_analysis: &'a Analysis,
    lt_analysis: &'a Analysis,
}

impl Context<'_> {
    fn gap_cause(&self, r: &Runs<'_>) -> GapCause {
        let status = &self.statuses[r.ex.rule_index];
        if *status != RuleStatus::Compiled {
            return GapCause::NotCompiled(status.clone());
        }
        if reproduces(&r.ours_on_lt, &r.ex.corrections) {
            return if surfaces(r.ours_analysis) != surfaces(r.lt_analysis) {
                GapCause::Tokenization
            } else if tag_sets(r.ours_analysis, &self.boundary)
                != tag_sets(r.lt_analysis, &self.boundary)
            {
                GapCause::Tags
            } else {
                GapCause::AnalysisOther
            };
        }
        let id = &self.rules[r.ex.rule_index].id;
        let own: Vec<&Diagnostic> = r
            .raw_on_lt
            .iter()
            .filter(|d| &d.code == id && on_marker(d, r.ex.marker))
            .collect();
        if own.is_empty() {
            return GapCause::NoMatch;
        }
        let unchanged = |d: &Diagnostic| {
            let spanned = r.ex.text.get(d.span.start..d.span.end).unwrap_or_default();
            d.suggestions.iter().all(|s| s.replacement == spanned)
        };
        if own.iter().all(|d| unchanged(d)) {
            return GapCause::SuggestionUnchanged;
        }
        let case_only = own.iter().flat_map(|d| &d.suggestions).any(|s| {
            let lower = s.replacement.to_lowercase();
            r.ex.corrections.iter().any(|c| c.to_lowercase() == lower)
        });
        if case_only {
            GapCause::SuggestionCase
        } else {
            GapCause::SuggestionOther
        }
    }

    fn outcome(&self, idx: usize, r: &Runs<'_>) -> Outcome {
        let ex = r.ex;
        let mut o = Outcome {
            idx,
            full_id: ex.full_id.clone(),
            text: ex.text.clone(),
            positive: !ex.corrections.is_empty(),
            lt: false,
            lt_any: false,
            ours: false,
            ours_on_lt: false,
            gap: None,
            false_positive: None,
        };
        if o.positive {
            let lt_grammar =
                r.lt.matches
                    .iter()
                    .filter(|m| !m.java && self.grammar_full_ids.contains(&m.full_id));
            o.lt = lt_reproduces(lt_grammar, &ex.corrections);
            o.lt_any = lt_reproduces(&r.lt.matches, &ex.corrections);
            o.ours = reproduces(&r.ours, &ex.corrections);
            o.ours_on_lt = reproduces(&r.ours_on_lt, &ex.corrections);
            if o.lt && !o.ours {
                o.gap = Some(self.gap_cause(r));
            }
        } else {
            o.lt = r.lt.matches.iter().any(|m| m.rule == ex.rule_id);
            o.ours = r.ours.iter().any(|d| d.code == ex.rule_id);
            o.ours_on_lt = r.ours_on_lt.iter().any(|d| d.code == ex.rule_id);
            if o.ours && !o.lt {
                o.false_positive = Some(if o.ours_on_lt {
                    FalsePositiveCause::Matcher
                } else {
                    FalsePositiveCause::Analysis
                });
            }
        }
        o
    }
}

/// Compare our pipeline (`engine` + `rules`) with LanguageTool on `examples`.
///
/// `lt` must hold one row per example, in order (row `i` has id `i`). `sent_start`/`sent_end` are the
/// language's boundary tags, ignored when comparing tag sets.
///
/// # Panics
/// Panics if `lt` is not aligned with `examples`.
#[must_use]
pub fn compare<E: Engine>(
    engine: &E,
    rules: &[Rule],
    examples: &[Example],
    lt: &[LtSentence],
    sent_start: &str,
    sent_end: &str,
    synthesizer: Option<&Arc<dyn Synthesizer>>,
) -> CoverageReport {
    assert_eq!(examples.len(), lt.len(), "one LT row per example");
    // Analyse each sentence once per side; both checkers then read the stored analyses, which is
    // what `Checker::check` would compute (it calls `Engine::analyze` on the text).
    let mut ours_by_text = HashMap::new();
    let mut lt_by_text = HashMap::new();
    for (ex, l) in examples.iter().zip(lt) {
        ours_by_text
            .entry(ex.text.clone())
            .or_insert_with(|| engine.analyze(&ex.text));
        lt_by_text
            .entry(ex.text.clone())
            .or_insert_with(|| lt_analysis(&ex.text, l));
    }
    let matcher = || IrMatcher::with_synthesizer(rules, synthesizer.cloned());
    let ours = Checker::new(Composite::new(FixedAnalyses(&ours_by_text), matcher()));
    let on_lt = Checker::new(Composite::new(FixedAnalyses(&lt_by_text), matcher()));
    let raw = matcher();
    let ctx = Context {
        rules,
        statuses: rules
            .iter()
            .map(|r| RuleStatus::of(r, synthesizer))
            .collect(),
        grammar_full_ids: examples.iter().map(|e| e.full_id.clone()).collect(),
        boundary: [sent_start, sent_end, "PARA_END"],
    };
    let outcomes: Vec<Outcome> = examples
        .iter()
        .zip(lt)
        .enumerate()
        .map(|(idx, (ex, l))| {
            assert_eq!(l.id, idx.to_string(), "LT rows out of step with examples");
            let runs = Runs {
                ex,
                lt: l,
                ours: grammar_only(&ours, &ex.text),
                ours_on_lt: grammar_only(&on_lt, &ex.text),
                raw_on_lt: raw.grammar_diagnostics(&ex.text, &lt_by_text[&ex.text]),
                ours_analysis: &ours_by_text[&ex.text],
                lt_analysis: &lt_by_text[&ex.text],
            };
            ctx.outcome(idx, &runs)
        })
        .collect();
    summarize(rules, examples, outcomes)
}

fn summarize(rules: &[Rule], examples: &[Example], outcomes: Vec<Outcome>) -> CoverageReport {
    let mut t = Totals::default();
    let mut gaps: BTreeMap<GapCause, BTreeMap<String, usize>> = BTreeMap::new();
    let mut fps: BTreeMap<FalsePositiveCause, BTreeMap<String, usize>> = BTreeMap::new();
    for o in &outcomes {
        let rule = rules[examples[o.idx].rule_index].id.clone();
        if o.positive {
            t.positives += 1;
            t.lt_grammar += usize::from(o.lt);
            t.lt_any += usize::from(o.lt_any);
            t.ours += usize::from(o.ours);
            t.ours_on_lt += usize::from(o.ours_on_lt);
            t.ours_not_lt += usize::from(o.ours && !o.lt);
        } else {
            t.negatives += 1;
            t.lt_false_positives += usize::from(o.lt);
            t.ours_false_positives += usize::from(o.ours);
            t.ours_on_lt_false_positives += usize::from(o.ours_on_lt);
        }
        if let Some(g) = &o.gap {
            *gaps
                .entry(g.clone())
                .or_default()
                .entry(rule.clone())
                .or_default() += 1;
        }
        if let Some(f) = o.false_positive {
            *fps.entry(f).or_default().entry(rule).or_default() += 1;
        }
    }
    CoverageReport {
        totals: t,
        gaps: ranked(gaps),
        false_positives: ranked(fps),
        outcomes,
    }
}

fn ranked<C>(by_cause: BTreeMap<C, BTreeMap<String, usize>>) -> Vec<CauseCount<C>> {
    let mut out: Vec<CauseCount<C>> = by_cause
        .into_iter()
        .map(|(cause, per_rule)| {
            let mut rules: Vec<(String, usize)> = per_rule.into_iter().collect();
            rules.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            CauseCount {
                cause,
                count: rules.iter().map(|(_, n)| n).sum(),
                rules,
            }
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.count));
    out
}

#[cfg(test)]
mod tests {
    use rlt_ir::{SugPart, Suggestion, TokenPat};

    use super::*;

    /// An engine that splits on spaces and hyphens and tags every word `X`.
    struct SplitEngine;

    impl Engine for SplitEngine {
        fn analyze(&self, text: &str) -> Analysis {
            let mut tokens = Vec::new();
            let mut start = None;
            for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
                match (start, c == ' ' || c == '-') {
                    (None, false) => start = Some(i),
                    (Some(s), true) => {
                        tokens.push(Token {
                            text: text[s..i].to_owned(),
                            span: Span { start: s, end: i },
                            tags: vec!["X".to_owned()],
                            lemmas: vec![],
                        });
                        start = None;
                    }
                    _ => {}
                }
            }
            Analysis { tokens }
        }
        fn is_known(&self, _word: &str) -> bool {
            true
        }
    }

    fn rule(id: &str, word: &str, fix: &str) -> Rule {
        Rule {
            id: id.to_owned(),
            pattern: vec![Construct::Token(TokenPat {
                text: Some(word.to_owned()),
                ..Default::default()
            })],
            antipatterns: vec![],
            message: String::new(),
            suggestions: vec![Suggestion {
                parts: vec![SugPart::Text(fix.to_owned())],
            }],
        }
    }

    fn example(rule_index: usize, id: &str, text: &str, correction: &str) -> Example {
        Example {
            rule_id: id.to_owned(),
            text: text.to_owned(),
            marker: None,
            corrections: vec![correction.to_owned()],
            rule_index,
            full_id: format!("{id}[1]"),
        }
    }

    fn lt_row(id: usize, rule: &str, fix: &str, words: &[&str]) -> LtSentence {
        LtSentence {
            id: id.to_string(),
            matches: vec![LtMatch {
                rule: rule.to_owned(),
                full_id: format!("{rule}[1]"),
                java: false,
                repl: vec![fix.to_owned()],
            }],
            tokens: words
                .iter()
                .map(|w| LtToken {
                    text: (*w).to_owned(),
                    readings: vec![format!("{w}/X")],
                })
                .collect(),
        }
    }

    #[test]
    fn lt_analysis_finds_spans_and_drops_null() {
        let row = LtSentence {
            id: "0".to_owned(),
            matches: vec![],
            tokens: vec![
                LtToken {
                    text: String::new(),
                    readings: vec!["null/SENT_START".to_owned()],
                },
                LtToken {
                    text: "E-Mail".to_owned(),
                    readings: vec!["E-Mail/SUB".to_owned(), "null/null".to_owned()],
                },
                LtToken {
                    text: "an".to_owned(),
                    readings: vec!["an/PRP".to_owned()],
                },
            ],
        };
        let a = lt_analysis("E-Mail  an", &row);
        let spans: Vec<(usize, usize)> = a
            .tokens
            .iter()
            .map(|t| (t.span.start, t.span.end))
            .collect();
        assert_eq!(spans, [(0, 0), (0, 6), (8, 10)]);
        assert_eq!(a.tokens[1].tags, ["SUB"]);
        assert_eq!(a.tokens[1].lemmas, ["E-Mail"]);
        assert_eq!(a.tokens[0].lemmas, [] as [String; 0]);
    }

    #[test]
    fn a_rule_that_needs_inflected_forms_runs_only_with_a_synthesizer() {
        use rlt_ir::SugPart;
        let mut needs_forms = rule("FORMS", "foo", "bar");
        needs_forms.suggestions[0].parts = vec![SugPart::Synth {
            no: 1,
            case: rlt_ir::Case::Keep,
            transform: None,
            postag: "ADJ:(.*)".to_owned(),
            postag_regexp: true,
            postag_replace: Some("ADJ:PRD:GRU".to_owned()),
            lemma: None,
            suppress_misspelled: false,
        }];
        assert_eq!(
            RuleStatus::of(&needs_forms, None),
            RuleStatus::NeedsSynthesizer
        );
        let synthesizer: Arc<dyn Synthesizer> = Arc::new(NoForms);
        assert_eq!(
            RuleStatus::of(&needs_forms, Some(&synthesizer)),
            RuleStatus::Compiled
        );
    }

    #[test]
    fn attributes_gaps_to_tokenization_and_unrun_rules() {
        // Rule 0 matches the whole word "E-Mail", which our engine splits; rule 1 cannot run.
        let mut unrunnable = rule("NO_SUG", "foo", "bar");
        unrunnable.suggestions.clear();
        let rules = [rule("EMAIL", "E-Mail", "Mail"), unrunnable];
        let examples = [
            example(0, "EMAIL", "E-Mail", "Mail"),
            example(1, "NO_SUG", "foo", "bar"),
        ];
        let lt = [
            lt_row(0, "EMAIL", "Mail", &["E-Mail"]),
            lt_row(1, "NO_SUG", "bar", &["foo"]),
        ];
        let report = compare(
            &SplitEngine,
            &rules,
            &examples,
            &lt,
            "SENT_START",
            "SENT_END",
            None,
        );
        let gaps: Vec<_> = report.outcomes.iter().map(|o| o.gap.clone()).collect();
        assert_eq!(
            gaps,
            [
                Some(GapCause::Tokenization),
                Some(GapCause::NotCompiled(RuleStatus::NoSuggestion))
            ]
        );
        assert_eq!(
            (
                report.totals.lt_grammar,
                report.totals.ours,
                report.totals.ours_on_lt
            ),
            (2, 0, 1)
        );
    }
}
