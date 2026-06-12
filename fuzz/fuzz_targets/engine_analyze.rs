#![no_main]

use libfuzzer_sys::fuzz_target;

// One target fuzzes the native engine for EVERY configured language: the first input byte selects the
// language (`rlt_fuzz::codes()`, derived from `rlt_lang::LANGUAGES`), the rest is the text. Set
// `RLT_FUZZ_LANG=fr` to pin one. Adding a language is one entry in the canonical language list — not
// another `engine_analyze_<lang>.rs`. Asserts the engine's token spans stay valid (in-bounds,
// char-boundary, surface == source) over arbitrary input.
fuzz_target!(|input: (u8, String)| rlt_fuzz::fuzz_analyze(input.0, &input.1));
