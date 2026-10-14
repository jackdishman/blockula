//! Proof robustness: tampered, truncated, extended, non-canonical and random proofs are
//! rejected with an error, never a panic, and a proof only verifies its own statement.

use std::sync::OnceLock;

use blockula_ghost::circuit::{VerifyError, decode_proof, prove_with_seed};
use blockula_ghost::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// One honest action and proof, shared by the tests.
fn honest() -> &'static (PublicInputs, Vec<u8>) {
    static P: OnceLock<(PublicInputs, Vec<u8>)> = OnceLock::new();
    P.get_or_init(|| make(1))
}

fn make(seed: u64) -> (PublicInputs, Vec<u8>) {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut bytes = || {
        let mut b = [0u8; 32];
        rng.fill_bytes(&mut b);
        b
    };
    let key = SpendingKey::from_seed(bytes());
    let owner = key.full_viewing_key().owner();
    let a = Note { value: 600, owner, rseed: bytes() };
    let b = Note { value: 400, owner, rseed: bytes() };
    let mut tree = FullTree::new();
    let pa = tree.append(a.commitment()).unwrap();
    let pb = tree.append(b.commitment()).unwrap();
    let w = ActionWitness {
        spends: [
            SpendWitness::new(&key, a, tree.path(pa).unwrap()),
            SpendWitness::new(&key, b, tree.path(pb).unwrap()),
        ],
        outputs: [Note { value: 990, owner, rseed: bytes() }, Note { value: 0, owner, rseed: bytes() }],
        anchor: tree.root(),
        sighash: bytes(),
    };
    prove_with_seed(&w, bytes()).unwrap()
}

/// Iterations for the fuzz-style tests: `GHOST_FUZZ_ITERS` (default `default`), for soaks.
fn iterations(default: usize) -> usize {
    std::env::var("GHOST_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn rejected(public: &PublicInputs, proof: &[u8]) -> VerifyError {
    verify(public, proof).expect_err("a tampered proof verified")
}

#[test]
fn honest_proof_verifies_and_is_canonical() {
    let (public, proof) = honest();
    verify(public, proof).unwrap();
    assert!(proof.len() <= MAX_PROOF_BYTES);
    assert!(decode_proof(proof).is_ok());
}

#[test]
fn bit_flips_are_rejected() {
    let (public, proof) = honest();
    let mut rng = StdRng::seed_from_u64(2);
    // Every byte of the head and tail (lengths, commitments, the final polynomial and PoW
    // witnesses live there), and a random sample of the middle.
    let n = proof.len();
    let mut positions: Vec<usize> = (0..256).chain(n - 256..n).collect();
    positions.extend((0..600).map(|_| rng.next_u32() as usize % n));
    let mut outcomes = std::collections::BTreeMap::new();
    for at in positions {
        let mut p = proof.clone();
        p[at] ^= 1 << (rng.next_u32() % 8);
        *outcomes.entry(format!("{:?}", rejected(public, &p))).or_insert(0) += 1;
    }
    // Panics would be caught and reported as rejections; record how often each outcome occurs.
    eprintln!("bit-flip outcomes: {outcomes:?}");
}

#[test]
#[ignore = "flips one bit in every byte of a proof; slow"]
fn every_byte_flip_is_rejected() {
    let (public, proof) = honest();
    for at in 0..proof.len() {
        let mut p = proof.clone();
        p[at] ^= 0x01;
        rejected(public, &p);
    }
}

#[test]
fn truncation_and_extension_are_rejected() {
    let (public, proof) = honest();
    let n = proof.len();
    for len in [0, 1, 2, 8, 64, n / 2, n - 32, n - 1].into_iter().chain((1..64).map(|i| i * n / 64)) {
        assert_eq!(rejected(public, &proof[..len]), VerifyError::Malformed, "truncated to {len}");
    }
    for extra in [&[0u8][..], &[1, 2, 3], &[0xff; 64]] {
        let mut p = proof.clone();
        p.extend_from_slice(extra);
        assert_eq!(rejected(public, &p), VerifyError::Malformed);
    }
    let mut huge = proof.clone();
    huge.resize(MAX_PROOF_BYTES + 1, 0);
    assert_eq!(rejected(public, &huge), VerifyError::TooLarge);
}

#[test]
fn non_canonical_encoding_is_rejected() {
    // The proof starts with a small varint length; re-encode it overlong (0x81 0x00 for 1).
    let (public, proof) = honest();
    assert!(proof[0] < 0x80);
    let mut p = vec![proof[0] | 0x80, 0x00];
    p.extend_from_slice(&proof[1..]);
    assert_eq!(rejected(public, &p), VerifyError::Malformed);
}

#[test]
fn proofs_are_bound_to_their_statement() {
    let (public, proof) = honest();
    let (other_public, other_proof) = make(3);
    verify(&other_public, &other_proof).unwrap();
    assert!(verify(public, &other_proof).is_err());
    assert!(verify(&other_public, proof).is_err());
    // Change one public input at a time.
    let mut variants = Vec::new();
    let mut p = public.clone();
    p.sighash[31] ^= 0x80;
    variants.push(p);
    let mut p = public.clone();
    p.nullifiers.swap(0, 1);
    variants.push(p);
    let mut p = public.clone();
    p.commitments.swap(0, 1);
    variants.push(p);
    let mut p = public.clone();
    p.anchor = other_public.anchor;
    variants.push(p);
    let mut p = public.clone();
    p.nullifiers[0] = other_public.nullifiers[0];
    variants.push(p);
    let mut p = public.clone();
    p.value_balance = ValueBalance::new(p.value_balance.value() + 1).unwrap();
    variants.push(p);
    for p in variants {
        assert!(verify(&p, proof).is_err());
    }
}

#[test]
fn fuzz_random_bytes() {
    let (public, _) = honest();
    let mut rng = StdRng::seed_from_u64(4);
    for i in 0..iterations(400) {
        let len = match i % 4 {
            0 => (rng.next_u32() % 64) as usize,
            1 => (rng.next_u32() % 4096) as usize,
            _ => (rng.next_u32() as usize) % (MAX_PROOF_BYTES + 1),
        };
        let mut p = vec![0u8; len];
        rng.fill_bytes(&mut p);
        assert!(verify(public, &p).is_err());
    }
}

#[test]
fn fuzz_mutated_proofs() {
    // Structured mutations of a valid proof: random bytes, varint-sized edits, spliced chunks.
    let (public, proof) = honest();
    let n = proof.len();
    let mut rng = StdRng::seed_from_u64(5);
    let mut outcomes = std::collections::BTreeMap::new();
    for _ in 0..iterations(300) {
        let mut p = proof.clone();
        for _ in 0..1 + rng.next_u32() % 6 {
            let at = rng.next_u32() as usize % p.len();
            match rng.next_u32() % 5 {
                0 => p[at] = rng.next_u32() as u8,
                1 => p[at] = p[at].wrapping_add(1),
                2 => p[at] = 0xff,
                3 => {
                    let len = 1 + rng.next_u32() as usize % 64;
                    let from = rng.next_u32() as usize % n;
                    let chunk: Vec<u8> = proof[from..(from + len).min(n)].to_vec();
                    let end = (at + chunk.len()).min(p.len());
                    p[at..end].copy_from_slice(&chunk[..end - at]);
                }
                _ => {
                    p.insert(at, rng.next_u32() as u8);
                }
            }
        }
        if p == *proof {
            continue;
        }
        *outcomes.entry(format!("{:?}", rejected(public, &p))).or_insert(0) += 1;
    }
    eprintln!("mutation outcomes: {outcomes:?}");
}
