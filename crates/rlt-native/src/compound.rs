//! Compound-word splitting (German `Haus|tür`, `Arbeit|s|zimmer`).
//!
//! German concatenates words into compounds the morphological dictionary doesn't list. When a word is
//! out-of-lexicon, we try to split it into known constituents — longest-match left-to-right, allowing
//! linking morphemes (`-s-`, `-es-`, `-n-`, …) between parts. If it splits, the compound counts as
//! "known" (L1 spelling) and takes the **head** (last constituent)'s analyses — its POS / gender /
//! case, with the compound surface as the lemma — matching how LanguageTool tags compounds. The split
//! is dictionary-driven (the loaded [`Tagger`] is the known-word oracle), so it generalizes to any
//! compounding language that supplies [`rlt_lang::Compounding`].

use rlt_core::capitalize_first;
use rlt_lang::Compounding;

use crate::tagger::{Tagger, WordData};

/// Recursion-depth cap (max constituents) — guards against pathological inputs.
const MAX_PARTS: usize = 8;

/// If out-of-lexicon `word` splits into ≥2 known constituents, return the head constituent's analyses
/// (tags kept; lemma replaced by the whole compound surface). `None` if it does not split.
pub(crate) fn analyze_compound(
    word: &str,
    tagger: &Tagger,
    rules: &Compounding,
) -> Option<Vec<WordData>> {
    let parts = split(word, tagger, rules)?;
    let head = if rules.head_is_last { parts.last()? } else { parts.first()? };
    let analyses = head_analyses(tagger, head)?;
    Some(
        analyses
            .into_iter()
            .map(|wd| WordData {
                lemma: word.to_owned(),
                tag: wd.tag,
            })
            .collect(),
    )
}

/// Whether `word` splits into known constituents (used for L1 spelling — a compound is "known").
pub(crate) fn is_compound(word: &str, tagger: &Tagger, rules: &Compounding) -> bool {
    split(word, tagger, rules).is_some()
}

/// A constituent is known if the dictionary has it as-is or capitalized (German nouns are capitalized
/// but appear lower-cased inside a compound).
fn known_part(tagger: &Tagger, part: &str) -> bool {
    tagger.is_known(part) || tagger.is_known(&capitalize_first(part))
}

/// The head constituent's analyses (as-is or capitalized lookup).
fn head_analyses(tagger: &Tagger, part: &str) -> Option<Vec<WordData>> {
    tagger.analyses(part).or_else(|| tagger.analyses(&capitalize_first(part)))
}

/// Longest-match left-to-right split of `word` into ≥2 known parts (+ optional linking morphemes).
fn split(word: &str, tagger: &Tagger, rules: &Compounding) -> Option<Vec<String>> {
    // Match against the lower-cased form (`lookup_part` re-capitalizes per constituent); German is
    // case-sensitive but compounds lower-case their non-initial parts.
    let parts = rec(&word.to_lowercase(), tagger, rules, 0)?;
    (parts.len() >= 2).then_some(parts)
}

/// Recursive split of `s` into known parts. Returns the parts, or `None` if `s` can't be tiled.
fn rec(s: &str, tagger: &Tagger, rules: &Compounding, depth: usize) -> Option<Vec<String>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    if depth >= MAX_PARTS {
        return None;
    }
    // Candidate prefix end offsets (char boundaries), longest first — prefer the longest known prefix.
    let mut ends: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    ends.push(s.len());
    for &end in ends.iter().rev() {
        if end < rules.min_part_len {
            continue;
        }
        let prefix = &s[..end];
        if !known_part(tagger, prefix) {
            continue;
        }
        let rest = &s[end..];
        if rest.is_empty() {
            return Some(vec![prefix.to_owned()]); // prefix is the final constituent
        }
        // The remainder may start with a linking morpheme; try each (and none).
        for link in std::iter::once("").chain(rules.linking.iter().copied()) {
            let Some(after) = rest.strip_prefix(link) else {
                continue;
            };
            if after.is_empty() {
                continue; // a linking morpheme alone is not a constituent
            }
            if let Some(mut tail) = rec(after, tagger, rules, depth + 1) {
                let mut parts = vec![prefix.to_owned()];
                parts.append(&mut tail);
                return Some(parts);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tagger::build_artifact;

    fn de_rules() -> Compounding {
        Compounding {
            linking: &["s", "es", "n", "en"],
            head_is_last: true,
            min_part_len: 3,
        }
    }

    fn tagger_of(words: &[(&str, &str, &str)]) -> Tagger {
        let mut map: BTreeMap<String, Vec<WordData>> = BTreeMap::new();
        for (w, lemma, tag) in words {
            map.entry((*w).to_owned()).or_default().push(WordData {
                lemma: (*lemma).to_owned(),
                tag: (*tag).to_owned(),
            });
        }
        Tagger::from_bytes(&build_artifact(&map).unwrap()).unwrap()
    }

    #[test]
    fn splits_known_compounds_and_tags_the_head() {
        let tagger = tagger_of(&[
            ("Haus", "Haus", "SUB:NOM:SIN:NEU"),
            ("Tür", "Tür", "SUB:NOM:SIN:FEM"),
            ("Arbeit", "Arbeit", "SUB:NOM:SIN:FEM"),
            ("Zimmer", "Zimmer", "SUB:NOM:SIN:NEU"),
        ]);
        let rules = de_rules();

        // Haustür = Haus + Tür → head Tür (feminine noun); lemma is the whole compound.
        let h = analyze_compound("Haustür", &tagger, &rules).expect("Haustür splits");
        assert_eq!(h[0].tag, "SUB:NOM:SIN:FEM");
        assert_eq!(h[0].lemma, "Haustür");

        // Arbeitszimmer = Arbeit + s + Zimmer → head Zimmer (neuter).
        let z = analyze_compound("Arbeitszimmer", &tagger, &rules).expect("Arbeitszimmer splits");
        assert_eq!(z[0].tag, "SUB:NOM:SIN:NEU");
        assert!(is_compound("Arbeitszimmer", &tagger, &rules));
    }

    #[test]
    fn rejects_non_compounds() {
        let tagger = tagger_of(&[("Haus", "Haus", "SUB:NOM:SIN:NEU")]);
        let rules = de_rules();
        assert!(analyze_compound("xyzzyfoo", &tagger, &rules).is_none());
        // A single known word is not a compound (needs ≥2 parts).
        assert!(!is_compound("Haus", &tagger, &rules));
    }
}
