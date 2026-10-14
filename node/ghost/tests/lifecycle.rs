//! A whole lifecycle through the public API, with a toy "node" (frontier, root history,
//! nullifier set, turnstile): Alice shields, pays Bob privately, Bob finds the note by scanning
//! and unshields it, and a replay of his spend is refused.

use std::collections::HashSet;

use blockula_ghost::encryption::MEMO_BYTES;
use blockula_ghost::field::Digest;
use blockula_ghost::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// The node's view: what MAINNET-DESIGN §6 keeps in state.
struct Node {
    frontier: Frontier,
    tree: FullTree,
    roots: RootHistory,
    nullifiers: HashSet<Digest>,
    pool: u128,
    outputs: Vec<(Digest, EncryptedNote)>,
}

impl Node {
    fn new() -> Self {
        let frontier = Frontier::new();
        let mut roots = RootHistory::new(16);
        roots.push(frontier.root());
        Self { frontier, tree: FullTree::new(), roots, nullifiers: HashSet::new(), pool: 0, outputs: Vec::new() }
    }

    /// Admission + execution of one action whose value balance the tag implies.
    fn execute(&mut self, public: &PublicInputs, proof: &[u8], enc: [EncryptedNote; 2]) -> Result<(), String> {
        verify(public, proof).map_err(|e| format!("proof: {e:?}"))?;
        if !self.roots.contains(&public.anchor) {
            return Err("unknown anchor".into());
        }
        if public.nullifiers.iter().any(|nf| self.nullifiers.contains(nf)) {
            return Err("nullifier already spent".into());
        }
        // Turnstile: value leaving the pool can't exceed what's in it.
        let vb = public.value_balance.value();
        let pool = i128::try_from(self.pool).unwrap() - vb;
        if pool < 0 {
            return Err("turnstile".into());
        }
        self.pool = u128::try_from(pool).unwrap();
        self.nullifiers.extend(public.nullifiers);
        for (cm, enc) in public.commitments.into_iter().zip(enc) {
            assert_eq!(self.frontier.append(cm), self.tree.append(cm));
            self.outputs.push((cm, enc));
        }
        assert_eq!(self.frontier.root(), self.tree.root());
        self.roots.push(self.frontier.root());
        Ok(())
    }

    /// Positions and notes this key can decrypt.
    fn scan(&self, key: &SpendingKey) -> Vec<(u32, Note)> {
        let scanner = TrialDecryptor::new(&key.full_viewing_key().incoming());
        self.outputs
            .iter()
            .enumerate()
            .filter_map(|(pos, (cm, enc))| scanner.try_decrypt(cm, &enc.enc).map(|d| (pos as u32, d.note)))
            .collect()
    }
}

fn seed(rng: &mut StdRng) -> [u8; 32] {
    let mut b = [0u8; 32];
    rng.fill_bytes(&mut b);
    b
}

#[test]
fn shield_transfer_scan_unshield() {
    let mut rng = StdRng::seed_from_u64(1);
    let alice = SpendingKey::from_seed(seed(&mut rng));
    let bob = SpendingKey::from_seed(seed(&mut rng));
    let alice_ovk = alice.full_viewing_key().outgoing();
    let memo = [0u8; MEMO_BYTES];
    let mut node = Node::new();

    // 1. Alice shields 1,000 (value balance −1,000; her fee is paid transparently).
    let note = Note { value: 1_000, owner: alice.address().owner(), rseed: seed(&mut rng) };
    let enc = encrypt_note(&note, &alice.address(), &memo, Some(&alice_ovk), &mut rng).unwrap();
    let (pad, pad_enc) = dummy_output(&mut rng);
    let w = ActionWitness {
        spends: [SpendWitness::dummy(&mut rng), SpendWitness::dummy(&mut rng)],
        outputs: [note, pad],
        anchor: node.frontier.root(),
        sighash: seed(&mut rng),
    };
    let (public, proof) = prove(&w).unwrap();
    assert_eq!(public.value_balance.value(), -1_000);
    node.execute(&public, &proof, [enc, pad_enc]).unwrap();
    assert_eq!(node.pool, 1_000);

    // 2. Alice finds her note and pays Bob 600 with a fee of 10.
    let found = node.scan(&alice);
    assert_eq!(found.len(), 1);
    let (pos, note) = found[0];
    let to_bob = Note { value: 600, owner: bob.address().owner(), rseed: seed(&mut rng) };
    let change = Note { value: 390, owner: alice.address().owner(), rseed: seed(&mut rng) };
    let w = ActionWitness {
        spends: [SpendWitness::new(&alice, note, node.tree.path(pos).unwrap()), SpendWitness::dummy(&mut rng)],
        outputs: [to_bob, change],
        anchor: node.tree.root(),
        sighash: seed(&mut rng),
    };
    let (public, proof) = prove(&w).unwrap();
    assert_eq!(public.value_balance.value(), 10);
    let encs = [
        encrypt_note(&to_bob, &bob.address(), &memo, Some(&alice_ovk), &mut rng).unwrap(),
        encrypt_note(&change, &alice.address(), &memo, Some(&alice_ovk), &mut rng).unwrap(),
    ];
    node.execute(&public, &proof, encs.clone()).unwrap();
    assert_eq!(node.pool, 990);
    // Alice's outgoing key recovers what she sent.
    let sent = try_decrypt_outgoing(&alice_ovk, &public.commitments[0], &encs[0].enc, &encs[0].out).unwrap();
    assert_eq!(sent.note, to_bob);

    // 3. Bob scans and finds exactly his note.
    let found = node.scan(&bob);
    assert_eq!(found.len(), 1);
    let (bob_pos, bob_note) = found[0];
    assert_eq!(bob_note, to_bob);

    // 4. Bob unshields 595 with a fee of 5 (value balance 600, both outputs dummies).
    let (pad0, pad0_enc) = dummy_output(&mut rng);
    let (pad1, pad1_enc) = dummy_output(&mut rng);
    let spend = SpendWitness::new(&bob, bob_note, node.tree.path(bob_pos).unwrap());
    let anchor = node.tree.root();
    let w = ActionWitness {
        spends: [spend.clone(), SpendWitness::dummy(&mut rng)],
        outputs: [pad0, pad1],
        anchor,
        sighash: seed(&mut rng),
    };
    let (public, proof) = prove(&w).unwrap();
    assert_eq!(public.value_balance.value(), 600);
    node.execute(&public, &proof, [pad0_enc.clone(), pad1_enc.clone()]).unwrap();
    assert_eq!(node.pool, 390);

    // 5. The same note again (against the same, still recent, anchor): a fresh, valid proof,
    // refused by the nullifier set.
    let w = ActionWitness {
        spends: [spend, SpendWitness::dummy(&mut rng)],
        outputs: [pad0, pad1],
        anchor,
        sighash: seed(&mut rng),
    };
    let (public, proof) = prove(&w).unwrap();
    assert_eq!(node.execute(&public, &proof, [pad0_enc, pad1_enc]), Err("nullifier already spent".into()));
    assert_eq!(node.pool, 390);
}

#[test]
fn turnstile_stops_an_unshield_larger_than_the_pool() {
    // A valid proof that takes value out of an empty pool (possible only for a note created
    // outside this node's history, e.g. a counterfeit) is stopped by the turnstile.
    let mut rng = StdRng::seed_from_u64(2);
    let alice = SpendingKey::from_seed(seed(&mut rng));
    let mut node = Node::new();
    let mut elsewhere = FullTree::new();
    let note = Note { value: 50, owner: alice.address().owner(), rseed: seed(&mut rng) };
    let pos = elsewhere.append(note.commitment()).unwrap();
    let (pad0, enc0) = dummy_output(&mut rng);
    let (pad1, enc1) = dummy_output(&mut rng);
    let w = ActionWitness {
        spends: [SpendWitness::new(&alice, note, elsewhere.path(pos).unwrap()), SpendWitness::dummy(&mut rng)],
        outputs: [pad0, pad1],
        anchor: elsewhere.root(),
        sighash: seed(&mut rng),
    };
    let (public, proof) = prove(&w).unwrap();
    // Unknown anchor first; with the anchor accepted, the turnstile.
    assert_eq!(node.execute(&public, &proof, [enc0.clone(), enc1.clone()]), Err("unknown anchor".into()));
    node.roots.push(elsewhere.root());
    assert_eq!(node.execute(&public, &proof, [enc0, enc1]), Err("turnstile".into()));
}
