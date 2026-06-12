//! Shared helpers for the fuzz targets.
//!
//! The per-language `engine_analyze` invariant check lives here so a new language is one entry in
//! [`CODES`] rather than another near-identical `engine_analyze_<lang>.rs` (they only ever differed by
//! the `LangConfig` and the `resources/<code>/` subdir).

use std::path::Path;
use std::sync::OnceLock;

use rlt_core::Engine;
use rlt_native::NativeEngine;

/// The languages the `engine_analyze` target rotates through (each fuzz input selects one). Add a new
/// language here — no new fuzz file. A language whose artifacts aren't built is skipped at runtime.
pub const CODES: &[&str] = &["en", "de", "ru", "ar", "fr", "es", "it"];

/// Load a language's native engine from its built `resources/<code>/` artifacts, or `None` if absent.
fn load(code: &str) -> Option<NativeEngine> {
    // Anchor to the workspace `resources/` via the fuzz crate's manifest dir, independent of cwd.
    const RES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../resources");
    let cfg = rlt_lang::config(code)?;
    let disambig = Path::new(RES).join(format!("{code}/disambig.rkyv"));
    NativeEngine::from_paths(
        cfg,
        &Path::new(RES).join("segment.srx"),
        &Path::new(RES).join(format!("{code}/tagger.rkyv")),
        disambig.exists().then_some(disambig.as_path()),
    )
    .ok()
}

/// The engine for `CODES[idx]`, loaded once and cached (`None` when its artifacts aren't built).
fn engine(idx: usize) -> Option<&'static NativeEngine> {
    static ENGINES: [OnceLock<Option<NativeEngine>>; CODES.len()] =
        [const { OnceLock::new() }; CODES.len()];
    ENGINES[idx].get_or_init(|| load(CODES[idx])).as_ref()
}

/// Run `analyze` over `text` for the language picked by `selector` — or the one pinned by the
/// `RLT_FUZZ_LANG` env var, if set — and assert every token span is in-bounds, on a char boundary, and
/// equal to its source slice. Segmentation, tokenization (incl. elision), FST + structural tagging,
/// normalization and disambiguation all do byte-offset arithmetic over arbitrary Unicode; an
/// off-by-one surfaces here as a panic (a fuzz crash). A no-op when the chosen language is unbuilt.
pub fn fuzz_analyze(selector: u8, text: &str) {
    static PINNED: OnceLock<Option<usize>> = OnceLock::new();
    let pinned = PINNED.get_or_init(|| {
        std::env::var("RLT_FUZZ_LANG").ok().and_then(|c| CODES.iter().position(|x| *x == c))
    });
    let idx = pinned.unwrap_or((selector as usize) % CODES.len());
    let Some(engine) = engine(idx) else { return };
    for token in engine.analyze(text).tokens {
        let (start, end) = (token.span.start, token.span.end);
        assert!(start <= end && end <= text.len(), "span {start}..{end} OOB (len {})", text.len());
        assert!(
            text.is_char_boundary(start) && text.is_char_boundary(end),
            "span {start}..{end} not on a char boundary",
        );
        // Tagging / normalization / disambiguation never alter a token's surface text.
        assert_eq!(token.text, text[start..end], "token surface must equal its source span");
    }
}
