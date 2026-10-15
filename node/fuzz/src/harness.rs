//! One entry point per fuzz target. Each feeds attacker-controlled bytes to the node's own decoder,
//! through the same calls the p2p, HTTP-sync and fast-sync paths make, then checks more than "no
//! panic": canonical re-encoding, JSON round trips, and verification that never panics. Each
//! returns whether the input was accepted all the way, so `tests/seeds.rs` can tell a live seed
//! from a stale one.

use crate::{
    chain::{Chain, Genesis, State},
    checkpoints,
    consensus::{self, CBlock, Shared},
    crypto::Keys,
    epochs::{self, EpochRegistry},
    node::{self, SyncBatch},
    pq::{self, MlDsaScheme, PrivateKey},
    snapshot,
    state::tree,
    types::{self, Tx},
};
use commonware_codec::{Decode as _, DecodeExt as _, Encode as _};
use commonware_consensus::{
    simplex::types::{Certificate, Finalization, Finalize, Notarization, Notarize, Nullification, Nullify, Proposal, Vote},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{Digestible as _, Signer as _, blake3::Digest, certificate::Verifier as _};
use commonware_parallel::Sequential;
use commonware_utils::{iter::NonEmpty, ordered::Set, sys_rng};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};

/// The fuzz targets, by name (`fuzz_targets/<name>.rs`, `seeds/<name>/`).
pub const TARGETS: [&str; 8] = ["cblock", "tx_json", "tx_wire", "snapshot_manifest", "state_snapshot", "sync_batch", "epochs_registry", "certificate"];

/// Runs target `name` on `data`.
pub fn run(name: &str, data: &[u8]) -> bool {
    match name {
        "cblock" => cblock(data),
        "tx_json" => tx_json(data),
        "tx_wire" => tx_wire(data),
        "snapshot_manifest" => snapshot_manifest(data),
        "state_snapshot" => state_snapshot(data),
        "sync_batch" => sync_batch(data),
        "epochs_registry" => epochs_registry(data),
        "certificate" => certificate(data),
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
    V.get_or_init(|| MlDsaScheme::verifier(&consensus::namespace(CHAIN_ID), four_validators()))
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
        ok &= tx.is_some_and(|tx| consensus::check_tx_stateless(&tx, CHAIN_ID, prefix()).is_ok());
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
    let ok = consensus::check_tx_stateless(&tx, CHAIN_ID, prefix()).is_ok();
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
    let ok = consensus::check_tx_stateless(&tx, CHAIN_ID, prefix()).is_ok();
    if tx.check_format().is_ok() {
        genesis_state().describe(&tx, None);
    }
    ok
}

/// `GET /api/snapshot/latest` as fast sync reads it: JSON, then `verify_manifest` (hex blocks,
/// hash links, the checkpoint commitment, the validator-set history and the certificate).
pub fn snapshot_manifest(data: &[u8]) -> bool {
    let Ok(m) = serde_json::from_slice::<snapshot::Manifest>(data) else { return false };
    let Ok(v) = snapshot::verify_manifest(genesis(), &m) else { return false };
    let _ = snapshot::check_chunk(&v, 0, data);
    // And against trusted checkpoints, as `join` checks it: one the manifest proves, one it can't.
    let set_hash = checkpoints::genesis_checkpoint(genesis()).set_hash;
    for (height, set_hash) in [(m.height, None), (m.height / 2, set_hash), (u64::MAX, None)] {
        let cp = checkpoints::Checkpoint { height, hash: v.hash_at(height).unwrap_or_else(|| "ab".repeat(32)), set_hash };
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

/// `GET /api/epochs` as `exchange_epochs` merges it (`Chain::merge_epochs`), and the validator-set
/// history check fast sync runs (`verify_chain`), epoch 0 included (FUZZ-1, FUZZ-2).
pub fn epochs_registry(data: &[u8]) -> bool {
    let Ok(reg) = serde_json::from_slice::<EpochRegistry>(data) else { return false };
    let g = genesis();
    let verified = epochs::verify_chain(g, &reg, reg.known_through).is_ok();
    for epoch in 0..=reg.known_through.min(3) {
        let _ = epochs::verify_chain(g, &reg, epoch);
    }
    let _ = (reg.proven_through(), reg.set(reg.known_through));
    EpochRegistry::genesis(g).merge_sigs(CHAIN_ID, &reg);
    reg.clone().merge_sigs(CHAIN_ID, &reg);
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

// ------------------------------------------------------------------------------------ seeds

/// A signed tx from `keys` paying exactly its charge (`min_fee + tip`), valid through
/// `VALID_UNTIL`.
fn signed_tx(keys: &Keys, nonce: u64, tip: u64, action: Value) -> Tx {
    let mut v = json!({ "chain_id": CHAIN_ID, "from": keys.address(), "nonce": nonce, "fee": "0", "tip": tip.to_string(), "valid_until": VALID_UNTIL, "action": action, "memo": "", "public_key": "", "signature": "" });
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
    let scheme = MlDsaScheme::signer(&consensus::namespace(CHAIN_ID), consensus::participants(g).unwrap(), PrivateKey::from_seed_bytes(VALIDATOR_SEED)).unwrap();
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

/// Inputs that crash a target today: open findings, kept as reproducers (also committed under
/// `regressions/`). `tests/seeds.rs` asserts they still panic, so fixing one fails that test and
/// it moves to the seeds as a regression input. None are open: FUZZ-1 and FUZZ-2 are fixed and
/// their reproducers are seeds (`fuzz-1-…`, `fuzz-2-…`).
pub fn open_findings() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    vec![]
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
        signed_tx(&funded, 3, 0, json!({ "type": "add_key", "key_id": types::key_id(&other.public_key()), "label": "agent", "role": { "type": "session", "limit": "5000000000", "period_blocks": 600, "expires_at": 100000 } })),
        signed_tx(&funded, 4, 0, json!({ "type": "register_validator", "consensus_key": hex::encode(funded.public_key()), "p2p_addr": "203.0.113.9:7071", "http_addr": "https://v.example", "bond": "50000000000", "pop": pop("203.0.113.9:7071", "https://v.example") })),
        signed_tx(&funded, 5, 0, json!({ "type": "create_hunt", "repo": "jackdishman/blockula", "issue": 7, "amount": "10000000000", "expires_at": 9000, "title": "Fuzz the decoders" })),
        signed_tx(&funded, 6, 0, json!({ "type": "reveal_votes", "seance_id": 2, "votes": "ab=", "spam": [5, 6], "salt": "pepper" })),
        signed_tx(&funded, 7, 0, json!({ "type": "attest_claim", "claim_id": 1, "ok": true, "merge_sha": "cd".repeat(20), "note": "" })),
        // Protocol v1 tags (30–49): nested proposal kinds and actions, lists, conviction.
        signed_tx(&funded, 8, 0, json!({ "type": "propose", "kind": { "type": "treasury_spend", "to": "alice.boo", "amount": "5000000000" }, "deposit": "1000000000000", "title": "Fund the fuzzers", "url": "https://example.com/p/1" })),
        signed_tx(&funded, 9, 0, json!({ "type": "vote", "proposal_id": 0, "aye": true, "amount": "1000000000", "conviction": 3 })),
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
    reg.sign_missing(CHAIN_ID, VALIDATOR_SEED);
    out.push(("epochs_registry", "handoff-epoch-1", serde_json::to_vec(&reg).unwrap()));
    // FUZZ-2's reproducer: a "handoff proof" for epoch 0 made `add_sig` compute `0 - 1`.
    let genesis_set = EpochRegistry::genesis(g).set(0).unwrap().clone();
    out.push(("epochs_registry", "fuzz-2-handoff-proof-for-epoch-0", serde_json::to_vec(&json!({ "sets": { "0": genesis_set }, "known_through": 0, "proofs": { "0": { "0": "00" } } })).unwrap()));

    // Votes and certificates of the 4-validator set (3 signers make a quorum).
    let schemes: Vec<MlDsaScheme> = SET_SEEDS.iter().map(|s| MlDsaScheme::signer(&consensus::namespace(CHAIN_ID), four_validators(), PrivateKey::from_seed_bytes(*s)).unwrap()).collect();
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
    out
}
