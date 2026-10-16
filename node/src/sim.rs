//! Deterministic multi-validator simulation: 4 validators running the real consensus wiring
//! (`consensus::start_validator`) over Commonware's simulated network and deterministic runtime.

use crate::chain::{Chain, Genesis, SummonOpts, ValidatorStatus};
use crate::consensus::{self, Shared, ValidatorConfig, CHANNELS};
use crate::crypto::Keys;
use crate::pq::{PrivateKey, PublicKey};
use crate::protection::{Protection, Timing};
use crate::types::*;
use commonware_p2p::simulated::{Config as NetConfig, Link, Network, Oracle};
use commonware_runtime::{Clock as _, Handle, Runner as _, Spawner as _, Supervisor as _, deterministic};
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
    /// Its slashing protection (the record in its home, as on a real validator).
    protection: Arc<Protection>,
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
    start_with(context, oracle, genesis, idx, run, home, key, Node::default()).await
}

/// How a simulated node differs from a stock one.
#[derive(Clone)]
struct Node {
    /// Newest protocol version its binary runs.
    supported: u32,
    /// Upgrades in the state from block 1 on (put there before the first block, as a
    /// `ScheduleUpgrade` proposal would; every node must get the same).
    schedule: Vec<crate::upgrade::ScheduledUpgrade>,
}

impl Default for Node {
    fn default() -> Self {
        Self { supported: consensus::SUPPORTED_PROTOCOL_VERSION, schedule: vec![] }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start_with(
    context: &deterministic::Context,
    oracle: &mut Oracle<PublicKey, deterministic::Context>,
    genesis: &Genesis,
    idx: usize,
    run: usize,
    home: PathBuf,
    key: PrivateKey,
    node: Node,
) -> Val {
    let mut c = Chain::open(&home).unwrap();
    if c.height() == 0 {
        // Committed with block 1, the same on every node.
        c.state.upgrade.schedule = node.schedule.clone();
    }
    crate::upgrade::configure(&mut c, node.supported, None);
    let chain: Shared = Arc::new(Mutex::new(c));
    let pk = commonware_cryptography::Signer::public_key(&key);
    // The simulated disks don't crash, so the record isn't fsynced.
    let protection = Protection::open(&home.join(crate::protection::RECORD_FILE), pk.as_ref(), Timing::for_block_time(genesis.block_time_ms), false).unwrap();
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
        protection: Some(protection.clone()),
    };
    let ctx = context.child("validator").with_attribute("idx", idx).with_attribute("run", run);
    serve_http_view(&ctx, genesis, idx, run, &chain, &protection);
    let (handle, _marshal) = consensus::start_validator(ctx, cfg, oracle.manager(), control, channels).await;
    Val { home, key, chain, handle: Some(handle), protection }
}

/// The simulated validators' HTTP APIs, by chain instance (genesis hash) and index: the latest
/// run and its chain, and whether it's cut off from the others.
type HttpPeers = std::collections::HashMap<String, std::collections::BTreeMap<usize, (usize, std::sync::Weak<Mutex<Chain>>, bool)>>;
static HTTP_PEERS: std::sync::LazyLock<Mutex<HttpPeers>> = std::sync::LazyLock::new(Default::default);

/// Stands in for the HTTP peers a validator asks about the network while slashing protection's
/// restart rule watches (node.rs `watch_network`, audit F18): every two seconds, the tips of the
/// chain's other validators, unless one of them is [`cut_off`]. Runs until `run` is replaced.
fn serve_http_view(ctx: &deterministic::Context, genesis: &Genesis, idx: usize, run: usize, chain: &Shared, protection: &Arc<Protection>) {
    let instance = ident(genesis).1;
    HTTP_PEERS.lock().entry(instance.clone()).or_default().insert(idx, (run, Arc::downgrade(chain), false));
    let (chain, protection) = (chain.clone(), protection.clone());
    ctx.child("http_view").spawn(move |ctx| async move {
        loop {
            let peers: Vec<Shared> = {
                let all = HTTP_PEERS.lock();
                let mine = &all[&instance];
                match mine.get(&idx) {
                    Some((r, _, _)) if *r != run => return,
                    Some((_, _, true)) => vec![],
                    _ => mine.iter().filter(|(i, (_, _, cut))| **i != idx && !cut).filter_map(|(_, (_, c, _))| c.upgrade()).collect(),
                }
            };
            if protection.watching() {
                let tips: Vec<u64> = peers.iter().map(|c| c.lock().network_height_claimed()).collect();
                let (params, height, claimed) = {
                    let c = chain.lock();
                    (c.state.params.clone(), c.height(), c.network_height_claimed())
                };
                let now = ctx.current().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
                protection.note_network(&params, height, claimed, &tips, None, now);
            }
            ctx.sleep(Duration::from_secs(2)).await;
        }
    });
}

/// Cuts validator `idx` off from the others' HTTP APIs (and theirs from it), or reconnects it.
fn cut_off(genesis: &Genesis, idx: usize, cut: bool) {
    if let Some((_, _, c)) = HTTP_PEERS.lock().get_mut(&ident(genesis).1).and_then(|m| m.get_mut(&idx)) {
        *c = cut;
    }
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

/// The chain id and genesis hash a sim tx is signed for.
fn ident(g: &Genesis) -> (String, String) {
    (g.chain_id.clone(), hex::encode(consensus::genesis_block(g).digest_bytes()))
}

fn transfer(g: &Genesis, from: &Keys, nonce: u64, to: &str, amount: u64) -> Tx {
    let (chain_id, genesis_hash) = ident(g);
    let mut tx = Tx { chain_id, genesis_hash, from: from.address(), nonce, fee: 0, tip: 0, valid_until: VALID_UNTIL, action: Action::Transfer { to: to.into(), amount }, memo: "sim".into(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    tx.fee = min_fee(tx.size());
    tx.sign(from)
}

const EPOCH_BLOCKS: u64 = 10;

/// Summons a 4-validator chain (epochs of `EPOCH_BLOCKS`) with genesis at the simulated epoch.
fn summon(name: &str) -> (PathBuf, Vec<PathBuf>, Genesis, Vec<[u8; 32]>) {
    summon_with(name, EPOCH_BLOCKS, &[])
}

/// [`summon`] with another epoch length and genesis parameter overrides.
fn summon_with(name: &str, epoch_blocks: u64, params: &[(&str, u64)]) -> (PathBuf, Vec<PathBuf>, Genesis, Vec<[u8; 32]>) {
    let root = std::env::temp_dir().join(format!("blockula-{name}-{}", crate::chain::now_ms()));
    let params = params.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let homes = Chain::summon(&root, &SummonOpts { epoch_blocks, params, ..SummonOpts::new("sim", 4) }).unwrap();
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

fn signed(g: &Genesis, from: &Keys, nonce: u64, action: Action) -> Tx {
    let (chain_id, genesis_hash) = ident(g);
    let mut tx = Tx { chain_id, genesis_hash, from: from.address(), nonce, fee: 0, tip: 0, valid_until: VALID_UNTIL, action, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
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
            let tx = transfer(&genesis, &funder, n, &alice.address(), (n + 1) * DROPS_PER_FANG);
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
        let tx = transfer(&genesis, &alice, 0, &funder.address(), DROPS_PER_FANG / 2);
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

fn register(g: &Genesis, keys: &Keys, nonce: u64, bond: u64, port: u16) -> Tx {
    let (p2p, http) = (format!("127.0.0.1:{port}"), format!("127.0.0.1:{}", port + 1));
    let pop = crate::chain::make_pop(keys.seed(), &g.chain_id, &keys.address(), &p2p, &http);
    signed(g, keys, nonce, Action::RegisterValidator { consensus_key: hex::encode(keys.public_key()), p2p_addr: p2p, http_addr: http, bond, pop })
}

/// CV-2 and CV-4 on the real consensus path: the set is full (max 4); two candidates register
/// permissionlessly with more stake than the genesis validators (one also gets a delegation) and
/// each replaces the weakest member, one per epoch (⌊(4−1)/3⌋ = 1), then proposes and signs
/// finalizations. An admin of this test chain then removes a member; a displaced candidate
/// takes its seat. Every node agrees throughout.
#[test]
fn candidates_join_by_stake_within_the_churn_limit() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon_with("churn", EPOCH_BLOCKS, &[("max_validators", 4)]);
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
        vals[0].chain.lock().submit(signed(&genesis, &admin, 0, Action::Transfer { to: a.address(), amount: 1_000 * DROPS_PER_FANG })).unwrap();
        vals[0].chain.lock().submit(signed(&genesis, &admin, 1, Action::Transfer { to: b.address(), amount: 1_000 * DROPS_PER_FANG })).unwrap();
        wait_until(&context, "funding", || vals[2].chain.lock().state.account(&b.address()).balance > 0).await;
        let next_epoch = (vals[0].chain.lock().height() / EPOCH_BLOCKS + 1) * EPOCH_BLOCKS;
        wait_for_height(&context, &vals[..1], next_epoch).await;
        // (Genesis validators bonded twice the minimum.)
        vals[1].chain.lock().submit(register(&genesis, &a, 0, 3 * min, 30_400)).unwrap();
        vals[2].chain.lock().submit(register(&genesis, &b, 0, 4 * min, 30_500)).unwrap();
        wait_until(&context, "registration", || vals[3].chain.lock().state.validators.get(&b.address()).is_some_and(|v| v.status == ValidatorStatus::Candidate)).await;
        vals[3].chain.lock().submit(signed(&genesis, &admin, 2, Action::Delegate { validator: b.address(), amount: 100 * DROPS_PER_FANG })).unwrap();
        wait_until(&context, "delegation", || vals[0].chain.lock().state.staking.pools[&b.address()].stake > 4 * min).await;
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
        vals[4].chain.lock().submit(signed(&genesis, &admin, 3, Action::RemoveValidator { validator: removed.clone() })).unwrap();
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

/// CRY-3 and SF-1 end to end: every validator runs its beacon duties (as node.rs does), so from
/// epoch 2 on each value mixes all 4 reveals and nobody misses one. Each epoch's leaders follow
/// the permutation seeded from its value, and `consensus::leader` (what downtime accounting
/// uses) names exactly the proposers simplex elected.
#[test]
fn beacon_rounds_seed_the_leader_order() {
    let (root, homes, genesis, seeds) = summon("beacon");
    let executor = deterministic::Runner::timed(Duration::from_secs(900));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        for (v, seed) in vals.iter().zip(&seeds) {
            let (chain, home, seed) = (v.chain.clone(), v.home.clone(), *seed);
            context.child("beacon").spawn(move |ctx| async move {
                let mut secrets = crate::beacon::Secrets::open_deterministic(&home, seed).unwrap();
                loop {
                    ctx.sleep(Duration::from_millis(500)).await;
                    let _ = crate::beacon::duties(&mut chain.lock(), &mut secrets);
                }
            });
        }
        wait_for_height(&context, &vals, 6 * EPOCH_BLOCKS).await;
        let c = vals[0].chain.lock();
        for e in 2..=5 {
            let v = &c.state.beacon.values[&e];
            assert_eq!((v.reveals, v.missed.len()), (4, 0), "epoch {e}'s beacon");
        }
        let mut shuffled = 0;
        for e in 0..=5 {
            let set = c.state.active_set(e).and_then(|s| consensus::keys_of(s.iter().map(|v| v.consensus_key.as_str()))).unwrap();
            let rotation: Vec<String> = (1..=4).map(|v| set[((e + v) % 4) as usize].address("boo")).collect();
            if (1..=4).map(|v| consensus::leader(&c.state, e, v).unwrap()).collect::<Vec<_>>() != rotation {
                shuffled += 1;
            }
        }
        assert!(shuffled > 0, "seeded orders differ from the plain rotation");
        for h in 1..=c.height() {
            let b = c.block(h).unwrap();
            let e = c.state.params.epoch_of(h);
            assert_eq!(consensus::leader(&c.state, e, b.view).as_deref(), Some(b.proposer.as_str()), "block {h} (epoch {e}, view {})", b.view);
        }
        println!("✓ beacon values from 4 reveals each; {} blocks proposed by their seeded leaders ({shuffled} of 6 epochs reordered)", c.height());
        drop(c);
        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
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
            vals[1].chain.lock().submit(transfer(&genesis, &funder, n, &Keys::from_seed([100 + n as u8; 32]).address(), DROPS_PER_FANG)).unwrap();
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
        // So is a manifest that contradicts a trusted checkpoint (here at the epoch anchor, #299).
        let real = crate::checkpoints::Checkpoint { height: 299, hash: vals[0].chain.lock().hash_at(299).unwrap(), set_hash: None };
        let trusting = |cp| crate::checkpoints::Trust::new(&genesis, &crate::checkpoints::Pins { checkpoints: vec![cp], ..Default::default() }, false).unwrap();
        snapshot::verify_manifest_trusted(&genesis, &m, &trusting(real.clone())).unwrap();
        assert!(snapshot::verify_manifest_trusted(&genesis, &m, &trusting(crate::checkpoints::Checkpoint { hash: "ab".repeat(32), ..real })).is_err());
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
        joiner.lock().install_snapshot(&v, state, &packed).unwrap();
        assert_eq!(joiner.lock().base_height(), m.height);
        // The epoch anchor (#299, before the snapshot) came certified by epoch 29's set.
        let (a, hash) = v.epoch_start.clone().expect("the epoch anchor is before the snapshot");
        assert_eq!((a, Some(hash)), (299, vals[0].chain.lock().hash_at(299)));
        assert_eq!(joiner.lock().hash_at(a), vals[0].chain.lock().hash_at(a));
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

/// Waits until every node in `vals` has executed `height`.
async fn wait_for(context: &deterministic::Context, vals: &[&Val], height: u64) {
    for _ in 0..4_000 {
        if vals.iter().all(|v| v.chain.lock().height() >= height) {
            return;
        }
        context.sleep(Duration::from_millis(250)).await;
    }
    let heights: Vec<u64> = vals.iter().map(|v| v.chain.lock().height()).collect();
    panic!("nodes stuck at {heights:?}, wanted {height}");
}

/// OPS-3: protocol v2 is scheduled at height 60. Validators restart onto a v2 binary one at a
/// time well before it (and signal v2 in the blocks they propose), the network keeps finalizing
/// across the activation height with v2 blocks, and the validator left on v1 halts after block 59
/// without diverging. Upgraded, it rejoins. Old epochs' consensus journals are pruned meanwhile.
#[test]
fn validators_upgrade_one_at_a_time_and_a_laggard_halts_at_the_activation_height() {
    let (root, homes, genesis, seeds) = summon("upgrade");
    const AT: u64 = 60;
    let (v1, v2) = (consensus::SUPPORTED_PROTOCOL_VERSION, consensus::SUPPORTED_PROTOCOL_VERSION + 1);
    let schedule = vec![crate::upgrade::ScheduledUpgrade { version: v2, activation_height: AT }];
    let executor = deterministic::Runner::timed(Duration::from_secs(1_800));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let old = Node { supported: v1, schedule: schedule.clone() };
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start_with(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed), old.clone()).await);
        }
        wait_for_height(&context, &vals, 5).await;
        // Each operator installs the v2 binary in their own window, one at a time.
        for i in 0..3 {
            let mut v = vals.remove(i);
            v.handle.take().unwrap().abort();
            let (home, key) = (v.home.clone(), v.key.clone());
            drop(v);
            let h = vals[0].chain.lock().height();
            wait_for_height(&context, &vals, h + 2).await;
            let upgraded = start_with(&context, &mut oracle, &genesis, i, 1, home, key, Node { supported: v2, schedule: vec![] }).await;
            vals.insert(i, upgraded);
            let h = vals[0].chain.lock().height();
            wait_for_height(&context, &vals, h + 6).await;
        }
        // Readiness on chain: three of four validators signal v2 in the blocks they propose (the
        // last one restarted proposes again once it has watched the network for a few views).
        let ready = || {
            let c = vals[3].chain.lock();
            let active = c.state.active_sets[&c.state.params.epoch_of(c.height() + 1)].clone();
            let signals = crate::upgrade::signals(&active, c.view().headers(u64::MAX, 40).iter().map(|(h, m)| (*h, m.proposer.as_str(), m.signal)));
            signals.values().filter(|(s, _)| *s >= v2).count()
        };
        wait_until(&context, "three validators signalling v2", || ready() == 3).await;
        assert!(vals[0].chain.lock().height() < AT - 10, "the upgrades finished well before the activation height");
        assert_eq!(crate::upgrade::version_at(&vals[3].chain.lock().state, AT), v2);
        // Across the activation height: the v1 node stops after AT - 1; the rest go on.
        let upgraded: Vec<&Val> = vals[..3].iter().collect();
        wait_for(&context, &upgraded, AT + 10).await;
        let laggard = &vals[3];
        wait_until(&context, "the v1 node halts", || laggard.chain.lock().halt.signal.get().is_some()).await;
        let halt = laggard.chain.lock().halt.signal.get().unwrap();
        assert_eq!(halt, crate::upgrade::Halt::UpgradeRequired { height: AT, version: v2, supported: v1 });
        assert_eq!(halt.exit_code(), crate::upgrade::EXIT_UPGRADE_REQUIRED);
        assert_eq!(laggard.chain.lock().height(), AT - 1, "it didn't execute the v2 block");
        {
            let (a, b) = (vals[0].chain.lock(), laggard.chain.lock());
            assert_eq!(a.root_at(AT - 1), b.root_at(AT - 1), "no divergence up to the halt");
            assert_eq!(a.block(AT - 1).unwrap().hash, b.block(AT - 1).unwrap().hash);
            assert!((1..AT).all(|h| a.block(h).unwrap().version == v1) && (AT..=AT + 10).all(|h| a.block(h).unwrap().version == v2));
        }
        // Its operator upgrades it too (the process exited): it executes the v2 blocks and agrees.
        let mut v = vals.pop().unwrap();
        v.handle.take().unwrap().abort();
        let (home, key) = (v.home.clone(), v.key.clone());
        drop(v);
        vals.push(start_with(&context, &mut oracle, &genesis, 3, 1, home, key, Node { supported: v2, schedule: vec![] }).await);
        let target = vals[0].chain.lock().height() + 8;
        wait_for_height(&context, &vals, target).await;
        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
        // Epochs older than the current one minus two lost their simplex journals.
        let epoch = vals[0].chain.lock().state.params.epoch_of(h);
        assert!(epoch >= 6);
        for e in 0..=epoch {
            let present = commonware_runtime::Storage::scan(&context, &consensus::simplex_partition("v0", e)).await.is_ok_and(|b| !b.is_empty());
            assert_eq!(present, e + consensus::JOURNAL_EPOCHS_KEPT >= epoch, "epoch {e}'s journal (current epoch {epoch})");
        }
        println!("✓ upgraded across #{AT} one validator at a time; the v1 node halted at #{}, then caught up", AT - 1);
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
    let scheme = crate::pq::MlDsaScheme::signer(&consensus::genesis_namespace(genesis), consensus::participants(genesis).unwrap(), key).unwrap();
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
        // Issuance mints every epoch, so the burn is what shows the slash.
        let burned = vals[0].chain.lock().state.burned;
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
        assert!(c.state.burned - burned >= p.slashed - p.slashed / 10, "the slash (less the reporters' 10%) was burned");
        assert!(!c.state.active_sets[&boundary].contains(&bad));
        assert_eq!(c.state.active_sets[&boundary].len(), 3);
        let reports = (1..=c.height()).filter_map(|h| c.block(h)).flat_map(|b| b.txs).filter(|t| matches!(t.action, Action::ReportEquivocation { .. })).count();
        println!("✓ the double-signer was slashed {} drops on every node ({} report tx(s), {} fault(s)); 3 nodes agree at height {h}", p.slashed, reports, c.state.staking.punished.len());
    });
    std::fs::remove_dir_all(root).ok();
}

/// OPS-6: a validator whose volume is restored from an old snapshot (its slashing-protection
/// record from earlier, its consensus journal empty) comes back while the network is still in the
/// same epoch and close to the views it had signed. It never signs in a view its earlier run
/// signed in: it watches the network first, then signs only views the network finished after.
/// Then it votes normally again, and nothing it signed conflicts with its earlier run.
#[test]
fn a_validator_restored_from_an_old_snapshot_does_not_double_sign() {
    let (root, homes, genesis, seeds) = summon_with("restore", 1_000, &[]);
    let executor = deterministic::Runner::timed(Duration::from_secs(1_800));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        wait_for_height(&context, &vals, 10).await;
        // The provider's snapshot of validator 1's volume.
        let record = vals[1].home.join(crate::protection::RECORD_FILE);
        let snapshot = std::fs::read(&record).unwrap();
        let snapshot_view = vals[1].protection.record().last_view();
        let h = vals[0].chain.lock().height();
        wait_for_height(&context, &vals, h + 15).await;
        // The machine dies. Its earlier run signed through `old_view`.
        let mut v = vals.remove(1);
        v.handle.take().unwrap().abort();
        let earlier = v.protection.log();
        let old_view = v.protection.record().last_view();
        let (home, key) = (v.home.clone(), v.key.clone());
        drop(v);
        assert!(old_view > snapshot_view + 10, "it signed {} views after the snapshot", old_view - snapshot_view);
        // Restored a second later from the snapshot: the record rolls back, the journal is gone.
        context.sleep(Duration::from_secs(1)).await;
        std::fs::write(&record, &snapshot).unwrap();
        let _ = commonware_runtime::Storage::remove(&context, &consensus::simplex_partition("v1", 0), None).await;
        let restored = start(&context, &mut oracle, &genesis, 1, 1, home, key).await;
        assert_eq!(restored.protection.record().last_view(), snapshot_view, "the record rolled back");
        vals.insert(1, restored);
        let h = vals[0].chain.lock().height();
        wait_for_height(&context, &vals, h + 25).await;
        let restored = &vals[1];
        let later = restored.protection.log();
        assert!(later.len() > 20, "it votes again ({} signatures)", later.len());
        assert!(later.iter().all(|(e, _, _, _)| *e == 0), "still the same epoch");
        // Never a view the earlier run signed in, so nothing can conflict with it.
        let first = later.iter().map(|(_, v, _, _)| *v).min().unwrap();
        assert!(first > old_view, "first view signed after the restore: {first}, the earlier run signed through {old_view}");
        for (e, v, k, hash) in &later {
            for (e0, v0, k0, hash0) in &earlier {
                if (e, v) == (e0, v0) {
                    let conflict = (k == k0 && hash != hash0) || matches!((k, k0), (crate::protection::Kind::Nullify, crate::protection::Kind::Finalize) | (crate::protection::Kind::Finalize, crate::protection::Kind::Nullify));
                    assert!(!conflict, "double-signed {k:?}/{k0:?} in view {v}");
                }
            }
        }
        assert!(restored.protection.halted().is_none(), "no other instance was running");
        // Its signatures are back in finalization certificates, and everyone agrees.
        let new_pk = commonware_cryptography::Signer::public_key(&restored.key);
        let c = vals[0].chain.lock();
        let mut signed_certs = 0;
        for b in (h + 10..=c.height()).filter_map(|x| c.block(x)).filter(|b| c.cert_len(b.height) > 0) {
            count_signed(&c, 0, &new_pk, &b, &mut signed_certs);
        }
        drop(c);
        assert!(signed_certs > 0, "the restored validator signs certificates again");
        let top = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(top).unwrap()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {top}: {roots:?}");
        println!("✓ restored validator resumed at view {first} (earlier run: through {old_view}, snapshot at {snapshot_view}); {} signatures, none conflicting; in {signed_certs} certificates", later.len());
    });
    std::fs::remove_dir_all(root).ok();
}

/// CV-5 and downtime (CV-3) end to end. A Byzantine leader stuffs every proposal with a junk tx:
/// honest verifiers replay the parent's state, find it unadmittable and reject the block, so none
/// of its blocks finalize and nothing it stuffed is stored; its empty slots count as missed and
/// it's jailed, leaves the set, and (behaving again) unjails and rejoins. Then a validator goes
/// offline: jailed, out at the boundary, back after a restart and `Unjail`. Every node agrees.
#[test]
fn junk_leaders_and_offline_validators_are_jailed_and_come_back() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon_with("jail", EPOCH_BLOCKS, &[("downtime_window", 8), ("downtime_max_missed", 3), ("jail_blocks", 15)]);
    let executor = deterministic::Runner::timed(Duration::from_secs(6_000));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        wait_for_height(&context, &vals, 3).await;
        let addr = |i: usize| Keys::from_seed(seeds[i]).address();
        let jailed = |vals: &[Val], who: &str| vals.iter().all(|v| v.chain.lock().state.staking.pools[who].jail.is_some());
        let in_set = |vals: &[Val], who: &str| vals.iter().all(|v| v.chain.lock().state.validators[who].status == ValidatorStatus::Active);
        let unjail = |vals: &[Val], i: usize| {
            let k = Keys::from_seed(seeds[i]);
            let mut c = vals[0].chain.lock();
            let nonce = c.next_nonce(&k.address());
            c.submit(signed(&genesis, &k, nonce, Action::Unjail { validator: k.address() })).unwrap();
        };

        // The junk leader.
        let (junk, from) = (addr(2), vals[2].chain.lock().height() + 1);
        vals[2].chain.lock().junk_proposals = true;
        wait_until(&context, "the junk leader's jailing", || jailed(&vals, &junk)).await;
        wait_until(&context, "its exit", || !in_set(&vals, &junk)).await;
        vals[2].chain.lock().junk_proposals = false;
        let until = vals[0].chain.lock().state.staking.pools[&junk].jail.as_ref().unwrap().until;
        {
            let c = vals[0].chain.lock();
            let p = &c.state.staking.pools[&junk];
            assert!(p.jail.as_ref().unwrap().reason.contains("leader slots"), "{:?}", p.jail);
            assert_eq!(p.junk_charged, 0, "none of its blocks was finalized, so it never paid for junk");
            let later: Vec<Block> = (from + 1..=c.height()).filter_map(|h| c.block(h)).collect();
            assert!(later.iter().all(|b| b.skipped.is_empty() && b.txs.iter().all(|t| !t.memo.starts_with("junk"))), "no junk was stored");
            let p2 = later.iter().filter(|b| b.proposer == junk).count();
            assert!(p2 <= 1, "its stuffed blocks were rejected ({p2} finalized)");
        }
        wait_for_height(&context, &vals, until).await;
        unjail(&vals, 2);
        wait_until(&context, "the junk leader's return", || in_set(&vals, &junk) && !jailed(&vals[..1], &junk)).await;

        // The offline validator.
        let gone = addr(1);
        let mut v = vals.remove(1);
        v.handle.take().unwrap().abort();
        let (home, key) = (v.home.clone(), v.key.clone());
        drop(v);
        wait_until(&context, "the offline validator's jailing", || jailed(&vals, &gone)).await;
        wait_until(&context, "its exit", || !in_set(&vals, &gone)).await;
        let until = vals[0].chain.lock().state.staking.pools[&gone].jail.as_ref().unwrap().until;
        wait_for_height(&context, &vals, until).await;
        let restarted = start(&context, &mut oracle, &genesis, 1, 1, home, key).await;
        vals.insert(1, restarted);
        unjail(&vals, 1);
        wait_until(&context, "the offline validator's return", || in_set(&vals, &gone)).await;
        let target = vals[0].chain.lock().height() + 2 * EPOCH_BLOCKS;
        wait_for_height(&context, &vals, target).await;
        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
        let c = vals[0].chain.lock();
        for who in [&junk, &gone] {
            let p = &c.state.staking.pools[who];
            assert!(p.jail.is_none() && p.slashed > 0 && c.state.active_sets[&c.state.params.epoch_of(h)].contains(who));
        }
        println!("✓ a junk leader and an offline validator were jailed, slashed, and came back; 4 nodes agree at height {h}");
    });
    std::fs::remove_dir_all(root).ok();
}

/// 👻 Ghost Mode through real consensus: a shield, an unsigned private transfer and an unshield,
/// each submitted to one validator, gossiped (the unsigned ones carry no sender at all), proposed
/// by whoever leads, proof-checked by every verifier before it votes and executed by every node
/// to the same state root.
#[test]
fn ghost_txs_finalize_through_consensus() {
    use crate::ghost::tests::{enable_in_genesis, Wallet, FEE};
    use blockula_ghost::SpendingKey;
    let (root, homes, _, seeds) = summon("ghost");
    let files: Vec<PathBuf> = homes.iter().map(|h| h.join("genesis.json")).collect();
    enable_in_genesis(&files, &[("ghost_pool_cap", 1_000 * DROPS_PER_FANG)]);
    let genesis = Genesis::load(&files[0]).unwrap();
    let executor = deterministic::Runner::timed(Duration::from_secs(900));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        wait_for_height(&context, &vals, 3).await;
        let funder = Keys::from_seed(seeds[3]);
        let (alice, bob) = (SpendingKey::from_seed([71; 32]), SpendingKey::from_seed([72; 32]));
        let carol = Keys::from_seed([73; 32]).address();
        let mut w = Wallet::new("sim");
        let pool = |v: &Val| v.chain.lock().state.ghost.meta.pool;

        let (shield, note) = w.shield(&vals[3].chain.lock(), &funder, 100 * DROPS_PER_FANG, &alice);
        vals[3].chain.lock().submit(shield).unwrap();
        wait_until(&context, "the shield", || vals.iter().all(|v| pool(v) == 100 * DROPS_PER_FANG)).await;

        let outputs = [w.output(60 * DROPS_PER_FANG, &bob, &alice), w.output(40 * DROPS_PER_FANG - FEE, &alice, &alice)];
        let to_bob = outputs[0].0;
        let transfer = w.transfer(&vals[0].chain.lock(), &alice, note, outputs);
        let id = vals[1].chain.lock().submit(transfer).unwrap();
        wait_until(&context, "the private transfer", || vals.iter().all(|v| pool(v) == 100 * DROPS_PER_FANG - FEE)).await;
        let (h, _) = vals[2].chain.lock().tx_location(&id).unwrap();
        assert!(vals[2].chain.lock().block(h).unwrap().txs.iter().any(|t| t.is_unsigned()));

        let unshield = w.unshield(&vals[0].chain.lock(), &bob, to_bob, &carol, 50 * DROPS_PER_FANG);
        vals[2].chain.lock().submit(unshield).unwrap();
        wait_until(&context, "the unshield", || vals.iter().all(|v| v.chain.lock().state.account(&carol).balance == 50 * DROPS_PER_FANG)).await;

        let h = vals.iter().map(|v| v.chain.lock().height()).min().unwrap();
        let roots: Vec<String> = vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect();
        assert!(roots.windows(2).all(|w| w[0] == w[1]), "state roots diverged at {h}: {roots:?}");
        assert!(vals.iter().all(|v| pool(v) == 100 * DROPS_PER_FANG - 50 * DROPS_PER_FANG - 2 * FEE && v.chain.lock().mempool.is_empty()));
        println!("✓ shield, private transfer and unshield finalized; 4 validators agree at height {h}");
    });
    std::fs::remove_dir_all(root).ok();
}

mod byzantine;
