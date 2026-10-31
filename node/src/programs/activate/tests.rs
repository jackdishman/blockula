//! Switching programs on and off, and their parameters (WBS S2's activation and parameter
//! acceptance), on the state machine directly: a v2 dev chain whose bootstrap council passes
//! `ActivateFeature { programs }`, a genesis that lists programs, and `program_*` `ParamChange`s
//! before and after programs exist. Every block ends with `State::debug_check` (the incremental
//! root equals a full rebuild, and every invariant holds, programs' included). Transactions go
//! through `State::apply`, as a block applies them: the codec's rules, admission, then execution.
//! The same refusals on a running chain's every path (mempool, proposer, verifier) are
//! `programs::tests::program_parameters_are_refused_on_every_path_until_programs_exist`.

use super::*;
use crate::chain::{Allocation, BlockInfo, Genesis, Outcome, ValidatorInfo};
use crate::crypto::Keys;
use crate::governance::ProposalStatus;
use crate::programs::params::{Param, COUNCIL_PARAMS};
use crate::programs::{status, Status};
use crate::state::{meta, ns};
use crate::types::*;
use crate::upgrade::{is_active, Feature, ScheduledUpgrade, PROGRAMS_FEATURE, PROGRAMS_VERSION};
use anyhow::{bail, Result};

const FANG: u64 = DROPS_PER_FANG;
const CHAIN: &str = "programs-dev-1";
const ALICE: u8 = 1;
const VALIDATORS: [u8; 3] = [31, 32, 33];

fn keys(key_seed: u8) -> Keys {
    Keys::from_seed([key_seed; 32])
}

fn addr(key_seed: u8) -> String {
    keys(key_seed).address()
}

/// A dev chain with three validators (the bootstrap council until block 1,000), a funded user,
/// short governance periods (a 10-block vote, a 3-block timelock), the given upgrade schedule and
/// genesis features.
fn genesis(upgrades: Vec<ScheduledUpgrade>, features: Vec<String>) -> Genesis {
    let alloc = |key_seed: u8, fang: u64| Allocation { address: addr(key_seed), amount: fang * FANG, vesting: None, label: None };
    let mut allocations = vec![alloc(ALICE, 100_000)];
    allocations.extend(VALIDATORS.iter().map(|v| alloc(*v, 1_000)));
    let overrides = [("voting_blocks", 10), ("timelock_blocks", 3), ("lock_period_blocks", 5), ("proposal_deposit", 50 * FANG), ("quorum_floor", 0), ("bootstrap_end_height", 1_000)];
    Genesis {
        chain_id: CHAIN.into(),
        token: "FANG".into(),
        decimals: DECIMALS,
        block_time_ms: 1_000,
        validators: VALIDATORS.iter().enumerate().map(|(i, v)| ValidatorInfo { address: addr(*v), public_key: hex::encode(keys(*v).public_key()), p2p_addr: format!("127.0.0.1:{}", 7101 + i), http_addr: format!("127.0.0.1:{}", 7201 + i) }).collect(),
        faucet: false,
        created_at_ms: 1_750_000_000_000,
        allocations,
        epoch_blocks: 600,
        validator_admins: vec![],
        contribution: None,
        address_prefix: "boo".into(),
        param_overrides: overrides.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        operators: vec![],
        reserved_names: vec![],
        soul_ledger: vec![],
        features,
        upgrades,
        bootstrap_excluded: vec![],
        pool_repos: Default::default(),
    }
}

/// Born at v2 (scheduled from block 1), as `blockula-1` and dev chains are.
fn born_at_v2() -> Vec<ScheduledUpgrade> {
    vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 1 }]
}

/// The state machine and the height of the block being built.
struct T {
    st: State,
    h: u64,
}

impl T {
    fn new(g: Genesis) -> T {
        g.check().unwrap();
        T { st: g.initial_state(), h: 1 }
    }

    /// Applies a tx from `key_seed`'s account in the current block (signatures are checked before the
    /// state machine, so it carries a dummy one). `Err` if it couldn't be included or failed.
    fn tx(&mut self, key_seed: u8, action: Action) -> Result<()> {
        let k = keys(key_seed);
        let from = addr(key_seed);
        let mut tx = Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), nonce: self.st.account(&from).nonce, from, fee: 0, tip: 0, valid_until: self.h + 10, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        match self.st.apply(&tx, CHAIN, &addr(VALIDATORS[0]), self.h)? {
            Outcome::Applied => Ok(()),
            Outcome::Failed(e) => bail!(e),
        }
    }

    /// The tx Alice signs to propose `kind` with the minimum deposit.
    fn proposal(&self, kind: ProposalKind) -> Tx {
        let k = keys(ALICE);
        let action = Action::Propose { kind, deposit: 50 * FANG, title: "a proposal".into(), url: String::new() };
        let mut tx = Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), nonce: self.st.account(&addr(ALICE)).nonce, from: addr(ALICE), fee: 0, tip: 0, valid_until: self.h + 10, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        tx
    }

    /// Alice proposes `kind`: its id, or why it was refused.
    fn propose(&mut self, kind: ProposalKind) -> Result<u64> {
        let id = self.st.governance.next_proposal_id;
        self.tx(ALICE, Action::Propose { kind, deposit: 50 * FANG, title: "a proposal".into(), url: String::new() })?;
        Ok(id)
    }

    /// [`T::propose`] as the mempool's simulation applies a tx whose format was checked when it
    /// arrived: `apply_j` without the codec's rules (`Tx::check_format`), but with admission's.
    fn propose_in_the_mempool(&mut self, kind: ProposalKind) -> Result<u64> {
        let id = self.st.governance.next_proposal_id;
        let tx = self.proposal(kind);
        let mut j = Journal::default();
        match self.st.apply_j(&mut j, &tx, CHAIN, &addr(VALIDATORS[0]), self.h, crate::chain::ApplyMode::Mempool)? {
            Outcome::Applied => Ok(id),
            Outcome::Failed(e) => bail!(e),
        }
    }

    /// `kind`, proposed and voted aye by two of the three validators (⅔ of the bootstrap council).
    fn council(&mut self, kind: ProposalKind) -> u64 {
        let id = self.propose(kind).unwrap();
        self.council_votes(id);
        id
    }

    fn council_votes(&mut self, id: u64) {
        for v in &VALIDATORS[..2] {
            let digest = crate::governance::digest_of(&self.st.governance.proposals[&id]);
            self.tx(*v, Action::Vote { proposal_id: id, aye: true, amount: FANG, conviction: 0, proposal_digest: digest }).unwrap();
        }
    }

    /// Closes the current block (end-of-block steps, commit, root and invariant checks).
    fn next(&mut self) {
        let (hash, proposer) = (format!("block-{}", self.h), addr(VALIDATORS[0]));
        self.st.end_block(&BlockInfo { chain_id: CHAIN, height: self.h, version: crate::upgrade::version_at(&self.st, self.h), hash: &hash, proposer: &proposer, view: self.h, parent_view: self.h - 1, skipped: &[], tx_bytes: 0 });
        self.st.commit_block().unwrap();
        self.st.debug_check(self.h);
        self.h += 1;
    }

    fn through(&mut self, height: u64) {
        while self.h <= height {
            self.next();
        }
    }

    /// Runs proposal `id` through its vote and its timelock; returns the block it ran in.
    fn finish(&mut self, id: u64) -> u64 {
        let at = self.st.governance.proposals[&id].voting_end + self.st.chain_params.timelock_blocks;
        self.through(at);
        assert_eq!(self.st.governance.proposals[&id].status, ProposalStatus::Executed, "{}", self.st.governance.proposals[&id].outcome);
        at
    }

    fn meta(&self) -> &crate::programs::state::ProgramsMeta {
        self.st.programs.meta.as_ref().expect("programs have switched on")
    }
}

fn param(key: &str, value: u64) -> ProposalKind {
    ProposalKind::ParamChange { key: key.into(), value }
}

/// `on_activate` writes META once: the default parameters, the fuel rate at the floor, nothing
/// else. A second call (a reactivation) changes nothing.
#[test]
fn the_first_activation_writes_the_defaults_and_the_floor() {
    let mut st = genesis(born_at_v2(), vec![]).initial_state();
    assert!(st.programs.meta.is_none());
    on_activate(&mut st, None);
    let m = st.programs.meta.clone().unwrap();
    assert_eq!(m.params, params::defaults(&st.params));
    assert_eq!(m.params.len(), Param::ALL.len());
    assert_eq!((m.fuel_rate, params::param(&st, Param::FuelFloorRate)), (250, 250));
    assert_eq!((m.programs, m.state_bytes, m.value_held, m.sponsored, m.escrow), (0, 0, 0, 0, 0));
    assert!(m.pause.is_none() && m.votes.is_empty() && m.pause_signed.is_empty());
    // A reactivation keeps whatever programs had.
    let changed = |st: &mut State| {
        let m = st.programs.meta.as_mut().unwrap();
        m.fuel_rate = 999;
        m.params.insert(Param::ValueCap.key().into(), 5);
        m.clone()
    };
    let kept = changed(&mut st);
    on_activate(&mut st, None);
    let mut j = Journal::default();
    on_activate(&mut st, Some(&mut j));
    assert_eq!(st.programs.meta, Some(kept));
    assert!(j.programs.meta.is_none(), "nothing to journal");
}

/// On a v2 dev chain the bootstrap council switches programs on with `ActivateFeature { programs }`:
/// META appears in the block the proposal runs in (its end-of-block steps), committed under that
/// block's root, and `status` is `On` from the next block (that block's own transactions ran
/// `Dormant`). `DeactivateFeature { programs }` leaves META (`ExitOnly`), and switching programs
/// back on (`On` again) keeps their parameters and their fuel rate.
#[test]
fn the_council_switches_programs_on_off_and_on_again() {
    let mut t = T::new(genesis(born_at_v2(), vec![]));
    assert!(t.st.programs.meta.is_none() && t.st.record(ns::META, meta::PROGRAMS).is_none());
    assert_eq!(status(&t.st, t.h), Status::Dormant, "born at v2, programs off");
    let on = t.council(ProposalKind::ActivateFeature { feature: PROGRAMS_FEATURE.into() });
    let runs_at = t.st.governance.proposals[&on].voting_end + t.st.chain_params.timelock_blocks;
    t.through(runs_at - 1);
    assert!(t.st.programs.meta.is_none(), "nothing before the block it runs in");
    assert_eq!(status(&t.st, runs_at), Status::Dormant, "the transactions of the block it runs in");
    let root_before = t.st.root();
    t.next();
    assert_eq!(t.st.governance.proposals[&on].status, ProposalStatus::Executed, "{}", t.st.governance.proposals[&on].outcome);
    assert_eq!(t.st.upgrade.features.get(PROGRAMS_FEATURE), Some(&Feature { activated_at: runs_at }));
    assert_eq!(status(&t.st, runs_at + 1), Status::On, "from the next block");
    assert_eq!(t.meta().params, params::defaults(&t.st.params));
    assert_eq!(t.meta().fuel_rate, 250);
    let record = t.st.record(ns::META, meta::PROGRAMS).expect("META is a record");
    assert_eq!(record[0], super::super::state::FORMAT);
    assert_ne!(t.st.root(), root_before);
    // The council moves a parameter, and the fuel rate is the block controller's (§5.6): set high
    // here, every empty block's end lowers it by `next_rate`'s step (staying above the floor in
    // this test), and nothing else moves it.
    let cap = t.council(param(Param::ValueCap.key(), 1_000 * FANG));
    t.finish(cap);
    let (start, from) = (1_000_000, t.h);
    t.st.programs.meta.as_mut().unwrap().fuel_rate = start;
    t.st.commit_block().unwrap();
    let limit = params::param(&t.st, Param::MaxBlockFuel);
    let decayed = |t: &T| (from..t.h).fold(start, |r, _| crate::programs::fuel::next_rate(r, 0, limit, &t.st.chain_params));
    // META is `kept`, but for the rate, which is the controller's.
    let is_kept = |t: &T, kept: &crate::programs::state::ProgramsMeta| {
        assert_eq!(t.meta().fuel_rate, decayed(t), "the rate only the controller moved");
        assert_eq!(&crate::programs::state::ProgramsMeta { fuel_rate: kept.fuel_rate, ..t.meta().clone() }, kept);
    };
    let kept = t.meta().clone();
    assert_eq!(kept.params[Param::ValueCap.key()], 1_000 * FANG);
    // Off: exit-only. META stays, as it was.
    let off = t.council(ProposalKind::DeactivateFeature { feature: PROGRAMS_FEATURE.into() });
    let off_at = t.finish(off);
    assert!(!is_active(&t.st, PROGRAMS_FEATURE));
    is_kept(&t, &kept);
    assert_eq!(status(&t.st, off_at + 1), Status::ExitOnly);
    // A program parameter still changes in exit-only (§7.3).
    let rate = t.council(param(Param::FuelFloorRate.key(), 300));
    t.finish(rate);
    let mut kept = kept;
    kept.params.insert(Param::FuelFloorRate.key().into(), 300);
    is_kept(&t, &kept);
    // On again: the parameters and the rate are kept (no reset to the floor).
    assert_eq!(status(&t.st, t.h), Status::ExitOnly);
    let again = t.council(ProposalKind::ActivateFeature { feature: PROGRAMS_FEATURE.into() });
    let at = t.finish(again);
    assert_eq!(t.st.upgrade.features.get(PROGRAMS_FEATURE), Some(&Feature { activated_at: at }));
    is_kept(&t, &kept);
    assert!(t.meta().fuel_rate > 300, "still above the floor: not reset to it");
    assert_eq!(status(&t.st, at + 1), Status::On);
}

/// A proposal that switches programs on and fails when it runs leaves no META: the activation is
/// journaled with the rest of the proposal's action.
#[test]
fn a_failed_activation_leaves_nothing() {
    let mut st = genesis(born_at_v2(), vec![]).initial_state();
    let root = st.root();
    let mut j = Journal::default();
    crate::upgrade::exec_activate_feature(&mut st, &mut j, CHAIN, PROGRAMS_FEATURE, 5).unwrap();
    assert!(st.programs.meta.is_some() && j.programs.meta == Some(None));
    st.rollback(j);
    assert!(st.programs.meta.is_none() && !is_active(&st, PROGRAMS_FEATURE));
    st.commit_block().unwrap();
    assert_eq!(st.root(), root);
    // Refused (here: below v2) is refused before anything is written.
    let mut v1 = genesis(vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 100 }], vec![]).initial_state();
    assert!(crate::upgrade::exec_activate_feature(&mut v1, &mut Journal::default(), CHAIN, PROGRAMS_FEATURE, 99).is_err());
    assert!(v1.programs.meta.is_none());
}

/// A genesis that lists `programs` on a chain born at v2 has META at height 0, in the genesis
/// state and its root; at v1 `Genesis::check` refuses it (W2); a v2 genesis without it has none.
#[test]
fn a_genesis_that_lists_programs_starts_with_them() {
    let g = genesis(born_at_v2(), vec![PROGRAMS_FEATURE.into()]);
    g.check().unwrap();
    let st = g.initial_state();
    assert_eq!(st.upgrade.features.get(PROGRAMS_FEATURE), Some(&Feature { activated_at: 0 }));
    assert_eq!(st.programs.meta.as_ref().map(|m| (m.fuel_rate, m.params.clone())), Some((250, params::defaults(&st.params))));
    assert!(st.record(ns::META, meta::PROGRAMS).is_some());
    assert_eq!(st.root_hash(), st.full_root(), "committed with the genesis state");
    st.check_invariants().unwrap();
    // Block 0 runs v1 and executes nothing; programs are on from block 1, the first at v2.
    assert_eq!((status(&st, 0), status(&st, 1)), (Status::Dormant, Status::On));
    // The same genesis without the feature: another root, no META, dormant.
    let without = genesis(born_at_v2(), vec![]).initial_state();
    assert!(without.programs.meta.is_none());
    assert_eq!(status(&without, 1), Status::Dormant);
    assert_ne!(without.root(), st.root());
    // At v1 (no schedule, or v2 only later) the genesis is refused, as W2 words it.
    for upgrades in [vec![], vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 2 }]] {
        let e = format!("{:#}", genesis(upgrades.clone(), vec![PROGRAMS_FEATURE.into()]).check().unwrap_err());
        assert!(e.contains("unknown feature programs (known: ghost)"), "{upgrades:?}: {e}");
    }
    // Ghost Mode at genesis is no reason for META.
    let ghost = genesis(born_at_v2(), vec![crate::ghost::FEATURE.into()]).initial_state();
    assert!(ghost.programs.meta.is_none());
}

/// A `program_*` `ParamChange` before programs exist is refused when proposed, as v1 refuses an
/// unknown key, word for word, by admission in a block and in the mempool alike, and nothing is
/// charged; once programs exist it's admitted, checked against programs' bounds and sets their
/// map. The council passes every key but `program_min_upgrade_delay_blocks`, which takes the
/// token holders' supermajority (`needs_supermajority` follows `tokens_only`).
#[test]
fn program_parameters_exist_once_programs_do() {
    let mut t = T::new(genesis(born_at_v2(), vec![]));
    // What a v1 node says to any unknown key: `ProposalKind::check` (the codec) and
    // `ChainParams::set` word it alike.
    let unknown = |k: &str| format!("unknown parameter `{k}`");
    let alice = |t: &T| t.st.account(&addr(ALICE)).clone();
    let before = alice(&t);
    for k in Param::ALL {
        let value = k.default(&t.st.params);
        let kind = || param(k.key(), value);
        assert_eq!(t.propose(kind()).unwrap_err().to_string(), unknown(k.key()), "{k:?}");
        let mempool = t.propose_in_the_mempool(kind());
        assert_eq!(mempool.unwrap_err().to_string(), unknown(k.key()), "{k:?}: refused without the codec's rules too, programs never having switched on");
    }
    assert_eq!((alice(&t).balance, alice(&t).nonce), (before.balance, before.nonce), "unadmittable: nothing charged, no nonce used");
    assert!(t.st.governance.proposals.is_empty());
    let on = t.council(ProposalKind::ActivateFeature { feature: PROGRAMS_FEATURE.into() });
    t.finish(on);
    // Now they're programs': admitted, and filed as proposals …
    let id = t.propose(param(Param::ValueCap.key(), 1)).unwrap();
    assert_eq!(t.st.governance.proposals[&id].kind, param(Param::ValueCap.key(), 1));
    // … with their bounds applied when proposed (a failure: admitted, charged); an unknown
    // `program_*` key is still the codec's to refuse.
    assert_eq!(t.propose(param("program_max_block_fuel", 70_000_000)).unwrap_err().to_string(), "parameter `program_max_block_fuel` must be 20000000–60000000, got 70000000");
    assert_eq!(t.propose(param("program_max_code_bytes", 1)).unwrap_err().to_string(), unknown("program_max_code_bytes"));
    assert!(t.propose_in_the_mempool(param("program_max_code_bytes", 1)).unwrap_err().to_string().starts_with("unknown parameter `program_max_code_bytes` (known: "), "and programs' own check, past the codec");
    // The council passes each of its keys …
    let values = [(Param::FuelFloorRate, 500), (Param::MaxBlockFuel, 60_000_000), (Param::MaxTxFuel, 20_000_000), (Param::StateSoftBytes, 512 << 20), (Param::StoragePrice, 1_000), (Param::ValueCap, 0)];
    let ids: Vec<u64> = values.iter().map(|(k, v)| t.council(param(k.key(), *v))).collect();
    // … but not the minimum upgrade delay: the token holders' alone.
    let delay = t.council(param(Param::MinUpgradeDelayBlocks.key(), 3_600));
    t.through(t.st.governance.proposals[&delay].voting_end + t.st.chain_params.timelock_blocks);
    for (id, (k, v)) in ids.iter().zip(values) {
        assert_eq!(t.st.governance.proposals[id].status, ProposalStatus::Executed, "{k:?}");
        assert_eq!(t.meta().params[k.key()], v, "{k:?}");
        assert_eq!(params::param(&t.st, k), v);
    }
    let p = &t.st.governance.proposals[&delay];
    assert_eq!(p.status, ProposalStatus::Rejected);
    assert!(p.outcome.contains("the bootstrap council can't pass a change to program_min_upgrade_delay_blocks"), "{}", p.outcome);
    assert_eq!(t.meta().params[Param::MinUpgradeDelayBlocks.key()], 172_800);
    for k in Param::ALL {
        let kind = param(k.key(), 1);
        let council = COUNCIL_PARAMS.contains(&k.key());
        assert_eq!((crate::governance::needs_supermajority(&t.st, &kind), crate::governance::council_may_pass(&kind)), (!council, council), "{k:?}");
    }
    // The token holders pass it, by the supermajority.
    let id = t.propose(param(Param::MinUpgradeDelayBlocks.key(), 3_600)).unwrap();
    let digest = crate::governance::digest_of(&t.st.governance.proposals[&id]);
    t.tx(ALICE, Action::Vote { proposal_id: id, aye: true, amount: 90_000 * FANG, conviction: 1, proposal_digest: digest }).unwrap();
    t.finish(id);
    assert_eq!(t.meta().params[Param::MinUpgradeDelayBlocks.key()], 3_600);
}

/// `is_program_param` routes a key to programs only when all three hold: the `program_` prefix,
/// v2 in force at the height asked, and META `programs`. `exec_set_param` asks at `econ.height`
/// (the block it runs in); a key routed there that's out of bounds changes nothing.
#[test]
fn routing_needs_the_prefix_v2_and_meta() {
    let mut st = genesis(vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 10 }], vec![]).initial_state();
    let key = Param::ValueCap.key();
    assert!(!params::is_program_param(&st, key, 10), "no META");
    on_activate(&mut st, None);
    assert!(params::is_program_param(&st, key, 10) && params::is_program_param(&st, key, u64::MAX));
    assert!(!params::is_program_param(&st, key, 9), "below v2");
    assert!(!params::is_program_param(&st, "max_validators", 10) && !params::is_program_param(&st, "programvalue_cap", 10), "not the prefix");
    // Executing at block 10 (econ.height is the block's own once `econ::end_block` ran).
    st.econ.height = 10;
    let mut j = Journal::default();
    st.exec_set_param(&mut j, key, 7).unwrap();
    assert_eq!(params::param(&st, Param::ValueCap), 7);
    assert!(st.exec_set_param(&mut j, key, params::Param::ValueCap.bounds(&st.params).1 + 1).is_err());
    assert_eq!(params::param(&st, Param::ValueCap), 7, "a refused change changes nothing");
    // Below v2 the same key is `ChainParams`' to refuse, as in v1.
    st.econ.height = 9;
    assert_eq!(st.exec_set_param(&mut j, key, 8).unwrap_err().to_string(), "unknown parameter `program_value_cap`");
    // Journaled: rolled back with the proposal.
    st.rollback(j);
    assert_eq!(params::param(&st, Param::ValueCap), Param::ValueCap.default(&st.params));
}

/// Admission (`programs::admit`, the last check of every path a transaction takes) refuses a
/// `ParamChange` of each of programs' keys, on its own and inside a `MultisigPropose`, in the words
/// v1's codec refuses an unknown key with, wherever programs are dormant: on a v1 chain, at v2
/// before programs switch on, and below v2 on a state that has META. Once programs exist (on,
/// exit-only or paused) it lets them by. `ChainParams`' keys pass everywhere; another `program_*`
/// key is refused while programs are dormant, and once they exist it's the codec's (and programs'
/// own check's) to refuse.
#[test]
fn admission_refuses_program_parameters_until_programs_exist() {
    let v1 = genesis(vec![], vec![]).initial_state();
    let born = genesis(born_at_v2(), vec![]).initial_state();
    // Upgraded to v2 at block 10 and switched on since (hand-built: META and the feature).
    let mut upgraded = genesis(vec![ScheduledUpgrade { version: PROGRAMS_VERSION, activation_height: 10 }], vec![]).initial_state();
    upgraded.upgrade.features.insert(PROGRAMS_FEATURE.into(), Feature { activated_at: 10 });
    on_activate(&mut upgraded, None);
    let on = genesis(born_at_v2(), vec![PROGRAMS_FEATURE.into()]).initial_state();
    let mut exit_only = on.clone();
    exit_only.upgrade.features.remove(PROGRAMS_FEATURE);
    let mut paused = on.clone();
    paused.programs.meta.as_mut().unwrap().pause = Some(crate::programs::state::Pause { from: 3, until: 9, started: 3, signers: vec![addr(VALIDATORS[0])] });
    let cases = [("a v1 chain", &v1, 5, false), ("born at v2, programs off", &born, 5, false), ("META, below v2", &upgraded, 9, false), ("META, at v2", &upgraded, 10, true), ("on at genesis", &on, 1, true), ("exit-only", &exit_only, 5, true), ("paused", &paused, 5, true)];
    let tx = |kind: ProposalKind, nested: bool| {
        let propose = Action::Propose { kind, deposit: 50 * FANG, title: "a proposal".into(), url: String::new() };
        let action = if nested { Action::MultisigPropose { action: Box::new(propose) } } else { propose };
        Tx { from: addr(ALICE), ..crate::types::tests::vector_tx(action) }
    };
    for (label, st, h, admitted) in cases {
        assert_eq!(status(st, h) != Status::Dormant, admitted, "{label}");
        for key in Param::ALL.map(Param::key).into_iter().chain(["program_warp"]) {
            for nested in [false, true] {
                let r = crate::programs::admit(st, &tx(param(key, 1), nested), h, crate::chain::ApplyMode::Block);
                match (admitted, nested) {
                    (true, _) => r.unwrap_or_else(|e| panic!("{label}, {key}, nested {nested}: {e:#}")),
                    (false, false) => assert_eq!(r.unwrap_err().to_string(), format!("unknown parameter `{key}`"), "{label}"),
                    (false, true) => {
                        let e = r.unwrap_err();
                        assert_eq!((e.to_string(), format!("{e:#}")), ("the proposed action".to_string(), format!("the proposed action: unknown parameter `{key}`")), "{label}");
                    }
                }
            }
        }
        for nested in [false, true] {
            crate::programs::admit(st, &tx(param("max_validators", 40), nested), h, crate::chain::ApplyMode::Block).unwrap();
            crate::programs::admit(st, &tx(ProposalKind::Text {}, nested), h, crate::chain::ApplyMode::Block).unwrap();
        }
    }
}
