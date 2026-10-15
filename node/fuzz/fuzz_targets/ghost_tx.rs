#![no_main]
//! See `blockula_fuzz::harness::ghost_tx`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::ghost_tx(data);
});
