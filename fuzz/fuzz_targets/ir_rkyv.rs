#![no_main]

use libfuzzer_sys::fuzz_target;

// The runtime loads the converter's rkyv rule blob (`resources/en.rkyv`) at startup — in a web
// deployment from a URL, i.e. a potentially untrusted source. Malformed bytes must be rejected by
// rkyv's validation (`from_bytes`), never panic or read out of bounds. `from_rkyv_bytes` runs that
// validated deserialize and then compiles the rules, so this also fuzzes rule compilation
// (regex building, suggestion pre-compilation) on hostile-but-structurally-valid archives.
fuzz_target!(|data: &[u8]| {
    let _ = rlt_core::IrMatcher::from_rkyv_bytes(data);
});
