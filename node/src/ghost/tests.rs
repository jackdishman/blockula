//! Ghost Mode through the node: real proofs (the `blockula-ghost` prover, as a wallet would run
//! it), real txs through the mempool and blocks. Every test block also checks the incremental
//! root against a rebuild and the invariants (pool included) in `State::debug_check`.

use super::*;
use crate::chain::{now_ms, Chain, SummonOpts};
use crate::crypto::{self, Keys};
use blockula_ghost::encryption::MEMO_BYTES;
use blockula_ghost::field::Digest;
use blockula_ghost::{dummy_output, encrypt_note, ActionWitness, EncryptedNote, FullTree, Note, SpendWitness, SpendingKey, TrialDecryptor};
use std::path::PathBuf;

const FANG: u64 = DROPS_PER_FANG;
/// An unsigned tx's fee in these tests: the base fee of a tx well above a real one's size, plus
/// the default verify fee (the excess is burned).
const FEE: u64 = 10_000 + 200_000 * 10 + FANG / 1_000;

/// Deterministic randomness for test wallets (BLAKE3's XOF). Provers draw their masks from the
/// OS regardless.
struct TestRng(blake3::OutputReader);

impl TestRng {
    fn new(seed: &str) -> Self {
        TestRng(blake3::Hasher::new_derive_key("blockula ghost node tests").update(seed.as_bytes()).finalize_xof())
    }

    fn bytes(&mut self) -> [u8; 32] {
        let mut b = [0; 32];
        self.0.fill(&mut b);
        b
    }
}

impl rand_core::TryRng for TestRng {
    type Error = std::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0; 4];
        self.0.fill(&mut b);
        Ok(u32::from_le_bytes(b))
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0; 8];
        self.0.fill(&mut b);
        Ok(u64::from_le_bytes(b))
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.0.fill(dst);
        Ok(())
    }
}

impl rand_core::TryCryptoRng for TestRng {}

/// A test chain (this node is its first validator) and a wallet's randomness.
struct Net {
    root: PathBuf,
    home: PathBuf,
    c: Chain,
    validators: Vec<Keys>,
    rng: TestRng,
}


/// A chain of `validators` validators with 20-block epochs, Ghost Mode on at genesis when `on`,
/// a 1,000 FANG pool cap, and `params` overriding chain parameters.
fn net(tag: &str, on: bool, validators: usize, params: &[(&str, u64)]) -> Net {
    let root = std::env::temp_dir().join(format!("blockula-ghost-{tag}-{}-{}", now_ms(), std::process::id()));
    let mut o = SummonOpts::new(&format!("ghost-{tag}"), validators);
    o.epoch_blocks = 20;
    o.params.insert("ghost_pool_cap".into(), 1_000 * FANG);
    for (k, v) in params {
        o.params.insert(k.to_string(), *v);
    }
    let homes = Chain::summon(&root, &o).unwrap();
    let files: Vec<PathBuf> = homes.iter().map(|h| h.join("genesis.json")).chain((validators > 1).then(|| root.join("genesis.json"))).collect();
    for f in files.iter().filter(|_| on) {
        let mut g: serde_json::Value = serde_json::from_slice(&std::fs::read(f).unwrap()).unwrap();
        g["features"] = json!([FEATURE]);
        std::fs::write(f, serde_json::to_vec_pretty(&g).unwrap()).unwrap();
    }
    let validators = homes.iter().map(|h| Keys::from_seed(hex::decode(std::fs::read_to_string(h.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap())).collect();
    let c = Chain::open(&homes[0]).unwrap();
    Net { root, home: homes[0].clone(), c, validators, rng: TestRng::new(tag) }
}

impl Net {
    fn cleanup(self) {
        let root = self.root.clone();
        drop(self);
        std::fs::remove_dir_all(root).ok();
    }

    /// Closes the chain and opens its home again.
    fn reopen(self) -> Net {
        let Net { root, home, c, validators, rng } = self;
        drop(c);
        let c = Chain::open(&home).unwrap();
        Net { root, home, c, validators, rng }
    }

    /// Funds transparent accounts from the faucet (one block).
    fn fund(&mut self, who: &[&Keys], amount: u64) {
        for k in who {
            self.c.faucet(&k.address(), amount).unwrap();
        }
        self.c.produce_block().unwrap();
    }

    /// What the HTTP API does with a submitted tx: stateless checks (signature, proof), then
    /// mempool admission.
    fn submit(&mut self, tx: &Tx) -> Result<String> {
        crate::consensus::check_tx_stateless(tx, &self.c.genesis.chain_id, &self.c.genesis.address_prefix)?;
        self.c.submit(tx.clone())
    }

    /// Executes `txs` as the next block and returns which of them were included.
    fn block(&mut self, txs: Vec<Tx>) -> Vec<bool> {
        let ids: Vec<String> = txs.iter().map(Tx::id).collect();
        self.c.produce_block_with(txs).unwrap();
        let included: Vec<String> = self.c.last_block().unwrap().txs.iter().map(Tx::id).collect();
        ids.iter().map(|id| included.contains(id)).collect()
    }

    fn empty_blocks(&mut self, n: u64) {
        for _ in 0..n {
            self.c.produce_block().unwrap();
        }
    }

    fn instance(&self) -> [u8; 32] {
        self.c.state.ghost.meta.instance
    }

    /// The current anchor.
    fn anchor(&self) -> Digest {
        digest_from_bytes(&self.c.state.ghost.meta.root).unwrap()
    }

    /// The wallet's view of the commitment tree, rebuilt from the state's commitments.
    fn tree(&self) -> FullTree {
        let mut t = FullTree::new();
        for (pos, cm) in self.c.state.ghost.commitments.iter() {
            assert_eq!(u64::from(t.append(digest_from_bytes(cm).unwrap()).unwrap()), *pos);
        }
        assert_eq!(t.root(), self.anchor(), "the wallet's tree and the chain's anchor agree");
        t
    }

    /// A spend of `note` (owned by `key`) at its position in the tree.
    fn spend(&self, key: &SpendingKey, note: Note) -> SpendWitness {
        let t = self.tree();
        let cm = digest_to_bytes(&note.commitment());
        let pos = self.c.state.ghost.commitments.iter().find(|(_, c)| **c == cm).map(|(p, _)| *p as u32).expect("the note is in the tree");
        SpendWitness::new(key, note, t.path(pos).unwrap())
    }

    fn output(&mut self, value: u64, to: &SpendingKey, from: &SpendingKey) -> (Note, EncryptedNote) {
        let note = Note { value, owner: to.address().owner(), rseed: self.rng.bytes() };
        let enc = encrypt_note(&note, &to.address(), &[0; MEMO_BYTES], Some(&from.full_viewing_key().outgoing()), &mut self.rng).unwrap();
        (note, enc)
    }

    fn dummy_spend(&mut self) -> SpendWitness {
        SpendWitness::dummy(&mut self.rng)
    }

    fn dummy_output(&mut self) -> (Note, EncryptedNote) {
        dummy_output(&mut self.rng)
    }

    /// A shield of `amount` from `from` to the ghost key `to`, proved and signed. Returns the note.
    fn shield(&mut self, from: &Keys, amount: u64, to: &SpendingKey) -> (Tx, Note) {
        let out = self.output(amount, to, to);
        let note = out.0;
        let act = Act { spends: [self.dummy_spend(), self.dummy_spend()], outputs: [out, self.dummy_output()], anchor: self.anchor() };
        let mut tx = Tx { chain_id: self.c.genesis.chain_id.clone(), from: from.address(), nonce: self.c.next_nonce(&from.address()), fee: FEE, tip: 0, valid_until: self.c.height() + 100, action: Action::Shield { amount, bundle: Box::new(act.bundle(self.instance())) }, memo: String::new(), public_key: hex::encode(from.public_key()), signature: String::new() };
        tx = act.prove(tx);
        tx.signature = "00".repeat(crypto::SIGNATURE_LEN);
        (tx.sign(from), note)
    }

    /// An unsigned tx around a ghost action whose proof is still empty, valid for 100 blocks.
    fn unsigned(&self, action: Action, fee: u64, tip: u64) -> Tx {
        Tx { chain_id: self.c.genesis.chain_id.clone(), from: String::new(), nonce: 0, fee, tip, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: String::new(), signature: String::new() }
    }

    /// A private transfer spending `note` (of `key`) into `outputs`, with fee `FEE`.
    fn transfer(&mut self, key: &SpendingKey, note: Note, outputs: [(Note, EncryptedNote); 2]) -> Tx {
        let act = Act { spends: [self.spend(key, note), self.dummy_spend()], outputs, anchor: self.anchor() };
        let tx = self.unsigned(Action::PrivateTransfer { bundle: Box::new(act.bundle(self.instance())) }, FEE, 0);
        act.prove(tx)
    }

    /// An unshield of `amount` to `to` from `note` (of `key`), the rest back to `key`, with fee `FEE`.
    fn unshield(&mut self, key: &SpendingKey, note: Note, to: &str, amount: u64) -> Tx {
        let change = self.output(note.value - amount - FEE, key, key);
        let act = Act { spends: [self.spend(key, note), self.dummy_spend()], outputs: [change, self.dummy_output()], anchor: self.anchor() };
        let tx = self.unsigned(Action::Unshield { to: to.into(), amount, bundle: Box::new(act.bundle(self.instance())) }, FEE, 0);
        act.prove(tx)
    }
}

/// One Action as a wallet assembles it before proving.
struct Act {
    spends: [SpendWitness; 2],
    outputs: [(Note, EncryptedNote); 2],
    anchor: Digest,
}

impl Act {
    /// The bundle, with an empty proof.
    fn bundle(&self, instance: [u8; 32]) -> GhostBundle {
        let h = |d: &Digest| HexBytes(digest_to_bytes(d).to_vec());
        GhostBundle {
            instance: HexBytes(instance.to_vec()),
            anchor: h(&self.anchor),
            nullifiers: [h(&self.spends[0].nullifier()), h(&self.spends[1].nullifier())],
            commitments: [h(&self.outputs[0].0.commitment()), h(&self.outputs[1].0.commitment())],
            enc: [HexBytes(self.outputs[0].1.enc.clone()), HexBytes(self.outputs[1].1.enc.clone())],
            out: [HexBytes(self.outputs[0].1.out.clone()), HexBytes(self.outputs[1].1.out.clone())],
            proof: HexBytes::default(),
        }
    }

    /// Proves the action for `tx`'s sighash (every field but the proof) and puts the proof in.
    fn prove(self, mut tx: Tx) -> Tx {
        let sighash = tx.ghost_sighash().unwrap();
        let w = ActionWitness { spends: self.spends, outputs: [self.outputs[0].0, self.outputs[1].0], anchor: self.anchor, sighash };
        let (_, proof) = blockula_ghost::prove(&w).unwrap();
        set_proof(&mut tx, proof);
        tx
    }
}

fn set_proof(tx: &mut Tx, proof: Vec<u8>) {
    match &mut tx.action {
        Action::Shield { bundle, .. } | Action::PrivateTransfer { bundle } | Action::Unshield { bundle, .. } => bundle.proof = HexBytes(proof),
        _ => unreachable!(),
    }
}

fn bundle_mut(tx: &mut Tx) -> &mut GhostBundle {
    match &mut tx.action {
        Action::Shield { bundle, .. } | Action::PrivateTransfer { bundle } | Action::Unshield { bundle, .. } => bundle,
        _ => unreachable!(),
    }
}

fn err(r: Result<String>) -> String {
    format!("{:#}", r.expect_err("should be refused"))
}

fn balance(n: &Net, k: &Keys) -> u64 {
    n.c.state.account(&k.address()).balance
}

/// Shield → private transfer (found by the recipient's trial decryption) → unshield, through the
/// mempool and blocks; the pool, balances and supply add up at every step; the result survives a
/// restart and a rebuild from the block logs.
#[test]
fn shield_transfer_unshield_through_the_node() {
    let mut n = net("lifecycle", true, 1, &[]);
    let (alice, carol) = (Keys::from_seed([1; 32]), Keys::from_seed([3; 32]));
    let (ghost_alice, ghost_bob) = (SpendingKey::from_seed([11; 32]), SpendingKey::from_seed([12; 32]));
    n.fund(&[&alice], 300 * FANG);
    let verify_fee = n.c.state.chain_params.ghost_verify_fee;
    assert!(verify_fee > 0);

    // 1. Alice shields 100 FANG: her balance pays it, the verify fee and the byte fee.
    let (tx, note_a) = n.shield(&alice, 100 * FANG, &ghost_alice);
    let (before, supply) = (balance(&n, &alice), n.c.state.supply);
    let charge = tx.charge().unwrap();
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    let s = &n.c.state;
    assert_eq!(s.ghost.meta.pool, 100 * FANG);
    assert_eq!(balance(&n, &alice), before - 100 * FANG - verify_fee - charge);
    assert_eq!(s.supply, supply - verify_fee - (charge - tx.tip), "the verify fee and the base fee are burned");
    assert_eq!((s.ghost.commitments.len(), s.ghost.nullifiers.len(), s.ghost.roots.len()), (2, 2, 1), "both outputs (one a dummy) and both (dummy) nullifiers recorded; the empty root retired");
    assert!(n.c.describe_last(0).contains("shielded 100 FANG into Ghost Mode"));

    // 2. Alice pays Bob 60 FANG privately; the change comes back to her. The fee leaves the pool.
    let to_bob = n.output(60 * FANG, &ghost_bob, &ghost_alice);
    let change = n.output(40 * FANG - FEE, &ghost_alice, &ghost_alice);
    let tx = n.transfer(&ghost_alice, note_a, [to_bob, change]);
    assert!(tx.is_unsigned() && tx.to_bytes().starts_with(GHOST_TX_DOMAIN));
    let proposer = n.c.me().unwrap();
    let (supply, proposer_before) = (n.c.state.supply, n.c.state.account(&proposer).balance);
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.ghost.meta.pool, 100 * FANG - FEE);
    assert_eq!(n.c.state.supply, supply - FEE, "an unsigned tx's whole fee is burned (no tip)");
    assert_eq!(n.c.state.account(&proposer).balance, proposer_before);
    assert!(n.c.describe_last(0).contains("👻 a ghost transaction happened"));
    // Bob finds exactly his note by trial-decrypting the block's outputs.
    let scanner = TrialDecryptor::new(&ghost_bob.full_viewing_key().incoming());
    let block = n.c.last_block().unwrap().clone();
    let found: Vec<Note> = block.txs.iter().filter_map(|t| t.action.ghost_bundle()).flat_map(|b| b.commitments.iter().zip(&b.enc).filter_map(|(cm, enc)| scanner.try_decrypt(&digest_from_bytes(&cm.array32().unwrap()).unwrap(), &enc.0)).collect::<Vec<_>>()).map(|d| d.note).collect();
    assert_eq!(found.len(), 1);
    let note_b = found[0];
    assert_eq!(note_b.value, 60 * FANG);

    // 3. Bob unshields 50 FANG to Carol's transparent account (a new account), with a tip.
    let tx = n.unshield(&ghost_bob, note_b, &carol.address(), 50 * FANG);
    assert_eq!(n.c.state.account(&carol.address()).balance, 0);
    // The proof is bound to the fee: a relayer can't raise it (or the tip, or the recipient).
    let mut raised = tx.clone();
    raised.fee += 1;
    assert!(err(n.submit(&raised)).contains("doesn't verify"));
    let mut redirected = tx.clone();
    if let Action::Unshield { to, .. } = &mut redirected.action {
        *to = alice.address();
    }
    assert!(err(n.submit(&redirected)).contains("doesn't verify"));
    let mut swapped = tx.clone();
    bundle_mut(&mut swapped).enc.swap(0, 1);
    assert!(err(n.submit(&swapped)).contains("doesn't verify"), "the ciphertexts are bound too");
    let supply = n.c.state.supply;
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    let s = &n.c.state;
    assert_eq!(s.account(&carol.address()).balance, 50 * FANG);
    assert_eq!(s.ghost.meta.pool, 100 * FANG - FEE - 50 * FANG - FEE, "the pool holds exactly the live notes: Alice's and Bob's change");
    assert_eq!(s.ghost.meta.pool, (40 * FANG - FEE) + (10 * FANG - FEE));
    assert_eq!(s.supply, supply - FEE);
    assert!(n.c.describe_last(0).contains(&format!("50 FANG left Ghost Mode for {}", short(&carol.address()))));
    assert_eq!(s.ghost.commitments.len(), 6);

    // The same state after a restart (from the state database) and after a rebuild from the
    // block logs (every ghost tx executed again).
    let (root, height, ghost) = (n.c.state.root(), n.c.height(), n.c.state.ghost.meta.clone());
    let n = n.reopen();
    assert_eq!((n.c.state.root(), n.c.height(), n.c.replayed), (root.clone(), height, 0));
    assert_eq!(n.c.state.ghost.meta, ghost);
    std::fs::remove_file(n.home.join(crate::store::DB_FILE)).unwrap();
    let n = n.reopen();
    assert_eq!((n.c.state.root(), n.c.replayed), (root.clone(), height));
    // And through the records snapshots stream.
    let mut records = vec![];
    n.c.state.for_each_record(|ns, k, v| records.push(Ok((ns, k.to_vec(), v.to_vec()))));
    let rebuilt = State::from_records(records).unwrap();
    assert_eq!(rebuilt.root(), root);
    assert_eq!(rebuilt.ghost.meta, ghost);
    n.cleanup();
}

trait DescribeLast {
    fn describe_last(&self, i: usize) -> String;
}

impl DescribeLast for Chain {
    /// The summary of tx `i` of the last block.
    fn describe_last(&self, i: usize) -> String {
        let b = self.last_block().unwrap();
        self.state.describe(&b.txs[i], Some(b.height))
    }
}

/// A note spends once: a second spend is refused by the mempool and skipped in blocks, whether
/// it comes in a later block or in the same one; a higher tip per byte replaces a queued spend.
#[test]
fn double_spends_are_refused() {
    let mut n = net("double", true, 1, &[]);
    let alice = Keys::from_seed([1; 32]);
    let key = SpendingKey::from_seed([21; 32]);
    let carol = Keys::from_seed([3; 32]).address();
    n.fund(&[&alice], 300 * FANG);
    let (tx, note) = n.shield(&alice, 100 * FANG, &key);
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();

    // Two different spends of the same note (different amounts, so different txs and outputs).
    let first = n.unshield(&key, note, &carol, 10 * FANG);
    let second = n.unshield(&key, note, &carol, 20 * FANG);
    assert_ne!(first.id(), second.id());
    assert_eq!(first.action.ghost_bundle().unwrap().nullifiers[0], second.action.ghost_bundle().unwrap().nullifiers[0]);
    // In one block: the first applies, the second is skipped (and costs nobody anything).
    let pool = n.c.state.ghost.meta.pool;
    assert_eq!(n.block(vec![first.clone(), second.clone()]), vec![true, false]);
    assert_eq!(n.c.last_block().unwrap().skipped.len(), 1);
    assert!(n.c.failure(&second.id()).unwrap().0.contains("already spent"));
    assert_eq!(n.c.state.ghost.meta.pool, pool - 10 * FANG - FEE);
    // In a later block, and at the mempool: refused.
    assert!(err(n.submit(&second)).contains("already spent"));
    assert_eq!(n.block(vec![second.clone()]), vec![false]);
    // The same tx again: its id is already finalized, so it's a no-op at the mempool…
    assert_eq!(n.c.submit(first.clone()).unwrap(), first.id());
    assert!(!n.c.in_mempool(&first.id()));
    // …and refused in a block.
    assert_eq!(n.block(vec![first]), vec![false]);

    // Queued spends: one per note; a higher tip per byte replaces the queued one.
    let (tx, note) = n.shield(&alice, 100 * FANG, &key);
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    let low = n.unshield(&key, note, &carol, 10 * FANG);
    let same = n.unshield(&key, note, &carol, 11 * FANG);
    n.submit(&low).unwrap();
    assert!(err(n.submit(&same)).contains("already spends one of these notes"));
    let act_tip = {
        let change = n.output(100 * FANG - 12 * FANG - FEE - 5_000, &key, &key);
        let act = Act { spends: [n.spend(&key, note), n.dummy_spend()], outputs: [change, n.dummy_output()], anchor: n.anchor() };
        let tx = n.unsigned(Action::Unshield { to: carol.clone(), amount: 12 * FANG, bundle: Box::new(act.bundle(n.instance())) }, FEE + 5_000, 5_000);
        act.prove(tx)
    };
    n.submit(&act_tip).unwrap();
    assert!(!n.c.in_mempool(&low.id()) && n.c.in_mempool(&act_tip.id()));
    assert!(n.c.failure(&low.id()).unwrap().0.contains("replaced"));
    let proposer = n.c.me().unwrap();
    let before = n.c.state.account(&proposer).balance;
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.account(&proposer).balance, before + 5_000, "the tip goes to the proposer");
    assert_eq!(n.c.state.account(&carol).balance, 22 * FANG);
    n.cleanup();
}

/// Anchors: the current root, and a replaced root for `ghost_anchor_window` blocks, no longer.
#[test]
fn stale_anchors_are_refused() {
    let mut n = net("stale", true, 1, &[("ghost_anchor_window", 3)]);
    let alice = Keys::from_seed([1; 32]);
    let key = SpendingKey::from_seed([31; 32]);
    let carol = Keys::from_seed([3; 32]).address();
    n.fund(&[&alice], 300 * FANG);
    let (tx, note) = n.shield(&alice, 100 * FANG, &key);
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    // Proved against the root after the first shield …
    let old = n.unshield(&key, note, &carol, 10 * FANG);
    // … which another shield then replaces, at block `r`.
    let (tx, _) = n.shield(&alice, 5 * FANG, &key);
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    let r = n.c.height();
    let anchor = old.action.ghost_bundle().unwrap().anchor.array32().unwrap();
    assert_eq!(n.c.state.ghost.roots.get(&anchor), Some(&r));
    // Usable through block r + 3 (the window), refused from r + 4 on; then the record goes.
    n.empty_blocks(2);
    assert_eq!(n.c.height() + 1, r + 3);
    let mut probe = n.c.state.clone();
    assert!(probe.apply(&old, &n.c.genesis.chain_id, "x", r + 3).is_ok(), "still within the window");
    n.empty_blocks(1);
    assert!(err(n.submit(&old)).contains("isn't a recent root"));
    assert_eq!(n.block(vec![old.clone()]), vec![false]);
    assert!(n.c.failure(&old.id()).unwrap().0.contains("isn't a recent root"));
    assert!(!n.c.state.ghost.roots.contains_key(&anchor), "aged out of the state");
    // Proved again on the current root, the same spend goes through.
    let fresh = n.unshield(&key, note, &carol, 10 * FANG);
    n.submit(&fresh).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.account(&carol).balance, 10 * FANG);
    n.cleanup();
}

/// The turnstile: a valid proof spending a note from outside the pool's history (a counterfeit,
/// what a soundness bug would let through) can't take out more than the pool holds, so a broken
/// circuit can steal shielded deposits but never mint.
#[test]
fn the_turnstile_bounds_a_counterfeit_note() {
    let mut n = net("turnstile", true, 1, &[]);
    let alice = Keys::from_seed([1; 32]);
    let (honest, thief) = (SpendingKey::from_seed([41; 32]), SpendingKey::from_seed([42; 32]));
    let carol = Keys::from_seed([3; 32]).address();
    n.fund(&[&alice], 300 * FANG);
    let (tx, _) = n.shield(&alice, 20 * FANG, &honest);
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    // A note worth 500 FANG in a tree the chain never had, and its root accepted as an anchor
    // (as if membership proofs were broken).
    let fake = Note { value: 500 * FANG, owner: thief.address().owner(), rseed: n.rng.bytes() };
    let mut elsewhere = FullTree::new();
    let pos = elsewhere.append(fake.commitment()).unwrap();
    let fake_root = digest_to_bytes(&elsewhere.root());
    n.c.state.ghost.roots.insert(fake_root, u64::MAX);
    n.c.state.ghost.retired.insert(u64::MAX, fake_root);
    let spend = || SpendWitness::new(&thief, fake, elsewhere.path(pos).unwrap());
    let carry = |n: &mut Net, amount: u64| {
        let change = n.output(500 * FANG - amount - FEE, &thief, &thief);
        let act = Act { spends: [spend(), n.dummy_spend()], outputs: [change, n.dummy_output()], anchor: elsewhere.root() };
        let tx = n.unsigned(Action::Unshield { to: carol.clone(), amount, bundle: Box::new(act.bundle(n.instance())) }, FEE, 0);
        act.prove(tx)
    };
    let (pool, supply) = (n.c.state.ghost.meta.pool, n.c.state.supply);
    let greedy = carry(&mut n, 400 * FANG);
    assert!(crate::ghost::verify_proof(&greedy).is_ok(), "the proof itself is valid");
    assert!(err(n.submit(&greedy)).contains("turnstile"));
    assert_eq!(n.block(vec![greedy.clone()]), vec![false]);
    assert!(n.c.failure(&greedy.id()).unwrap().0.contains("turnstile"));
    assert_eq!((n.c.state.ghost.meta.pool, n.c.state.supply), (pool, supply), "nothing minted, nothing moved");
    assert_eq!(n.c.state.account(&carol).balance, 0);
    // A private transfer whose fee alone is more than the pool holds is stopped the same way.
    let outputs = [n.output(500 * FANG - (pool + 1), &thief, &thief), n.dummy_output()];
    let act = Act { spends: [spend(), n.dummy_spend()], outputs, anchor: elsewhere.root() };
    let tx = n.unsigned(Action::PrivateTransfer { bundle: Box::new(act.bundle(n.instance())) }, pool + 1, 0);
    let tx = act.prove(tx);
    assert!(err(n.submit(&tx)).contains("turnstile"));
    // Within the pool, the counterfeit does take honest deposits: what the turnstile bounds
    // (and the pool cap with it) is the damage, not the bug.
    let modest = carry(&mut n, pool - FEE);
    n.submit(&modest).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!((n.c.state.ghost.meta.pool, n.c.state.account(&carol).balance), (0, pool - FEE));
    assert_eq!(n.c.state.supply, supply - FEE, "still nothing minted");
    n.cleanup();
}

/// `ghost_pool_cap` bounds the pool: a shield past it fails (charged, like any failed signed tx)
/// until governance raises the cap.
#[test]
fn the_pool_cap_is_enforced() {
    let mut n = net("cap", true, 1, &[("ghost_pool_cap", 100 * FANG)]);
    let alice = Keys::from_seed([1; 32]);
    let key = SpendingKey::from_seed([51; 32]);
    n.fund(&[&alice], 500 * FANG);
    let (tx, _) = n.shield(&alice, 60 * FANG, &key);
    n.submit(&tx).unwrap();
    n.c.produce_block().unwrap();
    let (over, _) = n.shield(&alice, 50 * FANG, &key);
    assert!(err(n.submit(&over)).contains("ghost_pool_cap"));
    let before = balance(&n, &alice);
    assert_eq!(n.block(vec![over.clone()]), vec![true], "included and charged its fee");
    let b = n.c.last_block().unwrap();
    assert!(b.failed[&0].contains("ghost_pool_cap"));
    assert_eq!(balance(&n, &alice), before - over.charge().unwrap(), "only the fee: neither the amount nor the verify fee");
    assert_eq!(n.c.state.ghost.meta.pool, 60 * FANG);
    // Up to the cap exactly is fine; then governance raises it.
    let (exact, _) = n.shield(&alice, 40 * FANG, &key);
    n.submit(&exact).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.ghost.meta.pool, 100 * FANG);
    let mut j = Journal::default();
    n.c.state.exec_set_param(&mut j, "ghost_pool_cap", 200 * FANG).unwrap();
    let (more, _) = n.shield(&alice, 50 * FANG, &key);
    n.submit(&more).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.ghost.meta.pool, 150 * FANG);
    n.cleanup();
}

/// `GhostPause`: f + 1 distinct validators of the epoch (2 of 4) pause the pool from the next
/// block; signatures don't carry across epochs; only validators may sign; governance resumes the
/// pool by switching the feature off and on.
#[test]
fn f_plus_one_validators_pause_and_governance_resumes() {
    let mut n = net("pause", true, 4, &[]);
    let alice = Keys::from_seed([1; 32]);
    let key = SpendingKey::from_seed([61; 32]);
    n.fund(&[&alice], 300 * FANG);
    let v: Vec<Keys> = n.validators.iter().map(|k| k.clone_keys()).collect();
    let pause = |n: &Net, k: &Keys| {
        let mut tx = Tx { chain_id: n.c.genesis.chain_id.clone(), from: k.address(), nonce: n.c.next_nonce(&k.address()), fee: 0, tip: 0, valid_until: n.c.height() + 100, action: Action::GhostPause {}, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        tx.sign(k)
    };
    assert_eq!(pause_threshold(4), 2);
    assert_eq!((pause_threshold(1), pause_threshold(3), pause_threshold(31)), (1, 1, 11));
    // A non-validator can't.
    assert!(err(n.submit(&pause(&n, &alice))).contains("only a validator"));
    // One signature in epoch 0, one in epoch 1: not f + 1 in either.
    n.submit(&pause(&n, &v[1])).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.ghost.meta.votes.as_ref().unwrap().voters.len(), 1);
    assert!(err(n.submit(&pause(&n, &v[1]))).contains("already signed"));
    while n.c.state.params.epoch_of(n.c.height() + 1) == 0 {
        n.c.produce_block().unwrap();
    }
    n.submit(&pause(&n, &v[2])).unwrap();
    n.c.produce_block().unwrap();
    assert!(!paused(&n.c.state, n.c.height() + 1));
    // A second validator in the same epoch: paused from the next block. The shield queued with
    // it in that block still goes through; the next one is refused.
    let (shield, _) = n.shield(&alice, 10 * FANG, &key);
    assert_eq!(n.block(vec![pause(&n, &v[3]), shield]), vec![true, true]);
    let h = n.c.height();
    assert!(paused(&n.c.state, h + 1) && !paused(&n.c.state, h));
    assert_eq!(status_json(&n.c.state, h)["paused"], true);
    let (later, _) = n.shield(&alice, 10 * FANG, &key);
    assert!(err(n.submit(&later)).contains("Ghost Mode is paused"));
    assert_eq!(n.block(vec![later.clone()]), vec![false]);
    assert!(err(n.submit(&pause(&n, &v[0]))).contains("paused"), "nothing more to sign");
    // Governance: off, then on again (a new activation, which the pause doesn't cover).
    let mut j = Journal::default();
    crate::upgrade::exec_deactivate_feature(&mut n.c.state, &mut j, FEATURE).unwrap();
    assert!(err(n.submit(&later)).contains("Ghost Mode is off"));
    let h = n.c.height();
    crate::upgrade::exec_activate_feature(&mut n.c.state, &mut j, FEATURE, h).unwrap();
    assert!(!paused(&n.c.state, n.c.height() + 1));
    n.submit(&later).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.ghost.meta.pool, 20 * FANG);
    n.cleanup();
}

/// Disabled by default (DEC-2): every ghost action is refused, at the mempool and in blocks,
/// with the same clear error, and nothing in the state moves.
#[test]
fn disabled_by_default_refuses_everything() {
    let mut on = net("prover", true, 1, &[]);
    let mut n = net("off", false, 1, &[]);
    let alice = Keys::from_seed([1; 32]);
    let key = SpendingKey::from_seed([71; 32]);
    n.fund(&[&alice], 300 * FANG);
    on.fund(&[&alice], 300 * FANG);
    assert!(!crate::upgrade::is_active(&n.c.state, FEATURE));
    assert_eq!(status_json(&n.c.state, n.c.height())["active"], false);
    // Real txs, made on a chain where Ghost Mode is on, re-aimed at this one.
    let (shield, note) = on.shield(&alice, 10 * FANG, &key);
    on.submit(&shield).unwrap();
    on.c.produce_block().unwrap();
    let shield = Tx { chain_id: n.c.genesis.chain_id.clone(), nonce: n.c.next_nonce(&alice.address()), ..shield }.sign(&alice);
    let outputs = [on.output(10 * FANG - FEE, &key, &key), on.dummy_output()];
    let mut transfer = on.transfer(&key, note, outputs);
    transfer.chain_id = n.c.genesis.chain_id.clone();
    let address = Tx { chain_id: n.c.genesis.chain_id.clone(), from: alice.address(), nonce: 0, fee: 0, tip: 0, valid_until: n.c.height() + 100, action: Action::SetGhostAddress { address: HexBytes(key.address().to_bytes().to_vec()) }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: String::new() };
    let signed = |mut tx: Tx, n: &Net, k: &Keys| {
        tx.nonce = n.c.next_nonce(&tx.from);
        tx.signature = "00".repeat(crypto::SIGNATURE_LEN);
        tx.fee = min_fee(tx.size());
        tx.sign(k)
    };
    let address = signed(address, &n, &alice);
    let v0 = n.validators[0].clone_keys();
    let pause = signed(Tx { from: v0.address(), action: Action::GhostPause {}, public_key: hex::encode(v0.public_key()), ..address.clone() }, &n, &v0);
    let unshield = {
        let mut t = transfer.clone();
        t.action = Action::Unshield { to: alice.address(), amount: 1, bundle: Box::new(t.action.ghost_bundle().unwrap().clone()) };
        t
    };
    for tx in [&shield, &transfer, &unshield, &address, &pause] {
        let e = format!("{:#}", n.c.submit(tx.clone()).expect_err("refused"));
        assert!(e.contains("Ghost Mode is off"), "{}: {e}", tx.action.kind());
    }
    let all = vec![shield, transfer, unshield, address, pause];
    assert_eq!(n.block(all.clone()), vec![false; 5]);
    assert_eq!(n.c.last_block().unwrap().skipped.len(), 5);
    for tx in &all {
        assert!(n.c.failure(&tx.id()).unwrap().0.contains("Ghost Mode is off"));
    }
    let s = &n.c.state;
    assert_eq!((s.ghost.meta.pool, s.ghost.commitments.len(), s.ghost.nullifiers.len(), s.ghost.addresses.len()), (0, 0, 0, 0));
    n.cleanup();
    on.cleanup();
}

/// Size limits: a ghost tx over `ghost_max_tx_bytes` can't be included; the block budget holds
/// ghost txs to both limits and leaves other txs alone; the codec bounds every kind of tx.
#[test]
fn size_limits() {
    let mut n = net("size", true, 1, &[("ghost_max_tx_bytes", 64 << 10)]);
    let alice = Keys::from_seed([1; 32]);
    let key = SpendingKey::from_seed([81; 32]);
    n.fund(&[&alice], 300 * FANG);
    let (tx, _) = n.shield(&alice, 10 * FANG, &key);
    assert!(tx.size() > 64 << 10);
    assert!(err(n.submit(&tx)).contains("ghost_max_tx_bytes"));
    assert_eq!(n.block(vec![tx.clone()]), vec![false]);
    // The block budget, with the default limits and the shield's real size.
    let p = ChainParams { ghost_max_tx_bytes: 256 << 10, ghost_max_block_bytes: 300 << 10, ..Default::default() };
    let size = tx.to_bytes().len();
    let mut budget = BlockBudget::new(&p);
    budget.take(&tx, size).unwrap();
    budget.take(&tx, size).unwrap();
    assert!(budget.take(&tx, size).unwrap_err().to_string().contains("ghost_max_block_bytes"));
    let transfer = Tx { action: Action::Transfer { to: alice.address(), amount: 1 }, ..tx.clone() };
    budget.take(&transfer, 60_000).unwrap();
    assert!(BlockBudget::new(&p).take(&tx, (256 << 10) + 1).unwrap_err().to_string().contains("ghost_max_tx_bytes"));
    // The codec: ghost txs up to MAX_GHOST_TX_BYTES, others to MAX_TX_BYTES.
    let mut big = tx.clone();
    bundle_mut(&mut big).proof = HexBytes(vec![1; blockula_ghost::MAX_PROOF_BYTES + 1]);
    assert!(big.check_format().unwrap_err().to_string().contains("proof"));
    assert!(Tx::from_bytes(&big.to_bytes()).unwrap_err().to_string().contains("`proof` too long"));
    n.cleanup();
}

/// The state machine refuses what the stateless checks would (a block's txs go through it
/// again), and a proof made for another chain instance is refused before it's verified.
#[test]
fn proofs_are_bound_to_their_chain() {
    let mut n = net("instance", true, 1, &[]);
    let alice = Keys::from_seed([1; 32]);
    let key = SpendingKey::from_seed([91; 32]);
    n.fund(&[&alice], 300 * FANG);
    let (tx, _) = n.shield(&alice, 10 * FANG, &key);
    // Valid on its chain …
    assert!(verify_proof(&tx).is_ok());
    // … re-aimed at another instance (same chain id): refused before verification.
    let mut other = tx.clone();
    bundle_mut(&mut other).instance = HexBytes(vec![7; 32]);
    let other = other.sign(&alice);
    assert!(verify_proof(&other).is_err(), "the instance is in the sighash");
    let mut probe = n.c.state.clone();
    let e = probe.apply(&other, &n.c.genesis.chain_id, "x", n.c.height() + 1).unwrap_err();
    assert!(format!("{e:#}").contains("made for another chain"), "{e:#}");
    // And the chain's instance is what /api/ghost serves wallets.
    assert_eq!(status_json(&n.c.state, n.c.height())["instance"], hex::encode(n.instance()));
    assert_eq!(n.instance(), chain_instance(&n.c.genesis));
    n.cleanup();
}

/// The node's release profile keeps overflow checks on in blockula-ghost and every Plonky3 crate
/// (what the verifier's robustness evidence was gathered with, GHOST-MODE.md section 7), and
/// never aborts on panic (`verify` catches verifier panics).
#[test]
fn release_profile_covers_plonky3() {
    let manifest = include_str!("../../Cargo.toml");
    let lock = include_str!("../../Cargo.lock");
    let crates: BTreeSet<&str> = lock.lines().filter_map(|l| l.strip_prefix("name = \"")).map(|l| l.trim_end_matches('"')).filter(|n| n.starts_with("p3-") || *n == "blockula-ghost").collect();
    assert!(crates.len() > 10 && crates.contains("p3-uni-stark") && crates.contains("blockula-ghost"));
    for c in crates {
        assert!(manifest.contains(&format!("[profile.release.package.{c}]\noverflow-checks = true")), "node/Cargo.toml needs [profile.release.package.{c}] overflow-checks = true");
    }
    assert!(!manifest.contains("panic = \"abort\""), "blockula-ghost needs panic = \"unwind\"");
}
