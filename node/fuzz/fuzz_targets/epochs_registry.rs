#![no_main]
//! See `blockula_fuzz::harness::epochs_registry`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::epochs_registry(data);
});
