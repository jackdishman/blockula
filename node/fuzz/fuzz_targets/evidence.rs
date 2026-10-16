#![no_main]
//! See `blockula_fuzz::harness::evidence`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::evidence(data);
});
