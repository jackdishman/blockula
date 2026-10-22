#![no_main]
//! See `blockula_vm_fuzz::program_validate`: `validate()` never panics, and nothing it accepts
//! panics an engine's translator.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_vm_fuzz::program_validate(data);
});
