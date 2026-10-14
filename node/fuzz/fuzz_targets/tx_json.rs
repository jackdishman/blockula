#![no_main]
//! See `blockula_fuzz::harness::tx_json`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::tx_json(data);
});
