//! Known-answer and differential tests for the ML-KEM-768 implementation the handshake uses
//! (RustCrypto `ml-kem`, pinned `=0.3.2`, never independently audited).
//!
//! - NIST ACVP (FIPS 203): keyGen, encapsulation, decapsulation, and both key checks.
//! - Wycheproof: decapsulation from seeds (including implicit rejection and malleable
//!   ciphertexts) and encapsulation-key validation (including the modulus check).
//! - aws-lc-rs (FIPS 140-3 validated ML-KEM) as an independent oracle, in both directions.
//!
//! Vectors live in `testdata/ml-kem-768.json` (built by `testdata/trim.py`, sources pinned there).

#![allow(deprecated)] // ACVP and Wycheproof use the legacy expanded decapsulation key encoding.

use super::{KeyShare, ML_KEM_CT_SIZE, ML_KEM_EK_SIZE};
use commonware_codec::{DecodeExt, Error as CodecError};
use ml_kem::{
    B32, Decapsulate, DecapsulationKey768, Encapsulate, EncapsulationKey768, ExpandedKeyEncoding,
    KeyExport, TryKeyInit,
    array::{Array, ArraySize},
    ml_kem_768::Ciphertext,
};
use rand_core::Rng;
use serde_json::Value;
use std::sync::OnceLock;

fn vectors() -> &'static Value {
    static VECTORS: OnceLock<Value> = OnceLock::new();
    VECTORS.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/ml-kem-768.json");
        let raw = std::fs::read(path).expect("read ML-KEM vectors");
        serde_json::from_slice(&raw).expect("parse ML-KEM vectors")
    })
}

fn cases(path: &[&str]) -> &'static [Value] {
    let mut v = vectors();
    for key in path {
        v = &v[*key];
    }
    let cases = v
        .as_array()
        .unwrap_or_else(|| panic!("no vectors at {path:?}"));
    assert!(!cases.is_empty(), "no vectors at {path:?}");
    cases
}

fn bytes(case: &Value, field: &str) -> Vec<u8> {
    hex::decode(
        case[field]
            .as_str()
            .unwrap_or_else(|| panic!("missing {field}")),
    )
    .unwrap_or_else(|e| panic!("bad hex in {field}: {e}"))
}

fn array<U: ArraySize>(case: &Value, field: &str) -> Array<u8, U> {
    Array::try_from(&bytes(case, field)[..]).unwrap_or_else(|_| panic!("{field}: wrong length"))
}

fn id(case: &Value) -> u64 {
    case["tcId"].as_u64().expect("tcId")
}

fn seed(d: &[u8], z: &[u8]) -> ml_kem::Seed {
    let mut seed = ml_kem::Seed::default();
    seed[..32].copy_from_slice(d);
    seed[32..].copy_from_slice(z);
    seed
}

#[test]
fn acvp_key_gen() {
    for case in cases(&["acvp", "keyGen"]) {
        let dk = DecapsulationKey768::from_seed(seed(&bytes(case, "d"), &bytes(case, "z")));
        assert_eq!(
            dk.encapsulation_key().to_bytes()[..],
            bytes(case, "ek")[..],
            "tc {}",
            id(case)
        );
        assert_eq!(
            dk.to_expanded_bytes()[..],
            bytes(case, "dk")[..],
            "tc {}",
            id(case)
        );
    }
}

#[test]
fn acvp_encapsulation() {
    for case in cases(&["acvp", "encapsulation"]) {
        let ek = EncapsulationKey768::new_from_slice(&bytes(case, "ek")).expect("valid ek");
        let (c, k) = ek.encapsulate_deterministic(&array(case, "m"));
        assert_eq!(c[..], bytes(case, "c")[..], "tc {}", id(case));
        assert_eq!(k[..], bytes(case, "k")[..], "tc {}", id(case));
    }
}

#[test]
fn acvp_decapsulation() {
    for case in cases(&["acvp", "decapsulation"]) {
        let dk = DecapsulationKey768::from_expanded_bytes(&array(case, "dk")).expect("valid dk");
        let k = dk.decapsulate(&array(case, "c"));
        assert_eq!(k[..], bytes(case, "k")[..], "tc {}", id(case));
    }
}

#[test]
fn acvp_key_checks() {
    for case in cases(&["acvp", "encapsulationKeyCheck"]) {
        let ok = EncapsulationKey768::new_from_slice(&bytes(case, "ek")).is_ok();
        assert_eq!(ok, case["testPassed"].as_bool().unwrap(), "tc {}", id(case));
    }
    for case in cases(&["acvp", "decapsulationKeyCheck"]) {
        let dk = bytes(case, "dk");
        let ok = Array::try_from(&dk[..])
            .is_ok_and(|dk| DecapsulationKey768::from_expanded_bytes(&dk).is_ok());
        assert_eq!(ok, case["testPassed"].as_bool().unwrap(), "tc {}", id(case));
    }
}

#[test]
fn wycheproof_decapsulation() {
    for case in cases(&["wycheproof", "mlkem_768_test"]) {
        let valid = case["result"] == "valid";
        let seed = bytes(case, "seed");
        let Ok(seed) = ml_kem::Seed::try_from(&seed[..]) else {
            assert!(!valid, "tc {}: valid case with a bad seed", id(case));
            continue;
        };
        let dk = DecapsulationKey768::from_seed(seed);
        assert_eq!(
            dk.encapsulation_key().to_bytes()[..],
            bytes(case, "ek")[..],
            "tc {}",
            id(case)
        );
        match dk.decapsulate_slice(&bytes(case, "c")) {
            Ok(k) => {
                assert!(valid, "tc {}: accepted an invalid ciphertext", id(case));
                assert_eq!(k[..], bytes(case, "K")[..], "tc {}", id(case));
            }
            Err(_) => assert!(!valid, "tc {}: rejected a valid ciphertext", id(case)),
        }
    }
}

#[test]
fn wycheproof_encapsulation_key_validation() {
    let mut modulus_cases = 0;
    for case in cases(&["wycheproof", "mlkem_768_encaps_test"]) {
        let valid = case["result"] == "valid";
        let ek_bytes = bytes(case, "ek");
        match EncapsulationKey768::new_from_slice(&ek_bytes) {
            Ok(ek) => {
                assert!(valid, "tc {}: accepted an invalid key", id(case));
                let (c, k) = ek.encapsulate_deterministic(&array(case, "m"));
                assert_eq!(c[..], bytes(case, "c")[..], "tc {}", id(case));
                assert_eq!(k[..], bytes(case, "K")[..], "tc {}", id(case));
            }
            Err(_) => assert!(!valid, "tc {}: rejected a valid key", id(case)),
        }

        // The handshake's own decode path must agree on full-size keys.
        if ek_bytes.len() == ML_KEM_EK_SIZE {
            let mut share = ek_bytes.clone();
            share.extend_from_slice(&[9u8; 32]);
            let decoded = KeyShare::decode(&share[..]);
            assert_eq!(decoded.is_ok(), valid, "tc {}", id(case));
            if !valid {
                assert!(matches!(decoded, Err(CodecError::Invalid(..))));
                modulus_cases += 1;
            }
        }
    }
    assert!(
        modulus_cases >= 24,
        "too few modulus-overflow cases: {modulus_cases}"
    );
}

/// Sets the first 12-bit coefficient of an encoded encapsulation key.
fn set_first_coefficient(ek: &mut [u8], value: u16) {
    ek[0] = value as u8;
    ek[1] = (ek[1] & 0xf0) | ((value >> 8) as u8 & 0x0f);
}

#[test]
fn modulus_check_boundary() {
    let mut rng = commonware_utils::test_rng();
    let (_, ek) = random_keypair(&mut rng);
    let mut raw = ek.to_bytes().to_vec();
    set_first_coefficient(&mut raw, 3328); // q - 1: valid
    assert!(EncapsulationKey768::new_from_slice(&raw).is_ok());
    set_first_coefficient(&mut raw, 3329); // q: invalid
    assert!(EncapsulationKey768::new_from_slice(&raw).is_err());
}

#[test]
fn deterministic_encapsulation_matches_random_path() {
    // `encapsulate_with_rng` must be `encapsulate_deterministic` on 32 bytes from the RNG,
    // which is what the ACVP vectors pin.
    let mut a = commonware_utils::test_rng();
    let mut b = commonware_utils::test_rng();
    let (_, ek) = random_keypair(&mut a);
    let _ = random_keypair(&mut b);
    let (ct, ss) = ek.encapsulate_with_rng(&mut a);
    let mut m = B32::default();
    b.fill_bytes(&mut m);
    assert_eq!((ct, ss), ek.encapsulate_deterministic(&m));
}

fn random_keypair(rng: &mut impl Rng) -> (DecapsulationKey768, EncapsulationKey768) {
    let mut seed = ml_kem::Seed::default();
    rng.fill_bytes(&mut seed);
    let dk = DecapsulationKey768::from_seed(seed);
    let ek = dk.encapsulation_key().clone();
    (dk, ek)
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod aws_lc {
    use super::*;
    use aws_lc_rs::kem::{DecapsulationKey, EncapsulationKey, ML_KEM_768};

    const ROUNDS: usize = 32;

    #[test]
    fn rustcrypto_keys_aws_lc_encapsulates() {
        let mut rng = commonware_utils::test_rng();
        for _ in 0..ROUNDS {
            let (dk, ek) = random_keypair(&mut rng);
            let theirs = EncapsulationKey::new(&ML_KEM_768, &ek.to_bytes()).expect("aws-lc ek");
            let (ct, ss) = theirs.encapsulate().expect("aws-lc encapsulate");
            let ct: Ciphertext = Array::try_from(ct.as_ref()).expect("ciphertext size");
            assert_eq!(dk.decapsulate(&ct)[..], *ss.as_ref());

            // The same key in the expanded encoding decapsulates identically in aws-lc.
            let their_dk = DecapsulationKey::new(&ML_KEM_768, &dk.to_expanded_bytes()).unwrap();
            assert_eq!(
                their_dk.decapsulate(ct[..].into()).unwrap().as_ref(),
                ss.as_ref()
            );
        }
    }

    #[test]
    fn aws_lc_keys_rustcrypto_encapsulates() {
        let mut rng = commonware_utils::test_rng();
        for _ in 0..ROUNDS {
            let theirs = DecapsulationKey::generate(&ML_KEM_768).expect("aws-lc keygen");
            let ek_bytes = theirs.encapsulation_key().unwrap().key_bytes().unwrap();
            assert_eq!(ek_bytes.as_ref().len(), ML_KEM_EK_SIZE);
            let ek = EncapsulationKey768::new_from_slice(ek_bytes.as_ref()).expect("valid ek");
            let (ct, ss) = ek.encapsulate_with_rng(&mut rng);
            assert_eq!(ct.len(), ML_KEM_CT_SIZE);
            let theirs_ss = theirs
                .decapsulate(ct[..].into())
                .expect("aws-lc decapsulate");
            assert_eq!(theirs_ss.as_ref(), &ss[..]);
        }
    }

    #[test]
    fn implicit_rejection_agrees() {
        let mut rng = commonware_utils::test_rng();
        for _ in 0..ROUNDS {
            let (dk, ek) = random_keypair(&mut rng);
            let their_dk = DecapsulationKey::new(&ML_KEM_768, &dk.to_expanded_bytes()).unwrap();

            // A valid ciphertext with one bit flipped, and pure noise.
            let (mut ct, ss) = ek.encapsulate_with_rng(&mut rng);
            ct[rng.next_u32() as usize % ML_KEM_CT_SIZE] ^= 1 << (rng.next_u32() % 8);
            let mut noise = Ciphertext::default();
            rng.fill_bytes(&mut noise);
            for ct in [ct, noise] {
                let ours = dk.decapsulate(&ct);
                let theirs = their_dk.decapsulate(ct[..].into()).unwrap();
                assert_eq!(&ours[..], theirs.as_ref());
                assert_ne!(ours, ss);
            }
        }
    }

    #[test]
    fn modulus_check_agrees() {
        let mut rng = commonware_utils::test_rng();
        for value in [0u16, 3328, 3329, 4095] {
            let (_, ek) = random_keypair(&mut rng);
            let mut raw = ek.to_bytes().to_vec();
            set_first_coefficient(&mut raw, value);
            let ours = EncapsulationKey768::new_from_slice(&raw).is_ok();
            let theirs =
                EncapsulationKey::new(&ML_KEM_768, &raw).is_ok_and(|key| key.encapsulate().is_ok());
            assert_eq!(ours, value < 3329, "coefficient {value}");
            assert_eq!(ours, theirs, "coefficient {value}");
        }
    }
}
