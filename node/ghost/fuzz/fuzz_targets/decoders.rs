//! Fuzzes every decoder of untrusted bytes: addresses, viewing keys, the frontier, digests and
//! trial decryption. None may panic; whatever decodes must re-encode to the same bytes.

#![no_main]

use blockula_ghost::field::{digest_from_bytes, digest_to_bytes, xof_to_digest};
use blockula_ghost::*;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(a) = Address::from_bytes(data) {
        assert_eq!(a.to_bytes().as_slice(), data);
    }
    if let Some(k) = FullViewingKey::from_bytes(data) {
        assert_eq!(k.to_bytes().as_slice(), data);
    }
    if let Some(k) = IncomingViewingKey::from_bytes(data) {
        assert_eq!(k.to_bytes().as_slice(), data);
    }
    if let Some(f) = Frontier::from_bytes(data) {
        assert_eq!(f.to_bytes(), data);
        let _ = f.root();
    }
    if let Ok(bytes) = <[u8; 32]>::try_from(data) {
        if let Some(d) = digest_from_bytes(&bytes) {
            assert_eq!(digest_to_bytes(&d), bytes);
        }
    }
    let ivk = SpendingKey::from_seed([1; 32]).full_viewing_key().incoming();
    let cm = xof_to_digest("fuzz", b"cm");
    assert!(try_decrypt_incoming(&ivk, &cm, data).is_none());
});
