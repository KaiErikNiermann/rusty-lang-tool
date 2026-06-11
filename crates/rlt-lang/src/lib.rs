//! Per-language configuration — the single place a language is defined.
//!
//! Adding a language should be *data, not code*: a new [`LangConfig`] constant plus one arm in
//! [`config`]. Everything language-specific lives here — the LanguageTool module name, the POS-dict
//! Maven coordinates (which differ per language: English is `org.languagetool:english-pos-dict`,
//! German is `de.danielnaber:german-pos-dict`), the structural [`TagSet`] (Penn vs STTS tag names),
//! which optional sources apply (AGID/closed-class/L3/L4), and the compound-splitting rules.
//!
//! Built artifacts live under `resources/<code>/` (`tagger.rkyv`, `disambig.rkyv`, `grammar.rkyv`,
//! the fetched POS dict). `segment.srx` is shared (it is one multilingual file), selected per language
//! by [`LangConfig::code`].

#![forbid(unsafe_code)]

/// Everything that distinguishes one language from another.
#[derive(Debug, Clone, Copy)]
pub struct LangConfig {
    /// ISO code — the SRX language selector and the `resources/<code>/` artifact directory.
    pub code: &'static str,
    /// The `languagetool-language-modules/<lt_module>` segment in the LT repo (usually == `code`).
    pub lt_module: &'static str,
    /// Where the morfologik POS dictionary is published (coordinates differ per language).
    pub pos_dict: PosDict,
    /// The structural tags the engine assigns by token shape (tagset differs per language).
    pub tagset: TagSet,
    /// Which optional dictionary sources / cascade layers apply.
    pub sources: Sources,
    /// L1 spell-checking parameters (the script's alphabet).
    pub spell: SpellConfig,
    /// How surface forms are normalized to their dictionary-lookup key (en/de/ru: `None`).
    pub normalization: Normalization,
    /// Compound-word splitting rules, if the language compounds (German does; English doesn't).
    pub compounds: Option<Compounding>,
}

/// Where a language's morfologik POS dictionary comes from. Most languages publish a standalone
/// `*-pos-dict` jar on Maven Central; some (e.g. Russian) ship the `.dict`/`.info` inside the
/// LanguageTool repo itself, so they need no separate download.
#[derive(Debug, Clone, Copy)]
pub enum PosDict {
    /// A `*-pos-dict` jar on Maven Central (English `org.languagetool:english-pos-dict`, German
    /// `de.danielnaber:german-pos-dict`).
    Maven {
        /// Maven groupId.
        group_id: &'static str,
        /// Maven artifactId.
        artifact_id: &'static str,
        /// Maven version.
        version: &'static str,
        /// Path of the `.dict` inside the jar.
        jar_dict_path: &'static str,
        /// Path of the `.info` inside the jar.
        jar_info_path: &'static str,
    },
    /// Dict + info ship inside the LT repo checkout, as filenames under [`LangConfig::lt_resource_dir`]
    /// (Russian `russian.dict`/`russian.info`) — already present after `fetch-lt`, no download.
    Repo {
        /// `.dict` filename under `lt_resource_dir()`.
        dict_file: &'static str,
        /// `.info` filename under `lt_resource_dir()`.
        info_file: &'static str,
    },
}

impl PosDict {
    /// The Maven Central jar URL, or `None` for a repo-shipped dict.
    #[must_use]
    pub fn jar_url(&self) -> Option<String> {
        match *self {
            PosDict::Maven {
                group_id,
                artifact_id,
                version,
                ..
            } => Some(format!(
                "https://repo1.maven.org/maven2/{group}/{artifact_id}/{version}/{artifact_id}-{version}.jar",
                group = group_id.replace('.', "/"),
            )),
            PosDict::Repo { .. } => None,
        }
    }
}

/// How a surface form is normalized to its dictionary-lookup key. Stripping is applied by the engine
/// before every tagger lookup; the token's own `text`/`span` are never altered (diagnostics stay on
/// the original bytes). en/de/ru use `None` (a no-op); Arabic strips tashkeel so vocalized input
/// matches the unvocalized dict keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Normalization {
    /// The lookup key is the surface form unchanged (byte-identical; the default).
    None,
    /// Strip Unicode nonspacing marks (`Mn`) before lookup — Arabic tashkeel, Hebrew niqqud, etc.
    StripCombiningMarks,
}

/// L1 spell-checking parameters that vary by script.
#[derive(Debug, Clone, Copy)]
pub struct SpellConfig {
    /// The script's lower-case alphabet: the membership set a token must fall within to be
    /// spell-checked, and the pool edit-distance-1 suggestions are drawn from. ASCII `a–z` for
    /// en/de; Cyrillic `а–я` + `ё` for ru.
    pub alphabet: &'static str,
}

/// The structural tags a language's grammar anchors on, assigned by the engine from token shape.
/// `SENT_START`/`SENT_END`/`oov_tag` are LanguageTool-universal; the rest are tagset-specific
/// (English Penn `CD`/`PCT`/`NNP` vs German STTS-style `ZAL`/punct/`EIG`).
#[derive(Debug, Clone, Copy)]
pub struct TagSet {
    /// Tag for an all-digit token (Penn `CD`, German `ZAL`).
    pub digit_tag: &'static str,
    /// The punctuation class tag (Penn `PCT`).
    pub punctuation_tag: &'static str,
    /// Extra literal tags per punctuation character, e.g. `[(",", ","), (".", ".")]`.
    pub punctuation_classes: &'static [(&'static str, &'static str)],
    /// The characters treated as `punctuation_tag` punctuation.
    pub punctuation_chars: &'static [char],
    /// Tag for a capitalized out-of-lexicon word (Penn `NNP`, German `EIG`).
    pub proper_noun_tag: &'static str,
    /// Tag for a lower-case out-of-lexicon word (`UNKNOWN`, universal).
    pub oov_tag: &'static str,
    /// Sentence-start sentinel tag (`SENT_START`, universal).
    pub sent_start: &'static str,
    /// Sentence-end tag on the final token (`SENT_END`, universal).
    pub sent_end: &'static str,
}

/// Which optional dictionary sources and cascade layers a language uses.
#[derive(Debug, Clone, Copy)]
pub struct Sources {
    /// Reconstruct the tagger from AGID when no morfologik dict is available (English fallback only).
    pub uses_agid: bool,
    /// A hand-authored closed-class supplement path (English AGID fallback only).
    pub closed_class: Option<&'static str>,
    /// L3 confusion-pair detection (needs a per-language n-gram corpus; English only for now).
    pub confusion: bool,
    /// L4 neural tagger (GECToR; English only for now).
    pub neural_l4: bool,
}

/// Compound-word splitting parameters (e.g. German `Haus|tür`, with linking morphemes).
#[derive(Debug, Clone, Copy)]
pub struct Compounding {
    /// Linking morphemes that may join constituents (`-s-`, `-es-`, `-n-`, `-en-`, …).
    pub linking: &'static [&'static str],
    /// Whether the head (which determines the compound's POS) is the last constituent.
    pub head_is_last: bool,
    /// Minimum byte length of a constituent (avoids splitting into fragments).
    pub min_part_len: usize,
}

impl LangConfig {
    /// `resources/<code>/` — the built-artifact directory for this language.
    #[must_use]
    pub fn resource_dir(&self) -> String {
        format!("resources/{}", self.code)
    }
    /// The FST POS-tagger artifact.
    #[must_use]
    pub fn tagger_path(&self) -> String {
        format!("resources/{}/tagger.rkyv", self.code)
    }
    /// The disambiguation tag-action artifact.
    #[must_use]
    pub fn disambig_path(&self) -> String {
        format!("resources/{}/disambig.rkyv", self.code)
    }
    /// The compiled grammar-rules (IR) artifact.
    #[must_use]
    pub fn grammar_blob_path(&self) -> String {
        format!("resources/{}/grammar.rkyv", self.code)
    }
    /// The L3 confusion-model artifact (English only).
    #[must_use]
    pub fn confusion_path(&self) -> String {
        format!("resources/{}/confusion.rkyv", self.code)
    }
    /// The L4 neural-tagger artifact directory (English only).
    #[must_use]
    pub fn l4_dir(&self) -> String {
        format!("resources/{}/l4", self.code)
    }
    /// The shared multilingual SRX segmentation file (one file for all languages).
    #[must_use]
    pub fn segment_srx_path(&self) -> &'static str {
        "resources/segment.srx"
    }
    /// The POS `.dict`: under the language dir for a Maven dict (where `fetch_pos_dict` extracts it),
    /// or directly in the LT repo checkout for a [`PosDict::Repo`] dict.
    #[must_use]
    pub fn pos_dict_local(&self) -> String {
        match self.pos_dict {
            PosDict::Maven { .. } => format!("resources/{}/pos.dict", self.code),
            PosDict::Repo { dict_file, .. } => format!("{}/{dict_file}", self.lt_resource_dir()),
        }
    }
    #[must_use]
    /// Local path of the POS `.info` (alongside the `.dict`).
    pub fn pos_info_local(&self) -> String {
        match self.pos_dict {
            PosDict::Maven { .. } => format!("resources/{}/pos.info", self.code),
            PosDict::Repo { info_file, .. } => format!("{}/{info_file}", self.lt_resource_dir()),
        }
    }
    #[must_use]
    /// Local path of the downloaded POS-dict jar.
    pub fn pos_jar_local(&self) -> String {
        format!("resources/{}/pos-dict.jar", self.code)
    }

    /// LT repo `…/resource/<lt_module>` directory (added/removed/multiwords/tagset/disambiguation).
    #[must_use]
    pub fn lt_resource_dir(&self) -> String {
        format!(
            "resources/lt/_repo/languagetool-language-modules/{m}/src/main/resources/org/languagetool/resource/{m}",
            m = self.lt_module,
        )
    }
    /// LT repo `…/rules/<lt_module>` directory (grammar.xml).
    #[must_use]
    pub fn lt_rules_dir(&self) -> String {
        format!(
            "resources/lt/_repo/languagetool-language-modules/{m}/src/main/resources/org/languagetool/rules/{m}",
            m = self.lt_module,
        )
    }
    /// Path of this language's `grammar.xml` in the LT checkout.
    #[must_use]
    pub fn grammar_xml_path(&self) -> String {
        format!("{}/grammar.xml", self.lt_rules_dir())
    }
    /// Path of this language's `disambiguation.xml` in the LT checkout.
    #[must_use]
    pub fn disambiguation_xml_path(&self) -> String {
        format!("{}/disambiguation.xml", self.lt_resource_dir())
    }
    /// The `languagetool-language-modules/<lt_module>/…/org/languagetool` sparse-checkout path.
    #[must_use]
    pub fn lt_sparse_path(&self) -> String {
        format!(
            "languagetool-language-modules/{}/src/main/resources/org/languagetool",
            self.lt_module,
        )
    }
}

/// Look up a language by ISO code.
#[must_use]
pub fn config(code: &str) -> Option<&'static LangConfig> {
    match code {
        "en" => Some(&EN),
        "de" => Some(&DE),
        "ru" => Some(&RU),
        "ar" => Some(&AR),
        "fr" => Some(&FR),
        "es" => Some(&ES),
        "it" => Some(&IT),
        _ => None,
    }
}

/// The `SENT_END`/`SENT_START` punctuation characters LanguageTool tags `PCT`. Shared default.
const PCT_CHARS: &[char] = &['.', ',', ';', ':', '…', '!', '?'];

/// English — the reference language. `tagset` reproduces today's hardcoded Penn literals exactly.
pub static EN: LangConfig = LangConfig {
    code: "en",
    lt_module: "en",
    pos_dict: PosDict::Maven {
        group_id: "org.languagetool",
        artifact_id: "english-pos-dict",
        version: "0.6",
        jar_dict_path: "org/languagetool/resource/en/english.dict",
        jar_info_path: "org/languagetool/resource/en/english.info",
    },
    tagset: TagSet {
        digit_tag: "CD",
        punctuation_tag: "PCT",
        punctuation_classes: &[
            (",", ","),
            (".", "."),
            ("!", "."),
            ("?", "."),
            ("…", "."),
            (":", ":"),
            (";", ":"),
        ],
        punctuation_chars: PCT_CHARS,
        proper_noun_tag: "NNP",
        oov_tag: "UNKNOWN",
        sent_start: "SENT_START",
        sent_end: "SENT_END",
    },
    sources: Sources {
        uses_agid: true,
        closed_class: Some("data/en-closed-class.tsv"),
        confusion: true,
        neural_l4: true,
    },
    spell: SpellConfig {
        alphabet: "abcdefghijklmnopqrstuvwxyz",
    },
    normalization: Normalization::None,
    compounds: None,
};

/// German. The morfologik dict is complete (no AGID/closed-class); STTS-style tagset; compounds split.
/// The exact STTS structural-tag strings are refined against the dict vocabulary + de tagset.txt (P4);
/// the values here are the initial best-guess, validated by the German oracle.
pub static DE: LangConfig = LangConfig {
    code: "de",
    lt_module: "de",
    pos_dict: PosDict::Maven {
        group_id: "de.danielnaber",
        artifact_id: "german-pos-dict",
        version: "1.2.4",
        jar_dict_path: "org/languagetool/resource/de/german.dict",
        jar_info_path: "org/languagetool/resource/de/german.info",
    },
    // STTS-derived structural tags, verified against de/tagset.txt + grammar.xml + disambiguation.xml:
    // ZAN = digit sequence (Ziffernfolge, not the number-word ZAL); PKT = the `[.,;:…!?]` class (88
    // literal + many regex rule refs); EIG = Eigenname — grammar references `EIG:.*` regex, so the
    // colon-suffixed form matches the most rules; German grammar uses no literal punctuation postags.
    tagset: TagSet {
        digit_tag: "ZAN",
        punctuation_tag: "PKT",
        punctuation_classes: &[],
        punctuation_chars: PCT_CHARS,
        proper_noun_tag: "EIG:NOM:SIN:MAS",
        oov_tag: "UNKNOWN",
        sent_start: "SENT_START",
        sent_end: "SENT_END",
    },
    sources: Sources {
        uses_agid: false,
        closed_class: None,
        confusion: true,
        neural_l4: false,
    },
    spell: SpellConfig {
        alphabet: "abcdefghijklmnopqrstuvwxyz",
    },
    normalization: Normalization::None,
    compounds: Some(Compounding {
        linking: &["s", "es", "n", "en", "er", "e", "ens", "ns"],
        head_is_last: true,
        min_part_len: 3,
    }),
};

/// Russian — the first far-from-Latin language. The morfologik dict ships inside the LT repo as
/// `russian.dict`/`russian.info` (KOI8-R encoded — handled by the morfologik reader's `encoding`
/// support), so it needs no Maven download. Cyrillic alphabet for L1; no compounding; L3 confusion
/// uses the repo's `confusion_sets.txt` (n-grams come from a Leipzig corpus, no prebuilt set exists).
/// Structural tags are derived from `tags_russian.txt` + the ru rules and validated by the oracle (P4).
pub static RU: LangConfig = LangConfig {
    code: "ru",
    lt_module: "ru",
    pos_dict: PosDict::Repo {
        dict_file: "russian.dict",
        info_file: "russian.info",
    },
    // NN:Name:Masc:Sin:Nom = a masculine-nominative-singular proper name (the dict's general
    // proper-noun form); the ru rules reference SENT_START/SENT_END but no punctuation postag, so
    // that value is low-stakes; digit/number tagging (NumC:Nom) is the initial guess refined in P4.
    tagset: TagSet {
        digit_tag: "NumC:Nom",
        punctuation_tag: "PNCT",
        punctuation_classes: &[],
        punctuation_chars: PCT_CHARS,
        proper_noun_tag: "NN:Name:Masc:Sin:Nom",
        oov_tag: "UNKNOWN",
        sent_start: "SENT_START",
        sent_end: "SENT_END",
    },
    sources: Sources {
        uses_agid: false,
        closed_class: None,
        confusion: true,
        neural_l4: false,
    },
    spell: SpellConfig {
        alphabet: "абвгдеёжзийклмнопрстуфхцчшщъыьэюя",
    },
    normalization: Normalization::None,
    compounds: None,
};

/// The Arabic `[.,;:…!?]` punctuation class plus the Arabic comma `،`, semicolon `؛`, and question
/// mark `؟` (so they're tagged `PCT` like their Latin counterparts).
const AR_PCT_CHARS: &[char] = &['.', ',', ';', ':', '…', '!', '?', '،', '؛', '؟'];

/// Arabic — the first RTL / combining-mark language. Dict ships in the LT repo (`arabic.dict`, CFSA2,
/// UTF-8); input is vocalized but the dict keys are not, so `StripCombiningMarks` removes tashkeel
/// before lookup. L3 skipped (upstream `confusion_sets.txt` is empty). Tagset/alphabet derived via
/// `cargo xtask lang-inspect --code ar`; `PCT` is the punctuation postag the ar rules reference; the
/// proper-noun/digit tags are structurally near-dead (Arabic is caseless; no rule references digits).
pub static AR: LangConfig = LangConfig {
    code: "ar",
    lt_module: "ar",
    pos_dict: PosDict::Repo {
        dict_file: "arabic.dict",
        info_file: "arabic.info",
    },
    tagset: TagSet {
        digit_tag: "CD",
        punctuation_tag: "PCT",
        punctuation_classes: &[],
        punctuation_chars: AR_PCT_CHARS,
        proper_noun_tag: "NA-;-1U-;---",
        oov_tag: "UNKNOWN",
        sent_start: "SENT_START",
        sent_end: "SENT_END",
    },
    sources: Sources {
        uses_agid: false,
        closed_class: None,
        confusion: false,
        neural_l4: false,
    },
    // 37 base letters (incl. hamza/alef variants, ta marbuta, alef maqsura, tatweel) — tashkeel is
    // handled generically by the spell layer, so marks are not in the alphabet. Derived by lang-inspect.
    spell: SpellConfig {
        alphabet: "ءآأؤإئابةتثجحخدذرزسشصضطظعغـفقكلمنهوىي",
    },
    normalization: Normalization::StripCombiningMarks,
    compounds: None,
};

/// French — Romance. Maven dict (`org.languagetool:french-pos-dict`, CFSA2/UTF-8/SUFFIX, `_` sep).
/// Latin + accented base letters `àâçéèêëîïôûùüÿœæ` (ligatures œ/æ are single dict letters); accents
/// are precomposed (NFC), so `Normalization::None`. Tagset via `lang-inspect --code fr` + tagset.txt:
/// `Y` = cardinal digit, `Z` = proper name, `M` = punctuation marker class. L3 deferred (`confusion:false`).
pub static FR: LangConfig = LangConfig {
    code: "fr",
    lt_module: "fr",
    pos_dict: PosDict::Maven {
        group_id: "org.languagetool",
        artifact_id: "french-pos-dict",
        version: "0.7",
        jar_dict_path: "org/languagetool/resource/fr/french.dict",
        jar_info_path: "org/languagetool/resource/fr/french.info",
    },
    tagset: TagSet {
        digit_tag: "Y",
        punctuation_tag: "M",
        punctuation_classes: &[],
        punctuation_chars: PCT_CHARS,
        proper_noun_tag: "Z",
        oov_tag: "UNKNOWN",
        sent_start: "SENT_START",
        sent_end: "SENT_END",
    },
    sources: Sources {
        uses_agid: false,
        closed_class: None,
        confusion: false,
        neural_l4: false,
    },
    spell: SpellConfig {
        alphabet: "abcdefghijklmnopqrstuvwxyzàâçéèêëîïôûùüÿœæ",
    },
    normalization: Normalization::None,
    compounds: None,
};

/// Spanish — Romance. Maven dict shipped by Softcatalà (`org.softcatala:spanish-pos-dict`, jar entries
/// `…/resource/es/es-ES.dict`+`es-ES.info`, CFSA2/UTF-8/SUFFIX). EAGLES/Freeling tagset: `Z` = cifra
/// (digit), `NP00000` = underspecified proper noun (grammar anchors on `NP.*`); punctuation is the
/// disambiguation-added `_PUNCT` class (referenced `_PUNCT.*` 44× in grammar.xml) — assigned by token
/// shape. Inverted `¿¡` are SRX/disambiguation-level (`_QM_OPEN`), not structural, so not in
/// `punctuation_chars`. Accents `áéíóúüñ` are full dict letters → `Normalization::None`. L3 deferred.
pub static ES: LangConfig = LangConfig {
    code: "es",
    lt_module: "es",
    pos_dict: PosDict::Maven {
        group_id: "org.softcatala",
        artifact_id: "spanish-pos-dict",
        version: "2.5",
        jar_dict_path: "org/languagetool/resource/es/es-ES.dict",
        jar_info_path: "org/languagetool/resource/es/es-ES.info",
    },
    tagset: TagSet {
        digit_tag: "Z",
        punctuation_tag: "_PUNCT",
        punctuation_classes: &[],
        punctuation_chars: PCT_CHARS,
        proper_noun_tag: "NP00000",
        oov_tag: "UNKNOWN",
        sent_start: "SENT_START",
        sent_end: "SENT_END",
    },
    sources: Sources {
        uses_agid: false,
        closed_class: None,
        confusion: false,
        neural_l4: false,
    },
    spell: SpellConfig {
        alphabet: "abcdefghijklmnopqrstuvwxyzáéíóúüñ",
    },
    normalization: Normalization::None,
    compounds: None,
};

/// Italian — Romance, but the **first FSA5** language: `italian.dict` ships in the LT repo in the older
/// uncompressed morfologik FSA5 format (version `0x05`, read by the FSA5 sibling reader) rather than
/// CFSA2, and is **ISO-8859-15** encoded (handled by the `encoding_rs` seam, like Russian's KOI8-R).
/// Tagset via `lang-inspect --code it`: `PON` = punctuation, `NPR` = proper noun, `DET-NUM-CARD` =
/// cardinal. Accents `àèéìíîïòóùú` are precomposed dict letters → `Normalization::None`. L3 deferred.
pub static IT: LangConfig = LangConfig {
    code: "it",
    lt_module: "it",
    pos_dict: PosDict::Repo {
        dict_file: "italian.dict",
        info_file: "italian.info",
    },
    tagset: TagSet {
        digit_tag: "DET-NUM-CARD",
        punctuation_tag: "PON",
        punctuation_classes: &[],
        punctuation_chars: PCT_CHARS,
        proper_noun_tag: "NPR",
        oov_tag: "UNKNOWN",
        sent_start: "SENT_START",
        sent_end: "SENT_END",
    },
    sources: Sources {
        uses_agid: false,
        closed_class: None,
        confusion: false,
        neural_l4: false,
    },
    spell: SpellConfig {
        alphabet: "abcdefghijklmnopqrstuvwxyzàèéìîïòóùú",
    },
    normalization: Normalization::None,
    compounds: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lookup_and_paths() {
        assert!(config("xx").is_none());
        let fr = config("fr").unwrap();
        assert!(fr.pos_dict.jar_url().as_deref().is_some_and(|u| u.ends_with("french-pos-dict-0.7.jar")));
        assert_eq!(fr.tagset.digit_tag, "Y");
        let en = config("en").unwrap();
        assert_eq!(en.tagger_path(), "resources/en/tagger.rkyv");
        assert_eq!(en.grammar_blob_path(), "resources/en/grammar.rkyv");
        assert_eq!(en.segment_srx_path(), "resources/segment.srx");
        assert_eq!(
            en.pos_dict.jar_url().as_deref(),
            Some("https://repo1.maven.org/maven2/org/languagetool/english-pos-dict/0.6/english-pos-dict-0.6.jar")
        );
        let de = config("de").unwrap();
        assert_eq!(
            de.pos_dict.jar_url().as_deref(),
            Some("https://repo1.maven.org/maven2/de/danielnaber/german-pos-dict/1.2.4/german-pos-dict-1.2.4.jar")
        );
        assert!(de.compounds.is_some() && en.compounds.is_none());

        // Russian: repo-shipped dict (no Maven URL); the .dict/.info resolve into the LT checkout.
        let ru = config("ru").unwrap();
        assert!(ru.pos_dict.jar_url().is_none());
        assert!(ru.pos_dict_local().ends_with("/resource/ru/russian.dict"));
        assert!(ru.pos_info_local().ends_with("/resource/ru/russian.info"));
        assert_eq!(ru.spell.alphabet.chars().count(), 33);
    }
}
