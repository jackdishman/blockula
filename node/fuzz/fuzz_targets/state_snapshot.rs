#![no_main]
//! See `blockula_fuzz::harness::state_snapshot`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::state_snapshot(data);
});
