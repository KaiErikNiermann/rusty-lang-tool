//! Word-form synthesis from LanguageTool's synthesis dictionary.
//!
//! LT's `BaseSynthesizer` (v6.8, `languagetool-core/.../synthesis/BaseSynthesizer.java`) answers
//! "the forms of lemma L with POS tag T" from a morfologik automaton keyed `L|T` (`lookup`, line 203),
//! adds the forms of the language's `added.txt`, removes those of `removed.txt` and
//! `do-not-synthesize.txt`, and, for a tag *regex*, runs the lookup for every tag of the language's
//! tag list that the regex matches in full (`synthesizeForPosTags`, line 272). A language's subclass
//! filters further (`GermanSynthesizer`, lines 54-98), which [`SynthPolicy`] carries as data.
//!
//! The artifact keeps the automaton as LT ships it (German: 5M entries in 1.6 MB) next to the tag
//! list and the supplements, and looks forms up in place with [`rlt_morfologik::Dictionary`].
//!
//! Not reproduced: `GermanSynthesizer.getCompoundForms`, the fallback that splits a lemma missing
//! from the dictionary into compound parts and inflects the last one.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use rkyv::{Archive, Deserialize, Serialize};
use rlt_core::Synthesizer;
use rlt_lang::SynthPolicy;
use rlt_morfologik::{Dictionary, parse_info};

/// Errors building or loading a synthesis artifact.
#[derive(Debug, thiserror::Error)]
pub enum SynthError {
    /// The rkyv artifact could not be (de)serialized.
    #[error("synthesis artifact: {0}")]
    Rkyv(String),
    /// The dictionary or its `.info` is not readable morfologik data.
    #[error("synthesis dictionary: {0}")]
    Dictionary(String),
    /// A supplement line is not `form<TAB>lemma<TAB>tag` (LT's `ManualSynthesizer` rejects it too).
    #[error("{file}: line {line}: expected 3 tab-separated fields, found {found}: {text:?}")]
    Supplement {
        /// Which supplement file.
        file: String,
        /// 1-based line number.
        line: usize,
        /// How many fields the line has.
        found: usize,
        /// The offending line.
        text: String,
    },
}

/// One line of a supplement file: the inflected `form` of `lemma` having POS `tag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualEntry {
    /// The inflected word form.
    pub form: String,
    /// The lemma it belongs to.
    pub lemma: String,
    /// Its POS tag, taken literally (LT's `ManualSynthesizer` does not expand `+`).
    pub tag: String,
}

/// Parse `added.txt`/`removed.txt`/`do-not-synthesize.txt` the way LT's `ManualSynthesizer.loadMapping`
/// does (`ManualSynthesizer.java` lines 194-222): trim each line, skip blank and `#` lines, cut a
/// trailing `# comment`, split on tabs, and insist on exactly three fields.
///
/// # Errors
/// Returns [`SynthError::Supplement`] for a line that is not three fields.
pub fn parse_manual_entries(text: &str, file: &str) -> Result<Vec<ManualEntry>, SynthError> {
    let mut entries = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let content = line
            .split_once('#')
            .map_or(line, |(before, _)| before)
            .trim();
        let fields: Vec<&str> = content.split('\t').collect();
        let &[form, lemma, tag] = fields.as_slice() else {
            return Err(SynthError::Supplement {
                file: file.to_owned(),
                line: index + 1,
                found: fields.len(),
                text: raw.to_owned(),
            });
        };
        entries.push(ManualEntry {
            form: form.to_owned(),
            lemma: lemma.to_owned(),
            tag: tag.to_owned(),
        });
    }
    Ok(entries)
}

/// The serialized synthesis artifact.
#[derive(Debug, Archive, Serialize, Deserialize)]
struct SynthData {
    /// The morfologik `.dict` bytes, unchanged.
    dict: Vec<u8>,
    /// The dictionary's `.info` text (separator, encoder, encoding).
    info: String,
    /// Every POS tag a tag regex is matched against: the language's tag list, then tags only the
    /// supplements use (LT `BaseSynthesizer.loadTags`, line 306).
    tags: Vec<String>,
    /// `(lemma|tag, form)` pairs from `added.txt`.
    added: Vec<(String, String)>,
    /// `(lemma|tag, form)` pairs from `removed.txt` and `do-not-synthesize.txt`.
    removed: Vec<(String, String)>,
}

/// The inputs of [`build_artifact`], as read from the LT jar and checkout.
pub struct SynthSources<'a> {
    /// The `.dict` bytes.
    pub dict: Vec<u8>,
    /// The `.info` text.
    pub info: &'a str,
    /// The tag list file (`SynthesizerTools.loadWords`: trimmed lines, no blanks or `#` comments).
    pub tags: &'a str,
    /// Entries of `added.txt`.
    pub added: &'a [ManualEntry],
    /// Entries of `removed.txt` and `do-not-synthesize.txt` (both only ever remove forms).
    pub removed: &'a [ManualEntry],
}

/// `lemma|tag`, the key LT's lookup uses (`BaseSynthesizer.lookup`, line 206).
fn key(lemma: &str, tag: &str) -> String {
    format!("{lemma}|{tag}")
}

/// Build the synthesis artifact bytes (offline).
///
/// # Errors
/// Returns [`SynthError`] if the dictionary is not readable or the table cannot be serialized.
pub fn build_artifact(sources: &SynthSources<'_>) -> Result<Vec<u8>, SynthError> {
    let meta = parse_info(sources.info).map_err(|e| SynthError::Dictionary(e.to_string()))?;
    Dictionary::new(sources.dict.clone(), meta)
        .map_err(|e| SynthError::Dictionary(e.to_string()))?;
    let mut tags: Vec<String> = sources
        .tags
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let known: HashSet<String> = tags.iter().cloned().collect();
    for entry in sources.added {
        if !known.contains(&entry.tag) && !tags.contains(&entry.tag) {
            tags.push(entry.tag.clone());
        }
    }
    let pairs = |entries: &[ManualEntry]| -> Vec<(String, String)> {
        entries
            .iter()
            .map(|e| (key(&e.lemma, &e.tag), e.form.clone()))
            .collect()
    };
    let data = SynthData {
        dict: sources.dict.clone(),
        info: sources.info.to_owned(),
        tags,
        added: pairs(sources.added),
        removed: pairs(sources.removed),
    };
    rkyv::to_bytes::<rkyv::rancor::Error>(&data)
        .map(|b| b.to_vec())
        .map_err(|e| SynthError::Rkyv(e.to_string()))
}

/// What the synthesizer needs of the dictionary: the forms stored under a `lemma|tag` key.
trait Lexicon: Send + Sync {
    fn forms(&self, key: &str) -> Vec<String>;
}

impl Lexicon for Dictionary {
    fn forms(&self, key: &str) -> Vec<String> {
        self.lookup(key).into_iter().map(|(form, _)| form).collect()
    }
}

/// A loaded synthesizer: the dictionary, the tag list, the supplements and the language's filters.
pub struct DictSynthesizer {
    dict: Box<dyn Lexicon>,
    tags: Vec<String>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, HashSet<String>>,
    policy: SynthPolicy,
}

impl DictSynthesizer {
    /// Load from the artifact bytes, with the language's `policy`.
    ///
    /// # Errors
    /// Returns [`SynthError`] if the artifact or its dictionary is malformed.
    pub fn from_bytes(bytes: &[u8], policy: SynthPolicy) -> Result<Self, SynthError> {
        let data = rkyv::from_bytes::<SynthData, rkyv::rancor::Error>(&rlt_ir::align_bytes(bytes))
            .map_err(|e| SynthError::Rkyv(e.to_string()))?;
        let meta = parse_info(&data.info).map_err(|e| SynthError::Dictionary(e.to_string()))?;
        let dict =
            Dictionary::new(data.dict, meta).map_err(|e| SynthError::Dictionary(e.to_string()))?;
        Ok(Self::with_lexicon(
            Box::new(dict),
            data.tags,
            data.added,
            data.removed,
            policy,
        ))
    }

    fn with_lexicon(
        dict: Box<dyn Lexicon>,
        tags: Vec<String>,
        added: Vec<(String, String)>,
        removed: Vec<(String, String)>,
        policy: SynthPolicy,
    ) -> Self {
        let mut added_by_key: HashMap<String, Vec<String>> = HashMap::new();
        for (k, form) in added {
            added_by_key.entry(k).or_default().push(form);
        }
        let mut removed_by_key: HashMap<String, HashSet<String>> = HashMap::new();
        for (k, form) in removed {
            removed_by_key.entry(k).or_default().insert(form);
        }
        Self {
            dict,
            tags,
            added: added_by_key,
            removed: removed_by_key,
            policy,
        }
    }

    /// The forms of `lemma` with exactly `tag`: `BaseSynthesizer.lookup` (lines 203-230) and the
    /// case guard of `GermanSynthesizer.lookup` (lines 54-68).
    fn lookup(&self, lemma: &str, tag: &str) -> Vec<String> {
        let k = key(lemma, tag);
        let mut forms = self.dict.forms(&k);
        if let Some(extra) = self.added.get(&k) {
            forms.extend(extra.iter().cloned());
        }
        if let Some(gone) = self.removed.get(&k) {
            forms.retain(|f| !gone.contains(f));
        }
        if self.policy.keep_case_class && !self.policy.case_exempt_lemmas.contains(&lemma) {
            let lower = starts_lowercase(lemma);
            forms.retain(|f| starts_lowercase(f) == lower);
        }
        forms
    }

    /// Drop the forms the language never offers (`GermanSynthesizer.synthesize`, lines 76 and 85).
    fn drop_forbidden(&self, mut forms: Vec<String>) -> Vec<String> {
        forms.retain(|f| !self.policy.forbidden_forms.contains(&f.as_str()));
        forms
    }
}

/// `StringTools.startsWithLowercase`: the first character is a lower-case letter.
fn starts_lowercase(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_lowercase)
}

impl Synthesizer for DictSynthesizer {
    fn synthesize(&self, lemma: &str, tag: &str) -> Vec<String> {
        self.drop_forbidden(self.lookup(lemma, tag))
    }

    fn synthesize_matching(&self, lemma: &str, tags: &Regex) -> Vec<String> {
        let forms = self
            .tags
            .iter()
            .filter(|t| tags.is_match(t))
            .flat_map(|t| self.lookup(lemma, t))
            .collect();
        self.drop_forbidden(forms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplements_follow_manual_synthesizer_parsing() {
        let text = "# header\n \t# indented comment\n\nfoo\tbar\tSUB:NOM # trailing\nnehm\tnehmen\tVER:1\t\n";
        let entries = parse_manual_entries(text, "added.txt").unwrap();
        assert_eq!(
            entries,
            [
                ManualEntry {
                    form: "foo".into(),
                    lemma: "bar".into(),
                    tag: "SUB:NOM".into()
                },
                ManualEntry {
                    form: "nehm".into(),
                    lemma: "nehmen".into(),
                    tag: "VER:1".into()
                },
            ]
        );
    }

    #[test]
    fn a_supplement_line_without_three_fields_is_an_error() {
        let err = parse_manual_entries("ok\tok\tTAG\nbroken\tline\n", "removed.txt").unwrap_err();
        assert!(
            matches!(
                &err,
                SynthError::Supplement {
                    line: 2,
                    found: 2,
                    ..
                }
            ),
            "{err}"
        );
    }

    /// A synthesizer over a literal `lemma|tag -> forms` table with German-like filters.
    fn synthesizer(
        entries: &[(&str, &[&str])],
        added: &[(&str, &str)],
        removed: &[(&str, &str)],
    ) -> DictSynthesizer {
        struct Table(HashMap<String, Vec<String>>);
        impl Lexicon for Table {
            fn forms(&self, key: &str) -> Vec<String> {
                self.0.get(key).cloned().unwrap_or_default()
            }
        }
        let table = entries
            .iter()
            .map(|(k, forms)| {
                (
                    (*k).to_owned(),
                    forms.iter().map(|f| (*f).to_owned()).collect(),
                )
            })
            .collect();
        let pairs = |p: &[(&str, &str)]| -> Vec<(String, String)> {
            p.iter()
                .map(|(k, f)| ((*k).to_owned(), (*f).to_owned()))
                .collect()
        };
        DictSynthesizer::with_lexicon(
            Box::new(Table(table)),
            ["ADJ:PRD:GRU", "ADJ:NOM:SIN:MAS:GRU:DEF", "SUB:NOM:SIN:MAS"]
                .map(str::to_owned)
                .to_vec(),
            pairs(added),
            pairs(removed),
            SynthPolicy {
                keep_case_class: true,
                case_exempt_lemmas: &["mein"],
                forbidden_forms: &["paßt"],
            },
        )
    }

    #[test]
    fn supplements_add_and_remove_forms_for_a_key() {
        let s = synthesizer(
            &[("gut|ADJ:PRD:GRU", &["gut", "gutt"])],
            &[("gut|ADJ:PRD:GRU", "guut")],
            &[("gut|ADJ:PRD:GRU", "gutt")],
        );
        assert_eq!(s.synthesize("gut", "ADJ:PRD:GRU"), ["gut", "guut"]);
    }

    #[test]
    fn a_form_must_start_in_the_case_of_its_lemma_except_for_exempt_lemmas() {
        let s = synthesizer(
            &[
                ("Haus|SUB:NOM:SIN:MAS", &["Haus", "haus"]),
                ("mein|SUB:NOM:SIN:MAS", &["Mein", "mein"]),
            ],
            &[],
            &[],
        );
        assert_eq!(s.synthesize("Haus", "SUB:NOM:SIN:MAS"), ["Haus"]);
        assert_eq!(s.synthesize("mein", "SUB:NOM:SIN:MAS"), ["Mein", "mein"]);
    }

    #[test]
    fn forbidden_forms_are_never_offered() {
        let s = synthesizer(&[("passen|ADJ:PRD:GRU", &["paßt", "passt"])], &[], &[]);
        assert_eq!(s.synthesize("passen", "ADJ:PRD:GRU"), ["passt"]);
    }

    #[test]
    fn a_tag_regex_visits_every_tag_it_matches_in_full() {
        let s = synthesizer(
            &[
                ("schnell|ADJ:PRD:GRU", &["schnell"]),
                ("schnell|ADJ:NOM:SIN:MAS:GRU:DEF", &["schnelle"]),
            ],
            &[],
            &[],
        );
        let all_adj = Regex::new("^(?:ADJ:.*)$").unwrap();
        let mut got = s.synthesize_matching("schnell", &all_adj);
        got.sort();
        assert_eq!(got, ["schnell", "schnelle"]);
        // A regex that matches a tag only in part matches nothing: LT uses `Matcher.matches`.
        let part = Regex::new("^(?:ADJ)$").unwrap();
        assert_eq!(s.synthesize_matching("schnell", &part), [] as [String; 0]);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads built artifacts from disk; miri runs under fs isolation"
    )]
    fn the_german_artifact_inflects_through_the_real_dictionary() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/de/synth.rkyv");
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!(
                "skip: resources/de/synth.rkyv not present (cargo xtask build-synth --lang de)"
            );
            return;
        };
        let policy = rlt_lang::DE.synth.expect("German has a synthesizer").policy;
        let s = DictSynthesizer::from_bytes(&bytes, policy).expect("load artifact");
        assert_eq!(s.synthesize("schnell", "ADJ:PRD:GRU"), ["schnell"]);
        assert_eq!(s.synthesize("Haus", "SUB:NOM:PLU:NEU"), ["Häuser"]);
        // A regex covers every tag it matches in full: both cases of the masculine definite.
        let re = Regex::new("^(?:ADJ:(NOM|AKK):SIN:MAS:GRU:DEF)$").unwrap();
        let mut forms = s.synthesize_matching("schnell", &re);
        forms.sort();
        assert_eq!(forms, ["schnelle", "schnellen"]);
        assert_eq!(s.synthesize("Xyzzyq", "SUB:NOM:SIN:NEU"), [] as [String; 0]);
        assert_eq!(s.synthesize("unser", "PRO:POS:NOM:SIN:MAS:BEG"), ["unser"]);
    }

    #[test]
    fn lowercase_start_is_a_letter_test_not_a_not_uppercase_test() {
        assert!(starts_lowercase("haus"));
        assert!(!starts_lowercase("Haus"));
        assert!(!starts_lowercase("3D"), "a digit is not lower case");
        assert!(!starts_lowercase(""));
    }
}
