#![no_main]

use std::path::Path;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use rlt_core::Engine;
use rlt_native::NativeEngine;

// The Russian engine over arbitrary Unicode — the first far-from-Latin script. Cyrillic is 2-byte
// UTF-8, so this stresses the tokenizer's byte-offset arithmetic and the KOI8-R-decoded tagger over
// multibyte input. Asserts every token span is in-bounds, on a char boundary, and equals its source
// text. Loads the real Russian artifacts once (skipped when absent).
fn engine() -> Option<&'static NativeEngine> {
    const RES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../resources");
    static ENGINE: OnceLock<Option<NativeEngine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let disambig = Path::new(RES).join("ru/disambig.rkyv");
            NativeEngine::from_paths(
                &rlt_lang::RU,
                &Path::new(RES).join("segment.srx"),
                &Path::new(RES).join("ru/tagger.rkyv"),
                disambig.exists().then_some(disambig.as_path()),
            )
            .ok()
        })
        .as_ref()
}

fuzz_target!(|text: String| {
    let Some(engine) = engine() else { return };
    for token in engine.analyze(&text).tokens {
        let (start, end) = (token.span.start, token.span.end);
        assert!(start <= end && end <= text.len(), "span {start}..{end} OOB (len {})", text.len());
        assert!(
            text.is_char_boundary(start) && text.is_char_boundary(end),
            "span {start}..{end} not on a char boundary",
        );
        assert_eq!(token.text, text[start..end], "token surface must equal its source span");
    }
});
