#![no_main]

use libfuzzer_sys::fuzz_target;

// The native engine loads its POS tagger artifact (`tagger.rkyv`) at startup — in a web deployment
// from a URL, i.e. a potentially untrusted source. Malformed bytes must be rejected by rkyv's
// validation inside `from_bytes` (and the embedded fst's own validation), never panic or read out of
// bounds.
fuzz_target!(|data: &[u8]| {
    let _ = rlt_native::Tagger::from_bytes(data);
});
