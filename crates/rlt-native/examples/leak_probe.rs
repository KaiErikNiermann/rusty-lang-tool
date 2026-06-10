//! Leak probe for the native engine — load it, hammer `analyze` + `is_known`, then drop everything
//! and exit so any retained allocation is a genuine leak.
//!
//! Run under LeakSanitizer (nightly):
//! ```sh
//! RUSTFLAGS="-Zsanitizer=leak" cargo run --release \
//!   --target x86_64-unknown-linux-gnu -p rlt-native --example leak_probe
//! ```
//! LSan reports any "definitely lost" blocks at exit. Needs the artifacts (`cargo xtask build-tagger`
//! + `build-disambig`; `segment.srx` via `fetch-lt`).

use std::path::Path;

use rlt_core::Engine;
use rlt_native::NativeEngine;

const CORPUS: &str = "The committee reviewed the proposal carefully before the meeting. \
Several members raised concerns about the budget, which had grown considerably. \
In 2023, the organization reported record revenues of 4.2 million dollars.";
const WORDS: &[&str] = &["the", "running", "London", "recieve", "gives", "zxqwv"];
const ITERS: usize = 3000;

fn main() {
    let disambig = Path::new("resources/en/disambig.rkyv");
    let engine = NativeEngine::from_paths(
        &rlt_lang::EN,
        Path::new("resources/segment.srx"),
        Path::new("resources/en/tagger.rkyv"),
        disambig.exists().then_some(disambig),
    )
    .expect("load native engine — build the artifacts first");

    let mut acc = 0usize;
    for _ in 0..ITERS {
        acc += engine.analyze(CORPUS).tokens.len();
        acc += WORDS.iter().filter(|w| engine.is_known(w)).count();
    }
    // Drop the engine explicitly so its artifacts are freed before the leak check at exit.
    drop(engine);
    println!("leak_probe: {ITERS} iterations, {acc} accumulated (engine dropped)");
}
