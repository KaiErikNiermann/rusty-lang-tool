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

/// Read a morfologik `<lang>.dict` into `(inflected, lemma, tag)` triples. Dispatches on the FSA
/// version byte: CFSA2 (`0xc6`, the compressed format en/de/ru/ar ship) or FSA5 (`0x05`, the older
/// format Italian ships).
///
/// # Errors
/// Returns an error if the bytes are not a supported morfologik automaton.
pub fn read_triples(dict: &[u8], meta: &DictMeta) -> Result<Vec<(String, String, String)>> {
    ensure!(dict.len() > 8 && &dict[..4] == b"\\fsa", "not a morfologik FSA (bad magic)");
    let mut out = Vec::new();
    match dict[4] {
        0xc6 => collect_triples(&Cfsa2::parse(dict)?, meta, &mut out),
        0x05 => collect_triples(&Fsa5::parse(dict)?, meta, &mut out),
        v => bail!("unsupported FSA version {v:#x} (expected CFSA2 0xc6 or FSA5 0x05)"),
    }
    Ok(out)
}

/// Walk an automaton, decoding each accepted byte sequence into a triple.
fn collect_triples(fsa: &impl Fsa, meta: &DictMeta, out: &mut Vec<(String, String, String)>) {
    let mut seq = Vec::new();
    visit(fsa, fsa.root(), &mut seq, &mut |entry| {
        if let Some(triple) = decode_entry(entry, meta) {
            out.push(triple);
        }
    });
}

/// The arc primitives shared by the CFSA2 and FSA5 readers; the [`visit`] DFS is identical for both.
trait Fsa {
    /// Offset of the start node.
    fn root(&self) -> usize;
    /// The label byte consumed by `arc`.
    fn label(&self, arc: usize) -> u8;
    /// Whether `arc` accepts (ends a dictionary entry).
    fn is_final(&self, arc: usize) -> bool;
    /// Whether `arc` is the last in its node.
    fn is_last(&self, arc: usize) -> bool;
    /// The destination node of `arc`, or `None` if it has no continuation (a leaf).
    fn target(&self, arc: usize) -> Option<usize>;
    /// The next sibling arc within the node, or `0` when `arc` is the last.
    fn next_arc(&self, arc: usize) -> usize;
}

/// Depth-first walk yielding every accepted byte sequence (one per dictionary entry).
fn visit(fsa: &impl Fsa, node: usize, seq: &mut Vec<u8>, emit: &mut impl FnMut(&[u8])) {
    let mut arc = node; // getFirstArc(node) == node for LT dicts (no node-data prefix)
    loop {
        seq.push(fsa.label(arc));
        if fsa.is_final(arc) {
            emit(seq);
        }
        if let Some(dest) = fsa.target(arc) {
            visit(fsa, dest, seq, emit);
        }
        seq.pop();
        if fsa.is_last(arc) {
            break;
        }
        arc = fsa.next_arc(arc);
    }
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
}

impl Fsa for Cfsa2<'_> {
    fn root(&self) -> usize {
        self.root
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

    fn next_arc(&self, arc: usize) -> usize {
        if self.is_last(arc) {
            0
        } else {
            self.skip_arc(arc)
        }
    }

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
}

// FSA5 arc flags live in the byte at `arc + 1` (the first goto byte); the label is at `arc`.
const FSA5_BIT_FINAL: u8 = 1 << 0;
const FSA5_BIT_LAST: u8 = 1 << 1;
const FSA5_BIT_NEXT: u8 = 1 << 2;

/// The older, uncompressed morfologik FSA5 automaton (version `0x05`) — what Italian's `italian.dict`
/// ships. Unlike CFSA2 there is no label table (labels are inline bytes) and the goto field is a
/// fixed `gtl` bytes wide; the flags occupy the low 3 bits of the first goto byte and the destination
/// address is the goto field decoded little-endian and shifted right by 3. (Ported from morfologik's
/// `FSA5.java`.)
struct Fsa5<'a> {
    /// Everything after the 8-byte header; arc/node offsets index into this.
    arcs: &'a [u8],
    /// Goto-field width in bytes (`hgtl & 0x0f`).
    gtl: usize,
    /// Per-node header bytes before the first arc (`hgtl >> 4`; 0 for LT dicts).
    node_data_length: usize,
    /// Offset of the start node.
    root: usize,
}

impl<'a> Fsa5<'a> {
    fn parse(file: &'a [u8]) -> Result<Self> {
        ensure!(file.len() > 8, "file too short for an FSA5 header");
        ensure!(&file[..4] == b"\\fsa", "not a morfologik FSA (bad magic)");
        ensure!(file[4] == 0x05, "expected FSA5 version 0x05, got {:#x}", file[4]);
        // file[5] = filler, file[6] = annotation separator, file[7] = hgtl.
        let hgtl = file[7];
        let node_data_length = usize::from(hgtl >> 4);
        let gtl = usize::from(hgtl & 0x0f);
        ensure!(gtl >= 1, "FSA5 goto length must be >= 1 (hgtl {hgtl:#x})");
        let mut fsa = Self {
            arcs: &file[8..],
            gtl,
            node_data_length,
            root: 0,
        };
        // getRootNode(): skip the dummy first arc from node 0, then take the destination of the
        // first arc of the node it reaches.
        let epsilon = fsa.skip_arc(fsa.first_arc(0));
        fsa.root = fsa.target(fsa.first_arc(epsilon)).unwrap_or(0);
        ensure!(fsa.root != 0, "empty FSA5 automaton");
        Ok(fsa)
    }

    /// First arc of `node` (skips any node-data prefix; a no-op for LT dicts).
    fn first_arc(&self, node: usize) -> usize {
        self.node_data_length + node
    }

    /// The flag byte (first goto byte) of `arc`.
    fn flag_byte(&self, arc: usize) -> u8 {
        self.arcs[arc + 1]
    }

    fn is_next(&self, arc: usize) -> bool {
        self.flag_byte(arc) & FSA5_BIT_NEXT != 0
    }

    /// Offset of the arc after `arc`: a next-arc is `label + 1 flag byte` (2 bytes); otherwise
    /// `label + gtl goto bytes`.
    fn skip_arc(&self, arc: usize) -> usize {
        arc + if self.is_next(arc) { 2 } else { 1 + self.gtl }
    }

    /// The destination address packed into the `gtl` goto bytes (little-endian), with the low 3 flag
    /// bits dropped.
    fn address(&self, arc: usize) -> usize {
        let mut value = 0usize;
        for i in (0..self.gtl).rev() {
            value = (value << 8) | usize::from(self.arcs[arc + 1 + i]);
        }
        value >> 3
    }
}

impl Fsa for Fsa5<'_> {
    fn root(&self) -> usize {
        self.root
    }

    fn label(&self, arc: usize) -> u8 {
        self.arcs[arc]
    }

    fn is_final(&self, arc: usize) -> bool {
        self.flag_byte(arc) & FSA5_BIT_FINAL != 0
    }

    fn is_last(&self, arc: usize) -> bool {
        self.flag_byte(arc) & FSA5_BIT_LAST != 0
    }

    fn next_arc(&self, arc: usize) -> usize {
        if self.is_last(arc) {
            0
        } else {
            self.skip_arc(arc)
        }
    }

    fn target(&self, arc: usize) -> Option<usize> {
        if self.is_next(arc) {
            // The destination node immediately follows this arc.
            Some(self.skip_arc(arc))
        } else {
            match self.address(arc) {
                0 => None,
                node => Some(node),
            }
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
    fn reads_real_languagetool_french_dict() {
        // French's POS dict is Maven-shipped (org.languagetool:french-pos-dict), extracted by
        // `cargo xtask build-tagger --lang fr` to resources/fr/pos.dict — CFSA2 + UTF-8, SUFFIX, `_` sep.
        // Pins the Romance/Latin-with-accents path: precomposed accented keys (é/à/ç) decode as clean
        // UTF-8 with NO combining marks, which is what makes `Normalization::None` correct for French.
        let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/fr"));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("pos.dict")),
            std::fs::read_to_string(base.join("pos.info")),
        ) else {
            eprintln!("skip: resources/fr/pos.dict not present (run `cargo xtask build-tagger --lang fr`)");
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'_', "french.info declares fsa.dict.separator=_");
        assert_eq!(meta.encoder, Encoder::Suffix);
        assert_eq!(meta.encoding, None, "french.info declares fsa.dict.encoding=utf-8");

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("french pos.dict: {} triples", triples.len());
        assert!(triples.len() > 600_000, "French morphology is rich; got {}", triples.len());

        let combining = |s: &str| s.chars().any(|c| ('\u{0300}'..='\u{036F}').contains(&c));
        assert!(
            triples
                .iter()
                .take(20_000)
                .all(|(i, l, _)| !i.contains('\u{fffd}') && !l.contains('\u{fffd}') && !combining(i)),
            "decoded keys must be clean, precomposed (no combining marks) UTF-8",
        );

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word.entry(inflected.clone()).or_default().push((lemma.clone(), tag.clone()));
        }
        // «chats» (cats) is a SUFFIX diff from lemma «chat», masculine-plural noun `N m p`.
        assert!(
            by_word
                .get("chats")
                .is_some_and(|v| v.iter().any(|(l, t)| l == "chat" && t.starts_with("N m p"))),
            "chats→chat/N m p: {:?}",
            by_word.get("chats"),
        );
        // An accented common noun «canapé» (couch) must read back cleanly as a masculine noun.
        assert!(
            by_word.get("canapé").is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("N m"))),
            "canapé should be a known masculine noun: {:?}",
            by_word.get("canapé"),
        );
    }

    #[test]
    fn reads_real_languagetool_spanish_dict() {
        // Spanish's POS dict is Maven-shipped by Softcatalà (`org.softcatala:spanish-pos-dict`),
        // extracted to `resources/es/pos.dict` by `build-tagger`. CFSA2 + UTF-8, separator `_`, SUFFIX.
        // Proves precomposed Spanish accents `áéíóúüñ` survive as real dict keys (no combining marks).
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
        // «casa» (house) is a common noun → EAGLES `NC`; «sofá» (accented) must be a first-class key.
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
    fn reads_real_languagetool_italian_dict() {
        // Italian's italian.dict ships in the LT repo in the **FSA5** format (version 0x05, not CFSA2)
        // and is ISO-8859-15 encoded — this exercises both the FSA5 sibling reader and the `encoding`
        // seam at once. The dict is unvocalized Latin (no combining marks → `Normalization::None`).
        let base = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/lt/_repo/languagetool-language-modules/it/src/main/resources/org/languagetool/resource/it"
        ));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("italian.dict")),
            std::fs::read_to_string(base.join("italian.info")),
        ) else {
            eprintln!("skip: italian.dict not present (run `cargo xtask fetch-lt`)");
            return;
        };
        assert_eq!(dict[4], 0x05, "italian.dict is the FSA5 format");
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'_');
        assert_eq!(meta.encoder, Encoder::Suffix);
        assert_eq!(
            meta.encoding.map(encoding_rs::Encoding::name),
            Some("ISO-8859-15"),
            "italian.info declares fsa.dict.encoding=ISO-8859-15",
        );

        let triples = read_triples(&dict, &meta).expect("read FSA5 dict");
        eprintln!("italian.dict (FSA5): {} triples", triples.len());
        assert!(triples.len() > 400_000, "Italian morphology is rich; got {}", triples.len());

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word.entry(inflected.clone()).or_default().push((lemma.clone(), tag.clone()));
        }
        // «gatto» (cat) → masculine noun; «città» (accented, ISO-8859-15 à) must be a clean key —
        // proving the FSA5 traversal + ISO-8859-15 decode reconstruct real Italian forms.
        assert!(
            by_word.get("gatto").is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("NOUN-M"))),
            "gatto should be a masculine noun: {:?}",
            by_word.get("gatto"),
        );
        assert!(
            by_word.get("città").is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("NOUN"))),
            "accented città should decode as a clean noun key: {:?}",
            by_word.get("città"),
        );
    }

    #[test]
    fn rejects_unknown_fsa_version() {
        // A version byte that is neither CFSA2 (0xc6) nor FSA5 (0x05) must error cleanly, never panic.
        let bad = b"\\fsa\x07\x00\x00\x00rest";
        let err = read_triples(
            bad,
            &DictMeta { separator: b'+', encoder: Encoder::Suffix, encoding: None },
        )
        .expect_err("unknown FSA version must be rejected");
        assert!(err.to_string().contains("unsupported FSA version"), "actionable error: {err}");
    }
}
