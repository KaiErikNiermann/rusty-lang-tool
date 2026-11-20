//! Suggestion synthesis: generating the inflected forms a rule's `<match postag="…">` asks for.
//!
//! A rule such as "use the uninflected adjective" writes `<match no="2" postag="ADJ:(.*)"
//! postag_regexp="yes" postag_replace="ADJ:PRD:GRU"/>`. LanguageTool builds a target POS tag from the
//! matched token's tags, asks the language's synthesizer for the forms of the token's lemma with that
//! tag, and offers each form as a suggestion. [`Synthesizer`] is the seam to the data that knows the
//! forms (an artifact built by the converter, loaded by the engine); this module holds the rule
//! side, ported from `MatchState.toFinalString` and `MatchState.getTargetPosTag` (LanguageTool
//! v6.8, `languagetool-core/.../rules/patterns/MatchState.java`, lines 215-281 and 329-375).

use std::collections::BTreeSet;

use regex::Regex;

use crate::{Token, java_regex};

/// A source of inflected word forms for one language (LT's `Synthesizer`).
///
/// Implementations are read-only after construction, so one value can serve every matcher thread.
pub trait Synthesizer: Send + Sync {
    /// The forms of `lemma` whose POS tag is exactly `tag` (LT `synthesize(token, posTag)`).
    ///
    /// Empty if there are none. Language-specific filters (forbidden forms, case guards) belong here.
    fn synthesize(&self, lemma: &str, tag: &str) -> Vec<String>;

    /// The forms of `lemma` for every tag that `tags` matches in full (LT
    /// `synthesize(token, posTag, true)`, which matches the pattern against the language's list of
    /// possible tags with `Matcher.matches`).
    fn synthesize_matching(&self, lemma: &str, tags: &Regex) -> Vec<String>;
}

/// One `<match postag="…">` of a suggestion, with its regexes compiled.
pub(crate) struct CompiledSynth {
    /// The `postag` attribute.
    postag: String,
    /// `postag` is a regex over tags (`postag_regexp="yes"`, or implied by a fixed lemma: LT's
    /// `Match.setLemmaString` sets `postagRegexp = true`, `Match.java` line 133).
    regexp: bool,
    /// `postag` as a regex that must match a whole tag (Java `matches`).
    tag_full: Regex,
    /// `postag` unanchored, for `replaceAll` (Java's `Matcher.replaceAll` finds anywhere).
    tag_find: Regex,
    /// `postag_replace`, in the `regex` crate's replacement syntax.
    replace: Option<String>,
    /// A fixed lemma to inflect instead of the matched token's lemmas.
    lemma: Option<String>,
    /// Drop the suggestion when synthesis finds nothing (instead of showing `(word)`).
    suppress: bool,
}

impl CompiledSynth {
    /// Compile the attributes; `None` if `postag` is not a valid regex.
    pub(crate) fn new(
        postag: &str,
        postag_regexp: bool,
        postag_replace: Option<&str>,
        lemma: Option<&str>,
        suppress: bool,
    ) -> Option<Self> {
        let translated = java_regex::translate(postag);
        Some(Self {
            postag: postag.to_owned(),
            regexp: postag_regexp || lemma.is_some(),
            tag_full: Regex::new(&format!("^(?:{translated})$")).ok()?,
            tag_find: Regex::new(&translated).ok()?,
            replace: postag_replace.map(java_regex::translate_replacement),
            lemma: lemma.map(str::to_owned),
            suppress,
        })
    }

    /// The forms to offer for the matched `token`, or `None` when the suggestion must be dropped.
    ///
    /// Several forms mean several suggestions. The forms are sorted and unique, as LT collects them
    /// in a `TreeSet` (`MatchState.java` lines 235, 273).
    pub(crate) fn forms(&self, synth: &dyn Synthesizer, token: &Token) -> Option<Vec<String>> {
        if !self.regexp {
            // LT line 272-280: the `postag` attribute itself is the tag; nothing found is nothing
            // offered (LT then cuts the suggestion's own text out of the message as well, which
            // is not reproduced: the suggestion is dropped).
            let forms: BTreeSet<String> = token
                .lemmas
                .iter()
                .flat_map(|lemma| synth.synthesize(lemma, &self.postag))
                .collect();
            return (!forms.is_empty()).then(|| forms.into_iter().collect());
        }
        let (lemmas, target) = match &self.lemma {
            Some(lemma) => (vec![lemma.clone()], self.static_target_tag(&token.tags)),
            None => (token.lemmas.clone(), self.target_tag(&token.tags)),
        };
        let word = self.lemma.as_deref().unwrap_or(&token.text);
        let mut forms = BTreeSet::new();
        if lemmas.is_empty() {
            // LT lines 238-243: a reading without a lemma (an unknown word) stands for itself.
            forms.insert(token.text.clone());
        } else if let Ok(tags) = Regex::new(&format!("^(?:{})$", java_regex::translate(&target))) {
            for lemma in &lemmas {
                forms.extend(synth.synthesize_matching(lemma, &tags));
            }
        }
        if forms.is_empty() {
            // LT line 268: nothing synthesized shows as `(word)`; `suppress_misspelled` removes
            // such suggestions afterwards (`PatternRuleMatcher.removeSuppressMisspelled`).
            return (!self.suppress).then(|| vec![format!("({word})")]);
        }
        Some(forms.into_iter().collect())
    }

    /// The tags of `tags` that `postag` matches in full, in reading order.
    fn matching<'a>(&self, tags: &'a [String]) -> Vec<&'a str> {
        tags.iter()
            .map(String::as_str)
            .filter(|t| self.tag_full.is_match(t))
            .collect()
    }

    /// `MatchState.getTargetPosTag` for a matched token's own lemmas (lines 345-372): the last
    /// matching tag (`BaseSynthesizer.getTargetPosTag`, line 356 — LT's own `FIXME` says this is not
    /// always right), or `postag` itself when none matches; with `postag_replace`, every matching tag
    /// rewritten and joined by `|` into one regex.
    fn target_tag(&self, tags: &[String]) -> String {
        let matching = self.matching(tags);
        let target = matching.last().copied().unwrap_or(&self.postag);
        let Some(replace) = &self.replace else {
            return target.to_owned();
        };
        let sources = if matching.is_empty() {
            vec![target]
        } else {
            matching
        };
        sources
            .iter()
            .map(|t| self.tag_find.replace_all(t, replace.as_str()))
            .collect::<Vec<_>>()
            .join("|")
    }

    /// `MatchState.getTargetPosTag` for a fixed lemma (lines 334-344): the replacement applies to the
    /// one chosen tag, and only if some tag of the matched token matched.
    fn static_target_tag(&self, tags: &[String]) -> String {
        let matching = self.matching(tags);
        let target = matching.last().copied().unwrap_or(&self.postag);
        match &self.replace {
            Some(replace) if !matching.is_empty() => {
                self.tag_find.replace_all(target, replace.as_str()).into()
            }
            _ => target.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A synthesizer over a literal `(lemma, tag) -> forms` table.
    #[derive(Default)]
    struct Table(BTreeMap<(&'static str, &'static str), Vec<&'static str>>);

    impl Table {
        fn with(mut self, lemma: &'static str, tag: &'static str, forms: &[&'static str]) -> Self {
            self.0.insert((lemma, tag), forms.to_vec());
            self
        }
    }

    impl Synthesizer for Table {
        fn synthesize(&self, lemma: &str, tag: &str) -> Vec<String> {
            self.0
                .iter()
                .filter(|((l, t), _)| *l == lemma && *t == tag)
                .flat_map(|(_, forms)| forms.iter().map(|f| (*f).to_owned()))
                .collect()
        }

        fn synthesize_matching(&self, lemma: &str, tags: &Regex) -> Vec<String> {
            self.0
                .iter()
                .filter(|((l, t), _)| *l == lemma && tags.is_match(t))
                .flat_map(|(_, forms)| forms.iter().map(|f| (*f).to_owned()))
                .collect()
        }
    }

    fn token(text: &str, tags: &[&str], lemmas: &[&str]) -> Token {
        Token {
            text: text.to_owned(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            lemmas: lemmas.iter().map(|l| (*l).to_owned()).collect(),
            ..Token::default()
        }
    }

    fn adjectives() -> Table {
        Table::default()
            .with("schnell", "ADJ:PRD:GRU", &["schnell"])
            .with("schnell", "ADJ:NOM:SIN:MAS:GRU:SOL", &["schneller"])
            .with("schnell", "ADJ:NOM:SIN:MAS:GRU:DEF", &["schnelle"])
    }

    #[test]
    fn replacement_rewrites_the_matched_tag_into_the_target() {
        // `ADJ:(.*)` with `ADJ:PRD:GRU` replaces the tag wholesale: one fixed target.
        let c = CompiledSynth::new("ADJ:(.*)", true, Some("ADJ:PRD:GRU"), None, false).unwrap();
        let t = token("schnelle", &["ADJ:NOM:SIN:MAS:GRU:DEF"], &["schnell"]);
        assert_eq!(c.target_tag(&t.tags), "ADJ:PRD:GRU");
        assert_eq!(c.forms(&adjectives(), &t), Some(vec!["schnell".to_owned()]));
    }

    #[test]
    fn replacement_keeps_groups_and_joins_every_matching_tag() {
        // Both readings match; each is rewritten with its own capture, then joined into an
        // alternation, so forms for both targets are generated.
        let c = CompiledSynth::new(
            "ADJ:NOM:(.*):GRU:(.*)",
            true,
            Some("ADJ:NOM:$1:GRU:SOL"),
            None,
            false,
        )
        .unwrap();
        let t = token(
            "schnelle",
            &[
                "ADJ:NOM:SIN:MAS:GRU:DEF",
                "ADJ:NOM:SIN:FEM:GRU:DEF",
                "SUB:NOM:SIN:MAS",
            ],
            &["schnell"],
        );
        assert_eq!(
            c.target_tag(&t.tags),
            "ADJ:NOM:SIN:MAS:GRU:SOL|ADJ:NOM:SIN:FEM:GRU:SOL"
        );
        assert_eq!(
            c.forms(&adjectives(), &t),
            Some(vec!["schneller".to_owned()])
        );
    }

    #[test]
    fn without_a_replacement_the_last_matching_tag_is_the_target() {
        let c = CompiledSynth::new("ADJ:.*", true, None, None, false).unwrap();
        let t = token(
            "x",
            &["ADJ:PRD:GRU", "SUB:NOM:SIN:MAS", "ADJ:NOM:SIN:MAS:GRU:DEF"],
            &["schnell"],
        );
        assert_eq!(c.target_tag(&t.tags), "ADJ:NOM:SIN:MAS:GRU:DEF");
    }

    #[test]
    fn a_tag_that_never_matches_falls_back_to_the_pattern_itself() {
        // LT's `posTags.isEmpty()` branch: the regex is run over the language's tag list.
        let c = CompiledSynth::new("ADJ:PRD:.*", true, None, None, false).unwrap();
        let t = token("x", &["SUB:NOM:SIN:MAS"], &["schnell"]);
        assert_eq!(c.target_tag(&t.tags), "ADJ:PRD:.*");
        assert_eq!(c.forms(&adjectives(), &t), Some(vec!["schnell".to_owned()]));
    }

    #[test]
    fn a_plain_postag_is_looked_up_literally_for_every_lemma() {
        let table = Table::default()
            .with("gehen", "VER:INF:NON", &["gehen"])
            .with("ging", "VER:INF:NON", &["ging"]);
        let c = CompiledSynth::new("VER:INF:NON", false, None, None, false).unwrap();
        let t = token("ging", &["VER:1:SIN:PRT:NON"], &["gehen", "ging"]);
        // Sorted and unique, like LT's TreeSet.
        assert_eq!(
            c.forms(&table, &t),
            Some(vec!["gehen".to_owned(), "ging".to_owned()])
        );
        let none = token("x", &[], &["unbekannt"]);
        assert_eq!(c.forms(&table, &none), None, "nothing found offers nothing");
    }

    #[test]
    fn nothing_synthesized_shows_the_word_in_parentheses_unless_suppressed() {
        let t = token("Haus", &["SUB:NOM:SIN:NEU"], &["Haus"]);
        let shown = CompiledSynth::new("ADJ:.*", true, None, None, false).unwrap();
        assert_eq!(
            shown.forms(&adjectives(), &t),
            Some(vec!["(Haus)".to_owned()])
        );
        let suppressed = CompiledSynth::new("ADJ:.*", true, None, None, true).unwrap();
        assert_eq!(suppressed.forms(&adjectives(), &t), None);
    }

    #[test]
    fn a_word_without_lemma_stands_for_itself() {
        let c = CompiledSynth::new("ADJ:.*", true, None, None, false).unwrap();
        let t = token("Blorp", &["UNKNOWN"], &[]);
        assert_eq!(c.forms(&adjectives(), &t), Some(vec!["Blorp".to_owned()]));
    }

    #[test]
    fn a_fixed_lemma_takes_its_tag_from_the_matched_token() {
        let table = Table::default()
            .with("verfügen", "VER:3:SIN:PRÄ:NON", &["verfügt"])
            .with("verfügen", "VER:1:SIN:PRÄ:NON", &["verfüge"]);
        let c = CompiledSynth::new("(.+)", false, Some("$1"), Some("verfügen"), false).unwrap();
        let t = token("verfugt", &["UNKNOWN", "VER:3:SIN:PRÄ:NON"], &[]);
        assert_eq!(c.static_target_tag(&t.tags), "VER:3:SIN:PRÄ:NON");
        assert_eq!(c.forms(&table, &t), Some(vec!["verfügt".to_owned()]));
    }

    #[test]
    fn an_invalid_postag_regex_is_rejected() {
        assert!(CompiledSynth::new("ADJ:(", true, None, None, false).is_none());
    }
}
