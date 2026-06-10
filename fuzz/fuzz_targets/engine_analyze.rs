#![no_main]

use std::path::Path;
use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use rlt_core::Engine;
use rlt_native::NativeEngine;

// The richest native-engine target: run `analyze` over arbitrary Unicode and assert the span
// arithmetic holds. Segmentation (srx), the word tokenizer, FST tagging, structural tagging
// (CD/PCT/NNP/SENT_START/SENT_END) and disambiguation all manipulate byte offsets; a single off-by-one
// would surface here as an out-of-bounds or non-char-boundary span. The engine is loaded once from the
// real artifacts (skipped when absent, e.g. a fresh checkout).
fn engine() -> Option<&'static NativeEngine> {
    // Anchor to the workspace `resources/` via the fuzz crate's manifest dir, so the artifacts resolve
    // regardless of the fuzzer's working directory.
    const RES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../resources");
    static ENGINE: OnceLock<Option<NativeEngine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let disambig = Path::new(RES).join("en/disambig.rkyv");
            NativeEngine::from_paths(
                &rlt_lang::EN,
                &Path::new(RES).join("segment.srx"),
                &Path::new(RES).join("en/tagger.rkyv"),
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
        // Tagging + disambiguation never alter a token's surface text.
        assert_eq!(token.text, text[start..end], "token surface must equal its source span");
    }
});
