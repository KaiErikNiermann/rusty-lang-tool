//! Leak probe for the native engine — load it, hammer `analyze` + `is_known`, then drop everything
//! and exit so any retained allocation is a genuine leak.
//!
//! Run under LeakSanitizer (nightly); pass a language code (default `en`):
//! ```sh
//! RUSTFLAGS="-Zsanitizer=leak" cargo run --release \
//!   --target x86_64-unknown-linux-gnu -p rlt-native --example leak_probe -- de
//! ```
//! LSan reports any "definitely lost" blocks at exit. Needs the artifacts (`cargo xtask build-lang
//! --lang <code>`; `segment.srx` via `fetch-lt`).

use std::path::{Path, PathBuf};

use rlt_core::Engine;
use rlt_native::NativeEngine;

const EN_CORPUS: &str = "The committee reviewed the proposal carefully before the meeting. \
Several members raised concerns about the budget, which had grown considerably. \
In 2023, the organization reported record revenues of 4.2 million dollars.";
const EN_WORDS: &[&str] = &["the", "running", "London", "recieve", "gives", "zxqwv"];
const DE_CORPUS: &str = "Der Ausschuss prüfte den Vorschlag sorgfältig vor der Sitzung. \
Mehrere Mitglieder äußerten Bedenken über das Budget, das beträchtlich gewachsen war. \
Die Haustür war offen und das Arbeitszimmer ist groß.";
const DE_WORDS: &[&str] = &[
    "der",
    "Häuser",
    "Haustür",
    "schön",
    "xqzzy",
    "Arbeitszimmer",
];
const ITERS: usize = 3000;

fn main() {
    let lang = std::env::args().nth(1).unwrap_or_else(|| "en".to_owned());
    let cfg = rlt_lang::config(&lang).expect("known language code (en, de)");
    let disambig = PathBuf::from(cfg.disambig_path());
    let engine = NativeEngine::from_paths(
        cfg,
        Path::new(cfg.segment_srx_path()),
        &PathBuf::from(cfg.tagger_path()),
        disambig.exists().then_some(disambig.as_path()),
    )
    .expect("load native engine — build the artifacts first");
    let (corpus, words) = if lang == "de" {
        (DE_CORPUS, DE_WORDS)
    } else {
        (EN_CORPUS, EN_WORDS)
    };

    let mut acc = 0usize;
    for _ in 0..ITERS {
        acc += engine.analyze(corpus).tokens.len();
        acc += words.iter().filter(|w| engine.is_known(w)).count();
    }
    // Drop the engine explicitly so its artifacts are freed before the leak check at exit.
    drop(engine);
    println!("leak_probe: {ITERS} iterations, {acc} accumulated (engine dropped)");
}
