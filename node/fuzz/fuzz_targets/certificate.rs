#![no_main]
//! See `blockula_fuzz::harness::certificate`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::certificate(data);
});
