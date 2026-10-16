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
    // Honest validators run with slashing protection, as in `super::start`.
    let protection = Protection::open(&home.join(crate::protection::RECORD_FILE), key.public_key().as_ref(), Timing::for_block_time(genesis.block_time_ms), false).unwrap();
    let cfg = ValidatorConfig { chain: chain.clone(), key: key.clone(), genesis: consensus::genesis_block(genesis), block_time_ms: genesis.block_time_ms, epoch_blocks: genesis.epoch_blocks, partition: format!("v{idx}"), protection: Some(protection.clone()) };
    let ctx = context.child("validator").with_attribute("idx", idx).with_attribute("run", 0);
    serve_http_view(&ctx, genesis, idx, 0, &chain, &protection);
    let (handle, _marshal) = consensus::start_validator(ctx, cfg, oracle.manager(), control, channels).await;
    Val { home, key, chain, handle: Some(handle), protection }
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

/// SEC-3: a leader that cites a wrong `exec_root` (lying about the state it executed, or having
/// diverged from it) gets every proposal rejected, since honest verifiers check the cited root
/// once they've executed that height. None of its blocks finalize while it does, the chain stays
/// live (its views are nullified and skipped), its leader slots count as missed and, with a
/// short downtime window, it's jailed and leaves the set. Every node agrees throughout.
#[test]
fn a_leader_citing_a_wrong_exec_root_is_rejected() {
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon_with("badroot", EPOCH_BLOCKS, &[("downtime_window", 8), ("downtime_max_missed", 3), ("jail_blocks", 15)]);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        wait_for_height(&context, &vals, 3).await;
        let liar = Keys::from_seed(seeds[2]).address();
        vals[2].chain.lock().wrong_exec_root = true;
        // Whatever it proposed before the switch may still finalize.
        context.sleep(Duration::from_secs(3)).await;
        let from = max_height(&vals) + 1;
        let jailed = |vals: &[Val]| vals.iter().all(|v| v.chain.lock().state.staking.pools[&liar].jail.is_some());
        wait_until(&context, "the lying leader's jailing", || jailed(&vals)).await;
        wait_until(&context, "its exit from the set", || vals.iter().all(|v| v.chain.lock().state.validators[&liar].status != ValidatorStatus::Active)).await;
        let to = max_height(&vals);
        wait_for_height(&context, &vals, to).await;
        assert!(to >= from + EPOCH_BLOCKS, "the chain stayed live ({from}..={to})");
        assert_eq!(proposed_by(&vals, 2, from..=to), 0, "none of its blocks with a wrong exec_root was finalized");
        {
            let c = vals[0].chain.lock();
            let l = &c.state.staking.liveness[&liar];
            assert!(l.missed_total > 0 && c.state.staking.pools[&liar].jail.as_ref().unwrap().reason.contains("leader slots"), "its rejected proposals are missed slots: {l:?}");
            // Its stake isn't slashed for lying (no evidence of it), only for the missed slots.
        }
        let h = assert_agree(&vals);
        println!("✓ a leader citing a wrong exec_root: 0 of its blocks finalized in {from}..={to}, jailed for the missed slots; 4 nodes agree through {h}");
    });
    std::fs::remove_dir_all(root).ok();
}

/// A tx from `from` with an explicit fee cap and tip (signed).
fn paying(g: &Genesis, from: &Keys, nonce: u64, action: Action, fee: u64, tip: u64) -> Tx {
    let (chain_id, genesis_hash) = ident(g);
    let tx = Tx { chain_id, genesis_hash, from: from.address(), nonce, fee, tip, valid_until: VALID_UNTIL, action, memo: "load".into(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    tx.sign(from)
}

/// TOK-2's spam load test on the real consensus path. Blocks hold ~12 transfers
/// (`max_block_bytes` = 64 KiB); three spammers queue 150 transfers at the floor's tip (0), far
/// more than the next dozen blocks hold, and junk that doesn't pay the base fee isn't admitted at
/// all. Honest users then send transfers with a modest tip, priced the way wallets price them
/// (`econ::max_fee`, what `/api/fees` suggests, plus the tip): each confirms within
/// `CONFIRM_BLOCKS` blocks of reaching the mempool, ahead of the backlog, and is charged its
/// block's base fee plus its tip, never more than it offered. Full blocks raise the base fee.
#[test]
fn spam_doesnt_delay_honest_txs_with_a_competitive_tip() {
    const CONFIRM_BLOCKS: u64 = 3;
    const SPAM_PER_SENDER: u64 = 50;
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon_with("spam", EPOCH_BLOCKS, &[("max_block_bytes", MAX_TX_BYTES as u64)]);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
    executor.start(|context| async move {
        let mut oracle = network(&context, &genesis, &[]).await;
        let mut vals = vec![];
        for (i, (home, seed)) in homes.iter().zip(&seeds).enumerate() {
            vals.push(start(&context, &mut oracle, &genesis, i, 0, home.clone(), PrivateKey::from_seed_bytes(*seed)).await);
        }
        wait_for_height(&context, &vals, 3).await;
        // Honest users, funded by validator 3.
        let funder = Keys::from_seed(seeds[3]);
        let users: Vec<Keys> = (0..3u8).map(|i| Keys::from_seed([90 + i; 32])).collect();
        for (n, u) in users.iter().enumerate() {
            vals[3].chain.lock().submit(transfer(&genesis, &funder, n as u64, &u.address(), 100 * DROPS_PER_FANG)).unwrap();
        }
        wait_until(&context, "the users' funding", || users.iter().all(|u| vals.iter().all(|v| v.chain.lock().state.account(&u.address()).balance > 0))).await;

        // Junk that doesn't pay the base fee never enters a mempool.
        let spammers: Vec<Keys> = seeds[..3].iter().map(|s| Keys::from_seed(*s)).collect();
        let sink = funder.address();
        let floor = |size: u64| min_fee(size);
        let cheap = paying(&genesis, &spammers[0], vals[0].chain.lock().next_nonce(&spammers[0].address()), Action::Transfer { to: sink.clone(), amount: 1 }, 1, 0);
        assert!(vals[0].chain.lock().submit(cheap).unwrap_err().to_string().contains("fee"), "a tx under the base fee is refused");
        // The spam: valid, paying up to 4× the floor if the base fee rises, no tip.
        let spam_from = max_height(&vals);
        let mut spam_ids = vec![];
        for (i, s) in spammers.iter().enumerate() {
            let mut c = vals[i].chain.lock();
            let first = c.next_nonce(&s.address());
            for n in first..first + SPAM_PER_SENDER {
                let probe = paying(&genesis, s, n, Action::Transfer { to: sink.clone(), amount: 1 }, 0, 0);
                let tx = paying(&genesis, s, n, Action::Transfer { to: sink.clone(), amount: 1 }, 4 * floor(probe.size()), 0);
                spam_ids.push(c.submit(tx).unwrap());
            }
        }
        let backlog_blocks = spam_ids.len() as u64 / 12;
        println!("  {} spam txs queued at #{spam_from} (~{backlog_blocks} full blocks of backlog)", spam_ids.len());
        wait_for_height(&context, &vals, spam_from + 2).await;

        // Honest txs, one at a time while the backlog drains.
        let mut report = vec![];
        for (k, u) in users.iter().enumerate() {
            let (sent_at, tx) = {
                let c = vals[k].chain.lock();
                let tip = 1_000;
                let probe = paying(&genesis, u, 0, Action::Transfer { to: sink.clone(), amount: DROPS_PER_FANG }, 0, tip);
                let fee = crate::econ::max_fee(&c.state, probe.size()) + tip;
                (c.height(), paying(&genesis, u, 0, Action::Transfer { to: sink.clone(), amount: DROPS_PER_FANG }, fee, tip))
            };
            let id = vals[k].chain.lock().submit(tx.clone()).unwrap();
            wait_until(&context, "the honest tx", || vals.iter().all(|v| v.chain.lock().has_tx(&id))).await;
            let (at, pending_spam, base) = {
                let c = vals[0].chain.lock();
                let (at, _) = c.tx_location(&id).unwrap();
                (at, spam_ids.iter().filter(|s| !c.has_tx(s)).count(), c.block(at).unwrap().base_fee.expect("executed blocks record their base fee").fee(tx.size()))
            };
            let charged = base + tx.tip;
            assert!(at - sent_at <= CONFIRM_BLOCKS, "honest tx {k} sent at #{sent_at} confirmed at #{at}: more than {CONFIRM_BLOCKS} blocks");
            assert!(charged <= tx.fee, "charged {charged}, more than the {} it offered", tx.fee);
            assert!(pending_spam > 0, "the spam backlog was still pending when honest tx {k} confirmed");
            report.push(format!("tx {k}: sent #{sent_at}, in #{at} ({} blocks), charged {charged} drops (offered {}, floor {}), {pending_spam} spam txs still waiting", at - sent_at, tx.fee, floor(tx.size()) + tx.tip));
            let h = max_height(&vals);
            wait_for_height(&context, &vals, h + 2).await;
        }
        // The backlog drains, and full blocks raised the base fee (of the blocks 17 after them:
        // the slots their execution fixed).
        wait_until(&context, "the spam to drain", || spam_ids.iter().all(|s| vals[0].chain.lock().has_tx(s))).await;
        let c = vals[0].chain.lock();
        let floor_rate = crate::econ::FeeSlot::floor(&c.state.chain_params).rate;
        let charged = (spam_from..=c.height()).filter_map(|h| c.block(h)?.base_fee).map(|s| s.rate);
        let peak = charged.chain(c.state.econ.fees.iter().map(|s| s.rate)).max().unwrap();
        assert!(peak > floor_rate, "full blocks raise the base fee ({peak} vs the floor's {floor_rate})");
        let full = (spam_from + 1..spam_from + backlog_blocks).filter_map(|h| c.block(h)).filter(|b| b.txs.iter().map(|t| t.size()).sum::<u64>() > MAX_TX_BYTES as u64 / 2).count();
        assert!(full as u64 >= backlog_blocks / 2, "the spam filled blocks ({full})");
        drop(c);
        for r in &report {
            println!("  {r}");
        }
        let h = assert_agree(&vals);
        println!("✓ {} spam txs, 3 honest txs each confirmed within {CONFIRM_BLOCKS} blocks at their offered fee; base rate peaked at {peak} milli-drops/byte (floor {floor_rate}); 4 nodes agree through {h}", spam_ids.len());
    });
    std::fs::remove_dir_all(root).ok();
}

/// Audit F32/F56 on the real consensus path. Validator duties carry no tip, and proposals used to
/// take them after every tipped tx, so a backlog of tipping txs through the reveal half of an
/// epoch made every validator miss its beacon reveal (jailed and slashed). Here eight accounts
/// queue 512 transfers with a tip, ~40 full blocks of 64 KiB (four epochs), while all four
/// validators run their beacon duties: the duties take the mempool's lane on every node (gossip
/// included) and lead each proposal, so every epoch's beacon gets all four reveals while the
/// backlog drains behind them, and nobody is jailed.
#[test]
fn validator_duties_lead_a_tipped_backlog() {
    const SPAMMERS: u8 = 8;
    const TIP: u64 = 10;
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_env("BLOCKULA_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off"))).with_test_writer().try_init();
    let (root, homes, genesis, seeds) = summon_with("duties", EPOCH_BLOCKS, &[("max_block_bytes", MAX_TX_BYTES as u64)]);
    let executor = deterministic::Runner::timed(Duration::from_secs(3_000));
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
        wait_for_height(&context, &vals, 3).await;
        // The spammers, funded by validator 3.
        let funder = Keys::from_seed(seeds[3]);
        let spammers: Vec<Keys> = (0..SPAMMERS).map(|i| Keys::from_seed([70 + i; 32])).collect();
        let addrs: std::collections::HashSet<String> = spammers.iter().map(|s| s.address()).collect();
        {
            let mut c = vals[3].chain.lock();
            for s in &spammers {
                let n = c.next_nonce(&funder.address());
                c.submit(transfer(&genesis, &funder, n, &s.address(), 10 * DROPS_PER_FANG)).unwrap();
            }
        }
        wait_until(&context, "the spammers' funding", || spammers.iter().all(|s| vals.iter().all(|v| v.chain.lock().state.account(&s.address()).balance > 0))).await;

        // Each queues as many transfers as a sender may, tipping, with room for the base fee to rise.
        let spam_from = max_height(&vals);
        let mut spam_ids = vec![];
        for (i, s) in spammers.iter().enumerate() {
            let mut c = vals[i % vals.len()].chain.lock();
            for n in 0..crate::chain::MAX_PENDING_PER_SENDER as u64 {
                let pay = |fee: u64| paying(&genesis, s, n, Action::Transfer { to: funder.address(), amount: 1 }, fee, TIP);
                spam_ids.push(c.submit(pay(200 * min_fee(pay(0).size()) + TIP)).unwrap());
            }
        }
        println!("  {} tipping txs queued at #{spam_from} (~{} full blocks of backlog)", spam_ids.len(), spam_ids.len() / 12);
        wait_until(&context, "the backlog to drain", || !vals[0].chain.lock().mempool.iter().any(|t| addrs.contains(&t.from))).await;
        let drained = max_height(&vals);
        wait_for_height(&context, &vals, drained + EPOCH_BLOCKS).await;

        let c = vals[0].chain.lock();
        assert!(spam_ids.iter().all(|id| c.has_tx(id)), "the backlog drained by inclusion");
        // Each epoch whose reveal half (the second half of the epoch before it) fell inside the
        // backlog got every reveal, in full blocks.
        let reveal_half = |e: u64| ((e - 1) * EPOCH_BLOCKS + EPOCH_BLOCKS / 2)..=(e * EPOCH_BLOCKS);
        let congested: Vec<u64> = (2..).take_while(|e| e * EPOCH_BLOCKS <= drained).filter(|e| *reveal_half(*e).start() > spam_from + 1).collect();
        assert!(congested.len() >= 2, "the backlog spanned the reveals of epochs {congested:?}");
        for &e in &congested {
            let v = &c.state.beacon.values[&e];
            assert_eq!((v.reveals, v.missed.len()), (4, 0), "epoch {e}'s beacon, revealed under the backlog");
            let full = reveal_half(e).filter(|h| c.block(*h).is_some_and(|b| b.txs.iter().map(|t| t.size()).sum::<u64>() > MAX_TX_BYTES as u64 / 2)).count();
            assert!(full * 2 > EPOCH_BLOCKS as usize / 2, "epoch {e}'s reveal half was congested ({full} full blocks)");
        }
        let is_duty = |t: &Tx| matches!(t.action, Action::BeaconCommit { .. } | Action::BeaconReveal { .. });
        let mut led = 0;
        for h in spam_from + 1..=drained {
            let b = c.block(h).unwrap();
            let (last_duty, first_spam) = (b.txs.iter().rposition(is_duty), b.txs.iter().position(|t| addrs.contains(&t.from)));
            if let (Some(d), Some(s)) = (last_duty, first_spam) {
                assert!(d < s, "block {h}: the duties lead the tipped txs");
                led += 1;
            }
        }
        for seed in &seeds {
            let me = Keys::from_seed(*seed).address();
            assert!(c.state.staking.pools[&me].jail.is_none(), "{me} was never jailed");
        }
        drop(c);
        let h = assert_agree(&vals);
        println!("✓ {} tipping txs over #{spam_from}..=#{drained}: duties led {led} blocks, epochs {congested:?} got all 4 reveals, nobody jailed; 4 nodes agree through {h}", spam_ids.len());
    });
    std::fs::remove_dir_all(root).ok();
}
