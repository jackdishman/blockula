#![no_main]
//! See `blockula_fuzz::harness::sync_batch`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::sync_batch(data);
});
