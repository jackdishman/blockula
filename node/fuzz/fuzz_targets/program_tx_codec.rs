#![no_main]
//! See `blockula_fuzz::harness::program_tx_codec`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::program_tx_codec(data);
});
