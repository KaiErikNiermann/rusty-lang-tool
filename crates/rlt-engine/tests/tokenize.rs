//! Integration test for the nlprule-backed engine. Requires the tokenizer binary, which is
//! fetched on demand and not committed; the test skips (rather than fails) when it is absent so
//! `cargo test` stays green on a fresh checkout. Run `cargo xtask fetch-engine` to exercise it.

use std::path::Path;

use rlt_core::Engine;
use rlt_engine::VendoredEngine;

const TOKENIZER_BIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/en_tokenizer.bin"
);

#[test]
fn tokenizes_and_pos_tags() {
    if !Path::new(TOKENIZER_BIN).exists() {
        eprintln!("skipping: {TOKENIZER_BIN} absent (run `cargo xtask fetch-engine`)");
        return;
    }

    let engine = VendoredEngine::from_path(Path::new(TOKENIZER_BIN)).expect("load tokenizer");
    let analysis = engine.analyze("I should of went their yesterday.");

    // Spans cover the right surface forms.
    let went = analysis
        .tokens
        .iter()
        .find(|t| t.text == "went")
        .expect("`went` token present");
    assert!(
        went.tags.iter().any(|t| t == "VBD"),
        "`went` should be tagged past-tense verb (VBD); got {:?}",
        went.tags,
    );

    let their = analysis
        .tokens
        .iter()
        .find(|t| t.text == "their")
        .expect("`their` token present");
    assert!(
        their.tags.iter().any(|t| t == "PRP$"),
        "`their` should be tagged possessive pronoun (PRP$); got {:?}",
        their.tags,
    );
}
