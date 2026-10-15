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
    let root = std::env::temp_dir().join(format!("blockula-{name}-{}", crate::chain::now_ms()));
    let homes = Chain::summon(&root, &SummonOpts { epoch_blocks: EPOCH_BLOCKS, ..SummonOpts::new("sim", 4) }).unwrap();
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
        assert_eq!(restarted.chain.lock().height(), crashed_at, "replayed blocks.jsonl on restart");
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

/// Season 0 onboarding: a 5th validator registers (bonding FANG), an admin approves it, it
/// becomes active at the next epoch boundary and proposes + signs finalizations; then an admin
/// removes a genesis validator and the network keeps finalizing with everyone agreeing.
#[test]
fn validator_joins_at_epoch_boundary_and_genesis_validator_is_removed() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon("epochs");
    let new_seed = [77u8; 32];
    let new_home = root.join("v4");
    std::fs::create_dir_all(&new_home).unwrap();
    std::fs::copy(homes[0].join("genesis.json"), new_home.join("genesis.json")).unwrap();
    std::fs::write(new_home.join("validator.key"), hex::encode(new_seed)).unwrap();

    let executor = deterministic::Runner::timed(Duration::from_secs(1800));
    executor.start(|context| async move {
        let new_key = PrivateKey::from_seed_bytes(new_seed);
        let new_pk = commonware_cryptography::Signer::public_key(&new_key);
        let mut oracle = network(&context, &genesis, &[new_pk.clone()]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        // The would-be validator runs from the start: it observes (and syncs) until it's active.
        vals.push(start(&context, &mut oracle, &genesis, 4, 0, new_home.clone(), new_key.clone()).await);
        wait_for_height(&context, &vals, 2).await;

        let admin = Keys::from_seed(seeds[0]);
        assert_eq!(genesis.validator_admins, vec![admin.address()]);
        let newbie = Keys::from_seed(new_seed);
        let me = newbie.address();
        // Fund it, register (bond = chain minimum), approve. Each tx goes to a different node.
        vals[0].chain.lock().submit(signed("sim", &admin, 0, Action::Transfer { to: me.clone(), amount: 100 * DROPS_PER_FANG })).unwrap();
        wait_until(&context, "funding", || vals[2].chain.lock().state.account(&me).balance > 0).await;
        let bond = genesis.min_bond;
        let pop = crate::chain::make_pop(new_seed, "sim", &me, "127.0.0.1:30304", "127.0.0.1:7074");
        let reg = Action::RegisterValidator { consensus_key: hex::encode(newbie.public_key()), p2p_addr: "127.0.0.1:30304".into(), http_addr: "127.0.0.1:7074".into(), bond, pop };
        // A stranger can't approve; the admin can.
        let stranger = Keys::from_seed(seeds[1]);
        assert!(vals[1].chain.lock().submit(signed("sim", &stranger, 0, Action::ApproveValidator { validator: me.clone() })).is_err());
        vals[1].chain.lock().submit(signed("sim", &newbie, 0, reg)).unwrap();
        wait_until(&context, "registration", || vals[3].chain.lock().state.validators.get(&me).is_some_and(|v| v.status == ValidatorStatus::Pending)).await;
        {
            let c = vals[0].chain.lock();
            let fees: u64 = (1..=c.height()).filter_map(|h| c.block(h)).flat_map(|b| b.txs).filter(|t| t.from == me).map(|t| t.fee).sum();
            assert_eq!(c.state.account(&me).balance, 100 * DROPS_PER_FANG - bond - fees, "bond moved out of the balance");
        }
        vals[2].chain.lock().submit(signed("sim", &admin, 1, Action::ApproveValidator { validator: me.clone() })).unwrap();
        wait_until(&context, "activation", || vals.iter().all(|v| v.chain.lock().state.validators.get(&me).is_some_and(|r| r.status == ValidatorStatus::Active))).await;
        let active_from = vals[0].chain.lock().state.validators[&me].active_from.unwrap();
        let first_height = active_from * EPOCH_BLOCKS;
        println!("✓ new validator active from epoch {active_from} (height {first_height})");
        // Run a couple of epochs with 5 validators.
        wait_for_height(&context, &vals, first_height + 2 * EPOCH_BLOCKS).await;
        {
            let c = vals[0].chain.lock();
            assert_eq!(c.state.active_sets[&active_from].len(), 5);
            let later: Vec<Block> = (first_height + 1..=c.height()).filter_map(|h| c.block(h)).collect();
            let proposed = later.iter().filter(|b| b.proposer == me).count();
            assert!(proposed > 0, "new validator proposed blocks");
            // It signs finalization certificates: find its index in each epoch's set.
            let mut signed_certs = 0;
            for b in later.iter().filter(|b| c.cert_len(b.height) > 0) {
                let epoch = c.state.params.epoch_of(b.height);
                count_signed(&c, epoch, &new_pk, b, &mut signed_certs);
            }
            assert!(signed_certs > 0, "new validator's signature appears in finalization certificates");
            println!("✓ new validator proposed {proposed} blocks and signed {signed_certs} finalization certificates");
        }

        // Remove a genesis validator; the network must keep finalizing without it.
        let removed = Keys::from_seed(seeds[3]).address();
        vals[4].chain.lock().submit(signed("sim", &admin, 2, Action::RemoveValidator { validator: removed.clone() })).unwrap();
        wait_until(&context, "removal", || vals.iter().all(|v| v.chain.lock().state.validators.get(&removed).is_some_and(|r| r.active_until.is_some()))).await;
        let gone_after = vals[0].chain.lock().state.validators[&removed].active_until.unwrap();
        let target = (gone_after + 3) * EPOCH_BLOCKS;
        wait_for_height(&context, &vals, target).await;
        let c0 = vals[0].chain.lock();
        assert!(!c0.state.active_sets[&(gone_after + 1)].contains(&removed));
        assert_eq!(c0.state.active_sets[&(gone_after + 1)].len(), 4);
        assert!(((gone_after + 1) * EPOCH_BLOCKS + 1..=c0.height()).filter_map(|h| c0.block(h)).all(|b| b.proposer != removed));
        drop(c0);
        // Can't unbond until a full epoch after leaving; nobody but the owner can.
        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
        println!("✓ 5 nodes agree at height {h} after a join and a removal");
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
        drop(joiner);
        let reopened = Chain::open(&joiner_home).unwrap();
        assert_eq!(reopened.state.root(), root_now);
        assert_eq!(reopened.height(), h);
        println!("✓ fast-synced at #{} and followed to #{h} with matching roots", m.height);
    });
    std::fs::remove_dir_all(root).ok();
}
