#![no_main]

use libfuzzer_sys::fuzz_target;

// The L3 confusion model (`resources/confusion.rkyv`) is loaded at runtime via its compact
// columnar/varint codec (`rlt_ir::deserialize_confusion`). Malformed bytes must fail gracefully
// through the bounds-checked reader rather than crash the checker.
fuzz_target!(|data: &[u8]| {
    let _ = rlt_core::ConfusionChecker::from_rkyv_bytes(data);
});
