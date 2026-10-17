//! Governance tests, against the state machine directly (no disk): full proposal lifecycles,
//! tallies, conviction locks, the bootstrap council, the spend gate, every proposal kind,
//! distributions, multisig accounts, GOV-2/GOV-3, records and rollback, and random sequences.
//! Every block ends with `State::debug_check` (incremental root == full rebuild, invariants).

use super::*;
use crate::chain::{make_pop, treasury, Allocation, Genesis, Operator, Outcome, ValidatorInfo, Vesting};
use crate::crypto::Keys;
use crate::poc::SOUL;

const FANG: u64 = DROPS_PER_FANG;
const CHAIN: &str = "gov-1";
const P2P: [&str; 3] = ["127.0.0.1:7101", "127.0.0.1:7102", "127.0.0.1:7103"];
const HTTP: [&str; 3] = ["127.0.0.1:7201", "127.0.0.1:7202", "127.0.0.1:7203"];
/// Account seeds: three funded users, a fourth with nothing, a vesting founder, the operator
/// that owns validator 3, and the validators' consensus keys.
const ALICE: u8 = 1;
const BOB: u8 = 2;
const CAROL: u8 = 3;
const DAVE: u8 = 4;
const FOUNDER: u8 = 9;
const OWNER: u8 = 34;
const VALIDATORS: [u8; 3] = [31, 32, 33];

fn keys(seed: u8) -> Keys {
    Keys::from_seed([seed; 32])
}

fn addr(seed: u8) -> String {
    keys(seed).address()
}

fn kid(seed: u8) -> String {
    key_id(&keys(seed).public_key())
}

/// Three validators (the third owned by an operator), funded users, a founder whose tokens are
/// all still vesting, the community account and a treasury vesting over 400 blocks. Short
/// governance periods; no quorum floor (quorum is 4% of the unlocked supply, ≈ 6,560 FANG), no
/// spend gate and no council unless `params` says so.
fn genesis(params: &[(&str, u64)]) -> Genesis {
    let alloc = |seed: u8, fang: u64| Allocation { address: addr(seed), amount: fang * FANG, vesting: None, label: None };
    let mut allocations = vec![alloc(ALICE, 100_000), alloc(BOB, 50_000), alloc(CAROL, 10_000), alloc(OWNER, 1_000)];
    allocations.extend(VALIDATORS.iter().map(|v| alloc(*v, 1_000)));
    allocations.push(Allocation { address: addr(FOUNDER), amount: 1_000_000 * FANG, vesting: Some(Vesting { start: 0, cliff: 10_000, end: 20_000 }), label: Some("founder".into()) });
    allocations.push(Allocation { address: community("boo"), amount: 5_000_000 * FANG, vesting: None, label: Some("community".into()) });
    allocations.push(Allocation { address: treasury("boo"), amount: 4_000_000 * FANG, vesting: Some(Vesting { start: 0, cliff: 0, end: 400 }), label: Some("treasury".into()) });
    let mut overrides: BTreeMap<String, u64> = [("voting_blocks", 10), ("timelock_blocks", 3), ("lock_period_blocks", 5), ("proposal_deposit", 50 * FANG), ("quorum_floor", 0), ("governance_spend_height", 0), ("bootstrap_end_height", 0), ("multisig_expiry_blocks", 10), ("min_self_bond", FANG)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    for (k, v) in params {
        overrides.insert(k.to_string(), *v);
    }
    let v3 = keys(VALIDATORS[2]);
    Genesis {
        chain_id: CHAIN.into(),
        token: "FANG".into(),
        decimals: DECIMALS,
        block_time_ms: 1000,
        validators: VALIDATORS.iter().enumerate().map(|(i, v)| ValidatorInfo { address: addr(*v), public_key: hex::encode(keys(*v).public_key()), p2p_addr: P2P[i].into(), http_addr: HTTP[i].into() }).collect(),
        faucet: false,
        created_at_ms: 1_750_000_000_000,
        allocations,
        epoch_blocks: 600,
        validator_admins: vec![],
        contribution: None,
        address_prefix: "boo".into(),
        param_overrides: overrides,
        operators: vec![Operator { owner: addr(OWNER), consensus_key: hex::encode(v3.public_key()), pop: make_pop([VALIDATORS[2]; 32], CHAIN, &addr(OWNER), P2P[2], HTTP[2]), p2p_addr: P2P[2].into(), http_addr: HTTP[2].into(), self_bond: 10 * FANG, commission_bps: 0, provider: None, asn: None, country: None, entity: None }],
        reserved_names: vec![],
        soul_ledger: vec![],
        features: vec![],
        bootstrap_excluded: vec![],
    }
}

/// The state machine and the height of the block being built.
struct T {
    st: State,
    h: u64,
    /// Bytes of the txs the current block carried (econ's fullness).
    tx_bytes: u64,
}

impl T {
    fn new(params: &[(&str, u64)]) -> T {
        let g = genesis(params);
        g.check().unwrap();
        T { st: g.initial_state(), h: 1, tx_bytes: 0 }
    }

    /// Applies a tx acting for `from`, signed by `k` (signatures are checked before the state
    /// machine, so it carries a dummy one), in the current block. `Ok(charge)`, or why it wasn't
    /// included or failed.
    fn tx_as(&mut self, k: &Keys, from: &str, action: Action) -> Result<u64> {
        // The state machine alone doesn't check the chain instance (admission over HTTP does).
        let mut tx = Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), from: from.into(), nonce: self.st.account(from).nonce, fee: 0, tip: 0, valid_until: self.h + 10, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        self.tx_bytes += tx.size();
        match self.st.apply(&tx, CHAIN, &addr(VALIDATORS[0]), self.h)? {
            Outcome::Applied => Ok(tx.charge().unwrap()),
            Outcome::Failed(e) => bail!(e),
        }
    }

    fn tx(&mut self, seed: u8, action: Action) -> Result<u64> {
        self.tx_as(&keys(seed), &addr(seed), action)
    }

    fn err(&mut self, seed: u8, action: Action) -> String {
        self.tx(seed, action).unwrap_err().to_string()
    }

    /// Proposes `kind` with the minimum deposit; returns its id.
    fn propose(&mut self, seed: u8, kind: ProposalKind) -> u64 {
        let id = self.st.governance.next_proposal_id;
        self.tx(seed, Action::Propose { kind, deposit: 50 * FANG, title: "a proposal".into(), url: "https://example.com/p".into() }).unwrap();
        id
    }

    fn vote(&mut self, seed: u8, id: u64, aye: bool, fang: u64, conviction: u8) -> Result<u64> {
        self.tx(seed, Action::Vote { proposal_id: id, aye, amount: fang * FANG, conviction })
    }

    /// Closes the current block (end-of-block steps, commit, root and invariant checks).
    fn next(&mut self) {
        let hash = format!("block-{}", self.h);
        let proposer = addr(VALIDATORS[0]);
        let tx_bytes = std::mem::take(&mut self.tx_bytes);
        self.st.end_block(&BlockInfo { height: self.h, hash: &hash, proposer: &proposer, view: self.h, parent_view: self.h - 1, skipped: &[], tx_bytes });
        self.st.commit_block().unwrap();
        self.st.debug_check(self.h);
        self.h += 1;
    }

    /// Closes every block up to and including `height`.
    fn through(&mut self, height: u64) {
        while self.h <= height {
            self.next();
        }
    }

    fn bal(&self, seed: u8) -> u64 {
        self.st.account(&addr(seed)).balance
    }

    fn p(&self, id: u64) -> &Proposal {
        &self.st.governance.proposals[&id]
    }

    /// Runs proposal `id` through its vote and timelock.
    fn finish(&mut self, id: u64) {
        let end = self.p(id).voting_end;
        self.through(end + self.st.chain_params.timelock_blocks);
    }
}

fn text() -> ProposalKind {
    ProposalKind::Text {}
}

fn param(key: &str, value: u64) -> ProposalKind {
    ProposalKind::ParamChange { key: key.into(), value }
}

#[test]
fn a_proposal_passes_waits_out_its_timelock_and_runs() {
    let mut t = T::new(&[]);
    let before = t.bal(ALICE);
    let fee = t.tx(ALICE, Action::Propose { kind: param("max_validators", 64), deposit: 60 * FANG, title: "More validators".into(), url: String::new() }).unwrap();
    assert_eq!(t.bal(ALICE), before - 60 * FANG - fee, "the whole deposit is escrowed");
    assert_eq!(escrow(&t.st), 60 * FANG as u128);
    assert!(t.err(BOB, Action::Propose { kind: text(), deposit: 49 * FANG, title: "cheap".into(), url: String::new() }).contains("at least 50 FANG"));
    assert!(t.err(BOB, Action::Propose { kind: param("max_validators", 0), deposit: 50 * FANG, title: "zero".into(), url: String::new() }).contains("must be 1–64"), "out-of-bounds parameters are refused up front");
    let end = t.p(0).voting_end;
    assert_eq!((t.p(0).status, end, t.p(0).proposer.clone()), (ProposalStatus::Voting, t.h + 10, addr(ALICE)));
    t.vote(ALICE, 0, true, 10_000, 1).unwrap();
    t.vote(BOB, 0, false, 2_000, 1).unwrap();
    assert!(t.vote(CAROL, 7, true, 1, 0).unwrap_err().to_string().contains("no proposal #7"));
    t.through(end - 1);
    // Votes count through the last block of the vote.
    t.vote(CAROL, 0, true, 1, 0).unwrap();
    let alice = t.bal(ALICE);
    t.through(end);
    let p = t.p(0).clone();
    assert_eq!((p.status, p.execute_at, p.voters), (ProposalStatus::Queued, Some(end + 3), 3));
    assert_eq!((p.aye_amount, p.nay_amount, p.aye_weight, p.nay_weight), ((10_001 * FANG) as u128, (2_000 * FANG) as u128, (10_000 * FANG + FANG / 10) as u128, (2_000 * FANG) as u128));
    assert!(p.tally.as_ref().unwrap().quorum_reached && p.outcome.contains("passed by token vote"), "{}", p.outcome);
    assert_eq!(t.bal(ALICE), alice + 60 * FANG, "quorum reached: deposit refunded");
    assert_eq!(escrow(&t.st), 0);
    assert!(t.vote(CAROL, 0, true, 1, 0).unwrap_err().to_string().contains("closed at block"));
    // The timelock: nothing happens until `execute_at`.
    t.through(end + 2);
    assert_eq!(t.st.chain_params.max_validators, 31);
    t.through(end + 3);
    assert_eq!((t.p(0).status, t.st.chain_params.max_validators), (ProposalStatus::Executed, 64));
    assert_eq!(t.p(0).outcome, "max_validators is now 64");
    assert!(t.st.governance.open_proposals.is_empty());
}

#[test]
fn without_quorum_the_deposit_is_burned() {
    let mut t = T::new(&[]);
    let id = t.propose(ALICE, text());
    t.vote(ALICE, id, true, 1_000, 6).unwrap();
    let end = t.p(id).voting_end;
    t.through(end - 1);
    let (supply, burned, alice) = (t.st.supply, t.st.burned, t.bal(ALICE));
    t.through(end);
    let p = t.p(id);
    assert_eq!(p.status, ProposalStatus::Rejected);
    assert!(p.outcome.starts_with("no quorum: 1000 FANG of 65") && p.outcome.ends_with(" needed voted; deposit burned"), "{}", p.outcome);
    let tally = p.tally.clone().unwrap();
    // Unlocked supply: users, validators and the operator (with its bond); not the founder's
    // unvested million, not the system accounts.
    assert_eq!(tally.unlocked, unlocked_supply(&t.st, end));
    assert!(tally.unlocked > 163_000 * FANG as u128 && tally.unlocked < 164_000 * FANG as u128, "{}", tally.unlocked);
    assert_eq!(tally.quorum, tally.unlocked * 400 / 10_000);
    assert_eq!((t.st.supply, t.st.burned, t.bal(ALICE)), (supply - 50 * FANG, burned + 50 * FANG, alice));
    // The floor applies when it's higher than the share of the unlocked supply.
    let mut t = T::new(&[("quorum_floor", 20_000 * FANG)]);
    let id = t.propose(ALICE, text());
    t.vote(ALICE, id, true, 19_999, 1).unwrap();
    t.finish(id);
    assert_eq!((t.p(id).status, t.p(id).tally.as_ref().unwrap().quorum), (ProposalStatus::Rejected, (20_000 * FANG) as u128));
}

/// The unlocked supply is taken once an epoch: votes closing later in the epoch reuse it.
#[test]
fn the_unlocked_supply_is_taken_once_an_epoch() {
    let mut t = T::new(&[]);
    let a = t.propose(ALICE, text());
    t.through(5);
    let b = t.propose(ALICE, text());
    t.through(t.p(a).voting_end);
    let first = t.p(a).tally.as_ref().unwrap().unlocked;
    assert_eq!(t.st.governance.quorum_base, Some((0, first)));
    // 10,000 FANG move to a keyless account, out of the unlocked supply: not seen until epoch 1.
    t.tx(ALICE, Action::Transfer { to: community("boo"), amount: 10_000 * FANG }).unwrap();
    t.through(t.p(b).voting_end);
    assert_eq!(t.p(b).tally.as_ref().unwrap().unlocked, first);
    t.through(599);
    let c = t.propose(ALICE, text());
    let end = t.p(c).voting_end;
    t.through(end);
    let later = t.p(c).tally.as_ref().unwrap().unlocked;
    assert_eq!((later, t.st.governance.quorum_base), (unlocked_supply(&t.st, end), Some((1, later))));
    assert!(later < first - 9_999 * FANG as u128);
}

/// The quorum base is running totals (balances, pool stake, unbonding) less what the few vesting
/// accounts still hold, not a walk over every account, and it follows every change exactly:
/// transfers and fees, new accounts, delegating, unbonding, rewards, moves to keyless accounts and
/// the vesting curve. (`check_invariants` also compares it with a walk after every block.)
#[test]
fn the_unlocked_supply_is_kept_exact_without_walking_the_accounts() {
    let mut t = T::new(&[]);
    let at = |t: &T, h: u64| {
        let u = unlocked_supply(&t.st, h);
        assert_eq!(u, unlocked_supply_by_walk(&t.st, h), "running total vs walk at {h}");
        u
    };
    let u0 = at(&t, t.h);
    // Users, validators and the operator (its bond included); not the founder's unvested
    // million, not the system accounts.
    assert_eq!(u0, 164_000 * FANG as u128);
    // A transfer that opens an account burns its fee and the creation fee; the rest still votes.
    let creation = t.st.chain_params.account_creation_fee as u128;
    let fee = t.tx(ALICE, Action::Transfer { to: addr(DAVE), amount: 1_000 * FANG }).unwrap() as u128;
    let u1 = at(&t, t.h);
    assert_eq!(u1, u0 - fee - creation);
    // Delegating and unbonding move FANG between balance, pool and entry: only the fees go.
    let v3 = addr(VALIDATORS[2]);
    let fee = t.tx(DAVE, Action::Delegate { validator: v3.clone(), amount: 500 * FANG }).unwrap() as u128;
    let u2 = at(&t, t.h);
    assert_eq!(u2, u1 - fee);
    let shares = t.st.staking.delegations.get(&format!("{}/{v3}", addr(DAVE))).unwrap().shares as u64;
    let fee = t.tx(DAVE, Action::Undelegate { validator: v3.clone(), shares: shares / 2 }).unwrap() as u128;
    t.next();
    let u3 = at(&t, t.h);
    assert_eq!(u3, u2 - fee);
    assert!(t.st.staking.entries.total() > 0);
    // FANG sent to a keyless account stops counting.
    let fee = t.tx(BOB, Action::Transfer { to: community("boo"), amount: 2_000 * FANG }).unwrap() as u128;
    let u4 = at(&t, t.h);
    assert_eq!(u4, u3 - fee - 2_000 * FANG as u128);
    // Issuance at the epoch boundary: the stakers' share lands in the pools and counts; the
    // pool's and the treasury's shares don't.
    let staked = t.st.staking.pools.total();
    t.through(600);
    let rewards = t.st.staking.pools.total() - staked;
    assert!(rewards > 0);
    assert_eq!(at(&t, t.h), u4 + rewards);
    // The founder's allocation counts as it vests (linearly from 0 to 20,000, nothing before the
    // cliff at 10,000), with the walk agreeing at each height.
    let before = at(&t, 9_999);
    assert_eq!(at(&t, 10_000), before + 500_000 * FANG as u128);
    assert_eq!(at(&t, 15_000), before + 750_000 * FANG as u128);
    assert_eq!(at(&t, 20_000), before + 1_000_000 * FANG as u128);
    // Locked tokens the founder stakes stay unvested: only its liquid 10 FANG (less fees) vote.
    let fee = t.tx(ALICE, Action::Transfer { to: addr(FOUNDER), amount: 10 * FANG }).unwrap() as u128;
    let u5 = at(&t, t.h);
    assert_eq!(u5, u4 + rewards - fee);
    let fee = t.tx(FOUNDER, Action::Delegate { validator: v3, amount: 100_000 * FANG }).unwrap() as u128;
    t.next();
    assert_eq!(at(&t, t.h), u5 - fee);
}

#[test]
fn majorities_supermajorities_and_conviction() {
    let mut t = T::new(&[]);
    let upgrade = |v: u32| ProposalKind::ScheduleUpgrade { version: crate::consensus::PROTOCOL_VERSION + v, activation_height: 100_000 };
    let spend = |fang: u64| ProposalKind::TreasurySpend { to: addr(DAVE), amount: fang * FANG };
    let cases: Vec<(ProposalKind, u64, u8, u64, u8, ProposalStatus)> = vec![
        // A tie isn't a majority (and quorum was reached, so the deposit comes back).
        (text(), 5_000, 1, 5_000, 1, ProposalStatus::Rejected),
        // Conviction beats raw amount: 10,000 × 0.1 < 2,000 × 1.
        (text(), 10_000, 0, 2_000, 1, ProposalStatus::Rejected),
        (text(), 2_000, 1, 10_000, 0, ProposalStatus::Queued),
        // Upgrades need ⅔ of the weight.
        (upgrade(1), 6_000, 1, 4_000, 1, ProposalStatus::Rejected),
        (upgrade(2), 7_000, 1, 3_000, 1, ProposalStatus::Queued),
        // So do issuance parameters, but not other parameters.
        (param("issuance_initial_ppm", 50_000), 6_000, 1, 4_000, 1, ProposalStatus::Rejected),
        (param("max_validators", 40), 6_000, 1, 4_000, 1, ProposalStatus::Queued),
        // And treasury spends above 1% of the treasury (4M FANG): 10,000 is small, 50,000 large.
        (spend(10_000), 6_000, 1, 4_000, 1, ProposalStatus::Queued),
        (spend(50_000), 6_000, 1, 4_000, 1, ProposalStatus::Rejected),
        (spend(50_000), 7_000, 1, 3_000, 1, ProposalStatus::Queued),
        // Switching a feature on (Ghost Mode) needs ⅔ too; switching it off doesn't.
        (ProposalKind::ActivateFeature { feature: "ghost".into() }, 6_000, 1, 4_000, 1, ProposalStatus::Rejected),
        (ProposalKind::ActivateFeature { feature: "ghost".into() }, 7_000, 1, 3_000, 1, ProposalStatus::Queued),
        (ProposalKind::DeactivateFeature { feature: "ghost".into() }, 6_000, 1, 4_000, 1, ProposalStatus::Queued),
    ];
    let ids: Vec<u64> = cases.iter().map(|c| t.propose(ALICE, c.0.clone())).collect();
    for (id, (_, aye, ac, nay, nc, _)) in ids.iter().zip(&cases) {
        t.vote(ALICE, *id, true, *aye, *ac).unwrap();
        t.vote(BOB, *id, false, *nay, *nc).unwrap();
    }
    let alice = t.bal(ALICE);
    let supermajority: Vec<bool> = cases.iter().map(|c| needs_supermajority(&t.st, &c.0)).collect();
    t.through(t.p(0).voting_end);
    for ((id, c), needed) in ids.iter().zip(&cases).zip(supermajority) {
        let p = t.p(*id);
        assert_eq!(p.status, c.5, "{:?}: {}", c.0, p.outcome);
        assert_eq!(p.tally.as_ref().unwrap().supermajority, needed, "{:?}", c.0);
    }
    assert!(t.p(3).outcome.contains("fewer than 66.67% of the vote weight"), "{}", t.p(3).outcome);
    assert!(t.p(0).outcome.contains("didn't outweigh"), "{}", t.p(0).outcome);
    assert_eq!(t.bal(ALICE), alice + 50 * FANG * ids.len() as u64, "every vote reached quorum: all deposits back");
    let dave = t.bal(DAVE);
    t.finish(ids[9]);
    assert_eq!(t.bal(DAVE), dave + 60_000 * FANG, "both passed spends ran");
    assert_eq!(crate::upgrade::version_at(&t.st, 100_000), crate::consensus::PROTOCOL_VERSION + 2);
}

#[test]
fn conviction_sets_weight_and_lock_and_votes_can_change() {
    let mut t = T::new(&[]);
    let (a, b) = (t.propose(ALICE, text()), t.propose(ALICE, text()));
    let end = t.p(a).voting_end;
    let lock = |t: &T, seed: u8, id: u64| t.st.account(&addr(seed)).locks.into_iter().find(|l| l.reason == format!("vote:{id}")).map(|l| (l.amount.locked_at(0), (0..).find(|h| l.amount.locked_at(*h) == 0).unwrap()));
    for (conviction, tenths, periods) in [(0u8, 1u128, 0u64), (1, 10, 1), (3, 30, 4), (6, 60, 32)] {
        let mut t = T::new(&[]);
        let id = t.propose(ALICE, text());
        t.vote(BOB, id, true, 100, conviction).unwrap();
        let v = &t.st.governance.votes[&IdKey::new(id, &addr(BOB))];
        assert_eq!(v.weight, 100 * FANG as u128 * tenths / 10);
        // Locked through the last block of the vote, then `periods` lock periods (5 blocks).
        assert_eq!(v.locked_until, t.p(id).voting_end + 1 + 5 * periods);
        assert_eq!(lock(&t, BOB, id), Some((100 * FANG, v.locked_until)));
    }
    assert!(t.vote(ALICE, a, true, 1, 7).unwrap_err().to_string().contains("conviction must be 0–6"));
    // The same tokens vote on two proposals: the locks overlap.
    t.vote(ALICE, a, true, 90_000, 2).unwrap();
    t.vote(ALICE, b, false, 90_000, 0).unwrap();
    assert_eq!((t.p(a).aye_amount, t.p(b).nay_amount), ((90_000 * FANG) as u128, (90_000 * FANG) as u128));
    // A re-vote replaces the vote; the lock never shrinks.
    t.vote(ALICE, a, false, 5_000, 0).unwrap();
    let p = t.p(a);
    assert_eq!((p.aye_amount, p.nay_amount, p.nay_weight, p.voters), (0, (5_000 * FANG) as u128, (500 * FANG) as u128, 1));
    let v = t.st.governance.votes[&IdKey::new(a, &addr(ALICE))].clone();
    assert_eq!((v.amount, v.lock_amount, v.locked_until), (5_000 * FANG, 90_000 * FANG, end + 1 + 10));
    // Unlock releases only what expired.
    assert!(t.err(ALICE, Action::Unlock {}).contains("no expired vote locks"));
    t.through(end + 1);
    t.tx(ALICE, Action::Unlock {}).unwrap();
    assert_eq!((lock(&t, ALICE, a).is_some(), lock(&t, ALICE, b).is_some()), (true, false), "conviction 0 frees at the close; conviction 2 holds 10 more blocks");
    t.through(end + 9);
    assert!(t.err(ALICE, Action::Unlock {}).contains("no expired vote locks"));
    t.next();
    t.tx(ALICE, Action::Unlock {}).unwrap();
    assert!(t.st.account(&addr(ALICE)).locks.is_empty());
}

/// P-GOV × P-ECON: a vote locks the tokens it used, and transfers respect the lock, so the same
/// tokens can't be voted, moved to another account and voted again. The lock lapses on its own at
/// `locked_until`; `Unlock` then drops the record.
#[test]
fn voted_tokens_cant_be_moved_until_the_lock_expires() {
    let mut t = T::new(&[]);
    let id = t.propose(ALICE, text());
    t.vote(CAROL, id, true, 9_000, 1).unwrap();
    let locked_until = t.st.governance.votes[&IdKey::new(id, &addr(CAROL))].locked_until;
    let free = crate::econ::transferable(&t.st, &addr(CAROL), t.h);
    assert_eq!(free, t.bal(CAROL) - 9_000 * FANG);
    // More than the free part can't leave, though the balance covers it.
    let err = t.err(CAROL, Action::Transfer { to: addr(BOB), amount: free + FANG });
    assert!(err.contains(&format!("is locked (vote:{id})")), "{err}");
    // The free part can (less what fees take).
    let free = crate::econ::transferable(&t.st, &addr(CAROL), t.h);
    t.tx(CAROL, Action::Transfer { to: addr(BOB), amount: free - FANG }).unwrap();
    assert!(t.bal(CAROL) >= 9_000 * FANG && t.bal(CAROL) < 9_001 * FANG);
    // Still locked in block `locked_until - 1`, free in `locked_until`.
    t.through(locked_until - 2);
    assert_eq!(t.h, locked_until - 1);
    assert!(t.err(CAROL, Action::Transfer { to: addr(BOB), amount: 10 * FANG }).contains("is locked"));
    t.next();
    t.tx(CAROL, Action::Transfer { to: addr(BOB), amount: 10 * FANG }).unwrap();
    t.tx(CAROL, Action::Unlock {}).unwrap();
    assert!(t.st.account(&addr(CAROL)).locks.is_empty());
}

#[test]
fn unvested_tokens_dont_vote() {
    let mut t = T::new(&[]);
    let id = t.propose(ALICE, text());
    assert_eq!(crate::econ::voting_balance(&t.st, &addr(FOUNDER), t.h), 0);
    assert!(t.vote(FOUNDER, id, true, 1, 1).unwrap_err().to_string().contains("vote with up to 0 FANG"));
    // Liquid tokens on top of the vesting ones do vote.
    t.tx(ALICE, Action::Transfer { to: addr(FOUNDER), amount: 100 * FANG }).unwrap();
    let can = crate::econ::voting_balance(&t.st, &addr(FOUNDER), t.h);
    assert!(can < 100 * FANG && can > 99 * FANG, "100 FANG less the failed vote's fee: {can}");
    assert!(t.tx(FOUNDER, Action::Vote { proposal_id: id, aye: true, amount: can + 1, conviction: 1 }).is_err());
    t.tx(FOUNDER, Action::Vote { proposal_id: id, aye: true, amount: 50 * FANG, conviction: 1 }).unwrap();
    // Keyless system accounts never count toward the unlocked supply.
    let u = unlocked_supply(&t.st, t.h);
    assert!(u < 170_000 * FANG as u128);
}

/// Audit F03: a vote lock holds vested tokens, on top of what's still vesting, so a vesting
/// account (the founder here) can't vote, move the same tokens to another account and vote them
/// again; a plain account can't either. Before the fix the founder's 1M FANG vesting lock
/// swallowed its 99 FANG vote lock (the larger applied), and 98 FANG voted twice.
#[test]
fn a_vesting_account_cant_vote_and_move_the_same_tokens() {
    let mut t = T::new(&[]);
    let id = t.propose(ALICE, text());
    let founder = addr(FOUNDER);
    t.tx(ALICE, Action::Transfer { to: founder.clone(), amount: 100 * FANG }).unwrap();
    let before = crate::econ::transferable(&t.st, &founder, t.h);
    t.vote(FOUNDER, id, true, 99, 1).unwrap();
    let after = crate::econ::transferable(&t.st, &founder, t.h);
    assert!(after < FANG && before > 99 * FANG, "the vote locks 99 of its ~100 liquid FANG: {before} → {after}");
    let e = t.err(FOUNDER, Action::Transfer { to: addr(DAVE), amount: 98 * FANG });
    assert!(e.contains("is locked"), "{e}");
    assert_eq!(t.p(id).aye_amount, (99 * FANG) as u128);
    // Once the lock lapses the tokens move.
    let until = t.st.governance.votes[&IdKey::new(id, &founder)].locked_until;
    t.through(until - 1);
    t.tx(FOUNDER, Action::Transfer { to: addr(DAVE), amount: 98 * FANG }).unwrap();
}

/// Audit F42: a voter who voted everything it could can still change its vote (the re-vote's
/// fee took its holdings below the lock, which only needs to cover what grows it).
#[test]
fn a_full_balance_voter_can_change_its_vote() {
    let mut t = T::new(&[]);
    let id = t.propose(ALICE, text());
    let all = crate::econ::voting_balance(&t.st, &addr(CAROL), t.h);
    // What's left once this vote's own fee is paid: everything.
    let fee = min_fee(Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), from: addr(CAROL), nonce: 0, fee: 0, tip: 0, valid_until: 0, action: Action::Vote { proposal_id: id, aye: true, amount: all, conviction: 0 }, memo: String::new(), public_key: hex::encode(keys(CAROL).public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) }.size());
    t.tx(CAROL, Action::Vote { proposal_id: id, aye: true, amount: all - fee, conviction: 0 }).unwrap();
    assert_eq!(crate::econ::transferable(&t.st, &addr(CAROL), t.h), 0);
    t.vote(CAROL, id, false, 1, 0).unwrap();
    t.tx(CAROL, Action::Vote { proposal_id: id, aye: false, amount: 1, conviction: 1 }).unwrap();
    let p = t.p(id);
    assert_eq!((p.aye_amount, p.nay_amount, p.voters), (0, 1, 1));
    let v = &t.st.governance.votes[&IdKey::new(id, &addr(CAROL))];
    assert_eq!((v.amount, v.lock_amount), (1, all - fee), "the lock never shrinks");
}

#[test]
fn the_bootstrap_council_passes_proposals_until_the_bootstrap_ends() {
    let mut t = T::new(&[("bootstrap_end_height", 1_000)]);
    let [v1, v2, v3] = VALIDATORS;
    // ⅔ of the set approve with no token turnout to speak of: passes, deposit back.
    let a = t.propose(ALICE, text());
    t.vote(v1, a, true, 1, 0).unwrap();
    t.vote(v2, a, true, 1, 0).unwrap();
    assert_eq!(t.p(a).council.keys().cloned().collect::<BTreeSet<_>>(), BTreeSet::from([addr(v1), addr(v2)]));
    // A third of the set isn't enough.
    let b = t.propose(ALICE, text());
    t.vote(v1, b, true, 1, 0).unwrap();
    // Token nays outweighing token ayes block the council.
    let c = t.propose(ALICE, text());
    t.vote(v1, c, true, 1, 0).unwrap();
    t.vote(v2, c, true, 1, 0).unwrap();
    t.vote(BOB, c, false, 1, 1).unwrap();
    // The operator's owner account votes for its validator.
    let d = t.propose(ALICE, text());
    t.vote(OWNER, d, true, 1, 0).unwrap();
    t.vote(v1, d, true, 1, 0).unwrap();
    assert_eq!(t.p(d).council.get(&addr(v3)).map(|c| (c.aye, c.voter.clone())), Some((true, addr(OWNER))));
    // One council vote per validator: a re-vote replaces it.
    let e = t.propose(ALICE, text());
    t.vote(v1, e, true, 1, 0).unwrap();
    t.vote(v2, e, true, 1, 0).unwrap();
    t.vote(v1, e, false, 1, 0).unwrap();
    assert!(!t.p(e).council[&addr(v1)].aye);
    // Users who aren't validators don't sit on the council.
    let f = t.propose(ALICE, text());
    t.vote(BOB, f, true, 1, 0).unwrap();
    t.vote(CAROL, f, true, 1, 0).unwrap();
    assert!(t.p(f).council.is_empty());
    let alice = t.bal(ALICE);
    t.finish(f);
    let status: Vec<ProposalStatus> = [a, b, c, d, e, f].iter().map(|id| t.p(*id).status).collect();
    use ProposalStatus::*;
    assert_eq!(status, vec![Executed, Rejected, Rejected, Executed, Rejected, Rejected]);
    assert!(t.p(a).outcome.contains("recorded") && t.p(a).tally.as_ref().unwrap().council_ayes == 2);
    // Deposits: back for a, c, d, e (⅔ of the council voted), burned for b and f.
    assert_eq!(t.bal(ALICE), alice + 4 * 50 * FANG);
    // After the bootstrap the council is gone: the same ⅔ is just two tiny token votes.
    let mut t = T::new(&[("bootstrap_end_height", 20)]);
    let g = t.propose(ALICE, text());
    t.through(t.p(g).voting_end - 1);
    let h = t.propose(ALICE, text());
    for id in [g, h] {
        t.vote(v1, id, true, 1, 0).unwrap();
        t.vote(v2, id, true, 1, 0).unwrap();
    }
    t.finish(h);
    assert_eq!((t.p(g).status, t.p(h).status), (Executed, Rejected), "g closed at block 11, h at 21");
    assert_eq!(t.p(h).tally.as_ref().unwrap().council_size, 0, "no council once the bootstrap is over");
    let i = t.propose(ALICE, text());
    t.vote(v1, i, true, 1, 0).unwrap();
    assert!(t.p(i).council.is_empty(), "votes after the bootstrap aren't council votes");
}

/// Audit F15, F16: governance's own safety parameters change only by a token supermajority, never
/// by the bootstrap council, and never past their floors: the council can't extend itself, open
/// spending early or rewrite quorum, voting and the timelock with two 1-FANG votes, and a simple
/// majority can't lower the supermajority or the timelock before scheduling an upgrade.
#[test]
fn governance_safety_parameters_need_tokens_and_keep_their_floors() {
    let mut t = T::new(&[("bootstrap_end_height", 1_000), ("governance_spend_height", 5_000), ("quorum_floor", 30_000 * FANG)]);
    let [v1, v2, _] = VALIDATORS;
    let refused = |t: &mut T, kind: ProposalKind, why: &str| {
        let e = t.err(ALICE, Action::Propose { kind: kind.clone(), deposit: 50 * FANG, title: "x".into(), url: String::new() });
        assert!(e.contains(why), "{kind:?}: {e}");
    };
    let day = 86_400;
    // What can never pass: the council outliving its end, the spend gate opening earlier, a
    // supermajority under 60%, a timelock or lock period under a day, voting under 3 days, quorum
    // under 1% or 5M FANG, a large-spend line over 5%.
    refused(&mut t, param("bootstrap_end_height", 3_650 * day), "never lasts longer");
    refused(&mut t, param("governance_spend_height", 0), "never moves earlier");
    for (key, value, why) in [("supermajority_bps", 5_001, "to 6000 at the least"), ("treasury_large_bps", 10_000, "500 (5% of the treasury) at the most"), ("quorum_bps", 1, "to 100 at the least")] {
        refused(&mut t, param(key, value), why);
    }
    // This chain starts below the floors (short test periods, 30k FANG quorum floor): they only
    // move up.
    for (key, value) in [("timelock_blocks", 1), ("voting_blocks", 10 - 1), ("lock_period_blocks", 1), ("quorum_floor", 0)] {
        refused(&mut t, param(key, value), "at the least");
    }
    // Moves within the rules still need tokens: two council votes of 1 FANG pass a text proposal,
    // not a governance parameter.
    let council = |t: &mut T, kind: ProposalKind| {
        let id = t.propose(ALICE, kind);
        t.vote(v1, id, true, 1, 0).unwrap();
        t.vote(v2, id, true, 1, 0).unwrap();
        id
    };
    // Nor the tip cap or the bootstrap delegation's commission cap, whose raises pay the
    // validators (audit F01, F12).
    let ids = [council(&mut t, text()), council(&mut t, param("bootstrap_end_height", 500)), council(&mut t, param("timelock_blocks", 2 * day)), council(&mut t, param("governance_spend_height", 6_000)), council(&mut t, param("tip_cap_multiple", 1_000)), council(&mut t, param("bootstrap_commission_cap_bps", 10_000))];
    t.finish(ids[5]);
    let status: Vec<ProposalStatus> = ids.iter().map(|id| t.p(*id).status).collect();
    assert_eq!(status, [ProposalStatus::Executed, ProposalStatus::Rejected, ProposalStatus::Rejected, ProposalStatus::Rejected, ProposalStatus::Rejected, ProposalStatus::Rejected]);
    assert!(t.p(ids[4]).tally.as_ref().unwrap().supermajority && t.p(ids[5]).tally.as_ref().unwrap().supermajority);
    assert_eq!((t.st.chain_params.tip_cap_multiple, t.st.chain_params.bootstrap_commission_cap_bps), (20, 1_000));
    assert_eq!(t.p(ids[1]).tally.as_ref().unwrap().council_ayes, 2, "⅔ of the council said aye, and it wasn't enough");
    assert!(t.p(ids[1]).tally.as_ref().unwrap().supermajority);
    assert_eq!((t.st.chain_params.bootstrap_end_height, t.st.chain_params.governance_spend_height), (1_000, 5_000));
    // Token holders: a simple majority isn't enough, the supermajority is.
    let (a, b) = (t.propose(ALICE, param("supermajority_bps", 6_000)), t.propose(ALICE, param("governance_spend_height", 10_000)));
    let late = t.propose(ALICE, param("governance_spend_height", 6_000));
    for id in [a, b, late] {
        t.vote(ALICE, id, true, 60_000, 1).unwrap();
    }
    t.vote(BOB, a, false, 40_000, 1).unwrap();
    t.finish(late);
    assert_eq!([a, b].map(|id| t.p(id).status), [ProposalStatus::Rejected, ProposalStatus::Executed]);
    assert!(t.p(a).outcome.contains("fewer than 66.67%"), "{}", t.p(a).outcome);
    // Checked again when they run: the gate moved to 10,000 first, so 6,000 would move it back.
    assert_eq!(t.p(late).status, ProposalStatus::Failed);
    assert!(t.p(late).outcome.contains("never moves earlier"), "{}", t.p(late).outcome);
    assert_eq!((t.st.chain_params.supermajority_bps, t.st.chain_params.governance_spend_height), (6_667, 10_000));
}

#[test]
fn spending_waits_for_the_spend_height_and_vested_treasury_funds() {
    let mut t = T::new(&[("governance_spend_height", 30)]);
    let spend = |to: u8, fang: u64| ProposalKind::TreasurySpend { to: addr(to), amount: fang * FANG };
    let dist = ProposalKind::Distribution { distribution_id: t.st.governance.next_proposal_id, merkle_root: "ab".repeat(32), total: FANG, claim_until: 1_000 };
    for kind in [spend(CAROL, 1), dist.clone()] {
        assert!(t.err(ALICE, Action::Propose { kind, deposit: 50 * FANG, title: "early".into(), url: String::new() }).contains("open at block 30"));
    }
    t.propose(ALICE, text());
    t.through(29);
    let small = t.propose(ALICE, spend(CAROL, 10_000));
    // 3.9M FANG needs the supermajority, and the treasury's vesting allows ~1M by then.
    let big = t.propose(ALICE, spend(DAVE, 3_900_000));
    for id in [small, big] {
        t.vote(ALICE, id, true, 50_000, 1).unwrap();
    }
    let (carol, treasury) = (t.bal(CAROL), t.st.account(&t.st.treasury()).balance);
    t.finish(big);
    assert_eq!((t.p(small).status, t.p(big).status), (ProposalStatus::Executed, ProposalStatus::Failed));
    assert_eq!(t.p(small).outcome, format!("paid 10000 FANG to {}", short(&addr(CAROL))));
    assert!(t.p(big).outcome.contains("the treasury can spend"), "{}", t.p(big).outcome);
    assert_eq!((t.bal(CAROL), t.bal(DAVE)), (carol + 10_000 * FANG, 0));
    assert_eq!(t.st.account(&t.st.treasury()).balance, treasury - 10_000 * FANG, "the failed spend moved nothing");
}

/// Every proposal kind runs its lever: the former admin powers, upgrades and features, spending.
#[test]
fn every_proposal_kind_executes() {
    let mut t = T::new(&[]);
    // Fixtures: GitHub links, a hunt with two funders, a feature to switch.
    t.tx(ALICE, Action::LinkGithub { handle: "alice-gh".into(), gist_id: "abc1".into() }).unwrap();
    t.tx(BOB, Action::LinkGithub { handle: "bob-gh".into(), gist_id: "abc2".into() }).unwrap();
    t.tx(CAROL, Action::CreateHunt { repo: "jackdishman/blockula".into(), issue: 7, amount: 20 * FANG, expires_at: 500, title: "a hunt".into() }).unwrap();
    t.tx(BOB, Action::FundHunt { hunt_id: 0, amount: 5 * FANG }).unwrap();
    let next = t.st.governance.next_proposal_id;
    let (root, _) = build_distribution(next + 2, &[(addr(DAVE), 7 * FANG)]).unwrap();
    let kinds = vec![
        param("quorum_bps", 500),
        ProposalKind::TreasurySpend { to: addr(CAROL), amount: 1_000 * FANG },
        ProposalKind::Distribution { distribution_id: next + 2, merkle_root: hex::encode(root), total: 7 * FANG, claim_until: 200 },
        ProposalKind::SetRepo { repo: "jackdishman/blockula".into(), weight: 5 },
        ProposalKind::GrantSoul { to: addr(ALICE), amount: 50 * SOUL, reason: "bootstrap".into() },
        ProposalKind::FlagSybil { account: addr(DAVE), reason: "same person".into() },
        ProposalKind::UnlinkGithub { handle: "BOB-gh".into() },
        ProposalKind::RefundHunt { hunt_id: 0 },
        ProposalKind::ScheduleUpgrade { version: crate::consensus::PROTOCOL_VERSION + 1, activation_height: 100_000 },
        ProposalKind::ActivateFeature { feature: "ghost".into() },
        text(),
    ];
    let ids: Vec<u64> = kinds.into_iter().map(|k| t.propose(ALICE, k)).collect();
    for id in &ids {
        t.vote(ALICE, *id, true, 50_000, 1).unwrap();
    }
    let (carol, bob, community_before) = (t.bal(CAROL), t.bal(BOB), t.st.account(&community("boo")).balance);
    t.finish(*ids.last().unwrap());
    for id in &ids {
        assert_eq!(t.p(*id).status, ProposalStatus::Executed, "#{id}: {}", t.p(*id).outcome);
    }
    let s = &t.st;
    assert_eq!(s.chain_params.quorum_bps, 500);
    assert_eq!(t.bal(CAROL), carol + 1_000 * FANG + 20 * FANG, "the spend, and her hunt refunded");
    assert_eq!(t.bal(BOB), bob + 5 * FANG, "funders get exactly what they put in");
    assert_eq!(s.hunts[&0].status, crate::chain::HuntStatus::Cancelled);
    assert_eq!(s.account(&community("boo")).balance, community_before - 7 * FANG);
    assert_eq!(s.governance.distributions[&(next + 2)].total, 7 * FANG);
    assert_eq!(s.poc.repos.get("jackdishman/blockula"), Some(&5));
    assert_eq!(s.soul_of(&addr(ALICE), t.h), 50 * SOUL);
    assert_eq!(s.poc.sybils[&addr(DAVE)].by, format!("proposal #{}", ids[5]));
    assert!(!s.github.contains_key("bob-gh") && s.github.contains_key("alice-gh"));
    assert_eq!(crate::upgrade::version_at(s, 100_000), crate::consensus::PROTOCOL_VERSION + 1);
    assert!(crate::upgrade::is_active(s, "ghost"));
    // Round two: the claim Alice can now make, rejected; a clawback with nothing to claw back
    // fails when it runs and changes nothing; the feature goes off.
    t.tx(ALICE, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr: 12, familiar_key: None }).unwrap();
    let cancel = ProposalKind::CancelUpgrade { version: crate::consensus::PROTOCOL_VERSION + 1, activation_height: 100_000 };
    let ids = [ProposalKind::RejectClaim { claim_id: 0, reason: "reverted".into() }, ProposalKind::Clawback { account: addr(ALICE), reason: "x".into() }, ProposalKind::DeactivateFeature { feature: "ghost".into() }, cancel].map(|k| t.propose(ALICE, k));
    for id in ids {
        t.vote(ALICE, id, true, 50_000, 1).unwrap();
    }
    t.finish(ids[2]);
    assert_eq!(ids.map(|id| t.p(id).status), [ProposalStatus::Executed, ProposalStatus::Failed, ProposalStatus::Executed, ProposalStatus::Executed]);
    assert_eq!(crate::upgrade::version_at(&t.st, 100_000), crate::consensus::PROTOCOL_VERSION, "the upgrade is withdrawn");
    assert_eq!(t.st.poc.claims[&0].status, crate::poc::ClaimStatus::Rejected);
    assert!(t.p(ids[1]).outcome.contains("no unvested rewards"), "{}", t.p(ids[1]).outcome);
    assert!(!crate::upgrade::is_active(&t.st, "ghost"));
    // Round three (audit F54): the appeal path. A sybil flag is lifted by a later proposal, and
    // the account can earn again; one that isn't flagged can't be the subject of one.
    let e = t.err(ALICE, Action::Propose { kind: ProposalKind::UnflagSybil { account: addr(BOB), reason: "x".into() }, deposit: 50 * FANG, title: "x".into(), url: String::new() });
    assert!(e.contains("isn't flagged"), "{e}");
    assert!(t.st.is_sybil(&addr(DAVE)));
    let id = t.propose(ALICE, ProposalKind::UnflagSybil { account: addr(DAVE), reason: "mistaken identity".into() });
    assert_eq!(describe_kind(&t.st, &t.p(id).kind, &|_| None), format!("lift {}'s sybil flag", short(&addr(DAVE))));
    t.vote(ALICE, id, true, 50_000, 1).unwrap();
    t.finish(id);
    assert_eq!(t.p(id).status, ProposalStatus::Executed, "{}", t.p(id).outcome);
    assert!(!t.st.is_sybil(&addr(DAVE)));
    let mut j = crate::chain::Journal::default();
    t.st.exec_grant_soul(&mut j, &addr(DAVE), SOUL, t.h).unwrap();
    assert_eq!(t.st.soul_of(&addr(DAVE), t.h), SOUL, "it earns Soul again");
}

/// F50: governance can withdraw a scheduled upgrade before it activates (a release pulled for a
/// bug, a slipped release). `CancelUpgrade` names the exact schedule entry, needs the
/// supermajority, runs the block after its vote with no timelock, and must land at least
/// `MIN_UPGRADE_LEAD` blocks before the activation; every `ScheduleUpgrade` leaves room for one
/// proposed the block after it runs. A stale or repeated cancellation fails cleanly.
#[test]
fn a_scheduled_upgrade_can_be_cancelled_before_it_activates() {
    let mut t = T::new(&[]);
    let lead = crate::upgrade::MIN_UPGRADE_LEAD;
    let v = crate::consensus::PROTOCOL_VERSION + 1;
    let (voting, timelock) = (t.st.chain_params.voting_blocks, t.st.chain_params.timelock_blocks);
    let refused = |t: &mut T, kind: ProposalKind, why: &str| {
        let e = t.err(ALICE, Action::Propose { kind: kind.clone(), deposit: 50 * FANG, title: "x".into(), url: String::new() });
        assert!(e.contains(why), "{kind:?}: {e}");
    };
    // The earliest activation leaves, after the schedule runs, a block to propose a cancellation,
    // its vote, the block it runs in and the lead.
    let runs_at = t.h + voting + timelock;
    let earliest = runs_at + 1 + voting + 1 + lead;
    let cancel = ProposalKind::CancelUpgrade { version: v, activation_height: earliest };
    assert!(needs_supermajority(&t.st, &cancel));
    refused(&mut t, cancel.clone(), &format!("no upgrade to v{v} at block {earliest} is scheduled"));
    refused(&mut t, ProposalKind::ScheduleUpgrade { version: v, activation_height: earliest - 1 }, &format!("then for a CancelUpgrade's vote and 64 blocks: block {earliest} or later"));
    let schedule = t.propose(ALICE, ProposalKind::ScheduleUpgrade { version: v, activation_height: earliest });
    t.vote(ALICE, schedule, true, 50_000, 1).unwrap();
    // Until it has run there's nothing to cancel: while it's voted on, the remedy is a nay; once
    // it has passed, the error says when a cancellation can be proposed.
    refused(&mut t, cancel.clone(), &format!("proposal #{schedule}, which would schedule it, is still being voted on"));
    let closes = t.p(schedule).voting_end;
    t.through(closes);
    refused(&mut t, cancel.clone(), &format!("isn't scheduled yet: proposal #{schedule} schedules it at block {runs_at}"));
    t.through(runs_at);
    assert_eq!((t.p(schedule).status, crate::upgrade::version_at(&t.st, earliest)), (ProposalStatus::Executed, v));
    // The block after, a cancellation for another height is refused; the exact one is accepted.
    refused(&mut t, ProposalKind::CancelUpgrade { version: v, activation_height: earliest + 1 }, &format!("v{v} is scheduled at block {earliest}, not {}", earliest + 1));
    let c = t.propose(BOB, cancel.clone());
    assert_eq!(describe_kind(&t.st, &cancel, &|_| None), format!("cancel the upgrade to protocol v{v} at block {earliest}"));
    t.vote(ALICE, c, true, 50_000, 1).unwrap();
    // It closes and runs the very next block (no timelock), exactly the lead before the upgrade.
    let end = t.p(c).voting_end;
    t.through(end);
    assert_eq!((t.p(c).status, t.p(c).execute_at), (ProposalStatus::Queued, Some(end + 1)));
    assert!(t.p(c).outcome.contains(&format!("runs at block {}", end + 1)), "{}", t.p(c).outcome);
    assert_eq!(end + 1 + lead, earliest);
    t.through(end + 1);
    assert_eq!(t.p(c).status, ProposalStatus::Executed, "{}", t.p(c).outcome);
    assert_eq!(t.p(c).outcome, format!("cancelled the upgrade to protocol v{v} at block {earliest}"));
    assert!(t.st.upgrade.schedule.is_empty());
    assert_eq!(crate::upgrade::version_at(&t.st, earliest), crate::consensus::PROTOCOL_VERSION);
    // Nothing left to cancel, and the version can be scheduled again.
    refused(&mut t, cancel, "no upgrade to");
    let again = t.h + voting + timelock + 1 + voting + 1 + lead + 100;
    let cancel = ProposalKind::CancelUpgrade { version: v, activation_height: again };
    let s = t.propose(ALICE, ProposalKind::ScheduleUpgrade { version: v, activation_height: again });
    t.vote(ALICE, s, true, 50_000, 1).unwrap();
    t.finish(s);
    assert_eq!(crate::upgrade::version_at(&t.st, again), v);
    // 60% isn't enough: it's the supermajority that scheduled it.
    let c = t.propose(BOB, cancel.clone());
    t.vote(ALICE, c, true, 6_000, 1).unwrap();
    t.vote(CAROL, c, false, 4_000, 1).unwrap();
    let end = t.p(c).voting_end;
    t.through(end);
    assert_eq!(t.p(c).status, ProposalStatus::Rejected, "{}", t.p(c).outcome);
    assert!(t.p(c).outcome.contains("fewer than"), "{}", t.p(c).outcome);
    // Two cancellations both pass: the first withdraws it, the second fails cleanly when it runs
    // (it can't re-cancel anything), and nothing else changes.
    let (c1, c2) = (t.propose(BOB, cancel.clone()), t.propose(CAROL, cancel.clone()));
    t.vote(ALICE, c1, true, 50_000, 1).unwrap();
    t.vote(ALICE, c2, true, 50_000, 1).unwrap();
    let end = t.p(c2).voting_end;
    t.through(end + 1);
    assert_eq!((t.p(c1).status, t.p(c2).status), (ProposalStatus::Executed, ProposalStatus::Failed));
    assert_eq!(t.p(c2).outcome, format!("failed when it ran: no upgrade to v{v} at block {again} is scheduled"));
    assert!(t.st.upgrade.schedule.is_empty());
    // Scheduled once more: once a cancellation could no longer land 64 blocks before the
    // activation, it's refused up front, and the operational path (OPERATIONS.md "Upgrades") is
    // what's left; after the activation it's refused too.
    let last = t.h + voting + timelock + 1 + voting + 1 + lead + 10;
    let cancel = ProposalKind::CancelUpgrade { version: v, activation_height: last };
    let s = t.propose(ALICE, ProposalKind::ScheduleUpgrade { version: v, activation_height: last });
    t.vote(ALICE, s, true, 50_000, 1).unwrap();
    t.finish(s);
    t.through(last - lead - voting - 2);
    let c = t.propose(BOB, cancel.clone());
    assert_eq!(t.p(c).voting_end + 1 + lead, last, "the last block a cancellation can be proposed in");
    t.next();
    refused(&mut t, cancel.clone(), "less than 64 blocks before it");
    t.through(last);
    refused(&mut t, cancel, "less than 64 blocks before it");
    assert_eq!(crate::upgrade::version_at(&t.st, t.h), v);
    // The one proposed in time, which nobody voted for, changed nothing either.
    assert_eq!(t.p(c).status, ProposalStatus::Rejected, "{}", t.p(c).outcome);
}

#[test]
fn proposals_are_checked_when_they_are_made() {
    let mut t = T::new(&[]);
    let refused = |t: &mut T, kind: ProposalKind, why: &str| {
        let e = t.err(ALICE, Action::Propose { kind: kind.clone(), deposit: 50 * FANG, title: "x".into(), url: String::new() });
        assert!(e.contains(why), "{kind:?}: {e}");
    };
    refused(&mut t, ProposalKind::RejectClaim { claim_id: 5, reason: "x".into() }, "no claim #5");
    refused(&mut t, ProposalKind::UnlinkGithub { handle: "nobody".into() }, "isn't linked");
    refused(&mut t, ProposalKind::RefundHunt { hunt_id: 0 }, "no hunt #0");
    refused(&mut t, ProposalKind::ActivateFeature { feature: "warp".into() }, "unknown feature warp");
    // Upgrades that can't run when the vote and the timelock are over (audit F46): too soon after
    // them, or not newer than what's in force; and one that leaves no room for a cancellation's
    // vote before it (audit F50).
    let runs_at = t.h + 10 + 3;
    let v = crate::consensus::PROTOCOL_VERSION;
    let lead = crate::upgrade::MIN_UPGRADE_LEAD;
    let voting = t.st.chain_params.voting_blocks;
    refused(&mut t, ProposalKind::ScheduleUpgrade { version: 9, activation_height: 10 }, "the vote and the timelock end at block");
    refused(&mut t, ProposalKind::ScheduleUpgrade { version: v + 1, activation_height: runs_at + lead - 1 }, &format!("at least 64 blocks after block {runs_at}"));
    refused(&mut t, ProposalKind::ScheduleUpgrade { version: v, activation_height: runs_at + 1_000 }, "isn't newer than");
    refused(&mut t, ProposalKind::ScheduleUpgrade { version: v + 1, activation_height: runs_at + lead }, "room for the vote and the timelock");
    t.propose(ALICE, ProposalKind::ScheduleUpgrade { version: v + 1, activation_height: runs_at + voting + lead + 2 });
    let next = t.st.governance.next_proposal_id;
    refused(&mut t, ProposalKind::Distribution { distribution_id: next, merkle_root: "ab".repeat(32), total: FANG, claim_until: 14 }, "after the vote and the timelock");
    refused(&mut t, ProposalKind::TreasurySpend { to: "nobody.boo".into(), amount: FANG }, "no one has claimed nobody.boo");
    refused(&mut t, param("warp_speed", 1), "unknown parameter");
    // Names resolve when proposed: the account voters saw is the one paid.
    t.tx(CAROL, Action::RegisterName { name: "carol".into() }).unwrap();
    let id = t.propose(ALICE, ProposalKind::TreasurySpend { to: "carol.boo".into(), amount: FANG });
    assert_eq!(t.p(id).kind, ProposalKind::TreasurySpend { to: addr(CAROL), amount: FANG });
    t.tx(CAROL, Action::TransferName { name: "carol".into(), to: addr(BOB) }).unwrap();
    t.vote(ALICE, id, true, 50_000, 1).unwrap();
    let (bob, carol) = (t.bal(BOB), t.bal(CAROL));
    t.finish(id);
    assert_eq!((t.bal(BOB), t.bal(CAROL)), (bob, carol + FANG));
    // Session keys can't propose or vote.
    let agent = keys(50);
    t.tx(ALICE, Action::AddKey { key_id: kid(50), label: "agent".into(), role: KeyRole::Session { limit: 1_000 * FANG, period_blocks: 100, expires_at: 1_000 } }).unwrap();
    for action in [Action::Propose { kind: text(), deposit: 50 * FANG, title: "x".into(), url: String::new() }, Action::Vote { proposal_id: id, aye: true, amount: FANG, conviction: 0 }, Action::Unlock {}] {
        assert!(t.tx_as(&agent, &addr(ALICE), action).unwrap_err().to_string().contains("session keys can only"));
    }
}

#[test]
fn merkle_trees_build_and_verify() {
    for n in 1..=9usize {
        let entries: Vec<(String, u64)> = (0..n).map(|i| (addr(100 + i as u8), (i as u64 + 1) * FANG)).collect();
        let (root, proofs) = build_distribution(3, &entries).unwrap();
        for ((a, amount), proof) in entries.iter().zip(&proofs) {
            assert_eq!(root_from_proof(distribution_leaf(3, a, *amount), proof), root, "{n} leaves");
            assert!(proof.len() <= (usize::BITS - (n - 1).leading_zeros()) as usize, "proofs are log2(n) long");
            assert_ne!(root_from_proof(distribution_leaf(3, a, amount + 1), proof), root);
            assert_ne!(root_from_proof(distribution_leaf(4, a, *amount), proof), root, "the id is in the leaf");
        }
    }
    assert!(build_distribution(1, &[(addr(1), 1), (addr(1), 2)]).unwrap_err().to_string().contains("twice"));
    assert!(build_distribution(1, &[]).is_err() && build_distribution(1, &[(addr(1), 0)]).is_err());
    // A leaf is never a node: domain separation.
    let (l, r) = (distribution_leaf(1, &addr(1), 1), distribution_leaf(1, &addr(2), 1));
    assert_eq!(merkle_parent(&l, &r), merkle_parent(&r, &l), "sorted pairs");
    assert_ne!(merkle_parent(&l, &r), distribution_leaf(1, &addr(1), 1));
    // Shared vectors (docs/GOVERNANCE.md; the TS helper must reproduce them).
    let a = crate::crypto::render_address("boo", &[7; 32]);
    let b = crate::crypto::render_address("boo", &[8; 32]);
    let c = crate::crypto::render_address("tboo", &[9; 32]);
    assert_eq!((a.as_str(), b.as_str(), c.as_str()), ("boo1qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qurswpc8qursz6hc9p", "boo1pqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyq66rhlm", "tboo1pyysjzgfpyysjzgfpyysjzgfpyysjzgfpyysjzgfpyysjzgfpyysj0amg2"));
    assert_eq!(hex::encode(distribution_leaf(7, &a, 1_000_000_000)), VECTOR_LEAF);
    let entries = [(a.clone(), 1_000_000_000), (b, 2_500_000_000), (c, 1)];
    let (root, proofs) = build_distribution(7, &entries).unwrap();
    assert_eq!(hex::encode(root), VECTOR_ROOT);
    let proofs: Vec<Vec<String>> = proofs.iter().map(|p| p.iter().map(hex::encode).collect()).collect();
    assert_eq!(proofs, VECTOR_PROOFS.map(|p| p.iter().map(|h| h.to_string()).collect::<Vec<_>>()));
}

/// The shared vectors the SDK's helper (`sdk-ts/src/distribution.ts`) checks:
/// `sdk-ts/test/distribution-vectors.json`, distribution 7 (the table in docs/GOVERNANCE.md) and
/// trees of 1 to 9 entries. Regenerate with `cargo test governance::tests::write_distribution_vectors -- --ignored`.
fn distribution_vectors() -> serde_json::Value {
    let tree = |id: u64, entries: &[(String, u64)]| {
        let (root, proofs) = build_distribution(id, entries).unwrap();
        serde_json::json!({
            "distribution_id": id,
            "entries": entries.iter().zip(&proofs).map(|((a, amount), p)| serde_json::json!({ "address": a, "amount": amount.to_string(), "leaf": hex::encode(distribution_leaf(id, a, *amount)), "proof": p.iter().map(hex::encode).collect::<Vec<_>>() })).collect::<Vec<_>>(),
            "root": hex::encode(root),
        })
    };
    let a = crate::crypto::render_address("boo", &[7; 32]);
    let b = crate::crypto::render_address("boo", &[8; 32]);
    let c = crate::crypto::render_address("tboo", &[9; 32]);
    let mut trees = vec![tree(7, &[(a, 1_000_000_000), (b, 2_500_000_000), (c, 1)])];
    for n in 1..=9u8 {
        trees.push(tree(1_000 + n as u64, &(0..n).map(|i| (addr(100 + i), (i as u64 + 1) * FANG + i as u64)).collect::<Vec<_>>()));
    }
    serde_json::json!({ "trees": trees })
}

fn distribution_vectors_file() -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    dir.ancestors().map(|d| d.join("sdk-ts/test/distribution-vectors.json")).find(|p| p.exists()).unwrap_or_else(|| dir.join("../sdk-ts/test/distribution-vectors.json"))
}

#[test]
fn distribution_vectors_match_the_sdk_file() {
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(distribution_vectors_file()).expect("sdk-ts/test/distribution-vectors.json")).unwrap();
    assert_eq!(file, distribution_vectors(), "regenerate with `cargo test governance::tests::write_distribution_vectors -- --ignored`");
    assert_eq!(file["trees"][0]["root"], VECTOR_ROOT);
}

#[test]
#[ignore]
fn write_distribution_vectors() {
    std::fs::write(distribution_vectors_file(), serde_json::to_string_pretty(&distribution_vectors()).unwrap() + "\n").unwrap();
}

// Distribution 7 over boo1qurswpc8…z6hc9p (bytes [7; 32]) 1 FANG, boo1pqyqszqg…66rhlm
// ([8; 32]) 2.5 FANG and tboo1pyysjzgf…j0amg2 ([9; 32]) 1 drop.
const VECTOR_LEAF: &str = "17031a8f14226964c8df49241c3572e7b17e3f492ae175f6b0b7e3d68e7b9515";
const VECTOR_ROOT: &str = "f1ccd2c1386b5f7bcffeb39b226cbef621a1f11c3ee919109d05bd8b5aeced3b";
const VECTOR_PROOFS: [&[&str]; 3] = [
    &["52da68dc24ed02c8ef334e1c3d788582aef1b1b949a7be762ddf94725cb84364", "b60bfc85af7ea12a7375f737eaf2ffed0f7768d57b9952d5994d16fb774b32c2"],
    &["ab68c5cbdeed677ec1daf469d1fe2818daca608866456091bef5c1382f186333"],
    &["17031a8f14226964c8df49241c3572e7b17e3f492ae175f6b0b7e3d68e7b9515", "b60bfc85af7ea12a7375f737eaf2ffed0f7768d57b9952d5994d16fb774b32c2"],
];

#[test]
fn distributions_pay_valid_claims_once_and_return_the_rest() {
    let mut t = T::new(&[]);
    let id = t.st.governance.next_proposal_id;
    let entries: Vec<(String, u64)> = [(ALICE, 100), (BOB, 50), (CAROL, 25), (DAVE, 10)].iter().map(|(s, f)| (addr(*s), f * FANG)).collect();
    let (root, proofs) = build_distribution(id, &entries).unwrap();
    let proof = |i: usize| proofs[i].iter().map(hex::encode).collect::<Vec<_>>();
    let claim = |amount: u64, proof: Vec<String>| Action::ClaimDistribution { distribution_id: id, amount: amount * FANG, proof };
    let claim_until = t.h + 40;
    t.propose(ALICE, ProposalKind::Distribution { distribution_id: id, merkle_root: hex::encode(root), total: 185 * FANG, claim_until });
    t.vote(ALICE, id, true, 50_000, 1).unwrap();
    assert!(t.err(ALICE, claim(100, proof(0))).contains("no distribution"));
    let community_before = t.st.account(&community("boo")).balance;
    t.finish(id);
    assert_eq!(t.st.account(&community("boo")).balance, community_before - 185 * FANG);
    assert_eq!(escrow(&t.st), 185 * FANG as u128);
    let alice = t.bal(ALICE);
    let fee = t.tx(ALICE, claim(100, proof(0))).unwrap();
    assert_eq!(t.bal(ALICE), alice + 100 * FANG - fee);
    assert!(t.err(ALICE, claim(100, proof(0))).contains("already claimed"));
    assert!(t.err(BOB, claim(51, proof(1))).contains("doesn't lead to"), "wrong amount");
    assert!(t.err(BOB, claim(25, proof(2))).contains("doesn't lead to"), "someone else's leaf");
    assert!(t.err(BOB, claim(50, vec!["zz".repeat(32)])).contains("lowercase hex"), "malformed proofs aren't admitted");
    t.tx(BOB, claim(50, proof(1))).unwrap();
    let d = &t.st.governance.distributions[&id];
    assert_eq!((d.claimed, d.claims), (150 * FANG, 2));
    assert_eq!(escrow(&t.st), 35 * FANG as u128);
    // Claims land through `claim_until`; after it the rest goes back to the community account.
    t.through(claim_until - 1);
    t.tx(CAROL, claim(25, proof(2))).unwrap();
    let community_mid = t.st.account(&community("boo")).balance;
    t.through(claim_until + 1);
    let d = t.st.governance.distributions[&id].clone();
    assert_eq!((d.closed_at, d.returned), (Some(claim_until + 1), 10 * FANG));
    assert_eq!(t.st.account(&community("boo")).balance, community_mid + 10 * FANG);
    assert!(t.st.governance.open_distributions.is_empty() && escrow(&t.st) == 0);
    t.tx(ALICE, Action::Transfer { to: addr(DAVE), amount: FANG }).unwrap();
    assert!(t.err(DAVE, claim(10, proof(3))).contains("closed"));
    // A tree that promises more than the total pays out only the total.
    let id = t.st.governance.next_proposal_id;
    let entries = vec![(addr(ALICE), 150 * FANG), (addr(BOB), 150 * FANG)];
    let (root, proofs) = build_distribution(id, &entries).unwrap();
    t.propose(ALICE, ProposalKind::Distribution { distribution_id: id, merkle_root: hex::encode(root), total: 200 * FANG, claim_until: t.h + 100 });
    t.vote(ALICE, id, true, 50_000, 1).unwrap();
    t.finish(id);
    let claim = |amount: u64, p: &Vec<[u8; 32]>| Action::ClaimDistribution { distribution_id: id, amount, proof: p.iter().map(hex::encode).collect() };
    t.tx(ALICE, claim(150 * FANG, &proofs[0])).unwrap();
    assert!(t.err(BOB, claim(150 * FANG, &proofs[1])).contains("has only 50 FANG left"));
}

/// Audit F45: a distribution's leaves bind the id its tree was built for, and the kind names that
/// id. A proposal that lands first (a front-running text proposal, or anyone's) used to shift the
/// id silently, so the distribution passed, escrowed its total and nobody could claim; now the
/// late proposal is refused before it takes a deposit, and a tree rebuilt for the new id goes
/// through and pays.
#[test]
fn a_distribution_built_for_another_id_is_refused_up_front() {
    let mut t = T::new(&[]);
    let id = t.st.governance.next_proposal_id;
    let entries = vec![(addr(CAROL), 11 * FANG)];
    let (root, _) = build_distribution(id, &entries).unwrap();
    let dist = |id: u64, root: [u8; 32]| ProposalKind::Distribution { distribution_id: id, merkle_root: hex::encode(root), total: 11 * FANG, claim_until: 500 };
    // Someone else's proposal takes id 0 first.
    assert_eq!(t.propose(BOB, text()), id);
    let (alice, community_before) = (t.bal(ALICE), t.st.account(&community("boo")).balance);
    let e = t.err(ALICE, Action::Propose { kind: dist(id, root), deposit: 50 * FANG, title: "drop".into(), url: String::new() });
    assert!(e.contains(&format!("built for id {id}, but this proposal gets id {}", id + 1)), "{e}");
    assert!(alice - t.bal(ALICE) < FANG, "only the fee: no deposit taken");
    assert_eq!(t.st.governance.next_proposal_id, id + 1);
    // Rebuilt for the id it gets, it passes and pays.
    let (root, proofs) = build_distribution(id + 1, &entries).unwrap();
    assert_eq!(t.propose(ALICE, dist(id + 1, root)), id + 1);
    t.vote(ALICE, id + 1, true, 50_000, 1).unwrap();
    t.finish(id + 1);
    assert_eq!(t.p(id + 1).status, ProposalStatus::Executed, "{}", t.p(id + 1).outcome);
    assert_eq!(t.st.account(&community("boo")).balance, community_before - 11 * FANG);
    let carol = t.bal(CAROL);
    let fee = t.tx(CAROL, Action::ClaimDistribution { distribution_id: id + 1, amount: 11 * FANG, proof: proofs[0].iter().map(hex::encode).collect() }).unwrap();
    assert_eq!(t.bal(CAROL), carol + 11 * FANG - fee);
}

/// A claimant with nothing to pay the fee with (a new account) pays it, and its account's
/// creation fee, out of the claim. A claim that doesn't check out (bad proof, too small to cover
/// the fees, already claimed) is no help: without a balance it isn't admitted at all, so it's
/// never free.
#[test]
fn claims_pay_their_own_fee_out_of_the_claim() {
    let mut t = T::new(&[]);
    let id = t.st.governance.next_proposal_id;
    let (dave, eve) = (addr(DAVE), addr(40));
    let entries = vec![(dave.clone(), 10 * FANG), (eve.clone(), 1_000)];
    let (root, proofs) = build_distribution(id, &entries).unwrap();
    let proof = |i: usize| proofs[i].iter().map(hex::encode).collect::<Vec<_>>();
    let claim = |amount: u64, proof: Vec<String>| Action::ClaimDistribution { distribution_id: id, amount, proof };
    t.propose(ALICE, ProposalKind::Distribution { distribution_id: id, merkle_root: hex::encode(root), total: 10 * FANG + 1_000, claim_until: t.h + 100 });
    t.vote(ALICE, id, true, 50_000, 1).unwrap();
    t.finish(id);
    assert!(!t.st.accounts.contains_key(&dave), "DAVE has never held anything");
    let (supply, burned) = (t.st.supply, t.st.burned);
    // A bad proof: not admitted (no account, no fee, nothing claimed).
    let e = t.err(DAVE, claim(10 * FANG, proof(1)));
    assert!(e.contains("insufficient balance for the fee") && e.contains("doesn't lead to"), "{e}");
    let e = t.err(DAVE, claim(11 * FANG, proof(0)));
    assert!(e.contains("the claim can't pay it"), "{e}");
    // Too small to cover the fees.
    let e = t.err(40, claim(1_000, proof(1)));
    assert!(e.contains("claims 0.000001 FANG"), "{e}");
    assert!(!t.st.accounts.contains_key(&dave) && !t.st.accounts.contains_key(&eve));
    assert_eq!((t.st.supply, t.st.burned, t.st.governance.distributions[&id].claims), (supply, burned, 0));
    // A valid claim opens the account and pays both fees out of the 10 FANG.
    let creation = t.st.chain_params.account_creation_fee;
    let charge = t.tx(DAVE, claim(10 * FANG, proof(0))).unwrap();
    assert_eq!(t.bal(DAVE), 10 * FANG - charge - creation);
    assert_eq!(t.st.account(&dave).nonce, 1);
    assert_eq!(t.st.burned, burned + charge + creation, "the base fee (no tip) and the creation fee burn");
    assert_eq!(t.st.governance.distributions[&id].claimed, 10 * FANG);
    // Claiming again, now with a balance: admitted, charged, and it fails.
    let before = t.bal(DAVE);
    assert!(t.err(DAVE, claim(10 * FANG, proof(0))).contains("already claimed"));
    assert!(t.bal(DAVE) < before && t.st.account(&dave).nonce == 2);
    t.next();
}

#[test]
fn multisig_accounts_need_their_threshold() {
    let mut t = T::new(&[]);
    let alice = addr(ALICE);
    let (k1, k2, k3) = (keys(ALICE), keys(11), keys(12));
    for seed in [11, 12] {
        t.tx(ALICE, Action::AddKey { key_id: kid(seed), label: format!("device {seed}"), role: KeyRole::Owner }).unwrap();
    }
    assert!(t.err(ALICE, Action::MultisigCreate { owners: vec![kid(ALICE), kid(11)], threshold: 2 }).contains("exactly this account's 3 owner key(s)"));
    assert!(t.err(ALICE, Action::MultisigCreate { owners: vec![kid(ALICE), kid(11), kid(12)], threshold: 4 }).contains("threshold must be 1–3"));
    assert!(t.err(BOB, Action::MultisigPropose { action: Box::new(Action::Unlock {}) }).contains("isn't a multisig"));
    t.tx(ALICE, Action::MultisigCreate { owners: vec![kid(12), kid(ALICE), kid(11)], threshold: 2 }).unwrap();
    // Owner keys can't act alone any more…
    let e = t.tx_as(&k2, &alice, Action::Transfer { to: addr(BOB), amount: FANG }).unwrap_err().to_string();
    assert!(e.contains("2-of-3 multisig: propose this"), "{e}");
    // …but they propose, and the threshold-th approval runs the action.
    let send = |fang: u64| Box::new(Action::Transfer { to: addr(BOB), amount: fang * FANG });
    let bob = t.bal(BOB);
    t.tx_as(&k2, &alice, Action::MultisigPropose { action: send(10) }).unwrap();
    assert_eq!(t.st.governance.multisigs[&alice].pending, BTreeSet::from([0]));
    assert!(t.tx_as(&k2, &alice, Action::MultisigApprove { id: 0 }).unwrap_err().to_string().contains("already approved"));
    t.tx_as(&k3, &alice, Action::MultisigApprove { id: 0 }).unwrap();
    assert_eq!(t.bal(BOB), bob + 10 * FANG);
    assert!(t.st.governance.multisig_proposals.is_empty() && t.st.governance.multisigs[&alice].pending.is_empty());
    assert!(t.tx_as(&k1, &alice, Action::MultisigApprove { id: 0 }).unwrap_err().to_string().contains("no open multisig action #0"));
    // A failing action fails its approval and stays open (retry after funding it).
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: send(1_000_000) }).unwrap();
    assert!(t.tx_as(&k3, &alice, Action::MultisigApprove { id: 1 }).unwrap_err().to_string().contains("multisig action #1: insufficient balance"));
    assert_eq!(t.st.governance.multisig_proposals[&1].approvals, BTreeSet::from([kid(ALICE)]));
    // Proposals expire; the next proposal clears them out.
    let expires = t.st.governance.multisig_proposals[&1].expires_at;
    t.through(expires);
    assert!(t.tx_as(&k3, &alice, Action::MultisigApprove { id: 1 }).unwrap_err().to_string().contains("expired at block"));
    // Another account's proposal can't be approved from here.
    t.tx(BOB, Action::MultisigCreate { owners: vec![kid(BOB)], threshold: 1 }).unwrap();
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: send(1) }).unwrap();
    assert!(!t.st.governance.multisig_proposals.contains_key(&1), "the expired one was pruned");
    assert!(t.tx(BOB, Action::MultisigApprove { id: 2 }).unwrap_err().to_string().contains("belongs to"));
    // A 1-of-1 multisig runs its proposals at once.
    let carol = t.bal(CAROL);
    t.tx(BOB, Action::MultisigPropose { action: Box::new(Action::Transfer { to: addr(CAROL), amount: FANG }) }).unwrap();
    assert_eq!(t.bal(CAROL), carol + FANG);
    // Session keys keep working, within their limits.
    let agent = keys(50);
    let add = Action::AddKey { key_id: kid(50), label: "agent".into(), role: KeyRole::Session { limit: 5 * FANG, period_blocks: 100, expires_at: t.h + 1_000 } };
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(add) }).unwrap();
    t.tx_as(&k2, &alice, Action::MultisigApprove { id: 4 }).unwrap();
    t.tx_as(&agent, &alice, Action::Transfer { to: addr(CAROL), amount: FANG }).unwrap();
    assert!(t.tx_as(&agent, &alice, Action::Transfer { to: addr(CAROL), amount: 5 * FANG }).unwrap_err().to_string().contains("limit"));
    assert!(t.tx_as(&agent, &alice, Action::MultisigApprove { id: 2 }).unwrap_err().to_string().contains("session keys can only"));
    // Keys can go, but never below the threshold.
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(Action::RemoveKey { key_id: kid(12) }) }).unwrap();
    t.tx_as(&k2, &alice, Action::MultisigApprove { id: 5 }).unwrap();
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(Action::RemoveKey { key_id: kid(11) }) }).unwrap();
    assert!(t.tx_as(&k2, &alice, Action::MultisigApprove { id: 6 }).unwrap_err().to_string().contains("fewer than the threshold"));
    // The threshold changes through the multisig itself.
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(Action::MultisigCreate { owners: vec![kid(ALICE), kid(11)], threshold: 1 }) }).unwrap();
    t.tx_as(&k2, &alice, Action::MultisigApprove { id: 7 }).unwrap();
    assert_eq!(t.st.governance.multisigs[&alice].threshold, 1);
    let bob = t.bal(BOB);
    t.tx_as(&k2, &alice, Action::MultisigPropose { action: send(2) }).unwrap();
    assert_eq!(t.bal(BOB), bob + 2 * FANG);
    // Governance through a multisig: the account proposes and votes.
    let id = t.st.governance.next_proposal_id;
    t.tx_as(&k2, &alice, Action::MultisigPropose { action: Box::new(Action::Propose { kind: text(), deposit: 50 * FANG, title: "from a multisig".into(), url: String::new() }) }).unwrap();
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(Action::Vote { proposal_id: id, aye: true, amount: 10_000 * FANG, conviction: 1 }) }).unwrap();
    assert_eq!((t.p(id).proposer.clone(), t.p(id).voters), (alice.clone(), 1));
    // At most 32 open proposals per account, and each of its 2 owner keys may have proposed 16.
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(Action::MultisigCreate { owners: vec![kid(ALICE), kid(11)], threshold: 2 }) }).unwrap();
    assert_eq!(t.st.governance.multisigs[&alice].pending, BTreeSet::from([2, 6]), "the unapproved send and the refused key removal");
    for _ in 2..16 {
        t.tx_as(&k1, &alice, Action::MultisigPropose { action: send(1) }).unwrap();
    }
    assert!(t.tx_as(&k1, &alice, Action::MultisigPropose { action: send(1) }).unwrap_err().to_string().contains("its share among 2 owner keys"));
    for _ in 0..16 {
        t.tx_as(&k2, &alice, Action::MultisigPropose { action: send(1) }).unwrap();
    }
    assert_eq!(t.st.governance.multisigs[&alice].pending.len(), MAX_MULTISIG_PENDING);
    assert!(t.tx_as(&k2, &alice, Action::MultisigPropose { action: send(1) }).unwrap_err().to_string().contains("32 open multisig proposals"));
}

/// Audit F14: one owner key of a 2-of-3 multisig can't fill the pending list and keep the others
/// from proposing (even its own removal): each key may have proposed only its share, ⌊32 / 3⌋.
/// Audit F17: what's left of its spam expires and is swept without anyone sending anything, and
/// a proposal's action is bounded (a validator registration fits, 32 KiB of evidence doesn't).
#[test]
fn one_owner_key_cant_freeze_a_multisig_and_expired_proposals_go() {
    let mut t = T::new(&[]);
    let alice = addr(ALICE);
    let (k1, k2, rogue) = (keys(ALICE), keys(11), keys(12));
    for seed in [11, 12] {
        t.tx(ALICE, Action::AddKey { key_id: kid(seed), label: format!("device {seed}"), role: KeyRole::Owner }).unwrap();
    }
    t.tx(ALICE, Action::MultisigCreate { owners: vec![kid(ALICE), kid(11), kid(12)], threshold: 2 }).unwrap();
    let junk = || Action::MultisigPropose { action: Box::new(Action::Unlock {}) };
    for _ in 0..MAX_MULTISIG_PENDING / 3 {
        t.tx_as(&rogue, &alice, junk()).unwrap();
    }
    let e = t.tx_as(&rogue, &alice, junk()).unwrap_err().to_string();
    assert!(e.contains("its share among 3 owner keys"), "{e}");
    // The honest keys still propose, and remove the rogue.
    let id = t.st.governance.next_multisig_id;
    t.tx_as(&k2, &alice, Action::MultisigPropose { action: Box::new(Action::RemoveKey { key_id: kid(12) }) }).unwrap();
    t.tx_as(&k1, &alice, Action::MultisigApprove { id }).unwrap();
    assert!(!owner_keys(&t.st, &alice).contains(&kid(12)));
    // Its proposals expire, and the sweep takes them out of the state.
    let expires = t.st.governance.multisig_proposals.values().map(|p| p.expires_at).max().unwrap();
    assert_eq!(t.st.governance.multisig_proposals.len(), 10);
    t.through(expires + 2);
    assert!(t.st.governance.multisig_proposals.is_empty() && t.st.governance.multisigs[&alice].pending.is_empty());
    // Bounded actions.
    let evidence = Action::ReportEquivocation { evidence: "ab".repeat(MAX_EVIDENCE_BYTES) };
    let e = t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(evidence) }).unwrap_err().to_string();
    assert!(e.contains(&format!("at most {MAX_MULTISIG_ACTION_BYTES} bytes")), "{e}");
    let node = keys(60);
    let register = Action::RegisterValidator { consensus_key: hex::encode(node.public_key()), p2p_addr: "validator.example.com:30303".into(), http_addr: "https://validator.example.com".into(), bond: 10 * FANG, pop: make_pop([60; 32], CHAIN, &alice, "validator.example.com:30303", "https://validator.example.com") };
    assert!(register.to_bytes().len() > 10 << 10);
    let id = t.st.governance.next_multisig_id;
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(register) }).unwrap();
    t.tx_as(&k2, &alice, Action::MultisigApprove { id }).unwrap();
    assert_eq!(t.st.validators[&node.address()].owner, alice);
}

/// Audit F47: once a vote closes its tally is final, and its vote records are swept, a batch a
/// block (each vote's lock lives on in the voter's account until `Unlock`).
#[test]
fn closed_votes_are_swept_and_their_tallies_stay() {
    let mut t = T::new(&[]);
    let id = t.propose(ALICE, text());
    for seed in [ALICE, BOB, CAROL] {
        t.vote(seed, id, true, 5_000, 1).unwrap();
    }
    // Hundreds more, written straight into the state (with the tally they add up to).
    let extra = VOTE_SWEEP as u64 * 2 + 7;
    for i in 0..extra {
        let voter = crate::crypto::render_address("boo", &crate::crypto::hash(&i.to_le_bytes()));
        t.st.governance.votes.insert(IdKey::new(id, &voter), VoteRecord { aye: false, amount: 1, conviction: 1, weight: 1, lock_amount: 1, locked_until: 0, council: None, at: t.h });
        let p = t.st.governance.proposals.get_mut(&id).unwrap();
        (p.nay_amount, p.nay_weight, p.voters) = (p.nay_amount + 1, p.nay_weight + 1, p.voters + 1);
    }
    let end = t.p(id).voting_end;
    t.through(end);
    let p = t.p(id).clone();
    assert_eq!((p.status, p.voters, p.aye_amount), (ProposalStatus::Queued, 3 + extra, (15_000 * FANG) as u128));
    let left = |t: &T| t.st.governance.votes.range(IdKey::new(id, "")..).take_while(|(k, _)| k.id == id).count();
    assert_eq!(left(&t), (3 + extra) as usize - VOTE_SWEEP, "the closing block swept a batch");
    t.next();
    t.next();
    assert_eq!(left(&t), 0);
    assert!(t.st.governance.closed_votes.is_empty());
    assert_eq!(t.p(id).voters, 3 + extra, "the tally stays");
    // The voters' locks are still there, and still lock.
    assert!(t.st.account(&addr(CAROL)).locks.iter().any(|l| l.id == format!("vote:{id}")));
    t.finish(id);
    assert_eq!(t.p(id).status, ProposalStatus::Executed);
}

/// Audit F44: treasury spends that pass within 30 days count together against
/// `treasury_large_bps`, so a large spend split into small proposals still needs ⅔.
#[test]
fn split_treasury_spends_count_together() {
    let mut t = T::new(&[]);
    let treasury = t.st.account(&t.st.treasury()).balance;
    let small = treasury / 100 - FANG;
    let ids: Vec<u64> = (0..5).map(|_| t.propose(ALICE, ProposalKind::TreasurySpend { to: addr(DAVE), amount: small })).collect();
    for id in &ids {
        t.vote(ALICE, *id, true, 50_000, 1).unwrap();
        t.vote(BOB, *id, false, 40_000, 1).unwrap();
    }
    t.finish(ids[4]);
    let status: Vec<ProposalStatus> = ids.iter().map(|id| t.p(*id).status).collect();
    assert_eq!(status, [ProposalStatus::Executed, ProposalStatus::Rejected, ProposalStatus::Rejected, ProposalStatus::Rejected, ProposalStatus::Rejected]);
    assert!(!t.p(ids[0]).tally.as_ref().unwrap().supermajority && t.p(ids[1]).tally.as_ref().unwrap().supermajority);
    assert_eq!(t.bal(DAVE), small);
    assert_eq!(recent_spends(&t.st, t.h), small as u128);
    // Thirty days later the window has moved on (aged here by hand: a month of blocks is long).
    for (at, _) in t.st.governance.recent_spends.values_mut() {
        *at = 0;
    }
    assert_eq!(recent_spends(&t.st, 30 * 86_400), 0);
    t.h = 30 * 86_400;
    t.next();
    let small = t.st.account(&t.st.treasury()).balance / 100 - FANG;
    let id = t.propose(ALICE, ProposalKind::TreasurySpend { to: addr(DAVE), amount: small });
    t.vote(ALICE, id, true, 50_000, 1).unwrap();
    t.vote(BOB, id, false, 40_000, 1).unwrap();
    t.finish(id);
    assert_eq!(t.p(id).status, ProposalStatus::Executed, "{}", t.p(id).outcome);
    assert_eq!(t.st.governance.recent_spends.keys().copied().collect::<Vec<_>>(), vec![id], "the old one was pruned");
}

/// GOV-2 and GOV-3: without admins, nobody but a hunt's creator moves its escrow, and the
/// former admin levers are proposals only.
/// SF-13 × SF-11: a validator whose owner is a multisig still signs its system txs with its
/// consensus key (it isn't one of the owner keys the multisig gates); `UpdateValidator`, which
/// changes the pool's terms, goes through the multisig like any owner action.
#[test]
fn a_multisig_owners_validator_still_signs_its_system_txs() {
    let mut t = T::new(&[]);
    t.tx(OWNER, Action::MultisigCreate { owners: vec![kid(OWNER)], threshold: 1 }).unwrap();
    assert!(t.err(OWNER, Action::Transfer { to: addr(ALICE), amount: FANG }).contains("multisig_propose"));
    let ck = keys(VALIDATORS[2]);
    // Admitted and charged to the owner; the action itself fails (there's no claim #9).
    let before = t.bal(OWNER);
    let e = t.tx_as(&ck, &addr(OWNER), Action::AttestClaim { claim_id: 9, ok: true, merge_sha: "ab".repeat(20), note: String::new() }).unwrap_err().to_string();
    assert!(!e.contains("multisig") && t.bal(OWNER) < before, "{e}");
    let e = t.tx_as(&ck, &addr(OWNER), Action::UpdateValidator { commission_bps: 100, p2p_addr: String::new(), http_addr: String::new() }).unwrap_err().to_string();
    assert!(e.contains("multisig_propose"), "{e}");
}

// ---- tips (audit F01, F04, F06): the tip is a payment to the proposer, not a burn

impl T {
    /// `tx_as` with a tip, in a block proposed by `proposer` (who gets the tip). `fee` covers
    /// exactly the base fee and the tip.
    fn tip_as(&mut self, k: &Keys, from: &str, action: Action, tip: u64, proposer: &str) -> Result<u64> {
        let mut tx = Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), from: from.into(), nonce: self.st.account(from).nonce, fee: 0, tip, valid_until: self.h + 10, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.fee = crate::econ::base_fee(&self.st, self.h, tx.size()) + tip;
        let base = tx.fee - tip;
        match self.st.apply(&tx, CHAIN, proposer, self.h)? {
            Outcome::Applied => Ok(base),
            Outcome::Failed(e) => bail!(e),
        }
    }

    /// The base fee of `action` from `from`, signed by `k`, in the current block.
    fn base(&self, k: &Keys, from: &str, action: Action) -> u64 {
        let tx = Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), from: from.into(), nonce: 0, fee: 0, tip: 0, valid_until: self.h, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        crate::econ::base_fee(&self.st, self.h, tx.size())
    }
}

/// F01 (a, b): a fully vesting founder used to sign a 0-FANG transfer to itself with a tip of
/// its whole balance, which a colluding proposer received liquid (the lock void). The tip is now
/// capped at `tip_cap_multiple` × the base fee, and must come from FANG the sender could
/// transfer: vesting and vote locks hold it like any transfer. The burned base fee may still come
/// from locked tokens.
#[test]
fn tips_come_from_transferable_fang_and_are_capped() {
    let mut t = T::new(&[]);
    let (founder, thief) = (keys(FOUNDER), addr(DAVE));
    let noop = || Action::Transfer { to: addr(FOUNDER), amount: 0 };
    assert!(t.err(FOUNDER, Action::Transfer { to: thief.clone(), amount: 500_000 * FANG }).contains("locked"));
    let (balance, base) = (t.bal(FOUNDER), t.base(&founder, &addr(FOUNDER), noop()));
    // The PoC: the whole balance as a tip. Refused, nothing charged.
    let e = t.tip_as(&founder, &addr(FOUNDER), noop(), balance - base, &thief).unwrap_err().to_string();
    assert!(e.contains("tip too high: at most 20× the base fee"), "{e}");
    // Within the cap, still not out of locked tokens.
    let e = t.tip_as(&founder, &addr(FOUNDER), noop(), 10 * base, &thief).unwrap_err().to_string();
    assert!(e.contains("the tip must come from transferable FANG"), "{e}");
    assert_eq!((t.bal(FOUNDER), t.bal(DAVE)), (balance, 0), "nothing moved, nothing charged");
    // Fees (the burned base fee) may still come from locked tokens.
    t.tip_as(&founder, &addr(FOUNDER), noop(), 0, &thief).unwrap();
    assert_eq!((t.bal(FOUNDER), t.bal(DAVE)), (balance - base, 0));
    // Liquid tokens on top of the vesting ones may tip.
    t.tx(ALICE, Action::Transfer { to: addr(FOUNDER), amount: FANG }).unwrap();
    let base = t.base(&founder, &addr(FOUNDER), noop());
    t.tip_as(&founder, &addr(FOUNDER), noop(), 20 * base, &thief).unwrap();
    assert_eq!(t.bal(DAVE), 20 * base, "the proposer got the tip");
    assert!(t.tip_as(&founder, &addr(FOUNDER), noop(), 20 * base + 1, &thief).unwrap_err().to_string().contains("tip too high"));
    // Vote locks hold tips too: CAROL votes all but five base fees' worth of what she holds.
    let id = t.propose(ALICE, text());
    let vote = |amount: u64| Action::Vote { proposal_id: id, aye: true, amount, conviction: 1 };
    let (carol, base) = (t.bal(CAROL), t.base(&keys(CAROL), &addr(CAROL), noop()));
    t.tx(CAROL, vote(carol - t.base(&keys(CAROL), &addr(CAROL), vote(0)) - 5 * base)).unwrap();
    let free = crate::econ::transferable(&t.st, &addr(CAROL), t.h);
    assert_eq!(free, 5 * base);
    let e = t.tip_as(&keys(CAROL), &addr(CAROL), noop(), free + 1, &thief).unwrap_err().to_string();
    assert!(e.contains("the tip must come from transferable FANG"), "{e}");
    let dave = t.bal(DAVE);
    t.tip_as(&keys(CAROL), &addr(CAROL), noop(), free, &thief).unwrap();
    assert_eq!(t.bal(DAVE), dave + free);
    // Governance sets the cap, within its bounds.
    assert!(t.err(BOB, Action::Propose { kind: param("tip_cap_multiple", 0), deposit: 50 * FANG, title: "no tips".into(), url: String::new() }).contains("must be 1–1000"));
    let id = t.propose(ALICE, param("tip_cap_multiple", 2));
    t.vote(ALICE, id, true, 10_000, 1).unwrap();
    t.finish(id);
    assert_eq!(t.st.chain_params.tip_cap_multiple, 2);
    let base = t.base(&keys(BOB), &addr(BOB), noop());
    assert!(t.tip_as(&keys(BOB), &addr(BOB), noop(), 2 * base + 1, &thief).unwrap_err().to_string().contains("at most 2× the base fee"));
    t.tip_as(&keys(BOB), &addr(BOB), noop(), 2 * base, &thief).unwrap();
}

/// F01 (b): one owner key of an M-of-N multisig used to empty the account through the tip of a
/// `MultisigPropose`, the proposal itself never reaching its threshold. A multisig's owner keys
/// act alone in such txs, so they carry no tip; session keys still tip within their limit.
#[test]
fn a_multisig_owner_key_cant_tip() {
    let mut t = T::new(&[]);
    let alice = addr(ALICE);
    let (k1, k2) = (keys(ALICE), keys(11));
    t.tx(ALICE, Action::AddKey { key_id: kid(11), label: "device".into(), role: KeyRole::Owner }).unwrap();
    t.tx(ALICE, Action::MultisigCreate { owners: vec![kid(ALICE), kid(11)], threshold: 2 }).unwrap();
    let propose = || Action::MultisigPropose { action: Box::new(Action::Transfer { to: addr(BOB), amount: 1 }) };
    let (balance, thief) = (t.bal(ALICE), addr(DAVE));
    for tip in [1, t.base(&k2, &alice, propose())] {
        let e = t.tip_as(&k2, &alice, propose(), tip, &thief).unwrap_err().to_string();
        assert!(e.contains("is a multisig: its owner keys' txs carry no tip"), "{e}");
    }
    assert_eq!((t.bal(ALICE), t.bal(DAVE)), (balance, 0));
    // Without a tip it's an ordinary proposal and approval.
    let proposed = t.tip_as(&k2, &alice, propose(), 0, &thief).unwrap();
    assert!(t.tip_as(&k1, &alice, Action::MultisigApprove { id: 0 }, 1, &thief).unwrap_err().to_string().contains("carry no tip"));
    let approved = t.tip_as(&k1, &alice, Action::MultisigApprove { id: 0 }, 0, &thief).unwrap();
    assert_eq!((t.bal(ALICE), t.bal(DAVE)), (balance - proposed - approved - 1, 0), "base fees and the approved drop");
    // A session key (approved by both) tips within its limit, which counts the tip.
    let agent = keys(50);
    let add = Action::AddKey { key_id: kid(50), label: "agent".into(), role: KeyRole::Session { limit: FANG, period_blocks: 100, expires_at: t.h + 1_000 } };
    t.tx_as(&k1, &alice, Action::MultisigPropose { action: Box::new(add) }).unwrap();
    t.tx_as(&k2, &alice, Action::MultisigApprove { id: 1 }).unwrap();
    t.tip_as(&agent, &alice, Action::Transfer { to: addr(BOB), amount: 1 }, 5, &thief).unwrap();
    assert_eq!(t.bal(DAVE), 5);
}

/// F04, F06 (and F01 c): a validator's consensus key signs its system txs for its owner (SF-13),
/// and the owner pays. It used to set any tip, so a stolen hot key moved the cold owner's whole
/// balance to the proposer (itself, in its own slot), through a multisig owner's threshold and
/// a vesting owner's lock alike. Those txs now carry no tip: the owner pays the base fee only.
#[test]
fn a_consensus_key_cant_tip_its_owners_fang() {
    let mut t = T::new(&[]);
    let (ck, owner, hot) = (keys(VALIDATORS[2]), addr(OWNER), addr(VALIDATORS[2]));
    let terms = || Action::UpdateValidator { commission_bps: 0, p2p_addr: String::new(), http_addr: String::new() };
    let attest = || Action::AttestClaim { claim_id: 9, ok: true, merge_sha: "ab".repeat(20), note: String::new() };
    let commit = || Action::BeaconCommit { epoch: 0, commitment: "cd".repeat(32) };
    let (balance, hot_balance) = (t.bal(OWNER), t.bal(VALIDATORS[2]));
    // F06's PoC: a no-op `UpdateValidator` with the owner's balance as the tip, in the hot key's slot.
    let base = t.base(&ck, &owner, terms());
    let e = t.tip_as(&ck, &owner, terms(), balance - base, &hot).unwrap_err().to_string();
    assert!(e.contains("tip too high"), "{e}");
    for (action, tip) in [(terms(), 20 * base), (terms(), 1), (attest(), base), (commit(), 1)] {
        let e = t.tip_as(&ck, &owner, action, tip, &hot).unwrap_err().to_string();
        assert!(e.contains("consensus key signs system txs") && e.contains("without a tip"), "{e}");
    }
    assert_eq!((t.bal(OWNER), t.bal(VALIDATORS[2])), (balance, hot_balance), "nothing moved, nothing charged");
    // Without a tip: the owner pays the base fee, the hot key's account gets nothing.
    t.tip_as(&ck, &owner, terms(), 0, &hot).unwrap();
    assert_eq!((t.bal(OWNER), t.bal(VALIDATORS[2])), (balance - base, hot_balance));
    // F04: a multisig owner, and an owner whose FANG is all vesting, fare the same.
    t.tx(OWNER, Action::MultisigCreate { owners: vec![kid(OWNER)], threshold: 1 }).unwrap();
    let e = t.tip_as(&ck, &owner, commit(), 1, &hot).unwrap_err().to_string();
    assert!(e.contains("without a tip"), "{e}");
    let vesting = crate::chain::Lock { id: crate::econ::VESTING_LOCK.into(), amount: crate::chain::LockAmount::Vesting { total: 1_000_000 * FANG, start: 0, cliff: 10_000, end: 20_000 }, reason: "operator grant".into() };
    t.st.accounts.get_mut(&owner).unwrap().locks.push(vesting);
    let e = t.tip_as(&ck, &owner, attest(), 1, &hot).unwrap_err().to_string();
    assert!(e.contains("without a tip"), "{e}");
    // The hot key's own account (its fee float) is its own to tip from.
    t.tip_as(&ck, &hot, Action::Transfer { to: addr(ALICE), amount: FANG }, 1, &addr(VALIDATORS[0])).unwrap();
}

#[test]
fn chains_without_admins_have_no_admin_levers() {
    let mut t = T::new(&[]);
    t.tx(ALICE, Action::CreateHunt { repo: "jackdishman/blockula".into(), issue: 1, amount: 20 * FANG, expires_at: 500, title: "a hunt".into() }).unwrap();
    t.tx(BOB, Action::FundHunt { hunt_id: 0, amount: 5 * FANG }).unwrap();
    for seed in [BOB, VALIDATORS[0]] {
        assert!(t.err(seed, Action::AwardHunt { hunt_id: 0, to: addr(seed), pr: 1, familiar_key: None }).contains("only the hunt's creator"));
        assert!(t.err(seed, Action::CancelHunt { hunt_id: 0 }).contains("governance refund"));
    }
    for action in [
        Action::SetRepo { repo: "a/b".into(), weight: 1 },
        Action::GrantSoul { to: addr(BOB), amount: SOUL, reason: "r".into() },
        Action::FlagSybil { account: addr(BOB), reason: "r".into() },
        Action::RejectClaim { claim_id: 0, reason: "r".into() },
        Action::Clawback { account: addr(BOB), reason: "r".into() },
    ] {
        let e = t.err(ALICE, action);
        assert!(e.contains("this chain has none: put it to a governance proposal"), "{e}");
    }
    t.tx(BOB, Action::LinkGithub { handle: "bob-gh".into(), gist_id: "abc".into() }).unwrap();
    assert!(t.err(ALICE, Action::UnlinkGithub { handle: "bob-gh".into() }).contains("only the linked account"));
    let carol = t.bal(CAROL);
    t.tx(ALICE, Action::AwardHunt { hunt_id: 0, to: addr(CAROL), pr: 1, familiar_key: None }).unwrap();
    assert_eq!(t.bal(CAROL), carol + 25 * FANG);
    assert!(t.st.poc.souls.is_empty(), "Soul comes only from the genesis ledger and GrantSoul proposals");
}

/// Governance records survive encoding (every record kind, through `State::from_records`), and
/// a journal undoes every governance change, dirty marks included.
#[test]
fn records_round_trip_and_journals_roll_back() {
    let mut t = T::new(&[("bootstrap_end_height", 1_000)]);
    let id = t.st.governance.next_proposal_id;
    let (root, proofs) = build_distribution(id, &[(addr(BOB), 5 * FANG), (addr(CAROL), 5 * FANG)]).unwrap();
    t.propose(ALICE, ProposalKind::Distribution { distribution_id: id, merkle_root: hex::encode(root), total: 10 * FANG, claim_until: 500 });
    t.vote(ALICE, id, true, 50_000, 3).unwrap();
    t.vote(VALIDATORS[0], id, true, 1, 0).unwrap();
    t.finish(id);
    t.tx(BOB, Action::ClaimDistribution { distribution_id: id, amount: 5 * FANG, proof: proofs[0].iter().map(hex::encode).collect() }).unwrap();
    t.tx(OWNER, Action::MultisigCreate { owners: vec![kid(OWNER)], threshold: 1 }).unwrap();
    t.tx(ALICE, Action::AddKey { key_id: kid(11), label: "laptop".into(), role: KeyRole::Owner }).unwrap();
    t.tx(ALICE, Action::MultisigCreate { owners: vec![kid(ALICE), kid(11)], threshold: 2 }).unwrap();
    t.tx(ALICE, Action::MultisigPropose { action: Box::new(Action::Propose { kind: ProposalKind::GrantSoul { to: addr(BOB), amount: SOUL, reason: "r".into() }, deposit: 50 * FANG, title: "t".into(), url: String::new() }) }).unwrap();
    // A vote still open (closed votes are swept).
    let open = t.propose(BOB, text());
    t.vote(CAROL, open, true, 1, 0).unwrap();
    t.next();
    let mut records = vec![];
    t.st.for_each_record(|n, k, v| records.push(Ok((n, k.to_vec(), v.to_vec()))));
    assert!((37..=42).all(|n| records.iter().any(|r| r.as_ref().is_ok_and(|r| r.0 == n))), "every governance namespace has records");
    let back = State::from_records(records).unwrap();
    assert_eq!(back.root(), t.st.root());
    back.check_invariants().unwrap();
    assert_eq!(*back.governance.multisig_proposals, *t.st.governance.multisig_proposals);
    assert_eq!(*back.governance.proposals, *t.st.governance.proposals);
    assert_eq!(*back.governance.distribution_claims, *t.st.governance.distribution_claims);
    // A journal over every kind of governance change undoes all of it.
    let root = t.st.full_root();
    let mut j = Journal::default();
    let txs = [
        (keys(BOB), addr(BOB), Action::Propose { kind: text(), deposit: 50 * FANG, title: "t".into(), url: String::new() }),
        (keys(BOB), addr(BOB), Action::Vote { proposal_id: id + 2, aye: true, amount: FANG, conviction: 1 }),
        (keys(CAROL), addr(CAROL), Action::ClaimDistribution { distribution_id: id, amount: 5 * FANG, proof: proofs[1].iter().map(hex::encode).collect() }),
        (keys(11), addr(ALICE), Action::MultisigApprove { id: 0 }),
        (keys(BOB), addr(BOB), Action::MultisigCreate { owners: vec![kid(BOB)], threshold: 1 }),
    ];
    for (k, from, action) in txs {
        let mut tx = Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), from: from.clone(), nonce: t.st.account(&from).nonce, fee: 0, tip: 0, valid_until: t.h + 10, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        assert_eq!(t.st.apply_j(&mut j, &tx, CHAIN, &addr(VALIDATORS[0]), t.h, true).unwrap(), Outcome::Applied, "{:?}", tx.action);
    }
    assert_eq!(t.st.governance.next_proposal_id, id + 4, "the approval proposed #{}", id + 3);
    t.st.rollback(j);
    assert_eq!(t.st.full_root(), root);
    assert_eq!(t.st.commit_block().unwrap().records, vec![], "nothing left to commit");
}

/// Random governance sequences keep the root and the invariants (checked after every block).
#[test]
fn random_governance_sequences_keep_the_invariants() {
    let mut t = T::new(&[("bootstrap_end_height", 150), ("governance_spend_height", 20)]);
    let users = [ALICE, BOB, CAROL, OWNER, VALIDATORS[0], VALIDATORS[1], FOUNDER];
    t.tx(BOB, Action::AddKey { key_id: kid(13), label: "two".into(), role: KeyRole::Owner }).unwrap();
    let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    // (distribution id, entries, proofs)
    type Tree = (u64, Vec<(String, u64)>, Vec<MerklePath>);
    let mut trees: Vec<Tree> = vec![];
    for _ in 0..400 {
        let r = next();
        let who = users[(r % users.len() as u64) as usize];
        let proposals = t.st.governance.next_proposal_id.max(1);
        let pid = (r >> 8) % proposals;
        let amt = 1 + (r >> 16) % 30_000;
        let _ = match (r >> 40) % 12 {
            0 => t.tx(who, Action::Propose { kind: match (r >> 20) % 4 { 0 => text(), 1 => param("max_validators", 10 + (r >> 24) % 50), 2 => ProposalKind::TreasurySpend { to: addr(DAVE), amount: amt * FANG }, _ => param("issuance_pool_bps", (r >> 24) % 4_000) }, deposit: 50 * FANG, title: "random".into(), url: String::new() }),
            1 => {
                let id = t.st.governance.next_proposal_id;
                let entries: Vec<(String, u64)> = users.iter().take(1 + (r >> 20) as usize % 6).map(|s| (addr(*s), (1 + (r >> 30) % 9) * FANG)).collect();
                let (root, proofs) = build_distribution(id, &entries).unwrap();
                let total = entries.iter().map(|e| e.1).sum::<u64>() - (r >> 50) % 2 * FANG;
                let res = t.tx(ALICE, Action::Propose { kind: ProposalKind::Distribution { distribution_id: id, merkle_root: hex::encode(root), total, claim_until: t.h + 20 + (r >> 33) % 40 }, deposit: 50 * FANG, title: "drop".into(), url: String::new() });
                if res.is_ok() {
                    trees.push((id, entries, proofs));
                }
                res
            }
            2..=5 => {
                // Mostly on a proposal still voting, with what the voter can vote with.
                let voting: Vec<u64> = t.st.governance.open_proposals.iter().copied().filter(|id| t.st.governance.proposals[id].status == ProposalStatus::Voting).collect();
                let id = if voting.is_empty() || r % 5 == 0 { pid } else { voting[(r >> 8) as usize % voting.len()] };
                let can = crate::econ::voting_balance(&t.st, &addr(who), t.h) / FANG;
                t.vote(who, id, (r >> 21) % 3 != 0, 1 + amt % can.max(1), ((r >> 25) % 7) as u8)
            }
            6 => t.tx(who, Action::Unlock {}),
            7 if trees.iter().any(|tr| t.st.governance.open_distributions.contains(&tr.0)) => {
                let open: Vec<_> = trees.iter().filter(|tr| t.st.governance.open_distributions.contains(&tr.0)).collect();
                let (id, entries, proofs) = open[(r >> 12) as usize % open.len()];
                let i = (r >> 18) as usize % entries.len();
                let seed = users.iter().copied().find(|s| addr(*s) == entries[i].0).unwrap();
                t.tx(seed, Action::ClaimDistribution { distribution_id: *id, amount: entries[i].1, proof: proofs[i].iter().map(hex::encode).collect() })
            }
            8 => t.tx(BOB, Action::MultisigCreate { owners: vec![kid(BOB), kid(13)], threshold: 1 + (r >> 20) as u8 % 2 }),
            9 => t.tx_as(&keys(if r % 2 == 0 { BOB } else { 13 }), &addr(BOB), Action::MultisigPropose { action: Box::new(Action::Transfer { to: addr(CAROL), amount: FANG }) }),
            10 => t.tx_as(&keys(if r % 2 == 0 { BOB } else { 13 }), &addr(BOB), Action::MultisigApprove { id: (r >> 8) % t.st.governance.next_multisig_id.max(1) }),
            _ => {
                for _ in 0..1 + (r >> 20) % 4 {
                    t.next();
                }
                Ok(0)
            }
        };
    }
    t.through(t.h + 200);
    let g = &t.st.governance;
    let voters: u64 = g.proposals.values().map(|p| p.voters).sum();
    assert!(g.next_proposal_id > 10 && voters > 10, "the sequence exercised governance ({} proposals, {voters} votes)", g.next_proposal_id);
    assert!(g.votes.is_empty() && g.closed_votes.is_empty(), "every closed vote was swept");
    assert!(g.proposals.values().any(|p| p.status == ProposalStatus::Executed) && g.proposals.values().any(|p| p.status == ProposalStatus::Rejected));
    assert!(!g.distribution_claims.is_empty() && !g.multisigs.is_empty());
    assert!(g.open_proposals.is_empty() && g.open_distributions.is_empty() && escrow(&t.st) == 0);
}
