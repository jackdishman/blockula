#![no_main]
//! See `blockula_fuzz::harness::snapshot_manifest`.

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    blockula_fuzz::harness::snapshot_manifest(data);
});
