//! Read LanguageTool's morfologik FSA part-of-speech dictionaries into `(inflected, lemma, tag)`
//! triples — the same shape [`rlt_native::build_from_triples`] consumes.
//!
//! LanguageTool ships each language's POS dictionary as a morfologik finite-state automaton (the
//! binary `<lang>.dict`, distributed in the `*-pos-dict` Maven artifacts, absent from the git repo)
//! plus a `.info` metadata file. There is no Rust reader for this format — morfologik is Java, with a
//! Ruby port (`mormor`); nlprule sidesteps it with Java/Python build tooling. This module is a small
//! pure-Rust reader for the **CFSA2** format (the one LT uses), so any LT language's *actual* current
//! dictionary becomes a download-and-convert away — no AGID reconstruction, no Java at build time.
//!
//! Format references: morfologik `CFSA2.java` (arc/v-int encoding) and the `mormor` enumerator + the
//! `SUFFIX` sequence encoder. CFSA2 only for now; FSA5 (`version=0x05`) would be a sibling reader.

use anyhow::{Result, anyhow, bail, ensure};

/// Parsed `.info` metadata (only the fields we need to decode entries).
#[derive(Debug, Clone)]
pub struct DictMeta {
    /// The byte separating `inflected | encoded-base | tag` within an FSA entry (`fsa.dict.separator`).
    pub separator: u8,
    /// How the base form is encoded relative to the inflected form (`fsa.dict.encoder`).
    pub encoder: Encoder,
    /// Byte encoding of the dict's strings (`fsa.dict.encoding`). `None` ⇒ UTF-8 (en/de): validated
    /// via `from_utf8`, invalid entries skipped. `Some(enc)` ⇒ a legacy single-byte encoding such as
    /// Russian's KOI8-R, decoded via `encoding_rs`. The separator/encoder operate on the raw encoded
    /// bytes; only the final inflected/base/tag fields are decoded to UTF-8.
    pub encoding: Option<&'static encoding_rs::Encoding>,
}

/// The lemma-encoding scheme. LanguageTool's English POS dict uses `SUFFIX`; the others are here for
/// when more languages are wired up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    /// The base form is stored verbatim (no diff against the inflected form).
    None,
    /// `encoded[0]` = trailing bytes to trim from the inflected form (offset by `'A'`); `encoded[1..]`
    /// is the suffix to append. `base = inflected[..len - trim] + encoded[1..]`.
    Suffix,
}

/// Parse the `.info` file. Only `fsa.dict.separator` and `fsa.dict.encoder` are required.
///
/// # Errors
/// Returns an error if the separator/encoder are missing or the encoder is unsupported.
pub fn parse_info(info: &str) -> Result<DictMeta> {
    let mut separator = None;
    let mut encoder = None;
    let mut encoding = None;
    for line in info.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "fsa.dict.separator" => {
                let bytes = value.trim().as_bytes();
                ensure!(bytes.len() == 1, "separator must be one byte, got {value:?}");
                separator = Some(bytes[0]);
            }
            "fsa.dict.encoder" => {
                encoder = Some(match value.trim() {
                    "SUFFIX" => Encoder::Suffix,
                    "NONE" => Encoder::None,
                    other => bail!("unsupported morfologik encoder {other:?} (only SUFFIX/NONE)"),
                });
            }
            "fsa.dict.encoding" => {
                let label = value.trim();
                // UTF-8 stays on the validating `from_utf8` path (None); anything else resolves to
                // an encoding_rs codec (e.g. KOI8-R for Russian, windows-1251, iso-8859-*).
                if !label.eq_ignore_ascii_case("utf-8") && !label.eq_ignore_ascii_case("utf8") {
                    encoding = Some(
                        encoding_rs::Encoding::for_label(label.as_bytes())
                            .ok_or_else(|| anyhow!("unknown fsa.dict.encoding {label:?}"))?,
                    );
                }
            }
            _ => {}
        }
    }
    Ok(DictMeta {
        separator: separator.unwrap_or(b'+'),
        encoder: encoder.unwrap_or(Encoder::Suffix),
        encoding,
    })
}

/// Decode a field's raw bytes to a `String` per the dict's encoding. UTF-8 (`None`) validates and
/// returns `None` on invalid bytes (so the caller skips the entry, preserving en/de behaviour); a
/// legacy codec maps every byte and never fails (single-byte encodings are total).
fn decode_field(bytes: &[u8], meta: &DictMeta) -> Option<String> {
    match meta.encoding {
        None => String::from_utf8(bytes.to_vec()).ok(),
        Some(enc) => Some(enc.decode_without_bom_handling(bytes).0.into_owned()),
    }
}

/// Read a CFSA2 `<lang>.dict` into `(inflected, lemma, tag)` triples.
///
/// # Errors
/// Returns an error if the bytes are not a CFSA2 automaton.
pub fn read_triples(dict: &[u8], meta: &DictMeta) -> Result<Vec<(String, String, String)>> {
    let fsa = Cfsa2::parse(dict)?;
    let mut out = Vec::new();
    let mut seq = Vec::new();
    fsa.visit(fsa.root, &mut seq, &mut |entry| {
        if let Some(triple) = decode_entry(entry, meta) {
            out.push(triple);
        }
    });
    Ok(out)
}

/// Split an FSA entry `inflected SEP encoded-base SEP tag` and reconstruct the base form.
fn decode_entry(entry: &[u8], meta: &DictMeta) -> Option<(String, String, String)> {
    let sep = meta.separator;
    let i1 = entry.iter().position(|&b| b == sep)?;
    let inflected = &entry[..i1];
    let rest = &entry[i1 + 1..];
    let i2 = rest.iter().position(|&b| b == sep)?;
    let encoded_base = &rest[..i2];
    let tag = &rest[i2 + 1..];

    let base = match meta.encoder {
        Encoder::None => encoded_base.to_vec(),
        Encoder::Suffix => decode_suffix(inflected, encoded_base),
    };
    Some((
        decode_field(inflected, meta)?,
        decode_field(&base, meta)?,
        decode_field(tag, meta)?,
    ))
}

/// SUFFIX decoder: `base = inflected[.. len - trim] ++ encoded[1..]`, where `trim = (encoded[0] - 'A')
/// & 0xFF` (clamped — a large value means "replace the whole stem").
fn decode_suffix(inflected: &[u8], encoded: &[u8]) -> Vec<u8> {
    let Some((&code, suffix)) = encoded.split_first() else {
        return inflected.to_vec();
    };
    // `(code - 'A') & 0xFF` in u8 arithmetic; a large value (e.g. the 0xFF "replace all" sentinel)
    // saturates `keep` to 0 below.
    let trim = usize::from(code.wrapping_sub(b'A'));
    let keep = inflected.len().saturating_sub(trim);
    let mut base = inflected[..keep].to_vec();
    base.extend_from_slice(suffix);
    base
}

/// A parsed CFSA2 automaton (a borrowed view over the arcs region + the label table).
struct Cfsa2<'a> {
    /// The arcs region of the file (everything after the header). Arc offsets index into this.
    arcs: &'a [u8],
    /// `labels[i]` = the literal byte for an arc whose label index is `i` (`i` in `1..=31`).
    labels: Vec<u8>,
    /// Offset of the start node.
    root: usize,
}

// Arc flag bits (the high 3 bits of an arc's first byte); the low 5 bits are the label index.
const BIT_TARGET_NEXT: u8 = 1 << 7;
const BIT_LAST_ARC: u8 = 1 << 6;
const BIT_FINAL_ARC: u8 = 1 << 5;
const LABEL_INDEX_MASK: u8 = 0x1f;
/// `fsa.dict` flag: nodes are prefixed with a v-int perfect-hash count. Off for LT's POS dicts.
const FLAG_NUMBERS: u16 = 0x100;

impl<'a> Cfsa2<'a> {
    fn parse(file: &'a [u8]) -> Result<Self> {
        ensure!(file.len() > 8, "file too short for a CFSA2 header");
        ensure!(&file[..4] == b"\\fsa", "not a morfologik FSA (bad magic)");
        ensure!(file[4] == 0xc6, "unsupported FSA version {:#x} (expected CFSA2 0xc6)", file[4]);
        let flags = (u16::from(file[5]) << 8) | u16::from(file[6]);
        ensure!(flags & FLAG_NUMBERS == 0, "CFSA2 with NUMBERS flag is unsupported");
        let n_labels = usize::from(file[7]);
        let table_end = 8 + n_labels;
        ensure!(file.len() >= table_end, "truncated label table");
        let labels = file[8..table_end].to_vec();
        let mut fsa = Self {
            arcs: &file[table_end..],
            labels,
            root: 0,
        };
        // The root node is the destination of the implicit epsilon arc at offset 0.
        fsa.root = fsa.target(0).unwrap_or(0);
        ensure!(fsa.root != 0, "empty automaton");
        Ok(fsa)
    }

    fn flags(&self, arc: usize) -> u8 {
        self.arcs[arc]
    }

    /// The label byte of `arc` (from the table when the index is non-zero, else the explicit byte).
    fn label(&self, arc: usize) -> u8 {
        let index = (self.flags(arc) & LABEL_INDEX_MASK) as usize;
        if index > 0 {
            self.labels[index]
        } else {
            self.arcs[arc + 1]
        }
    }

    fn is_final(&self, arc: usize) -> bool {
        self.flags(arc) & BIT_FINAL_ARC != 0
    }

    fn is_last(&self, arc: usize) -> bool {
        self.flags(arc) & BIT_LAST_ARC != 0
    }

    /// Advance past the v-int starting at `offset`; returns the offset just after it.
    fn skip_vint(&self, mut offset: usize) -> usize {
        while self.arcs[offset] & 0x80 != 0 {
            offset += 1;
        }
        offset + 1
    }

    /// Read the LEB128 v-int at `offset` (7 bits/byte, little-endian, high bit = continuation).
    fn read_vint(&self, mut offset: usize) -> usize {
        let mut value = 0usize;
        let mut shift = 0u32;
        loop {
            let byte = self.arcs[offset];
            offset += 1;
            value |= usize::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        value
    }

    /// Offset of the arc after `arc` within the same node.
    fn skip_arc(&self, arc: usize) -> usize {
        let flags = self.flags(arc);
        let mut offset = arc + 1;
        if flags & LABEL_INDEX_MASK == 0 {
            offset += 1; // explicit label byte
        }
        if flags & BIT_TARGET_NEXT == 0 {
            offset = self.skip_vint(offset); // target v-int
        }
        offset
    }

    /// The next sibling arc, or `0` when `arc` is the last in its node.
    fn next_arc(&self, arc: usize) -> usize {
        if self.is_last(arc) {
            0
        } else {
            self.skip_arc(arc)
        }
    }

    /// The destination node of `arc`, or `None` if the arc is terminal (a leaf — no continuation).
    fn target(&self, arc: usize) -> Option<usize> {
        let flags = self.flags(arc);
        if flags & BIT_TARGET_NEXT != 0 {
            // The destination node immediately follows this node's last arc.
            let mut a = arc;
            while !self.is_last(a) {
                a = self.skip_arc(a);
            }
            Some(self.skip_arc(a))
        } else {
            let offset = arc + if flags & LABEL_INDEX_MASK == 0 { 2 } else { 1 };
            match self.read_vint(offset) {
                0 => None,
                node => Some(node),
            }
        }
    }

    /// Depth-first walk yielding every accepted byte sequence (one per dictionary entry).
    fn visit(&self, node: usize, seq: &mut Vec<u8>, emit: &mut impl FnMut(&[u8])) {
        let mut arc = node; // getFirstArc(node) == node (NUMBERS flag is off)
        loop {
            seq.push(self.label(arc));
            if self.is_final(arc) {
                emit(seq);
            }
            if let Some(dest) = self.target(arc) {
                self.visit(dest, seq, emit);
            }
            seq.pop();
            if self.is_last(arc) {
                break;
            }
            arc = self.next_arc(arc);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn suffix_decode_regular_and_irregular() {
        // running -> run (trim 4 = 'E', no suffix); left -> leave (trim 2 = 'C', suffix "ave")
        assert_eq!(decode_suffix(b"running", b"E"), b"run");
        assert_eq!(decode_suffix(b"left", b"Cave"), b"leave");
        assert_eq!(decode_suffix(b"the", b"A"), b"the"); // trim 0, identity
    }

    #[test]
    fn reads_real_languagetool_english_dict() {
        let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/en"));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("pos.dict")),
            std::fs::read_to_string(base.join("pos.info")),
        ) else {
            eprintln!("skip: resources/en/pos.dict not present");
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'+');
        assert_eq!(meta.encoder, Encoder::Suffix);

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("english.dict: {} triples", triples.len());
        assert!(triples.len() > 100_000, "expected a large dictionary, got {}", triples.len());

        // Group by inflected → set of (lemma, tag), then spot-check known forms.
        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word.entry(inflected.clone()).or_default().push((lemma.clone(), tag.clone()));
        }
        let has = |w: &str, lemma: &str, tag: &str| {
            by_word.get(w).is_some_and(|v| v.iter().any(|(l, t)| l == lemma && t == tag))
        };
        // Closed-class words AGID lacked, and a couple of inflections — proving the decode is correct.
        assert!(has("the", "the", "DT"), "the/DT: {:?}", by_word.get("the"));
        assert!(has("running", "run", "VBG"), "running: {:?}", by_word.get("running"));
        assert!(has("is", "be", "VBZ"), "is: {:?}", by_word.get("is"));
        assert!(has("better", "good", "JJR") || has("better", "well", "RBR"), "better: {:?}", by_word.get("better"));
    }

    #[test]
    fn reads_real_languagetool_german_dict() {
        // German exercises a different separator (`_`) + non-ASCII (umlaut) SUFFIX decoding + the STTS
        // tagset — proving the reader is language-agnostic, not English-specific.
        let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/de"));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("pos.dict")),
            std::fs::read_to_string(base.join("pos.info")),
        ) else {
            eprintln!("skip: resources/de/pos.dict not present (cargo xtask build-tagger --lang de)");
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'_', "German uses an underscore separator");
        assert_eq!(meta.encoder, Encoder::Suffix);

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("german.dict: {} triples", triples.len());
        assert!(triples.len() > 1_000_000, "German morphology is rich; got {}", triples.len());

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word.entry(inflected.clone()).or_default().push((lemma.clone(), tag.clone()));
        }
        let has = |w: &str, lemma: &str, tag: &str| {
            by_word.get(w).is_some_and(|v| v.iter().any(|(l, t)| l == lemma && t == tag))
        };
        // Closed-class + an umlaut plural (Häuser→Haus) — the umlaut proves multibyte SUFFIX trimming.
        assert!(has("der", "der", "ART:DEF:DAT:SIN:FEM"), "der: {:?}", by_word.get("der"));
        assert!(has("ist", "sein", "VER:3:SIN:PRÄ:NON"), "ist: {:?}", by_word.get("ist"));
        assert!(has("Häuser", "Haus", "SUB:NOM:PLU:NEU"), "Häuser: {:?}", by_word.get("Häuser"));
    }

    #[test]
    fn reads_real_languagetool_russian_dict() {
        // Russian's russian.dict ships in the LT repo and is KOI8-R encoded (not UTF-8) — this proves
        // the `fsa.dict.encoding` path decodes Cyrillic correctly (every triple would be dropped if we
        // treated the bytes as UTF-8), and that SUFFIX trimming works in that single-byte encoding.
        let base = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/lt/_repo/languagetool-language-modules/ru/src/main/resources/org/languagetool/resource/ru"
        ));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("russian.dict")),
            std::fs::read_to_string(base.join("russian.info")),
        ) else {
            eprintln!("skip: russian.dict not present (run `cargo xtask fetch-lt`)");
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'+');
        assert_eq!(meta.encoder, Encoder::Suffix);
        assert_eq!(
            meta.encoding.map(encoding_rs::Encoding::name),
            Some("KOI8-R"),
            "russian.info declares fsa.dict.encoding=koi8-r",
        );

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("russian.dict: {} triples", triples.len());
        assert!(triples.len() > 1_000_000, "Russian morphology is rich; got {}", triples.len());

        // Every decoded string must be valid UTF-8 Cyrillic (no KOI8-R bytes leaked through, no
        // replacement chars from a mis-decode).
        assert!(
            triples
                .iter()
                .take(10_000)
                .all(|(i, l, _)| !i.contains('\u{fffd}') && !l.contains('\u{fffd}')),
            "decoded forms must be clean UTF-8",
        );

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word.entry(inflected.clone()).or_default().push((lemma.clone(), tag.clone()));
        }
        // Lemma + tag-prefix spot-checks (exact morphology strings aren't pinned): «книги» (a non-Nom
        // form of «книга», book) is a SUFFIX diff that must reconstruct the base; «читаю» → «читать».
        let lemma_with_tag = |w: &str, lemma: &str, tag_prefix: &str| {
            by_word
                .get(w)
                .is_some_and(|v| v.iter().any(|(l, t)| l == lemma && t.starts_with(tag_prefix)))
        };
        assert!(lemma_with_tag("книги", "книга", "NN"), "книги→книга/NN: {:?}", by_word.get("книги"));
        assert!(lemma_with_tag("читаю", "читать", "VB"), "читаю→читать/VB: {:?}", by_word.get("читаю"));
    }

    #[test]
    fn reads_real_languagetool_arabic_dict() {
        // Arabic's arabic.dict ships in the LT repo, CFSA2 + UTF-8 (unlike Russian's KOI8-R). This
        // pins the format and proves Arabic-script SUFFIX decoding produces clean UTF-8 — and that the
        // dict keys are UNvocalized (no tashkeel), which is what drives `Normalization::StripCombiningMarks`.
        let base = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/lt/_repo/languagetool-language-modules/ar/src/main/resources/org/languagetool/resource/ar"
        ));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("arabic.dict")),
            std::fs::read_to_string(base.join("arabic.info")),
        ) else {
            eprintln!("skip: arabic.dict not present (run `cargo xtask fetch-lt`)");
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'+');
        assert_eq!(meta.encoder, Encoder::Suffix);
        assert_eq!(meta.encoding, None, "arabic.info declares fsa.dict.encoding=utf-8");

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("arabic.dict: {} triples", triples.len());
        assert!(triples.len() > 1_000_000, "Arabic morphology is rich; got {}", triples.len());

        // Decoded forms are clean UTF-8, and the dict keys carry no tashkeel (Arabic combining marks
        // U+064B–065F / U+0670) — i.e. the dict is unvocalized, which is what makes the engine's
        // `StripCombiningMarks` necessary for vocalized input.
        let tashkeel = |s: &str| s.chars().any(|c| ('\u{064B}'..='\u{065F}').contains(&c) || c == '\u{0670}');
        assert!(
            triples
                .iter()
                .take(20_000)
                .all(|(i, l, _)| !i.contains('\u{fffd}') && !l.contains('\u{fffd}') && !tashkeel(i)),
            "decoded keys must be clean, unvocalized UTF-8",
        );

        // كتاب (kitāb, "book") is a noun — assert it's an inflected key with a noun (`N`) tag.
        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word.entry(inflected.clone()).or_default().push((lemma.clone(), tag.clone()));
        }
        assert!(
            by_word.get("كتاب").is_some_and(|v| v.iter().any(|(_, t)| t.starts_with('N'))),
            "كتاب should be a known noun: {:?}",
            by_word.get("كتاب"),
        );
    }

    #[test]
    fn reads_real_languagetool_spanish_dict() {
        // Spanish's POS dict is Maven-shipped by Softcatalà (`org.softcatala:spanish-pos-dict`),
        // extracted to `resources/es/pos.dict` by `build-tagger`. It is CFSA2 + UTF-8, separator `_`,
        // SUFFIX-encoded. This pins the format and proves precomposed Spanish accents `áéíóúüñ`
        // survive as real dict keys (no combining marks → `Normalization::None`, unlike Arabic).
        let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/es"));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("pos.dict")),
            std::fs::read_to_string(base.join("pos.info")),
        ) else {
            eprintln!("skip: es pos.dict not present (run `cargo xtask build-tagger --lang es`)");
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'_', "es-ES.info declares fsa.dict.separator=_");
        assert_eq!(meta.encoder, Encoder::Suffix);
        assert_eq!(meta.encoding, None, "es-ES.info declares fsa.dict.encoding=utf-8");

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("es pos.dict: {} triples", triples.len());
        assert!(triples.len() > 1_000_000, "Spanish morphology is rich; got {}", triples.len());

        // Decoded forms are clean UTF-8 and the dict keys carry NO combining marks (Spanish accents
        // are precomposed: á = U+00E1, not a + U+0301) — which is what makes `Normalization::None`
        // correct (no StripCombiningMarks needed, unlike Arabic).
        let combining = |s: &str| s.chars().any(|c| ('\u{0300}'..='\u{036F}').contains(&c));
        assert!(
            triples
                .iter()
                .take(50_000)
                .all(|(i, l, _)| !i.contains('\u{fffd}') && !l.contains('\u{fffd}') && !combining(i)),
            "decoded keys must be clean, precomposed UTF-8",
        );

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word.entry(inflected.clone()).or_default().push((lemma.clone(), tag.clone()));
        }
        // «casa» (house) is a feminine noun → EAGLES `NC` (Nombre Común); «sofá» (an accented key)
        // must be present and reconstruct its lemma — proving accented forms are first-class keys.
        assert!(
            by_word.get("casa").is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("NC"))),
            "casa should be a known common noun: {:?}",
            by_word.get("casa"),
        );
        assert!(
            by_word.get("sofá").is_some_and(|v| v.iter().any(|(l, t)| l == "sofá" && t.starts_with('N'))),
            "accented sofá should be a known noun key: {:?}",
            by_word.get("sofá"),
        );
    }

    #[test]
    fn rejects_fsa5_with_clean_error() {
        // FSA5 (`\fsa\x05`) is a different format we don't yet read — the guard must error cleanly,
        // never panic, so a language that ships FSA5 fails with an actionable message (future branch).
        let fsa5 = b"\\fsa\x05\x00\x00\x00rest";
        let err = read_triples(
            fsa5,
            &DictMeta { separator: b'+', encoder: Encoder::Suffix, encoding: None },
        )
        .expect_err("FSA5 must be rejected");
        assert!(err.to_string().contains("CFSA2"), "actionable error: {err}");
    }
}
