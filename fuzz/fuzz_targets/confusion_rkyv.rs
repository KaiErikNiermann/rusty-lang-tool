#![no_main]

use libfuzzer_sys::fuzz_target;

// The L3 confusion model (`resources/confusion.rkyv`) is loaded at runtime the same way as the rule
// blob. Malformed bytes must fail gracefully through rkyv validation rather than crash the checker.
fuzz_target!(|data: &[u8]| {
    let _ = rlt_core::ConfusionChecker::from_rkyv_bytes(data);
});
