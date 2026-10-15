//! Deterministic multi-validator simulation: 4 validators running the real consensus wiring
//! (`consensus::start_validator`) over Commonware's simulated network and deterministic runtime.

use crate::chain::{Chain, Genesis, SummonOpts, ValidatorStatus};
use crate::consensus::{self, Shared, ValidatorConfig, CHANNELS};
use crate::crypto::Keys;
use crate::pq::{PrivateKey, PublicKey};
use crate::types::*;
use commonware_p2p::simulated::{Config as NetConfig, Link, Network, Oracle};
use commonware_runtime::{Clock as _, Handle, Runner as _, Supervisor as _, deterministic};
use commonware_codec::{Decode as _, DecodeExt as _};
use commonware_consensus::simplex::types::Finalization;
use commonware_cryptography::blake3::Digest;
use commonware_utils::{NZUsize, probability};
use parking_lot::Mutex;
use std::{path::PathBuf, sync::Arc, time::Duration};

struct Val {
    home: PathBuf,
    key: PrivateKey,
    chain: Shared,
    handle: Option<Handle<()>>,
}

async fn start(
    context: &deterministic::Context,
    oracle: &mut Oracle<PublicKey, deterministic::Context>,
    genesis: &Genesis,
    idx: usize,
    run: usize,
    home: PathBuf,
    key: PrivateKey,
) -> Val {
    let chain: Shared = Arc::new(Mutex::new(Chain::open(&home).unwrap()));
    let pk = commonware_cryptography::Signer::public_key(&key);
    let control = oracle.control(pk.clone());
    let mut channels = vec![];
    for c in CHANNELS {
        channels.push(control.register(c, consensus::quota(c)).await.unwrap());
    }
    let cfg = ValidatorConfig {
        chain: chain.clone(),
        key: key.clone(),
        genesis: consensus::genesis_block(genesis),
        block_time_ms: genesis.block_time_ms,
        epoch_blocks: genesis.epoch_blocks,
        partition: format!("v{idx}"),
    };
    let ctx = context.child("validator").with_attribute("idx", idx).with_attribute("run", run);
    let (handle, _marshal) = consensus::start_validator(ctx, cfg, oracle.manager(), control, channels).await;
    Val { home, key, chain, handle: Some(handle) }
}

async fn wait_for_height(context: &deterministic::Context, vals: &[Val], height: u64) {
    for _ in 0..2_000 {
        if vals.iter().all(|v| v.chain.lock().height() >= height) {
            return;
        }
        context.sleep(Duration::from_millis(250)).await;
    }
    let heights: Vec<u64> = vals.iter().map(|v| v.chain.lock().height()).collect();
    panic!("validators stuck at {heights:?}, wanted {height}");
}

/// Sim txs are valid through this height (every scenario stays within an hour of blocks of it).
const VALID_UNTIL: u64 = 3_000;

fn transfer(chain_id: &str, from: &Keys, nonce: u64, to: &str, amount: u64) -> Tx {
    let mut tx = Tx { chain_id: chain_id.into(), from: from.address(), nonce, fee: 0, tip: 0, valid_until: VALID_UNTIL, action: Action::Transfer { to: to.into(), amount }, memo: "sim".into(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    tx.fee = min_fee(tx.size());
    tx.sign(from)
}

const EPOCH_BLOCKS: u64 = 10;

/// Summons a 4-validator chain (epochs of `EPOCH_BLOCKS`) with genesis at the simulated epoch.
fn summon(name: &str) -> (PathBuf, Vec<PathBuf>, Genesis, Vec<[u8; 32]>) {
    summon_with(name, &[])
}

/// [`summon`] with genesis parameter overrides.
fn summon_with(name: &str, params: &[(&str, u64)]) -> (PathBuf, Vec<PathBuf>, Genesis, Vec<[u8; 32]>) {
    let root = std::env::temp_dir().join(format!("blockula-{name}-{}", crate::chain::now_ms()));
    let params = params.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let homes = Chain::summon(&root, &SummonOpts { epoch_blocks: EPOCH_BLOCKS, params, ..SummonOpts::new("sim", 4) }).unwrap();
    // The simulated clock starts at the Unix epoch, so genesis must too.
    for h in &homes {
        let mut g = Genesis::load(&h.join("genesis.json")).unwrap();
        g.created_at_ms = 0;
        std::fs::write(h.join("genesis.json"), serde_json::to_string_pretty(&g).unwrap()).unwrap();
    }
    let genesis = Genesis::load(&homes[0].join("genesis.json")).unwrap();
    let seeds = homes.iter().map(|h| hex::decode(std::fs::read_to_string(h.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap()).collect();
    (root, homes, genesis, seeds)
}

/// Simulated network: genesis validators as primary peers, `extra` (would-be validators) as
/// secondary, all linked to each other.
async fn network(context: &deterministic::Context, genesis: &Genesis, extra: &[PublicKey]) -> Oracle<PublicKey, deterministic::Context> {
    let participants = consensus::participants(genesis).unwrap();
    let (network, oracle) = Network::new_with_split_peers(
        context.child("network"),
        NetConfig { max_size: consensus::MAX_MESSAGE_SIZE, max_peers_per_set: NZUsize!(16), disconnect_on_block: true, tracked_peer_sets: NZUsize!(4) },
        participants.iter().cloned(),
        extra.iter().cloned(),
    )
    .await;
    network.start();
    let link = Link { latency: Duration::from_millis(40), jitter: Duration::from_millis(5), success_rate: probability!(1.0) };
    let all: Vec<PublicKey> = participants.iter().cloned().chain(extra.iter().cloned()).collect();
    for a in &all {
        for b in &all {
            if a != b {
                oracle.add_link(a.clone(), b.clone(), link.clone()).await.unwrap();
            }
        }
    }
    oracle
}

fn signed(chain_id: &str, from: &Keys, nonce: u64, action: Action) -> Tx {
    let mut tx = Tx { chain_id: chain_id.into(), from: from.address(), nonce, fee: 0, tip: 0, valid_until: VALID_UNTIL, action, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    tx.fee = min_fee(tx.size());
    tx.sign(from)
}

async fn wait_until(context: &deterministic::Context, what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..4_000 {
        if f() {
            return;
        }
        context.sleep(Duration::from_millis(250)).await;
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn four_validators_finalize_gossip_crash_and_recover() {
    let (root, homes, genesis, seeds) = summon("sim");
    let executor = deterministic::Runner::timed(Duration::from_secs(900));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        wait_for_height(&context, &vals, 3).await;

        // Txs submitted to validator 3 only must reach whoever leads and get finalized.
        let funder = Keys::from_seed(seeds[3]);
        let alice = Keys::from_seed([42; 32]);
        let mut ids = vec![];
        for n in 0..3 {
            let tx = transfer("sim", &funder, n, &alice.address(), (n + 1) * DROPS_PER_FANG);
            let id = vals[3].chain.lock().submit(tx).unwrap();
            // Gossip: the other validators see it in their mempools well before the next block.
            context.sleep(Duration::from_millis(150)).await;
            for v in &vals[..3] {
                let c = v.chain.lock();
                assert!(c.mempool.iter().any(|t| t.id() == id) || c.has_tx(&id), "tx gossiped to every validator");
            }
            ids.push(id);
            let h = vals[0].chain.lock().height();
            wait_for_height(&context, &vals, h + 1).await;
        }
        let start_h = vals[0].chain.lock().height();
        wait_for_height(&context, &vals, start_h + 6).await;
        let me3 = vals[3].chain.lock().me().unwrap();
        let mut proposers = vec![];
        for v in &vals {
            let c = v.chain.lock();
            for id in &ids {
                let (h, _) = c.tx_location(id).expect("tx finalized on every validator");
                proposers.push(c.block(h).unwrap().proposer);
            }
            assert_eq!(c.state.account(&alice.address()).balance, 6 * DROPS_PER_FANG);
            assert!(c.mempool.is_empty(), "included txs are evicted from the mempool");
        }
        assert!(proposers.iter().any(|p| *p != me3), "at least one tx was proposed by a validator it wasn't submitted to");

        // Crash validator 1, keep going without it (3 of 4 is still a quorum), then restart it.
        let crashed = 1;
        let mut v = vals.remove(crashed);
        v.handle.take().unwrap().abort();
        let (home, key) = (v.home.clone(), v.key.clone());
        let crashed_at = v.chain.lock().height();
        drop(v);
        let later = vals[0].chain.lock().height() + 30;
        wait_for_height(&context, &vals, later).await;
        // A tx sent while validator 1 is down.
        let tx = transfer("sim", &alice, 0, &funder.address(), DROPS_PER_FANG / 2);
        let late_id = vals[0].chain.lock().submit(tx).unwrap();

        let restarted = start(&context, &mut oracle, &genesis, crashed, 1, home, key).await;
        assert_eq!(restarted.chain.lock().height(), crashed_at, "restarted from the state database at its height");
        vals.insert(crashed, restarted);
        let target = vals[0].chain.lock().height() + 5;
        wait_for_height(&context, &vals, target).await;

        // Everyone agrees on every state root, and the late tx reached the restarted node.
        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
        let hashes: Vec<String> = vals.iter().map(|v| v.chain.lock().block(h).unwrap().hash).collect();
        assert!(hashes.windows(2).all(|w| w[0] == w[1]));
        assert!(vals[crashed].chain.lock().has_tx(&late_id));
        println!("✓ 4 validators agree at height {h}, root {}", roots[0]);
    });
    std::fs::remove_dir_all(root).ok();
}

/// A home for a would-be validator: the chain's genesis and its own key.
fn newcomer_home(root: &std::path::Path, genesis_from: &std::path::Path, name: &str, seed: [u8; 32]) -> PathBuf {
    let home = root.join(name);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::copy(genesis_from.join("genesis.json"), home.join("genesis.json")).unwrap();
    std::fs::write(home.join("validator.key"), hex::encode(seed)).unwrap();
    home
}

fn register(chain_id: &str, keys: &Keys, nonce: u64, bond: u64, port: u16) -> Tx {
    let (p2p, http) = (format!("127.0.0.1:{port}"), format!("127.0.0.1:{}", port + 1));
    let pop = crate::chain::make_pop(keys.seed(), chain_id, &keys.address(), &p2p, &http);
    signed(chain_id, keys, nonce, Action::RegisterValidator { consensus_key: hex::encode(keys.public_key()), p2p_addr: p2p, http_addr: http, bond, pop })
}

/// CV-2 and CV-4 on the real consensus path: the set is full (max 4); two candidates register
/// permissionlessly with more stake than the genesis validators (one also gets a delegation) and
/// each replaces the weakest member, one per epoch (⌊(4−1)/3⌋ = 1), then proposes and signs
/// finalizations. An admin of this test chain then removes a member; a displaced candidate
/// takes its seat. Every node agrees throughout.
#[test]
fn candidates_join_by_stake_within_the_churn_limit() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon_with("churn", &[("max_validators", 4)]);
    let new_seeds = [[77u8; 32], [78u8; 32]];
    let new_homes: Vec<PathBuf> = new_seeds.iter().enumerate().map(|(i, s)| newcomer_home(&root, &homes[0], &format!("n{i}"), *s)).collect();

    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let new_keys: Vec<PrivateKey> = new_seeds.iter().map(|s| PrivateKey::from_seed_bytes(*s)).collect();
        let new_pks: Vec<PublicKey> = new_keys.iter().map(commonware_cryptography::Signer::public_key).collect();
        let mut oracle = network(&context, &genesis, &new_pks).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        // The candidates run from the start: they observe (and sync) until they're in the set.
        for (i, (home, key)) in new_homes.iter().zip(&new_keys).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, 4 + i, 0, home.clone(), key.clone()).await);
        }
        wait_for_height(&context, &vals, 2).await;

        let admin = Keys::from_seed(seeds[0]);
        assert_eq!(genesis.validator_admins, vec![admin.address()]);
        let min = genesis.chain_params().unwrap().min_self_bond;
        let (a, b) = (Keys::from_seed(new_seeds[0]), Keys::from_seed(new_seeds[1]));
        // Fund them; at the start of an epoch, both register with more than the genesis
        // validators' self-bond, `b` with more than `a`, and `b` also gets a delegation. No
        // approval anywhere.
        vals[0].chain.lock().submit(signed("sim", &admin, 0, Action::Transfer { to: a.address(), amount: 1_000 * DROPS_PER_FANG })).unwrap();
        vals[0].chain.lock().submit(signed("sim", &admin, 1, Action::Transfer { to: b.address(), amount: 1_000 * DROPS_PER_FANG })).unwrap();
        wait_until(&context, "funding", || vals[2].chain.lock().state.account(&b.address()).balance > 0).await;
        let next_epoch = (vals[0].chain.lock().height() / EPOCH_BLOCKS + 1) * EPOCH_BLOCKS;
        wait_for_height(&context, &vals[..1], next_epoch).await;
        vals[1].chain.lock().submit(register("sim", &a, 0, 2 * min, 30_400)).unwrap();
        vals[2].chain.lock().submit(register("sim", &b, 0, 3 * min, 30_500)).unwrap();
        wait_until(&context, "registration", || vals[3].chain.lock().state.validators.get(&b.address()).is_some_and(|v| v.status == ValidatorStatus::Candidate)).await;
        vals[3].chain.lock().submit(signed("sim", &admin, 2, Action::Delegate { validator: b.address(), amount: 100 * DROPS_PER_FANG })).unwrap();
        wait_until(&context, "delegation", || vals[0].chain.lock().state.staking.pools[&b.address()].stake > 3 * min).await;
        let same_epoch = {
            let c = vals[0].chain.lock();
            let e = |v: &str| c.state.params.epoch_of(c.state.validators[v].registered_at);
            e(&a.address()) == e(&b.address())
        };

        // One seat changes per epoch: `b` first (more stake), then `a`.
        let active = |addr: &str| vals.iter().all(|v| v.chain.lock().state.validators[addr].status == ValidatorStatus::Active);
        wait_until(&context, "both activations", || active(&a.address()) && active(&b.address())).await;
        let (ea, eb) = {
            let c = vals[0].chain.lock();
            (c.state.validators[&a.address()].active_from.unwrap(), c.state.validators[&b.address()].active_from.unwrap())
        };
        assert_eq!(ea.abs_diff(eb), 1, "one seat per epoch: they joined in consecutive epochs");
        if same_epoch {
            assert_eq!(ea, eb + 1, "b ranked first");
        }
        let (eb, ea) = (eb.min(ea), ea.max(eb));
        assert_eq!(vals[0].chain.lock().state.active_sets[&eb].len(), 4);
        {
            let c = vals[0].chain.lock();
            for e in eb.saturating_sub(1)..ea {
                let (prev, next) = (&c.state.active_sets[&e], &c.state.active_sets[&(e + 1)]);
                let replaced = prev.iter().filter(|v| !next.contains(v)).count();
                assert!(replaced <= crate::staking::churn_limit(prev.len()), "epoch {e}: {replaced} replaced");
            }
            let displaced: Vec<String> = genesis.validators.iter().map(|v| v.address.clone()).filter(|v| !c.state.active_sets[&ea].contains(v)).collect();
            assert_eq!(displaced.len(), 2);
            assert!(displaced.iter().all(|v| c.state.validators[v].status == ValidatorStatus::Candidate && c.state.validators[v].active_until.is_some()));
        }
        // Both propose and sign finalizations.
        let first = ea * EPOCH_BLOCKS;
        wait_for_height(&context, &vals, first + 2 * EPOCH_BLOCKS).await;
        {
            let c = vals[0].chain.lock();
            let later: Vec<Block> = (first + 1..=c.height()).filter_map(|h| c.block(h)).collect();
            for (k, pk) in [(&a, &new_pks[0]), (&b, &new_pks[1])] {
                assert!(later.iter().any(|blk| blk.proposer == k.address()), "{} proposed", k.address());
                let mut signed_certs = 0;
                for blk in later.iter().filter(|blk| c.cert_len(blk.height) > 0) {
                    count_signed(&c, c.state.params.epoch_of(blk.height), pk, blk, &mut signed_certs);
                }
                assert!(signed_certs > 0, "{} signed finalizations", k.address());
            }
        }
        // The admin lever of test chains: a removal is a mandatory exit, and a displaced
        // candidate (still running) takes the seat.
        let set_now = vals[0].chain.lock().state.active_sets[&ea].clone();
        let removed = set_now.iter().find(|v| **v != a.address() && **v != b.address() && **v != admin.address()).unwrap().clone();
        vals[4].chain.lock().submit(signed("sim", &admin, 3, Action::RemoveValidator { validator: removed.clone() })).unwrap();
        wait_until(&context, "removal", || vals.iter().all(|v| v.chain.lock().state.validators[&removed].active_until.is_some_and(|e| e >= ea))).await;
        let gone_after = vals[0].chain.lock().state.validators[&removed].active_until.unwrap();
        wait_for_height(&context, &vals, (gone_after + 3) * EPOCH_BLOCKS).await;
        let c0 = vals[0].chain.lock();
        let after = &c0.state.active_sets[&(gone_after + 1)];
        assert!(!after.contains(&removed) && after.len() == 4, "{after:?}");
        assert_eq!(c0.state.validators[&removed].status, ValidatorStatus::Removed);
        drop(c0);
        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
        println!("✓ two candidates joined one per epoch (epochs {eb} and {ea}); 6 nodes agree at height {h} after a removal");
    });
    std::fs::remove_dir_all(root).ok();
}

fn count_signed(c: &Chain, epoch: u64, pk: &PublicKey, b: &Block, count: &mut usize) {
    let Some(set) = c.state.active_set(epoch).and_then(|s| consensus::keys_of(s.iter().map(|v| v.consensus_key.as_str()))) else { return };
    let Some(idx) = set.position(pk) else { return };
    let bytes = c.cert(b.height).unwrap();
    let f = Finalization::<crate::pq::MlDsaScheme, Digest>::decode_cfg(bytes.as_slice(), &set.len()).expect("certificate decodes");
    if f.certificate.signers.iter().any(|s| usize::from(s) == idx) {
        *count += 1;
    }
}

/// Fast sync: a fresh node verifies a certified snapshot (~height 300) against the validator-set
/// history, rebuilds the state, then follows the chain with matching roots. Tampered manifests
/// (bad chunk, bad root, forged certificate, wrong validator set) are rejected.
#[test]
fn fast_sync_from_certified_snapshot_and_tamper_rejection() {
    use crate::snapshot;
    let (root, homes, genesis, seeds) = summon("fast");
    let joiner_home = root.join("joiner");
    std::fs::create_dir_all(&joiner_home).unwrap();
    std::fs::copy(homes[0].join("genesis.json"), joiner_home.join("genesis.json")).unwrap();

    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        // Some activity so the snapshot isn't just genesis.
        let funder = Keys::from_seed(seeds[0]);
        for n in 0..5u64 {
            vals[1].chain.lock().submit(transfer("sim", &funder, n, &Keys::from_seed([100 + n as u8; 32]).address(), DROPS_PER_FANG)).unwrap();
        }
        wait_for_height(&context, &vals, 3 * snapshot::CHECKPOINT_INTERVAL + snapshot::CHECKPOINT_DELAY + 2).await;
        // Every checkpoint-certifying block committed to its checkpoint's root.
        {
            let c = vals[2].chain.lock();
            for cp in [100u64, 200, 300] {
                let b = crate::consensus::CBlock::decode(c.cblock(cp + snapshot::CHECKPOINT_DELAY).unwrap().as_slice()).unwrap();
                assert_eq!(b.exec_height, cp);
                assert_eq!(hex::encode(b.exec_root), c.root_at(cp).unwrap());
            }
        }
        // Wait for validator 0's snapshot writer and certificate, then build the manifest.
        let mut manifest = None;
        for _ in 0..400 {
            if let Ok(m) = snapshot::build_manifest(&vals[0].chain.lock().live) {
                if m.height >= 300 {
                    manifest = Some(m);
                    break;
                }
            }
            context.sleep(Duration::from_millis(250)).await;
        }
        let m = manifest.expect("certified snapshot at 300");
        let home0 = homes[0].clone();
        let chunks: Vec<Vec<u8>> = (0..m.chunks.len()).map(|i| snapshot::chunk(&home0, m.height, i).unwrap()).collect();

        // Tampering is caught.
        let v = snapshot::verify_manifest(&genesis, &m).unwrap();
        let mut bad = chunks.clone();
        bad[0][10] ^= 1;
        assert!(snapshot::restore(&genesis, &v, &bad).err().unwrap().to_string().contains("doesn't match"));
        // A manifest can't make the joiner download (and hold) more than a state's worth of chunks.
        let mut flood = m.clone();
        flood.chunks = vec![m.chunks[0].clone(); snapshot::MAX_SNAPSHOT_CHUNKS + 1];
        assert!(snapshot::verify_manifest(&genesis, &flood).err().unwrap().to_string().contains("chunks"));
        let mut big = m.clone();
        big.chunks[0].size = snapshot::CHUNK_SIZE + 1;
        assert!(snapshot::verify_manifest(&genesis, &big).err().unwrap().to_string().contains("chunk sizes"));
        let mut wrong_root = m.clone();
        wrong_root.state_root = "11".repeat(32);
        assert!(snapshot::verify_manifest(&genesis, &wrong_root).err().unwrap().to_string().contains("commit"));
        let mut forged = m.clone();
        let mut f = hex::decode(&forged.finalization).unwrap();
        let n = f.len();
        f[n - 100] ^= 0xff;
        forged.finalization = hex::encode(f);
        assert!(snapshot::verify_manifest(&genesis, &forged).is_err(), "forged certificate");
        let mut wrong_set = m.clone();
        let fake: Vec<String> = (0..4u8).map(|i| hex::encode(commonware_cryptography::Signer::public_key(&PrivateKey::from_seed_bytes([200 + i; 32])).as_ref())).collect();
        let mut fake_sorted = fake.clone();
        fake_sorted.sort();
        wrong_set.epochs.sets.insert(1, fake_sorted);
        assert!(snapshot::verify_manifest(&genesis, &wrong_set).err().unwrap().to_string().contains("validator set"));
        let mut lying_genesis = m.clone();
        lying_genesis.epochs.sets.insert(0, fake);
        assert!(snapshot::verify_manifest(&genesis, &lying_genesis).is_err());

        // The honest manifest installs and the node follows the chain.
        let state = snapshot::restore(&genesis, &v, &chunks).unwrap();
        let joiner: Shared = Arc::new(Mutex::new(Chain::open(&joiner_home).unwrap()));
        let packed = joiner_home.join("base.state.zst.part");
        std::fs::write(&packed, chunks.concat()).unwrap();
        joiner.lock().install_snapshot(&v, state, &packed, None).unwrap();
        assert_eq!(joiner.lock().base_height(), m.height);
        assert!(joiner.lock().block(m.height).is_none(), "no history before the snapshot");
        let target = vals[0].chain.lock().height() + 20;
        wait_for_height(&context, &vals, target).await;
        // Follow with verified batches (the same path `join` uses over HTTP).
        let provider = crate::consensus::EpochProvider::new(joiner.clone(), None);
        while joiner.lock().height() < target {
            let from = joiner.lock().height() + 1;
            let c0 = vals[0].chain.lock();
            let epoch = c0.state.params.epoch_of(from);
            let end = c0.state.params.last_of(epoch).min(c0.height());
            let to = (from..=end).find(|h| c0.cert(*h).is_some()).expect("a certificate in range");
            let batch = crate::node::SyncBatch { tip: c0.height(), blocks: (from..=to).map(|h| hex::encode(c0.cblock(h).unwrap())).collect(), to: Some(to), finalization: Some(hex::encode(c0.cert(to).unwrap())) };
            drop(c0);
            let verifier = provider.epoch_scheme(epoch).unwrap();
            crate::node::apply_batch(&joiner, &verifier, epoch, &batch).unwrap();
        }
        let h = joiner.lock().height();
        assert_eq!(joiner.lock().root_at(h), vals[0].chain.lock().root_at(h));
        assert_eq!(joiner.lock().state.account(&Keys::from_seed([104; 32]).address()).balance, DROPS_PER_FANG);
        // Restart: the fast-synced home reopens from its base and replays what it executed since.
        let root_now = joiner.lock().state.root();
        // The home's database takes a lock: every handle on the old chain must be gone first.
        drop(provider);
        drop(joiner);
        let reopened = Chain::open(&joiner_home).unwrap();
        assert_eq!(reopened.state.root(), root_now);
        assert_eq!(reopened.height(), h);
        assert_eq!(reopened.replayed, 0, "the state database holds everything; nothing is replayed");
        // Without its database, the home rebuilds from base.state.zst and the logged blocks.
        drop(reopened);
        std::fs::remove_file(joiner_home.join(crate::store::DB_FILE)).unwrap();
        let rebuilt = Chain::open(&joiner_home).unwrap();
        assert_eq!((rebuilt.state.root(), rebuilt.height(), rebuilt.replayed), (root_now, h, h - m.height));
        assert_eq!(rebuilt.view().history_from(), m.height + 1);
        println!("✓ fast-synced at #{} and followed to #{h} with matching roots", m.height);
    });
    std::fs::remove_dir_all(root).ok();
}

/// A Byzantine validator that equivocates instead of running consensus (like simplex's own
/// `Conflicter` mock, on our muxed vote channel): it answers every notarize it hears with two of
/// its own for that round, one for a made-up block and one for the real one.
async fn conflicter(context: &deterministic::Context, oracle: &mut Oracle<PublicKey, deterministic::Context>, genesis: &Genesis, key: PrivateKey) -> Handle<()> {
    use commonware_codec::{Encode as _, ReadExt as _, varint::UInt};
    use commonware_consensus::simplex::types::{Notarize, Proposal, Vote};
    use commonware_p2p::{Receiver as _, Recipients, Sender as _};
    use commonware_runtime::Spawner as _;
    let control = oracle.control(commonware_cryptography::Signer::public_key(&key));
    let mut channels = vec![];
    for c in CHANNELS {
        channels.push(control.register(c, consensus::quota(c)).await.unwrap());
    }
    let scheme = crate::pq::MlDsaScheme::signer(&consensus::namespace(&genesis.chain_id), consensus::participants(genesis).unwrap(), key).unwrap();
    context.child("conflicter").spawn(move |_| async move {
        let (mut sender, mut receiver) = channels.remove(consensus::CH_VOTES as usize);
        let mut fake = 0u8;
        while let Ok((_, msg)) = receiver.recv().await {
            let mut buf = msg.as_ref();
            let Ok(epoch) = UInt::<u64>::read(&mut buf).map(u64::from) else { continue };
            let Ok(Vote::Notarize(theirs)) = Vote::<crate::pq::MlDsaScheme, Digest>::decode(buf) else { continue };
            fake = fake.wrapping_add(1);
            let made_up = Proposal::new(theirs.round(), theirs.proposal.parent, Digest([fake; 32]));
            for p in [made_up, theirs.proposal.clone()] {
                let vote = Vote::Notarize(Notarize::sign(&scheme, p).unwrap());
                let out = [UInt(epoch).encode().to_vec(), vote.encode().to_vec()].concat();
                let _ = sender.send(Recipients::All, out, true);
            }
        }
        drop(channels);
    })
}

/// CV-3 end to end: a validator double-signs; the honest nodes' engines catch it (historical
/// votes kept), their evidence reporters verify it, their evidence tasks submit
/// `ReportEquivocation`, and every node slashes and tombstones it identically. It leaves the set
/// at the next boundary and the chain goes on without it.
#[test]
fn an_equivocating_validator_is_slashed_identically_everywhere() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon("equivocate");
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate().take(3) {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        let bad = Keys::from_seed(seeds[3]).address();
        let _byzantine = conflicter(&context, &mut oracle, &genesis, PrivateKey::from_seed_bytes(seeds[3])).await;
        let stake = vals[0].chain.lock().state.staking.pools[&bad].stake;
        let supply = vals[0].chain.lock().state.supply;
        wait_until(&context, "the double-signer's slash", || vals.iter().all(|v| v.chain.lock().state.validators[&bad].status == ValidatorStatus::Tombstoned)).await;
        let tombstoned_at = vals[0].chain.lock().state.staking.pools[&bad].tombstoned_at.unwrap();
        // Out at the next boundary; three honest validators carry on.
        let boundary = vals[0].chain.lock().state.params.epoch_of(tombstoned_at) + 1;
        wait_for_height(&context, &vals, (boundary + 2) * EPOCH_BLOCKS).await;
        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
        let c = vals[0].chain.lock();
        let p = &c.state.staking.pools[&bad];
        assert!(p.stake < stake && p.slashed > 0, "slashed: {} of {stake}", p.slashed);
        assert!(!c.state.staking.punished.is_empty());
        for (key, rec) in c.state.staking.punished.iter() {
            assert!(key.starts_with(&bad) && rec.kind == crate::staking::CONFLICTING_NOTARIZE && rec.slashed > 0);
            assert!(genesis.validators.iter().any(|v| v.address == rec.reporter), "an honest validator reported it");
        }
        assert!(c.state.supply < supply, "the slash (less the reporters' share) was burned");
        assert!(!c.state.active_sets[&boundary].contains(&bad));
        assert_eq!(c.state.active_sets[&boundary].len(), 3);
        let reports = (1..=c.height()).filter_map(|h| c.block(h)).flat_map(|b| b.txs).filter(|t| matches!(t.action, Action::ReportEquivocation { .. })).count();
        println!("✓ the double-signer was slashed {} drops on every node ({} report tx(s), {} fault(s)); 3 nodes agree at height {h}", p.slashed, reports, c.state.staking.punished.len());
    });
    std::fs::remove_dir_all(root).ok();
}
