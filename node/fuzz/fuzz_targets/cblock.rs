#![no_main]
//! See `blockula_fuzz::harness::cblock`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::cblock(data);
});
