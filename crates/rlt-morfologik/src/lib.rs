//! Read LanguageTool's morfologik FSA dictionaries: enumerate every entry as an `(inflected, lemma,
//! tag)` triple ([`read_triples`], the shape `rlt_native::build_from_triples` consumes), or look one
//! word up in place ([`Dictionary::lookup`], what morfologik's `DictionaryLookup` does).
//!
//! LanguageTool ships each language's dictionaries as morfologik finite-state automata (the binary
//! `<lang>.dict`, distributed in the `*-pos-dict` Maven artifacts, absent from the git repo) plus a
//! `.info` metadata file. There is no Rust reader for this format — morfologik is Java, with a
//! Ruby port (`mormor`); nlprule sidesteps it with Java/Python build tooling. This crate is a small
//! pure-Rust reader for the **CFSA2** format (the one LT uses most) and the older **FSA5**, so any LT
//! language's *actual* current dictionary becomes a download-and-convert away — no AGID
//! reconstruction, no Java at build time — and a synthesis dictionary can be queried at run time
//! straight from its compact automaton (German's 5M-entry `german_synth.dict` is 1.6 MB).
//!
//! Format references: morfologik `CFSA2.java` (arc/v-int encoding) and the `mormor` enumerator + the
//! `SUFFIX` sequence encoder.

use anyhow::{Result, anyhow, bail, ensure};

/// The version byte (after the `\\fsa` magic) of the compressed CFSA2 automaton en/de/ru/ar ship.
const CFSA2_VERSION: u8 = 0xc6;
/// The version byte of the older, uncompressed FSA5 automaton (Italian).
const FSA5_VERSION: u8 = 0x05;

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
    /// `fsa.dict.frequency-included` — when true, every entry's tag field has a single trailing
    /// frequency byte (a printable code, e.g. `'K'`) appended after the real tag; it must be dropped
    /// so tags match the grammar (French/Spanish ship this; en/de/ru/ar/it do not).
    pub frequency_included: bool,
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
    let mut frequency_included = false;
    for line in info.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "fsa.dict.separator" => {
                let bytes = value.trim().as_bytes();
                ensure!(
                    bytes.len() == 1,
                    "separator must be one byte, got {value:?}"
                );
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
            "fsa.dict.frequency-included" => {
                frequency_included = value.trim().eq_ignore_ascii_case("true");
            }
            _ => {}
        }
    }
    Ok(DictMeta {
        separator: separator.unwrap_or(b'+'),
        encoder: encoder.unwrap_or(Encoder::Suffix),
        encoding,
        frequency_included,
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
    ensure!(
        dict.len() > 8 && &dict[..4] == b"\\fsa",
        "not a morfologik FSA (bad magic)"
    );
    let mut out = Vec::new();
    match dict[4] {
        CFSA2_VERSION => collect_triples(&Cfsa2::parse(dict)?, meta, &mut out),
        FSA5_VERSION => collect_triples(&Fsa5::parse(dict)?, meta, &mut out),
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

/// A morfologik dictionary held in memory for repeated word lookups, without expanding it.
///
/// Mirrors morfologik's `DictionaryLookup.lookup(word)`: every entry whose inflected part equals
/// `word` yields its decoded base form and tag. For a synthesis dictionary (`german_synth.dict`) the
/// "word" is `lemma|POS-tag` and the decoded base form is the inflected word form.
#[derive(Debug)]
pub struct Dictionary {
    bytes: Vec<u8>,
    meta: DictMeta,
}

impl Dictionary {
    /// Take ownership of a `.dict` file's bytes, checking the header.
    ///
    /// # Errors
    /// Returns an error if the bytes are not a supported morfologik automaton.
    pub fn new(bytes: Vec<u8>, meta: DictMeta) -> Result<Self> {
        match bytes.get(..5) {
            Some(b"\\fsa\xc6") => {
                Cfsa2::parse(&bytes)?;
            }
            Some(b"\\fsa\x05") => {
                Fsa5::parse(&bytes)?;
            }
            _ => bail!("not a supported morfologik FSA (bad magic or version)"),
        }
        Ok(Self { bytes, meta })
    }

    /// The `(base form, tag)` of every entry whose inflected part is exactly `word`, in automaton
    /// order. Empty if `word` is absent (or cannot be encoded in the dictionary's byte encoding).
    #[must_use]
    pub fn lookup(&self, word: &str) -> Vec<(String, String)> {
        let key = match self.meta.encoding {
            None => word.as_bytes().to_vec(),
            Some(enc) => {
                let (bytes, _, had_errors) = enc.encode(word);
                if had_errors {
                    return Vec::new();
                }
                bytes.into_owned()
            }
        };
        let mut out = Vec::new();
        let sep = self.meta.separator;
        let mut emit = |rest: &[u8]| {
            let (encoded, tag) = match rest.iter().position(|&b| b == sep) {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, &[][..]),
            };
            out.extend(decode_tail(&key, encoded, tag, &self.meta));
        };
        // The header was validated in `new`, so parsing again cannot fail.
        if self.bytes[4] == CFSA2_VERSION {
            if let Ok(fsa) = Cfsa2::parse(&self.bytes) {
                lookup_entries(&fsa, &key, sep, &mut emit);
            }
        } else if let Ok(fsa) = Fsa5::parse(&self.bytes) {
            lookup_entries(&fsa, &key, sep, &mut emit);
        }
        out
    }
}

/// Follow `key` then `sep` from the root and emit what every entry continues with after them.
fn lookup_entries(fsa: &impl Fsa, key: &[u8], sep: u8, emit: &mut impl FnMut(&[u8])) {
    let mut node = fsa.root();
    let mut path = key.iter().copied().chain(std::iter::once(sep)).peekable();
    while let Some(byte) = path.next() {
        let Some(arc) = find_arc(fsa, node, byte) else {
            return;
        };
        if path.peek().is_some() {
            match fsa.target(arc) {
                Some(next) => node = next,
                None => return,
            }
        } else {
            // `key SEP` consumed: an entry may end right here, and others continue below.
            if fsa.is_final(arc) {
                emit(&[]);
            }
            if let Some(below) = fsa.target(arc) {
                visit(fsa, below, &mut Vec::new(), emit);
            }
        }
    }
}

/// The arc leaving `node` that carries `label`, if any.
fn find_arc(fsa: &impl Fsa, node: usize, label: u8) -> Option<usize> {
    let mut arc = node;
    loop {
        if fsa.label(arc) == label {
            return Some(arc);
        }
        if fsa.is_last(arc) {
            return None;
        }
        arc = fsa.next_arc(arc);
    }
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
    let (base, tag) = decode_tail(inflected, &rest[..i2], &rest[i2 + 1..], meta)?;
    Some((decode_field(inflected, meta)?, base, tag))
}

/// Decode the `encoded-base` and `tag` fields that follow `inflected` in an entry into strings.
fn decode_tail(
    inflected: &[u8],
    encoded_base: &[u8],
    mut tag: &[u8],
    meta: &DictMeta,
) -> Option<(String, String)> {
    // Drop the trailing frequency byte (a single appended printable code) so the tag matches the
    // grammar's postags, e.g. `N m pK` → `N m p`. No-op for dicts without `frequency-included`.
    if meta.frequency_included && !tag.is_empty() {
        tag = &tag[..tag.len() - 1];
    }
    let base = match meta.encoder {
        Encoder::None => encoded_base.to_vec(),
        Encoder::Suffix => decode_suffix(inflected, encoded_base),
    };
    Some((decode_field(&base, meta)?, decode_field(tag, meta)?))
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
        ensure!(
            file[4] == CFSA2_VERSION,
            "unsupported FSA version {:#x} (expected CFSA2 0xc6)",
            file[4]
        );
        let flags = (u16::from(file[5]) << 8) | u16::from(file[6]);
        ensure!(
            flags & FLAG_NUMBERS == 0,
            "CFSA2 with NUMBERS flag is unsupported"
        );
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
        ensure!(
            file[4] == FSA5_VERSION,
            "expected FSA5 version 0x05, got {:#x}",
            file[4]
        );
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
    use std::path::PathBuf;

    /// Resolve a repo-root-relative path (e.g. from `rlt_lang::lt_resource_path!`) against this
    /// crate's manifest dir. These fixtures read the real LanguageTool checkout, and each guards on
    /// the files being absent — so a path that drifts does not fail the test, it silently turns it
    /// into a no-op. Going through `rlt_lang` keeps the layout in one place.
    fn repo_path(rel: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).join(rel)
    }

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
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
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
        assert!(
            triples.len() > 100_000,
            "expected a large dictionary, got {}",
            triples.len()
        );

        // Group by inflected → set of (lemma, tag), then spot-check known forms.
        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word
                .entry(inflected.clone())
                .or_default()
                .push((lemma.clone(), tag.clone()));
        }
        let has = |w: &str, lemma: &str, tag: &str| {
            by_word
                .get(w)
                .is_some_and(|v| v.iter().any(|(l, t)| l == lemma && t == tag))
        };
        // Closed-class words AGID lacked, and a couple of inflections — proving the decode is correct.
        assert!(has("the", "the", "DT"), "the/DT: {:?}", by_word.get("the"));
        assert!(
            has("running", "run", "VBG"),
            "running: {:?}",
            by_word.get("running")
        );
        assert!(has("is", "be", "VBZ"), "is: {:?}", by_word.get("is"));
        assert!(
            has("better", "good", "JJR") || has("better", "well", "RBR"),
            "better: {:?}",
            by_word.get("better")
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
    fn reads_real_languagetool_german_dict() {
        // German exercises a different separator (`_`) + non-ASCII (umlaut) SUFFIX decoding + the STTS
        // tagset — proving the reader is language-agnostic, not English-specific.
        let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/de"));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("pos.dict")),
            std::fs::read_to_string(base.join("pos.info")),
        ) else {
            eprintln!(
                "skip: resources/de/pos.dict not present (cargo xtask build-tagger --lang de)"
            );
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(meta.separator, b'_', "German uses an underscore separator");
        assert_eq!(meta.encoder, Encoder::Suffix);

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("german.dict: {} triples", triples.len());
        assert!(
            triples.len() > 1_000_000,
            "German morphology is rich; got {}",
            triples.len()
        );

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word
                .entry(inflected.clone())
                .or_default()
                .push((lemma.clone(), tag.clone()));
        }
        let has = |w: &str, lemma: &str, tag: &str| {
            by_word
                .get(w)
                .is_some_and(|v| v.iter().any(|(l, t)| l == lemma && t == tag))
        };
        // Closed-class + an umlaut plural (Häuser→Haus) — the umlaut proves multibyte SUFFIX trimming.
        assert!(
            has("der", "der", "ART:DEF:DAT:SIN:FEM"),
            "der: {:?}",
            by_word.get("der")
        );
        assert!(
            has("ist", "sein", "VER:3:SIN:PRÄ:NON"),
            "ist: {:?}",
            by_word.get("ist")
        );
        assert!(
            has("Häuser", "Haus", "SUB:NOM:PLU:NEU"),
            "Häuser: {:?}",
            by_word.get("Häuser")
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
    fn reads_real_languagetool_russian_dict() {
        // Russian's russian.dict ships in the LT repo and is KOI8-R encoded (not UTF-8) — this proves
        // the `fsa.dict.encoding` path decodes Cyrillic correctly (every triple would be dropped if we
        // treated the bytes as UTF-8), and that SUFFIX trimming works in that single-byte encoding.
        let base = repo_path(rlt_lang::lt_resource_path!("ru"));
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
        assert!(
            triples.len() > 1_000_000,
            "Russian morphology is rich; got {}",
            triples.len()
        );

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
            by_word
                .entry(inflected.clone())
                .or_default()
                .push((lemma.clone(), tag.clone()));
        }
        // Lemma + tag-prefix spot-checks (exact morphology strings aren't pinned): «книги» (a non-Nom
        // form of «книга», book) is a SUFFIX diff that must reconstruct the base; «читаю» → «читать».
        let lemma_with_tag = |w: &str, lemma: &str, tag_prefix: &str| {
            by_word.get(w).is_some_and(|v| {
                v.iter()
                    .any(|(l, t)| l == lemma && t.starts_with(tag_prefix))
            })
        };
        assert!(
            lemma_with_tag("книги", "книга", "NN"),
            "книги→книга/NN: {:?}",
            by_word.get("книги")
        );
        assert!(
            lemma_with_tag("читаю", "читать", "VB"),
            "читаю→читать/VB: {:?}",
            by_word.get("читаю")
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
    fn reads_real_languagetool_arabic_dict() {
        // Arabic's arabic.dict ships in the LT repo, CFSA2 + UTF-8 (unlike Russian's KOI8-R). This
        // pins the format and proves Arabic-script SUFFIX decoding produces clean UTF-8 — and that the
        // dict keys are UNvocalized (no tashkeel), which is what drives `Normalization::StripCombiningMarks`.
        let base = repo_path(rlt_lang::lt_resource_path!("ar"));
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
        assert_eq!(
            meta.encoding, None,
            "arabic.info declares fsa.dict.encoding=utf-8"
        );

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("arabic.dict: {} triples", triples.len());
        assert!(
            triples.len() > 1_000_000,
            "Arabic morphology is rich; got {}",
            triples.len()
        );

        // Decoded forms are clean UTF-8, and the dict keys carry no tashkeel (Arabic combining marks
        // U+064B–065F / U+0670) — i.e. the dict is unvocalized, which is what makes the engine's
        // `StripCombiningMarks` necessary for vocalized input.
        let tashkeel = |s: &str| {
            s.chars()
                .any(|c| ('\u{064B}'..='\u{065F}').contains(&c) || c == '\u{0670}')
        };
        assert!(
            triples
                .iter()
                .take(20_000)
                .all(|(i, l, _)| !i.contains('\u{fffd}')
                    && !l.contains('\u{fffd}')
                    && !tashkeel(i)),
            "decoded keys must be clean, unvocalized UTF-8",
        );

        // كتاب (kitāb, "book") is a noun — assert it's an inflected key with a noun (`N`) tag.
        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word
                .entry(inflected.clone())
                .or_default()
                .push((lemma.clone(), tag.clone()));
        }
        assert!(
            by_word
                .get("كتاب")
                .is_some_and(|v| v.iter().any(|(_, t)| t.starts_with('N'))),
            "كتاب should be a known noun: {:?}",
            by_word.get("كتاب"),
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
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
            eprintln!(
                "skip: resources/fr/pos.dict not present (run `cargo xtask build-tagger --lang fr`)"
            );
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        assert_eq!(
            meta.separator, b'_',
            "french.info declares fsa.dict.separator=_"
        );
        assert_eq!(meta.encoder, Encoder::Suffix);
        assert_eq!(
            meta.encoding, None,
            "french.info declares fsa.dict.encoding=utf-8"
        );

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("french pos.dict: {} triples", triples.len());
        assert!(
            triples.len() > 600_000,
            "French morphology is rich; got {}",
            triples.len()
        );

        let combining = |s: &str| s.chars().any(|c| ('\u{0300}'..='\u{036F}').contains(&c));
        assert!(
            triples
                .iter()
                .take(20_000)
                .all(|(i, l, _)| !i.contains('\u{fffd}')
                    && !l.contains('\u{fffd}')
                    && !combining(i)),
            "decoded keys must be clean, precomposed (no combining marks) UTF-8",
        );

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word
                .entry(inflected.clone())
                .or_default()
                .push((lemma.clone(), tag.clone()));
        }
        // «chats» (cats) → lemma «chat», masculine-plural noun. The tag is exactly `N m p` — the
        // trailing `frequency-included` byte (`N m pK` in the raw dict) is stripped by the reader.
        assert!(
            by_word
                .get("chats")
                .is_some_and(|v| v.iter().any(|(l, t)| l == "chat" && t == "N m p")),
            "chats→chat/N m p (freq byte stripped): {:?}",
            by_word.get("chats"),
        );
        // An accented common noun «canapé» (couch) must read back cleanly as a masculine noun.
        assert!(
            by_word
                .get("canapé")
                .is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("N m"))),
            "canapé should be a known masculine noun: {:?}",
            by_word.get("canapé"),
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
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
        assert_eq!(
            meta.separator, b'_',
            "es-ES.info declares fsa.dict.separator=_"
        );
        assert_eq!(meta.encoder, Encoder::Suffix);
        assert_eq!(
            meta.encoding, None,
            "es-ES.info declares fsa.dict.encoding=utf-8"
        );

        let triples = read_triples(&dict, &meta).expect("read dict");
        eprintln!("es pos.dict: {} triples", triples.len());
        assert!(
            triples.len() > 1_000_000,
            "Spanish morphology is rich; got {}",
            triples.len()
        );

        let combining = |s: &str| s.chars().any(|c| ('\u{0300}'..='\u{036F}').contains(&c));
        assert!(
            triples
                .iter()
                .take(50_000)
                .all(|(i, l, _)| !i.contains('\u{fffd}')
                    && !l.contains('\u{fffd}')
                    && !combining(i)),
            "decoded keys must be clean, precomposed UTF-8",
        );

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word
                .entry(inflected.clone())
                .or_default()
                .push((lemma.clone(), tag.clone()));
        }
        // «casa» (house) is a common noun → EAGLES `NC`; «sofá» (accented) must be a first-class key.
        assert!(
            by_word
                .get("casa")
                .is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("NC"))),
            "casa should be a known common noun: {:?}",
            by_word.get("casa"),
        );
        assert!(
            by_word
                .get("sofá")
                .is_some_and(|v| v.iter().any(|(l, t)| l == "sofá" && t.starts_with('N'))),
            "accented sofá should be a known noun key: {:?}",
            by_word.get("sofá"),
        );
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
    fn reads_real_languagetool_italian_dict() {
        // Italian's italian.dict ships in the LT repo in the **FSA5** format (version 0x05, not CFSA2)
        // and is ISO-8859-15 encoded — this exercises both the FSA5 sibling reader and the `encoding`
        // seam at once. The dict is unvocalized Latin (no combining marks → `Normalization::None`).
        let base = repo_path(rlt_lang::lt_resource_path!("it"));
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
        assert!(
            triples.len() > 400_000,
            "Italian morphology is rich; got {}",
            triples.len()
        );

        let mut by_word: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (inflected, lemma, tag) in &triples {
            by_word
                .entry(inflected.clone())
                .or_default()
                .push((lemma.clone(), tag.clone()));
        }
        // «gatto» (cat) → masculine noun; «città» (accented, ISO-8859-15 à) must be a clean key —
        // proving the FSA5 traversal + ISO-8859-15 decode reconstruct real Italian forms.
        assert!(
            by_word
                .get("gatto")
                .is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("NOUN-M"))),
            "gatto should be a masculine noun: {:?}",
            by_word.get("gatto"),
        );
        assert!(
            by_word
                .get("città")
                .is_some_and(|v| v.iter().any(|(_, t)| t.starts_with("NOUN"))),
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
            &DictMeta {
                separator: b'+',
                encoder: Encoder::Suffix,
                encoding: None,
                frequency_included: false,
            },
        )
        .expect_err("unknown FSA version must be rejected");
        assert!(
            err.to_string().contains("unsupported FSA version"),
            "actionable error: {err}"
        );
    }

    /// A minimal FSA5 (goto length 2, no node data, no suffix sharing) accepting exactly `entries`.
    fn tiny_fsa5(entries: &[&[u8]]) -> Vec<u8> {
        #[derive(Default)]
        struct Node {
            kids: BTreeMap<u8, (bool, usize)>,
        }
        let mut nodes = vec![Node::default()];
        for entry in entries {
            let mut at = 0;
            for (i, &byte) in entry.iter().enumerate() {
                let last = i + 1 == entry.len();
                let known = nodes[at].kids.get(&byte).copied();
                let next = match known {
                    Some((_, next)) if next != 0 => next,
                    _ if last => 0,
                    _ => {
                        nodes.push(Node::default());
                        nodes.len() - 1
                    }
                };
                let was_final = known.is_some_and(|(f, _)| f);
                nodes[at].kids.insert(byte, (was_final || last, next));
                at = next;
            }
        }
        // Arcs region: dummy arc (2 bytes), epsilon arc (3 bytes), then the nodes in order.
        let mut offsets = Vec::new();
        let mut next_offset = 5;
        for node in &nodes {
            offsets.push(next_offset);
            next_offset += 3 * node.kids.len();
        }
        let goto = |target: usize, flags: u8| -> [u8; 2] {
            u16::try_from((target << 3) | usize::from(flags))
                .unwrap()
                .to_le_bytes()
        };
        let mut out = b"\\fsa\x05\x00\x00\x02".to_vec(); // header: filler, annotation, hgtl = gtl 2
        out.extend([0, 0b100]); // dummy arc whose flag byte says "next"
        out.push(0);
        out.extend(goto(offsets[0], 0b10)); // epsilon arc to the root, last in its node
        for node in &nodes {
            let count = node.kids.len();
            for (i, (&label, &(is_final, next))) in node.kids.iter().enumerate() {
                let flags = u8::from(is_final) | if i + 1 == count { 0b10 } else { 0 };
                out.push(label);
                out.extend(goto(if next == 0 { 0 } else { offsets[next] }, flags));
            }
        }
        out
    }

    #[test]
    fn lookup_follows_the_key_then_the_separator_through_a_small_automaton() {
        let meta = DictMeta {
            separator: b'+',
            encoder: Encoder::None,
            encoding: None,
            frequency_included: false,
        };
        let dict = Dictionary::new(
            tiny_fsa5(&[b"ab+x+T1", b"ab+yy+T2", b"ab+", b"abc+z", b"b+q"]),
            meta,
        )
        .expect("valid FSA5");
        let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
        // Every continuation of `ab+` comes back, including the entry that ends at the separator.
        assert_eq!(
            dict.lookup("ab"),
            [pair("", ""), pair("x", "T1"), pair("yy", "T2")]
        );
        assert_eq!(dict.lookup("abc"), [pair("z", "")]);
        assert_eq!(dict.lookup("b"), [pair("q", "")]);
        // A key that is only a prefix of another, or not there at all, finds nothing.
        assert_eq!(dict.lookup("a"), []);
        assert_eq!(dict.lookup("abd"), []);
        assert_eq!(dict.lookup(""), []);
    }

    #[test]
    fn dictionary_rejects_bad_header() {
        let meta = DictMeta {
            separator: b'+',
            encoder: Encoder::Suffix,
            encoding: None,
            frequency_included: false,
        };
        assert!(Dictionary::new(b"\\fsa\x07\x00\x00\x00rest".to_vec(), meta.clone()).is_err());
        assert!(Dictionary::new(b"nope".to_vec(), meta).is_err());
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
    fn lookup_agrees_with_enumeration_on_the_german_pos_dict() {
        // `lookup` must return exactly what `read_triples` enumerates for the same inflected form,
        // for words that exercise umlauts, several readings and an absent key.
        let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/de"));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("pos.dict")),
            std::fs::read_to_string(base.join("pos.info")),
        ) else {
            eprintln!(
                "skip: resources/de/pos.dict not present (cargo xtask build-tagger --lang de)"
            );
            return;
        };
        let meta = parse_info(&info).expect("parse .info");
        let triples = read_triples(&dict, &meta).expect("read dict");
        let lookup = Dictionary::new(dict, meta).expect("valid dict");
        for word in ["Häuser", "ist", "der", "schnellen", "Xyzzyq"] {
            let mut expected: Vec<(String, String)> = triples
                .iter()
                .filter(|(inflected, ..)| inflected == word)
                .map(|(_, lemma, tag)| (lemma.clone(), tag.clone()))
                .collect();
            let mut got = lookup.lookup(word);
            expected.sort();
            got.sort();
            assert_eq!(got, expected, "lookup({word:?})");
        }
        assert!(
            lookup
                .lookup("Häuser")
                .contains(&("Haus".to_owned(), "SUB:NOM:PLU:NEU".to_owned()))
        );
        assert_eq!(lookup.lookup("Xyzzyq"), []);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "reads fetched LT data from disk; miri runs under fs isolation"
    )]
    fn lookup_reads_the_german_synthesis_dictionary() {
        // A synthesis dictionary is keyed `lemma|tag`; the decoded base form is the inflected word.
        // `schnell|ADJ:AKK:PLU:FEM:GRU:DEF` → `schnellen` is read off the automaton's SUFFIX
        // encoding (`Yen`: trim 24 bytes of the key, append `en`).
        let base = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/de"));
        let (Ok(dict), Ok(info)) = (
            std::fs::read(base.join("synth.dict")),
            std::fs::read_to_string(base.join("synth.info")),
        ) else {
            eprintln!(
                "skip: resources/de/synth.dict not present (cargo xtask build-synth --lang de)"
            );
            return;
        };
        let dict =
            Dictionary::new(dict, parse_info(&info).expect("parse .info")).expect("valid dict");
        let forms = |key: &str| -> Vec<String> {
            dict.lookup(key).into_iter().map(|(form, _)| form).collect()
        };
        assert_eq!(forms("schnell|ADJ:AKK:PLU:FEM:GRU:DEF"), ["schnellen"]);
        assert_eq!(forms("schnell|ADJ:AKK:PLU:FEM:KOM:DEF"), ["schnelleren"]);
        assert_eq!(forms("Haus|SUB:NOM:PLU:NEU"), ["Häuser"]);
        assert_eq!(forms("schnell|NOPE"), [] as [String; 0]);
        assert_eq!(
            forms("schnell"),
            [] as [String; 0],
            "a prefix of a key is not a key"
        );
    }
}
