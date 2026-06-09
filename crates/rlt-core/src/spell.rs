//! L1 — dictionary spell checking.
//!
//! Membership and suggestion both ride on the engine's lexicon ([`Engine::is_known`]): a word-like
//! token the engine doesn't know is flagged, and corrections are Norvig edit-distance-1 candidates
//! filtered down to words the engine *does* know. No separate suggestion word list is needed — the
//! engine's dictionary validates candidates. The future custom engine answers `is_known` from its
//! own FSA, so this layer is unchanged by the swap.

use std::collections::BTreeSet;

use crate::{Analysis, Diagnostic, Engine, Source, Suggestion};

/// Lower-case ASCII alphabet used to generate replacement/insertion edits.
const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
/// Minimum token length to spell-check (skips 1–2 char fragments and most abbreviations).
const MIN_LEN: usize = 3;
/// Cap on suggestions offered per misspelling.
const MAX_SUGGESTIONS: usize = 5;

/// Produce [`Source::Spelling`] diagnostics for every word-like token the engine does not know.
pub(crate) fn spelling_diagnostics<E: Engine>(engine: &E, analysis: &Analysis) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for token in &analysis.tokens {
        if !is_checkable(&token.text) || engine.is_known(&token.text) {
            continue;
        }
        diagnostics.push(Diagnostic {
            span: token.span,
            code: "SPELL".to_owned(),
            message: format!("“{}” may be misspelled.", token.text),
            suggestions: suggestions(engine, &token.text),
            source: Source::Spelling,
        });
    }
    diagnostics
}

/// A token is checkable if it is a run of ASCII letters of at least [`MIN_LEN`] — this skips
/// numbers, punctuation, URLs and mixed alphanumerics, which a lexicon would wrongly flag.
fn is_checkable(word: &str) -> bool {
    word.len() >= MIN_LEN && word.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Edit-distance-1 corrections that the engine recognizes, re-cased to match `word`, ranked.
fn suggestions<E: Engine>(engine: &E, word: &str) -> Vec<Suggestion> {
    let lower = word.to_ascii_lowercase();
    let known: BTreeSet<String> = edits1(&lower)
        .into_iter()
        .filter(|cand| engine.is_known(cand))
        .collect();

    known
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|cand| Suggestion {
            replacement: recase(word, &cand),
        })
        .collect()
}

/// Apply `source`'s leading capitalization to `candidate` (so `Recieve` → `Receive`, not
/// `receive`).
fn recase(source: &str, candidate: &str) -> String {
    if source.chars().next().is_some_and(char::is_uppercase) {
        let mut c = candidate.chars();
        match c.next() {
            Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
            None => candidate.to_owned(),
        }
    } else {
        candidate.to_owned()
    }
}

/// All strings one edit (delete / transpose / replace / insert) away from `word` (ASCII).
fn edits1(word: &str) -> BTreeSet<String> {
    let chars = word.as_bytes();
    let n = chars.len();
    let mut out = BTreeSet::new();

    // Deletes
    for i in 0..n {
        let mut s = Vec::with_capacity(n - 1);
        s.extend_from_slice(&chars[..i]);
        s.extend_from_slice(&chars[i + 1..]);
        out.insert(String::from_utf8(s).expect("ascii"));
    }
    // Transposes
    for i in 0..n.saturating_sub(1) {
        let mut s = chars.to_vec();
        s.swap(i, i + 1);
        out.insert(String::from_utf8(s).expect("ascii"));
    }
    // Replaces
    for i in 0..n {
        for &c in ALPHABET {
            if c == chars[i] {
                continue;
            }
            let mut s = chars.to_vec();
            s[i] = c;
            out.insert(String::from_utf8(s).expect("ascii"));
        }
    }
    // Inserts
    for i in 0..=n {
        for &c in ALPHABET {
            let mut s = Vec::with_capacity(n + 1);
            s.extend_from_slice(&chars[..i]);
            s.push(c);
            s.extend_from_slice(&chars[i..]);
            out.insert(String::from_utf8(s).expect("ascii"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Span, Token};

    /// A tiny fake engine whose lexicon is a fixed word set — lets us test L1 without nlprule.
    struct FakeEngine {
        lexicon: &'static [&'static str],
    }

    impl Engine for FakeEngine {
        fn analyze(&self, _text: &str) -> Analysis {
            Analysis::default()
        }
        fn is_known(&self, word: &str) -> bool {
            let w = word.to_ascii_lowercase();
            self.lexicon.iter().any(|k| *k == w)
        }
    }

    fn token(text: &str) -> Token {
        Token {
            text: text.to_owned(),
            span: Span {
                start: 0,
                end: text.len(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn flags_misspelling_and_suggests_correction() {
        let engine = FakeEngine {
            lexicon: &["receive", "the", "message"],
        };
        let analysis = Analysis {
            tokens: vec![token("recieve")],
        };

        let diags = spelling_diagnostics(&engine, &analysis);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].source, Source::Spelling);
        assert!(
            diags[0]
                .suggestions
                .iter()
                .any(|s| s.replacement == "receive"),
            "expected 'receive' among {:?}",
            diags[0].suggestions,
        );
    }

    #[test]
    fn leaves_known_words_alone() {
        let engine = FakeEngine {
            lexicon: &["receive", "the", "message"],
        };
        let analysis = Analysis {
            tokens: vec![token("the"), token("message")],
        };
        assert!(spelling_diagnostics(&engine, &analysis).is_empty());
    }

    #[test]
    fn skips_short_and_non_alpha_tokens() {
        let engine = FakeEngine {
            lexicon: &["receive"],
        };
        let analysis = Analysis {
            tokens: vec![token("42"), token("a"), token("x1y")],
        };
        assert!(spelling_diagnostics(&engine, &analysis).is_empty());
    }

    #[test]
    fn preserves_leading_capitalization() {
        let engine = FakeEngine {
            lexicon: &["receive"],
        };
        let diags = spelling_diagnostics(
            &engine,
            &Analysis {
                tokens: vec![token("Recieve")],
            },
        );
        assert!(
            diags[0]
                .suggestions
                .iter()
                .any(|s| s.replacement == "Receive")
        );
    }
}
