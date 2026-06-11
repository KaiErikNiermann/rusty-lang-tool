#![no_main]

use libfuzzer_sys::fuzz_target;

// The L1 spelling `edits1` (delete/transpose/replace/insert) over arbitrary input + the Russian
// Cyrillic alphabet. This is the panic surface the char-based rewrite fixed: the previous byte-level
// edits did `String::from_utf8(..).expect("ascii")`, which panics on multibyte input. The char-based
// version must produce only valid UTF-8 candidates and never panic over any input. `fuzz_edits1`
// returns the candidate count, forcing full evaluation.
const RU_ALPHABET: &str = "абвгдеёжзийклмнопрстуфхцчшщъыьэюя";

fuzz_target!(|word: String| {
    // Cap length so the O(n·|alphabet|) candidate set stays bounded per run.
    if word.chars().count() > 64 {
        return;
    }
    let _ = rlt_core::fuzz_edits1(&word, RU_ALPHABET);
    let _ = rlt_core::fuzz_edits1(&word, "abcdefghijklmnopqrstuvwxyz");
    // Arabic base letters — RTL + combining marks in the input exercise the mark-stripping path.
    let _ = rlt_core::fuzz_edits1(&word, "ءآأؤإئابةتثجحخدذرزسشصضطظعغـفقكلمنهوىي");
});
