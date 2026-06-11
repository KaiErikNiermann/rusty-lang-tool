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
    /// Compound-word splitting rules, if the language compounds (German does; English doesn't).
    pub compounds: Option<Compounding>,
}

/// Maven coordinates + in-jar layout of a language's morfologik POS dictionary.
#[derive(Debug, Clone, Copy)]
pub struct PosDict {
    /// Maven groupId (e.g. `org.languagetool` for English, `de.danielnaber` for German).
    pub group_id: &'static str,
    /// Maven artifactId (e.g. `english-pos-dict`, `german-pos-dict`).
    pub artifact_id: &'static str,
    /// Maven version (e.g. `0.6`, `1.2.4`).
    pub version: &'static str,
    /// Path of the `.dict` inside the jar.
    pub jar_dict_path: &'static str,
    /// Path of the `.info` inside the jar.
    pub jar_info_path: &'static str,
}

impl PosDict {
    /// The Maven Central jar URL.
    #[must_use]
    pub fn jar_url(&self) -> String {
        let group = self.group_id.replace('.', "/");
        format!(
            "https://repo1.maven.org/maven2/{group}/{a}/{v}/{a}-{v}.jar",
            a = self.artifact_id,
            v = self.version,
        )
    }
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
    /// The fetched POS `.dict` / `.info` / jar under the language dir.
    #[must_use]
    pub fn pos_dict_local(&self) -> String {
        format!("resources/{}/pos.dict", self.code)
    }
    #[must_use]
    /// Local path of the POS `.info`.
    pub fn pos_info_local(&self) -> String {
        format!("resources/{}/pos.info", self.code)
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
        _ => None,
    }
}

/// The `SENT_END`/`SENT_START` punctuation characters LanguageTool tags `PCT`. Shared default.
const PCT_CHARS: &[char] = &['.', ',', ';', ':', '…', '!', '?'];

/// English — the reference language. `tagset` reproduces today's hardcoded Penn literals exactly.
pub static EN: LangConfig = LangConfig {
    code: "en",
    lt_module: "en",
    pos_dict: PosDict {
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
    compounds: None,
};

/// German. The morfologik dict is complete (no AGID/closed-class); STTS-style tagset; compounds split.
/// The exact STTS structural-tag strings are refined against the dict vocabulary + de tagset.txt (P4);
/// the values here are the initial best-guess, validated by the German oracle.
pub static DE: LangConfig = LangConfig {
    code: "de",
    lt_module: "de",
    pos_dict: PosDict {
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
    compounds: Some(Compounding {
        linking: &["s", "es", "n", "en", "er", "e", "ens", "ns"],
        head_is_last: true,
        min_part_len: 3,
    }),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_lookup_and_paths() {
        assert!(config("fr").is_none());
        let en = config("en").unwrap();
        assert_eq!(en.tagger_path(), "resources/en/tagger.rkyv");
        assert_eq!(en.grammar_blob_path(), "resources/en/grammar.rkyv");
        assert_eq!(en.segment_srx_path(), "resources/segment.srx");
        assert_eq!(
            en.pos_dict.jar_url(),
            "https://repo1.maven.org/maven2/org/languagetool/english-pos-dict/0.6/english-pos-dict-0.6.jar"
        );
        let de = config("de").unwrap();
        assert_eq!(
            de.pos_dict.jar_url(),
            "https://repo1.maven.org/maven2/de/danielnaber/german-pos-dict/1.2.4/german-pos-dict-1.2.4.jar"
        );
        assert!(de.compounds.is_some() && en.compounds.is_none());
    }
}
