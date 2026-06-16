//! L1 — dictionary spell checking.
//!
//! Membership and suggestion both ride on the engine's lexicon ([`Engine::is_known`]): a word-like
//! token the engine doesn't know is flagged, and corrections are Norvig edit-distance-1 candidates
//! filtered down to words the engine *does* know. No separate suggestion word list is needed — the
//! engine's dictionary validates candidates. The future custom engine answers `is_known` from its
//! own FSA, so this layer is unchanged by the swap.

use std::collections::BTreeSet;

use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::{Analysis, Diagnostic, Engine, Source, Suggestion, recase};

/// Whether `c` is a Unicode nonspacing mark (`Mn`) — a combining diacritic. Such marks are never an
/// independently-spellable unit, so the spell layer treats them as transparent: they don't count
/// toward a word's length, aren't tested against the alphabet, and are stripped before edit
/// generation. NFC en/de/ru tokens carry no `Mn` chars, so this is a no-op for them.
fn is_combining_mark(c: char) -> bool {
    c.general_category() == GeneralCategory::NonspacingMark
}

/// Lower-case alphabet `edits1` draws replacement/insertion chars from when no per-language alphabet
/// is supplied — the historical English/German default. Per-language alphabets (e.g. Cyrillic for
/// Russian) come from `rlt_lang::SpellConfig` and are passed through [`spelling_diagnostics`].
pub const ASCII_ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz";
/// Minimum token length (in characters) to spell-check (skips 1–2 char fragments and abbreviations).
const MIN_LEN: usize = 3;
/// Cap on suggestions offered per misspelling.
const MAX_SUGGESTIONS: usize = 5;

/// Produce [`Source::Spelling`] diagnostics for every word-like token the engine does not know.
/// `alphabet` is the script's lower-case letters — both the membership set a token must fall within
/// to be checkable and the pool edit candidates are drawn from. Operating on `char`s (not bytes)
/// keeps multibyte scripts correct: no edit can split a code point or yield invalid UTF-8.
pub(crate) fn spelling_diagnostics<E: Engine>(
    engine: &E,
    analysis: &Analysis,
    alphabet: &str,
    message: &str,
) -> Vec<Diagnostic> {
    let alphabet: Vec<char> = alphabet.chars().collect();
    let mut diagnostics = Vec::new();
    for token in &analysis.tokens {
        if !is_checkable(&token.text, &alphabet) || engine.is_known(&token.text) {
            continue;
        }
        diagnostics.push(Diagnostic {
            span: token.span,
            code: "SPELL".to_owned(),
            message: message.to_owned(),
            suggestions: suggestions(engine, &token.text, &alphabet),
            source: Source::Spelling,
        });
    }
    diagnostics
}

/// A token is checkable if it is a run of at least [`MIN_LEN`] letters all drawn from `alphabet`
/// (case-insensitively) — this skips numbers, punctuation, URLs and mixed alphanumerics, which a
/// lexicon would wrongly flag, and tokens in a different script than the active language.
fn is_checkable(word: &str, alphabet: &[char]) -> bool {
    let mut len = 0;
    for c in word.chars() {
        if is_combining_mark(c) {
            continue; // transparent: a diacritic neither disqualifies the word nor adds length
        }
        len += 1;
        if !c.to_lowercase().all(|l| alphabet.contains(&l)) {
            return false;
        }
    }
    len >= MIN_LEN
}

/// Fuzz/test hook: the count of edit-distance-1 candidates for `word` over `alphabet`. Exercises the
/// char-based [`edits1`] — the historical panic surface (byte edits on multibyte input) — without
/// exposing internals. Hidden from the public API.
#[doc(hidden)]
#[must_use]
pub fn fuzz_edits1(word: &str, alphabet: &str) -> usize {
    let alphabet: Vec<char> = alphabet.chars().collect();
    edits1(&word.to_lowercase(), &alphabet).len()
}

/// Edit-distance-1 corrections that the engine recognizes, re-cased to match `word`, ranked.
fn suggestions<E: Engine>(engine: &E, word: &str, alphabet: &[char]) -> Vec<Suggestion> {
    // Strip combining marks before generating edits so candidates are base-letter forms the lexicon
    // knows (the engine's `is_known` normalizes the same way). No-op for mark-free en/de/ru words.
    let lower: String = word.to_lowercase().chars().filter(|c| !is_combining_mark(*c)).collect();
    let known: BTreeSet<String> = edits1(&lower, alphabet)
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

/// All strings one edit (delete / transpose / replace / insert from `alphabet`) away from `word`.
/// Operates on `char`s so it is correct — and panic-free — for multibyte scripts.
fn edits1(word: &str, alphabet: &[char]) -> BTreeSet<String> {
    let chars: Vec<char> = word.chars().collect();
    let n = chars.len();
    let mut out = BTreeSet::new();
    let build = |cs: &[char]| cs.iter().collect::<String>();

    // Deletes
    for i in 0..n {
        let mut s = chars.clone();
        s.remove(i);
        out.insert(build(&s));
    }
    // Transposes
    for i in 0..n.saturating_sub(1) {
        let mut s = chars.clone();
        s.swap(i, i + 1);
        out.insert(build(&s));
    }
    // Replaces
    for i in 0..n {
        for &c in alphabet {
            if c == chars[i] {
                continue;
            }
            let mut s = chars.clone();
            s[i] = c;
            out.insert(build(&s));
        }
    }
    // Inserts
    for i in 0..=n {
        for &c in alphabet {
            let mut s = chars.clone();
            s.insert(i, c);
            out.insert(build(&s));
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

        let diags = spelling_diagnostics(&engine, &analysis, ASCII_ALPHABET, "misspelled");
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
        assert!(spelling_diagnostics(&engine, &analysis, ASCII_ALPHABET, "misspelled").is_empty());
    }

    #[test]
    fn skips_short_and_non_alpha_tokens() {
        let engine = FakeEngine {
            lexicon: &["receive"],
        };
        let analysis = Analysis {
            tokens: vec![token("42"), token("a"), token("x1y")],
        };
        assert!(spelling_diagnostics(&engine, &analysis, ASCII_ALPHABET, "misspelled").is_empty());
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
            ASCII_ALPHABET, "misspelled",
        );
        assert!(
            diags[0]
                .suggestions
                .iter()
                .any(|s| s.replacement == "Receive")
        );
    }

    /// 33-letter lower-case Russian alphabet (а–я + ё), the `RU` `SpellConfig` value.
    const RU_ALPHABET: &str = "абвгдеёжзийклмнопрстуфхцчшщъыьэюя";

    /// Cyrillic engine whose lexicon is matched case-insensitively (Unicode, not ASCII).
    struct CyrillicEngine {
        lexicon: &'static [&'static str],
    }
    impl Engine for CyrillicEngine {
        fn analyze(&self, _text: &str) -> Analysis {
            Analysis::default()
        }
        fn is_known(&self, word: &str) -> bool {
            let w = word.to_lowercase();
            self.lexicon.iter().any(|k| *k == w)
        }
    }

    #[test]
    fn flags_cyrillic_misspelling_and_suggests() {
        // "превет" (one replace from "привет" = hello) should be flagged with the fix suggested.
        let engine = CyrillicEngine {
            lexicon: &["привет", "мир"],
        };
        let diags = spelling_diagnostics(
            &engine,
            &Analysis {
                tokens: vec![token("превет")],
            },
            RU_ALPHABET, "misspelled",
        );
        assert_eq!(diags.len(), 1, "expected one Cyrillic misspelling");
        assert!(
            diags[0].suggestions.iter().any(|s| s.replacement == "привет"),
            "expected 'привет' among {:?}",
            diags[0].suggestions,
        );
    }

    #[test]
    fn leaves_known_cyrillic_alone_and_preserves_caps() {
        let engine = CyrillicEngine {
            lexicon: &["привет", "мир"],
        };
        // Known word is left alone; a capitalized misspelling round-trips the leading capital.
        assert!(
            spelling_diagnostics(
                &engine,
                &Analysis { tokens: vec![token("Мир")] },
                RU_ALPHABET, "misspelled",
            )
            .is_empty()
        );
        let diags = spelling_diagnostics(
            &engine,
            &Analysis { tokens: vec![token("Превет")] },
            RU_ALPHABET, "misspelled",
        );
        assert!(
            diags[0].suggestions.iter().any(|s| s.replacement == "Привет"),
            "expected capitalized 'Привет' among {:?}",
            diags[0].suggestions,
        );
    }

    /// 37-letter Arabic alphabet (base letters, no tashkeel) — the `AR` `SpellConfig` value.
    const AR_ALPHABET: &str = "ءآأؤإئابةتثجحخدذرزسشصضطظعغـفقكلمنهوىي";

    #[test]
    fn combining_marks_are_transparent_to_spelling() {
        // Tashkeel (combining marks) are transparent: a vocalized word is checkable iff its BASE
        // letters are in the alphabet, and marks don't count toward MIN_LEN. So vocalized Arabic
        // reaches the (engine-normalized) membership check instead of being silently skipped.
        let alphabet: Vec<char> = AR_ALPHABET.chars().collect();
        assert!(is_checkable("كِتَاب", &alphabet), "vocalized 4-letter word must be checkable");
        assert!(is_checkable("مَكْتَبَة", &alphabet));
        // Only base letters count toward length: "اَ" is 1 letter + 1 mark → below MIN_LEN.
        assert!(!is_checkable("اَ", &alphabet));
        // edits1 strips marks first, so candidates are clean base-letter strings (no Mn leaks).
        let cands = edits1(&"كِتَاب".chars().filter(|c| !is_combining_mark(*c)).collect::<String>(), &alphabet);
        assert!(cands.iter().all(|c| !c.chars().any(is_combining_mark)));
    }

    /// Regression: the previous byte-based `edits1` did `String::from_utf8(..).expect("ascii")` on
    /// individual byte edits, which panics on multibyte input. The char-based rewrite must produce
    /// only valid UTF-8 candidates and never panic. (Latent before only because `is_checkable`
    /// gated `edits1` to ASCII; now Cyrillic reaches it.)
    #[test]
    fn edits1_is_panic_free_on_multibyte() {
        let alphabet: Vec<char> = RU_ALPHABET.chars().collect();
        for word in ["привет", "ёж", "мир", "съешь"] {
            let cands = edits1(word, &alphabet);
            assert!(!cands.is_empty());
            // Every candidate is valid UTF-8 (it's a String) and contains no replacement char.
            assert!(cands.iter().all(|c| !c.contains('\u{fffd}')));
        }
    }
}
