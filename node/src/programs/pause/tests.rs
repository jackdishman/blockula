//! The pause (docs/PROGRAMS-V0.md §7.2, §10.1's training wheels). First its rules on their own
//! (WBS S5's modules): the threshold at n = 1, 4 and 7, votes across epoch boundaries and their life,
//! a lone signature spending no window, the window and each of `fix_pending`'s conditions, renewal,
//! the lapse at exactly `until`, the 30-day cap and the 60-day spacing between runs, a pause in
//! exit-only, and the two signing forms counting once: each signature runs [`sign`], tag 69's
//! execution, on a chain born at v2 with programs on, and programs' invariants are checked after
//! every one. Then the same rules through tag 69's hooks (WBS S5's): signed transactions through
//! `State::apply` (admission, the fee phase, the dispatch), where every refusal is unadmittable and
//! charges nothing; tags 62–64 refused while a pause runs and admitted from `until`; the duty lane
//! (`staking::duty_of`) and the owner key's tip; and a block produced on a dev chain.

use super::*;
use crate::chain::{Allocation, Genesis, Outcome, ValidatorInfo};
use crate::crypto::Keys;
use crate::governance::Proposal;
use crate::types::{key_id, Action, Tx, DECIMALS, DROPS_PER_FANG};
use crate::upgrade::{Feature, ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use std::collections::BTreeMap;

const FANG: u64 = DROPS_PER_FANG;
const CHAIN: &str = "programs-pause-1";
/// A day of mainnet's 1 s blocks: every span is a whole number of them.
const DAY: u64 = 86_400;
/// Longer than every height these tests sign at, so one active set (epoch 0's) covers them all,
/// except where a test crosses epochs on purpose.
const ONE_EPOCH: u64 = 100 * DAY;
const ALICE: u8 = 1;

fn keys(byte: u8) -> Keys {
    Keys::from_seed([byte; 32])
}

fn alice() -> String {
    keys(ALICE).address()
}

/// Validator `i`'s key_seed.
fn key_seed(i: usize) -> u8 {
    31 + i as u8
}

fn v(i: usize) -> String {
    keys(key_seed(i)).address()
}

/// Validator `i`'s consensus key, the key of its own account (the duty form of tag 69).
fn consensus_key(i: usize) -> String {
    key_id(&keys(key_seed(i)).public_key())
}

/// Validators `is`, as a pause lists its signers: sorted by address.
fn sorted(is: impl IntoIterator<Item = usize>) -> Vec<String> {
    let mut out: Vec<String> = is.into_iter().map(v).collect();
    out.sort();
    out
}

/// A chain born at v2 with programs on from genesis (so META `programs` exists from block 0), `n`
/// validators (all in epoch 0's active set), a funded user, mainnet's 1 s blocks.
fn chain_with(n: usize, epoch_blocks: u64) -> State {
    let alloc = |address: String| Allocation { address, amount: 1_000 * FANG, vesting: None, label: None };
    let g = Genesis {
        chain_id: CHAIN.into(),
        token: "FANG".into(),
        decimals: DECIMALS,
        block_time_ms: 1_000,
        validators: (0..n).map(|i| ValidatorInfo { address: v(i), public_key: hex::encode(keys(key_seed(i)).public_key()), p2p_addr: format!("127.0.0.1:{}", 7101 + i), http_addr: format!("127.0.0.1:{}", 7201 + i) }).collect(),
        faucet: false,
        created_at_ms: 1_750_000_000_000,
        allocations: std::iter::once(alice()).chain((0..n).map(v)).map(alloc).collect(),
        epoch_blocks,
        validator_admins: vec![],
        contribution: None,
        address_prefix: "boo".into(),
        param_overrides: Default::default(),
        operators: vec![],
        reserved_names: vec![],
        soul_ledger: vec![],
        features: vec![PROGRAMS_FEATURE.into()],
        upgrades: vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }],
        bootstrap_excluded: vec![],
        pool_repos: Default::default(),
    };
    g.check().unwrap();
    let st = g.initial_state();
    assert_eq!(status(&st, 1), Status::On);
    st
}

fn chain(n: usize) -> State {
    chain_with(n, ONE_EPOCH)
}

fn meta(st: &State) -> &super::super::state::ProgramsMeta {
    st.programs.meta.as_ref().expect("programs are on")
}

/// A `ProgramsPause` from `sender` signed with `signer`, executed in the block at `height` (as
/// `State::apply_j` runs an action: journaled, and rolled back on `Err`); then programs'
/// invariants, which the records it writes must keep.
fn sign_as(st: &mut State, sender: &str, signer: &str, height: u64) -> Result<()> {
    // The block at `height` is running: its transactions see `econ::now` = `height`.
    st.econ.height = height - 1;
    let mut j = Journal::default();
    let r = sign(st, &mut j, sender, signer, height);
    if r.is_err() {
        st.rollback(j);
    }
    super::super::check_invariants(st).expect("programs' invariants hold after a signature");
    r
}

/// Validator `i` signs at `height` from its own account with its consensus key.
fn sign_at(st: &mut State, i: usize, height: u64) -> Result<()> {
    sign_as(st, &v(i), &consensus_key(i), height)
}

/// Each of `is` signs at `height`; panics on a refusal.
fn all_sign(st: &mut State, is: impl IntoIterator<Item = usize>, height: u64) {
    for i in is {
        sign_at(st, i, height).unwrap_or_else(|e| panic!("validator {i} at block {height}: {e}"));
    }
}

fn refusal(st: &mut State, i: usize, height: u64) -> String {
    let before = st.programs.meta.clone();
    let e = sign_at(st, i, height).expect_err("refused").to_string();
    assert_eq!(st.programs.meta, before, "a refused signature changes nothing");
    assert_eq!(may_sign(st, &v(i), height).unwrap_err().to_string(), e, "admission refuses it in the same words");
    e
}

/// A governance proposal of `kind` with `status`, in the open list while it's voting or queued.
fn propose(st: &mut State, kind: ProposalKind, status: ProposalStatus) {
    let g = &mut st.governance;
    let id = g.next_proposal_id;
    g.next_proposal_id += 1;
    let p = Proposal { id, proposer: alice(), kind, title: "a fix".into(), url: String::new(), deposit: 0, created_at: 1, voting_end: 10, execute_at: None, status, aye_amount: 0, nay_amount: 0, aye_weight: 0, nay_weight: 0, voters: 0, council: BTreeMap::new(), tally: None, outcome: String::new() };
    g.proposals.insert(id, p);
    if matches!(status, ProposalStatus::Voting | ProposalStatus::Queued) {
        g.open_proposals.insert(id);
    }
}

fn schedule_upgrade() -> ProposalKind {
    ProposalKind::ScheduleUpgrade { version: 3, activation_height: 200 * DAY }
}

/// Makes a fix pending.
type Fix = fn(&mut State);

/// Each way a fix is pending (`fix_pending`'s three conditions, a proposal's two statuses), and
/// what makes it so.
fn fixes() -> [(&'static str, Fix); 4] {
    [
        ("a ScheduleUpgrade voting", |st| propose(st, schedule_upgrade(), ProposalStatus::Voting)),
        ("a ScheduleUpgrade queued", |st| propose(st, schedule_upgrade(), ProposalStatus::Queued)),
        ("a DeactivateFeature { programs } voting", |st| propose(st, ProposalKind::DeactivateFeature { feature: PROGRAMS_FEATURE.into() }, ProposalStatus::Voting)),
        ("an upgrade scheduled above the height", |st| st.upgrade.schedule.push(ScheduledUpgrade { version: 3, activation_height: 200 * DAY })),
    ]
}

/// The spans are days at the chain's block time: at mainnet's 1 s, a vote lives 86,400 blocks, a
/// pause lasts 259,200, the window and the cap are 2,592,000 each (§7.2's numbers), and a new run
/// waits twice the cap; 2 s blocks halve them.
#[test]
fn the_spans_are_days_at_the_chains_block_time() {
    let at = |ms: u64| Spans::of(&Params { block_time_ms: ms, ..Params::default() });
    assert_eq!(at(1_000), Spans { vote_blocks: 86_400, pause_blocks: 259_200, sign_window: 2_592_000, max_span: 2_592_000 });
    assert_eq!(at(2_000), Spans { vote_blocks: 43_200, pause_blocks: 129_600, sign_window: 1_296_000, max_span: 1_296_000 });
    let run = Pause { from: 101, until: 101 + 3 * DAY, started: 101, signers: vec![] };
    assert_eq!((at(1_000).cap(&run), at(1_000).next_run(&run)), (101 + 30 * DAY, 101 + 60 * DAY));
    assert_eq!((VOTE_DAYS, PAUSE_DAYS, SIGN_WINDOW_DAYS, MAX_SPAN_DAYS), (1, 3, 30, 30));
}

/// f + 1 of the active set pause, f = (n - 1) / 3: 1 of 1 to 3, 2 of 4 to 6, 3 of 7
/// (`ghost::pause_threshold`; the sizes between spec §10.1's 1, 4 and 7 tell f + 1 from n / 3 + 1).
/// Below it only the votes grow; the signature that reaches it starts a run from the next block,
/// for three days, signed by the counted voters, each of whose `pause_signed` becomes that block,
/// and the votes clear. The pause holds from its `from`, not in the block that reached it.
#[test]
fn the_threshold_is_f_plus_1_at_n_1_to_7() {
    for (n, needed) in [(1, 1), (2, 1), (3, 1), (4, 2), (5, 2), (6, 2), (7, 3)] {
        assert_eq!(crate::ghost::pause_threshold(n), needed);
        let mut st = chain(n);
        let h = 100;
        for i in 0..needed - 1 {
            sign_at(&mut st, i, h).unwrap();
            let m = meta(&st);
            assert!(m.pause.is_none() && m.pause_signed.is_empty(), "n = {n}: {} of {needed} signatures", i + 1);
            assert_eq!(m.votes, (0..=i).map(|i| (v(i), h)).collect::<BTreeMap<_, _>>(), "n = {n}");
        }
        sign_at(&mut st, needed - 1, h).unwrap();
        let m = meta(&st);
        assert_eq!(m.pause, Some(Pause { from: h + 1, until: h + 1 + 3 * DAY, started: h + 1, signers: sorted(0..needed) }), "n = {n}");
        assert!(m.votes.is_empty(), "n = {n}: the votes clear");
        assert_eq!(m.pause_signed, (0..needed).map(|i| (v(i), h)).collect::<BTreeMap<_, _>>(), "n = {n}: each counted voter spent its window");
        assert_eq!((status(&st, h), status(&st, h + 1)), (Status::On, Status::Paused { until: h + 1 + 3 * DAY }), "n = {n}");
    }
}

/// A vote lives a day, across epoch boundaries: two signatures in adjacent epochs pause (n = 4),
/// but not once the first is a day old, nor when its validator has left the active set (its vote
/// is dropped before counting, and it can't sign itself). The threshold is the set's at the
/// signature's height.
#[test]
fn votes_live_a_day_across_epoch_boundaries() {
    let epoch = 1_000;
    // Five validators, four in each epoch's set (so f + 1 = 2 throughout).
    let mut base = chain_with(5, epoch);
    let set: Vec<String> = (0..4).map(v).collect();
    for e in 0..=(2 * DAY / epoch) {
        base.active_sets.insert(e, set.clone());
    }
    let last_of_epoch_0 = epoch - 1;

    let mut st = base.clone();
    sign_at(&mut st, 0, last_of_epoch_0).unwrap();
    sign_at(&mut st, 1, epoch).unwrap();
    assert_eq!(meta(&st).pause.as_ref().map(|p| (p.from, p.signers.clone())), Some((epoch + 1, sorted([0, 1]))), "adjacent epochs");

    let mut st = base.clone();
    sign_at(&mut st, 0, last_of_epoch_0).unwrap();
    let mut later = st.clone();
    sign_at(&mut later, 1, last_of_epoch_0 + DAY - 1).unwrap();
    assert!(later.programs.meta.as_ref().unwrap().pause.is_some(), "the last block of the first vote's day still counts it");
    sign_at(&mut st, 1, last_of_epoch_0 + DAY).unwrap();
    let m = meta(&st);
    assert!(m.pause.is_none(), "a vote a day old doesn't count");
    assert_eq!(m.votes, BTreeMap::from([(v(1), last_of_epoch_0 + DAY)]), "and is dropped");

    // Validator 0 leaves the set at epoch 1 (validator 4 takes its seat): its vote is dropped
    // there, and it can't sign.
    let mut st = base.clone();
    st.active_sets.insert(1, (1..5).map(v).collect());
    sign_at(&mut st, 0, last_of_epoch_0).unwrap();
    sign_at(&mut st, 1, epoch).unwrap();
    let m = meta(&st);
    assert!(m.pause.is_none() && m.votes == BTreeMap::from([(v(1), epoch)]), "a vote of a validator that left the set doesn't count");
    assert_eq!(sign_at(&mut st, 0, epoch + 1).unwrap_err().to_string(), "only a validator in epoch 1's active set (its consensus key or its owner) can pause programs");

    // A larger set at epoch 1 needs more: two votes of 4, counted against 7.
    let mut st = chain_with(7, epoch);
    st.active_sets.insert(0, (0..4).map(v).collect());
    st.active_sets.insert(1, (0..7).map(v).collect());
    sign_at(&mut st, 0, last_of_epoch_0).unwrap();
    sign_at(&mut st, 1, epoch).unwrap();
    assert!(meta(&st).pause.is_none(), "2 of 7 don't pause");
    sign_at(&mut st, 4, epoch).unwrap();
    assert_eq!(meta(&st).pause.as_ref().map(|p| p.signers.clone()), Some(sorted([0, 1, 4])));
}

/// A signature that never reaches the threshold costs its fee and nothing else: no window is
/// spent, and once its vote lapses its validator may sign again. While it's live, it can't.
#[test]
fn a_lone_signature_spends_no_window() {
    let mut st = chain(4);
    sign_at(&mut st, 0, 100).unwrap();
    assert!(meta(&st).pause_signed.is_empty());
    assert_eq!(refusal(&mut st, 0, 101), format!("{} already signed a pause at block 100; its signature counts until block {}", short(&v(0)), 100 + DAY - 1));
    assert!(refusal(&mut st, 0, 100 + DAY - 1).contains("already signed"));
    sign_at(&mut st, 0, 100 + DAY).unwrap();
    let m = meta(&st);
    assert!(m.pause.is_none() && m.pause_signed.is_empty());
    assert_eq!(m.votes, BTreeMap::from([(v(0), 100 + DAY)]));
}

/// A validator that started (or renewed) a pause may sign again only 30 days later, unless a fix
/// is pending, under each of `fix_pending`'s three conditions; a validator that hasn't signed one
/// is free to. Within a run that's what the window decides: the cap ends every run before its
/// starters' window does, and the spacing starts the next only after every window of the last one
/// has passed, so the window's own boundary is checked on a hand-set `pause_signed`.
#[test]
fn the_window_holds_a_signer_back_unless_a_fix_is_pending() {
    let mut st = chain(7);
    all_sign(&mut st, 0..3, 100);
    let h = 100 + DAY;
    let window = format!("{} started or renewed a pause at block 100: it may sign again from block {}, or while a fix is pending (a ScheduleUpgrade or DeactivateFeature {{ programs }} proposal voting or queued, or an upgrade scheduled)", short(&v(0)), 100 + 30 * DAY);
    assert_eq!(refusal(&mut st, 0, h), window);
    may_sign(&st, &v(3), h).unwrap();

    for (what, fix) in fixes() {
        let mut st = st.clone();
        fix(&mut st);
        assert!(fix_pending(&st, h), "{what}");
        // Validators 0 (window waived) and 3 (no window) renew the running pause.
        all_sign(&mut st, [0, 3, 4], h);
        let m = meta(&st);
        assert_eq!(m.pause.as_ref().map(|p| (p.until, p.signers.clone())), Some((h + 1 + 3 * DAY, sorted([0, 3, 4]))), "{what}");
        assert_eq!((m.pause_signed[&v(0)], m.pause_signed[&v(1)]), (h, 100), "{what}");
    }

    // The window's boundary: 30 days after the signature that spent it.
    let mut st = chain(4);
    st.programs.meta.as_mut().unwrap().pause_signed.insert(v(0), 100);
    assert_eq!(may_sign(&st, &v(0), 100 + 30 * DAY - 1).unwrap_err().to_string(), window);
    may_sign(&st, &v(0), 100 + 30 * DAY).unwrap();
}

/// `fix_pending` is exactly its three conditions: a `ScheduleUpgrade` or `DeactivateFeature {
/// programs }` proposal that's voting or queued, or an upgrade scheduled above the height. A closed
/// proposal, another feature's switch, switching programs on, cancelling an upgrade, a parameter
/// change, and an upgrade already in force don't count.
#[test]
fn fix_pending_is_exactly_its_three_conditions() {
    let base = chain(4);
    let h = 100;
    assert!(!fix_pending(&base, h), "born at v2 at block 1: nothing scheduled above it");
    let with = |kind: ProposalKind, status: ProposalStatus| {
        let mut st = base.clone();
        propose(&mut st, kind, status);
        fix_pending(&st, h)
    };
    for status in [ProposalStatus::Voting, ProposalStatus::Queued] {
        assert!(with(schedule_upgrade(), status), "{status:?}");
        assert!(with(ProposalKind::DeactivateFeature { feature: PROGRAMS_FEATURE.into() }, status), "{status:?}");
        for kind in [
            ProposalKind::DeactivateFeature { feature: crate::ghost::FEATURE.into() },
            ProposalKind::ActivateFeature { feature: PROGRAMS_FEATURE.into() },
            ProposalKind::CancelUpgrade { version: 3, activation_height: 200 * DAY },
            ProposalKind::ParamChange { key: "program_value_cap".into(), value: 0 },
        ] {
            assert!(!with(kind.clone(), status), "{kind:?}");
        }
    }
    for status in [ProposalStatus::Rejected, ProposalStatus::Executed, ProposalStatus::Failed] {
        assert!(!with(schedule_upgrade(), status), "{status:?}");
        assert!(!with(ProposalKind::DeactivateFeature { feature: PROGRAMS_FEATURE.into() }, status), "{status:?}");
    }
    // Governance's open list holds only voting and queued proposals; one left there closed (a state
    // its invariants refuse) still doesn't count.
    let mut st = base.clone();
    propose(&mut st, schedule_upgrade(), ProposalStatus::Executed);
    let id = st.governance.next_proposal_id - 1;
    st.governance.open_proposals.insert(id);
    assert!(!fix_pending(&st, h), "a closed proposal in the open list");
    let scheduled = |at: u64| {
        let mut st = base.clone();
        st.upgrade.schedule.push(ScheduledUpgrade { version: 3, activation_height: at });
        fix_pending(&st, h)
    };
    assert!(scheduled(h + 1));
    assert!(!scheduled(h), "an upgrade in force at the height is no longer pending");
}

/// A renewal, by validators that haven't spent their window, moves `until` to three days after its
/// next block, never back; `from` and `started` stay, its signers become the new ones, and they
/// spend their window too.
#[test]
fn a_renewal_extends_until_and_keeps_the_run() {
    let mut st = chain(4);
    all_sign(&mut st, [0, 1], 100);
    let h = 101 + 2 * DAY;
    all_sign(&mut st, [2, 3], h);
    let m = meta(&st);
    assert_eq!(m.pause, Some(Pause { from: 101, until: h + 1 + 3 * DAY, started: 101, signers: sorted([2, 3]) }));
    assert_eq!(m.pause_signed, (0..4).map(|i| (v(i), if i < 2 { 100 } else { h })).collect::<BTreeMap<_, _>>());
    assert!(m.votes.is_empty());

    // Never back: a run whose `until` is already later keeps it.
    let mut st = chain(4);
    all_sign(&mut st, [0, 1], 100);
    let later = 101 + 3 * DAY + 50;
    st.programs.meta.as_mut().unwrap().pause.as_mut().unwrap().until = later;
    all_sign(&mut st, [2, 3], 101);
    assert_eq!(meta(&st).pause.as_ref().map(|p| p.until), Some(later));
}

/// The cap (the maintainer's decision of 2026-10-20): renewed every two days with a fix pending (so
/// the window never binds), a run's `until` is clipped at `started` + 30 days, and once it's there
/// every signature is refused, uncharged, fix or not. It lapses there, and a new run is refused
/// below `started` + 60 days and starts at exactly that height, with a new `started`.
#[test]
fn the_cap_ends_a_run_at_30_days_and_spaces_runs_60_days_apart() {
    let mut st = chain(4);
    propose(&mut st, schedule_upgrade(), ProposalStatus::Voting);
    all_sign(&mut st, [0, 1], 100);
    let (started, cap) = (101, 101 + 30 * DAY);
    let mut h = 100;
    loop {
        h += 2 * DAY;
        let until = meta(&st).pause.as_ref().unwrap().until;
        assert!(h < until && until < cap, "the run is running, under its cap, at block {h}");
        all_sign(&mut st, [0, 1], h);
        let p = meta(&st).pause.clone().unwrap();
        assert_eq!((p.from, p.started), (started, started), "a renewal keeps the run's start");
        if h + 1 + 3 * DAY <= cap {
            assert_eq!(p.until, h + 1 + 3 * DAY);
        } else {
            assert_eq!(p.until, cap, "clipped at started + 30 days");
            break;
        }
    }
    assert_eq!(h, 100 + 28 * DAY, "the 14th renewal reaches the cap");
    assert!(fix_pending(&st, h + 1));
    let capped = format!("this pause has reached its 30-day cap: it ends at block {cap} and can't be renewed, and programs can be paused again from block {}", started + 60 * DAY);
    for (i, at) in [(2, h + 1), (3, cap - 1)] {
        assert_eq!(refusal(&mut st, i, at), capped, "even with a fix pending");
    }
    assert_eq!((status(&st, cap - 1), status(&st, cap)), (Status::Paused { until: cap }, Status::On), "it lapses at the cap");

    let spaced = format!("programs can be paused again from block {}: the last run of pauses started at block {started}, and runs start at least 60 days apart", started + 60 * DAY);
    for at in [cap, cap + 10 * DAY, started + 60 * DAY - 1] {
        assert_eq!(refusal(&mut st, 2, at), spaced, "block {at}, even with a fix pending");
    }
    let h = started + 60 * DAY;
    all_sign(&mut st, [2, 3], h);
    let m = meta(&st);
    assert_eq!(m.pause, Some(Pause { from: h + 1, until: h + 1 + 3 * DAY, started: h + 1, signers: sorted([2, 3]) }), "a new run");
    assert_eq!(m.pause_signed, BTreeMap::from([(v(2), h), (v(3), h)]), "the last run's signatures are past their window, and dropped");
}

/// When a new run starts, `pause_signed` entries older than the window are dropped and younger ones
/// kept (a renewal keeps them all: `a_renewal_extends_until_and_keeps_the_run`). In reachable states
/// every entry is older by then, since the spacing outlasts every window of the last run, so the
/// younger one is hand-set.
#[test]
fn a_new_run_drops_the_expired_window_entries() {
    let mut st = chain(4);
    let h = 100 + 30 * DAY;
    let m = st.programs.meta.as_mut().unwrap();
    m.pause_signed.insert(v(2), h - 30 * DAY);
    m.pause_signed.insert(v(3), h - 30 * DAY + 1);
    all_sign(&mut st, [0, 1], h);
    assert_eq!(meta(&st).pause_signed, BTreeMap::from([(v(0), h), (v(1), h), (v(3), h - 30 * DAY + 1)]));
}

/// A pause lapses on its own at exactly `until`: `Paused` through `until − 1`, `On` from `until`, the
/// record kept (for the spacing). A signature at `until` meets the spacing rule, not a renewal.
#[test]
fn a_pause_lapses_at_exactly_until() {
    let mut st = chain(1);
    sign_at(&mut st, 0, 100).unwrap();
    let until = 101 + 3 * DAY;
    assert_eq!((status(&st, 100), status(&st, 101), status(&st, until - 1), status(&st, until)), (Status::On, Status::Paused { until }, Status::Paused { until }, Status::On));
    assert!(meta(&st).pause.is_some(), "the lapsed run's record stays in META");
    assert!(refusal(&mut st, 0, until).starts_with(&format!("programs can be paused again from block {}", 101 + 60 * DAY)));
}

/// A pause isn't tied to an activation: in exit-only (META kept, the feature off) it's signed,
/// holds (`Paused` over `ExitOnly`) and renewed the same way, and it lapses back to `ExitOnly`.
#[test]
fn a_pause_holds_in_exit_only() {
    let mut st = chain(4);
    st.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(status(&st, 100), Status::ExitOnly);
    all_sign(&mut st, [0, 1], 100);
    let until = 101 + 3 * DAY;
    assert_eq!((status(&st, 101), status(&st, until)), (Status::Paused { until }, Status::ExitOnly));
    all_sign(&mut st, [2, 3], 101 + DAY);
    let until = 102 + 4 * DAY;
    assert_eq!(meta(&st).pause.as_ref().map(|p| p.until), Some(until));
    assert_eq!((status(&st, until - 1), status(&st, until)), (Status::Paused { until }, Status::ExitOnly));
    // Switched back on, the same record holds whatever the feature says.
    st.upgrade.features.insert(PROGRAMS_FEATURE.into(), Feature { activated_at: 200 });
    assert_eq!((status(&st, until - 1), status(&st, until)), (Status::Paused { until }, Status::On));
}

/// A validator has one signature, made with its consensus key from its own account or with an
/// owner key from its owner's: both count for it, once. A key that's neither, or an account that
/// runs no validator of the set, counts for nobody.
#[test]
fn both_signing_forms_count_once_for_the_same_validator() {
    let mut st = chain(4);
    st.validators.get_mut(&v(0)).unwrap().owner = alice();
    let owner_key = key_id(&keys(ALICE).public_key());
    assert_eq!(validator_of(&st, &alice(), &owner_key, 100).unwrap(), v(0));

    let mut by_key = st.clone();
    sign_at(&mut by_key, 0, 100).unwrap();
    let e = sign_as(&mut by_key, &alice(), &owner_key, 100).unwrap_err().to_string();
    assert!(e.starts_with(&format!("{} already signed a pause at block 100", short(&v(0)))), "{e}");

    let mut by_owner = st.clone();
    sign_as(&mut by_owner, &alice(), &owner_key, 100).unwrap();
    assert_eq!(meta(&by_owner).votes, BTreeMap::from([(v(0), 100)]));
    assert!(refusal(&mut by_owner, 0, 100).contains("already signed"));

    let nobody = "only a validator in epoch 0's active set (its consensus key or its owner) can pause programs";
    for (sender, signer) in [(alice(), consensus_key(1)), (v(1), owner_key.clone()), (keys(2).address(), key_id(&keys(2).public_key()))] {
        assert_eq!(sign_as(&mut st.clone(), &sender, &signer, 100).unwrap_err().to_string(), nobody);
    }
    // A validator outside the epoch's set can't sign either.
    let mut out = st.clone();
    out.active_sets.insert(0, (1..4).map(v).collect());
    assert_eq!(sign_at(&mut out, 0, 100).unwrap_err().to_string(), nobody);
}

/// The block whose signature started a pause takes no more: the pause runs from the next block,
/// and a signature counts toward renewing it from there.
#[test]
fn the_block_that_starts_a_pause_takes_no_more_signatures() {
    let mut st = chain(7);
    all_sign(&mut st, 0..3, 100);
    assert_eq!(refusal(&mut st, 3, 100), "programs are paused from block 101: a pause was signed in this block");
    sign_at(&mut st, 3, 101).unwrap();
    assert_eq!(meta(&st).votes, BTreeMap::from([(v(3), 101)]));
}

/// Nothing to pause while programs are dormant: below v2, and at v2 before they first switch on.
#[test]
fn nothing_is_paused_while_programs_are_dormant() {
    let dormant = "programs have never been switched on here, so there's nothing to pause";
    assert_eq!(may_sign(&State::default(), &v(0), 100).unwrap_err().to_string(), dormant);
    let mut st = chain(4);
    st.programs.meta = None;
    st.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(may_sign(&st, &v(0), 100).unwrap_err().to_string(), dormant);
    let mut j = Journal::default();
    assert_eq!(sign(&mut st, &mut j, &v(0), &consensus_key(0), 100).unwrap_err().to_string(), dormant);
    assert!(st.programs.meta.is_none() && j.programs.meta.is_none(), "and nothing is written");
}

/// A signature's writes are journaled with its transaction: rolled back, the votes, the pause and
/// `pause_signed` are as they were, a started run included.
#[test]
fn a_signature_rolls_back_with_its_transaction() {
    let mut st = chain(4);
    sign_at(&mut st, 0, 100).unwrap();
    let before = st.programs.meta.clone();
    let mut j = Journal::default();
    sign(&mut st, &mut j, &v(1), &consensus_key(1), 100).unwrap();
    assert!(meta(&st).pause.is_some());
    st.rollback(j);
    assert_eq!(st.programs.meta, before);
}

// ---- through tag 69's hooks (WBS S5): admission, the dispatch, the duty lane ----

/// Where these tests' blocks pay their tips: an account of no one else's.
fn proposer() -> String {
    keys(9).address()
}

/// A `ProgramsPause` from `sender`, signed with `k`'s key (its signature isn't checked here:
/// admission and verification check it before the state machine runs), valid at `height`, with a
/// fee of 1 FANG (only the charge is debited) and `tip`.
fn pause_tx(st: &State, sender: &str, k: &Keys, height: u64, tip: u64) -> Tx {
    Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), nonce: st.account(sender).nonce, from: sender.into(), fee: FANG, tip, valid_until: height + 10, action: Action::ProgramsPause {}, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) }
}

/// Validator `i`'s duty form of tag 69 at `height`: its consensus key, from its own account.
fn duty(st: &State, i: usize, height: u64) -> Tx {
    pause_tx(st, &v(i), &keys(key_seed(i)), height, 0)
}

/// Runs `tx` as the block at `height` does (`State::apply`: admission, the fee phase, the action),
/// once the block before it executed (`econ::now` is `height`). An `Err` is a refusal at admission:
/// the state is exactly as it was, so nobody is charged. Programs' invariants hold either way.
fn apply_at(st: &mut State, tx: &Tx, height: u64) -> Result<Outcome> {
    st.econ.height = height - 1;
    let root = st.full_root();
    let r = st.apply(tx, &tx.chain_id.clone(), &proposer(), height);
    if r.is_err() {
        assert_eq!(st.full_root(), root, "an unadmittable transaction changes nothing");
    }
    super::super::check_invariants(st).expect("programs' invariants hold");
    r
}

/// Validator `i` signs at `height` in its duty form: admitted, it applies and is charged its fee
/// (the base fee: it tips nothing); panics on a refusal.
fn signs(st: &mut State, i: usize, height: u64) {
    let t = duty(st, i, height);
    st.econ.height = height - 1;
    let (before, charge) = (st.account(&v(i)).balance, crate::econ::check_fee(st, &t, height).unwrap());
    assert_eq!(apply_at(st, &t, height).unwrap_or_else(|e| panic!("validator {i} at block {height}: {e}")), Outcome::Applied);
    assert_eq!(st.account(&v(i)).balance, before - charge, "validator {i} paid its fee");
}

/// Validator `i`'s duty form at `height`, refused at admission (so uncharged): why.
fn refused(st: &mut State, i: usize, height: u64) -> String {
    let t = duty(st, i, height);
    apply_at(st, &t, height).expect_err("refused at admission").to_string()
}

/// Tag 69 end to end at n = 1, 4 and 7: each validator's signature is admitted, charged its fee and
/// counted by its execution; the one that reaches f + 1 starts the pause from the next block, and a
/// signature in the block that started it is refused, uncharged.
#[test]
fn tag_69_is_admitted_and_counted_at_n_1_4_and_7() {
    for (n, needed) in [(1, 1), (4, 2), (7, 3)] {
        let mut st = chain(n);
        let h = 100;
        for i in 0..needed {
            assert!(meta(&st).pause.is_none(), "n = {n}: {i} of {needed} signatures");
            assert_eq!(meta(&st).votes.len(), i, "n = {n}");
            signs(&mut st, i, h);
        }
        let until = h + 1 + 3 * DAY;
        let m = meta(&st);
        assert_eq!(m.pause, Some(Pause { from: h + 1, until, started: h + 1, signers: sorted(0..needed) }), "n = {n}");
        assert_eq!(m.pause_signed, (0..needed).map(|i| (v(i), h)).collect::<BTreeMap<_, _>>(), "n = {n}");
        assert_eq!((status(&st, h), status(&st, h + 1)), (Status::On, Status::Paused { until }), "n = {n}");
        if n > needed {
            assert_eq!(refused(&mut st, needed, h), format!("programs are paused from block {}: a pause was signed in this block", h + 1), "n = {n}");
        }
    }
}

/// Two signatures in adjacent epochs, a block apart, pause (n = 4) through admission and execution.
#[test]
fn votes_cross_an_epoch_boundary_through_admission() {
    let epoch = 1_000;
    let mut st = chain_with(4, epoch);
    st.active_sets.insert(1, (0..4).map(v).collect());
    signs(&mut st, 0, epoch - 1);
    signs(&mut st, 1, epoch);
    assert_eq!(meta(&st).pause.as_ref().map(|p| (p.from, p.signers.clone())), Some((epoch + 1, sorted([0, 1]))));
}

/// Every limit of §7.2 is an admission check, so a signature it refuses is unadmittable and charges
/// nothing (`apply_at` checks the state is untouched): an account that runs no validator of the set,
/// a live vote, the window (waived while a fix is pending), and anything while programs are dormant.
/// A lone signature is admitted, charged its fee, and spends no window.
#[test]
fn a_refused_signature_is_never_charged() {
    let mut st = chain(4);
    let not_a_validator = pause_tx(&st, &alice(), &keys(ALICE), 100, 0);
    assert_eq!(apply_at(&mut st, &not_a_validator, 100).unwrap_err().to_string(), "only a validator in epoch 0's active set (its consensus key or its owner) can pause programs");

    signs(&mut st, 0, 100);
    assert!(meta(&st).pause_signed.is_empty() && meta(&st).votes.contains_key(&v(0)), "a lone signature spends no window");
    assert_eq!(refused(&mut st, 0, 101), format!("{} already signed a pause at block 100; its signature counts until block {}", short(&v(0)), 100 + DAY - 1));
    signs(&mut st, 1, 101);
    assert_eq!(meta(&st).pause.as_ref().map(|p| p.from), Some(102));

    // Its starters wait out the window, unless a fix is pending, under each of `fix_pending`'s
    // conditions.
    let h = 102 + DAY;
    assert_eq!(refused(&mut st, 0, h), format!("{} started or renewed a pause at block 101: it may sign again from block {}, or while a fix is pending (a ScheduleUpgrade or DeactivateFeature {{ programs }} proposal voting or queued, or an upgrade scheduled)", short(&v(0)), 101 + 30 * DAY));
    for (what, fix) in fixes() {
        let mut st = st.clone();
        fix(&mut st);
        signs(&mut st, 0, h);
        signs(&mut st, 2, h);
        assert_eq!(meta(&st).pause.as_ref().map(|p| (p.until, p.signers.clone())), Some((h + 1 + 3 * DAY, sorted([0, 2]))), "renewed with {what}");
    }

    // At v2 before programs first switch on.
    let mut dormant = chain(4);
    dormant.programs.meta = None;
    dormant.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(refused(&mut dormant, 0, 100), "programs are off: a programs_pause transaction is refused until protocol v2 is in force and governance switches the `programs` feature on");
}

/// The cap and the spacing, through admission: renewed every two days with a fix pending, the run
/// is clipped at `started` + 30 days; a signature once it's there is unadmittable (uncharged), fix
/// or not; it lapses there; a new run is unadmittable below `started` + 60 days and admitted at
/// exactly that height, with a new `started`.
#[test]
fn the_cap_and_the_spacing_hold_at_admission() {
    let mut st = chain(4);
    propose(&mut st, schedule_upgrade(), ProposalStatus::Voting);
    let (started, cap) = (101, 101 + 30 * DAY);
    let mut h = 100;
    signs(&mut st, 0, h);
    signs(&mut st, 1, h);
    while meta(&st).pause.as_ref().unwrap().until < cap {
        h += 2 * DAY;
        signs(&mut st, 0, h);
        signs(&mut st, 1, h);
    }
    assert_eq!((h, meta(&st).pause.as_ref().map(|p| (p.started, p.until))), (100 + 28 * DAY, Some((started, cap))), "clipped at the cap by the 14th renewal");
    assert!(fix_pending(&st, cap - 1));
    let capped = format!("this pause has reached its 30-day cap: it ends at block {cap} and can't be renewed, and programs can be paused again from block {}", started + 60 * DAY);
    for (i, at) in [(2, h + 1), (3, cap - 1)] {
        assert_eq!(refused(&mut st, i, at), capped, "block {at}, a fix pending");
    }
    assert_eq!((status(&st, cap - 1), status(&st, cap)), (Status::Paused { until: cap }, Status::On), "it lapses at the cap");
    let spaced = format!("programs can be paused again from block {}: the last run of pauses started at block {started}, and runs start at least 60 days apart", started + 60 * DAY);
    for at in [cap, started + 60 * DAY - 1] {
        assert_eq!(refused(&mut st, 2, at), spaced, "block {at}");
    }
    let h = started + 60 * DAY;
    signs(&mut st, 2, h);
    signs(&mut st, 3, h);
    assert_eq!(meta(&st).pause.as_ref().map(|p| (p.from, p.started, p.until)), Some((h + 1, h + 1, h + 1 + 3 * DAY)), "a new run");
}

/// A pause in exit-only, through admission and execution: started the same way, it holds over
/// `ExitOnly` and lapses back to it.
#[test]
fn a_pause_in_exit_only_through_admission() {
    let mut st = chain(4);
    st.upgrade.features.remove(PROGRAMS_FEATURE);
    signs(&mut st, 0, 100);
    signs(&mut st, 1, 100);
    let until = 101 + 3 * DAY;
    assert_eq!((status(&st, 100), status(&st, 101), status(&st, until - 1), status(&st, until)), (Status::ExitOnly, Status::Paused { until }, Status::Paused { until }, Status::ExitOnly));
}

/// Tags 62–64 wait out a pause (§7.2): from the block after the signature that starts it through
/// `until − 1`, an upload, a create and a call are unadmittable, uncharged; at exactly `until` the
/// pause has lapsed, and each is admitted and applies. A chain of one validator: f + 1 = 1.
#[test]
fn program_transactions_wait_out_a_pause() {
    use crate::programs::fixtures as fx;
    let mut st = fx::state_on();
    let blobs = fx::store(&mut st, fx::COUNTER.0, fx::COUNTER.1, 1);
    let counter = fx::program_of(fx::BOB, 0);
    let digest = fx::install(&mut st, &counter, blobs, 2);
    let h = 5;
    let t = fx::tx(&st, fx::VALIDATOR, Action::ProgramsPause {}, h);
    assert_eq!(apply_at(&mut st, &t, h).unwrap(), Outcome::Applied);
    let until = h + 1 + 3 * DAY;
    assert_eq!(status(&st, h + 1), Status::Paused { until });
    let actions = || {
        let increment = Action::CallProgram { calls: vec![fx::call(&counter, &digest, "increment", 0)], fuel_limit: 2_000_000, storage_limit: FANG, bindings: vec![] };
        [fx::upload(fx::ESCROW.0, fx::ESCROW.1, 10 * FANG), fx::create(&blobs.0, &blobs.1), increment]
    };
    for at in [h + 1, until - 1] {
        for a in actions() {
            let t = fx::tx(&st, fx::ALICE, a, at);
            assert_eq!(apply_at(&mut st, &t, at).unwrap_err().to_string(), format!("programs are paused until block {until}: a {} transaction is refused until then", t.action.kind()));
        }
    }
    for a in actions() {
        let t = fx::tx(&st, fx::ALICE, a, until);
        assert_eq!(apply_at(&mut st, &t, until).unwrap(), Outcome::Applied, "{}", t.action.kind());
    }
    assert_eq!(st.programs.meta.as_ref().unwrap().programs, 2, "the create applied");
}

/// The duty lane and the tip (§2.5): the consensus key's signature from its validator's own account
/// is a validator duty (`staking::duty_of`: a full mempool keeps a slot for it) while the validator
/// is in the epoch's set and may sign, and only then; an owner key's isn't one, and may tip like any
/// action, as may the consensus key on its own account (it's a key there: tag 69 has no tip rule of
/// its own). The consensus key can't sign tag 69 from its owner's account: it isn't one of the
/// system transactions it signs for an owner (`staking::system_tx_key`).
#[test]
fn the_duty_lane_and_the_owner_keys_tip() {
    let mut st = chain(4);
    st.validators.get_mut(&v(0)).unwrap().owner = alice();
    let h = 100;
    // The mempool asks at the next height, `econ::now`.
    st.econ.height = h - 1;
    let duty_of = |st: &State, t: &Tx| crate::staking::duty_of(st, t, st.params.epoch_of(crate::econ::now(st)));
    assert_eq!(duty_of(&st, &duty(&st, 0, h)), Some((v(0), "programs_pause")));
    assert_eq!(duty_of(&st, &pause_tx(&st, &alice(), &keys(ALICE), h, 0)), None, "an owner key's signature competes by its tip");
    let key_for_owner = pause_tx(&st, &alice(), &keys(key_seed(0)), h, 0);
    assert_eq!(duty_of(&st, &key_for_owner), None);
    assert_eq!(apply_at(&mut st, &key_for_owner, h).unwrap_err().to_string(), format!("this key isn't authorized on {}", short(&alice())));
    let mut out = st.clone();
    out.active_sets.insert(0, (1..4).map(v).collect());
    assert_eq!(duty_of(&out, &duty(&out, 0, h)), None, "a validator outside the set");
    let mut dormant = st.clone();
    dormant.programs.meta = None;
    dormant.upgrade.features.remove(PROGRAMS_FEATURE);
    assert_eq!(duty_of(&dormant, &duty(&dormant, 0, h)), None, "programs dormant");

    // The owner signs for validator 0 with a tip, and validator 1's consensus key with one too: both
    // are admitted, the tips go to the proposer, and together they pause.
    let tip = crate::econ::base_fee(&st, h, duty(&st, 1, h).size());
    let (proposer_before, alice_before) = (st.account(&proposer()).balance, st.account(&alice()).balance);
    let by_owner = pause_tx(&st, &alice(), &keys(ALICE), h, tip);
    let charge = crate::econ::check_fee(&st, &by_owner, h).unwrap();
    assert_eq!(apply_at(&mut st, &by_owner, h).unwrap(), Outcome::Applied);
    assert_eq!(st.account(&alice()).balance, alice_before - charge, "the owner paid the base fee and its tip");
    assert_eq!(meta(&st).votes, BTreeMap::from([(v(0), h)]), "for its validator");
    assert_eq!(duty_of(&st, &duty(&st, 0, h)), None, "a validator with a live vote has no duty");
    let by_key = pause_tx(&st, &v(1), &keys(key_seed(1)), h, tip);
    assert_eq!(apply_at(&mut st, &by_key, h).unwrap(), Outcome::Applied);
    assert_eq!(st.account(&proposer()).balance, proposer_before + 2 * tip);
    assert_eq!(meta(&st).pause.as_ref().map(|p| p.signers.clone()), Some(sorted([0, 1])));

    // At the next height: the starters' window holds them back, so theirs is no duty; a validator
    // that hasn't signed may renew the pause, and that is one.
    st.econ.height = h;
    assert_eq!(duty_of(&st, &duty(&st, 1, h + 1)), None);
    assert_eq!(duty_of(&st, &duty(&st, 2, h + 1)), Some((v(2), "programs_pause")));
}

/// On a dev chain (one validator, so f + 1 = 1), through its mempool and a produced block: the
/// validator's duty form of tag 69 takes a slot in the duty lane; the block that carries it pauses
/// programs from the next height, so an upload is refused at the mempool's door and left out of
/// the next block; and the validator, having started the pause, can't sign again (its window).
#[test]
fn a_dev_chains_validator_pauses_programs_through_its_mempool() {
    use crate::programs::fixtures as fx;
    let (home, mut c) = fx::dev_chain("pause-lane", &[]);
    let me = c.keys.as_ref().unwrap().clone_keys();
    let pause = c.submit_own(Action::ProgramsPause {}, "").unwrap();
    assert!(c.in_lane(&pause.id()), "a duty: in the lane");
    c.produce_block().unwrap();
    let (h, b) = (c.height(), c.last_block().unwrap().clone());
    assert!(b.txs.iter().any(|t| t.id() == pause.id()) && b.failed.is_empty(), "included, applied");
    let until = h + 1 + 3 * blocks_per_day(c.state.params.block_time_ms);
    assert_eq!(status(&c.state, h + 1), Status::Paused { until });
    let upload = fx::signed(&c, &me, c.next_nonce(&c.me().unwrap()), FANG, fx::upload(fx::COUNTER.0, fx::COUNTER.1, 10 * FANG));
    let e = c.submit(upload.clone()).unwrap_err().to_string();
    assert!(e.contains(&format!("programs are paused until block {until}: a upload_code transaction is refused until then")), "{e}");
    c.produce_block_with(vec![upload.clone()]).unwrap();
    assert!(!c.last_block().unwrap().txs.iter().any(|t| t.id() == upload.id()), "left out of the block");
    let e = c.submit_own(Action::ProgramsPause {}, "").unwrap_err().to_string();
    assert!(e.contains("started or renewed a pause"), "{e}");
    drop(c);
    std::fs::remove_dir_all(home).ok();
}
