#![no_main]
//! See `blockula_fuzz::harness::state_machine`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::state_machine(data);
});
