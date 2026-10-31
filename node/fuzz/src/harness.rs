//! One entry point per fuzz target. Each feeds attacker-controlled bytes to the node's own decoder,
//! through the same calls the p2p, HTTP-sync and fast-sync paths make, then checks more than "no
//! panic": canonical re-encoding, JSON round trips, and verification that never panics. Each
//! returns whether the input was accepted all the way, so `tests/seeds.rs` can tell a live seed
//! from a stale one.

use crate::{
    chain::{Chain, Genesis, Outcome, State},
    checkpoints,
    consensus::{self, CBlock, Shared},
    crypto::Keys,
    epochs::{self, EpochRegistry},
    ghost,
    node::{self, SyncBatch},
    pq::{self, MlDsaScheme, PrivateKey},
    snapshot, staking,
    state::tree,
    types::{self, Action, KeyRole, ProposalKind, Tx},
};
use commonware_codec::{Decode as _, DecodeExt as _, Encode as _};
use commonware_consensus::{
    simplex::types::{Activity, Certificate, ConflictingFinalize, ConflictingNotarize, Finalization, Finalize, Notarization, Notarize, Nullification, Nullify, NullifyFinalize, Proposal, Vote},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{Digestible as _, Signer as _, blake3::Digest, certificate::Verifier as _};
use commonware_parallel::Sequential;
use commonware_utils::{iter::NonEmpty, ordered::Set, sys_rng};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

// The `state_machine_programs` target's v2 fixture and program ops, and the node targets for programs'
// receipts and commits.
mod commit;
mod oracle;
mod receipt;
mod v2;
pub use commit::{commit_of, commit_tampered, plan_is_stable, program_commit, Commit};
pub use receipt::program_receipt;

/// The `state_machine_programs` seeds, the `program_receipt` seeds and the `program_commit` seeds,
/// by name: what `tests/harness.rs` holds each to (`seeds()` has them with every other target's,
/// but builds a chain in a directory named after the process, which tests can't share).
pub fn state_machine_programs_seeds() -> Vec<(&'static str, Vec<u8>)> {
    v2::seeds()
}

pub fn program_commit_seeds() -> Vec<(&'static str, Vec<u8>)> {
    commit::seeds()
}

pub fn program_receipt_seeds() -> Vec<(String, Vec<u8>)> {
    receipt::seeds()
}

/// The profile (`'G'`, `'T'`, `'X'` or `'P'`, `v2.rs`'s table) that `state_machine_programs` runs
/// `data` on: its first byte's.
pub fn state_machine_programs_profile(data: &[u8]) -> char {
    v2::Profile::of(data.first().copied().unwrap_or(0)).letter()
}

/// The fuzz targets, by name (`fuzz_targets/<name>.rs`, `seeds/<name>/`).
pub const TARGETS: [&str; 15] = ["cblock", "tx_json", "tx_wire", "ghost_tx", "snapshot_manifest", "state_snapshot", "sync_batch", "epochs_registry", "certificate", "state_machine", "evidence", "program_tx_codec", "program_receipt", "program_commit", "state_machine_programs"];

/// Runs target `name` on `data`.
pub fn run(name: &str, data: &[u8]) -> bool {
    match name {
        "cblock" => cblock(data),
        "tx_json" => tx_json(data),
        "tx_wire" => tx_wire(data),
        "ghost_tx" => ghost_tx(data),
        "snapshot_manifest" => snapshot_manifest(data),
        "state_snapshot" => state_snapshot(data),
        "sync_batch" => sync_batch(data),
        "epochs_registry" => epochs_registry(data),
        "certificate" => certificate(data),
        "state_machine" => state_machine(data),
        "evidence" => evidence(data),
        "program_tx_codec" => program_tx_codec(data),
        "program_receipt" => program_receipt(data),
        "program_commit" => program_commit(data),
        "state_machine_programs" => state_machine_programs(data),
        _ => panic!("unknown fuzz target {name}"),
    }
}

pub const CHAIN_ID: &str = "blockula-fuzz";
/// The fixture chain's only genesis validator; it also holds the genesis allocation.
pub const VALIDATOR_SEED: [u8; 32] = [0x42; 32];
/// The 4-validator set the `certificate` target verifies against.
pub const SET_SEEDS: [[u8; 32]; 4] = [[0x51; 32], [0x52; 32], [0x53; 32], [0x54; 32]];
const GENESIS_MS: u64 = 1_750_000_000_000;
/// Seed txs are valid through this height (the seed blocks are at heights 1–4).
const VALID_UNTIL: u64 = 3_000;

/// The fixture genesis, built from JSON so new genesis fields with defaults don't break it.
pub fn genesis() -> &'static Genesis {
    static G: OnceLock<Genesis> = OnceLock::new();
    G.get_or_init(|| {
        let v = Keys::from_seed(VALIDATOR_SEED);
        serde_json::from_value(json!({
            "chain_id": CHAIN_ID, "token": "FANG", "decimals": 9, "block_time_ms": 1000, "faucet": false, "created_at_ms": GENESIS_MS,
            "validators": [{ "address": v.address(), "public_key": hex::encode(v.public_key()), "p2p_addr": "127.0.0.1:7071", "http_addr": "127.0.0.1:7070" }],
            "allocations": [{ "address": v.address(), "amount": "1000000000000000000" }],
        }))
        .expect("fixture genesis no longer parses: update harness::genesis")
    })
}

/// A fresh full node on the fixture genesis, in its own temp dir.
fn open_chain(tag: &str) -> Shared {
    let home = std::env::temp_dir().join(format!("blockula-fuzz-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("genesis.json"), serde_json::to_vec(genesis()).unwrap()).unwrap();
    Arc::new(Mutex::new(Chain::open(&home).expect("open the fixture chain")))
}

/// The fixture chain's genesis hash (its instance, which every tx signs after the chain id).
pub fn genesis_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hex::encode(consensus::genesis_block(genesis()).digest_bytes()))
}

/// The fixture chain's instance, as handoffs bind it (F02).
fn instance() -> &'static epochs::Instance {
    static I: OnceLock<epochs::Instance> = OnceLock::new();
    I.get_or_init(|| epochs::Instance::of(genesis()))
}

/// The fixture chain's address prefix (`boo`, the genesis default).
fn prefix() -> &'static str {
    &genesis().address_prefix
}

/// The fixture genesis' state, for `describe`.
fn genesis_state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| genesis().initial_state())
}

fn set_verifier() -> &'static MlDsaScheme {
    static V: OnceLock<MlDsaScheme> = OnceLock::new();
    V.get_or_init(|| MlDsaScheme::verifier(&consensus::namespace(CHAIN_ID, genesis_hash()), four_validators()))
}

fn four_validators() -> Set<pq::PublicKey> {
    Set::try_from(SET_SEEDS.map(|s| PrivateKey::from_seed_bytes(s).public_key()).to_vec()).unwrap()
}

// ------------------------------------------------------------------------------------ targets

/// `CBlock::read_cfg`: what marshal and broadcast decode from peers. A block has one encoding
/// (its digest is over the re-encoded body), and every tx in it (wire form) goes through the
/// stateless checks `verify` runs before voting.
pub fn cblock(data: &[u8]) -> bool {
    let Ok(block) = CBlock::decode(data) else { return false };
    assert_eq!(block.encode().as_ref(), data, "CBlock decoding accepted a non-canonical encoding");
    let mut ok = true;
    for tx in block.parse_txs() {
        ok &= tx.is_some_and(|tx| consensus::check_tx_stateless(&tx, CHAIN_ID, genesis_hash(), prefix()).is_ok());
    }
    ok
}

/// Tx JSON as `POST /api/tx` parses it (strict: unknown fields are refused): then `check_format`
/// (which includes the wire round trip), the address prefix, the fee and the signature
/// (`check_tx_stateless`), the explorer summary, and a JSON round trip that must give back the
/// same tx.
pub fn tx_json(data: &[u8]) -> bool {
    let Ok(tx) = serde_json::from_slice::<Tx>(data) else { return false };
    let _ = (tx.signing_bytes(), tx.id(), tx.size(), tx.charge());
    let ok = consensus::check_tx_stateless(&tx, CHAIN_ID, genesis_hash(), prefix()).is_ok();
    let back: Tx = serde_json::from_slice(&serde_json::to_vec(&tx).unwrap()).expect("a parsed tx must parse again after re-serializing");
    assert_eq!(back, tx, "the JSON round trip changed the tx");
    if tx.check_format().is_ok() {
        genesis_state().describe(&tx, None);
    }
    ok
}

/// The Tx v3 wire form (`Tx::from_bytes`), as blocks and mempool gossip carry it. Strict: only
/// the exact bytes `to_bytes` writes parse, and `size()` is their length. Then the same checks as
/// [`tx_json`], and the JSON the API serves for it must parse back to the same tx.
pub fn tx_wire(data: &[u8]) -> bool {
    let Ok(tx) = Tx::from_bytes(data) else { return false };
    assert_eq!(tx.to_bytes(), data, "Tx::from_bytes accepted a non-canonical encoding");
    assert_eq!(tx.size(), data.len() as u64, "size() isn't the wire length");
    let back: Tx = serde_json::from_slice(&serde_json::to_vec(&tx).unwrap()).expect("a wire tx must survive its JSON form");
    assert_eq!(back, tx, "the JSON round trip changed a wire tx");
    let ok = consensus::check_tx_stateless(&tx, CHAIN_ID, genesis_hash(), prefix()).is_ok();
    if tx.check_format().is_ok() {
        genesis_state().describe(&tx, None);
    }
    ok
}

/// Ghost Mode's txs (tags 50–52) in their wire forms, as gossip and blocks carry them: the
/// unsigned `BLOCKULA_GHOST_V1` form and signed shields. Only canonical bytes parse; the
/// statement the proof is checked against comes from the tx alone; and the stateless checks,
/// Action proof included, never panic whatever the bytes (Plonky3's verifier runs under
/// `catch_unwind`). Accepted: a well-formed ghost tx whose proof verifies.
pub fn ghost_tx(data: &[u8]) -> bool {
    let Ok(tx) = Tx::from_bytes(data) else { return false };
    assert_eq!(tx.to_bytes(), data, "Tx::from_bytes accepted a non-canonical ghost tx");
    assert_eq!(tx.size(), data.len() as u64, "size() isn't the wire length");
    if tx.action.ghost_bundle().is_none() {
        return false;
    }
    let back: Tx = serde_json::from_slice(&serde_json::to_vec(&tx).unwrap()).expect("a ghost tx must survive its JSON form");
    assert_eq!(back, tx, "the JSON round trip changed a ghost tx");
    let _ = (tx.ghost_sighash(), ghost::statement(&tx), ghost::action_json(&tx.action));
    let ok = consensus::check_tx_stateless(&tx, CHAIN_ID, genesis_hash(), prefix()).is_ok();
    if tx.check_format().is_ok() {
        genesis_state().describe(&tx, None);
    }
    ok
}

/// A shield, a private transfer and an unshield on the fixture chain, with real proofs (the
/// `blockula-ghost` prover, as a wallet runs it). The spends' anchor is a tree of their own:
/// stateless checks don't look at anchors.
fn ghost_seed_txs(funded: &Keys) -> Vec<(&'static str, Tx)> {
    use blockula_ghost::field::{digest_to_bytes, Digest};
    use blockula_ghost::{dummy_output, encrypt_note, ActionWitness, EncryptedNote, FullTree, Note, SpendWitness, SpendingKey};
    let mut rng = commonware_utils::sys_rng();
    let key = SpendingKey::from_seed([0x47; 32]);
    let instance = types::HexBytes(ghost::chain_instance(genesis()).to_vec());
    let output = |rng: &mut _, value: u64| -> (Note, EncryptedNote) {
        let mut rseed = [0; 32];
        rand_core::Rng::fill_bytes(rng, &mut rseed);
        let note = Note { value, owner: key.address().owner(), rseed };
        (note, encrypt_note(&note, &key.address(), &[0; 256], Some(&key.full_viewing_key().outgoing()), rng).unwrap())
    };
    // Proves `spends` → `outputs` for `tx` (whose action holds a bundle with an empty proof).
    let finish = |mut tx: Tx, spends: [SpendWitness; 2], outputs: [(Note, EncryptedNote); 2], anchor: Digest| -> Tx {
        let h = |d: &Digest| types::HexBytes(digest_to_bytes(d).to_vec());
        let bundle = types::GhostBundle {
            instance: instance.clone(),
            anchor: h(&anchor),
            nullifiers: [h(&spends[0].nullifier()), h(&spends[1].nullifier())],
            commitments: [h(&outputs[0].0.commitment()), h(&outputs[1].0.commitment())],
            enc: [types::HexBytes(outputs[0].1.enc.clone()), types::HexBytes(outputs[1].1.enc.clone())],
            out: [types::HexBytes(outputs[0].1.out.clone()), types::HexBytes(outputs[1].1.out.clone())],
            proof: types::HexBytes::default(),
        };
        let slot = match &mut tx.action {
            types::Action::Shield { bundle: b, .. } | types::Action::PrivateTransfer { bundle: b } | types::Action::Unshield { bundle: b, .. } => b,
            _ => unreachable!(),
        };
        **slot = bundle;
        let w = ActionWitness { spends, outputs: [outputs[0].0, outputs[1].0], anchor, sighash: tx.ghost_sighash().unwrap() };
        let proof = blockula_ghost::prove(&w).expect("prove a seed action").1;
        match &mut tx.action {
            types::Action::Shield { bundle, .. } | types::Action::PrivateTransfer { bundle } | types::Action::Unshield { bundle, .. } => bundle.proof = types::HexBytes(proof),
            _ => unreachable!(),
        }
        tx
    };
    let empty = || Box::new(types::GhostBundle { instance: types::HexBytes::default(), anchor: types::HexBytes::default(), nullifiers: Default::default(), commitments: Default::default(), enc: Default::default(), out: Default::default(), proof: types::HexBytes::default() });
    // A shield of 5 FANG (dummy spends).
    let mut shield = signed_tx(funded, 13, 0, json!({ "type": "transfer", "to": "alice.boo", "amount": "1" }));
    shield.action = types::Action::Shield { amount: 5_000_000_000, bundle: empty() };
    shield.fee = 3_000_000;
    let outputs = [output(&mut rng, 5_000_000_000), dummy_output(&mut rng)];
    let shield = finish(shield, [SpendWitness::dummy(&mut rng), SpendWitness::dummy(&mut rng)], outputs, blockula_ghost::Frontier::new().root()).sign(funded);
    // A 5 FANG note in a tree, spent by a private transfer and by an unshield (fee 0.003 FANG).
    let note = output(&mut rng, 5_000_000_000).0;
    let mut tree = FullTree::new();
    let pos = tree.append(note.commitment()).unwrap();
    let spend = || SpendWitness::new(&key, note, tree.path(pos).unwrap());
    let fee = 3_000_000;
    let unsigned = |action: types::Action| Tx { chain_id: CHAIN_ID.into(), genesis_hash: genesis_hash().into(), from: String::new(), nonce: 0, fee, tip: 0, valid_until: VALID_UNTIL, action, memo: String::new(), public_key: String::new(), signature: String::new() };
    let outputs = [output(&mut rng, 5_000_000_000 - fee), dummy_output(&mut rng)];
    let transfer = finish(unsigned(types::Action::PrivateTransfer { bundle: empty() }), [spend(), SpendWitness::dummy(&mut rng)], outputs, tree.root());
    let outputs = [output(&mut rng, 3_000_000_000 - fee), dummy_output(&mut rng)];
    let unshield = finish(unsigned(types::Action::Unshield { to: "alice.boo".into(), amount: 2_000_000_000, bundle: empty() }), [spend(), SpendWitness::dummy(&mut rng)], outputs, tree.root());
    vec![("shield", shield), ("private-transfer", transfer), ("unshield", unshield)]
}

/// `GET /api/snapshot/latest` as fast sync reads it: JSON, then `verify_manifest` (hex blocks,
/// hash links, the checkpoint commitment, the validator-set history and the certificate). The
/// history a joiner fetches from `/api/epochs` rides along in the input as `epochs` (the genesis
/// set alone when it's absent).
pub fn snapshot_manifest(data: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct History {
        #[serde(default)]
        epochs: Option<EpochRegistry>,
    }
    let Ok(m) = serde_json::from_slice::<snapshot::Manifest>(data) else { return false };
    let history = serde_json::from_slice::<History>(data).ok().and_then(|h| h.epochs).unwrap_or_else(|| EpochRegistry::genesis(genesis()));
    let Ok(v) = snapshot::verify_manifest(genesis(), &m, &history) else { return false };
    // Nothing past the certified epoch is kept (F08), and it starts at the root it was checked from.
    assert!(v.epochs.base == 0 && v.epochs.known_through == snapshot::last_epoch(genesis(), &m), "verify_manifest kept history it didn't verify");
    assert!(v.epochs.sets.keys().chain(v.epochs.proofs.keys()).all(|e| *e <= v.epochs.known_through), "verify_manifest kept sets past the certified epoch");
    let _ = snapshot::check_chunk(&v, 0, data);
    // And against trusted checkpoints, as `join` checks it: one the manifest proves, one it can't.
    let set_hash = checkpoints::genesis_checkpoint(genesis()).set_hash;
    for (height, set_hash) in [(m.height, None), (m.height / 2, set_hash), (u64::MAX, None)] {
        let cp = checkpoints::Checkpoint { height, hash: v.hash_at(height).unwrap_or_else(|| "ab".repeat(32)), set_hash, timestamp_ms: None };
        let pins = checkpoints::Pins { checkpoints: vec![cp], ..Default::default() };
        let _ = v.check_trust(&checkpoints::Trust::new(genesis(), &pins, false).expect("the fixture chain is unknown, so any checkpoint past #0 is accepted"));
    }
    true
}

/// Fast-sync state records (`snapshot::read_state`, what the verified chunks decompress to):
/// strict decoding of every record into a state and its tree. Whatever decodes must be the
/// canonical encoding (re-encoding gives the same bytes), the rebuilt tree must match a
/// from-scratch root, and records must prove against it. The joiner then compares that root with
/// the certified one.
pub fn state_snapshot(data: &[u8]) -> bool {
    let Ok(state) = snapshot::read_state(&mut &data[..]) else { return false };
    let mut out = vec![];
    snapshot::write_state(&state, &mut out).unwrap();
    assert_eq!(out, data, "read_state accepted a non-canonical state encoding");
    let root = state.root_hash();
    assert_eq!(root, state.full_root(), "the rebuilt tree disagrees with its records");
    let mut checked = 0;
    state.for_each_record(|n, k, v| {
        if checked < 8 {
            assert!(tree::verify(&root, n, k, Some(v), &state.prove(n, k).unwrap()), "record ({n}, {k:?}) doesn't prove against the root");
            checked += 1;
        }
    });
    let _ = state.check_invariants();
    true
}

/// `GET /api/sync` batches as `sync_once` applies them to a node at genesis: decode, certificate
/// check, link check, execution. A batch that executes resets the node so runs stay independent.
pub fn sync_batch(data: &[u8]) -> bool {
    static CHAIN: std::sync::Mutex<Option<Shared>> = std::sync::Mutex::new(None);
    let Ok(batch) = serde_json::from_slice::<SyncBatch>(data) else { return false };
    let mut slot = CHAIN.lock().unwrap();
    let chain = slot.get_or_insert_with(|| open_chain("sync")).clone();
    let verifier = consensus::EpochProvider::new(chain.clone(), None).epoch_scheme(0).expect("the genesis set");
    let applied = node::apply_batch(&chain, &verifier, 0, &batch).is_ok();
    if applied {
        *slot = None;
    }
    applied
}

/// A `GET /api/epochs` page as `exchange_epochs` takes signatures from it (`candidates`, verified,
/// `add_verified`), as fast sync assembles pages (`append_page`) and verifies the history from
/// the genesis or a pinned set (`verify_chain`, `verify_from`), epoch 0 included (FUZZ-1,
/// FUZZ-2), and as a node pages, rebases and truncates its own.
pub fn epochs_registry(data: &[u8]) -> bool {
    let Ok(reg) = serde_json::from_slice::<EpochRegistry>(data) else { return false };
    let g = genesis();
    let verified = epochs::verify_chain(g, &reg, reg.known_through).is_ok();
    for epoch in 0..=reg.known_through.min(3) {
        let _ = epochs::verify_chain(g, &reg, epoch);
        if let Some(set) = reg.set(epoch) {
            let _ = epochs::verify_from(g, &reg, &epochs::Root { epoch, set_hash: epochs::set_hash(set) }, reg.known_through);
        }
    }
    let _ = (reg.proven_through(), reg.set(reg.known_through), reg.set(reg.base));
    let mut ours = EpochRegistry::genesis(g);
    let at = instance();
    let wanted = ours.candidates(at, &reg, 64);
    assert!(wanted.len() <= 64);
    ours.add_verified(at, epochs::verify_candidates(wanted));
    let mut same = reg.clone();
    same.merge_sigs(at, &reg);
    let _ = ours.clone().append_page(&reg);
    // Paging what it holds reassembles it, page by page, whatever it holds.
    let (mut first, mut next) = reg.slice(reg.base, 4096);
    let mut pages = 0;
    while let Some(from) = next.filter(|_| pages < 64) {
        let (page, more) = reg.slice(from, 4096);
        assert!(more.is_none_or(|m| m > from), "a page must move forward");
        first.append_page(&page).expect("a node's own pages fit together");
        (next, pages) = (more, pages + 1);
    }
    for at in [reg.base, reg.known_through / 2, reg.known_through, u64::MAX] {
        let (mut r, mut t) = (reg.clone(), reg.clone());
        r.rebase(at);
        t.truncate(at);
        let _ = (r.proven_through(), t.proven_through(), r.set(at), t.set(at));
    }
    verified
}

/// Finalization certificates (`pq::Certificate::read_cfg`), plus the simplex certificate and vote
/// messages that carry them over p2p, decoded for a 4-validator set and verified. The first byte
/// picks the participant count for the bare certificate decode.
pub fn certificate(data: &[u8]) -> bool {
    let Some((&n, rest)) = data.split_first() else { return false };
    let mut decoded = false;
    if let Ok(c) = pq::Certificate::decode_cfg(rest, &(1 + n as usize % 8)) {
        assert_eq!(c.encode().as_ref(), rest, "pq::Certificate decoding accepted a non-canonical encoding");
        decoded = true;
    }
    let verifier = set_verifier();
    let mut rng = sys_rng();
    if let Ok(c) = Certificate::<MlDsaScheme, Digest>::decode_cfg(rest, &verifier.certificate_codec_config()) {
        assert_eq!(c.encode().as_ref(), rest, "simplex Certificate decoding accepted a non-canonical encoding");
        decoded |= match &c {
            Certificate::Notarization(x) => x.verify(&mut rng, verifier, &Sequential),
            Certificate::Nullification(x) => x.verify::<_, Digest>(&mut rng, verifier, &Sequential),
            Certificate::Finalization(x) => x.verify(&mut rng, verifier, &Sequential),
        };
    }
    if let Ok(v) = Vote::<MlDsaScheme, Digest>::decode(rest) {
        assert_eq!(v.encode().as_ref(), rest, "simplex Vote decoding accepted a non-canonical encoding");
        decoded |= match &v {
            Vote::Notarize(x) => x.verify(&mut rng, verifier, &Sequential),
            Vote::Nullify(x) => x.verify::<_, Digest>(&mut rng, verifier, &Sequential),
            Vote::Finalize(x) => x.verify(&mut rng, verifier, &Sequential),
        };
    }
    decoded
}

// ---------------------------------------------------------------------------------- evidence

/// Epochs the evidence fixture's 4-validator set (`SET_SEEDS`) is active in: 0 through this one.
const EVIDENCE_EPOCHS: u64 = 3;
const EVIDENCE_EPOCH_BLOCKS: u64 = 10;
/// Owns the fixture validators' pools (so slashes take real stake) and reports the evidence.
const EVIDENCE_OWNER: [u8; 32] = [0x61; 32];

/// The state `evidence` verifies against: the `SET_SEEDS` validators, bonded by one owner, active
/// in epochs 0–`EVIDENCE_EPOCHS`, at the last block of that epoch.
fn evidence_state() -> &'static (State, u64) {
    static S: OnceLock<(State, u64)> = OnceLock::new();
    S.get_or_init(|| {
        let owner = Keys::from_seed(EVIDENCE_OWNER);
        let validators: Vec<Value> = SET_SEEDS.iter().enumerate().map(|(i, s)| {
            let k = PrivateKey::from_seed_bytes(*s).public_key();
            json!({ "address": k.address(prefix()), "public_key": hex::encode(k.as_ref()), "p2p_addr": format!("127.0.0.1:{}", 7101 + i), "http_addr": format!("127.0.0.1:{}", 7201 + i) })
        }).collect();
        let operators: Vec<Value> = SET_SEEDS.iter().zip(&validators).map(|(s, v)| {
            let (p2p, http) = (v["p2p_addr"].as_str().unwrap(), v["http_addr"].as_str().unwrap());
            json!({ "owner": owner.address(), "consensus_key": v["public_key"], "pop": crate::chain::make_pop(*s, CHAIN_ID, &owner.address(), p2p, http), "p2p_addr": p2p, "http_addr": http, "self_bond": "10000000000000", "commission_bps": 0 })
        }).collect();
        let g: Genesis = serde_json::from_value(json!({
            "chain_id": CHAIN_ID, "token": "FANG", "decimals": 9, "block_time_ms": 1000, "faucet": false, "created_at_ms": GENESIS_MS, "epoch_blocks": EVIDENCE_EPOCH_BLOCKS,
            "validators": validators, "operators": operators,
            "allocations": [{ "address": owner.address(), "amount": "1000000000000000" }],
        }))
        .expect("evidence fixture genesis no longer parses: update harness::evidence_state");
        let mut st = g.initial_state();
        let set = st.active_sets[&0].clone();
        for e in 1..=EVIDENCE_EPOCHS {
            st.active_sets.insert(e, set.clone());
        }
        let height = st.params.last_of(EVIDENCE_EPOCHS);
        (st, height)
    })
}

/// `ReportEquivocation` evidence, which any account may send: `staking::decode_evidence` accepts
/// only canonical encodings of the three fault kinds, and it, `verify_evidence`,
/// `evidence_status` and the explorer's description never panic. Evidence that verifies is
/// reported in a tx against a fixture with the 4-validator set: it applies, slashes and
/// tombstones once (the state's invariants hold after), and the same fault reported again fails.
pub fn evidence(data: &[u8]) -> bool {
    if let Ok(e) = staking::decode_evidence(data) {
        assert_eq!(e.activity.encode().as_ref(), data, "decode_evidence accepted a non-canonical encoding");
        assert!((staking::CONFLICTING_NOTARIZE..=staking::NULLIFY_FINALIZE).contains(&e.kind));
    }
    let (st, height) = evidence_state();
    let _ = staking::evidence_status(st, CHAIN_ID, genesis_hash(), data, *height);
    let Ok(fault) = staking::verify_evidence(st, CHAIN_ID, genesis_hash(), data, *height) else { return false };
    assert!(st.active_sets[&fault.epoch].contains(&fault.validator), "verified evidence names a validator outside its epoch's set");
    let owner = Keys::from_seed(EVIDENCE_OWNER);
    let report = |st: &State, nonce: u64| {
        let mut tx = Tx { chain_id: CHAIN_ID.into(), genesis_hash: genesis_hash().into(), from: owner.address(), nonce, fee: 0, tip: 0, valid_until: height + 10, action: Action::ReportEquivocation { evidence: hex::encode(data) }, memo: String::new(), public_key: hex::encode(owner.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.fee = crate::econ::base_fee(st, *height, tx.size());
        tx
    };
    let mut st = st.clone();
    let proposer = st.active_sets[&0][0].clone();
    let tx = report(&st, 0);
    let _ = st.describe_with(&tx, None, &|_| None);
    let supply = st.supply;
    assert_eq!(st.apply(&tx, CHAIN_ID, &proposer, *height).expect("verified evidence is admitted"), Outcome::Applied, "verified evidence must apply");
    assert!(st.supply <= supply, "a slash minted FANG");
    assert!(st.validators[&fault.validator].status == crate::chain::ValidatorStatus::Tombstoned, "the offender isn't tombstoned");
    if let Err(e) = st.check_invariants() {
        panic!("invariant broken after a slash: {e}");
    }
    assert!(matches!(st.apply(&report(&st, 1), CHAIN_ID, &proposer, *height), Ok(Outcome::Failed(_))), "a fault is punished once");
    true
}

/// Fault evidence by the `SET_SEEDS` validator at index `signer` of the sorted set, in `epoch`
/// at `view`: both votes really signed.
fn evidence_seed(kind: u8, signer: usize, epoch: u64, view: u64) -> Vec<u8> {
    let set = four_validators();
    let key = SET_SEEDS.iter().map(|s| PrivateKey::from_seed_bytes(*s)).find(|k| set.position(&k.public_key()) == Some(signer)).unwrap();
    let s = MlDsaScheme::signer(&consensus::namespace(CHAIN_ID, genesis_hash()), set, key).unwrap();
    let round = Round::new(Epoch::new(epoch), View::new(view));
    let p = |d: u8| Proposal::new(round, View::new(view - 1), Digest([d; 32]));
    let activity = match kind {
        staking::CONFLICTING_NOTARIZE => Activity::ConflictingNotarize(ConflictingNotarize::new(Notarize::sign(&s, p(1)).unwrap(), Notarize::sign(&s, p(2)).unwrap())),
        staking::CONFLICTING_FINALIZE => Activity::ConflictingFinalize(ConflictingFinalize::new(Finalize::sign(&s, p(1)).unwrap(), Finalize::sign(&s, p(2)).unwrap())),
        _ => Activity::NullifyFinalize(NullifyFinalize::new(Nullify::sign::<Digest>(&s, round).unwrap(), Finalize::sign(&s, p(1)).unwrap())),
    };
    activity.encode().to_vec()
}

// ------------------------------------------------------------------------------ state machine

/// The state machine target's chain: one validator (the Season 0 admin too, so the admin levers
/// are reachable), funded users (one with a vesting lock), 4-block epochs and Proof of
/// Contribution timings of a few blocks, so epochs, Séances and vesting all happen.
const SM_CHAIN: &str = "blockula-fuzz-sm";
/// The v2 fixture's chain (`state_machine_programs`): the same genesis born at protocol v2 with
/// programs on, under a chain id of its own (the proofs of possession and key consents bind it).
const SM_CHAIN_V2: &str = "blockula-fuzz-sm-v2";
const SM_USERS: usize = 6;
const SM_AGENTS: usize = 3;
/// Ops past this many are ignored, so one input stays fast.
const SM_MAX_OPS: usize = 300;
/// Bytes per op: kind, a, b, then a little-endian u16 amount (the raw-action op takes more).
const SM_OP: usize = 5;

/// A key the state machine target signs (in name) with, precomputed: deriving an ML-DSA public
/// key is slow, and the fuzzer builds thousands of txs a second.
struct Who {
    address: String,
    public_key: String,
    key_id: String,
}

impl Who {
    fn of(k: &Keys) -> Who {
        let pk = k.public_key();
        Who { address: crate::crypto::address_from_public_key(&pk), key_id: types::key_id(&pk), public_key: hex::encode(pk) }
    }
}

struct SmFixture {
    /// The chain the ops' txs are for.
    chain: &'static str,
    /// The state the first op runs on, and its height: genesis (0) for v1; for v2 the state after
    /// the blocks that upload and create the four guests (`v2::fixture`).
    state: State,
    height: u64,
    /// The newest protocol version a node running this genesis supports (the halt check's).
    supported: u32,
    validator: Who,
    /// The genesis validators, `validator` first: one, but for the pause profile's four.
    validators: Vec<Who>,
    users: Vec<Who>,
    agents: Vec<Who>,
    /// Per user: a consensus key it may register as a validator, and that key's PoP.
    candidates: Vec<(String, String)>,
    /// Addresses that hold nothing at genesis.
    newcomers: Vec<String>,
    /// The `newcomers`' keys, which sign for the validators a user registers with them (their
    /// consensus keys' own accounts).
    newcomer_keys: Vec<Who>,
    /// Per agent, per user: the agent key's consent to joining that user's account (AddKey's pop).
    agent_pops: Vec<Vec<String>>,
    /// The four guests and the programs made of them; `None` on the v1 fixture.
    programs: Option<v2::Guests>,
}

/// The v1 fixture (`None`: the chain as it has always run, protocol v1 with no programs) or the v2
/// one of a profile ([`v2::fixture`]).
fn sm_fixture(v2: Option<v2::Profile>) -> &'static SmFixture {
    static F: OnceLock<SmFixture> = OnceLock::new();
    match v2 {
        Some(profile) => v2::fixture(profile),
        None => F.get_or_init(|| sm_build(SM_CHAIN, Build::V1)),
    }
}

/// Which genesis `sm_build` builds.
#[derive(Clone, Copy)]
enum Build {
    /// The v1 fixture: protocol v1, one validator.
    V1,
    /// Born at protocol v2 with the `programs` feature on (and no program yet): with this many
    /// genesis validators and this block time.
    V2 { validators: usize, block_time_ms: u64 },
}

/// The seeds of genesis validators beyond the first, who are not the admin: each holds 1,000 FANG
/// to pay its signatures' fees.
const MORE_VALIDATOR_SEEDS: [[u8; 32]; 3] = [[0x43; 32], [0x44; 32], [0x45; 32]];

/// The state machine's genesis state and keys on `chain`.
fn sm_build(chain: &'static str, build: Build) -> SmFixture {
    let (more, block_time_ms) = match build {
        Build::V1 => (0, 1_000),
        Build::V2 { validators, block_time_ms } => (validators.saturating_sub(1), block_time_ms),
    };
    let validator = Keys::from_seed(VALIDATOR_SEED);
    let others: Vec<Keys> = MORE_VALIDATOR_SEEDS.iter().take(more).map(|s| Keys::from_seed(*s)).collect();
    let users: Vec<Keys> = (1..=SM_USERS as u8).map(|i| Keys::from_seed([i; 32])).collect();
    let agents: Vec<Keys> = (0..SM_AGENTS as u8).map(|i| Keys::from_seed([100 + i; 32])).collect();
    let mut allocations = vec![json!({ "address": validator.address(), "amount": "10000000000000000" })];
    for (i, u) in users.iter().enumerate() {
        let mut a = json!({ "address": u.address(), "amount": "100000000000000" });
        if i == SM_USERS - 1 {
            a["vesting"] = json!({ "start": 0, "cliff": 20, "end": 200 });
        }
        allocations.push(a);
    }
    allocations.extend(others.iter().map(|k| json!({ "address": k.address(), "amount": "1000000000000" })));
    let validator_infos: Vec<Value> = std::iter::once(&validator).chain(&others).enumerate().map(|(i, k)| json!({ "address": k.address(), "public_key": hex::encode(k.public_key()), "p2p_addr": format!("127.0.0.1:{}", 7071 + 2 * i), "http_addr": format!("127.0.0.1:{}", 7070 + 2 * i) })).collect();
    let mut genesis = json!({
        "chain_id": chain, "token": "FANG", "decimals": 9, "block_time_ms": block_time_ms, "faucet": false, "created_at_ms": GENESIS_MS,
        "validators": validator_infos,
        "allocations": allocations,
        "epoch_blocks": 4,
        "validator_admins": [validator.address()],
        "contribution": { "seance_blocks": 6, "claim_maturity_blocks": 0, "vesting_blocks": 10, "vouch_lock_blocks": 5, "claim_ttl_blocks": 15, "soul_half_life_blocks": 100 },
    });
    if block_time_ms != 1_000 {
        // Parameters whose defaults or bounds are a count of blocks per day, which a slow chain has
        // few of: a day of 10 blocks (the least one whose bounds are a range) takes these two.
        genesis["params"] = json!({ "max_tx_validity_blocks": 10, "ghost_anchor_window": 300 });
    }
    if matches!(build, Build::V2 { .. }) {
        genesis["features"] = json!([crate::upgrade::PROGRAMS_FEATURE]);
        genesis["upgrades"] = json!([{ "version": crate::upgrade::PROGRAMS_VERSION, "activation_height": 1 }]);
    }
    let genesis: Genesis = serde_json::from_value(genesis).expect("state machine genesis no longer parses: update harness::sm_build");
    genesis.check().expect("state machine genesis is invalid: update harness::sm_build");
    let candidates = users
        .iter()
        .enumerate()
        .map(|(i, u)| {
            let key_seed = [200 + i as u8; 32];
            (hex::encode(Keys::from_seed(key_seed).public_key()), crate::chain::make_pop(key_seed, chain, &u.address(), "203.0.113.9:7071", "https://v.example"))
        })
        .collect();
    let newcomer_keys: Vec<Keys> = (0..8u8).map(|i| Keys::from_seed([200 + i; 32])).collect();
    let newcomers = newcomer_keys.iter().map(Keys::address).collect();
    let agent_pops = agents.iter().map(|a| users.iter().map(|u| types::key_pop(a, chain, &"00".repeat(32), &u.address())).collect()).collect();
    SmFixture {
        chain,
        state: genesis.initial_state(),
        height: 0,
        supported: crate::upgrade::supported_version(&genesis),
        validator: Who::of(&validator),
        validators: std::iter::once(&validator).chain(&others).map(Who::of).collect(),
        users: users.iter().map(Who::of).collect(),
        agents: agents.iter().map(Who::of).collect(),
        candidates,
        newcomers,
        newcomer_keys: newcomer_keys.iter().map(Who::of).collect(),
        agent_pops,
        programs: None,
    }
}

/// How a block of the v2 fixture is run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exec {
    /// Unrolled (`v2::run_block`), with the oracles looking at the state around each transaction.
    Unrolled,
    /// Through `State::run_block` itself: the determinism rerun's second pass.
    RunBlock,
}

/// Whether `tx` is a program transaction (tags 62–64): the fee byte of one can ask for a fee below
/// its charge, a tx without fuel can't.
fn v2_is_program_tx(tx: &Tx) -> bool {
    matches!(tx.action, Action::UploadCode { .. } | Action::CreateProgram { .. } | Action::CallProgram { .. })
}

/// What a block of the v2 fixture ran, for the checks after it.
enum Block {
    Unrolled(v2::Ran),
    RunBlock { included: Vec<Tx>, failed: BTreeMap<usize, String> },
}

/// One fuzzed run of the state machine: the ops' txs in blocks, as execution applies them.
struct Sm<'a> {
    fx: &'a SmFixture,
    state: State,
    height: u64,
    txs: Vec<Tx>,
    applied: usize,
    /// Every block's root, for the determinism check (on the v2 fixture, each followed by the
    /// digest of the block's program receipts).
    roots: Vec<[u8; 32]>,
    halted: bool,
    /// How the v2 fixture's blocks run (the v1 fixture's always run `State::run_block`).
    exec: Exec,
    /// The v2 fixture's program transactions by kind and outcome code, as their receipts say.
    seen: BTreeMap<(&'static str, &'static str), usize>,
    /// Why admission refused each program transaction the unrolled block skipped, in order.
    refused: Vec<String>,
    /// The first receipt of each kind and outcome (the seeds' source).
    kept: BTreeMap<(&'static str, &'static str), crate::programs::receipt::ProgramReceipt>,
    /// What the guests' storage must read, from the transactions that applied (`oracle`).
    guests: oracle::Guests,
    /// The fuel the input's program transactions have asked for so far (`v2::FUEL_CAP`).
    fuel_signed: u64,
    /// How many of the queued txs of each sender are signed for a fee the chain must refuse (a drop
    /// short of the charge, or an idle chain's): a refused tx takes no nonce, so the sender's next
    /// one signs the same.
    doomed: BTreeMap<String, u64>,
    /// The machine couldn't run the input (a Setup-class fault twice, `v2::run_block`): not a
    /// finding, and the run stops there.
    resource_error: bool,
    /// The v2 fixture's empty blocks that have run with every check so far, and whether the next
    /// ones are made quick (`Sm::quick_block`).
    full_empty: u32,
    quick: bool,
    /// Tests only: a change made to the state right after each transaction the unrolled block
    /// applies, to show an oracle sees it.
    tamper: Option<fn(&mut State, &Tx)>,
}

impl<'a> Sm<'a> {
    /// A run from the fixture's state.
    fn new(fx: &'a SmFixture) -> Sm<'a> {
        Sm { fx, state: fx.state.clone(), height: fx.height, txs: vec![], applied: 0, roots: vec![], halted: false, exec: Exec::Unrolled, seen: BTreeMap::new(), refused: vec![], kept: BTreeMap::new(), guests: Default::default(), fuel_signed: 0, doomed: BTreeMap::new(), resource_error: false, full_empty: 0, quick: false, tamper: None }
    }

    fn proposer(&self) -> String {
        self.fx.validator.address.clone()
    }

    /// Queues a tx from `from` signed (in name: the state machine doesn't check signatures,
    /// admission does) by `signer`, at that sender's next nonce in this block.
    fn tx(&mut self, signer: &Who, from: &str, tip: u64, action: Action) {
        self.tx_paying(signer, from, tip, v2::Fee::Idle, action);
    }

    /// [`Sm::tx`] with a maximum fee of `fee`: what a tx pays on an idle chain (`Tx::charge`), or
    /// far above, or the exact charge in the block it will be in (`econ::charge` says what the node
    /// charges there; the oracles say what it should), or a drop less.
    fn tx_paying(&mut self, signer: &Who, from: &str, tip: u64, fee: v2::Fee, action: Action) {
        let nonce = self.next_nonce(from);
        let mut tx = Tx { chain_id: self.fx.chain.into(), genesis_hash: "00".repeat(32), from: from.into(), nonce, fee: 0, tip, valid_until: self.height + 10, action, memo: String::new(), public_key: signer.public_key.clone(), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        // The block this tx is queued for is the next one, on the state as it is now.
        let charge = |tx: &Tx| crate::econ::charge(&self.state, tx, self.height + 1).unwrap_or(u64::MAX);
        tx.fee = match fee {
            v2::Fee::Idle => tx.charge().unwrap_or(u64::MAX),
            v2::Fee::Max => v2::PROGRAM_FEE,
            v2::Fee::Exact => charge(&tx),
            v2::Fee::Short => charge(&tx).saturating_sub(1),
        };
        if matches!(fee, v2::Fee::Short | v2::Fee::Idle) && v2_is_program_tx(&tx) {
            *self.doomed.entry(from.to_string()).or_default() += 1;
        }
        self.queue(tx);
    }

    /// The nonce of the next tx `from` signs in this block: its account's, past those it has
    /// queued that will be taken.
    fn next_nonce(&self, from: &str) -> u64 {
        let queued = self.txs.iter().filter(|t| t.from == from).count() as u64;
        self.state.account(from).nonce + queued - self.doomed.get(from).copied().unwrap_or(0)
    }

    /// Queues `tx`, ending the block at 16.
    fn queue(&mut self, tx: Tx) {
        self.txs.push(tx);
        if self.txs.len() >= 16 {
            self.end_block();
        }
    }

    /// Executes the queued txs as the next block (after a dry run that's rolled back, as a
    /// proposal's is), runs the end-of-block steps and commits, then checks the state.
    fn end_block(&mut self) {
        if self.halted {
            return;
        }
        // The v2 fixture's blocks are bounded (`v2::BLOCK_CAP`): an input of slow blocks is not slower.
        if self.fx.programs.is_some() && self.height - self.fx.height >= v2::BLOCK_CAP {
            self.txs.clear();
            self.halted = true;
            return;
        }
        let height = self.height + 1;
        let proposer = self.proposer();
        let chain = self.fx.chain;
        let txs = std::mem::take(&mut self.txs);
        self.doomed.clear();
        // A pause's spans are hundreds of blocks, so the v2 fixture lets empty ones pass quickly.
        if self.fx.programs.is_some() && txs.is_empty() {
            if self.quick || self.full_empty >= v2::FULL_EMPTY_BLOCKS {
                return self.quick_block(height, &proposer);
            }
            self.full_empty += 1;
        }
        // Dry run + rollback (proposals, mempool checks) must leave the state exactly as it was.
        let before = self.state.full_root();
        let mut j = crate::chain::Journal::default();
        for tx in &txs {
            let _ = self.state.apply_j(&mut j, tx, chain, &proposer, height, crate::chain::ApplyMode::Mempool);
        }
        self.state.rollback(j);
        assert_eq!(self.state.full_root(), before, "a rolled-back dry run changed the state");
        // Exactly as execution runs a block (`State::run_block`).
        let hash = hex::encode(crate::crypto::hash(&height.to_le_bytes()));
        let mut applied = 0;
        let fx = self.fx;
        let ran = match (&fx.programs, self.exec) {
            (None, _) => {
                self.state.run_block(chain, height, &hash, &proposer, height, height - 1, &txs, |_, _, r| applied += matches!(r, Ok(Outcome::Applied)) as usize);
                None
            }
            (Some(_), Exec::Unrolled) => match v2::run_block(self, height, &hash, &proposer, &txs, &mut applied) {
                Some(ran) => Some(Block::Unrolled(ran)),
                None => return self.stop(),
            },
            (Some(_), Exec::RunBlock) => {
                let (mut included, mut failed) = (vec![], BTreeMap::new());
                self.state.run_block(chain, height, &hash, &proposer, height, height - 1, &txs, |tx, _, r| {
                    if let Ok(outcome) = r {
                        applied += matches!(outcome, Outcome::Applied) as usize;
                        if let Outcome::Failed(why) = outcome {
                            failed.insert(included.len(), why);
                        }
                        included.push(tx.clone());
                    }
                });
                if v2::fault_of(&self.state, height).is_err() {
                    return self.stop();
                }
                Some(Block::RunBlock { included, failed })
            }
        };
        self.applied += applied;
        let commit = self.state.commit_block().expect("an in-memory tree can't fail");
        assert_eq!(commit.root, self.state.full_root(), "incremental state root diverged from a full rebuild at height {height}");
        if let Err(e) = self.state.check_invariants().and_then(|_| self.state.check_record_sizes()) {
            panic!("state invariant broken at height {height}: {e}");
        }
        self.roots.push(commit.root);
        match ran {
            None => {}
            Some(Block::Unrolled(ran)) => {
                let digest = v2::check_block(self, height, ran);
                self.roots.push(digest);
            }
            Some(Block::RunBlock { included, failed }) => {
                let digest = v2::digest_block(self, height, &included, &failed);
                self.roots.push(digest);
            }
        }
        self.height = height;
        // A binary that can't run the next height stops, as the node does (`upgrade::after_block`).
        // The v1 genesis schedules no version, so its binary runs `SUPPORTED_PROTOCOL_VERSION`; the
        // v2 one schedules v2, which that binary runs too (`upgrade::supported_version`).
        self.halted = crate::upgrade::check_next_with(&self.state, height + 1, self.fx.supported).is_err();
    }

    /// An empty block of the v2 fixture without the per-block checks (the dry run, the rebuilt root,
    /// the invariants, the oracles): only its end-of-block steps and its commit. Its root and the
    /// digest of its (no) receipts join `roots` as any block's do, on either path.
    fn quick_block(&mut self, height: u64, proposer: &str) {
        let hash = hex::encode(crate::crypto::hash(&height.to_le_bytes()));
        let version = crate::upgrade::version_at(&self.state, height);
        self.state.end_block(&crate::chain::BlockInfo { chain_id: self.fx.chain, height, version, hash: &hash, proposer, view: height, parent_view: height - 1, skipped: &[], tx_bytes: 0 });
        let commit = self.state.commit_block().expect("an in-memory tree can't fail");
        self.roots.push(commit.root);
        self.roots.push(v2::empty_digest());
        self.height = height;
        self.halted = crate::upgrade::check_next_with(&self.state, height + 1, self.fx.supported).is_err();
    }

    /// The machine can't run this input (`v2::Resource`): the run ends here, unjudged.
    fn stop(&mut self) {
        self.resource_error = true;
        self.halted = true;
    }

    /// Interprets one op as a tx (or blocks). `amt` and the indexes come from the fuzzer.
    fn op(&mut self, kind: u8, a: u8, b: u8, amt: u64, raw: &[u8]) {
        let f = self.fx;
        let (ai, bi) = (a as usize % SM_USERS, b as usize % SM_USERS);
        let (user, other) = (&f.users[ai], f.users[bi].address.clone());
        let me = user.address.clone();
        let agent = &f.agents[b as usize % SM_AGENTS];
        let admin = &f.validator;
        let fang = |n: u64| n * types::DROPS_PER_FANG;
        let h = self.height;
        let tip = (a % 4) as u64;
        let newcomer = f.newcomers[b as usize % 8].clone();
        let hunt = amt % self.state.next_hunt_id.max(1);
        let claim = amt % self.state.poc.next_claim_id.max(1);
        let validator_addr = |s: &Self| s.state.validators.keys().nth(b as usize % s.state.validators.len().max(1)).cloned().unwrap_or_else(|| f.validator.address.clone());
        // The v2 fixture's ops are numbered after v1's (`v2::OPS`); v1's decode as before.
        let kind = match kind % if f.programs.is_some() { v2::OPS } else { 40 } {
            36 => 17,
            37 => 18,
            38 | 39 => 25,
            k => k,
        };
        let action = match kind {
            0 => Action::Transfer { to: other, amount: fang(amt % 50) + 1 },
            1 => Action::Transfer { to: newcomer, amount: fang(1) },
            2 => Action::RegisterName { name: format!("name{}", b % 8) },
            3 => Action::AddKey { key_id: agent.key_id.clone(), label: "agent".into(), role: KeyRole::Session { limit: fang(amt % 20), period_blocks: 1 + amt % 10, expires_at: h + 50 }, public_key: agent.public_key.clone(), pop: f.agent_pops[b as usize % SM_AGENTS][ai].clone() },
            4 => Action::RemoveKey { key_id: agent.key_id.clone() },
            5 => Action::RegisterFamiliar { key_id: agent.key_id.clone(), label: "claude".into() },
            6 => return self.tx(agent, &me, tip, Action::Transfer { to: other, amount: fang(amt % 3) }),
            7 => Action::CreateHunt { repo: "jackdishman/blockula".into(), issue: 1 + b as u64, amount: fang(10 + amt % 20), expires_at: h + 2 + amt % 10, title: "a hunt".into() },
            8 => Action::FundHunt { hunt_id: hunt, amount: fang(1 + amt % 3) },
            9 => Action::AwardHunt { hunt_id: hunt, to: other, pr: 1 + amt, familiar_key: (amt % 2 == 0).then(|| agent.key_id.clone()) },
            10 if amt % 2 == 0 => return self.tx(admin, &admin.address.clone(), tip, Action::CancelHunt { hunt_id: hunt }),
            10 => Action::CancelHunt { hunt_id: hunt },
            11 => Action::LinkGithub { handle: format!("user{ai}"), gist_id: format!("abc{ai}") },
            12 => Action::UnlinkGithub { handle: format!("user{}", if amt % 2 == 0 { ai } else { bi }) },
            13 => return self.tx(admin, &admin.address.clone(), tip, Action::GrantSoul { to: other, amount: (1 + amt % 30) * crate::poc::SOUL, reason: "fuzz".into() }),
            14 => return self.tx(admin, &admin.address.clone(), tip, Action::SetRepo { repo: "jackdishman/blockula".into(), weight: amt % 3 }),
            15 => Action::Vouch { account: if amt % 3 == 0 { other } else { newcomer } },
            16 => Action::Unvouch { account: if amt % 3 == 0 { other } else { newcomer } },
            17 => Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr: 1 + b as u64 + 10 * (amt % 4), familiar_key: None },
            18 => {
                // Mostly the oldest pending claim nobody attested (or will in this block), so
                // attestations land.
                let queued: Vec<u64> = self.txs.iter().filter_map(|t| if let Action::AttestClaim { claim_id, .. } = t.action { Some(claim_id) } else { None }).collect();
                let pending = self.state.poc.claims.values().find(|c| c.status == crate::poc::ClaimStatus::Pending && !c.attestations.contains_key(&admin.address.clone()) && !queued.contains(&c.id)).map(|c| c.id);
                let claim_id = pending.filter(|_| amt % 5 != 0).unwrap_or(claim);
                let ok = amt % 6 != 0;
                let (merge_sha, note) = if ok { ("ab".repeat(20), String::new()) } else { (String::new(), "closed".into()) };
                return self.tx(admin, &admin.address.clone(), tip, Action::AttestClaim { claim_id, ok, merge_sha, note });
            }
            19 => Action::JoinJury { bond: crate::poc::JUROR_BOND },
            20 => Action::LeaveJury {},
            21 => Action::FundPool { amount: fang(1 + amt % 30) },
            22 => Action::WithdrawVested {},
            23 => return self.tx(admin, &admin.address.clone(), tip, Action::Clawback { account: other, reason: "fuzz".into() }),
            24 if amt % 5 == 0 => return self.tx(admin, &admin.address.clone(), tip, Action::FlagSybil { account: other, reason: "fuzz".into() }),
            24 => return self.tx(admin, &admin.address.clone(), tip, Action::RejectClaim { claim_id: claim, reason: "fuzz".into() }),
            25 => {
                // A seated juror seals, then reveals, mostly-'a' votes in the open Séance.
                let Some(id) = self.state.poc.open_seance else { return self.end_block() };
                let seance = &self.state.poc.seances[&id];
                let Some(seat) = seance.jurors.get(&me) else { return };
                let votes: String = seat.pairs.iter().enumerate().map(|(i, _)| if (amt >> (i % 16)) & 1 == 0 { 'a' } else { 'b' }).collect();
                let spam: Vec<u64> = if amt % 7 == 0 { seance.claims.first().copied().into_iter().collect() } else { vec![] };
                if seat.commitment.is_none() && h < seance.commit_until {
                    Action::CommitVotes { seance_id: id, commitment: types::seance_commitment(id, &me, &votes, &spam, "salt"), pairs_digest: crate::poc::seat_pairs_digest(&self.state, id, &me, &seat.pairs) }
                } else {
                    Action::RevealVotes { seance_id: id, votes, spam, salt: "salt".into() }
                }
            }
            26 => {
                let (key, pop) = f.candidates[ai].clone();
                Action::RegisterValidator { consensus_key: key, p2p_addr: "203.0.113.9:7071".into(), http_addr: "https://v.example".into(), bond: self.state.chain_params.min_self_bond + fang(amt % 5), pop }
            }
            27 => {
                let v = validator_addr(self);
                let action = match amt % 3 {
                    0 => Action::ApproveValidator { validator: v },
                    1 => Action::RemoveValidator { validator: v },
                    _ => Action::Unbond { validator: v },
                };
                let (signer, from) = if amt % 3 == 2 { (user, me) } else { (admin, admin.address.clone()) };
                return self.tx(signer, &from, tip, action);
            }
            // Protocol v1 (tags 30–49): whatever each module does with them.
            28 => {
                let v = validator_addr(self);
                match amt % 6 {
                    0 => Action::Delegate { validator: v, amount: fang(1 + amt % 50) },
                    // By shares, or by an amount the chain converts at its own price (audit gap-7-5).
                    1 if (amt / 6) % 2 == 0 => Action::Undelegate { validator: v, shares: 1 + amt, amount: 0 },
                    1 => Action::Undelegate { validator: v, shares: 0, amount: fang(1 + amt % 50) },
                    2 => Action::Redelegate { from_validator: v, to_validator: f.validator.address.clone(), shares: 1 + amt },
                    3 => Action::WithdrawUnbonded {},
                    4 => Action::UpdateValidator { commission_bps: amt % 2_000, p2p_addr: String::new(), http_addr: String::new() },
                    _ => Action::Unjail { validator: v },
                }
            }
            29 => match amt % 3 {
                0 => {
                    let kinds = [
                        ProposalKind::ParamChange { key: "max_validators".into(), value: 2 + amt % 10 },
                        ProposalKind::TreasurySpend { to: other.clone(), amount: fang(1) },
                        ProposalKind::SetRepo { repo: "jackdishman/blockula".into(), weight: 1 + amt % 3 },
                        ProposalKind::GrantSoul { to: other.clone(), amount: crate::poc::SOUL, reason: "fuzz".into() },
                        ProposalKind::RefundHunt { hunt_id: hunt },
                        ProposalKind::ScheduleUpgrade { version: 2, activation_height: h + 1_000 },
                        ProposalKind::ActivateFeature { feature: "ghost".into() },
                        ProposalKind::Text {},
                        // What's scheduled, if anything (else refused when proposed).
                        match self.state.upgrade.schedule.last() {
                            Some(u) => ProposalKind::CancelUpgrade { version: u.version, activation_height: u.activation_height },
                            None => ProposalKind::CancelUpgrade { version: 2, activation_height: h + 1_000 },
                        },
                        // Refused unless the account is flagged (the appeal path, tag 14).
                        ProposalKind::UnflagSybil { account: other, reason: "fuzz".into() },
                    ];
                    let kind = kinds[b as usize % kinds.len()].clone();
                    Action::Propose { kind, deposit: fang(1_000 + amt), title: "fuzz".into(), url: String::new() }
                }
                // Bound to the proposal as stored, so most votes land (a wrong digest is refused).
                1 => Action::Vote { proposal_id: amt % 4, aye: b % 2 == 0, amount: fang(1 + amt % 100), conviction: (b % 7) as u8, proposal_digest: self.state.governance.proposals.get(&(amt % 4)).map(crate::governance::digest_of).unwrap_or_else(|| "00".repeat(32)) },
                _ => Action::Unlock {},
            },
            30 => match amt % 3 {
                0 => Action::MultisigCreate { owners: vec![user.key_id.clone(), agent.key_id.clone()], threshold: 1 + (b % 2) },
                1 => Action::MultisigPropose { action: Box::new(Action::Transfer { to: other, amount: fang(1) }) },
                _ => Action::MultisigApprove { id: amt % 4, action_digest: self.state.governance.multisig_proposals.get(&(amt % 4)).map(|p| types::action_digest(&p.action)).unwrap_or_else(|| "00".repeat(32)) },
            },
            31 => match amt % 3 {
                0 => Action::RenewName { name: format!("name{}", b % 8), years: 1 + (b % 3) },
                1 => Action::TransferName { name: format!("name{}", b % 8), to: other },
                _ => Action::ReleaseName { name: format!("name{}", b % 8) },
            },
            32 => {
                let (signer, from) = (admin, admin.address.clone());
                let epoch = self.state.params.epoch_of(h) + (amt % 3);
                let action = match amt % 7 {
                    0 => Action::BeaconCommit { epoch, commitment: hex::encode(crate::crypto::hash(&amt.to_le_bytes())) },
                    1 => Action::BeaconReveal { epoch, secret: hex::encode((amt as u128).to_le_bytes().repeat(2)) },
                    2 => Action::AttestRevert { claim_id: claim, revert_sha: "cd".repeat(20) },
                    3 => Action::ClaimDistribution { distribution_id: amt % 3, amount: fang(1), proof: vec![] },
                    // A small set's readiness and admission (tags 60–61).
                    4 => Action::ValidatorReady { epoch },
                    5 => Action::AdmitValidator { validator: validator_addr(self), epoch },
                    _ => Action::ReportEquivocation { evidence: hex::encode(raw) },
                };
                return self.tx(signer, &from, tip, action);
            }
            33 => match Action::from_bytes(raw) {
                // Any action the wire form can carry, with fuzzer-chosen fields.
                Ok(action) => action,
                Err(_) => return,
            },
            34 => {
                self.end_block();
                for _ in 0..amt % 4 {
                    self.end_block();
                }
                return;
            }
            // A tx the state machine must refuse (a stranger's key, a stale nonce), not apply.
            35 if amt % 2 == 0 => return self.tx(agent, &me, tip, Action::RegisterName { name: format!("sneaky{}", b % 4) }),
            35 => {
                let mut t = Tx { chain_id: f.chain.into(), genesis_hash: "00".repeat(32), from: me.clone(), nonce: self.state.account(&me).nonce.wrapping_sub(1), fee: 0, tip, valid_until: h + 10, action: Action::WithdrawVested {}, memo: String::new(), public_key: user.public_key.clone(), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
                t.fee = t.charge().unwrap_or(u64::MAX);
                self.txs.push(t);
                return;
            }
            // The v2 fixture's program ops (kinds 40–46): `v2.rs`.
            v2::OP_CALL..=v2::OP_GOV => return v2::op(self, kind, a, b, amt, raw),
            _ => return,
        };
        self.tx(user, &me, tip, action);
    }
}

/// Runs the ops in `data` from the v1 state machine fixture's genesis: every block's root, and how
/// many txs applied. Exactly as the target has always run (v1's op stream: kinds modulo 40).
fn sm_run(data: &[u8]) -> Sm<'static> {
    let mut sm = Sm::new(sm_fixture(None));
    sm.exec = Exec::RunBlock;
    sm_interpret(&mut sm, data, 40);
    sm_finish(sm)
}

/// The ops of `data` on `sm`, kinds taken modulo `kinds`.
fn sm_interpret(sm: &mut Sm<'_>, data: &[u8], kinds: u8) {
    let mut rest = data;
    for _ in 0..SM_MAX_OPS {
        let Some((op, tail)) = rest.split_first_chunk::<SM_OP>() else { break };
        rest = tail;
        let [kind, a, b, lo, hi] = *op;
        let amt = u16::from_le_bytes([lo, hi]) as u64;
        let k = kind % kinds;
        // The raw-action ops (32, 33) take `b % 64 + 1` more bytes: an action's wire encoding.
        let raw = if k == 33 || k == 32 {
            let n = (b as usize % 64 + 1).min(rest.len());
            let (raw, tail) = rest.split_at(n);
            rest = tail;
            raw
        } else if (v2::OP_CALL..=v2::OP_GOV).contains(&k) && kinds == v2::OPS {
            // A program op's payload: a length byte, then that many bytes (fewer at the end).
            let (n, tail) = rest.split_first().map_or((0, rest), |(n, tail)| (*n as usize, tail));
            let (raw, tail) = tail.split_at(n.min(tail.len()));
            rest = tail;
            raw
        } else {
            &[]
        };
        sm.op(kind, a, b, amt, raw);
    }
}

/// The run's last block, and the final state's snapshot round trip (what fast sync rebuilds from).
fn sm_finish(mut sm: Sm<'static>) -> Sm<'static> {
    sm.end_block();
    let mut bytes = vec![];
    snapshot::write_state(&sm.state, &mut bytes).unwrap();
    let back = snapshot::read_state(&mut bytes.as_slice()).expect("a state the state machine built must decode from its snapshot");
    assert_eq!(back.root_hash(), sm.state.root_hash(), "the snapshot of a state rebuilds another root");
    sm
}

/// The state machine (MAINNET-DESIGN 2, SEC-3): fuzzer-chosen sequences of txs of every kind (the
/// raw-action op takes any action the wire form carries) from a few accounts, agents and an
/// admin, in blocks across epoch ends and Séances. After every block the incremental root must
/// equal a full rebuild, a rolled-back dry run must leave no trace, and the invariants (supply
/// conservation, record bounds) must hold. One input in four runs twice: the same ops must give
/// the same roots (nondeterminism, e.g. from hash-map order, splits a chain).
///
/// Protocol v1, programs not in it. The v2 fixture, with program ops, is `state_machine_programs`.
pub fn state_machine(data: &[u8]) -> bool {
    let run = sm_run(data);
    if crate::crypto::hash(data)[0] % 4 == 0 {
        let again = sm_run(data);
        assert_eq!(run.roots, again.roots, "the same txs from the same genesis gave different state roots");
    }
    run.applied > 0
}

/// `state_machine_programs`, on a v2 fixture whose profile is `data`'s first byte (`v2.rs`'s
/// table): a run of `data`'s other bytes as ops, 0–39 as v1's and 40–46 program ops, with every
/// check of `state_machine`'s and the oracles of `oracle.rs` (docs/PROGRAMS-V0.md §10.4).
fn smp_run(data: &[u8], exec: Exec) -> Sm<'static> {
    let (profile, ops) = data.split_first().map_or((v2::Profile::of(0), &[][..]), |(first, ops)| (v2::Profile::of(*first), ops));
    let mut sm = Sm::new(sm_fixture(Some(profile)));
    sm.exec = exec;
    sm_interpret(&mut sm, ops, v2::OPS);
    sm_finish(sm)
}

/// The v2 fixture's state machine: [`smp_run`], then for one input in four a second run through
/// `State::run_block`, which must reach the same roots and the same digests of each block's
/// receipts as the unrolled blocks, and so does hold `State::run_block` to the unrolled copy. An
/// input the machine can't run (a Setup-class fault, not a finding) is not accepted and not
/// judged further. Accepted: some transaction applied.
pub fn state_machine_programs(data: &[u8]) -> bool {
    let run = smp_run(data, Exec::Unrolled);
    if !run.resource_error && crate::crypto::hash(data)[0] % 4 == 0 {
        rerun_must_agree(data, &run);
    }
    run.applied > 0
}

/// The determinism rerun: `data` through `State::run_block`, against `run`.
fn rerun_must_agree(data: &[u8], run: &Sm<'_>) {
    let again = smp_run(data, Exec::RunBlock);
    if !again.resource_error {
        assert_eq!(run.roots, again.roots, "the unrolled blocks and State::run_block gave different state roots or receipts from the same txs");
    }
}

/// How many program transactions of each kind ended each way (`("call_program", "aborted")`), as
/// their receipts say, when `data` runs on the v2 fixture: what its seeds and the fuzzer reach,
/// for `tests/harness.rs` and the campaign's record. It also runs the determinism rerun, whatever
/// the input.
pub fn state_machine_programs_receipts(data: &[u8]) -> BTreeMap<(&'static str, &'static str), usize> {
    let run = smp_run(data, Exec::Unrolled);
    assert!(!run.resource_error, "this machine couldn't run the input");
    rerun_must_agree(data, &run);
    run.seen
}

/// `state_machine_programs` with `tamper` called on the state after every transaction the unrolled
/// block applies: for `tests/harness.rs`, which shows each oracle sees the change a bug in the node
/// would make. Returns whether some transaction applied. A run the machine can't make (a Setup-class
/// fault) returns `None`.
pub fn state_machine_programs_tampered(data: &[u8], tamper: fn(&mut State, &Tx)) -> Option<bool> {
    let (profile, ops) = data.split_first().map_or((v2::Profile::of(0), &[][..]), |(first, ops)| (v2::Profile::of(*first), ops));
    let mut sm = Sm::new(sm_fixture(Some(profile)));
    sm.tamper = Some(tamper);
    sm_interpret(&mut sm, ops, v2::OPS);
    let sm = sm_finish(sm);
    (!sm.resource_error).then_some(sm.applied > 0)
}

/// The numbers `oracle::spec` copies from the spec, with the names `tests/harness.rs` holds them to
/// the node's `SCHEDULE_V0` and limits by.
pub fn oracle_spec_numbers() -> Vec<(&'static str, u64)> {
    use oracle::spec;
    vec![
        ("load_base", spec::LOAD_BASE),
        ("load_per_byte", spec::LOAD_PER_BYTE),
        ("upload_base", spec::UPLOAD_BASE),
        ("upload_per_byte", spec::UPLOAD_PER_BYTE),
        ("rate_scale", spec::RATE_SCALE),
        ("max_reservation", spec::MAX_RESERVATION),
        ("max_fuel_rate", spec::MAX_FUEL_RATE),
        ("floor_rate", spec::DEFAULT_FLOOR_RATE),
        ("max_block_fuel", spec::DEFAULT_MAX_BLOCK_FUEL),
        ("max_tx_fuel", spec::DEFAULT_MAX_TX_FUEL),
        ("value_cap", spec::DEFAULT_VALUE_CAP),
    ]
}

/// Every block's root of `state_machine`'s run of `data` and how many txs applied: for
/// `tests/harness.rs`, which holds the v1 seeds' runs to what the harness of 9e8cd806 (before any
/// of programs' fuzzing) gave.
pub fn state_machine_trace(data: &[u8]) -> (Vec<[u8; 32]>, usize) {
    let run = sm_run(data);
    (run.roots, run.applied)
}

/// Why admission refused each program transaction of a run of `data` on the v2 fixture, in order:
/// the texts `oracle::check_refused` reads, and so what `tests/harness.rs` shows is still said.
pub fn state_machine_programs_refusals(data: &[u8]) -> Vec<String> {
    smp_run(data, Exec::Unrolled).refused
}

/// Where a run of `data` on the v2 fixture ended, for `tests/harness.rs`: the height, the last run
/// of pauses (`started`, `from`, `until`, how many signed), programs' fuel rate and whether the
/// feature is on.
pub fn state_machine_programs_end(data: &[u8]) -> (u64, Option<(u64, u64, u64, usize)>, u64, bool) {
    let run = smp_run(data, Exec::Unrolled);
    let meta = run.state.programs.meta.as_ref().expect("the v2 fixture has programs");
    (run.height, meta.pause.as_ref().map(|p| (p.started, p.from, p.until, p.signers.len())), meta.fuel_rate, crate::upgrade::is_active(&run.state, crate::upgrade::PROGRAMS_FEATURE))
}

/// A state machine input from `(kind, a, b, amt)` ops (and raw bytes after a raw-action op).
fn sm_ops(ops: &[(u8, u8, u8, u16)]) -> Vec<u8> {
    ops.iter().flat_map(|&(k, a, b, amt)| [k, a, b, amt.to_le_bytes()[0], amt.to_le_bytes()[1]]).collect()
}

// --------------------------------------------------------------------------- program tx codec

/// The height `program_tx_codec` admits its txs at: the first block after genesis.
const PROGRAM_HEIGHT: u64 = 1;
/// The most the envelope's tx may be charged (1 FANG): far above the fixtures' base fee for the
/// largest tx, an upload of `programs::MAX_CODE_TX_BYTES`, so no action outgrows it.
const PROGRAM_FEE: u64 = types::DROPS_PER_FANG;

/// What `program_tx_codec` checks against: two chains on which programs are dormant, and the tx
/// each input's action travels in.
struct ProgramFixture {
    /// The fixture chain (protocol v1, as testnet-2 runs), and the same chain born at v2 with
    /// programs off (as `blockula-1` is). On both, `programs::status` is `Dormant` at
    /// [`PROGRAM_HEIGHT`].
    dormant: [(&'static str, State); 2],
    /// The tx around each action: from the fixture's funded account (its genesis key, at nonce 0),
    /// at most [`PROGRAM_FEE`] and no tip, valid at [`PROGRAM_HEIGHT`], with no memo and a
    /// signature of zeros. `State::apply` doesn't check signatures (gossip, the API and block
    /// verification do before it, which `tx_wire` fuzzes), so the dormant chains refuse a program
    /// action for what it is.
    envelope: Tx,
    /// The envelope's wire form around its action: everything before the action, and everything
    /// after it (the empty memo, the public key and the signature).
    head: Vec<u8>,
    tail: Vec<u8>,
}

fn program_fixture() -> &'static ProgramFixture {
    static F: OnceLock<ProgramFixture> = OnceLock::new();
    F.get_or_init(|| {
        let funded = Keys::from_seed(VALIDATOR_SEED);
        let mut v2 = serde_json::to_value(genesis()).unwrap();
        v2["upgrades"] = json!([{ "version": crate::upgrade::PROGRAMS_VERSION, "activation_height": PROGRAM_HEIGHT }]);
        let v2: Genesis = serde_json::from_value(v2).expect("the v2 fixture genesis no longer parses: update harness::program_fixture");
        v2.check().expect("the v2 fixture genesis is invalid: update harness::program_fixture");
        let dormant = [("the v1 chain", genesis().initial_state()), ("the chain born at v2 with programs off", v2.initial_state())];
        assert_eq!(crate::upgrade::version_at(&dormant[1].1, PROGRAM_HEIGHT), crate::upgrade::PROGRAMS_VERSION, "the v2 fixture doesn't run v2");
        let envelope = Tx { chain_id: CHAIN_ID.into(), genesis_hash: genesis_hash().into(), from: funded.address(), nonce: 0, fee: PROGRAM_FEE, tip: 0, valid_until: VALID_UNTIL, action: Action::ProgramsPause {}, memo: String::new(), public_key: hex::encode(funded.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        // The envelope holds tag 69 alone, so its signing bytes end with that byte and the empty
        // memo (a u16 length of 0), and the key and signature follow them.
        let (signing, wire) = (envelope.signing_bytes(), envelope.to_bytes());
        let (head, tail) = (signing[..signing.len() - 3].to_vec(), wire[signing.len() - 2..].to_vec());
        assert_eq!([&head[..], &[69], &tail[..]].concat(), wire, "the envelope's wire form isn't head ‖ action ‖ tail: update harness::program_fixture");
        // So a program action's refusal is the action's: the envelope admits a transfer, its fee
        // covers the base fee of the largest tx the format lets through, and programs are
        // dormant, on both chains.
        for (name, st) in &dormant {
            let transfer = Tx { action: Action::Transfer { to: Keys::from_seed([0x43; 32]).address(), amount: types::DROPS_PER_FANG }, ..envelope.clone() };
            let outcome = st.clone().apply(&transfer, CHAIN_ID, &funded.address(), PROGRAM_HEIGHT);
            assert!(matches!(outcome, Ok(Outcome::Applied)), "{name}: a transfer in the program_tx_codec envelope doesn't apply ({outcome:?}): update harness::program_fixture");
            let largest = crate::econ::base_fee(st, PROGRAM_HEIGHT, crate::programs::MAX_CODE_TX_BYTES as u64);
            assert!(largest <= PROGRAM_FEE, "{name}: the envelope's fee doesn't cover the base fee of an upload of the largest size ({largest} drops): raise PROGRAM_FEE");
            assert_eq!(crate::programs::status(st, PROGRAM_HEIGHT), crate::programs::Status::Dormant, "{name}: programs aren't dormant");
        }
        ProgramFixture { dormant, envelope, head, tail }
    })
}

/// Whether programs' rules refuse `action` while programs are dormant: a program action (tags
/// 62–64 and 69), or a parameter change of a `program_*` key, bare or proposed by a multisig,
/// which the codec lets through and admission refuses until programs exist (docs/PROGRAMS-V0.md
/// §4.5, `programs::params::admit`).
fn is_programs_action(action: &Action) -> bool {
    let program_param = |a: &Action| matches!(a, Action::Propose { kind: ProposalKind::ParamChange { key, .. }, .. } if key.starts_with(crate::programs::params::PREFIX));
    action.is_program() || program_param(action) || matches!(action, Action::MultisigPropose { action } if program_param(action))
}

/// Programs' actions (tags 62–64 and 69, docs/PROGRAMS-V0.md §2) as blocks, gossip and `POST
/// /api/tx` carry them: every node decodes and checks them, programs on or off (§10.4). An input
/// is an action's wire encoding (`Action::to_bytes`, the tag first) or, starting with `{` (which
/// no tag is), its JSON.
///
/// The wire decoder (`Action::from_bytes`) never panics, and what it takes is canonical (it
/// encodes back to the same bytes). Inside a multisig proposal (tag 43 first, `decode_at`'s
/// `nested`) a program tag never decodes, and any action a proposal may hold decodes as itself.
/// In a tx's place (the envelope's wire form around the input) the gossip decoder
/// (`Tx::from_bytes`) takes every action the action decoder takes, as it is, within the tx's size
/// bound, except a private transfer or an unshield (which travel unsigned only), and only
/// canonical bytes. Then, for an action from either form, [`program_action`]'s checks.
/// Accepted: one of programs' actions that a tx carries through its wire form.
pub fn program_tx_codec(data: &[u8]) -> bool {
    let f = program_fixture();
    if data.first() == Some(&b'{') {
        let Ok(action) = serde_json::from_slice::<Action>(data) else { return false };
        return program_action(f, action);
    }
    let decoded = Action::from_bytes(data);
    if let Ok(a) = &decoded {
        assert_eq!(a.to_bytes(), data, "Action::from_bytes accepted a non-canonical encoding");
    }
    let nested = Action::from_bytes(&[&[43], data].concat());
    if matches!(data.first(), Some(62..=64 | 69)) {
        assert!(nested.is_err(), "a multisig proposal decoded holding a program action");
    }
    if let Some(a) = decoded.as_ref().ok().filter(|a| !(a.is_program() || a.is_ghost() || matches!(a, Action::MultisigPropose { .. }))) {
        assert_eq!(nested.as_ref().ok(), Some(&Action::MultisigPropose { action: Box::new(a.clone()) }), "an action decodes otherwise inside a multisig proposal");
    }
    let wire = [&f.head[..], data, &f.tail[..]].concat();
    let in_tx = Tx::from_bytes(&wire);
    if let Ok(tx) = &in_tx {
        assert_eq!(tx.to_bytes(), wire, "Tx::from_bytes accepted a non-canonical encoding");
    }
    let Ok(action) = decoded else { return false };
    if action.is_unsigned() {
        // Unshields and private transfers travel only in their own unsigned form (`ghost_tx`'s),
        // which is all `Tx::to_bytes` gives them, so the signed form must refuse them (review F-1).
        assert!(in_tx.is_err(), "the gossip decoder took a private transfer or unshield in the signed form");
    } else {
        let tx = Tx { action: action.clone(), ..f.envelope.clone() };
        assert_eq!(tx.to_bytes(), wire, "a tx's wire form doesn't carry its action's encoding in place");
        if wire.len() <= tx.max_bytes() {
            assert_eq!(in_tx.ok(), Some(tx), "the gossip decoder refused, or changed, an action the action decoder takes");
        }
    }
    program_action(f, action)
}

/// The checks of [`program_tx_codec`] on a decoded action, from the wire or JSON. Its JSON gives
/// back the same action; a multisig proposal can't hold a program action (`check_format`'s
/// `nested`). In the envelope: the stateless checks (`check_tx_stateless`, as gossip runs them,
/// an upload's interface decoded) never panic, and a tx that passes `check_format` survives its
/// wire form and has a summary. Programs' actions (tags 62–64 and 69, and `program_*` parameter
/// changes) are refused on both dormant chains, by programs' own rules whenever nothing else in
/// the tx is wrong ([`check_dormant_refusal`]), and nobody is charged: v2 stays v1 until programs
/// switch on (§1.2).
fn program_action(f: &ProgramFixture, action: Action) -> bool {
    let back: Action = serde_json::from_slice(&serde_json::to_vec(&action).unwrap()).expect("an action must parse again from its JSON");
    assert_eq!(back, action, "the JSON round trip changed the action");
    if action.is_program() {
        assert!(action.check_as_multisig_action().is_err(), "a multisig proposal may hold a program action");
    }
    let tx = Tx { action, ..f.envelope.clone() };
    let _ = consensus::check_tx_stateless(&tx, CHAIN_ID, genesis_hash(), prefix());
    let carried = Tx::from_bytes(&tx.to_bytes()).is_ok_and(|back| back == tx);
    if tx.check_format().is_ok() {
        assert!(carried, "a tx check_format passed doesn't survive its wire form");
        genesis_state().describe(&tx, None);
    }
    if !is_programs_action(&tx.action) {
        return false;
    }
    for (name, st) in &f.dormant {
        let (sender, supply, burned) = (st.account(&tx.from), st.supply, st.burned);
        let mut after = st.clone();
        let outcome = after.apply(&tx, CHAIN_ID, &f.envelope.from, PROGRAM_HEIGHT);
        if let Err(why) = check_dormant_refusal(&tx, &outcome) {
            panic!("{name}: {why}");
        }
        let charged = after.account(&tx.from);
        assert_eq!((charged.balance, charged.nonce, after.supply, after.burned), (sender.balance, sender.nonce, supply, burned), "{name}: a refused {} tx changed the sender's account or the supply", tx.action.kind());
    }
    carried
}

/// Whether `outcome`, a dormant chain's `State::apply` of `tx` (one of programs' actions in the
/// envelope, [`program_tx`]), is what dormancy owes it: `Err(why not)` when it isn't. It must be a
/// refusal. When `tx` passes the stateless checks `State::admit` makes before programs' own (its
/// format and its addresses' prefix), the refusal must be programs' own, in `programs::admit`'s
/// words ([`dormant_refusal`]): the envelope passes all the others (the fixture checks it), so a
/// refusal of any other kind would mean a program action got past programs' rules into a later
/// check or handler (review F-4). A tx that fails those checks may be refused for them instead.
pub fn check_dormant_refusal(tx: &Tx, outcome: &anyhow::Result<Outcome>) -> Result<(), String> {
    let kind = tx.action.kind();
    let refusal = match outcome {
        Ok(o) => return Err(format!("programs are dormant, yet a {kind} tx was admitted: {o:?}")),
        Err(e) => format!("{e:#}"),
    };
    if tx.check_format().is_err() || tx.check_prefix(prefix()).is_err() {
        return Ok(());
    }
    let owed = dormant_refusal(&tx.action);
    if refusal != owed {
        return Err(format!("programs are dormant, yet a {kind} tx was refused for something else: {refusal:?}, not {owed:?}"));
    }
    Ok(())
}

/// Programs' refusal of one of programs' actions while they're dormant, in `programs::admit`'s
/// words (programs/tests.rs and activate/tests.rs pin them): a program action's "programs are
/// off", and for a `program_*` parameter change v1's "unknown parameter" (under "the proposed
/// action" when a multisig proposes it, as `Action::check_format` words a nested refusal).
fn dormant_refusal(action: &Action) -> String {
    match action {
        Action::Propose { kind: ProposalKind::ParamChange { key, .. }, .. } => format!("unknown parameter `{key}`"),
        Action::MultisigPropose { action } => format!("the proposed action: {}", dormant_refusal(action)),
        a => format!("programs are off: a {} transaction is refused until protocol v2 is in force and governance switches the `programs` feature on", a.kind()),
    }
}

/// `action` in the tx `program_tx_codec` checks every action in (the fixture's envelope).
pub fn program_tx(action: Action) -> Tx {
    Tx { action, ..program_fixture().envelope.clone() }
}

// ------------------------------------------------------------------------------------ seeds

/// A signed tx from `keys` paying exactly its charge (`min_fee + tip`), valid through
/// `VALID_UNTIL`.
fn signed_tx(keys: &Keys, nonce: u64, tip: u64, action: Value) -> Tx {
    let mut v = json!({ "chain_id": CHAIN_ID, "genesis_hash": genesis_hash(), "from": keys.address(), "nonce": nonce, "fee": "0", "tip": tip.to_string(), "valid_until": VALID_UNTIL, "action": action, "memo": "", "public_key": "", "signature": "" });
    let charge = serde_json::from_value::<Tx>(v.clone()).expect("tx JSON shape changed: update harness::signed_tx").sign(keys).charge().unwrap();
    v["fee"] = json!(charge.to_string());
    serde_json::from_value::<Tx>(v).unwrap().sign(keys)
}

fn proposal(epoch: u64, view: u64, parent_view: u64, block: &CBlock) -> Proposal<Digest> {
    Proposal::new(Round::new(Epoch::new(epoch), View::new(view)), View::new(parent_view), block.digest())
}

/// A finalization of `block` by the fixture chain's validator.
fn finalize_fixture(block: &CBlock, epoch: u64, view: u64) -> Vec<u8> {
    let g = genesis();
    let scheme = MlDsaScheme::signer(&consensus::namespace(CHAIN_ID, genesis_hash()), consensus::participants(g).unwrap(), PrivateKey::from_seed_bytes(VALIDATOR_SEED)).unwrap();
    let vote = Finalize::sign(&scheme, proposal(epoch, view, view - 1, block)).unwrap();
    Finalization::from_finalizes(&scheme, NonEmpty::try_new([vote].iter()).unwrap(), &Sequential).unwrap().encode().to_vec()
}

/// A fast-sync manifest for `checkpoint`: blocks `checkpoint + 1 ..= checkpoint + 4`, the last one
/// committing to the checkpoint's state root and certified by the fixture validator, and the epoch
/// anchor when it's before the checkpoint (the previous epoch's last block, certified in that epoch).
fn manifest(checkpoint: u64) -> Vec<u8> {
    let g = genesis();
    let proposer = Keys::from_seed(VALIDATOR_SEED).address();
    let state_root = crate::crypto::hash(b"blockula fuzz checkpoint state");
    let chunk = b"chunk".to_vec();
    let mut blocks: Vec<CBlock> = vec![];
    let mut parent = Digest(crate::crypto::hash(b"the checkpoint block"));
    for h in checkpoint + 1..=checkpoint + snapshot::CHECKPOINT_DELAY {
        let (exec_height, exec_root) = if h == checkpoint + snapshot::CHECKPOINT_DELAY { (checkpoint, state_root) } else { (checkpoint - 1, [7; 32]) };
        let b = CBlock::new(parent, h, h, h - 1, GENESIS_MS + 1000 * h, proposer.clone(), exec_height, exec_root, vec![]);
        parent = b.digest();
        blocks.push(b);
    }
    let last = blocks.last().unwrap();
    let epoch = g.params().epoch_of(last.height);
    let mut epochs = EpochRegistry::genesis(g);
    let set = epochs.set(0).unwrap().clone();
    for e in 1..=epoch {
        epochs.record(e, set.clone());
    }
    let mut m = json!({
        "chain_id": CHAIN_ID, "height": checkpoint, "state_root": hex::encode(state_root),
        "chunks": [{ "hash": hex::encode(crate::crypto::hash(&chunk)), "size": chunk.len() }],
        "blocks": blocks.iter().map(|b| hex::encode(b.encode())).collect::<Vec<_>>(),
        "finalization": hex::encode(finalize_fixture(last, epoch, last.height)),
        "epochs": epochs,
    });
    if let Some(a) = snapshot::anchor_height(&g.params(), checkpoint, last.height) {
        let anchor = CBlock::new(Digest(crate::crypto::hash(b"before the anchor")), a, a, a - 1, GENESIS_MS + 1000 * a, proposer.clone(), 0, [0; 32], vec![]);
        m["anchor"] = json!({ "block": hex::encode(anchor.encode()), "finalization": hex::encode(finalize_fixture(&anchor, g.params().epoch_of(a), a)) });
    }
    serde_json::to_vec(&m).unwrap()
}

/// Valid inputs for every target, built with the node's own code (`tests/seeds.rs` writes them to
/// `seeds/<target>/`). The fuzzer gets much deeper mutating these than starting from nothing.
pub fn seeds() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let g = genesis();
    let funded = Keys::from_seed(VALIDATOR_SEED);
    let other = Keys::from_seed([0x43; 32]);
    let v0 = PrivateKey::from_seed_bytes(VALIDATOR_SEED);
    let pop = |p2p: &str, http: &str| hex::encode(v0.sign(types::POP_CONTEXT, &types::pop_message(CHAIN_ID, &funded.address(), p2p, http)).as_ref());
    let txs = [
        signed_tx(&funded, 0, 1_000, json!({ "type": "transfer", "to": other.address(), "amount": "2000000000" })),
        signed_tx(&funded, 1, 0, json!({ "type": "transfer", "to": "alice.boo", "amount": "1" })),
        signed_tx(&funded, 2, 0, json!({ "type": "register_name", "name": "count-dracula" })),
        signed_tx(&funded, 3, 0, json!({ "type": "add_key", "key_id": types::key_id(&other.public_key()), "label": "agent", "role": { "type": "session", "limit": "5000000000", "period_blocks": 600, "expires_at": 100000 }, "public_key": hex::encode(other.public_key()), "pop": types::key_pop(&other, CHAIN_ID, genesis_hash(), &funded.address()) })),
        signed_tx(&funded, 4, 0, json!({ "type": "register_validator", "consensus_key": hex::encode(funded.public_key()), "p2p_addr": "203.0.113.9:7071", "http_addr": "https://v.example", "bond": "50000000000", "pop": pop("203.0.113.9:7071", "https://v.example") })),
        signed_tx(&funded, 5, 0, json!({ "type": "create_hunt", "repo": "jackdishman/blockula", "issue": 7, "amount": "10000000000", "expires_at": 9000, "title": "Fuzz the decoders" })),
        signed_tx(&funded, 6, 0, json!({ "type": "reveal_votes", "seance_id": 2, "votes": "ab=", "spam": [5, 6], "salt": "pepper" })),
        signed_tx(&funded, 7, 0, json!({ "type": "attest_claim", "claim_id": 1, "ok": true, "merge_sha": "cd".repeat(20), "note": "" })),
        // Protocol v1 tags (30–49): nested proposal kinds and actions, lists, conviction.
        signed_tx(&funded, 8, 0, json!({ "type": "propose", "kind": { "type": "treasury_spend", "to": "alice.boo", "amount": "5000000000" }, "deposit": "1000000000000", "title": "Fund the fuzzers", "url": "https://example.com/p/1" })),
        signed_tx(&funded, 9, 0, json!({ "type": "vote", "proposal_id": 0, "aye": true, "amount": "1000000000", "conviction": 3, "proposal_digest": types::proposal_digest(&types::ProposalKind::TreasurySpend { to: "alice.boo".into(), amount: 5_000_000_000 }, "Fund the fuzzers", "https://example.com/p/1") })),
        signed_tx(&funded, 10, 0, json!({ "type": "multisig_propose", "action": { "type": "delegate", "validator": funded.address(), "amount": "10000000000" } })),
        signed_tx(&funded, 11, 0, json!({ "type": "claim_distribution", "distribution_id": 1, "amount": "7", "proof": ["ab".repeat(32), "cd".repeat(32)] })),
        signed_tx(&funded, 12, 0, json!({ "type": "report_equivocation", "evidence": "0a".repeat(512) })),
    ];
    let wire: Vec<Vec<u8>> = txs.iter().map(Tx::to_bytes).collect();
    let mut out: Vec<(&'static str, &'static str, Vec<u8>)> = vec![];
    for ((name, tx), bytes) in ["transfer", "transfer-to-name", "register-name", "add-session-key", "register-validator", "create-hunt", "reveal-votes", "attest-claim", "propose-treasury-spend", "vote", "multisig-propose", "claim-distribution", "report-equivocation"].into_iter().zip(&txs).zip(&wire) {
        out.push(("tx_json", name, serde_json::to_vec(tx).unwrap()));
        out.push(("tx_wire", name, bytes.clone()));
    }
    // Ghost Mode (tags 50–52): real proofs, in both wire forms (`tx_wire` takes them too).
    for (name, tx) in ghost_seed_txs(&funded) {
        out.push(("ghost_tx", name, tx.to_bytes()));
    }

    // Blocks extending the fixture genesis, carrying real txs (wire form). View = height.
    let genesis_block = consensus::genesis_block(g);
    let root = consensus::hex_32(&g.initial_state().root());
    let mut chain = vec![genesis_block.clone()];
    for (h, txs) in [(1, wire[..2].to_vec()), (2, vec![]), (3, vec![wire[2].clone()]), (4, wire[3..8].to_vec())] {
        let parent = chain.last().unwrap();
        chain.push(CBlock::new(parent.digest(), h, h, h - 1, GENESIS_MS + 1000 * h, funded.address(), 0, root, txs));
    }
    out.push(("cblock", "genesis", genesis_block.encode().to_vec()));
    out.push(("cblock", "two-txs", chain[1].encode().to_vec()));
    out.push(("cblock", "one-tx", chain[3].encode().to_vec()));
    out.push(("cblock", "five-txs", chain[4].encode().to_vec()));

    // `/api/sync` batches: one block, and three blocks certified by the last.
    let batch = |blocks: &[CBlock]| {
        let last = blocks.last().unwrap();
        let body = json!({ "tip": last.height, "blocks": blocks.iter().map(|b| hex::encode(b.encode())).collect::<Vec<_>>(), "to": last.height, "finalization": hex::encode(finalize_fixture(last, 0, last.height)) });
        serde_json::to_vec(&body).unwrap()
    };
    out.push(("sync_batch", "one-block", batch(&chain[1..2])));
    out.push(("sync_batch", "three-blocks", batch(&chain[1..4])));

    // Fast-sync manifests for checkpoint 600 (epoch 1) and, FUZZ-1's reproducer, checkpoint 100
    // (epoch 0: `verify_chain(.., 0)` used to run `sets.range(1..=0)` and panic).
    out.push(("snapshot_manifest", "checkpoint-600", manifest(600)));
    out.push(("snapshot_manifest", "fuzz-1-manifest-in-epoch-0", manifest(100)));

    // State records: the fixture genesis, and the state after executing the seed blocks (accounts,
    // keys, a name, a hunt; the failed txs leave fees and nonces behind).
    let record_bytes = |st: &State| {
        let mut b = vec![];
        snapshot::write_state(st, &mut b).unwrap();
        b
    };
    out.push(("state_snapshot", "genesis", record_bytes(&g.initial_state())));
    let executed = open_chain("seed-state");
    for b in &chain[1..] {
        executed.lock().execute(consensus::finalized_from(b, None)).expect("execute a seed block");
    }
    out.push(("state_snapshot", "after-seed-blocks", record_bytes(&executed.lock().state)));
    let _ = std::fs::remove_dir_all(&executed.lock().home);

    // Validator-set history: genesis only, and a change at epoch 1 with its handoff signature.
    out.push(("epochs_registry", "genesis", serde_json::to_vec(&EpochRegistry::genesis(g)).unwrap()));
    let mut reg = EpochRegistry::genesis(g);
    let mut next = vec![hex::encode(funded.public_key()), hex::encode(other.public_key())];
    next.sort();
    reg.record(1, next.clone());
    reg.record(2, next);
    let executed = reg.clone();
    reg.sign_missing(instance(), VALIDATOR_SEED, |e| executed.set(e).cloned());
    out.push(("epochs_registry", "handoff-epoch-1", serde_json::to_vec(&reg).unwrap()));
    // FUZZ-2's reproducer: a "handoff proof" for epoch 0 made `add_sig` compute `0 - 1`.
    let genesis_set = EpochRegistry::genesis(g).set(0).unwrap().clone();
    out.push(("epochs_registry", "fuzz-2-handoff-proof-for-epoch-0", serde_json::to_vec(&json!({ "sets": { "0": genesis_set }, "known_through": 0, "proofs": { "0": { "0": "00" } } })).unwrap()));

    // Votes and certificates of the 4-validator set (3 signers make a quorum).
    let schemes: Vec<MlDsaScheme> = SET_SEEDS.iter().map(|s| MlDsaScheme::signer(&consensus::namespace(CHAIN_ID, genesis_hash()), four_validators(), PrivateKey::from_seed_bytes(*s)).unwrap()).collect();
    let p = proposal(2, 9, 8, &chain[1]);
    let round = p.round;
    let notarizes: Vec<_> = schemes.iter().map(|s| Notarize::sign(s, p.clone()).unwrap()).collect();
    let nullifies: Vec<_> = schemes.iter().map(|s| Nullify::sign::<Digest>(s, round).unwrap()).collect();
    let finalizes: Vec<_> = schemes.iter().map(|s| Finalize::sign(s, p.clone()).unwrap()).collect();
    let prefixed = |b: Vec<u8>| [vec![3u8], b].concat(); // first byte 3: 4 participants for the bare decode
    let v = &schemes[0];
    let fin = Finalization::from_finalizes(v, NonEmpty::try_new(finalizes[..3].iter()).unwrap(), &Sequential).unwrap();
    let not = Notarization::from_notarizes(v, NonEmpty::try_new(notarizes[1..].iter()).unwrap(), &Sequential).unwrap();
    let nul = Nullification::from_nullifies(v, NonEmpty::try_new(nullifies.iter()).unwrap(), &Sequential).unwrap();
    out.push(("certificate", "bare-certificate", prefixed(fin.certificate.encode().to_vec())));
    out.push(("certificate", "finalization", prefixed(Certificate::<_, Digest>::Finalization(fin).encode().to_vec())));
    out.push(("certificate", "notarization", prefixed(Certificate::<_, Digest>::Notarization(not).encode().to_vec())));
    out.push(("certificate", "nullification", prefixed(Certificate::<_, Digest>::Nullification(nul).encode().to_vec())));
    out.push(("certificate", "notarize-vote", prefixed(Vote::<_, Digest>::Notarize(notarizes[0].clone()).encode().to_vec())));
    out.push(("certificate", "nullify-vote", prefixed(Vote::<_, Digest>::Nullify(nullifies[1].clone()).encode().to_vec())));
    out.push(("certificate", "finalize-vote", prefixed(Vote::<_, Digest>::Finalize(finalizes[2].clone()).encode().to_vec())));

    // State machine runs: a full Séance, hunts and agents, validators and the v1 tags, and raw
    // actions (wire-encoded, after their op).
    let users = 0..SM_USERS as u8;
    let mut seance: Vec<(u8, u8, u8, u16)> = vec![(14, 0, 0, 1), (21, 0, 0, 29)];
    seance.extend(users.clone().map(|i| (13, 0, i, 20)));
    seance.extend(users.clone().flat_map(|i| [(11, i, 0, 0), (19, i, 0, 0)]));
    seance.extend([(34, 0, 0, 0), (17, 0, 0, 0), (17, 1, 1, 0), (17, 2, 2, 0), (34, 0, 0, 0), (18, 0, 0, 1), (18, 0, 0, 1), (18, 0, 0, 1), (34, 0, 0, 3)]);
    for _ in 0..4 {
        seance.extend(users.clone().map(|i| (25, i, 0, 0)));
        seance.push((34, 0, 0, 1));
    }
    seance.extend([(34, 0, 0, 3), (34, 0, 0, 3), (34, 0, 0, 3)]);
    seance.extend(users.clone().map(|i| (22, i, 0, 0)));
    out.push(("state_machine", "seance", sm_ops(&seance)));
    out.push(("state_machine", "hunts-and-agents", sm_ops(&[(3, 0, 0, 15), (5, 0, 0, 0), (34, 0, 0, 0), (6, 0, 0, 2), (7, 1, 3, 4), (34, 0, 0, 0), (8, 2, 0, 0), (8, 3, 0, 1), (9, 1, 0, 0), (7, 2, 4, 1), (34, 0, 0, 3), (10, 2, 0, 1), (10, 0, 0, 0), (4, 0, 0, 0), (6, 0, 0, 1), (2, 1, 3, 0), (0, 1, 2, 7), (1, 3, 1, 0), (35, 2, 1, 0), (35, 2, 1, 1), (34, 0, 0, 2)])));
    out.push(("state_machine", "validators-and-v1", sm_ops(&[(26, 0, 0, 0), (34, 0, 0, 0), (27, 0, 1, 0), (34, 0, 0, 3), (28, 1, 0, 0), (29, 1, 0, 0), (29, 2, 1, 1), (29, 3, 0, 2), (30, 0, 0, 0), (31, 0, 2, 0), (32, 0, 0, 0), (32, 0, 0, 1), (27, 0, 1, 1), (34, 0, 0, 3), (34, 0, 0, 3), (27, 0, 1, 2), (34, 0, 0, 1)])));
    let mut raw = vec![];
    for action in [Action::Transfer { to: Keys::from_seed([3; 32]).address(), amount: 7 }, Action::Vote { proposal_id: 0, aye: true, amount: 5, conviction: 2, proposal_digest: "00".repeat(32) }, Action::Delegate { validator: Keys::from_seed(VALIDATOR_SEED).address(), amount: 3 }] {
        let enc = action.to_bytes();
        raw.extend(sm_ops(&[(33, 1, (enc.len() - 1) as u8, 0)]));
        raw.extend(enc);
    }
    raw.extend(sm_ops(&[(34, 0, 0, 0)]));
    out.push(("state_machine", "raw-actions", raw));

    // Equivocation evidence of each kind, from different members, epochs and views.
    out.push(("evidence", "conflicting-notarize", evidence_seed(staking::CONFLICTING_NOTARIZE, 0, 1, 4)));
    out.push(("evidence", "conflicting-finalize", evidence_seed(staking::CONFLICTING_FINALIZE, 2, 3, 9)));
    out.push(("evidence", "nullify-finalize", evidence_seed(staking::NULLIFY_FINALIZE, 3, 0, 1)));

    // Programs' actions, each as its wire encoding and as its JSON: the vectors the SDK checks
    // (sdk-ts/test/tx-vectors.json's `program_cases`, which types::tests writes and keeps current:
    // every tag, each option set and unset, and integers, lengths and counts at their bounds), the
    // four guests' uploads with their interfaces, and a `program_*` parameter change, bare and
    // proposed by a multisig.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| std::fs::read(repo.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
    let vectors: Value = serde_json::from_slice(&read("sdk-ts/test/tx-vectors.json")).unwrap();
    let cases = vectors["program_cases"].as_array().expect("sdk-ts/test/tx-vectors.json has no program_cases");
    let mut programs: Vec<(&'static str, &'static str, Action)> = cases
        .iter()
        .map(|c| {
            let label = c["label"].as_str().expect("a program case without a label");
            let action = serde_json::from_value(c["tx"]["action"].clone()).unwrap_or_else(|e| panic!("program case {label:?}: {e}"));
            // Seed names are `&'static str`, as the literal ones are: two short strings a case,
            // leaked once per call of this generator.
            let wire = program_case_seed(label);
            let json: &'static str = format!("{wire}.json").leak();
            let wire: &'static str = wire.leak();
            (wire, json, action)
        })
        .collect();
    for (guest, wire, json) in [("counter", "upload-counter", "upload-counter.json"), ("token", "upload-token", "upload-token.json"), ("escrow", "upload-escrow", "upload-escrow.json"), ("router", "upload-router", "upload-router.json")] {
        let blob = |ext: &str| types::HexBytes(read(&format!("node/vm/tests/programs/{guest}.{ext}")));
        programs.push((wire, json, Action::UploadCode { wasm: blob("wasm"), interface: blob("iface"), storage_limit: 30 * types::DROPS_PER_FANG }));
    }
    let param = Action::Propose { kind: ProposalKind::ParamChange { key: "program_max_tx_fuel".into(), value: 5_000_000 }, deposit: 1_000_000_000_000, title: "Lower the fuel a program call may sign for".into(), url: String::new() };
    programs.push(("program-param-change", "program-param-change.json", param.clone()));
    programs.push(("multisig-program-param-change", "multisig-program-param-change.json", Action::MultisigPropose { action: Box::new(param) }));
    for (wire, json, action) in programs {
        out.push(("program_tx_codec", wire, action.to_bytes()));
        out.push(("program_tx_codec", json, serde_json::to_vec(&action).unwrap()));
    }

    // The v2 fixture's state machine runs (every kind of program op, each profile), the receipts
    // they and some hand-made ones leave (`program_receipt`), and diffs in the shapes the guests'
    // calls make (`program_commit`). Names are leaked, as the codec seeds' are.
    out.extend(v2::seeds().into_iter().map(|(name, bytes)| ("state_machine_programs", name, bytes)));
    out.extend(receipt::seeds().into_iter().map(|(name, bytes)| ("program_receipt", &*name.leak(), bytes)));
    out.extend(commit::seeds().into_iter().map(|(name, bytes)| ("program_commit", name, bytes)));
    out
}

/// The name of the `program_tx_codec` seed of sdk-ts/test/tx-vectors.json's `program_cases` entry
/// `label` (and, with `.json`, of its JSON's): the label's words (runs of ASCII letters and
/// digits) in lowercase, joined by `-`, so `call_program, a batch (tag 64)` is
/// `call-program-a-batch-tag-64`. Every vector the SDK checks is a seed, with no list to keep
/// (review F-2); `tests/seeds.rs` checks no two seeds share a name.
fn program_case_seed(label: &str) -> String {
    let words: Vec<String> = label.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_ascii_lowercase).collect();
    assert!(!words.is_empty(), "program case {label:?}: a label needs a letter or a digit to name its seed");
    words.join("-")
}
