//! Network faults and Byzantine validators (SEC-3), on the same 4-validator simulation as the
//! scenarios above: partitions that heal, clock skew within and beyond `MAX_CLOCK_DRIFT_MS`, and
//! validators that withhold their votes. Every scenario checks safety (no two validators ever
//! finalize different blocks, or reach different state roots, at a height) and liveness while
//! more than two thirds of the set is honest and connected; and that progress stops, safely,
//! when it isn't.

use super::*;
use crate::consensus::{CH_CERTS, CH_VOTES, MAX_CLOCK_DRIFT_MS};
use commonware_actor::{Feedback, Unreliable};
use commonware_cryptography::Signer as _;
use commonware_p2p::{CheckedSender, LimitedSender, Recipients};
use commonware_runtime::IoBufs;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::SystemTime;

/// A p2p sender that, while `mute` is set, accepts messages and drops them. On the vote and
/// certificate channels, that's a validator withholding its votes: certificates carry the votes
/// they aggregate, its own included, so a validator that kept forwarding them would still be
/// voting. It still receives everything, serves blocks and executes what the others finalize.
#[derive(Clone)]
struct Muted<S> {
    inner: S,
    mute: Option<Arc<AtomicBool>>,
}

impl<S: LimitedSender> LimitedSender for Muted<S> {
    type PublicKey = S::PublicKey;
    type Checked<'a>
        = MutedChecked<S::Checked<'a>>
    where
        Self: 'a;

    fn check(&mut self, recipients: Recipients<Self::PublicKey>) -> Result<Self::Checked<'_>, SystemTime> {
        let muted = self.mute.as_ref().is_some_and(|m| m.load(Relaxed));
        self.inner.check(recipients).map(|inner| MutedChecked { inner, muted })
    }
}

struct MutedChecked<C> {
    inner: C,
    muted: bool,
}

impl<C: CheckedSender> CheckedSender for MutedChecked<C> {
    type PublicKey = C::PublicKey;

    fn recipients(&self) -> Vec<Self::PublicKey> {
        self.inner.recipients()
    }

    fn send(self, message: impl Into<IoBufs> + Send, priority: bool) -> Unreliable<Feedback> {
        if self.muted {
            return Unreliable::new(Feedback::Ok);
        }
        self.inner.send(message, priority)
    }
}

/// [`super::start`], with the vote and certificate channels' senders behind `mute`.
async fn start_muteable(context: &deterministic::Context, oracle: &mut Oracle<PublicKey, deterministic::Context>, genesis: &Genesis, idx: usize, home: PathBuf, key: PrivateKey, mute: Arc<AtomicBool>) -> Val {
    let chain: Shared = Arc::new(Mutex::new(Chain::open(&home).unwrap()));
    let control = oracle.control(key.public_key());
    let mut channels = vec![];
    for c in CHANNELS {
        let (sender, receiver) = control.register(c, consensus::quota(c)).await.unwrap();
        channels.push((Muted { inner: sender, mute: (c == CH_VOTES || c == CH_CERTS).then(|| mute.clone()) }, receiver));
    }
    let cfg = ValidatorConfig { chain: chain.clone(), key: key.clone(), genesis: consensus::genesis_block(genesis), block_time_ms: genesis.block_time_ms, epoch_blocks: genesis.epoch_blocks, partition: format!("v{idx}") };
    let ctx = context.child("validator").with_attribute("idx", idx).with_attribute("run", 0);
    let (handle, _marshal) = consensus::start_validator(ctx, cfg, oracle.manager(), control, channels).await;
    Val { home, key, chain, handle: Some(handle) }
}

/// The 4 genesis validators, each with a mute switch on its votes.
async fn start_all(context: &deterministic::Context, oracle: &mut Oracle<PublicKey, deterministic::Context>, genesis: &Genesis, homes: &[PathBuf], seeds: &[[u8; 32]]) -> (Vec<Val>, Vec<Arc<AtomicBool>>) {
    let mutes: Vec<Arc<AtomicBool>> = (0..homes.len()).map(|_| Arc::new(AtomicBool::new(false))).collect();
    let mut vals = vec![];
    for (i, (home, seed)) in homes.iter().zip(seeds).enumerate() {
        vals.push(start_muteable(context, oracle, genesis, i, home.clone(), PrivateKey::from_seed_bytes(*seed), mutes[i].clone()).await);
    }
    (vals, mutes)
}

fn height(v: &Val) -> u64 {
    v.chain.lock().height()
}

fn max_height(vals: &[Val]) -> u64 {
    vals.iter().map(height).max().unwrap()
}

/// Safety: at every height two validators both executed, they finalized the same block and
/// reached the same state root. Returns the lowest height they all reached.
fn assert_agree(vals: &[Val]) -> u64 {
    let h = vals.iter().map(height).min().unwrap();
    for at in 1..=h {
        let seen: Vec<(Option<String>, Option<String>)> = vals.iter().map(|v| {
            let c = v.chain.lock();
            (c.hash_at(at), c.root_at(at))
        }).collect();
        assert!(seen.windows(2).all(|w| w[0] == w[1]), "validators finalized different blocks at height {at}: {seen:?}");
    }
    h
}

/// No validator gets past `stalled` for `secs` seconds (no quorum, no finalization).
async fn assert_stalled(context: &deterministic::Context, vals: &[Val], stalled: u64, secs: u64, why: &str) {
    for _ in 0..secs {
        context.sleep(Duration::from_secs(1)).await;
        assert!(max_height(vals) <= stalled, "{why}: a block was finalized without a quorum ({} > {stalled})", max_height(vals));
    }
}

fn link() -> Link {
    Link { latency: Duration::from_millis(40), jitter: Duration::from_millis(5), success_rate: probability!(1.0) }
}

/// Cuts (or, with `heal`, restores) every link between the two groups, both ways.
async fn partition(oracle: &Oracle<PublicKey, deterministic::Context>, vals: &[Val], a: &[usize], b: &[usize], heal: bool) {
    for &i in a {
        for &j in b {
            let (x, y) = (vals[i].key.public_key(), vals[j].key.public_key());
            for (from, to) in [(x.clone(), y.clone()), (y, x)] {
                if heal {
                    oracle.add_link(from, to, link()).await.unwrap();
                } else {
                    oracle.remove_link(from, to).await.unwrap();
                }
            }
        }
    }
}

/// Shifts validator `v`'s clock by `skew_ms` (0: back in sync).
fn set_skew(v: &Val, skew_ms: i64) {
    let key = Arc::as_ptr(&v.chain) as usize;
    let mut s = consensus::CLOCK_SKEW_MS.lock();
    s.retain(|(k, _)| *k != key);
    if skew_ms != 0 {
        s.push((key, skew_ms));
    }
}

/// Runs `blocks` blocks (as the validators other than `skewed` see them) with `skewed`'s clock
/// off by `skew_ms`, after letting what was in flight settle; returns the heights covered.
async fn skew_phase(context: &deterministic::Context, vals: &[Val], skewed: usize, skew_ms: i64, blocks: u64) -> (u64, u64) {
    set_skew(&vals[skewed], skew_ms);
    context.sleep(Duration::from_secs(3)).await;
    let from = max_height(vals) + 1;
    let others: Vec<usize> = (0..vals.len()).filter(|i| *i != skewed).collect();
    for _ in 0..2_000 {
        if others.iter().all(|i| height(&vals[*i]) >= from + blocks) {
            return (from, from + blocks);
        }
        context.sleep(Duration::from_millis(250)).await;
    }
    panic!("no progress with validator {skewed}'s clock off by {skew_ms} ms");
}

/// Blocks proposed by `v` among those finalized in `range`.
fn proposed_by(vals: &[Val], v: usize, range: std::ops::RangeInclusive<u64>) -> usize {
    let me = vals[v].key.public_key().address("boo");
    let c = vals[0].chain.lock();
    range.filter_map(|h| c.block(h)).filter(|b| b.proposer == me).count()
}

/// A 2–2 split has a quorum on neither side: nothing is finalized until it heals, then the chain
/// resumes. A 1–3 split keeps the majority finalizing; the isolated validator catches up once
/// reconnected. All four agree on every block throughout.
#[test]
fn partitions_heal_without_divergent_finalization() {
    let (root, homes, genesis, seeds) = summon("partition");
    let executor = deterministic::Runner::timed(Duration::from_secs(1800));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let (vals, _) = start_all(&context, &mut oracle, &genesis, &homes, &seeds).await;
        wait_for_height(&context, &vals, 5).await;

        partition(&oracle, &vals, &[0, 1], &[2, 3], false).await;
        // What was already voted on may still finalize; then nothing does.
        context.sleep(Duration::from_secs(5)).await;
        let stalled = max_height(&vals);
        assert_stalled(&context, &vals, stalled, 30, "2–2 partition").await;
        assert_agree(&vals);
        partition(&oracle, &vals, &[0, 1], &[2, 3], true).await;
        wait_for_height(&context, &vals, stalled + 5).await;
        let healed = assert_agree(&vals);
        println!("✓ 2–2 partition: stalled at {stalled} for 30 s, resumed and agree through {healed}");

        partition(&oracle, &vals, &[0], &[1, 2, 3], false).await;
        context.sleep(Duration::from_secs(2)).await;
        let isolated = height(&vals[0]);
        let target = max_height(&vals) + 10;
        wait_for_height(&context, &vals[1..], target).await;
        assert!(height(&vals[0]) <= isolated + 1, "the isolated validator finalized alone ({} > {isolated})", height(&vals[0]));
        assert_agree(&vals[1..]);
        partition(&oracle, &vals, &[0], &[1, 2, 3], true).await;
        wait_for_height(&context, &vals, target + 3).await;
        let h = assert_agree(&vals);
        println!("✓ 1–3 partition: the majority went on to {target}, the isolated validator caught up; all agree through {h}");
    });
    std::fs::remove_dir_all(root).ok();
}

/// One validator's clock runs ahead within the drift bound: its blocks are accepted. Ahead past
/// the bound: the others refuse its proposals (its views are skipped) and the chain goes on.
/// Behind past the bound: it refuses everyone else's blocks (so it doesn't vote), the other
/// three still finalize, and it keeps executing what they finalize. Back in sync, it proposes
/// again. All agree throughout.
#[test]
fn clock_skew_within_and_beyond_the_drift_bound() {
    let (root, homes, genesis, seeds) = summon("skew");
    let executor = deterministic::Runner::timed(Duration::from_secs(1800));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let (vals, _) = start_all(&context, &mut oracle, &genesis, &homes, &seeds).await;
        let skewed = 3;
        wait_for_height(&context, &vals, 3).await;
        let drift = MAX_CLOCK_DRIFT_MS as i64;

        let (from, to) = skew_phase(&context, &vals, skewed, drift * 6 / 10, 16).await;
        let ahead_within = proposed_by(&vals, skewed, from..=to);
        assert!(ahead_within > 0, "a validator {} ms ahead (within the {drift} ms bound) still gets its blocks finalized", drift * 6 / 10);
        assert_agree(&vals);

        let (from, to) = skew_phase(&context, &vals, skewed, drift * 3, 16).await;
        assert_eq!(proposed_by(&vals, skewed, from..=to), 0, "blocks stamped {} ms in the future are refused", drift * 3);
        assert_agree(&vals);

        let (from, to) = skew_phase(&context, &vals, skewed, -drift * 3, 16).await;
        assert_eq!(proposed_by(&vals, skewed, from..=to), 0);
        assert!(height(&vals[skewed]) + 2 >= to, "the validator behind still executes what the others finalize");
        assert_agree(&vals);

        let (from, to) = skew_phase(&context, &vals, skewed, 0, 16).await;
        assert!(proposed_by(&vals, skewed, from..=to) > 0, "back in sync, it proposes again");
        let h = assert_agree(&vals);
        set_skew(&vals[skewed], 0);
        println!("✓ clock skew: +{} ms accepted ({ahead_within} blocks), ±{} ms tolerated by the others; all agree through {h}", drift * 6 / 10, drift * 3);
    });
    std::fs::remove_dir_all(root).ok();
}

/// A validator that withholds its votes (online, silent on the vote and certificate channels):
/// the other three are a quorum, so the chain goes on, and its signature is in no certificate.
/// With a second one silent, only half the set votes: nothing is finalized until it speaks
/// again. Never a fork.
#[test]
fn withheld_votes_stall_only_without_a_quorum() {
    let (root, homes, genesis, seeds) = summon("withhold");
    let executor = deterministic::Runner::timed(Duration::from_secs(1800));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let (vals, mutes) = start_all(&context, &mut oracle, &genesis, &homes, &seeds).await;
        wait_for_height(&context, &vals, 3).await;

        mutes[3].store(true, Relaxed);
        context.sleep(Duration::from_secs(3)).await;
        let from = max_height(&vals) + 1;
        wait_for_height(&context, &vals, from + 20).await;
        assert_agree(&vals);
        let silent = vals[3].key.public_key();
        {
            let c = vals[0].chain.lock();
            let mut signed = 0;
            let mut certs = 0;
            for b in (from..=c.height()).filter_map(|h| c.block(h)).filter(|b| c.cert_len(b.height) > 0) {
                certs += 1;
                count_signed(&c, c.state.params.epoch_of(b.height), &silent, &b, &mut signed);
            }
            assert!(certs > 0, "certificates were kept for the range");
            assert_eq!(signed, 0, "the silent validator's signature is in no finalization certificate");
        }
        assert_eq!(proposed_by(&vals, 3, from..=from + 20), 0, "its proposals (sent with its vote) reach no one");
        println!("✓ one validator withholding votes: 20 blocks finalized without it");

        mutes[2].store(true, Relaxed);
        context.sleep(Duration::from_secs(5)).await;
        let stalled = max_height(&vals);
        assert_stalled(&context, &vals, stalled, 30, "2 of 4 withholding").await;
        assert_agree(&vals);
        mutes[2].store(false, Relaxed);
        wait_for_height(&context, &vals, stalled + 5).await;
        mutes[3].store(false, Relaxed);
        let back = max_height(&vals);
        wait_for_height(&context, &vals, back + 12).await;
        assert!(proposed_by(&vals, 3, back + 1..=back + 12) > 0, "voting again, it proposes again");
        let h = assert_agree(&vals);
        println!("✓ two withholding: stalled at {stalled}, resumed once one spoke again; all agree through {h}");
    });
    std::fs::remove_dir_all(root).ok();
}
