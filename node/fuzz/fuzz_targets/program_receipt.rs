#![no_main]
//! See `blockula_fuzz::harness::program_receipt`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::program_receipt(data);
});
