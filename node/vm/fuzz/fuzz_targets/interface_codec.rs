#![no_main]
//! See `blockula_vm_fuzz::interface_codec`: `interface::decode` never panics, what it accepts is
//! canonical and keeps every rule, and what `encode` writes decodes back to the same interface.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_vm_fuzz::interface_codec(data);
});
