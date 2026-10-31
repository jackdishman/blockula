#![no_main]
//! See `blockula_fuzz::harness::state_machine_programs`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::state_machine_programs(data);
});
