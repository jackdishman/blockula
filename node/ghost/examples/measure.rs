//! Measures what docs/GHOST-MODE.md reports: proof size, prove and verify time, security level,
//! and the cost of note encryption and trial decryption.
//!
//! `cargo run --release --example measure`

use std::time::{Duration, Instant};

use blockula_ghost::circuit::config::{self, security_report};
use blockula_ghost::circuit::layout::{HEIGHT, WIDTH};
use blockula_ghost::circuit::{decode_proof, prove_with_seed};
use blockula_ghost::encryption::MEMO_BYTES;
use blockula_ghost::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn action(seed: u64) -> ActionWitness {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut bytes = || {
        let mut b = [0u8; 32];
        rng.fill_bytes(&mut b);
        b
    };
    let key = SpendingKey::from_seed(bytes());
    let owner = key.full_viewing_key().owner();
    let mut tree = FullTree::new();
    for i in 0..100u64 {
        tree.append(Note { value: i, owner: [Default::default(); 8], rseed: bytes() }.commitment()).unwrap();
    }
    let a = Note { value: 5_000, owner, rseed: bytes() };
    let b = Note { value: 2_500, owner, rseed: bytes() };
    let pa = tree.append(a.commitment()).unwrap();
    let pb = tree.append(b.commitment()).unwrap();
    let recipient = SpendingKey::from_seed(bytes()).full_viewing_key().owner();
    ActionWitness {
        spends: [
            SpendWitness::new(&key, a, tree.path(pa).unwrap()),
            SpendWitness::new(&key, b, tree.path(pb).unwrap()),
        ],
        outputs: [Note { value: 7_000, owner: recipient, rseed: bytes() }, Note { value: 499, owner, rseed: bytes() }],
        anchor: tree.root(),
        sighash: bytes(),
    }
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn main() {
    let r = security_report();
    println!("trace: {HEIGHT} rows x {WIDTH} columns, {} constraints, max degree {}", r.num_constraints, r.max_constraint_degree);
    println!(
        "FRI: blowup 2^{}, {} queries, PoW bits query/batch/ood {}/{}/{}, {} random codewords, salt {} elements",
        config::LOG_BLOWUP,
        config::NUM_QUERIES,
        config::QUERY_POW_BITS,
        config::BATCH_POW_BITS,
        config::OOD_POW_BITS,
        config::NUM_RANDOM_CODEWORDS,
        config::SALT_ELEMS
    );
    println!(
        "security: proven {} bits (unique decoding {}, list decoding {}), conjectured {} bits; {} quotient chunks, {} batched functions",
        r.proven_bits, r.proven_unique_decoding_bits, r.proven_list_decoding_bits, r.conjectured_bits, r.num_quotient_chunks, r.num_batched_functions
    );

    // Warm up, then time.
    let w = action(1);
    let _ = prove_with_seed(&w, [0; 32]).unwrap();
    let mut prove_times = Vec::new();
    let mut sizes = Vec::new();
    let mut proofs = Vec::new();
    for i in 0..20u64 {
        let w = action(i);
        let t = Instant::now();
        let (public, proof) = prove(&w).unwrap();
        prove_times.push(t.elapsed());
        sizes.push(proof.len());
        proofs.push((public, proof));
    }
    let mut verify_times = Vec::new();
    for _ in 0..5 {
        for (public, proof) in &proofs {
            let t = Instant::now();
            verify(public, proof).unwrap();
            verify_times.push(t.elapsed());
        }
    }
    sizes.sort();
    println!(
        "proof: {}..{} bytes ({:.1}..{:.1} KiB); prove median {:?}; verify median {:?}",
        sizes[0],
        sizes[sizes.len() - 1],
        sizes[0] as f64 / 1024.0,
        sizes[sizes.len() - 1] as f64 / 1024.0,
        median(prove_times),
        median(verify_times)
    );
    let p = decode_proof(&proofs[0].1).unwrap();
    let part = |b: Vec<u8>| b.len();
    println!(
        "proof parts: commitments {} B, opened values {} B, opening proof (FRI + Merkle paths) {} B",
        part(postcard::to_allocvec(&p.commitments).unwrap()),
        part(postcard::to_allocvec(&p.opened_values).unwrap()),
        part(postcard::to_allocvec(&p.opening_proof).unwrap()),
    );

    // Note encryption and scanning.
    let sender = SpendingKey::from_seed([1; 32]);
    let recipient = SpendingKey::from_seed([2; 32]);
    let addr = recipient.address();
    let note = Note { value: 1, owner: addr.owner(), rseed: [3; 32] };
    let mut rng = StdRng::seed_from_u64(9);
    let ovk = sender.full_viewing_key().outgoing();
    let n = 200;
    let t = Instant::now();
    let mut cts = Vec::new();
    for _ in 0..n {
        cts.push(encrypt_note(&note, &addr, &[0; MEMO_BYTES], Some(&ovk), &mut rng).unwrap());
    }
    let enc = t.elapsed() / n;
    let cm = note.commitment();
    let mine = TrialDecryptor::new(&recipient.full_viewing_key().incoming());
    let other = TrialDecryptor::new(&sender.full_viewing_key().incoming());
    let t = Instant::now();
    for c in &cts {
        assert!(mine.try_decrypt(&cm, &c.enc).is_some());
    }
    let hit = t.elapsed() / n;
    let t = Instant::now();
    for c in &cts {
        assert!(other.try_decrypt(&cm, &c.enc).is_none());
    }
    let miss = t.elapsed() / n;
    println!("note encryption {enc:?}; trial decryption {miss:?} (not ours), {hit:?} (ours)");
    let mut frontier = Frontier::new();
    let t = Instant::now();
    for _ in 0..1000 {
        frontier.append(cm).unwrap();
    }
    println!("frontier append {:?}; root {:?}", t.elapsed() / 1000, {
        let t = Instant::now();
        let _ = frontier.root();
        t.elapsed()
    });
}
