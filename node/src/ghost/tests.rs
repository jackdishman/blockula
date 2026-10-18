//! Ghost Mode through the node: real proofs (the `blockula-ghost` prover, as a wallet would run
//! it), real txs through the mempool and blocks. Every test block also checks the incremental
//! root against a rebuild and the invariants (pool included) in `State::debug_check`.

use super::*;
use crate::chain::{now_ms, Chain, SummonOpts, MAX_MEMPOOL, MAX_PENDING_PER_SENDER};
use crate::crypto::{self, Keys};
use blockula_ghost::encryption::MEMO_BYTES;
use blockula_ghost::field::Digest;
use blockula_ghost::{dummy_output, encrypt_note, ActionWitness, EncryptedNote, FullTree, Note, SpendWitness, SpendingKey, TrialDecryptor};
use std::path::PathBuf;

const FANG: u64 = DROPS_PER_FANG;
/// An unsigned tx's fee in these tests: the base fee of a tx well above a real one's size, plus
/// the default verify fee (the excess is burned), on the fee grid (`FEE_GRID`).
pub(crate) const FEE: u64 = 4 * FEE_GRID;
const _: () = assert!(FEE >= 10_000 + 250_000 * 10 + FANG / 1_000 && FEE.is_multiple_of(FEE_GRID));

/// Deterministic randomness for test wallets (BLAKE3's XOF). Provers draw their masks from the
/// OS regardless.
pub(crate) struct TestRng(blake3::OutputReader);

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

/// A test wallet: it builds and proves ghost txs against a chain's current state, the way a real
/// wallet would (rebuilding the commitment tree from the chain's records).
pub(crate) struct Wallet {
    pub(crate) rng: TestRng,
}

impl Wallet {
    pub(crate) fn new(seed: &str) -> Self {
        Wallet { rng: TestRng::new(seed) }
    }

    pub(crate) fn output(&mut self, value: u64, to: &SpendingKey, from: &SpendingKey) -> (Note, EncryptedNote) {
        let note = Note { value, owner: to.address().owner(), rseed: self.rng.bytes() };
        let enc = encrypt_note(&note, &to.address(), &[0; MEMO_BYTES], Some(&from.full_viewing_key().outgoing()), &mut self.rng).unwrap();
        (note, enc)
    }

    pub(crate) fn dummy_spend(&mut self) -> SpendWitness {
        SpendWitness::dummy(&mut self.rng)
    }

    pub(crate) fn dummy_output(&mut self) -> (Note, EncryptedNote) {
        dummy_output(&mut self.rng)
    }

    /// A shield of `amount` from `from` (its next nonce) to the ghost key `to`, proved and signed.
    /// Returns the note.
    pub(crate) fn shield(&mut self, c: &Chain, from: &Keys, amount: u64, to: &SpendingKey) -> (Tx, Note) {
        let out = self.output(amount, to, to);
        let note = out.0;
        let act = Act { spends: [self.dummy_spend(), self.dummy_spend()], outputs: [out, self.dummy_output()], anchor: anchor(c) };
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: from.address(), nonce: c.next_nonce(&from.address()), fee: FEE, tip: 0, valid_until: c.height() + 100, action: Action::Shield { amount, bundle: Box::new(act.bundle(instance(c))) }, memo: String::new(), public_key: hex::encode(from.public_key()), signature: String::new() };
        tx = act.prove(tx);
        tx.signature = "00".repeat(crypto::SIGNATURE_LEN);
        (tx.sign(from), note)
    }

    /// A private transfer spending `note` (of `key`) into `outputs`, with fee `FEE`.
    pub(crate) fn transfer(&mut self, c: &Chain, key: &SpendingKey, note: Note, outputs: [(Note, EncryptedNote); 2]) -> Tx {
        let act = Act { spends: [spend(c, key, note), self.dummy_spend()], outputs, anchor: anchor(c) };
        let tx = unsigned(c, Action::PrivateTransfer { bundle: Box::new(act.bundle(instance(c))) }, FEE, 0);
        act.prove(tx)
    }

    /// An unshield of `amount` to `to` from `note` (of `key`), the rest back to `key`, with fee
    /// `FEE`, plus `account_creation_fee` if `to` has no account yet (as a wallet prices it).
    pub(crate) fn unshield(&mut self, c: &Chain, key: &SpendingKey, note: Note, to: &str, amount: u64) -> Tx {
        let fee = FEE + c.state.creation_fee_for(to);
        let change = self.output(note.value - amount - fee, key, key);
        let act = Act { spends: [spend(c, key, note), self.dummy_spend()], outputs: [change, self.dummy_output()], anchor: anchor(c) };
        let tx = unsigned(c, Action::Unshield { to: to.into(), amount, bundle: Box::new(act.bundle(instance(c))) }, fee, 0);
        act.prove(tx)
    }
}

fn instance(c: &Chain) -> [u8; 32] {
    c.state.ghost.meta.instance
}

/// The chain's current anchor.
fn anchor(c: &Chain) -> Digest {
    digest_from_bytes(&c.state.ghost.meta.root).unwrap()
}

/// A wallet's view of the commitment tree, rebuilt from the state's commitments.
fn tree(c: &Chain) -> FullTree {
    let mut t = FullTree::new();
    for (pos, cm) in c.state.ghost.commitments.iter() {
        assert_eq!(u64::from(t.append(digest_from_bytes(cm).unwrap()).unwrap()), *pos);
    }
    assert_eq!(t.root(), anchor(c), "the wallet's tree and the chain's anchor agree");
    t
}

/// A spend of `note` (owned by `key`) at its position in the tree.
fn spend(c: &Chain, key: &SpendingKey, note: Note) -> SpendWitness {
    let t = tree(c);
    let cm = digest_to_bytes(&note.commitment());
    let pos = c.state.ghost.commitments.iter().find(|(_, x)| **x == cm).map(|(p, _)| *p as u32).expect("the note is in the tree");
    SpendWitness::new(key, note, t.path(pos).unwrap())
}

/// An unsigned tx around a ghost action whose proof is still empty, valid for 100 blocks.
fn unsigned(c: &Chain, action: Action, fee: u64, tip: u64) -> Tx {
    Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: String::new(), nonce: 0, fee, tip, valid_until: c.height() + 100, action, memo: String::new(), public_key: String::new(), signature: String::new() }
}

/// A test chain (this node is its first validator) and a wallet.
struct Net {
    root: PathBuf,
    home: PathBuf,
    c: Chain,
    validators: Vec<Keys>,
    w: Wallet,
}

/// Turns Ghost Mode on in a summoned chain's genesis files (and sets `params`).
pub(crate) fn enable_in_genesis(files: &[PathBuf], params: &[(&str, u64)]) {
    for f in files {
        let mut g: serde_json::Value = serde_json::from_slice(&std::fs::read(f).unwrap()).unwrap();
        g["features"] = json!([FEATURE]);
        for (k, v) in params {
            g["params"][*k] = json!(v.to_string());
        }
        std::fs::write(f, serde_json::to_vec_pretty(&g).unwrap()).unwrap();
    }
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
    if on {
        let files: Vec<PathBuf> = homes.iter().map(|h| h.join("genesis.json")).chain((validators > 1).then(|| root.join("genesis.json"))).collect();
        enable_in_genesis(&files, &[]);
    }
    let validators = homes.iter().map(|h| Keys::from_seed(hex::decode(std::fs::read_to_string(h.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap())).collect();
    let c = Chain::open(&homes[0]).unwrap();
    Net { root, home: homes[0].clone(), c, validators, w: Wallet::new(tag) }
}

impl Net {
    fn cleanup(self) {
        let root = self.root.clone();
        drop(self);
        std::fs::remove_dir_all(root).ok();
    }

    /// Closes the chain and opens its home again.
    fn reopen(self) -> Net {
        let Net { root, home, c, validators, w } = self;
        drop(c);
        let c = Chain::open(&home).unwrap();
        Net { root, home, c, validators, w }
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
        crate::consensus::check_tx_stateless(tx, &self.c.genesis.chain_id, &self.c.genesis_hash, &self.c.genesis.address_prefix)?;
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
        instance(&self.c)
    }

    fn anchor(&self) -> Digest {
        anchor(&self.c)
    }

    fn spend(&self, key: &SpendingKey, note: Note) -> SpendWitness {
        spend(&self.c, key, note)
    }

    fn output(&mut self, value: u64, to: &SpendingKey, from: &SpendingKey) -> (Note, EncryptedNote) {
        self.w.output(value, to, from)
    }

    fn dummy_spend(&mut self) -> SpendWitness {
        self.w.dummy_spend()
    }

    fn dummy_output(&mut self) -> (Note, EncryptedNote) {
        self.w.dummy_output()
    }

    fn shield(&mut self, from: &Keys, amount: u64, to: &SpendingKey) -> (Tx, Note) {
        self.w.shield(&self.c, from, amount, to)
    }

    fn unsigned(&self, action: Action, fee: u64, tip: u64) -> Tx {
        unsigned(&self.c, action, fee, tip)
    }

    fn transfer(&mut self, key: &SpendingKey, note: Note, outputs: [(Note, EncryptedNote); 2]) -> Tx {
        self.w.transfer(&self.c, key, note, outputs)
    }

    fn unshield(&mut self, key: &SpendingKey, note: Note, to: &str, amount: u64) -> Tx {
        self.w.unshield(&self.c, key, note, to, amount)
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

/// A private transfer that passes every stateless check on chain `chain_id` (signature-free,
/// its proof valid), spending a note in a tree of its own (anchors are stateful): for tests of
/// paths that only look at txs statelessly, like consensus `verify`.
pub(crate) fn stateless_transfer(chain_id: &str, genesis_hash: &str, instance: [u8; 32], seed: &str, valid_until: u64) -> Tx {
    let mut rng = TestRng::new(seed);
    let key = SpendingKey::from_seed(rng.bytes());
    let note = Note { value: 10 * FANG, owner: key.address().owner(), rseed: rng.bytes() };
    let mut tree = FullTree::new();
    let pos = tree.append(note.commitment()).unwrap();
    let change = Note { value: 10 * FANG - FEE, owner: key.address().owner(), rseed: rng.bytes() };
    let enc = encrypt_note(&change, &key.address(), &[0; MEMO_BYTES], None, &mut rng).unwrap();
    let act = Act { spends: [SpendWitness::new(&key, note, tree.path(pos).unwrap()), SpendWitness::dummy(&mut rng)], outputs: [(change, enc), dummy_output(&mut rng)], anchor: tree.root() };
    let tx = Tx { chain_id: chain_id.into(), genesis_hash: genesis_hash.into(), from: String::new(), nonce: 0, fee: FEE, tip: 0, valid_until, action: Action::PrivateTransfer { bundle: Box::new(act.bundle(instance)) }, memo: String::new(), public_key: String::new(), signature: String::new() };
    act.prove(tx)
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

    // Bob publishes his ghost address on his account; wallets find it by any of its names.
    let bob = Keys::from_seed([2; 32]);
    n.fund(&[&bob], 10 * FANG);
    let mut publish = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: bob.address(), nonce: 0, fee: 0, tip: 0, valid_until: n.c.height() + 100, action: Action::SetGhostAddress { address: HexBytes(ghost_bob.address().to_bytes().to_vec()) }, memo: String::new(), public_key: hex::encode(bob.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    publish.fee = min_fee(publish.size());
    n.submit(&publish.sign(&bob)).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.ghost.addresses.get(&bob.address()), Some(&ghost_bob.address().to_bytes().to_vec()));
    assert!(n.c.describe_last(0).contains(&ghost_bob.address().fingerprint()));
    // Session keys can't shield (or spend shielded funds: that needs the spending key).
    let agent = Keys::from_seed([4; 32]);
    let mut add = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: alice.address(), nonce: n.c.next_nonce(&alice.address()), fee: 0, tip: 0, valid_until: n.c.height() + 100, action: crate::types::add_key(&agent, &n.c.genesis.chain_id, &n.c.genesis_hash, &alice.address(), "agent", KeyRole::Session { limit: 50 * FANG, period_blocks: 100, expires_at: 10_000 }), memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    add.fee = min_fee(add.size());
    n.submit(&add.sign(&alice)).unwrap();
    n.c.produce_block().unwrap();
    let (by_agent, _) = n.shield(&alice, FANG, &ghost_alice);
    let by_agent = Tx { public_key: hex::encode(agent.public_key()), ..by_agent }.sign(&agent);
    assert!(err(n.submit(&by_agent)).contains("session keys can only send transfers"));

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
    // The API shows it without its proof (size and hash instead), ciphertexts included.
    let shown = action_json(&tx.action).unwrap();
    let proof_len = tx.action.ghost_bundle().unwrap().proof.0.len();
    assert_eq!((shown["type"].as_str(), shown["bundle"]["proof_bytes"].as_u64()), (Some("private_transfer"), Some(proof_len as u64)));
    assert!(shown["bundle"].get("proof").is_none() && shown["bundle"]["enc"][0].as_str().map(str::len) == Some(2 * GHOST_ENC_BYTES));
    assert!(serde_json::to_vec(&shown).unwrap().len() < 20_000, "not the ~260 KB of a hex proof");
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

    // 3. Bob unshields 50 FANG to Carol's transparent account (a new account). Opening it burns
    // `account_creation_fee` like any other account's (DEC-6, audit F13): the fee must cover it
    // too, or the unshield can't be included.
    let open = n.c.state.chain_params.account_creation_fee;
    assert!(open > 0 && n.c.state.creation_fee_for(&carol.address()) == open);
    let cheap = {
        let change = n.output(60 * FANG - 50 * FANG - FEE, &ghost_bob, &ghost_bob);
        let act = Act { spends: [n.spend(&ghost_bob, note_b), n.dummy_spend()], outputs: [change, n.dummy_output()], anchor: n.anchor() };
        let tx = n.unsigned(Action::Unshield { to: carol.address(), amount: 50 * FANG, bundle: Box::new(act.bundle(n.instance())) }, FEE, 0);
        act.prove(tx)
    };
    assert!(err(n.submit(&cheap)).contains("account_creation_fee"));
    assert_eq!(n.block(vec![cheap]), vec![false], "can't be included either");
    let tx = n.unshield(&ghost_bob, note_b, &carol.address(), 50 * FANG);
    assert_eq!(tx.fee, FEE + open);
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
    assert_eq!(s.ghost.meta.pool, 100 * FANG - FEE - 50 * FANG - (FEE + open), "the pool holds exactly the live notes: Alice's and Bob's change");
    assert_eq!(s.ghost.meta.pool, (40 * FANG - FEE) + (10 * FANG - FEE - open));
    assert_eq!(s.supply, supply - FEE - open, "the fee is burned, the new account's creation fee with it");
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
    assert_eq!(n.c.state.ghost.meta.pool, pool - 10 * FANG - FEE - n.c.state.chain_params.account_creation_fee, "it opened Carol's account");
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
    // A replacement pays at least the relay of its own bytes on top of the queued tip (audit
    // F39): one grid step (0.001 FANG) is less than a ~135 KiB tx's base fee, two clear it.
    let tipped = |n: &mut Net, amount: u64, tip: u64| {
        let change = n.output(100 * FANG - amount - FEE - tip, &key, &key);
        let act = Act { spends: [n.spend(&key, note), n.dummy_spend()], outputs: [change, n.dummy_output()], anchor: n.anchor() };
        let tx = n.unsigned(Action::Unshield { to: carol.clone(), amount, bundle: Box::new(act.bundle(n.instance())) }, FEE + tip, tip);
        act.prove(tx)
    };
    let relay = crate::econ::base_fee(&n.c.state, n.c.height() + 1, low.size());
    assert!(FEE_GRID < relay && relay <= 2 * FEE_GRID, "{relay}");
    let one_step = tipped(&mut n, 12 * FANG, FEE_GRID);
    assert!(err(n.submit(&one_step)).contains(&format!("a tip of at least {}", 2 * FEE_GRID)));
    // Off the fee grid: refused whatever the tip (audit F38).
    let off_grid = tipped(&mut n, 12 * FANG, 2 * FEE_GRID + 1);
    assert!(err(n.submit(&off_grid)).contains("whole multiples"));
    let act_tip = tipped(&mut n, 12 * FANG, 2 * FEE_GRID);
    n.submit(&act_tip).unwrap();
    assert!(!n.c.in_mempool(&low.id()) && n.c.in_mempool(&act_tip.id()));
    assert!(n.c.failure(&low.id()).unwrap().0.contains("replaced"));
    // Replacing that one again costs its tip plus another relay.
    let again = tipped(&mut n, 13 * FANG, 3 * FEE_GRID);
    assert!(err(n.submit(&again)).contains(&format!("a tip of at least {}", 4 * FEE_GRID)));
    let proposer = n.c.me().unwrap();
    let before = n.c.state.account(&proposer).balance;
    n.c.produce_block().unwrap();
    assert_eq!(n.c.state.account(&proposer).balance, before + 2 * FEE_GRID, "the tip goes to the proposer");
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
    let fake = Note { value: 500 * FANG, owner: thief.address().owner(), rseed: n.w.rng.bytes() };
    let mut elsewhere = FullTree::new();
    let pos = elsewhere.append(fake.commitment()).unwrap();
    let fake_root = digest_to_bytes(&elsewhere.root());
    n.c.state.ghost.roots.insert(fake_root, u64::MAX);
    n.c.state.ghost.retired.insert(u64::MAX, fake_root);
    let spend = || SpendWitness::new(&thief, fake, elsewhere.path(pos).unwrap());
    // Carol has no account: the unshield's fee covers opening it.
    let fee = FEE + n.c.state.chain_params.account_creation_fee;
    let carry = |n: &mut Net, amount: u64| {
        let change = n.output(500 * FANG - amount - fee, &thief, &thief);
        let act = Act { spends: [spend(), n.dummy_spend()], outputs: [change, n.dummy_output()], anchor: elsewhere.root() };
        let tx = n.unsigned(Action::Unshield { to: carol.clone(), amount, bundle: Box::new(act.bundle(n.instance())) }, fee, 0);
        act.prove(tx)
    };
    let (pool, supply) = (n.c.state.ghost.meta.pool, n.c.state.supply);
    let greedy = carry(&mut n, 400 * FANG);
    assert!(crate::ghost::verify_proof(&greedy).is_ok(), "the proof itself is valid");
    assert!(err(n.submit(&greedy)).contains("turnstile"));
    assert_eq!(n.block(vec![greedy.clone()]), vec![false]);
    assert!(n.c.failure(&greedy.id()).unwrap().0.contains("turnstile"));
    // Nothing minted, nothing moved: the only change is the proposer's charge for carrying a tx
    // that couldn't be included (CV-5), burned.
    let junk = crate::econ::base_fee(&n.c.state, n.c.height(), greedy.size());
    assert_eq!((n.c.state.ghost.meta.pool, n.c.state.supply), (pool, supply - junk), "nothing minted, nothing moved");
    assert_eq!(n.c.state.account(&carol).balance, 0);
    // A private transfer whose fee alone is more than the pool holds is stopped the same way.
    let over = pool + FEE_GRID;
    let outputs = [n.output(500 * FANG - over, &thief, &thief), n.dummy_output()];
    let act = Act { spends: [spend(), n.dummy_spend()], outputs, anchor: elsewhere.root() };
    let tx = n.unsigned(Action::PrivateTransfer { bundle: Box::new(act.bundle(n.instance())) }, over, 0);
    let tx = act.prove(tx);
    assert!(err(n.submit(&tx)).contains("turnstile"));
    // Within the pool, the counterfeit does take honest deposits: what the turnstile bounds
    // (and the pool cap with it) is the damage, not the bug.
    let modest = carry(&mut n, pool - fee);
    n.submit(&modest).unwrap();
    n.c.produce_block().unwrap();
    assert_eq!((n.c.state.ghost.meta.pool, n.c.state.account(&carol).balance), (0, pool - fee));
    assert_eq!(n.c.state.supply, supply - junk - fee, "still nothing minted");
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
        let mut tx = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: k.address(), nonce: n.c.next_nonce(&k.address()), fee: 0, tip: 0, valid_until: n.c.height() + 100, action: Action::GhostPause {}, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
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
    let shield = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), nonce: n.c.next_nonce(&alice.address()), ..shield }.sign(&alice);
    let outputs = [on.output(10 * FANG - FEE, &key, &key), on.dummy_output()];
    let mut transfer = on.transfer(&key, note, outputs);
    (transfer.chain_id, transfer.genesis_hash) = (n.c.genesis.chain_id.clone(), n.c.genesis_hash.clone());
    let address = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: alice.address(), nonce: 0, fee: 0, tip: 0, valid_until: n.c.height() + 100, action: Action::SetGhostAddress { address: HexBytes(key.address().to_bytes().to_vec()) }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: String::new() };
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
    let p = crate::params::ChainParams { ghost_max_tx_bytes: 256 << 10, ghost_max_block_bytes: 300 << 10, ..Default::default() };
    let size = tx.to_bytes().len();
    let slot = crate::econ::FeeSlot::floor(&p);
    let mut budget = BlockBudget::new(Some(&slot));
    budget.take(&tx, size).unwrap();
    budget.take(&tx, size).unwrap();
    assert!(budget.take(&tx, size).unwrap_err().to_string().contains("ghost_max_block_bytes"));
    let transfer = Tx { action: Action::Transfer { to: alice.address(), amount: 1 }, ..tx.clone() };
    budget.take(&transfer, 60_000).unwrap();
    assert!(BlockBudget::new(Some(&slot)).take(&tx, (256 << 10) + 1).unwrap_err().to_string().contains("ghost_max_tx_bytes"));
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

/// The mempool holds at most `MAX_MEMPOOL_GHOST` ghost txs, and one queued spend per note. A
/// spend of the same note replaces the queued one, and a full ghost share gives up its cheapest
/// tx (audit F36: a higher tip gets in), only for a tip higher by a tenth and by the relay of the
/// newcomer's bytes (audit F39: every displacement pays for itself).
#[test]
fn mempool_bounds_ghost_txs() {
    let mk = |i: u32, tip: u64| {
        let mut b = crate::types::tests::vector_bundle(1);
        b.nullifiers = [HexBytes([i.to_le_bytes(), [1; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4]].concat()), HexBytes([i.to_le_bytes(), [2; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4]].concat())];
        Tx { chain_id: "c".into(), genesis_hash: "00".repeat(32), from: String::new(), nonce: 0, fee: FEE + tip, tip, valid_until: 9, action: Action::PrivateTransfer { bundle: b }, memo: String::new(), public_key: String::new(), signature: String::new() }
    };
    let relay = FEE_GRID + FEE_GRID / 2;
    let mut queued: Vec<Tx> = (0..MAX_MEMPOOL_GHOST as u32).map(|i| mk(i, 0)).collect();
    queued[3] = mk(3, 5 * FEE_GRID);
    let ids: Vec<String> = queued.iter().map(Tx::id).collect();
    let admit = |tx: &Tx, evict: bool| mempool_admit(&queued, &ids, tx, None, relay, u64::MAX, evict);
    assert!(mempool_admit(&queued[..10], &ids[..10], &mk(999, 0), None, relay, u64::MAX, true).unwrap().is_empty(), "room, no conflict");
    // Full: a tip that doesn't clear the cheapest queued tx's plus the relay is refused…
    for tip in [0, FEE_GRID] {
        let e = admit(&mk(999, tip), true).unwrap_err().to_string();
        assert!(e.contains("maximum") && e.contains(&format!("a tip of at least {}", 2 * FEE_GRID)), "{e}");
    }
    // …one that does evicts the newest of the cheapest (only where eviction is allowed).
    assert_eq!(admit(&mk(999, 2 * FEE_GRID), true).unwrap(), vec![(ids[MAX_MEMPOOL_GHOST - 1].clone(), "evicted from the mempool's full ghost share by a transaction with a higher tip")]);
    assert!(admit(&mk(999, 2 * FEE_GRID), false).unwrap_err().to_string().contains("maximum"));
    // The same note: replaced only past the bump (its tip + max(a tenth, the relay)).
    assert!(admit(&mk(7, 0), true).unwrap_err().to_string().contains("already spends"));
    assert!(admit(&mk(7, FEE_GRID), true).unwrap_err().to_string().contains(&format!("a tip of at least {}", 2 * FEE_GRID)));
    assert_eq!(admit(&mk(7, 2 * FEE_GRID), true).unwrap(), vec![(ids[7].clone(), "replaced by a transaction spending the same note with a higher tip")]);
    assert!(admit(&mk(3, 6 * FEE_GRID), true).unwrap_err().to_string().contains(&format!("a tip of at least {}", 7 * FEE_GRID)), "5 + max(0.5, 1.5) rounds up to 7");
    assert_eq!(admit(&mk(3, 7 * FEE_GRID), true).unwrap().len(), 1);
    let transfer = Tx { action: Action::GhostPause {}, ..mk(1, 0) };
    assert!(admit(&transfer, true).unwrap().is_empty(), "other txs aren't counted");
}

/// Review of F36: a full ghost share never gives up the newcomer's own tx (it would sit behind
/// the gap it made), a sender can't replace its own queued tx by note (its nonce is taken), and
/// the refusal never asks for a tip past the newcomer's cap. A signed `Shield` with its sender's
/// later txs behind it (the node never queues any: `behind_shield`) goes with them, and the
/// newcomer outbids them together (round 3: round 2 kept such a shield, which let shields pin
/// the share).
#[test]
fn the_ghost_share_gives_up_others_txs_with_what_waits_behind_them() {
    let nf = |i: u32, k: u8| HexBytes([i.to_le_bytes(), [k; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4]].concat());
    let unsigned = |i: u32, tip: u64| {
        let mut b = crate::types::tests::vector_bundle(1);
        b.nullifiers = [nf(i, 1), nf(i, 2)];
        Tx { chain_id: "c".into(), genesis_hash: "00".repeat(32), from: String::new(), nonce: 0, fee: FEE + tip, tip, valid_until: 9, action: Action::PrivateTransfer { bundle: b }, memo: String::new(), public_key: String::new(), signature: String::new() }
    };
    let shield = |i: u32, from: &str, nonce: u64, tip: u64| {
        let mut b = crate::types::tests::vector_bundle(1);
        b.nullifiers = [nf(i, 1), nf(i, 2)];
        Tx { from: from.into(), nonce, public_key: "01".repeat(32), signature: "02".repeat(32), action: Action::Shield { amount: FANG, bundle: b }, ..unsigned(i, tip) }
    };
    let relay = FEE_GRID + FEE_GRID / 2;
    let ids = |q: &[Tx]| q.iter().map(Tx::id).collect::<Vec<_>>();
    // Alice's shield is the cheapest ghost tx queued, with her transfer behind it: they go
    // together, for a tip that outbids them both.
    let transfer = |tip: u64| Tx { action: Action::Transfer { to: "boo1bob".into(), amount: 1 }, ..shield(0, "boo1alice", 1, tip) };
    let mut queued = vec![shield(0, "boo1alice", 0, 0), transfer(0)];
    queued.extend((1..MAX_MEMPOOL_GHOST as u32).map(|i| unsigned(i, FEE_GRID)));
    let qids = ids(&queued);
    let got = mempool_admit(&queued, &qids, &unsigned(999, 3 * FEE_GRID), None, relay, u64::MAX, true).unwrap();
    assert_eq!(got, vec![(qids[0].clone(), "evicted from the mempool's full ghost share by a transaction with a higher tip")]);
    // Her transfer tipping 2 steps makes the two together pricier per byte than a tx tipping 1:
    // the newest of those goes instead.
    queued[1] = transfer(2 * FEE_GRID);
    let qids = ids(&queued);
    let got = mempool_admit(&queued, &qids, &unsigned(999, 3 * FEE_GRID), None, relay, u64::MAX, true).unwrap();
    assert_eq!(got[0].0, qids[MAX_MEMPOOL_GHOST], "the newest of the cheapest");
    // Carol's lone shield is the cheapest: another sender's tx can displace it, her own next
    // one can't (it would sit behind the gap it made).
    let mut queued = vec![shield(0, "boo1carol", 0, 0)];
    queued.extend((1..MAX_MEMPOOL_GHOST as u32).map(|i| unsigned(i, FEE_GRID)));
    let qids = ids(&queued);
    let admit = |tx: &Tx| mempool_admit(&queued, &qids, tx, None, relay, u64::MAX, true);
    assert_eq!(admit(&shield(900, "boo1dave", 0, 2 * FEE_GRID)).unwrap()[0].0, qids[0]);
    assert_eq!(admit(&shield(901, "boo1carol", 1, 3 * FEE_GRID)).unwrap()[0].0, qids[MAX_MEMPOOL_GHOST - 1]);
    // Her own queued shield's notes: not hers to replace (but the shield it is, `own`, is left out).
    assert!(admit(&shield(0, "boo1carol", 1, 9 * FEE_GRID)).unwrap_err().to_string().contains("your queued transaction with nonce 0 already spends"));
    assert!(mempool_admit(&queued, &qids, &shield(0, "boo1carol", 0, 9 * FEE_GRID), Some(0), relay, u64::MAX, true).unwrap().is_empty(), "replacing her shield frees its slot");
    // A tip that would have to pass the cap isn't asked for (an unsigned tx's cap, on the grid).
    let e = mempool_admit(&queued, &qids, &unsigned(999, 0), None, relay, FEE_GRID, true).unwrap_err().to_string();
    assert!(e.contains("they tip more than this one may (at most 1000000 drops)"), "{e}");
    let e = mempool_admit(&queued, &qids, &unsigned(999, 0), None, relay, 2 * FEE_GRID - 1, true).unwrap_err().to_string();
    assert!(e.contains("they tip more than this one may (at most 1000000 drops)"), "{e}");
    let e = mempool_admit(&queued, &qids, &unsigned(999, 0), None, relay, 2 * FEE_GRID, true).unwrap_err().to_string();
    assert!(e.contains("a tip of at least 2000000 drops"), "{e}");
}

/// Review of F36, round 3 (the round-2 reviewer's `r1_scratch_pinned_shields_lock_the_ghost_share_regardless_of_tip`,
/// at the level of `mempool_admit`): 256 untipped shields, each with a tx of its sender queued
/// behind it, used to leave nothing the full share could give up, so an honest ghost tx was
/// refused at any tip. The node never queues a tx behind a shield (the next test), but a share
/// fed such a queue still gives a covered shield up, together with the txs behind it (they'd
/// wait on a nonce gap: `Chain::displace` drops them), for a tip that outbids them all.
#[test]
fn pinned_shields_dont_lock_the_ghost_share() {
    let nf = |i: u32, k: u8| HexBytes([i.to_le_bytes(), [k; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4]].concat());
    let unsigned = |i: u32, tip: u64| {
        let mut b = crate::types::tests::vector_bundle(1);
        b.nullifiers = [nf(i, 1), nf(i, 2)];
        Tx { chain_id: "c".into(), genesis_hash: "00".repeat(32), from: String::new(), nonce: 0, fee: FEE + tip, tip, valid_until: 9, action: Action::PrivateTransfer { bundle: b }, memo: String::new(), public_key: String::new(), signature: String::new() }
    };
    let shield = |i: u32, from: &str, nonce: u64, tip: u64| {
        let mut b = crate::types::tests::vector_bundle(1);
        b.nullifiers = [nf(i, 1), nf(i, 2)];
        Tx { from: from.into(), nonce, public_key: "01".repeat(32), signature: "02".repeat(32), action: Action::Shield { amount: FANG, bundle: b }, ..unsigned(i, tip) }
    };
    let transfer = |from: &str, nonce: u64, tip: u64| Tx { action: Action::Transfer { to: "boo1bob".into(), amount: 1 }, fee: 20_000 + tip, ..shield(0, from, nonce, tip) };
    let relay = FEE_GRID + FEE_GRID / 2;
    let mut queued = vec![];
    for i in 0..MAX_MEMPOOL_GHOST as u32 {
        let from = format!("boo1attacker{i}");
        queued.push(shield(i, &from, 0, 0));
        queued.push(transfer(&from, 1, 0));
    }
    let ids: Vec<String> = queued.iter().map(Tx::id).collect();
    let admit = |tx: &Tx| mempool_admit(&queued, &ids, tx, None, relay, 28 * FEE_GRID, true);
    let newest = ids[ids.len() - 2].clone();
    for tip in [3 * FEE_GRID, 28 * FEE_GRID] {
        assert_eq!(admit(&unsigned(999, tip)).unwrap(), vec![(newest.clone(), "evicted from the mempool's full ghost share by a transaction with a higher tip")], "an unsigned tx tipping {tip}");
        assert_eq!(admit(&shield(999, "boo1honest", 0, tip)).unwrap()[0].0, newest, "a shield tipping {tip}");
    }
    // Below the shield's tip plus the relay of the newcomer's bytes: the hint names the tip that gets in.
    let e = admit(&unsigned(999, FEE_GRID)).unwrap_err().to_string();
    assert!(e.contains(&format!("a tip of at least {}", 2 * FEE_GRID)), "{e}");
    // What goes with a covered shield is outbid too: its sender's later txs' tips count.
    let mut queued2 = queued.clone();
    let last = queued2.len() - 1;
    queued2[last] = transfer("boo1attacker255", 1, 5 * FEE_GRID);
    queued2.iter_mut().filter(|t| matches!(t.action, Action::Shield { .. })).for_each(|t| t.tip = 9 * FEE_GRID);
    queued2[last - 1].tip = 0;
    let ids2: Vec<String> = queued2.iter().map(Tx::id).collect();
    let admit2 = |tx: &Tx| mempool_admit(&queued2, &ids2, tx, None, relay, 28 * FEE_GRID, true);
    let e = admit2(&unsigned(999, 6 * FEE_GRID)).unwrap_err().to_string();
    assert!(e.contains(&format!("a tip of at least {}", 7 * FEE_GRID)), "0 + 5 + the relay, on the grid: {e}");
    assert_eq!(admit2(&unsigned(999, 7 * FEE_GRID)).unwrap()[0].0, ids2[last - 1]);
}

/// Remembers `tx`'s proof as valid: for tests of the mempool, not of proofs.
fn assume_valid(tx: &Tx) {
    let (public, proof) = statement(tx).unwrap().unwrap();
    VERDICTS.lock().put(statement_key(&public, proof), Ok(()));
}

/// A signed shield of 1 FANG from `from` at `nonce` tipping `tip`, with a stand-in proof of a
/// real one's size (so its tip cap is a real one's too) remembered as valid.
fn stand_in_shield(n: &mut Net, from: &Keys, nonce: u64, tip: u64) -> Tx {
    let to = SpendingKey::from_seed(n.w.rng.bytes());
    let out = n.output(FANG, &to, &to);
    let act = Act { spends: [n.dummy_spend(), n.dummy_spend()], outputs: [out, n.dummy_output()], anchor: n.anchor() };
    let mut tx = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: from.address(), nonce, fee: 0, tip, valid_until: n.c.height() + 100, action: Action::Shield { amount: FANG, bundle: Box::new(act.bundle(n.instance())) }, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    let mut proof = n.w.rng.bytes().to_vec();
    proof.resize(130 << 10, 7);
    set_proof(&mut tx, proof);
    tx.fee = 2 * crate::econ::base_fee(&n.c.state, n.c.height() + 1, tx.size()) + tip;
    let tx = tx.sign(from);
    assume_valid(&tx);
    tx
}

/// Review of F36, through the node: Alice queues a shield (untipped) and then a transfer, and
/// the ghost share fills with others' shields. A shield that outbids the cheapest used to evict
/// Alice's, leaving her transfer behind a nonce gap: her resubmissions refused, and the transfer
/// dropped a block later. Round 2 kept such a shield (which let shields pin the share); now her
/// transfer can't queue behind her shield at all, so a shield the share gives up is always its
/// sender's last tx: a transfer queued before it stays, and nobody is left behind a gap.
#[test]
fn a_full_ghost_share_never_leaves_a_sender_behind_a_nonce_gap() {
    let mut n = net("gap", true, 1, &[]);
    let accounts: Vec<Keys> = (0..MAX_MEMPOOL_GHOST as u32 + 1).map(|i| Keys::from_seed(crypto::hash(&(i + 7_000).to_le_bytes()))).collect();
    for k in &accounts {
        n.c.state.accounts.insert(k.address(), crate::chain::Account { balance: 10 * FANG, ..Default::default() });
        n.c.state.supply += 10 * FANG;
    }
    let (alice, bob, others) = (&accounts[0], &accounts[1], &accounts[2..]);
    let transfer = |n: &Net, nonce: u64| {
        let mut t = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: alice.address(), nonce, fee: 0, tip: 0, valid_until: n.c.height() + 100, action: Action::Transfer { to: bob.address(), amount: 1 }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        t.fee = 2 * crate::econ::base_fee(&n.c.state, n.c.height() + 1, t.size());
        t.sign(alice)
    };
    // A shield, then a transfer behind it: refused, so no gap can open behind the shield.
    let a = stand_in_shield(&mut n, alice, 0, 0);
    n.c.submit(a.clone()).unwrap();
    let e = n.c.submit(transfer(&n, 1)).unwrap_err().to_string();
    assert!(e.contains("nothing queues behind a shield"), "{e}");
    n.c.drop_pending(&a.id());
    // A transfer, then the shield: her last tx.
    let t = transfer(&n, 0);
    n.c.submit(t.clone()).unwrap();
    let a = stand_in_shield(&mut n, alice, 1, 0);
    n.c.submit(a.clone()).unwrap();
    let mut queued = vec![];
    for k in others {
        let tx = stand_in_shield(&mut n, k, 0, FEE_GRID);
        queued.push(n.c.submit(tx).unwrap());
    }
    assert_eq!(n.c.mempool.iter().filter(|t| t.action.ghost_bundle().is_some()).count(), MAX_MEMPOOL_GHOST);
    // Bob outbids the cheapest: Alice's shield goes, her transfer stays, and her queue is whole.
    let b = stand_in_shield(&mut n, bob, 0, 3 * FEE_GRID);
    n.c.submit(b.clone()).unwrap();
    assert!(!n.c.in_mempool(&a.id()) && n.c.failure(&a.id()).unwrap().0.contains("full ghost share"));
    assert!(n.c.in_mempool(&t.id()) && n.c.next_nonce(&alice.address()) == 1, "Alice's queue is whole");
    // Her shield again, outbidding the cheapest left (the newest of the others' goes).
    let a = stand_in_shield(&mut n, alice, 1, 3 * FEE_GRID);
    n.c.submit(a.clone()).unwrap();
    let gone = queued.last().unwrap();
    assert!(!n.c.in_mempool(gone) && n.c.failure(gone).unwrap().0.contains("full ghost share"));
    // A sender's own shield at its queued one's nonce replaces it, and displaces nobody else's.
    let again = stand_in_shield(&mut n, &others[0], 0, 3 * FEE_GRID);
    n.c.submit(again.clone()).unwrap();
    assert!(!n.c.in_mempool(&queued[0]) && n.c.in_mempool(&again.id()) && n.c.pending_count(&others[0].address()) == 1);
    assert!(n.c.in_mempool(&queued[queued.len() - 2]));
    // The next block takes Alice's transfer and shield in order.
    let mut block = vec![t.clone(), a.clone()];
    block.extend(n.c.proposal_txs().into_iter().filter(|x| x.from != alice.address()).take(3).cloned());
    assert_eq!(n.block(block)[..2], [true, true]);
    n.cleanup();
}

/// Review of F36, round 3, through the node (the round-2 reviewer's
/// `r1_scratch_pinned_shields_through_the_node`): 256 accounts each queue an untipped shield and
/// then a transfer behind it, which made every shield in the full share unevictable, so an
/// honest shield tipping its cap was refused. Now nothing queues behind a shield (its sender
/// waits until it's included, or replaces it), so every queued shield is its sender's last tx
/// and the share gives the cheapest up: a higher tip always gets in, and nobody is left behind
/// a nonce gap.
#[test]
fn shields_with_txs_behind_them_cant_pin_the_ghost_share() {
    let mut n = net("pin", true, 1, &[]);
    let accounts: Vec<Keys> = (0..MAX_MEMPOOL_GHOST as u32 + 1).map(|i| Keys::from_seed(crypto::hash(&(i + 9_000).to_le_bytes()))).collect();
    for k in &accounts {
        n.c.state.accounts.insert(k.address(), crate::chain::Account { balance: 10 * FANG, ..Default::default() });
        n.c.state.supply += 10 * FANG;
    }
    let (honest, attackers) = (&accounts[0], &accounts[1..]);
    let transfer = |n: &Net, from: &Keys, nonce: u64, tip: u64| {
        let mut t = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: from.address(), nonce, fee: 0, tip, valid_until: n.c.height() + 100, action: Action::Transfer { to: honest.address(), amount: 1 }, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        t.fee = 2 * crate::econ::base_fee(&n.c.state, n.c.height() + 1, t.size()) + tip;
        t.sign(from)
    };
    let mut shields = vec![];
    let mut behind = vec![];
    for k in attackers {
        let s = stand_in_shield(&mut n, k, 0, 0);
        shields.push(n.c.submit(s).unwrap());
        let t = transfer(&n, k, 1, 0);
        behind.push(n.c.submit(t));
    }
    assert_eq!(n.c.mempool.iter().filter(|t| t.action.ghost_bundle().is_some()).count(), MAX_MEMPOOL_GHOST);
    // An honest shield tipping its cap gets in: the newest of the cheapest goes.
    let probe = stand_in_shield(&mut n, honest, 0, 0);
    let cap = crate::econ::max_tip(&n.c.state, crate::econ::base_fee(&n.c.state, n.c.height() + 1, probe.size()));
    let h = stand_in_shield(&mut n, honest, 0, cap);
    let got = n.c.submit(h.clone());
    assert!(got.is_ok(), "an honest shield tipping its cap ({cap} drops) must get in: {got:?}");
    let gone = shields.last().unwrap();
    assert!(!n.c.in_mempool(gone) && n.c.failure(gone).unwrap().0.contains("full ghost share"));
    // Nothing was queued behind a shield, so nobody is left behind a gap.
    for (r, k) in behind.iter().zip(attackers) {
        let e = r.as_ref().expect_err("a tx behind a queued shield").to_string();
        assert!(e.contains("is waiting in the mempool, and nothing queues behind a shield"), "{e}");
        assert_eq!(n.c.pending_count(&k.address()), if k.address() == attackers.last().unwrap().address() { 0 } else { 1 });
    }
    assert_eq!(n.c.next_nonce(&attackers.last().unwrap().address()), 0);
    // The order the other way round is fine: a transfer, then a shield behind it (its last tx).
    let late = &attackers[attackers.len() - 1];
    let t = transfer(&n, late, 0, 0);
    n.c.submit(t.clone()).unwrap();
    let s = stand_in_shield(&mut n, late, 1, cap);
    n.c.submit(s.clone()).unwrap();
    assert_eq!(n.c.pending_count(&late.address()), 2);
    // The next block takes the honest shield, and the late sender's transfer and shield in order.
    assert_eq!(n.block(vec![h.clone(), t, s]), [true, true, true]);
    n.cleanup();
}

/// A shield is always its sender's last queued tx, so a sender can't queue anything behind it
/// (its next tx, of any kind, a validator duty its account pays, another shield): it waits until
/// the shield is included, or replaces it with its own shield at the same nonce tipping at least
/// what would displace it (replace-by-fee, the only way a signed tx replaces a queued one).
#[test]
fn a_queued_shield_is_its_senders_last_tx() {
    let mut n = net("tail", true, 1, &[]);
    let alice = Keys::from_seed([71; 32]);
    n.c.state.accounts.insert(alice.address(), crate::chain::Account { balance: 10 * FANG, ..Default::default() });
    n.c.state.supply += 10 * FANG;
    let s0 = stand_in_shield(&mut n, &alice, 0, 0);
    n.c.submit(s0.clone()).unwrap();
    let relay = crate::econ::base_fee(&n.c.state, n.c.height() + 1, s0.size());
    let need = (relay.max(1)).div_ceil(FEE_GRID) * FEE_GRID;
    // Anything at the next nonce is refused, with what to do instead.
    let mut t = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: alice.address(), nonce: 1, fee: 0, tip: 0, valid_until: n.c.height() + 100, action: Action::Transfer { to: n.validators[0].address(), amount: 1 }, memo: String::new(), public_key: hex::encode(alice.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
    t.fee = 2 * crate::econ::base_fee(&n.c.state, n.c.height() + 1, t.size());
    let e = err(n.c.submit(t.clone().sign(&alice)));
    assert!(e.contains("your shield (nonce 0) is waiting in the mempool, and nothing queues behind a shield") && e.contains(&format!("a shield at nonce 0 tipping at least {need} drops")), "{e}");
    let s = stand_in_shield(&mut n, &alice, 1, 0);
    let e = err(n.c.submit(s));
    assert!(e.contains("nothing queues behind a shield"), "{e}");
    // At its nonce: only a shield replaces it, and only for a tip that displaces it.
    let e = err(n.c.submit(Tx { nonce: 0, ..t.clone() }.sign(&alice)));
    assert!(e.contains("only a shield replaces your queued shield"), "{e}");
    let s = stand_in_shield(&mut n, &alice, 0, need - 1);
    let e = err(n.c.submit(s));
    assert!(e.contains(&format!("to replace your queued shield (nonce 0), tip at least {need} drops")), "{e}");
    let s1 = stand_in_shield(&mut n, &alice, 0, need);
    n.c.submit(s1.clone()).unwrap();
    assert!(!n.c.in_mempool(&s0.id()) && n.c.in_mempool(&s1.id()) && n.c.pending_count(&alice.address()) == 1);
    assert!(n.c.failure(&s0.id()).unwrap().0.contains("replaced by its sender's shield with a higher tip"));
    // Behind a full queue of its sender's other txs (the replacement runs after them, not after
    // the shield it replaces, and takes no new slot).
    let bob = Keys::from_seed([72; 32]);
    let last = MAX_PENDING_PER_SENDER as u64 - 1;
    let (b0, b1) = (stand_in_shield(&mut n, &bob, last, 0), stand_in_shield(&mut n, &bob, last, need));
    let bobs: Vec<Tx> = (0..last)
        .map(|nonce| {
            let mut b = Tx { from: bob.address(), nonce, public_key: hex::encode(bob.public_key()), fee: 0, ..t.clone() };
            b.fee = 2 * crate::econ::base_fee(&n.c.state, n.c.height() + 1, b.size());
            b.sign(&bob)
        })
        .collect();
    // His balance covers his transfers and one shield, not two.
    let balance = bobs.iter().map(|x| x.fee + 1).sum::<u64>() + b1.fee + FANG + n.c.state.chain_params.ghost_verify_fee;
    n.c.state.accounts.insert(bob.address(), crate::chain::Account { balance, ..Default::default() });
    n.c.state.supply += balance;
    for b in &bobs {
        n.c.submit(b.clone()).unwrap();
    }
    n.c.submit(b0.clone()).unwrap();
    assert_eq!(n.c.pending_count(&bob.address()), MAX_PENDING_PER_SENDER);
    n.c.submit(b1.clone()).unwrap();
    assert!(!n.c.in_mempool(&b0.id()) && n.c.in_mempool(&b1.id()) && n.c.pending_count(&bob.address()) == MAX_PENDING_PER_SENDER);
    let mut block = bobs.clone();
    block.push(b1);
    assert!(n.block(block).into_iter().all(|x| x), "his transfers, then the replacement");
    // The validator's own account (it pays its duties here): a duty waits behind its shield too.
    let me = n.validators[0].clone_keys();
    let nonce = n.c.next_nonce(&me.address());
    let mine = stand_in_shield(&mut n, &me, nonce, 0);
    n.c.submit(mine.clone()).unwrap();
    let e = n.c.submit_system(Action::BeaconCommit { epoch: 2, commitment: "11".repeat(32) }, "").unwrap_err().to_string();
    assert!(e.contains("nothing queues behind a shield"), "{e}");
    // Once the shields are included, the account queues again.
    assert_eq!(n.block(vec![s1, mine]), [true, true]);
    n.c.submit(Tx { nonce: 1, ..t.clone() }.sign(&alice)).unwrap();
    // Review of r3-consensus: a shield tipping 19x its base fee can't be outbid at the current
    // base fee (that takes its tip plus a tenth, past the 20x cap), so no refusal names a tip
    // for a replacement: each says to wait until it's included or leaves the mempool.
    let carol = Keys::from_seed([73; 32]);
    n.c.state.accounts.insert(carol.address(), crate::chain::Account { balance: 10 * FANG, ..Default::default() });
    n.c.state.supply += 10 * FANG;
    let probe = stand_in_shield(&mut n, &carol, 0, 0);
    let base = crate::econ::base_fee(&n.c.state, n.c.height() + 1, probe.size());
    let high = stand_in_shield(&mut n, &carol, 0, 19 * base);
    n.c.submit(high.clone()).unwrap();
    let cap_of = |n: &Net, tx: &Tx| crate::econ::max_tip(&n.c.state, crate::econ::base_fee(&n.c.state, n.c.height() + 1, tx.size()));
    let cap = cap_of(&n, &high);
    assert!(displacing_tip(high.tip, base) > cap && high.tip < cap, "{} {cap}", high.tip);
    let mut behind = Tx { from: carol.address(), nonce: 1, public_key: hex::encode(carol.public_key()), fee: 0, ..t.clone() };
    behind.fee = 2 * crate::econ::base_fee(&n.c.state, n.c.height() + 1, behind.size());
    let e = err(n.c.submit(behind.clone().sign(&carol)));
    assert!(e.contains("your shield (nonce 0) is waiting in the mempool, and nothing queues behind a shield") && e.contains(&format!("a replacement's cap at the current base fee ({cap} drops), so it can't be replaced: wait until it's included or leaves the mempool (it's valid until block {})", high.valid_until)), "{e}");
    assert!(!e.contains("tipping at least"), "no tip past the cap: {e}");
    // A replacement tipping its whole cap: refused, the same way.
    let at_cap = stand_in_shield(&mut n, &carol, 0, 0);
    let full = cap_of(&n, &at_cap);
    let at_cap = stand_in_shield(&mut n, &carol, 0, full);
    let e = err(n.c.submit(at_cap.clone()));
    assert!(e.contains(&format!("your queued shield (nonce 0) can't be replaced by this one: outbidding it takes a tip past a replacement's cap at the current base fee ({} drops)", at_cap.tip)) && !e.contains("tip at least"), "{e}");
    // What the old hint named is past the cap: the fee check refuses it.
    let past = stand_in_shield(&mut n, &carol, 0, displacing_tip(high.tip, base));
    assert!(err(n.c.submit(past)).contains("tip too high"));
    // The way out: it's included, and the account queues again.
    assert!(n.c.in_mempool(&high.id()));
    assert_eq!(n.block(vec![high]), [true]);
    n.c.submit(behind.sign(&carol)).unwrap();
    n.cleanup();
}

/// Review of r3-consensus (the same class in `mempool_admit`'s same-note branch, which predates
/// round 3): a tx spending a note a queued tx spends replaces it only by outbidding it, and when
/// that takes a tip past the newcomer's cap the refusal says to wait rather than naming it (for
/// an unsigned tx, the cap on the fee grid).
#[test]
fn a_same_note_replacement_never_names_a_tip_past_the_cap() {
    let mut n = net("samecap", true, 1, &[]);
    let alice = Keys::from_seed([74; 32]);
    let shield = stand_in_shield(&mut n, &alice, 0, 0);
    // Two unsigned spends of the same notes (the stand-in's bundle), the queued one tipping
    // `queued`; the newcomer is the same size.
    let spend = |tip: u64| Tx { from: String::new(), nonce: 0, public_key: String::new(), signature: String::new(), tip, fee: 0, action: Action::PrivateTransfer { bundle: Box::new(shield.action.ghost_bundle().unwrap().clone()) }, ..shield.clone() };
    let relay = crate::econ::base_fee(&n.c.state, n.c.height() + 1, spend(0).size());
    let cap = crate::econ::max_tip(&n.c.state, relay);
    let grid_cap = cap / FEE_GRID * FEE_GRID;
    let admit = |queued: u64, tip: u64| mempool_admit(&[spend(queued)], &["queued".to_string()], &spend(tip), None, relay, cap, true).map_err(|e| e.to_string());
    // Within the cap: the refusal names the tip that gets in, and that tip does.
    let low = 2 * FEE_GRID;
    let need = displacing_tip(low, relay);
    assert!(need <= grid_cap);
    assert!(admit(low, need - FEE_GRID).unwrap_err().contains(&format!("pay a tip of at least {need} drops")));
    assert_eq!(admit(low, need).unwrap(), vec![("queued".to_string(), "replaced by a transaction spending the same note with a higher tip")]);
    // Queued near the cap: outbidding it is past the grid cap, so the refusal says to wait.
    let near = grid_cap - FEE_GRID;
    assert!(displacing_tip(near, relay) > grid_cap);
    let e = admit(near, grid_cap).unwrap_err();
    assert!(e.contains(&format!("it tips more than this one may (at most {grid_cap} drops), so it can't be replaced at the current base fee: wait until it's included or leaves the mempool")) && !e.contains("at least"), "{e}");
    n.cleanup();
}

/// Review of fix-fees-2: an unsigned ghost tx tips a whole multiple of the fee grid, so a full
/// mempool's hint for one, and the cap it names, are on the grid too.
#[test]
fn a_full_mempool_hints_an_unsigned_tx_a_tip_on_the_grid() {
    let mut n = net("grid", true, 1, &[]);
    let alice = Keys::from_seed([81; 32]);
    let key = SpendingKey::from_seed([82; 32]);
    n.fund(&[&alice], 300 * FANG);
    let (s, note) = n.shield(&alice, 10 * FANG, &key);
    n.submit(&s).unwrap();
    n.c.produce_block().unwrap();
    let senders: Vec<Keys> = (0..(MAX_MEMPOOL / MAX_PENDING_PER_SENDER + 1) as u32).map(|i| Keys::from_seed(crypto::hash(&(i + 11_000).to_le_bytes()))).collect();
    for k in &senders {
        n.c.state.accounts.insert(k.address(), crate::chain::Account { balance: 10 * FANG, ..Default::default() });
        n.c.state.supply += 10 * FANG;
    }
    let pay = |n: &Net, k: &Keys, nonce: u64, tip: u64| {
        let mut t = Tx { chain_id: n.c.genesis.chain_id.clone(), genesis_hash: n.c.genesis_hash.clone(), from: k.address(), nonce, fee: 0, tip, valid_until: n.c.height() + 100, action: Action::Transfer { to: alice.address(), amount: 1 }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        t.fee = 2 * crate::econ::base_fee(&n.c.state, n.c.height() + 1, t.size()) + tip;
        t.sign(k)
    };
    'fill: for k in &senders {
        for nonce in 0..MAX_PENDING_PER_SENDER as u64 {
            if n.c.mempool.len() >= MAX_MEMPOOL {
                break 'fill;
            }
            let t = pay(&n, k, nonce, 1_000);
            n.c.submit(t).unwrap();
        }
    }
    let spend = |n: &mut Net, tip: u64| {
        let to = SpendingKey::from_seed([83; 32]);
        let outputs = [n.output(note.value - FEE - tip, &to, &key), n.dummy_output()];
        let act = Act { spends: [n.spend(&key, note), n.dummy_spend()], outputs, anchor: n.anchor() };
        let tx = n.unsigned(Action::PrivateTransfer { bundle: Box::new(act.bundle(n.instance())) }, FEE + tip, tip);
        act.prove(tx)
    };
    // Outbidding a transfer tipping 1,000 drops takes about 25 times that for ~135 KiB, which an
    // unsigned tx can't tip: the hint rounds it up to the grid.
    let t0 = spend(&mut n, 0);
    let e = err(n.submit(&t0));
    assert!(e.contains(&format!("mempool is full; resubmit with a tip of at least {FEE_GRID} drops")), "{e}");
    let t1 = spend(&mut n, FEE_GRID);
    n.submit(&t1).unwrap();
    assert_eq!(n.c.mempool.len(), MAX_MEMPOOL);
    n.cleanup();
}

/// Audit F41: junk (proofs that fail, even cheaply) is remembered apart from valid verdicts, so
/// it can't flush them; valid verdicts are kept least recently used first, and a hit refreshes
/// one.
#[test]
fn junk_doesnt_evict_remembered_verdicts() {
    let mut v = Verdicts { ok: HashMap::new(), by_use: BTreeMap::new(), clock: 0, failed: HashMap::new(), failed_order: VecDeque::new() };
    let key = |i: u32| *blake3::hash(&i.to_le_bytes()).as_bytes();
    v.put(key(0), Ok(()));
    // A junk proof's verdict: it fails at decoding.
    let tx = crate::types::tests::vector_tx(Action::PrivateTransfer { bundle: crate::types::tests::vector_bundle(11) });
    let (public, _) = statement(&tx).unwrap().unwrap();
    let junk = blockula_ghost::verify(&public, &[1]);
    assert!(junk.is_err());
    for i in 1..=(REMEMBERED_VERDICTS + REMEMBERED_FAILURES) as u32 * 2 {
        v.put(key(1_000_000 + i), junk);
    }
    assert_eq!(v.get(&key(0)), Some(Ok(())), "thousands of failures later, still remembered");
    assert!(v.failed.len() <= REMEMBERED_FAILURES && v.get(&key(1_000_000 + 1)).is_none(), "failures are bounded on their own");
    assert!(matches!(v.get(&key(1_000_000 + (REMEMBERED_VERDICTS + REMEMBERED_FAILURES) as u32 * 2)), Some(Err(_))));
    // Valid verdicts: least recently used goes first, and a hit refreshes.
    for i in 1..REMEMBERED_VERDICTS as u32 {
        v.put(key(i), Ok(()));
    }
    assert_eq!(v.ok.len(), REMEMBERED_VERDICTS);
    assert_eq!(v.get(&key(0)), Some(Ok(())), "used again: now the freshest");
    v.put(key(REMEMBERED_VERDICTS as u32), Ok(()));
    assert_eq!((v.get(&key(0)), v.get(&key(1))), (Some(Ok(())), None), "the least recently used went");
    assert_eq!((v.ok.len(), v.by_use.len()), (REMEMBERED_VERDICTS, REMEMBERED_VERDICTS));
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
