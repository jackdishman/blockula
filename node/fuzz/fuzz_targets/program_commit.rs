#![no_main]
//! See `blockula_fuzz::harness::program_commit`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::program_commit(data);
});
