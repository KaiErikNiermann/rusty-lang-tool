#![no_main]

use libfuzzer_sys::fuzz_target;

// The native engine loads its disambiguation artifact (`disambig.rkyv`) from a potentially untrusted
// source (a URL in the browser). `from_rkyv_bytes` runs rkyv's validated deserialize and then compiles
// the rules (regex building), so this fuzzes both rejection of malformed archives and rule compilation
// over hostile-but-structurally-valid ones.
fuzz_target!(|data: &[u8]| {
    let _ = rlt_core::Disambiguator::from_rkyv_bytes(data);
});
