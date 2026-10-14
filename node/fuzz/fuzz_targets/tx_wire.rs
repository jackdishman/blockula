#![no_main]
//! See `blockula_fuzz::harness::tx_wire`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::tx_wire(data);
});
