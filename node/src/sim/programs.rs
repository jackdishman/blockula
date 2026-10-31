//! The simulator scenarios of docs/PROGRAMS-V0.md §10.2: four validators over Commonware's simulated
//! network and deterministic runtime, with the real consensus wiring, running programs. What they
//! show is what no unit test can: that programs behave identically on every validator, through
//! restarts, snapshots, faults, pauses, governance and a lagging verifier.
//!
//! | Test | Scenario |
//! |---|---|
//! | [`programs_on_at_genesis`] | 1: the four guests deployed and called; full-fuel blocks; a restart from the block logs and one from a snapshot; exit-only and back by council vote; a pause and its renewal |
//! | [`a_faulting_transaction_is_quarantined_by_every_validator`] and [`a_fault_on_two_validators_costs_views_until_strikes_drop_it`] | 2: a transaction that faults on every validator, and on two of four (past f) |
//! | [`a_lagging_verifier_replays_each_block_once_and_keeps_voting`] | 3 |
//! | [`programs_switched_on_by_council_vote_at_a_height`] | 4 |
//! | [`a_pause_signed_into_full_blocks_lands_through_its_duty_lane`] and [`a_pause_signed_by_an_owner_lands_with_a_tip`] | beside 1: §2.5's lane and the owner form's tip, which blocks full of transactions would otherwise shut a pause out of |
//!
//! **What every scenario checks at every height** ([`Watch`]): the same block hash, state root and
//! program receipts (their JSON) on every validator, and the same `fuel_rate` where a validator is
//! sampled at a height's end; at the end, `State::debug_check` has run on every block of every node
//! (it does in tests) and `check_totals` holds on every node.
//!
//! **Test-only hooks** these scenarios use, each behind `#[cfg(test)]` and each a no-op until a
//! scenario sets it:
//! - `Programs::node` tags each node's state ([`start_all`]) and `view::inject::arm_on` arms a fault
//!   for some nodes only: the tag travels with the state to whichever thread runs the program, the
//!   block pass's included;
//! - `Chain::exec_delay_ms` with `consensus/exec_delay.rs` holds a node's finalized blocks, and the
//!   acknowledgements marshal waits for, before executing them: an executor that falls behind;
//! - `ReplayCache::of` finds a node's replay cache, and `ran` counts the blocks it cached beside the
//!   hits it served.
//!
//! **Where this departs from §10.2's list.** The pause comes last in scenario 1: it can't be lifted
//! (its lapse, three days of blocks later, is a unit test, §10.1), and exit-only and its reversal
//! need programs unpaused. The snapshot is installed twice, as `blockula join` does (it verifies the
//! manifest, installs it and follows with verified batches): a full node follows the chain from it,
//! and then validator 3 is replaced by its key started over that home, which votes and proposes.
//! The pause's mempool lane and its owner form are tests of their own (the last two), because they
//! need blocks full by bytes, and a wave of program calls fills a block's fuel, not its bytes: the
//! packer leaves a call over the fuel budget out and goes on to the next transaction. And this
//! simulator loses view 2 of about a quarter of its epochs whatever runs on it, which [`lost_views`]
//! leaves out.

use super::*;
use crate::consensus::ReplayCache;
use crate::programs::probe::{inject, Injected};
use crate::programs::{self, digest, fault};
use crate::upgrade::{ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use std::collections::BTreeMap;

const FANG: u64 = DROPS_PER_FANG;

/// A guest program of W1 (`node/vm/tests/programs`) and its interface (stage V's): `(wasm, interface)`.
type Guest = (&'static [u8], &'static [u8]);
const COUNTER: Guest = (include_bytes!("../../vm/tests/programs/counter.wasm"), include_bytes!("../../vm/tests/programs/counter.iface"));
const TOKEN: Guest = (include_bytes!("../../vm/tests/programs/token.wasm"), include_bytes!("../../vm/tests/programs/token.iface"));
const ESCROW: Guest = (include_bytes!("../../vm/tests/programs/escrow.wasm"), include_bytes!("../../vm/tests/programs/escrow.iface"));
const ROUTER: Guest = (include_bytes!("../../vm/tests/programs/router.wasm"), include_bytes!("../../vm/tests/programs/router.iface"));

// ------------------------------------------------------------------------------------ chains

/// A 4-validator chain born at v2 (scheduled from block 1, as `blockula dev --programs` writes it)
/// whose genesis lists the `programs` feature if `on`, with `params` over its parameters and
/// `block_time_ms` over the summoned 1 s: the dev chain's rules, never `blockula-1`'s. Its genesis
/// is written over `summon`'s as the scheduled-version scenario does.
fn summon_programs(name: &str, on: bool, block_time_ms: Option<u64>, params: &[(&str, u64)]) -> (PathBuf, Vec<PathBuf>, Genesis, Vec<[u8; 32]>) {
    let (root, homes, mut genesis, seeds) = summon_with(name, EPOCH_BLOCKS, params);
    genesis.upgrades = vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }];
    genesis.features = if on { vec![PROGRAMS_FEATURE.to_string()] } else { vec![] };
    if let Some(ms) = block_time_ms {
        genesis.block_time_ms = ms;
    }
    genesis.check().unwrap();
    for h in &homes {
        std::fs::write(h.join("genesis.json"), serde_json::to_string_pretty(&genesis).unwrap()).unwrap();
    }
    (root, homes, genesis, seeds)
}

/// Governance that a sim can finish: votes of 10 blocks (the parameter's floor), timelocks of 1,
/// deposits of 1 FANG. The bootstrap council (⅔ of the active validators, until block 365 days'
/// worth) passes `ActivateFeature` and `DeactivateFeature`.
const GOVERNANCE: [(&str, u64); 3] = [("voting_blocks", 10), ("timelock_blocks", 1), ("proposal_deposit", FANG)];

/// Starts the 4 genesis validators, each with its state tagged as node `i` (`Programs::node`, which
/// per-node fault arming reads).
async fn start_all(context: &deterministic::Context, oracle: &mut Oracle<PublicKey, deterministic::Context>, genesis: &Genesis, homes: &[PathBuf], seeds: &[[u8; 32]]) -> Vec<Val> {
    let mut vals = vec![];
    for (i, (home, key_seed)) in homes.iter().zip(seeds).enumerate() {
        vals.push(start(context, oracle, genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*key_seed)).await);
        vals[i].chain.lock().state.programs.node = Some(i);
    }
    vals
}

/// `BLOCKULA_LOG` turns the nodes' logs on for a scenario that's being debugged (off otherwise).
fn logs() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
}

/// A user of the sim: funded from validator 3's account at the start of a scenario.
fn user(key_seed: u8) -> Keys {
    Keys::from_seed([key_seed; 32])
}

/// An account's signer and its next nonce.
struct Wallet {
    keys: Keys,
    nonce: u64,
}

impl Wallet {
    fn new(keys: Keys) -> Wallet {
        Wallet { keys, nonce: 0 }
    }

    fn address(&self) -> String {
        self.keys.address()
    }

    /// `action` signed at the next nonce, with room in its fee for the fuel rate to rise while a
    /// scenario fills blocks (only the charge is taken: base fee for its bytes, plus fuel).
    fn sign(&self, g: &Genesis, action: Action) -> Tx {
        self.sign_tipping(g, action, 0)
    }

    /// [`Wallet::sign`], tipping `tip` drops.
    fn sign_tipping(&self, g: &Genesis, action: Action, tip: u64) -> Tx {
        let (chain_id, genesis_hash) = ident(g);
        let tx = Tx { chain_id, genesis_hash, from: self.keys.address(), nonce: self.nonce, fee: 2 * FANG, tip, valid_until: VALID_UNTIL, action, memo: String::new(), public_key: hex::encode(self.keys.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.sign(&self.keys)
    }

    /// Submits `action` to `node`'s mempool: its id, and the nonce is spent only if it was taken.
    fn send(&mut self, node: &Val, g: &Genesis, action: Action) -> anyhow::Result<String> {
        let tx = self.sign(g, action);
        let id = node.chain.lock().submit(tx)?;
        self.nonce += 1;
        Ok(id)
    }
}

// ------------------------------------------------------------------------------------ programs

/// A program of the sim: its address, and the version digest in force that a call names (§2.1).
#[derive(Clone, Debug)]
struct Prog {
    address: String,
    version: String,
}

fn upload(guest: Guest) -> Action {
    Action::UploadCode { wasm: HexBytes(guest.0.to_vec()), interface: HexBytes(guest.1.to_vec()), storage_limit: 5 * FANG }
}

/// A create of `guest`'s uploaded code, immutable, with `init` (the method's name and input) and the
/// fuel for it if there is one.
fn create(guest: Guest, init: Option<(&str, Vec<u8>)>) -> Action {
    let (fuel_limit, init) = match init {
        Some((method, input)) => (2_000_000, Some(InitCall { method: method.into(), input: HexBytes(input), value: 0 })),
        None => (0, None),
    };
    Action::CreateProgram { wasm_hash: hex::encode(digest::wasm_hash(guest.0)), interface_hash: hex::encode(digest::interface_hash(guest.1)), authority: None, upgrade_delay_blocks: 0, policy: ProgramPolicy::Open, accepts_transfers: false, init, fuel_limit, storage_limit: 2 * FANG, bindings: vec![] }
}

/// The program the create `creator` signs at `nonce` makes, with the version it starts at.
fn made_by(g: &Genesis, creator: &Keys, nonce: u64, guest: Guest) -> Prog {
    let address = digest::program_address(&g.address_prefix, &crate::crypto::address_bytes(&creator.address()).unwrap(), nonce);
    let version = hex::encode(digest::version_digest(&address, &digest::wasm_hash(guest.0), &digest::interface_hash(guest.1), digest::FIRST_VERSION));
    Prog { address, version }
}

fn call(p: &Prog, method: &str, input: Vec<u8>, value: u64) -> ProgramCall {
    ProgramCall { program: p.address.clone(), version_digest: p.version.clone(), method: method.into(), input: HexBytes(input), value }
}

fn bind(p: &Prog) -> Binding {
    Binding { program: p.address.clone(), version_digest: p.version.clone() }
}

/// A call batch with `fuel_limit` fuel, up to 2 FANG of deposits and `bindings`.
fn calls(calls: Vec<ProgramCall>, fuel_limit: u64, bindings: Vec<Binding>) -> Action {
    Action::CallProgram { calls, fuel_limit, storage_limit: 2 * FANG, bindings }
}

/// One `counter.increment`, reserving `fuel_limit` (its real cost is a few thousand).
fn increment(counter: &Prog, fuel_limit: u64) -> Action {
    calls(vec![call(counter, "increment", vec![], 0)], fuel_limit, vec![])
}

/// An address as a program takes it: its 32 bytes.
fn bytes32(address: &str) -> Vec<u8> {
    crate::crypto::address_bytes(address).unwrap().to_vec()
}

/// A program method's input of `parts` end to end.
fn input(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

// ------------------------------------------------------------------------------------ watching

/// What one node recorded of one height.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    hash: String,
    root: String,
    /// The block's program receipts as JSON: what every node computed from the same block.
    receipts: String,
    /// META's `fuel_rate` after the block, when the node was sampled at that height's end (a node
    /// catching up crosses several between two samples).
    fuel_rate: Option<u64>,
}

/// What each node did at each height, sampled as a scenario waits: the checks every scenario makes
/// at every height (the module docs). A node that disagrees with another about a height it has
/// reached is the scenario's failure, named when it's first seen. Nodes are told apart by their
/// homes (`v0`..`v3`), so a node that is down, or restarted, keeps its record.
struct Watch {
    seen: BTreeMap<u64, Vec<(String, Seen)>>,
    /// Each node's first height not yet recorded.
    next: BTreeMap<String, u64>,
    /// Simulated milliseconds between samples while a scenario waits.
    poll_ms: u64,
    /// How far each node's head was behind the highest one's, over the samples: the most, the sum
    /// and the number of samples.
    lag: BTreeMap<String, (u64, u64, u64)>,
}

/// A node's name: its home's last part.
fn name(v: &Val) -> String {
    v.home.file_name().unwrap().to_string_lossy().into_owned()
}

impl Watch {
    fn new() -> Watch {
        Watch { seen: BTreeMap::new(), next: BTreeMap::new(), poll_ms: 100, lag: BTreeMap::new() }
    }

    /// Forgets the lag samples so far (a scenario's phases each measure their own).
    fn reset_lag(&mut self) {
        self.lag.clear();
    }

    /// `who`'s lag behind the highest node: its mean and its most, in blocks.
    fn lag_of(&self, who: &str) -> (f64, u64) {
        let (max, sum, n) = self.lag.get(who).copied().unwrap_or_default();
        (sum as f64 / n.max(1) as f64, max)
    }

    /// Records what each node has executed since it was last sampled.
    fn sample(&mut self, vals: &[Val]) {
        let top = vals.iter().map(|v| v.chain.lock().height()).max().unwrap_or(0);
        for v in vals {
            let who = name(v);
            let behind = top - v.chain.lock().height();
            let lag = self.lag.entry(who.clone()).or_default();
            *lag = (lag.0.max(behind), lag.1 + behind, lag.2 + 1);
            let c = v.chain.lock();
            let head = c.height();
            let from = *self.next.get(&who).unwrap_or(&1);
            for h in from..=head {
                let (Some(block), Some(root)) = (c.block(h), c.root_at(h)) else { continue };
                let fuel_rate = (h == head).then(|| c.state.programs.meta.as_ref().map(|m| m.fuel_rate)).flatten();
                let obs = Seen { hash: block.hash.clone(), root: root.to_string(), receipts: serde_json::to_string(&block.programs).unwrap(), fuel_rate };
                let all = self.seen.entry(h).or_default();
                if let Some((other_who, other)) = all.iter().find(|(_, o)| (&o.hash, &o.root, &o.receipts) != (&obs.hash, &obs.root, &obs.receipts) || o.fuel_rate.zip(obs.fuel_rate).is_some_and(|(a, b)| a != b)) {
                    panic!("nodes {other_who} and {who} disagree about block {h}:\n  {other_who}: {other:?}\n  {who}: {obs:?}");
                }
                all.push((who.clone(), obs));
            }
            self.next.insert(who, from.max(head + 1));
        }
    }

    /// The heights at least two nodes were seen at: that many were compared.
    fn compared(&self) -> usize {
        self.seen.values().filter(|v| v.len() >= 2).count()
    }

    /// Height → `fuel_rate` after it, where some node was sampled at its end (all that were agree).
    fn fuel_rates(&self) -> BTreeMap<u64, u64> {
        self.seen.iter().filter_map(|(h, v)| v.iter().find_map(|(_, o)| o.fuel_rate).map(|r| (*h, r))).collect()
    }

    /// How many nodes recorded `fuel_rate` at `height`.
    fn rate_witnesses(&self, height: u64) -> usize {
        self.seen.get(&height).map_or(0, |v| v.iter().filter(|(_, o)| o.fuel_rate.is_some()).count())
    }
}

/// Waits until `ok` holds, sampling every node each `watch.poll_ms` simulated milliseconds.
async fn until(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], what: &str, mut ok: impl FnMut(&[Val]) -> bool) {
    // Ten simulated minutes: far more than any step of a scenario takes (they run 1 s blocks).
    for _ in 0..600_000 / watch.poll_ms {
        watch.sample(vals);
        if ok(vals) {
            return;
        }
        context.sleep(Duration::from_millis(watch.poll_ms)).await;
    }
    let heights: Vec<u64> = vals.iter().map(|v| v.chain.lock().height()).collect();
    panic!("timed out waiting for {what}; the nodes are at {heights:?}");
}

/// Waits until every node has executed `height`.
async fn until_height(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], height: u64) {
    until(context, watch, vals, &format!("height {height}"), |vals| vals.iter().all(|v| v.chain.lock().height() >= height)).await;
}

/// The lowest height every node has executed.
fn floor(vals: &[Val]) -> u64 {
    vals.iter().map(|v| v.chain.lock().height()).min().unwrap()
}

/// What the end of every scenario checks beyond the heights: the program totals hold on every node
/// (the epoch-boundary alarm's check, run on the final state), and `Chain::receipt_mismatches` is 0
/// on every node. That counter moves only when a node replays blocks from its block log (at start-up:
/// the receipts it computes against the ones the log holds), so it says something only about a node
/// that restarted from its logs, and is trivially 0 on the others; that every live node computes the
/// same receipts is [`Watch`]'s, at every height.
fn finish(vals: &[Val]) {
    for (i, v) in vals.iter().enumerate() {
        let c = v.chain.lock();
        assert!(programs::check_totals(&c.state), "node {i}: program totals drifted from their sums: {:?}", fault::accounting_drift());
        assert_eq!(c.receipt_mismatches, 0, "node {i}: receipts it computed differ from the logged ones");
    }
}

/// The height the transaction `id` was included at, on `v`.
fn included_at(v: &Val, id: &str) -> Option<u64> {
    v.chain.lock().tx_location(id).map(|(h, _)| h)
}

/// Whether the program transaction `id` failed (ran, charged, and did nothing else).
fn failed(v: &Val, id: &str) -> bool {
    let c = v.chain.lock();
    let (h, i) = c.tx_location(id).expect("included");
    c.block(h).unwrap().failed.contains_key(&i)
}

// ------------------------------------------------------------------------------------ scenario 2

/// This process's node-fault counts, by path and class (a scenario holds `fault::COUNTING`, so what
/// moves is its own).
fn fault_counts() -> Vec<(fault::Path, programs::state::FaultClass, u64)> {
    use programs::state::FaultClass::{Setup, Tx};
    [fault::Path::Propose, fault::Path::Verify, fault::Path::Execute, fault::Path::Startup].into_iter().flat_map(|p| [Tx, Setup].into_iter().map(move |c| (p, c, fault::count(p, c)))).collect()
}

/// The counts that moved since `before` ([`fault_counts`]) and by how much.
fn faults_since(before: &[(fault::Path, programs::state::FaultClass, u64)]) -> Vec<(fault::Path, programs::state::FaultClass, u64)> {
    fault_counts().into_iter().zip(before).filter(|(now, then)| now.2 != then.2).map(|(now, then)| (now.0, now.1, now.2 - then.2)).collect()
}

/// How many consensus views the chain lost across `heights`: for each block, the views between its
/// parent's and its own. Views restart with each epoch, at 1, so in an epoch's first block they're
/// the views before its own.
///
/// **Not counted: view 2 of an epoch.** This simulator loses it in about a quarter of its epochs (the
/// second block of an epoch then has view 3 and parent view 1), with no programs on the chain at all:
/// origin/main's simulator does it too, so it isn't a program's doing, and counting it would make
/// "no view was lost" a coin toss. The cause is Commonware simplex's leader-inactivity skip
/// (`SkipPolicy::Enabled`, `consensus.rs`) meeting a cold activity tracker: each epoch gets a new
/// engine, whose batcher has heard no one yet, and a node that has seen view 1's votes from a quorum
/// but not the leader of view 2's judges that leader inactive and nullifies at once; when two or three
/// nodes do, the view is lost (for nothing, or for the 3 s leader timeout). With `SkipPolicy::Disabled`
/// it never happens. At production epoch lengths it is one stall of at most 3 s per epoch start. A
/// program fault that cost that view alone goes uncounted here: one view in ten of a window.
fn lost_views(v: &Val, heights: std::ops::RangeInclusive<u64>) -> u64 {
    let c = v.chain.lock();
    heights
        .map(|h| {
            let (b, epoch) = (c.block(h).unwrap(), c.state.params.epoch_of(h));
            let first = if h > 1 && c.state.params.epoch_of(h - 1) == epoch { b.parent_view + 1 } else { 1 };
            (first..b.view).filter(|view| *view != 2).count() as u64
        })
        .sum()
}

/// Whether `id` is in `v`'s quarantine, by its id.
fn quarantines(v: &Val, id: &str) -> bool {
    let c = v.chain.lock();
    c.quarantine.holds_id(|held| held == id, c.height() + 1).is_some()
}

/// What `v`'s door says to `tx`: the refusal, or `None` if it takes it into its mempool.
fn door(v: &Val, tx: Tx) -> Option<String> {
    v.chain.lock().submit(tx).err().map(|e| e.to_string())
}

/// Two validators sign `ProgramsPause`, and it lands on every node: the pause.
async fn pause_programs(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], g: &Genesis, u: &mut Users) -> programs::state::Pause {
    u.validators[0].send(&vals[0], g, Action::ProgramsPause {}).unwrap();
    u.validators[1].send(&vals[1], g, Action::ProgramsPause {}).unwrap();
    let pause_of = |v: &Val| v.chain.lock().state.programs.meta.as_ref().unwrap().pause.clone();
    until(context, watch, vals, "the pause", |vals| vals.iter().all(|v| pause_of(v).is_some())).await;
    let pause = pause_of(&vals[0]).unwrap();
    assert!(vals.iter().all(|v| pause_of(v).as_ref() == Some(&pause)), "the same pause on every node");
    pause
}

/// 2: a transaction that faults on every validator (a `Tx` fault, injected into its program's view
/// for its id on all four nodes, as an engine bug a particular call trips would be). Whoever leads
/// trials it, meets the fault, leaves it out and packs on: it's never included, no view is lost,
/// every validator quarantines it once and drops it. Its sender's next program transaction is
/// refused (on every node) while a transfer from it takes the nonce and is included; a call to the
/// same program from another account is included; and a pause signed meanwhile lands.
#[test]
fn a_faulting_transaction_is_quarantined_by_every_validator() {
    // The fault counters are the process's: this scenario's are exact while it holds the lock.
    logs();
    let _counting = fault::COUNTING.lock();
    let (root, homes, genesis, seeds) = summon_programs("fault-all", true, None, &GOVERNANCE);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let g = &genesis;
        let mut oracle = network(&context, g, &[]).await;
        let vals = start_all(&context, &mut oracle, g, &homes, &seeds).await;
        let mut watch = Watch::new();
        until_height(&context, &mut watch, &vals, 3).await;
        let mut u = Users::new(&seeds);
        let d = deploy(&context, &mut watch, &vals, g, &mut u).await;
        let (alice, bob) = (u.alice.address(), u.bob.address());
        let (counted_before, faults_before) = (counted(&vals[0].chain.lock().state, &d.counter), fault_counts());

        let x_tx = u.alice.sign(g, increment(&d.counter, 300_000));
        let x = x_tx.id();
        inject::arm(&x, Injected::Panic);
        let (submitted_at, x_nonce) = (floor(&vals), x_tx.nonce);
        vals[0].chain.lock().submit(x_tx).unwrap();
        u.alice.nonce += 1;
        let other = u.bob.send(&vals[1], g, increment(&d.counter, 300_000)).unwrap();
        // Every validator takes its turn to lead, trials the transaction and meets the fault: four
        // met faults, counted as they happen, whatever the nodes then do with the transaction.
        let met = |_: &[Val]| faults_since(&faults_before).iter().filter(|(p, c, _)| *p == fault::Path::Propose && *c == programs::state::FaultClass::Tx).map(|(_, _, n)| n).sum::<u64>() >= 4;
        until(&context, &mut watch, &vals, "all four validators to meet the fault", met).await;
        let last_trial = floor(&vals);
        included_everywhere(&context, &mut watch, &vals, &other).await;
        // A view a proposer loses shows in the first block after it, so the window runs on past the
        // last trial's block: from the submission to four blocks after the fourth trial (and past the
        // inclusion of the other account's call), at least six blocks of leaders that all met the fault.
        let window_end = floor(&vals).max(last_trial + 4);
        until_height(&context, &mut watch, &vals, window_end).await;
        let window = submitted_at + 1..=window_end;
        let lost = lost_views(&vals[0], window.clone());
        println!("quarantine: {lost} views lost over blocks {window:?} (last trial at {last_trial})");
        assert_eq!(lost, 0, "the leaders that met it packed on: no view was lost over blocks {window:?}");
        // (A zero over a window of a block or two would say nothing.)
        assert!(window.clone().count() >= 6, "the window of blocks that shows a lost view is {window:?}");
        let faults = faults_since(&faults_before);
        assert_eq!(faults, [(fault::Path::Propose, programs::state::FaultClass::Tx, 4)], "each of the four validators met the fault once, in its own trial, and no other path met one");
        let quarantined_at = floor(&vals);
        for (i, v) in vals.iter().enumerate() {
            assert!(quarantines(v, &x), "node {i} quarantined it by its id");
            let c = v.chain.lock();
            assert!(!c.has_tx(&x) && !c.in_mempool(&x), "node {i} never included it, and dropped it");
            assert_eq!(c.quarantine.lens(), [1, 1, 0], "node {i} holds its id and its sender, once");
            assert_eq!(c.strikes.strikes_of(&x), 0, "a fault isn't a strike");
            assert_eq!(counted(&c.state, &d.counter), counted_before + 1, "node {i}: only the other account's call ran");
        }
        // Resubmitted anywhere, it's refused at the door.
        let again = u.alice.sign(g, increment(&d.counter, 300_000));
        for v in &vals {
            let e = door(v, Tx { nonce: x_nonce, ..again.clone() }).expect("a node took the transaction its quarantine holds");
            assert!(e.contains("quarantined on this node after a program execution fault"), "{e}");
        }

        // Its sender's transfer takes the nonce the dropped one held, and is included; the next
        // program transaction it signs is refused by every node, with the reason.
        u.alice.nonce = x_nonce;
        let pay = u.alice.send(&vals[2], g, Action::Transfer { to: bob.clone(), amount: FANG }).unwrap();
        included_everywhere(&context, &mut watch, &vals, &pay).await;
        for (i, v) in vals.iter().enumerate() {
            let e = door(v, u.alice.sign(g, increment(&d.counter, 300_000))).unwrap_or_else(|| panic!("node {i} took the faulting sender's next program transaction"));
            assert!(e.contains("quarantined on this node after a program execution fault, until block"), "{e}");
        }
        // Another account's call to the same program is included.
        let counted_mid = counted(&vals[0].chain.lock().state, &d.counter);
        let call_again = u.bob.send(&vals[3], g, increment(&d.counter, 300_000)).unwrap();
        included_everywhere(&context, &mut watch, &vals, &call_again).await;
        assert!(vals.iter().all(|v| counted(&v.chain.lock().state, &d.counter) == counted_mid + 1));
        println!("quarantined by every validator at {quarantined_at}; submitted at {submitted_at}; its sender {alice}");

        // A pause signed meanwhile lands (the quarantine holds for 600 blocks), and blocks go on.
        let pause = pause_programs(&context, &mut watch, &vals, g, &mut u).await;
        until_height(&context, &mut watch, &vals, pause.from + 5).await;
        assert!(vals.iter().all(|v| matches!(programs::status(&v.chain.lock().state, pause.from + 1), programs::Status::Paused { .. })));
        assert!(watch.compared() as u64 + 3 >= floor(&vals), "{} heights compared", watch.compared());
        inject::disarm(&x);
        finish(&vals);
    });
    std::fs::remove_dir_all(root).ok();
}

/// How many blocks past its submission the past-f variant gives validators 2 and 3 to drop the
/// transaction: it takes them under a quarter of that.
const DROP_WITHIN: u64 = 40;

/// The most views the past-f variant counts lost between the submission and the drop. In 60 release
/// runs it counted 3 or 4 (4 skipped proposals, two for each of validators 2 and 3, and up to two of
/// them uncounted views 2); a drop at three strikes costs at least one view more, and counted 5 or 6
/// in the review's runs.
const MOST_LOST: u64 = 4;

/// 2's variant, past f: the fault is injected on validators 0 and 1 only. When they lead they trial
/// the transaction, meet the fault and leave it out; when 2 and 3 lead they carry it, and 0 and 1
/// abstain on their blocks: two votes of four, so the view never certifies. Each such proposal
/// that is skipped is a strike for its proposer, and at two it drops the transaction and holds its
/// sender, after which every view certifies and nothing halts: the transaction never finalized.
#[test]
fn a_fault_on_two_validators_costs_views_until_strikes_drop_it() {
    logs();
    let _counting = fault::COUNTING.lock();
    let (root, homes, genesis, seeds) = summon_programs("fault-two", true, None, &GOVERNANCE);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let g = &genesis;
        let mut oracle = network(&context, g, &[]).await;
        let vals = start_all(&context, &mut oracle, g, &homes, &seeds).await;
        let mut watch = Watch::new();
        until_height(&context, &mut watch, &vals, 3).await;
        let mut u = Users::new(&seeds);
        let d = deploy(&context, &mut watch, &vals, g, &mut u).await;
        let faults_before = fault_counts();

        let x_tx = u.alice.sign(g, increment(&d.counter, 300_000));
        let x = x_tx.id();
        inject::arm_on(&x, Injected::Panic, &[0, 1]);
        let submitted_at = floor(&vals);
        vals[2].chain.lock().submit(x_tx).unwrap();
        u.alice.nonce += 1;
        // The two that carry it each lose two views to it, then drop it: the instant both have is
        // `dropped_at`, the highest block any node has (the nodes that dropped it may be a block ahead
        // of the others, and the lost views are before their block). The chain runs on to
        // `DROP_WITHIN` blocks past the submission, where a transaction that is still carried fails
        // the scenario by name, not by a wait's cap.
        let (mut dropped_at, mut strikes_then) = (None, vec![]);
        until(&context, &mut watch, &vals, "the chain to run on past the drop", |vals| {
            let top = vals.iter().map(|v| v.chain.lock().height()).max().unwrap();
            if dropped_at.is_none() && [2, 3].iter().all(|i| !vals[*i].chain.lock().in_mempool(&x) && vals[*i].chain.lock().strikes.strikes_of(&x) > 0) {
                strikes_then = [2, 3].iter().map(|i| vals[*i].chain.lock().strikes.strikes_of(&x)).collect();
                dropped_at = Some(top);
            }
            top >= submitted_at + DROP_WITHIN
        })
        .await;
        let strikes: Vec<u32> = [2, 3].iter().map(|i| vals[*i].chain.lock().strikes.strikes_of(&x)).collect();
        let dropped_at = dropped_at.unwrap_or_else(|| panic!("validators 2 and 3 hadn't both dropped it {DROP_WITHIN} blocks after it was submitted (their strikes {strikes:?})"));
        // The drop is at the second strike: the rule's number, written out here and not read from the
        // constant under test.
        assert_eq!(strikes_then, [2, 2], "validators 2 and 3 dropped it at two strikes each");
        until_height(&context, &mut watch, &vals, dropped_at).await;
        let lost = lost_views(&vals[0], submitted_at + 1..=dropped_at);
        println!("two validators fault: dropped {} blocks after the submission; {lost} views lost between {submitted_at} and {dropped_at}; faults {:?}", dropped_at - submitted_at, faults_since(&faults_before));
        // Four proposals were skipped, two for each of 2 and 3 (the strikes): each a lost view, and at
        // most two of them a view 2 of an epoch, which `lost_views` doesn't count (the window is under
        // two epochs long). So at least two views are counted. At most `MOST_LOST`: the chain lost no
        // other view to it; a drop at three strikes loses at least one more.
        assert!((2..=MOST_LOST).contains(&lost), "{lost} views lost");
        // From here every view certifies.
        until_height(&context, &mut watch, &vals, dropped_at + 30).await;
        let top = floor(&vals);
        let late = lost_views(&vals[0], dropped_at + 1..=top);
        let skips: Vec<(u64, u64, u64)> = (dropped_at + 1..=top).filter_map(|h| vals[0].chain.lock().block(h)).filter(|b| b.view != b.parent_view + 1).map(|b| (b.height, b.parent_view, b.view)).collect();
        assert_eq!(late, 0, "once it's dropped everywhere that carried it, every view certifies (dropped at {dropped_at}, checked to {top}; blocks that skipped views, as (height, parent view, view): {skips:?})");
        for (i, v) in vals.iter().enumerate() {
            let c = v.chain.lock();
            assert!(!c.has_tx(&x), "node {i}: the transaction never finalized");
            assert!(c.halt.signal.get().is_none(), "and no node halted on it");
        }
        // Validators 2 and 3 hold its sender, in the words of a strike; 0 and 1 never carried it, so
        // they didn't strike.
        assert!(vals[..2].iter().all(|v| v.chain.lock().strikes.strikes_of(&x) == 0));
        for i in [2, 3] {
            let e = door(&vals[i], u.alice.sign(g, increment(&d.counter, 300_000))).expect("held");
            assert!(e.contains("two of this node's proposals carrying this sender's program transactions weren't certified"), "node {i}: {e}");
        }
        // Validators 0 and 1 met the fault, each at least once: in its own trial when it led, or
        // verifying a block of 2's or 3's (once it has quarantined it, it abstains before running it).
        let met: u64 = faults_since(&faults_before).iter().filter(|(p, c, _)| matches!(p, fault::Path::Propose | fault::Path::Verify) && *c == programs::state::FaultClass::Tx).map(|(_, _, n)| n).sum();
        assert!(met >= 2, "the fault was met {met} times");
        // A call to the same program from another account is included, and the chain goes on.
        let other = u.bob.send(&vals[0], g, increment(&d.counter, 300_000)).unwrap();
        included_everywhere(&context, &mut watch, &vals, &other).await;
        let pause = pause_programs(&context, &mut watch, &vals, g, &mut u).await;
        until_height(&context, &mut watch, &vals, pause.from + 3).await;
        inject::disarm(&x);
        assert!(watch.compared() as u64 + 3 >= floor(&vals), "{} heights compared", watch.compared());
        finish(&vals);
    });
    std::fs::remove_dir_all(root).ok();
}

// ------------------------------------------------------------------------------------ scenario 3

/// The validator `v`'s own finalization signatures among the certificates of blocks `heights`
/// (which `vals[0]` holds): how often it voted, as the chain's certificates record.
fn certified_by(vals: &[Val], who: &Val, heights: std::ops::RangeInclusive<u64>) -> usize {
    let pk = commonware_cryptography::Signer::public_key(&who.key);
    let c = vals[0].chain.lock();
    let mut n = 0;
    for h in heights {
        if let Some(b) = c.block(h).filter(|_| c.cert_len(h) > 0) {
            count_signed(&c, c.state.params.epoch_of(h), &pk, &b, &mut n);
        }
    }
    n
}

/// `blocks` blocks' worth of full-fuel calls (four to a block: [`full_call_fuel`]), signed by the
/// three users and queued: their ids.
fn full_blocks(vals: &[Val], g: &Genesis, u: &mut Users, counter: &Prog, blocks: usize) -> Vec<String> {
    let quarter = full_call_fuel(blockula_vm::meter::SCHEDULE_V0.load_fuel(COUNTER.0.len()));
    (0..4 * blocks)
        .map(|n| match n % 3 {
            0 => u.alice.send(&vals[0], g, increment(counter, quarter)),
            1 => u.bob.send(&vals[1], g, increment(counter, quarter)),
            _ => u.carol.send(&vals[2], g, increment(counter, quarter)),
        }
        .unwrap())
        .collect()
}

/// Scenario 3's least share of the blocks the lagging validator ran as ancestors that its replay
/// cache served again, as 1 in this many. In 40 release runs it ran 26 to 29 blocks and was served
/// 5 to 7 times (one in 5.4 at the worst); the validators that weren't delayed ran 6 to 11 and were
/// served none. A cache that serves nothing, or a few entries, is under 1 in 8 whatever the
/// leader order does to the count.
const HITS_SHARE: u64 = 8;

/// What each node's replay cache did since `since`: `(hits, blocks run)`.
fn cache_use(caches: &[ReplayCache], since: &[(u64, u64)]) -> Vec<(u64, u64)> {
    caches.iter().zip(since).map(|(c, (served, ran))| (c.served() - served, c.ran() - ran)).collect()
}

/// 3: one validator's execution is delayed while blocks are full of program calls. Blocks come
/// faster here (20 ms) than a finalization round trip, so every validator's executed tip trails the
/// parent it votes on by a block or two, as execution that can't keep up with full-fuel blocks does
/// to all of them, and a replay of the unexecuted ancestors is on every vote and proposal; validator
/// 3's executor holds each block 90 ms on top, which it keeps pace with (the chain's blocks are
/// about 110 ms apart). It replays each block it needs as an ancestor once, the replay cache
/// serving the repeats (hits counted), votes all the while, and catches up when the delay ends.
#[test]
fn a_lagging_verifier_replays_each_block_once_and_keeps_voting() {
    logs();
    let (root, homes, genesis, seeds) = summon_programs("lagging", true, Some(20), &GOVERNANCE);
    let executor = deterministic::Runner::timed(Duration::from_secs(6_000));
    executor.start(|context| async move {
        let g = &genesis;
        let mut oracle = network(&context, g, &[]).await;
        let vals = start_all(&context, &mut oracle, g, &homes, &seeds).await;
        let mut watch = Watch::new();
        watch.poll_ms = 20;
        until_height(&context, &mut watch, &vals, 3).await;
        let mut u = Users::new(&seeds);
        let d = deploy(&context, &mut watch, &vals, g, &mut u).await;
        let caches: Vec<ReplayCache> = vals.iter().map(|v| ReplayCache::of(&v.chain).expect("a node's cache")).collect();
        let lagging = name(&vals[3]);
        let hits_before = consensus::replay_cache_hits();

        // ---- The control: full blocks, nobody delayed.
        let votes = |vals: &[Val]| -> Vec<usize> { vals.iter().map(|v| v.protection.log().len()).collect() };
        let (since, from, votes_since) = (caches.iter().map(|c| (c.served(), c.ran())).collect::<Vec<_>>(), floor(&vals), votes(&vals));
        watch.reset_lag();
        let ids = full_blocks(&vals, g, &mut u, &d.counter, 20);
        for id in &ids {
            included_everywhere(&context, &mut watch, &vals, id).await;
        }
        let to = floor(&vals);
        let control_signed = certified_by(&vals, &vals[3], from + 1..=to);
        let control_votes: Vec<usize> = votes(&vals).iter().zip(&votes_since).map(|(now, then)| now - then).collect();
        let control_cache = cache_use(&caches, &since);
        println!("control: blocks {from}..{to}, validator 3 in {control_signed} certificates, votes {control_votes:?}, lag {:?}, cache {control_cache:?}", watch.lag_of(&lagging));
        // Nobody lags in the control, so no replay is repeated: the cache serves nothing on any node.
        // (The hits below are therefore the delay's, not a property of the 20 ms blocks.)
        assert!(control_cache.iter().all(|(hits, _)| *hits == 0), "the replay cache served replays with nobody delayed: (hits, blocks run) per node {control_cache:?}");

        // ---- Validator 3's execution delayed, under 30 more full blocks.
        let (since, from, votes_since) = (caches.iter().map(|c| (c.served(), c.ran())).collect::<Vec<_>>(), floor(&vals), votes(&vals));
        watch.reset_lag();
        vals[3].chain.lock().exec_delay_ms = 90;
        let ids = full_blocks(&vals, g, &mut u, &d.counter, 30);
        for id in &ids {
            until(&context, &mut watch, &vals, "the calls", |vals| vals[..3].iter().all(|v| v.chain.lock().has_tx(id))).await;
        }
        let to = floor(&vals[..3]);
        let blocks = to - from;
        let (mean_lag, max_lag) = watch.lag_of(&lagging);
        let others_lag = ["v0", "v1", "v2"].map(|n| watch.lag_of(n).0);
        let used = cache_use(&caches, &since);
        let signed = certified_by(&vals, &vals[3], from + 1..=to);
        let delayed_votes: Vec<usize> = votes(&vals).iter().zip(&votes_since).map(|(now, then)| now - then).collect();
        println!("delayed: blocks {from}..{to}, validator 3 in {signed} certificates, votes {delayed_votes:?}, lag mean {mean_lag:.2} max {max_lag} (others {others_lag:.2?}), cache (hits, ran) {used:?}");
        // The blocks were full of program fuel.
        let limit = programs::params::param(&vals[0].chain.lock().state, programs::params::Param::MaxBlockFuel);
        let full = (from + 1..=to).filter(|h| vals[0].chain.lock().block(*h).unwrap().programs.values().map(|r| r.fuel.reserved).sum::<u64>() + 1_000 >= limit).count();
        assert!(full as u64 * 10 >= blocks * 8, "{full} of {blocks} blocks counted full fuel");
        // Its execution was behind, boundedly: a block or more behind most of the time, never far.
        assert!(mean_lag >= 0.5 && others_lag.iter().all(|l| *l < mean_lag), "validator 3 trailed by {mean_lag:.2} blocks on average, the others by {others_lag:.2?}");
        assert!(max_lag <= 6, "and by {max_lag} at the most");
        // It kept voting: it signed votes (through slashing protection, as every validator does) at
        // least four fifths as often as the least of the others, and is in the certificates.
        let least_other = *delayed_votes[..3].iter().min().unwrap();
        assert!(delayed_votes[3] * 5 >= least_other * 4 && delayed_votes[3] as u64 >= blocks, "validator 3 signed {} votes, the least of the others {least_other}, over {blocks} blocks", delayed_votes[3]);
        assert!(signed > 0, "and its signature is in the blocks' certificates ({signed})");
        // The lagging validator replayed the most ancestors (the others ran a few each, in their own
        // leader turns), and its cache served the repeats: a regression to a cache that serves a few
        // entries, or none, leaves `hits` short of its share of the blocks run.
        let (hits, ran) = used[3];
        let others_ran = used[..3].iter().map(|(_, ran)| *ran).max().unwrap();
        assert!(ran > others_ran, "validator 3 ran {ran} blocks as ancestors, another validator {others_ran}: the delay put no replay on it");
        assert!(hits >= 2 && hits * HITS_SHARE >= ran, "validator 3's replay cache served {hits} replays of the {ran} blocks it ran as ancestors over {blocks} blocks (at least 1 in {HITS_SHARE})");
        assert!(consensus::replay_cache_hits() >= hits_before + hits, "the node's metric counted them");

        // ---- The delay ends: it catches up to the same roots.
        vals[3].chain.lock().exec_delay_ms = 0;
        until_height(&context, &mut watch, &vals, to + 5).await;
        until(&context, &mut watch, &vals, "validator 3 to catch up", |vals| vals[3].chain.lock().height() + 1 >= floor(&vals[..3])).await;
        assert!(ids.iter().all(|id| vals[3].chain.lock().has_tx(id)), "it executed every call");
        assert!(watch.compared() as u64 + 3 >= floor(&vals), "{} heights compared", watch.compared());
        finish(&vals);
    });
    std::fs::remove_dir_all(root).ok();
}

// ------------------------------------------------------------------------------------ scenario 4

/// 4: programs off at genesis (a chain born at v2 without the `programs` feature, as `blockula-1`
/// launches): no program state, every program transaction refused, on every validator. The council
/// passes `ActivateFeature { programs }`, which runs at one height: META `programs` appears in that
/// block on every validator, with the same roots before it and after, and programs work from the
/// next block.
#[test]
fn programs_switched_on_by_council_vote_at_a_height() {
    logs();
    let (root, homes, genesis, seeds) = summon_programs("programs-vote", false, None, &GOVERNANCE);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let g = &genesis;
        let mut oracle = network(&context, g, &[]).await;
        let vals = start_all(&context, &mut oracle, g, &homes, &seeds).await;
        let mut watch = Watch::new();
        until_height(&context, &mut watch, &vals, 3).await;
        let mut u = Users::new(&seeds);
        fund(&context, &mut watch, &vals, g, &mut u).await;

        // Off: no program state, and every program transaction is refused by every node.
        let status_at = |v: &Val, height: u64| programs::status(&v.chain.lock().state, height);
        for (i, v) in vals.iter().enumerate() {
            {
                let c = v.chain.lock();
                let p = &c.state.programs;
                assert!(p.meta.is_none() && p.records.is_empty() && p.code.is_empty() && p.storage.is_empty(), "node {i} has program state");
            }
            let next = v.chain.lock().height() + 1;
            assert_eq!(status_at(v, next), programs::Status::Dormant);
            for (what, tx) in [("an upload", u.alice.sign(g, upload(COUNTER))), ("a create", u.alice.sign(g, create(COUNTER, None))), ("a call", u.alice.sign(g, increment(&made_by(g, &u.alice.keys, 0, COUNTER), 300_000))), ("a pause", u.validators[0].sign(g, Action::ProgramsPause {}))] {
                let e = door(v, tx).unwrap_or_else(|| panic!("node {i} took {what} while programs are off"));
                assert!(e.contains("programs are off"), "node {i} refused {what} in other words: {e}");
            }
        }

        // The council passes the activation; it's queued for one height, the same on every node.
        let id = council_votes(&context, &mut watch, &vals, g, &mut u, ProposalKind::ActivateFeature { feature: PROGRAMS_FEATURE.into() }, "programs on").await;
        until(&context, &mut watch, &vals, "the proposal to be queued", |vals| vals.iter().all(|v| v.chain.lock().state.governance.proposals[&id].execute_at.is_some())).await;
        let at = vals[0].chain.lock().state.governance.proposals[&id].execute_at.unwrap();
        assert!(vals.iter().all(|v| v.chain.lock().state.governance.proposals[&id].execute_at == Some(at)));
        assert!(vals.iter().all(|v| v.chain.lock().height() < at), "queued before it runs");
        until_height(&context, &mut watch, &vals, at + 2).await;

        // META appeared in block `at` and in no block before it, on every node; the roots agree
        // around it (the watch compared every height's), and differ across it.
        let rates = watch.fuel_rates();
        assert_eq!(rates.keys().next(), Some(&at), "programs' META first read at block {at} (sampled at the end of each height): {rates:?}");
        let roots: Vec<Vec<String>> = (at - 1..=at + 1).map(|h| vals.iter().map(|v| v.chain.lock().root_at(h).unwrap().to_string()).collect()).collect();
        for (h, r) in (at - 1..=at + 1).zip(&roots) {
            assert!(r.windows(2).all(|w| w[0] == w[1]), "roots at {h}: {r:?}");
        }
        assert!(roots[0][0] != roots[1][0] && roots[1][0] != roots[2][0], "the activation changed the state");
        assert!(vals.iter().all(|v| v.chain.lock().state.programs.meta.as_ref().is_some_and(|m| m.programs == 0 && m.fuel_rate == 250)), "at its defaults, with no programs yet");
        assert!(vals.iter().all(|v| status_at(v, at + 1) == programs::Status::On));

        // On: the guests deploy and run, and the nodes keep agreeing.
        let d = deploy_guests(&context, &mut watch, &vals, g, &mut u).await;
        let count = u.bob.send(&vals[1], g, increment(&d.counter, 300_000)).unwrap();
        included_everywhere(&context, &mut watch, &vals, &count).await;
        assert!(matches!(receipt_of(&vals[0], &count).1.outcome, programs::receipt::ReceiptOutcome::Ok));
        assert!(vals.iter().all(|v| counted(&v.chain.lock().state, &d.counter) == 1));
        assert!(watch.compared() as u64 + 3 >= floor(&vals), "{} heights compared", watch.compared());
        println!("programs off until block {at}, on after; deployed and called by {}", floor(&vals));
        finish(&vals);
    });
    std::fs::remove_dir_all(root).ok();
}

// ------------------------------------------------------------------------------------ scenario 1

/// The four guests deployed.
#[derive(Debug)]
struct Deployed {
    counter: Prog,
    token: Prog,
    escrow: Prog,
    router: Prog,
    /// The router that `router` forwards to.
    hop: Prog,
}

/// The users: Alice deploys, Bob and Carol use what she deployed. Validator 3 funds them; the
/// validators' own accounts also sign what only they can (a pause, a council vote).
struct Users {
    validators: Vec<Wallet>,
    alice: Wallet,
    bob: Wallet,
    carol: Wallet,
}

impl Users {
    fn new(seeds: &[[u8; 32]]) -> Users {
        Users { validators: seeds.iter().map(|s| Wallet::new(Keys::from_seed(*s))).collect(), alice: Wallet::new(user(81)), bob: Wallet::new(user(82)), carol: Wallet::new(user(83)) }
    }
}

/// [`fund`]s the users and [`deploy_guests`] (a program is callable from the block after its
/// creation, A5).
async fn deploy(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], g: &Genesis, u: &mut Users) -> Deployed {
    fund(context, watch, vals, g, u).await;
    deploy_guests(context, watch, vals, g, u).await
}

/// Validator 3 funds the users with 1,000 FANG each.
async fn fund(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], g: &Genesis, u: &mut Users) {
    for who in [&u.alice.address(), &u.bob.address(), &u.carol.address()] {
        let tx = transfer(g, &u.validators[3].keys, u.validators[3].nonce, who, 1_000 * FANG);
        vals[3].chain.lock().submit(tx).unwrap();
        u.validators[3].nonce += 1;
    }
    until(context, watch, vals, "the users' funds", |vals| vals.iter().all(|v| v.chain.lock().state.account(&u.carol.address()).balance > 0)).await;
}

/// Uploads the four guests (a block apart from their creates), creates each (the token with `init`,
/// a second router for the first to forward to) and waits until they're callable.
async fn deploy_guests(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], g: &Genesis, u: &mut Users) -> Deployed {    // Uploads, then (once they're stored: usable from the next block) the creates.
    let uploaded = [COUNTER, TOKEN, ESCROW, ROUTER];
    for guest in uploaded {
        u.alice.send(&vals[0], g, upload(guest)).unwrap();
    }
    let stored = |vals: &[Val]| vals.iter().all(|v| uploaded.iter().all(|gst| v.chain.lock().state.programs.code.contains_key(&digest::wasm_hash(gst.0))));
    until(context, watch, vals, "the uploads", stored).await;
    let creates = u.alice.nonce;
    let owner = bytes32(&u.alice.address());
    let token_init = input(&[&owner, &1_000_000u64.to_le_bytes()]);
    let made = Deployed { counter: made_by(g, &u.alice.keys, creates, COUNTER), token: made_by(g, &u.alice.keys, creates + 1, TOKEN), escrow: made_by(g, &u.alice.keys, creates + 2, ESCROW), router: made_by(g, &u.alice.keys, creates + 3, ROUTER), hop: made_by(g, &u.alice.keys, creates + 4, ROUTER) };
    for (guest, init) in [(COUNTER, None), (TOKEN, Some(("init", token_init))), (ESCROW, None), (ROUTER, None), (ROUTER, None)] {
        u.alice.send(&vals[1], g, create(guest, init)).unwrap();
    }
    let all = [&made.counter, &made.token, &made.escrow, &made.router, &made.hop];
    until(context, watch, vals, "the creates", |vals| vals.iter().all(|v| all.iter().all(|p| v.chain.lock().state.programs.records.contains_key(&p.address)))).await;
    // Callable from the block after the creation: one more block.
    let h = floor(vals);
    until_height(context, watch, vals, h + 1).await;
    made
}


/// `full` fuel for one call: with the counter's load, four of them fill the block's fuel budget (40M)
/// to within a few units, so every block of four is full.
fn full_call_fuel(load: u64) -> u64 {
    10_000_000 - load - 1
}

/// The counter's count (`u64`, 0 before its first call) in `state`.
fn counted(state: &crate::chain::State, counter: &Prog) -> u64 {
    let key = programs::state::storage_key(&crate::crypto::address_bytes(&counter.address).unwrap(), b"n");
    state.programs.storage.get(&key).map_or(0, |e| u64::from_le_bytes(e.value.as_slice().try_into().unwrap()))
}

/// The program receipt of `id` and its block, from `v`'s block log.
fn receipt_of(v: &Val, id: &str) -> (u64, programs::receipt::ProgramReceipt) {
    let c = v.chain.lock();
    let (h, i) = c.tx_location(id).expect("included");
    (h, c.block(h).unwrap().programs.get(&i).cloned().expect("a program receipt"))
}

/// Waits for `id` to be included on every node, which agree where: its height.
async fn included_everywhere(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], id: &str) -> u64 {
    until(context, watch, vals, &format!("tx {} on every node", &id[..8]), |vals| vals.iter().all(|v| v.chain.lock().has_tx(id))).await;
    let heights: Vec<Option<u64>> = vals.iter().map(|v| included_at(v, id)).collect();
    assert!(heights.windows(2).all(|w| w[0] == w[1]), "tx {id} was included at {heights:?}");
    heights[0].unwrap()
}

/// Asserts that META's `fuel_rate` followed §5.6's rule at every height of `heights` that was
/// sampled at its end and the one before: the next rate of the one before and the block's counted
/// fuel (Σ `reserved` over its receipts), on the parameters in force. Returns how many heights it
/// checked.
fn check_controller(vals: &[Val], watch: &Watch, heights: std::ops::RangeInclusive<u64>) -> usize {
    let rates = watch.fuel_rates();
    let c = vals[0].chain.lock();
    let (limit, floor) = (programs::params::param(&c.state, programs::params::Param::MaxBlockFuel), programs::params::param(&c.state, programs::params::Param::FuelFloorRate));
    let mut checked = 0;
    for h in heights {
        let (Some(before), Some(after)) = (h.checked_sub(1).and_then(|p| rates.get(&p)), rates.get(&h)) else { continue };
        let used: u64 = c.block(h).unwrap().programs.values().map(|r| r.fuel.reserved).sum();
        let want = programs::fuel::next_rate((*before).max(floor), used, limit, &c.state.chain_params).max(floor);
        assert_eq!(*after, want, "fuel_rate after block {h}: {before} then a block counting {used} of {limit} fuel");
        checked += 1;
    }
    checked
}

/// Joins a node from the newest certified snapshot of `vals[0]`, as `blockula join` does (the
/// same verification, install and verified follow as the fast-sync scenario's): the snapshot's
/// height, the node (whose home is `joiner_home`) and the epoch provider that verifies the blocks
/// it follows.
async fn join_from_snapshot(context: &deterministic::Context, vals: &[Val], genesis: &Genesis, joiner_home: &std::path::Path) -> (u64, Shared, consensus::EpochProvider) {
    use crate::snapshot;
    // Wait for validator 0's snapshot writer and certificate, then build the manifest.
    let mut manifest = None;
    for _ in 0..2_000 {
        if let Ok(m) = snapshot::build_manifest(&vals[0].chain.lock().live) {
            manifest = Some(m);
            break;
        }
        context.sleep(Duration::from_millis(250)).await;
    }
    let m = manifest.expect("a certified snapshot");
    let home0 = vals[0].home.clone();
    let chunks: Vec<Vec<u8>> = (0..m.chunks.len()).map(|i| snapshot::chunk(&home0, m.height, i).unwrap()).collect();
    // The validator-set history comes from /api/epochs pages (the node's whole registry here).
    let history = {
        let live = vals[0].chain.lock().live.clone();
        let page = |from: u64| -> (crate::epochs::EpochRegistry, Option<u64>) {
            let v: serde_json::Value = serde_json::from_slice(&live.epoch_page(from)).unwrap();
            (serde_json::from_value(v.clone()).unwrap(), v["next"].as_u64())
        };
        let (mut h, mut next) = page(0);
        while let Some(from) = next {
            let (p, n) = page(from);
            h.append_page(&p).unwrap();
            next = n;
        }
        h
    };
    let v = snapshot::verify_manifest(genesis, &m, &history).unwrap();
    // The state as the chunks rebuild it, checked against the certified root.
    let state = snapshot::restore(genesis, &v, &chunks).unwrap();
    std::fs::create_dir_all(joiner_home).unwrap();
    std::fs::copy(vals[0].home.join("genesis.json"), joiner_home.join("genesis.json")).unwrap();
    let joiner: Shared = Arc::new(Mutex::new(Chain::open(joiner_home).unwrap()));
    let packed = joiner_home.join("base.state.zst.part");
    std::fs::write(&packed, chunks.concat()).unwrap();
    joiner.lock().install_snapshot(&v, state, &packed).unwrap();
    assert_eq!(joiner.lock().base_height(), m.height);
    let provider = consensus::EpochProvider::new(joiner.clone(), None);
    (m.height, joiner, provider)
}

/// Follows `vals[0]`'s chain with verified batches (the path `join` uses over HTTP) until `joiner`
/// has executed `target`.
fn follow(joiner: &Shared, provider: &consensus::EpochProvider, from: &Val, target: u64) {
    while joiner.lock().height() < target {
        let next = joiner.lock().height() + 1;
        let c0 = from.chain.lock();
        let epoch = c0.state.params.epoch_of(next);
        let end = c0.state.params.last_of(epoch).min(c0.height());
        let to = (next..=end).find(|h| c0.cert(*h).is_some()).expect("a certificate in range");
        let batch = crate::node::SyncBatch { tip: c0.height(), blocks: (next..=to).map(|h| hex::encode(c0.cblock(h).unwrap())).collect(), to: Some(to), finalization: Some(hex::encode(c0.cert(to).unwrap())) };
        drop(c0);
        let verifier = provider.epoch_scheme(epoch).unwrap();
        crate::node::apply_batch(joiner, &verifier, epoch, &batch).unwrap();
    }
}

/// A council vote: `alice` proposes `kind` (deposit 1 FANG), validators 0 to 2 (⅔ of the four, the
/// bootstrap council) vote aye on what it renders to, and the proposal runs: returns once it has
/// on every node.
async fn council_passes(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], g: &Genesis, u: &mut Users, kind: ProposalKind, title: &str) {
    use crate::governance::ProposalStatus;
    let id = council_votes(context, watch, vals, g, u, kind, title).await;
    until(context, watch, vals, &format!("proposal #{id} to run"), |vals| vals.iter().all(|v| v.chain.lock().state.governance.proposals[&id].status == ProposalStatus::Executed)).await;
}

/// The first half of [`council_passes`]: the proposal and the council's votes. Returns its id.
async fn council_votes(context: &deterministic::Context, watch: &mut Watch, vals: &[Val], g: &Genesis, u: &mut Users, kind: ProposalKind, title: &str) -> u64 {
    let id = vals[0].chain.lock().state.governance.next_proposal_id;
    u.alice.send(&vals[0], g, Action::Propose { kind, deposit: FANG, title: title.into(), url: String::new() }).unwrap();
    until(context, watch, vals, "the proposal", |vals| vals.iter().all(|v| v.chain.lock().state.governance.proposals.contains_key(&id))).await;
    let digest = crate::governance::digest_of(&vals[1].chain.lock().state.governance.proposals[&id]);
    for (voter, node) in u.validators.iter_mut().zip(vals).take(3) {
        voter.send(node, g, Action::Vote { proposal_id: id, aye: true, amount: FANG, conviction: 0, proposal_digest: digest.clone() }).unwrap();
    }
    id
}

/// Restarts validator `i` (its run number `run`): it stops, and with `from_logs` its state database
/// is deleted, so that it replays every block of its logs, program blocks included, checking the
/// receipts it computes against the logged ones (`Chain::receipt_mismatches`). The other three go on
/// meanwhile, 3 of 4 being a quorum: they run program calls if `calls`, else (a pause refuses those)
/// they move FANG. Returns once the restarted node has caught up on what it missed.
#[allow(clippy::too_many_arguments)]
async fn restart(context: &deterministic::Context, watch: &mut Watch, oracle: &mut Oracle<PublicKey, deterministic::Context>, vals: &mut Vec<Val>, g: &Genesis, u: &mut Users, counter: &Prog, i: usize, run: usize, from_logs: bool, calls: bool) {
    let mut v = vals.remove(i);
    v.handle.take().unwrap().abort();
    let (home, key) = (v.home.clone(), v.key.clone());
    let crashed_at = v.chain.lock().height();
    drop(v);
    if from_logs {
        std::fs::remove_file(home.join(crate::store::DB_FILE)).unwrap();
    }
    let alice = u.alice.address();
    let meanwhile: Vec<String> = (0..6).map(|_| if calls { u.bob.send(&vals[0], g, increment(counter, 300_000)) } else { u.carol.send(&vals[0], g, Action::Transfer { to: alice.clone(), amount: FANG }) }.unwrap()).collect();
    until(context, watch, vals, "the transactions made while it's down", |vals| vals.iter().all(|v| meanwhile.iter().all(|id| v.chain.lock().has_tx(id)))).await;
    let restarted = start(context, oracle, g, i, run, home, key).await;
    restarted.chain.lock().state.programs.node = Some(i);
    let (opened_at, replayed) = {
        let c = restarted.chain.lock();
        (c.height(), c.replayed)
    };
    assert_eq!((opened_at, replayed), (crashed_at, if from_logs { crashed_at } else { 0 }), "validator {i} reopens at its height, replaying {}", if from_logs { "every block from its logs" } else { "nothing: its database has them" });
    vals.insert(i, restarted);
    let target = vals.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, v)| v.chain.lock().height()).min().unwrap() + 4;
    until_height(context, watch, vals, target).await;
    assert!(meanwhile.iter().all(|id| vals[i].chain.lock().has_tx(id)), "validator {i} caught up on what it missed");
}

#[test]
fn programs_on_at_genesis() {
    logs();
    let (root, homes, genesis, seeds) = summon_programs("programs-on", true, None, &GOVERNANCE);
    let joiner_home = root.join("joiner");
    let executor = deterministic::Runner::timed(Duration::from_secs(6_000));
    executor.start(|context| async move {
        let g = &genesis;
        let mut oracle = network(&context, g, &[]).await;
        let mut vals = start_all(&context, &mut oracle, g, &homes, &seeds).await;
        let mut watch = Watch::new();
        until_height(&context, &mut watch, &vals, 3).await;
        let mut u = Users::new(&seeds);
        let d = deploy(&context, &mut watch, &vals, g, &mut u).await;
        let (alice, bob, carol) = (u.alice.address(), u.bob.address(), u.carol.address());

        // ---- Calls: a token transfer, counter increments, and a router call that composes the
        // token and the escrow through its bindings (the router forwards to a second router, which
        // moves its own token balance, after Alice funded it). A call that fails is charged and
        // included by whoever leads.
        let transfer_to = |to: &str, amount: u64| call(&d.token, "transfer", input(&[&bytes32(to), &amount.to_le_bytes()]), 0);
        let fund_bob = u.alice.send(&vals[0], g, calls(vec![transfer_to(&bob, 1_000)], 2_000_000, vec![])).unwrap();
        let fund_hop = u.alice.send(&vals[0], g, calls(vec![transfer_to(&d.hop.address, 10_000)], 2_000_000, vec![])).unwrap();
        let three = u.bob.send(&vals[1], g, calls(vec![call(&d.counter, "increment", vec![], 0); 3], 1_000_000, vec![])).unwrap();
        let one = u.carol.send(&vals[2], g, increment(&d.counter, 300_000)).unwrap();
        let overdraft = u.carol.send(&vals[2], g, calls(vec![transfer_to(&bob, 1_000_000_000_000)], 2_000_000, vec![])).unwrap();
        for id in [&fund_bob, &fund_hop, &three, &one, &overdraft] {
            included_everywhere(&context, &mut watch, &vals, id).await;
        }
        assert!(failed(&vals[0], &overdraft) && !failed(&vals[0], &three));
        let (_, receipt) = receipt_of(&vals[1], &overdraft);
        assert!(matches!(receipt.outcome, programs::receipt::ReceiptOutcome::Aborted { abort_code: 11, .. }), "{:?}", receipt.outcome);
        assert!(receipt.fee.bytes + receipt.fee.fuel > 0, "a failed call is charged");
        let escrow_open = call(&d.escrow, "open", input(&[&1u64.to_le_bytes(), &bytes32(&carol)]), 5 * FANG);
        let route = call(&d.router, "send", input(&[&bytes32(&d.hop.address), &bytes32(&d.token.address), &bytes32(&carol), &250u64.to_le_bytes()]), 0);
        let composed = u.bob.send(&vals[1], g, calls(vec![escrow_open, route], 4_000_000, vec![bind(&d.hop), bind(&d.token)])).unwrap();
        included_everywhere(&context, &mut watch, &vals, &composed).await;
        let (_, receipt) = receipt_of(&vals[3], &composed);
        assert!(matches!(receipt.outcome, programs::receipt::ReceiptOutcome::Ok), "{:?}", receipt.outcome);
        let names: Vec<&str> = receipt.events.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["open", "transfer"], "the escrow's deal and the token's transfer, through the routers");
        assert_eq!(receipt.calls[1].return_data.0, 9_750u64.to_le_bytes(), "the last router's token balance after the hop");

        // ---- A call past what's left of the block's fuel budget (40M) waits for the next block: five
        // calls each reserving a quarter of it fill one block with four and leave the fifth.
        let load = blockula_vm::meter::SCHEDULE_V0.load_fuel(COUNTER.0.len());
        let quarter = full_call_fuel(load);
        let five: Vec<String> = (0..5).map(|_| u.alice.send(&vals[0], g, increment(&d.counter, quarter)).unwrap()).collect();
        let mut at = vec![];
        for id in &five {
            at.push(included_everywhere(&context, &mut watch, &vals, id).await);
        }
        assert!(at[..4].iter().all(|h| *h == at[0]) && at[4] > at[0], "four fill a block and the fifth waits: {at:?}");
        let counted_in = |h: u64| -> u64 { vals[0].chain.lock().block(h).unwrap().programs.values().map(|r| r.fuel.reserved).sum() };
        let limit = programs::params::param(&vals[0].chain.lock().state, programs::params::Param::MaxBlockFuel);
        assert!(counted_in(at[0]) <= limit && counted_in(at[0]) + 10 > limit, "the first block is full: {} of {limit}", counted_in(at[0]));
        assert_eq!(counted_in(at[4]), quarter + load);

        // ---- Full-fuel blocks raise `fuel_rate` on every validator alike, and empty ones lower it:
        // 32 calls from three senders fill eight blocks.
        let wave_from = floor(&vals) + 1;
        let mut wave = vec![];
        for n in 0..32 {
            wave.push(match n % 3 {
                0 => u.alice.send(&vals[0], g, increment(&d.counter, quarter)),
                1 => u.bob.send(&vals[1], g, increment(&d.counter, quarter)),
                _ => u.carol.send(&vals[2], g, increment(&d.counter, quarter)),
            }
            .unwrap());
        }
        let mut wave_at = vec![];
        for id in &wave {
            wave_at.push(included_everywhere(&context, &mut watch, &vals, id).await);
        }
        let wave_end = *wave_at.iter().max().unwrap();
        // Then empty blocks, until the rate is back on its floor.
        until_height(&context, &mut watch, &vals, wave_end + 12).await;
        let rates = watch.fuel_rates();
        let floor_rate = programs::params::param(&vals[0].chain.lock().state, programs::params::Param::FuelFloorRate);
        let checked = check_controller(&vals, &watch, wave_from..=wave_end + 12);
        assert!(checked >= 18, "the controller's rule was checked at {checked} heights");
        let peak = (wave_from..=wave_end).map(|h| rates[&h]).max().unwrap();
        assert!(peak > floor_rate * 2, "eight full blocks more than double the rate: {peak} over a floor of {floor_rate}");
        assert_eq!(rates[&(wave_end + 12)], floor_rate, "empty blocks bring it back to the floor");
        for h in wave_from..=wave_end + 12 {
            assert!(watch.rate_witnesses(h) >= 3, "height {h}'s fuel_rate was read on {} nodes", watch.rate_witnesses(h));
        }
        println!("calls, the wave ({wave_from}..={wave_end}, rate {floor_rate} to {peak}) done at {}", floor(&vals));

        // ---- Restarts: validator 1 from its state database, validator 2 from its block logs (its
        // database deleted, so it replays every block, program blocks included, and checks the
        // receipts it computes against the logged ones). The other three go on running programs
        // meanwhile (3 of 4 is a quorum); each catches up to the same roots (the watch compares them).
        for (i, from_logs) in [(1usize, false), (2, true)] {
            restart(&context, &mut watch, &mut oracle, &mut vals, g, &mut u, &d.counter, i, 1, from_logs, true).await;
        }
        println!("restarts done at {}", floor(&vals));

        // ---- A node that starts from a snapshot taken after the uploads: it carries the programs'
        // code, records and storage (namespaces 66-69 and 72), and executes the program blocks after
        // it. The first certified snapshot is at block 100 (certified at 104).
        let before: Vec<String> = (0..6).map(|n| if n % 2 == 0 { u.alice.send(&vals[0], g, increment(&d.counter, 300_000)) } else { u.carol.send(&vals[2], g, increment(&d.counter, 300_000)) }.unwrap()).collect();
        until(&context, &mut watch, &vals, "a certified snapshot", |vals| crate::snapshot::build_manifest(&vals[0].chain.lock().live).is_ok()).await;
        let (snapshot_at, joiner, provider) = join_from_snapshot(&context, &vals, g, &joiner_home).await;
        {
            let c = joiner.lock();
            let counter_then = counted(&c.state, &d.counter);
            let p = &c.state.programs;
            assert_eq!((p.code.len(), p.interfaces.len(), p.records.len()), (4, 4, 5), "the snapshot carries the code, interfaces and records");
            assert!(counter_then >= 30 && !p.storage.is_empty() && !p.blob_refs.is_empty(), "and the storage ({counter_then} counted) and the blobs' references");
            // Installing it also executed the manifest's blocks past it (to the certifying one).
            assert!(c.height() > snapshot_at);
            assert_eq!(c.state.root(), vals[0].chain.lock().root_at(c.height()).unwrap(), "at the validators' root for block {}", c.height());
        }
        assert!(before.iter().all(|id| included_at(&vals[0], id).is_some_and(|h| h < snapshot_at)), "the snapshot is of a chain that had run programs");
        // The chain goes on running programs after it; the node executes them from the restored state.
        let after: Vec<String> = (0..6).map(|n| if n % 2 == 0 { u.bob.send(&vals[1], g, increment(&d.counter, 300_000)) } else { u.alice.send(&vals[0], g, increment(&d.counter, 300_000)) }.unwrap()).collect();
        let mut last = 0;
        for id in &after {
            last = last.max(included_everywhere(&context, &mut watch, &vals, id).await);
        }
        let target = last + 2;
        until_height(&context, &mut watch, &vals, target).await;
        follow(&joiner, &provider, &vals[0], target);
        let h = joiner.lock().height();
        assert!(h >= target && h > snapshot_at);
        assert_eq!(joiner.lock().root_at(h), vals[0].chain.lock().root_at(h), "the node that started from the snapshot reached the validators' root at {h}");
        for at in snapshot_at + 1..=h {
            assert_eq!(joiner.lock().block(at).unwrap().programs, vals[0].chain.lock().block(at).unwrap().programs, "its receipts for block {at}");
        }
        assert!(after.iter().all(|id| included_at(&vals[0], id).is_some_and(|b| b > snapshot_at && joiner.lock().has_tx(id))), "it executed program calls after the snapshot");
        println!("snapshot at {snapshot_at}: a node started from it and followed to {h}");
        drop(provider);
        drop(joiner);

        // ---- The same home, as a validator's: validator 3 stops and its key starts again over the
        // home the snapshot installed and the verified batches extended (`start_validator` anchors
        // marshal at the certificate that home holds for its tip), with fresh consensus storage (a
        // partition of its own) and its slashing-protection record carried over, as an operator moving
        // a key would. It catches up from the other three, votes, proposes blocks that run program
        // calls from the restored state, and agrees with them on every root and receipt.
        let mut stopped = vals.remove(3);
        stopped.handle.take().unwrap().abort();
        let key = stopped.key.clone();
        let record = stopped.home.join(crate::protection::RECORD_FILE);
        if record.exists() {
            std::fs::copy(&record, joiner_home.join(crate::protection::RECORD_FILE)).unwrap();
        }
        drop(stopped);
        let restored = start(&context, &mut oracle, g, 4, 0, joiner_home.clone(), key).await;
        restored.chain.lock().state.programs.node = Some(3);
        let started_at = restored.chain.lock().height();
        let votes_before = restored.protection.log().len();
        vals.insert(3, restored);
        assert_eq!(vals[3].chain.lock().base_height(), snapshot_at, "its home is the snapshot's: no block of its own before it");
        assert!(started_at > snapshot_at, "and holds the blocks followed past it");
        until(&context, &mut watch, &vals, "the validator started from the snapshot to catch up", |vals| vals[3].chain.lock().height() + 1 >= floor(&vals[..3])).await;
        let me = u.validators[3].address();
        let mut proposed = None;
        for _ in 0..40 {
            u.bob.send(&vals[0], g, increment(&d.counter, 300_000)).unwrap();
            let next = floor(&vals) + 1;
            until_height(&context, &mut watch, &vals, next).await;
            let c = vals[0].chain.lock();
            proposed = (started_at + 1..=c.height()).find(|h| c.block(*h).is_some_and(|b| b.proposer == me && !b.programs.is_empty()));
            if proposed.is_some() {
                break;
            }
        }
        let proposed = proposed.expect("the validator started from the snapshot proposed a block that ran program calls");
        // It votes: through slashing protection, which signs nothing in a restarted validator's first
        // epoch until it has watched the network for a while, so its signature may take an epoch to
        // reach a certificate (they hold the first quorum's, not everyone's).
        until(&context, &mut watch, &vals, "the validator started from the snapshot to sign a certificate", |vals| certified_by(vals, &vals[3], started_at + 1..=floor(vals)) > 0).await;
        let top = floor(&vals);
        assert!(vals[3].protection.log().len() > votes_before, "and its votes went through slashing protection");
        for h in started_at + 1..=top {
            assert_eq!(vals[3].chain.lock().block(h).unwrap().programs, vals[0].chain.lock().block(h).unwrap().programs, "its receipts for block {h}");
        }
        assert_eq!(vals[3].chain.lock().root_at(top), vals[0].chain.lock().root_at(top), "and its root at {top}");
        println!("a validator started from the snapshot at {started_at}: proposed block {proposed} with program calls, caught up to {top}");


        // ---- `DeactivateFeature { programs }` passes by council vote: exit-only holds. A payout works
        // (Bob releases the escrow's deal to Carol), a call that carries value is refused, and so are
        // an upload and a create; calls that carry none go on running.
        let carol_before = vals[0].chain.lock().state.account(&carol).balance;
        council_passes(&context, &mut watch, &vals, g, &mut u, ProposalKind::DeactivateFeature { feature: PROGRAMS_FEATURE.into() }, "programs to exit-only").await;
        let status_now = |v: &Val| {
            let c = v.chain.lock();
            programs::status(&c.state, c.height() + 1)
        };
        assert!(vals.iter().all(|v| status_now(v) == programs::Status::ExitOnly), "exit-only on every node");
        let open_again = u.bob.sign(g, calls(vec![call(&d.escrow, "open", input(&[&2u64.to_le_bytes(), &bytes32(&carol)]), FANG)], 1_000_000, vec![]));
        for (i, v) in vals.iter().enumerate() {
            for (what, tx, phrase) in [("a call with value", open_again.clone(), "can't send a program FANG"), ("an upload", u.alice.sign(g, upload(COUNTER)), "programs are exit-only"), ("a create", u.alice.sign(g, create(COUNTER, None)), "programs are exit-only")] {
                let e = door(v, tx).unwrap_or_else(|| panic!("node {i} took {what} while programs are exit-only"));
                assert!(e.contains(phrase), "node {i} refused {what} in other words than {phrase:?}: {e}");
            }
        }
        let release = u.bob.send(&vals[1], g, calls(vec![call(&d.escrow, "release", 1u64.to_le_bytes().to_vec(), 0)], 1_000_000, vec![])).unwrap();
        let tick = u.alice.send(&vals[0], g, increment(&d.counter, 300_000)).unwrap();
        for id in [&release, &tick] {
            included_everywhere(&context, &mut watch, &vals, id).await;
            assert!(matches!(receipt_of(&vals[2], id).1.outcome, programs::receipt::ReceiptOutcome::Ok), "exit-only runs payouts and value-free calls");
        }
        assert!(vals.iter().all(|v| v.chain.lock().state.account(&carol).balance == carol_before + 5 * FANG), "the payout reached Carol on every node");

        // ---- `ActivateFeature` turns programs back on: the call that carries value works again.
        council_passes(&context, &mut watch, &vals, g, &mut u, ProposalKind::ActivateFeature { feature: PROGRAMS_FEATURE.into() }, "programs back on").await;
        assert!(vals.iter().all(|v| status_now(v) == programs::Status::On), "on again on every node");
        let reopened = u.bob.send(&vals[1], g, calls(vec![call(&d.escrow, "open", input(&[&2u64.to_le_bytes(), &bytes32(&carol)]), FANG)], 1_000_000, vec![])).unwrap();
        included_everywhere(&context, &mut watch, &vals, &reopened).await;
        assert!(matches!(receipt_of(&vals[0], &reopened).1.outcome, programs::receipt::ReceiptOutcome::Ok));
        println!("exit-only and back at {}", floor(&vals));

        // ---- Two validators sign `ProgramsPause`: every validator refuses program transactions from
        // the block after the one that carries the second signature, and transfers go on. The same
        // two can't sign again unless a fix is pending; with a `ScheduleUpgrade` open they can,
        // before the pause lapses (its lapse, 3 days of blocks later, is a unit test that sets
        // heights directly, §10.1), and it's renewed.
        let pause_of = |v: &Val| v.chain.lock().state.programs.meta.as_ref().unwrap().pause.clone();
        assert!(vals.iter().all(|v| pause_of(v).is_none()));
        let first = u.validators[0].send(&vals[0], g, Action::ProgramsPause {}).unwrap();
        until(&context, &mut watch, &vals, "the first signature", |vals| vals.iter().all(|v| v.chain.lock().state.programs.meta.as_ref().unwrap().votes.len() == 1)).await;
        assert!(vals.iter().all(|v| pause_of(v).is_none()), "one signature of two doesn't pause");
        // Twelve full-fuel calls are queued when the second signature goes in. The block that
        // carries it runs as many as its fuel budget holds (four; programs still run in it), and the
        // rest are left for the blocks of the pause, which refuse them: every node drops them.
        let backlog: Vec<String> = (0..12)
            .map(|n| match n % 3 {
                0 => u.alice.send(&vals[0], g, increment(&d.counter, quarter)),
                1 => u.bob.send(&vals[1], g, increment(&d.counter, quarter)),
                _ => u.carol.send(&vals[2], g, increment(&d.counter, quarter)),
            }
            .expect("programs still run: one signature isn't a pause"))
            .collect();
        let second = u.validators[1].send(&vals[1], g, Action::ProgramsPause {}).unwrap();
        until(&context, &mut watch, &vals, "the pause", |vals| vals.iter().all(|v| pause_of(v).is_some())).await;
        let pause = pause_of(&vals[0]).unwrap();
        assert!(vals.iter().all(|v| pause_of(v).as_ref() == Some(&pause)), "the same pause on every node");
        let mut signers: Vec<String> = u.validators[..2].iter().map(|w| w.address()).collect();
        signers.sort();
        assert_eq!(pause.signers, signers);
        // It starts at the block after the one that carried the second signature, not a block later
        // or earlier.
        let signed_at = included_everywhere(&context, &mut watch, &vals, &first).await.max(included_everywhere(&context, &mut watch, &vals, &second).await);
        assert_eq!(pause.from, signed_at + 1, "the pause starts at the block after the one with the second signature (at {signed_at})");
        until_height(&context, &mut watch, &vals, pause.from + 3).await;
        // The calls that ran did so before it; the others are in no block, then or since, and in no
        // mempool: the nodes dropped them when the block with the signature made the next one paused.
        let (ran, left): (Vec<&String>, Vec<&String>) = backlog.iter().partition(|id| included_at(&vals[0], id).is_some());
        assert!(!ran.is_empty() && !left.is_empty(), "the backlog straddles the pause: {} ran, {} left", ran.len(), left.len());
        assert!(ran.iter().all(|id| included_at(&vals[0], id).is_some_and(|h| h <= signed_at)), "every call that ran did so in or before the block with the signature (at {signed_at})");
        for (i, v) in vals.iter().enumerate() {
            let c = v.chain.lock();
            // Each sender's first call that was left is dropped for the pause, the sender's later ones
            // because it was.
            let why: Vec<String> = left.iter().map(|id| c.failure(id).map(|(why, _)| why.clone()).unwrap_or_default()).collect();
            for (id, why) in left.iter().zip(&why) {
                assert!(!c.has_tx(id) && !c.in_mempool(id), "node {i} still holds a call the pause refused, or ran it");
                assert!(why.contains("programs are paused until block") || why.contains("an earlier transaction from the same account was dropped"), "node {i} dropped it for: {why:?}");
            }
            assert!(why.iter().any(|w| w.contains("programs are paused until block")), "node {i} dropped a call because the pause refused it: {why:?}");
            let top = c.height();
            assert!((pause.from..=top).all(|h| c.block(h).unwrap().programs.is_empty()), "node {i}: a block of the pause ran a program transaction");
        }
        // (The dropped calls left their senders' nonces behind the wallets' count.)
        for w in [&mut u.alice, &mut u.bob, &mut u.carol] {
            w.nonce = vals[0].chain.lock().state.account(&w.address()).nonce;
        }
        let paused_for = pause.until - pause.from;
        assert_eq!(paused_for, programs::pause::Spans::of(&vals[0].chain.lock().state.params).pause_blocks, "three days of blocks");
        // Every node refuses programs from its first block on.
        for (i, v) in vals.iter().enumerate() {
            assert_eq!(status_now(v), programs::Status::Paused { until: pause.until }, "node {i}");
            for (what, tx) in [("a call", u.alice.sign(g, increment(&d.counter, 300_000))), ("an upload", u.alice.sign(g, upload(TOKEN))), ("a create", u.alice.sign(g, create(COUNTER, None)))] {
                let e = door(v, tx).unwrap_or_else(|| panic!("node {i} took {what} while programs are paused"));
                assert!(e.contains("programs are paused until block"), "node {i} refused {what} in other words: {e}");
            }
        }
        // Native transfers go on, and blocks keep coming.
        let pay = u.carol.send(&vals[2], g, Action::Transfer { to: alice.clone(), amount: FANG }).unwrap();
        included_everywhere(&context, &mut watch, &vals, &pay).await;
        let h = floor(&vals);
        until_height(&context, &mut watch, &vals, h + 5).await;
        // The window: the same two validators can't sign again, until a fix is pending.
        let e = door(&vals[0], u.validators[0].sign(g, Action::ProgramsPause {})).expect("a validator that started this pause can't sign again without a fix pending");
        assert!(e.contains("may sign again from block") && e.contains("a fix is pending"), "{e}");
        let fix_height = floor(&vals);
        let fix = u.alice.send(&vals[0], g, Action::Propose { kind: ProposalKind::ScheduleUpgrade { version: crate::upgrade::PREVIEW_PROTOCOL_VERSION, activation_height: fix_height + 300 }, deposit: FANG, title: "a fix for programs".into(), url: String::new() }).unwrap();
        included_everywhere(&context, &mut watch, &vals, &fix).await;
        u.validators[0].send(&vals[0], g, Action::ProgramsPause {}).unwrap();
        u.validators[1].send(&vals[1], g, Action::ProgramsPause {}).unwrap();
        until(&context, &mut watch, &vals, "the renewal", |vals| pause_of(&vals[0]).is_some_and(|p| p.until > pause.until) && vals.iter().all(|v| pause_of(v) == pause_of(&vals[0]))).await;
        let renewed = pause_of(&vals[0]).unwrap();
        assert_eq!((renewed.from, renewed.started), (pause.from, pause.started), "a renewal is the same run");
        assert!(renewed.until > pause.until && renewed.until - pause.until < 20, "its end moved a few blocks on: {} to {}", pause.until, renewed.until);
        assert!(vals.iter().all(|v| matches!(status_now(v), programs::Status::Paused { .. })));
        println!("paused at {}, renewed at {}", pause.from, floor(&vals));

        // ---- A second restart from the block logs, now, with the run's last blocks behind it:
        // validator 1 replays every block (the guests', the waves', the governance's and the pause's)
        // and checks the receipts it computes against the ones its logs hold, as validator 2 did
        // earlier: `finish` then has two nodes' replays to hold to `receipt_mismatches == 0`. (A
        // pause refuses programs, so the others move FANG meanwhile.)
        restart(&context, &mut watch, &mut oracle, &mut vals, g, &mut u, &d.counter, 1, 2, true, false).await;
        assert_eq!(vals.iter().filter(|v| v.chain.lock().replayed > 0).count(), 2, "validators 1 and 2 replayed their logs");

        // Every height from the first block to the last was compared (give or take the few the last
        // samples may not have reached), on at least two nodes.
        assert!(watch.compared() as u64 + 3 >= floor(&vals), "{} heights compared of {}", watch.compared(), floor(&vals));
        finish(&vals);
    });
    std::fs::remove_dir_all(root).ok();
}

// ------------------------------------------------------------------------------------ the pause's lane

/// §2.5 and §7.2: a `ProgramsPause` that a validator's consensus key signs from its own account is a
/// validator duty with a slot in the mempool's lane, which proposals fill first, "so a full mempool
/// can't keep it out". Here the blocks are full. `max_block_bytes` is at its floor (64 KiB, about
/// eleven transactions of an ML-DSA key and signature each) and 120 calls are queued ahead of the two
/// signatures, ten blocks' worth, all with the same (no) tip, so that a transaction outside the lane
/// would wait its turn in the queue. The second signature lands in the block after the one the
/// signatures were signed at, or the one after that (a proposal in the making may have missed them),
/// the pause starts at the block after it, and the calls it refuses are dropped.
///
/// The blocks are full by bytes, not by program fuel: the packer leaves out a program transaction
/// that is over the block's fuel budget and goes on to the next candidate, so a pause signature, which
/// is no program transaction, finds room in a block whose fuel is spent whether or not it has a lane.
#[test]
fn a_pause_signed_into_full_blocks_lands_through_its_duty_lane() {
    logs();
    let params = [GOVERNANCE[0], GOVERNANCE[1], GOVERNANCE[2], ("max_block_bytes", crate::types::MAX_TX_BYTES as u64)];
    let (root, homes, genesis, seeds) = summon_programs("pause-lane", true, None, &params);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let g = &genesis;
        let mut oracle = network(&context, g, &[]).await;
        let vals = start_all(&context, &mut oracle, g, &homes, &seeds).await;
        let mut watch = Watch::new();
        until_height(&context, &mut watch, &vals, 3).await;
        let mut u = Users::new(&seeds);
        let d = deploy(&context, &mut watch, &vals, g, &mut u).await;

        // The backlog: 40 calls from each of three senders (a sender queues up to 64), queued on three
        // nodes and gossiped. Blocks fill with it from the next one on.
        let backlog: Vec<String> = (0..120)
            .map(|n| match n % 3 {
                0 => u.alice.send(&vals[0], g, increment(&d.counter, 300_000)),
                1 => u.bob.send(&vals[1], g, increment(&d.counter, 300_000)),
                _ => u.carol.send(&vals[2], g, increment(&d.counter, 300_000)),
            }
            .unwrap())
            .collect();
        included_everywhere(&context, &mut watch, &vals, &backlog[0]).await;
        let signed_at = floor(&vals);
        let first = u.validators[0].send(&vals[0], g, Action::ProgramsPause {}).unwrap();
        let second = u.validators[1].send(&vals[1], g, Action::ProgramsPause {}).unwrap();
        let pause_of = |v: &Val| v.chain.lock().state.programs.meta.as_ref().unwrap().pause.clone();
        until(&context, &mut watch, &vals, "the pause", |vals| vals.iter().all(|v| pause_of(v).is_some())).await;
        let pause = pause_of(&vals[0]).unwrap();
        assert!(vals.iter().all(|v| pause_of(v).as_ref() == Some(&pause)), "the same pause on every node");

        // Both signatures went in with the next block or the one after, ahead of the backlog.
        let landed_at = included_everywhere(&context, &mut watch, &vals, &first).await.max(included_everywhere(&context, &mut watch, &vals, &second).await);
        assert!((signed_at + 1..=signed_at + 2).contains(&landed_at), "signed at block {signed_at}, the second signature landed at {landed_at}: it waited behind the backlog");
        assert_eq!(pause.from, landed_at + 1, "the pause starts at the block after the second signature");
        // The blocks were full, with calls queued behind the signatures that didn't get in.
        {
            let c = vals[0].chain.lock();
            let (txs, bytes): (Vec<usize>, Vec<usize>) = (signed_at + 1..=landed_at).map(|h| c.block(h).map(|b| (b.txs.len(), b.txs.iter().map(|t| t.to_bytes().len()).sum::<usize>())).unwrap()).unzip();
            println!("signed at {signed_at}, landed at {landed_at}: blocks of {txs:?} transactions, {bytes:?} bytes");
            assert!(txs.iter().all(|n| *n >= 8) && bytes.iter().all(|b| *b + 8_000 >= crate::types::MAX_TX_BYTES), "the blocks around the signatures were full: {txs:?} transactions, {bytes:?} bytes");
            let left = backlog.iter().filter(|id| !c.has_tx(id)).count();
            assert!(left >= 60, "{left} of the 120 calls were still queued behind the signatures");
        }
        // Whatever the backlog still held is refused by the pause and dropped; no block of the pause
        // runs a program.
        until_height(&context, &mut watch, &vals, pause.from + 3).await;
        for (i, v) in vals.iter().enumerate() {
            let c = v.chain.lock();
            assert!(backlog.iter().all(|id| !c.in_mempool(id)), "node {i} still queues calls the pause refuses");
            assert!((pause.from..=c.height()).all(|h| c.block(h).unwrap().programs.is_empty()), "node {i}: a block of the pause ran a program transaction");
        }
        assert!(watch.compared() as u64 + 3 >= floor(&vals), "{} heights compared", watch.compared());
        finish(&vals);
    });
    std::fs::remove_dir_all(root).ok();
}

/// §2.5's other form: a validator whose owner account is not its consensus key's (a registered
/// newcomer; summoned validators are their own owners) signs the pause with an owner key. That form
/// is no validator duty ("an owner key's signature competes by its tip"), so with the blocks full
/// it tips, and the validator's own consensus-key form, from another validator, takes its lane
/// slot. Together they reach the threshold of a set of five (f + 1 = 2) in the next block or the one
/// after, and the pause names the newcomer's validator (its consensus key's address), not its owner.
#[test]
fn a_pause_signed_by_an_owner_lands_with_a_tip() {
    logs();
    let params = [GOVERNANCE[0], GOVERNANCE[1], GOVERNANCE[2], ("max_block_bytes", crate::types::MAX_TX_BYTES as u64)];
    let (root, homes, genesis, seeds) = summon_programs("pause-owner", true, None, &params);
    let consensus_seed = [91u8; 32];
    let newcomer = newcomer_home(&root, &homes[0], "n0", consensus_seed);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let g = &genesis;
        let consensus_key = PrivateKey::from_seed_bytes(consensus_seed);
        let mut oracle = network(&context, g, &[commonware_cryptography::Signer::public_key(&consensus_key)]).await;
        let mut vals = start_all(&context, &mut oracle, g, &homes, &seeds).await;
        // The newcomer's node runs from the start, observing until its seat comes.
        vals.push(start(&context, &mut oracle, g, 4, 0, newcomer, consensus_key).await);
        vals[4].chain.lock().state.programs.node = Some(4);
        let mut watch = Watch::new();
        until_height(&context, &mut watch, &vals, 3).await;
        let mut u = Users::new(&seeds);
        fund(&context, &mut watch, &vals, g, &mut u).await;

        // Its owner is an account of its own, funded by validator 3, which registers the validator
        // (the consensus key's account is the validator's id) with twice the least self-bond.
        let mut owner = Wallet::new(Keys::from_seed([90; 32]));
        let validator = Keys::from_seed(consensus_seed).address();
        let bond = 2 * g.chain_params().unwrap().min_self_bond;
        let to_owner = transfer(g, &u.validators[3].keys, u.validators[3].nonce, &owner.address(), bond + 100 * FANG);
        vals[3].chain.lock().submit(to_owner).unwrap();
        u.validators[3].nonce += 1;
        until(&context, &mut watch, &vals, "the owner's funds", |vals| vals.iter().all(|v| v.chain.lock().state.account(&owner.address()).balance > bond)).await;
        let (p2p, http) = ("127.0.0.1:30600".to_string(), "127.0.0.1:30601".to_string());
        let pop = crate::chain::make_pop(consensus_seed, &g.chain_id, &owner.address(), &p2p, &http);
        owner.send(&vals[0], g, Action::RegisterValidator { consensus_key: hex::encode(Keys::from_seed(consensus_seed).public_key()), p2p_addr: p2p, http_addr: http, bond, pop }).unwrap();
        let in_set = |v: &Val| {
            let c = v.chain.lock();
            c.state.active_sets.get(&c.state.params.epoch_of(c.height() + 1)).is_some_and(|set| set.contains(&validator))
        };
        until(&context, &mut watch, &vals, "the newcomer to join the active set", |vals| vals.iter().all(in_set)).await;
        assert_eq!(vals[0].chain.lock().state.validators[&validator].owner, owner.address(), "its owner is not its own account");

        // A backlog of transfers (three senders, 40 each) fills the blocks; the two signatures go in
        // behind it: validator 0's own (a duty, a lane slot) and the newcomer's owner's, tipping.
        let (alice, bob) = (u.alice.address(), u.bob.address());
        let backlog: Vec<String> = (0..120)
            .map(|n| match n % 3 {
                0 => u.alice.send(&vals[0], g, Action::Transfer { to: bob.clone(), amount: FANG / 1_000 }),
                1 => u.bob.send(&vals[1], g, Action::Transfer { to: alice.clone(), amount: FANG / 1_000 }),
                _ => u.carol.send(&vals[2], g, Action::Transfer { to: alice.clone(), amount: FANG / 1_000 }),
            }
            .unwrap())
            .collect();
        included_everywhere(&context, &mut watch, &vals, &backlog[0]).await;
        let signed_at = floor(&vals);
        let tip = {
            let c = vals[0].chain.lock();
            let probe = owner.sign(g, Action::ProgramsPause {});
            crate::econ::max_tip(&c.state, crate::econ::base_fee(&c.state, c.height() + 1, probe.size())) / 2
        };
        assert!(tip > 0);
        let first = u.validators[0].send(&vals[0], g, Action::ProgramsPause {}).unwrap();
        let tx = owner.sign_tipping(g, Action::ProgramsPause {}, tip);
        let second = vals[1].chain.lock().submit(tx).expect("the owner form of the pause is admitted");
        owner.nonce += 1;
        assert!(vals[0].chain.lock().in_lane(&first) && !vals[1].chain.lock().in_lane(&second), "the consensus key's form is a lane duty, the owner's is not");
        let pause_of = |v: &Val| v.chain.lock().state.programs.meta.as_ref().unwrap().pause.clone();
        until(&context, &mut watch, &vals, "the pause", |vals| vals.iter().all(|v| pause_of(v).is_some())).await;
        let pause = pause_of(&vals[0]).unwrap();
        let landed_at = included_everywhere(&context, &mut watch, &vals, &first).await.max(included_everywhere(&context, &mut watch, &vals, &second).await);
        assert!((signed_at + 1..=signed_at + 2).contains(&landed_at), "signed at block {signed_at}, the second signature landed at {landed_at}: it waited behind the backlog");
        assert_eq!(pause.from, landed_at + 1);
        let mut signers = vec![u.validators[0].address(), validator];
        signers.sort();
        assert_eq!(pause.signers, signers, "the pause names the newcomer by its validator id, not its owner");
        assert!(backlog.iter().filter(|id| !vals[0].chain.lock().has_tx(id)).count() >= 60, "the backlog was still queued behind the signatures");
        until_height(&context, &mut watch, &vals, pause.from + 3).await;
        assert!(watch.compared() as u64 + 3 >= floor(&vals), "{} heights compared", watch.compared());
        finish(&vals);
    });
    std::fs::remove_dir_all(root).ok();
}
