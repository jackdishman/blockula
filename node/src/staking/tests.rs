//! Staking's rules, each with its adversarial cases: share math, delegation and unbonding,
//! rewards and commission, slashing (pools, unbonding and redelegation entries), evidence
//! verification and its golden vector, set selection and the churn limit, downtime and jailing,
//! the junk charge, commission bounds, the insider cap, locked rewards, SF-13 and the admin levers.

use super::*;
use crate::chain::{Allocation, Genesis, Lock, LockAmount, Operator, Outcome, ValidatorInfo, Vesting};
use crate::crypto::Keys;
use crate::pq::PrivateKey;
use commonware_codec::Encode as _;
use commonware_consensus::simplex::types::{ConflictingFinalize, ConflictingNotarize, Finalize, Notarize, Nullify, NullifyFinalize, Proposal};
use commonware_consensus::types::{Epoch, Round, View};

const FANG: u64 = DROPS_PER_FANG;
const EPOCH: u64 = 10;
const CHAIN: &str = "stake-test";

fn keys(seed: u8) -> Keys {
    Keys::from_seed([seed; 32])
}

/// Validator `i`'s consensus key seed, and its owner's.
fn vseed(i: u8) -> u8 {
    i + 1
}
fn owner(i: u8) -> Keys {
    keys(i + 101)
}
fn vaddr(i: u8) -> String {
    keys(vseed(i)).address()
}

/// A chain on the state machine alone: genesis validators with operators, accounts, parameter
/// overrides; txs apply at `height`, `block()` runs the end-of-block steps like execution does
/// (views advance one per block unless `skip` says how many to nullify first) and checks the
/// root and the invariants.
struct T {
    st: State,
    height: u64,
    epoch: u64,
    view: u64,
    /// Views to nullify before the next block.
    skip: u64,
    /// Sizes of txs the next block carried but couldn't admit.
    skipped: Vec<u64>,
    /// Bytes of every tx the next block carried (econ's fullness), skipped ones included.
    tx_bytes: u64,
}

struct Setup {
    /// Self-bonds of genesis validators 0, 1, … (commission 10% each).
    validators: Vec<u64>,
    accounts: Vec<(String, u64, Option<Vesting>)>,
    params: Vec<(&'static str, u64)>,
    admins: Vec<String>,
}

impl Default for Setup {
    fn default() -> Self {
        Self { validators: vec![10_000 * FANG; 4], accounts: vec![], params: vec![], admins: vec![] }
    }
}

impl Setup {
    fn genesis(&self) -> Genesis {
        let mut allocations = vec![];
        let mut validators = vec![];
        let mut operators = vec![];
        for (i, bond) in self.validators.iter().enumerate() {
            let i = i as u8;
            let (v, o) = (keys(vseed(i)), owner(i));
            let (p2p, http) = (format!("127.0.0.1:{}", 7100 + i as u16), format!("127.0.0.1:{}", 7200 + i as u16));
            validators.push(ValidatorInfo { address: v.address(), public_key: hex::encode(v.public_key()), p2p_addr: p2p.clone(), http_addr: http.clone() });
            allocations.push(Allocation { address: o.address(), amount: bond + 1_000 * FANG, vesting: None, label: None });
            operators.push(Operator { owner: o.address(), consensus_key: hex::encode(v.public_key()), pop: crate::chain::make_pop([vseed(i); 32], CHAIN, &o.address(), &p2p, &http), p2p_addr: p2p, http_addr: http, self_bond: *bond, commission_bps: 1_000, provider: None, asn: None, country: None, entity: None });
        }
        for (address, amount, vesting) in &self.accounts {
            allocations.push(Allocation { address: address.clone(), amount: *amount, vesting: vesting.clone(), label: Some("test".into()) });
        }
        let mut params = BTreeMap::from([("min_self_bond".to_string(), 1_000 * FANG), ("unbonding_blocks".to_string(), 100), ("evidence_max_age_epochs".to_string(), 10), ("jail_blocks".to_string(), 30)]);
        for (k, v) in &self.params {
            params.insert(k.to_string(), *v);
        }
        Genesis {
            chain_id: CHAIN.into(),
            token: "FANG".into(),
            decimals: DECIMALS,
            block_time_ms: 1000,
            validators,
            faucet: false,
            created_at_ms: 0,
            allocations,
            epoch_blocks: EPOCH,
            validator_admins: self.admins.clone(),
            contribution: None,
            address_prefix: "boo".into(),
            param_overrides: params,
            operators,
            reserved_names: vec![],
            soul_ledger: vec![],
            features: vec![],
            bootstrap_excluded: vec![],
        }
    }

    fn start(self) -> T {
        let g = self.genesis();
        g.check().unwrap();
        T { st: without_issuance(g.initial_state()), height: 1, epoch: 0, view: 0, skip: 0, skipped: vec![], tx_bytes: 0 }
    }
}

/// Staking's rules in isolation: nothing is minted at epoch boundaries (rewards come from
/// `distribute_rewards`, called directly where a test needs them; econ's tests cover issuance).
/// The issuance year starts with a zero base supply, so each epoch's mint is zero.
fn without_issuance(mut st: State) -> State {
    st.econ.issuance = crate::econ::Issuance { year: 1, base_supply: 0, rate_ppm: st.chain_params.issuance_initial_ppm };
    st
}

impl T {
    fn tx_from(&self, k: &Keys, from: &str, action: Action) -> Tx {
        // The state machine alone doesn't check the chain instance (admission over HTTP does).
        let mut tx = Tx { chain_id: CHAIN.into(), genesis_hash: "00".repeat(32), from: from.into(), nonce: self.st.account(from).nonce, fee: 0, tip: 0, valid_until: self.height + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
        tx.fee = tx.charge().unwrap();
        tx.sign(k)
    }

    /// `k` acting for its own account; `Err` if it can't even be admitted.
    fn run(&mut self, k: &Keys, action: Action) -> Result<Outcome> {
        let tx = self.tx_from(k, &k.address(), action);
        self.apply(&tx)
    }

    fn apply(&mut self, tx: &Tx) -> Result<Outcome> {
        self.tx_bytes += tx.size();
        self.st.apply(tx, CHAIN, &vaddr(0), self.height)
    }

    fn ok(&mut self, k: &Keys, action: Action) {
        let what = format!("{action:?}");
        assert_eq!(self.run(k, action).unwrap(), Outcome::Applied, "{what}");
    }

    /// The reason an admitted tx's action failed.
    fn fails(&mut self, k: &Keys, action: Action) -> String {
        match self.run(k, action) {
            Ok(Outcome::Failed(r)) => r,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// Ends the block at `height` the way execution does.
    fn block(&mut self) {
        let h = self.height;
        let epoch = self.st.params.epoch_of(h);
        if epoch != self.epoch {
            (self.epoch, self.view) = (epoch, 0);
        }
        let parent_view = self.view;
        let view = parent_view + 1 + std::mem::take(&mut self.skip);
        self.view = view;
        let proposer = self.leader(epoch, view);
        let skipped = std::mem::take(&mut self.skipped);
        let tx_bytes = std::mem::take(&mut self.tx_bytes) + skipped.iter().sum::<u64>();
        self.st.end_block(&BlockInfo { height: h, hash: &format!("{h:064x}"), proposer: &proposer, view, parent_view, skipped: &skipped, tx_bytes });
        self.st.commit_block().unwrap();
        self.st.debug_check(h);
        self.height += 1;
    }

    fn blocks(&mut self, n: u64) {
        for _ in 0..n {
            self.block();
        }
    }

    /// Runs blocks through the end of the current epoch (at least one).
    fn finish_epoch(&mut self) {
        loop {
            self.block();
            if self.st.params.is_boundary(self.height - 1) {
                return;
            }
        }
    }

    /// Runs blocks until `victim` has missed `n` more of its leader slots: its views are
    /// nullified, everyone else's produce blocks.
    /// Runs blocks in which `victim` misses its leader slots: `n` of them, or fewer if it leaves
    /// the set first. Returns how many it missed.
    fn starve(&mut self, victim: &str, n: u64) -> u64 {
        let mut missed = 0;
        while missed < n {
            let epoch = self.st.params.epoch_of(self.height);
            if !self.set(epoch).iter().any(|v| v == victim) {
                break;
            }
            let mut view = if epoch != self.epoch { 1 } else { self.view + 1 };
            while missed < n && self.leader(epoch, view) == victim {
                self.skip += 1;
                view += 1;
                missed += 1;
            }
            self.block();
        }
        missed
    }

    /// The leader's index in `leaders(epoch)`, elected as consensus does (the beacon's shuffle).
    fn idx(&self, epoch: u64, view: u64) -> usize {
        leader_index(crate::beacon::elector_seed(&self.st, epoch), epoch, view, leaders(&self.st, epoch).len())
    }

    fn leader(&self, epoch: u64, view: u64) -> String {
        leaders(&self.st, epoch)[self.idx(epoch, view)].clone()
    }

    fn set(&self, epoch: u64) -> Vec<String> {
        self.st.active_sets.get(&epoch).cloned().unwrap_or_default()
    }

    fn balance(&self, a: &str) -> u64 {
        self.st.account(a).balance
    }

    fn stake(&self, v: &str) -> u64 {
        self.st.staking.pools[v].stake
    }

    fn shares(&self, delegator: &str, v: &str) -> u128 {
        self.st.staking.delegations.get(&dkey(delegator, v)).map_or(0, |d| d.shares)
    }
}

/// Signs a vote of validator `i` in `epoch` against the set the state holds for it.
fn scheme_of(t: &T, i: u8, epoch: u64) -> MlDsaScheme {
    let set = participants(&t.st, epoch).unwrap();
    MlDsaScheme::signer(&crate::consensus::namespace(CHAIN), set, PrivateKey::from_seed_bytes([vseed(i); 32])).expect("in the set")
}

fn proposal(epoch: u64, view: u64, digest: u8) -> Proposal<Digest> {
    Proposal::new(Round::new(Epoch::new(epoch), View::new(view)), View::new(view - 1), Digest([digest; 32]))
}

/// Validator `i` notarizing two different blocks in (epoch, view): hex evidence.
fn double_notarize(t: &T, i: u8, epoch: u64, view: u64) -> String {
    let s = scheme_of(t, i, epoch);
    let a = Notarize::sign(&s, proposal(epoch, view, 1)).unwrap();
    let b = Notarize::sign(&s, proposal(epoch, view, 2)).unwrap();
    hex::encode(Activity::ConflictingNotarize(ConflictingNotarize::new(a, b)).encode())
}

// ------------------------------------------------------------------------------------- math

#[test]
fn mul_div_is_exact_with_256_bit_products() {
    let mut x = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..2_000 {
        let (a, b, d) = (next() as u128, next() as u128, (next() as u128).max(1));
        assert_eq!(mul_div(a, b, d), Some(a * b / d));
        assert_eq!(mul_div_ceil(a, b, d), Some((a * b).div_ceil(d)));
    }
    // Products past 128 bits.
    assert_eq!(mul_div(1 << 100, 1 << 100, 1 << 90), Some(1 << 110));
    assert_eq!(mul_div(u128::MAX, u128::MAX, u128::MAX), Some(u128::MAX));
    assert_eq!(mul_div_rem(u128::MAX, 3, 7), Some(((u128::MAX / 7) * 3 + (u128::MAX % 7) * 3 / 7, (u128::MAX % 7) * 3 % 7)));
    assert_eq!(mul_div(u128::MAX, 2, 1), None, "the quotient doesn't fit");
    assert_eq!(mul_div(1, 1, 0), None);
    assert_eq!(mul_div_ceil(u128::MAX, 1, 1), Some(u128::MAX));
    assert_eq!(mul_div_ceil(7, 3, 2), Some(11));
    assert_eq!(bps_of(u64::MAX, 10_000), u64::MAX);
    assert_eq!(bps_of(10_000, 99_999), 10_000, "bps cap at 100%");
}

/// Random delegations, sales, rewards, slashes and junk charges on one pool: nobody ever gets
/// out more than the pool held (rounding favours the pool), and the pool's shares stay its
/// delegations'.
#[test]
fn share_math_never_creates_fang() {
    let accounts: Vec<Keys> = (200..210).map(keys).collect();
    let mut t = Setup { accounts: accounts.iter().map(|k| (k.address(), 1_000_000 * FANG, None)).collect(), ..Default::default() }.start();
    let v = vaddr(0);
    let mut seed = 7u64;
    let mut rnd = |n: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % n.max(1)
    };
    let supply = t.st.supply;
    let mut minted = 0u64;
    for step in 0..400 {
        let k = &accounts[rnd(accounts.len() as u64) as usize];
        let mut j = Journal::default();
        match rnd(5) {
            0 | 1 => {
                let _ = t.run(k, Action::Delegate { validator: v.clone(), amount: FANG + rnd(50_000 * FANG) });
            }
            2 => {
                let held = t.shares(&k.address(), &v);
                if held > 0 {
                    let s = (1 + rnd(held as u64)).min(held as u64);
                    let before = t.stake(&v);
                    let worth = t.st.staking.pools[&v].value(s as u128);
                    t.ok(k, Action::Undelegate { validator: v.clone(), shares: s });
                    assert_eq!(t.stake(&v), before - worth, "a sale takes exactly what the shares were worth");
                }
            }
            3 => {
                // Issuance into the pool (the caller mints it, as econ::issue does).
                let r = rnd(1_000 * FANG);
                t.st.j_mint(&mut j, r);
                let paid = distribute_rewards(&mut t.st, &mut j, r, t.height);
                t.st.j_burn(&mut j, r - paid);
                minted += paid;
            }
            _ => {
                let cut = slash(&mut t.st, &mut j, &v, t.height, 1 + rnd(300));
                t.st.j_burn(&mut j, cut);
                let taken = take_from_delegation(&mut t.st, &mut j, &owner(0).address(), &v, rnd(FANG));
                t.st.j_burn(&mut j, taken);
            }
        }
        t.st.check_invariants().unwrap_or_else(|e| panic!("step {step}: {e}"));
        let p = &t.st.staking.pools[&v];
        let claims: u128 = t.st.staking.delegations.iter().filter(|(k, _)| k.ends_with(&format!("/{v}"))).map(|(_, d)| p.value(d.shares) as u128).sum();
        assert!(claims <= p.stake as u128, "step {step}: shares are worth {claims}, the pool holds {}", p.stake);
    }
    // Everything still adds up: what was minted is all that's new.
    assert!(t.st.supply <= supply + minted);
    t.block();
}

// ----------------------------------------------------------------------- delegation basics

#[test]
fn delegate_undelegate_and_withdraw() {
    let alice = keys(200);
    let mut t = Setup { accounts: vec![(alice.address(), 10_000 * FANG, None)], ..Default::default() }.start();
    let (v, a) = (vaddr(1), alice.address());
    // A first delegation buys a share per drop (the pool's rate is still 1).
    assert!(t.fails(&alice, Action::Delegate { validator: v.clone(), amount: FANG - 1 }).contains("at least 1 FANG"));
    assert!(t.fails(&alice, Action::Delegate { validator: keys(77).address(), amount: FANG }).contains("no validator"));
    t.ok(&alice, Action::Delegate { validator: v.clone(), amount: 500 * FANG });
    assert_eq!(t.shares(&a, &v), 500 * FANG as u128);
    assert_eq!((t.stake(&v), holdings(&t.st, &a)), (10_500 * FANG, (500 * FANG, 0)));
    // Top-ups may be small.
    t.ok(&alice, Action::Delegate { validator: v.clone(), amount: 1 });
    t.block();
    // Selling more than you hold, or from a pool you're not in, fails.
    assert!(t.fails(&alice, Action::Undelegate { validator: v.clone(), shares: 600 * FANG }).contains("not 600"));
    assert!(t.fails(&alice, Action::Undelegate { validator: vaddr(2), shares: 1 }).contains("no stake"));
    t.ok(&alice, Action::Undelegate { validator: v.clone(), shares: 200 * FANG });
    let started = t.height;
    assert_eq!(holdings(&t.st, &a), (300 * FANG + 1, 200 * FANG));
    // 100 blocks of unbonding, stretched to when evidence against epoch 0 stops being accepted
    // (10 epochs back: through epoch 10, which ends at block 109).
    let release = (started + 100).max(110);
    assert_eq!(release, 110);
    assert!(t.fails(&alice, Action::WithdrawUnbonded {}).contains(&format!("released at block {release}")));
    while t.height < release - 1 {
        t.block();
    }
    assert!(t.fails(&alice, Action::WithdrawUnbonded {}).contains("released at block"));
    t.block();
    let before = t.balance(&a);
    let withdraw = t.tx_from(&alice, &a, Action::WithdrawUnbonded {});
    assert_eq!(t.apply(&withdraw).unwrap(), Outcome::Applied);
    assert_eq!(t.balance(&a), before + 200 * FANG - withdraw.charge().unwrap());
    assert_eq!(holdings(&t.st, &a), (300 * FANG + 1, 0));
    assert!(t.fails(&alice, Action::WithdrawUnbonded {}).contains("nothing unbonding"));
    // Selling everything removes the delegation.
    t.ok(&alice, Action::Undelegate { validator: v.clone(), shares: 300 * FANG + 1 });
    assert!(!t.st.staking.delegations.contains_key(&dkey(&a, &v)));
    t.block();
}

#[test]
fn unbonding_entries_are_capped_per_delegator() {
    let alice = keys(200);
    let mut t = Setup { accounts: vec![(alice.address(), 10_000 * FANG, None)], ..Default::default() }.start();
    let v = vaddr(0);
    t.ok(&alice, Action::Delegate { validator: v.clone(), amount: 100 * FANG });
    for _ in 0..MAX_ENTRIES {
        t.ok(&alice, Action::Undelegate { validator: v.clone(), shares: 1 });
    }
    assert!(t.fails(&alice, Action::Undelegate { validator: v.clone(), shares: 1 }).contains("withdraw the finished ones"));
    assert!(t.fails(&alice, Action::Redelegate { from_validator: v.clone(), to_validator: vaddr(1), shares: 1 }).contains("withdraw the finished ones"));
    t.block();
}

// ------------------------------------------------------------------- rewards and commission

#[test]
fn rewards_compound_and_commission_is_minted_to_the_owner() {
    let alice = keys(200);
    let mut t = Setup { accounts: vec![(alice.address(), 100_000 * FANG, None)], ..Default::default() }.start();
    let (v0, v1, a) = (vaddr(0), vaddr(1), alice.address());
    t.ok(&alice, Action::Delegate { validator: v0.clone(), amount: 10_000 * FANG });
    t.block();
    // 4 pools: v0 holds 20,000 FANG, the others 10,000 each. 50,000 FANG of issuance.
    let mut j = Journal::default();
    let issued = 50_000 * FANG;
    t.st.j_mint(&mut j, issued);
    let paid = distribute_rewards(&mut t.st, &mut j, issued, t.height);
    assert_eq!(paid, issued, "divides evenly");
    assert_eq!((t.stake(&v0), t.stake(&v1)), (40_000 * FANG, 20_000 * FANG));
    // Commission (10%) is minted to the owner as shares: alice's half of v0 earned 90% of
    // 10,000; the owner its own 9,000 plus 2,000 of commission.
    // Rounding favours the pool: the drop the owner's new shares fall short of stays in it.
    assert_eq!(delegation_value(&t.st, &a, &v0), 19_000 * FANG);
    assert_eq!(self_bond(&t.st, &v0), 21_000 * FANG - 1);
    assert_eq!(t.st.staking.pools[&v0].rewards, 20_000 * FANG);
    // A jailed validator's pool gets nothing; the remainder is the caller's to burn.
    jail(&mut t.st, &mut j, &v1, t.height, 0, "test");
    t.st.j_mint(&mut j, 1_001);
    let paid = distribute_rewards(&mut t.st, &mut j, 1_001, t.height);
    assert_eq!((paid, t.stake(&v1)), (1_000, 20_000 * FANG), "500 + 250 + 250, none for the jailed pool");
    t.st.j_burn(&mut j, 1_001 - paid);
    t.block();
}

// --------------------------------------------------------------------------------- slashing

#[test]
fn double_signing_slashes_the_pool_its_exits_and_pays_the_reporter() {
    let (alice, bob, carol, reporter) = (keys(200), keys(201), keys(202), keys(203));
    let accounts = [&alice, &bob, &carol, &reporter].iter().map(|k| (k.address(), 100_000 * FANG, None)).collect();
    let mut t = Setup { accounts, ..Default::default() }.start();
    let v = vaddr(2);
    for k in [&alice, &bob, &carol] {
        t.ok(k, Action::Delegate { validator: v.clone(), amount: 10_000 * FANG });
    }
    // Alice leaves before the epoch of the fault; bob leaves during it; carol moves to v3.
    t.ok(&alice, Action::Undelegate { validator: v.clone(), shares: 10_000 * FANG });
    t.finish_epoch();
    t.blocks(3);
    t.ok(&bob, Action::Undelegate { validator: v.clone(), shares: 10_000 * FANG });
    t.ok(&carol, Action::Redelegate { from_validator: v.clone(), to_validator: vaddr(3), shares: 10_000 * FANG });
    t.block();
    let (supply, burned, rep0) = (t.st.supply, t.st.burned, t.balance(&reporter.address()));
    let pool0 = t.stake(&v);
    let evidence = double_notarize(&t, 2, 1, 4);
    assert_eq!(verify_evidence(&t.st, CHAIN, &hex::decode(&evidence).unwrap(), t.height).unwrap(), Fault { kind: CONFLICTING_NOTARIZE, validator: v.clone(), epoch: 1, view: 4 });
    let report = t.tx_from(&reporter, &reporter.address(), Action::ReportEquivocation { evidence: evidence.clone() });
    assert!(t.st.describe(&report, None).ends_with(&format!("reported validator {} for signing two different blocks in epoch 1, view 4", short(&v))));
    assert_eq!(t.apply(&report).unwrap(), Outcome::Applied);
    // 5% of the pool, of bob's unbonding (it left during epoch 1) and of carol's stake in v3
    // (she moved during it too); none of alice's (she left before).
    let slashed = pool0 / 20 + 500 * FANG + 500 * FANG;
    assert_eq!(t.stake(&v), pool0 - pool0 / 20);
    assert_eq!(holdings(&t.st, &bob.address()), (0, 9_500 * FANG));
    assert_eq!(holdings(&t.st, &alice.address()), (0, 10_000 * FANG));
    assert_eq!(delegation_value(&t.st, &carol.address(), &vaddr(3)), 9_500 * FANG);
    // 10% to the reporter, the rest burned; the validator is tombstoned for good.
    assert_eq!(t.balance(&reporter.address()), rep0 - report.charge().unwrap() + slashed / 10);
    assert_eq!(t.st.supply, supply - min_fee(report.size()) - (slashed - slashed / 10));
    assert_eq!(t.st.burned, burned + min_fee(report.size()) + slashed - slashed / 10);
    assert_eq!(t.st.validators[&v].status, ValidatorStatus::Tombstoned);
    assert!(!eligible(&t.st, &v));
    // Once per (validator, epoch, view): the same fault again is refused; another view slashes again.
    assert!(t.fails(&reporter, Action::ReportEquivocation { evidence }).contains("already punished"));
    let pool1 = t.stake(&v);
    t.ok(&reporter, Action::ReportEquivocation { evidence: double_notarize(&t, 2, 1, 5) });
    assert_eq!(t.stake(&v), pool1 - pool1 / 20);
    // Tombstoned: no new stake, no unjailing, out at the boundary (and never back).
    assert!(t.fails(&alice, Action::Delegate { validator: v.clone(), amount: FANG }).contains("tombstoned"));
    assert!(t.fails(&owner(2), Action::Unjail { validator: v.clone() }).contains("can never return"));
    t.finish_epoch();
    assert!(!t.set(2).contains(&v));
    t.finish_epoch();
    assert!(!t.set(3).contains(&v));
    // Its delegators still get out.
    t.ok(&owner(2), Action::Undelegate { validator: v.clone(), shares: t.shares(&owner(2).address(), &v) as u64 });
    t.block();
}

/// A redelegation can't outrun a slash: the moved stake answers for faults in epochs that began
/// before it moved, from the new pool or from where it went next.
#[test]
fn redelegated_stake_stays_slashable_and_cant_hop() {
    let (carol, reporter) = (keys(202), keys(203));
    let mut t = Setup { accounts: vec![(carol.address(), 100_000 * FANG, None), (reporter.address(), 1_000 * FANG, None)], ..Default::default() }.start();
    let (v2, v3) = (vaddr(2), vaddr(3));
    t.ok(&carol, Action::Delegate { validator: v2.clone(), amount: 10_000 * FANG });
    t.finish_epoch();
    t.block();
    t.ok(&carol, Action::Redelegate { from_validator: v2.clone(), to_validator: v3.clone(), shares: 10_000 * FANG });
    // No hop onward while the move is fresh…
    assert!(t.fails(&carol, Action::Redelegate { from_validator: v3.clone(), to_validator: vaddr(1), shares: FANG }).contains("can't move on before block"));
    // …and selling it right away doesn't dodge v2's slash either.
    t.ok(&carol, Action::Undelegate { validator: v3.clone(), shares: t.shares(&carol.address(), &v3) as u64 });
    t.block();
    t.ok(&reporter, Action::ReportEquivocation { evidence: double_notarize(&t, 2, 1, 2) });
    assert_eq!(holdings(&t.st, &carol.address()), (0, 9_500 * FANG));
    t.block();
}

// --------------------------------------------------------------------------------- evidence

/// Evidence is checked strictly: well-formed fault activities only, signed by a member of that
/// epoch's set under this chain's namespace, not from the future, not too old, not forged.
#[test]
fn evidence_is_verified_strictly() {
    let reporter = keys(203);
    let mut t = Setup { accounts: vec![(reporter.address(), 1_000 * FANG, None)], ..Default::default() }.start();
    t.finish_epoch();
    t.blocks(2);
    let h = t.height;
    let check = |t: &T, hex_ev: &str| verify_evidence(&t.st, CHAIN, &hex::decode(hex_ev).unwrap(), h).map_err(|e| e.to_string());
    // All three kinds verify.
    let s = scheme_of(&t, 1, 1);
    let fin = |d: u8| Finalize::sign(&s, proposal(1, 3, d)).unwrap();
    let cf = hex::encode(Activity::ConflictingFinalize(ConflictingFinalize::new(fin(1), fin(2))).encode());
    assert_eq!(check(&t, &cf).unwrap().kind, CONFLICTING_FINALIZE);
    let nf = hex::encode(Activity::NullifyFinalize(NullifyFinalize::new(Nullify::sign::<Digest>(&s, Round::new(Epoch::new(1), View::new(3))).unwrap(), fin(1))).encode());
    assert_eq!(check(&t, &nf).unwrap(), Fault { kind: NULLIFY_FINALIZE, validator: vaddr(1), epoch: 1, view: 3 });
    // Forged: a flipped signature byte; a key outside the set signing as a member.
    let good = double_notarize(&t, 1, 1, 3);
    let mut bytes = hex::decode(&good).unwrap();
    let n = bytes.len();
    bytes[n - 10] ^= 1;
    assert!(check(&t, &hex::encode(&bytes)).unwrap_err().contains("don't verify"));
    // A stranger's key signing in a member's slot (its own set with it swapped in).
    let pk = |seed: u8| commonware_cryptography::Signer::public_key(&PrivateKey::from_seed_bytes([seed; 32]));
    let set = participants(&t.st, 1).unwrap();
    let swapped = commonware_utils::ordered::Set::try_from(set.iter().cloned().map(|k| if k == pk(vseed(1)) { pk(99) } else { k }).collect::<Vec<_>>()).unwrap();
    let s99 = MlDsaScheme::signer(&crate::consensus::namespace(CHAIN), swapped, PrivateKey::from_seed_bytes([99; 32])).unwrap();
    let forged = Activity::ConflictingNotarize(ConflictingNotarize::new(Notarize::sign(&s99, proposal(1, 3, 1)).unwrap(), Notarize::sign(&s99, proposal(1, 3, 2)).unwrap()));
    assert!(check(&t, &hex::encode(forged.encode())).unwrap_err().contains("don't verify"), "a stranger's votes in a member's slot");
    // Another chain's namespace (a vote replayed from a testnet with the same keys).
    let other = MlDsaScheme::signer(&crate::consensus::namespace("other-chain"), participants(&t.st, 1).unwrap(), PrivateKey::from_seed_bytes([vseed(1); 32])).unwrap();
    let replayed = Activity::ConflictingNotarize(ConflictingNotarize::new(Notarize::sign(&other, proposal(1, 3, 1)).unwrap(), Notarize::sign(&other, proposal(1, 3, 2)).unwrap()));
    assert!(check(&t, &hex::encode(replayed.encode())).unwrap_err().contains("don't verify"));
    // Mismatched: the two votes from different members don't decode as one fault.
    let (s1, s2) = (scheme_of(&t, 1, 1), scheme_of(&t, 2, 1));
    let mut mixed = vec![CONFLICTING_NOTARIZE];
    mixed.extend(Notarize::sign(&s1, proposal(1, 3, 1)).unwrap().encode());
    mixed.extend(Notarize::sign(&s2, proposal(1, 3, 2)).unwrap().encode());
    assert!(check(&t, &hex::encode(&mixed)).unwrap_err().contains("malformed"));
    // The same vote twice isn't a conflict.
    let same = Notarize::sign(&s1, proposal(1, 3, 1)).unwrap().encode();
    let twice = [vec![CONFLICTING_NOTARIZE], same.to_vec(), same.to_vec()].concat();
    assert!(check(&t, &hex::encode(&twice)).unwrap_err().contains("malformed"));
    // Not a fault at all (a lone vote, tag 0), trailing bytes, truncation, nothing.
    assert!(check(&t, &hex::encode([vec![0u8], same.to_vec()].concat())).unwrap_err().contains("must be a Conflicting"));
    let mut trailing = hex::decode(&good).unwrap();
    trailing.push(0);
    assert!(check(&t, &hex::encode(&trailing)).unwrap_err().contains("malformed"));
    assert!(check(&t, &good[..good.len() - 2]).unwrap_err().contains("malformed"));
    assert!(decode_evidence(&[]).is_err());
    // From the future, or older than the evidence window.
    let future = {
        // Epoch 2's set is known once epoch 1 ends; at height h it's still epoch 1.
        let mut t2 = Setup { accounts: vec![], ..Default::default() }.start();
        t2.finish_epoch();
        t2.finish_epoch();
        double_notarize(&t2, 1, 2, 1)
    };
    assert!(check(&t, &future).unwrap_err().contains("after this one"));
    let max_age = t.st.chain_params.evidence_max_age_epochs;
    let old = double_notarize(&t, 1, 1, 3);
    let late = h + (max_age + 1) * EPOCH;
    assert!(verify_evidence(&t.st, CHAIN, &hex::decode(&old).unwrap(), late).unwrap_err().to_string().contains("too old"));
    // A set the state no longer holds can't be checked.
    let mut pruned = t.st.clone();
    pruned.active_sets.remove(&1);
    assert!(verify_evidence(&pruned, CHAIN, &hex::decode(&old).unwrap(), h).unwrap_err().to_string().contains("no longer in state"));
    // The status the node's evidence task asks for.
    assert!(matches!(evidence_status(&t.st, CHAIN, &hex::decode(&old).unwrap(), h), EvidenceStatus::Open(_)));
    assert_eq!(evidence_status(&t.st, CHAIN, &hex::decode(&future).unwrap(), h), EvidenceStatus::NotYet);
    assert_eq!(evidence_status(&t.st, CHAIN, &trailing, h), EvidenceStatus::Never);
    t.ok(&reporter, Action::ReportEquivocation { evidence: old.clone() });
    assert_eq!(evidence_status(&t.st, CHAIN, &hex::decode(&old).unwrap(), h), EvidenceStatus::Punished);
    t.block();
}

/// Commonware's codec is a consensus format now (evidence is verified on chain): these bytes
/// must never change under a dependency bump without a protocol upgrade.
#[test]
fn evidence_encoding_golden_vector() {
    // Four fixed consensus keys; validator index 0 of the sorted set double-notarizes.
    let keys: Vec<PrivateKey> = (1..=4u8).map(|i| PrivateKey::from_seed_bytes([i; 32])).collect();
    let set = commonware_utils::ordered::Set::try_from(keys.iter().map(commonware_cryptography::Signer::public_key).collect::<Vec<_>>()).unwrap();
    let first = keys.iter().find(|k| set.position(&commonware_cryptography::Signer::public_key(*k)) == Some(0)).unwrap().clone();
    let s = MlDsaScheme::signer(b"blockula/golden", set.clone(), first).unwrap();
    let p = |d: u8| Proposal::new(Round::new(Epoch::new(3), View::new(300)), View::new(299), Digest([d; 32]));
    let ev = Activity::ConflictingNotarize(ConflictingNotarize::new(Notarize::sign(&s, p(0xaa)).unwrap(), Notarize::sign(&s, p(0xbb)).unwrap())).encode().to_vec();
    // tag ‖ (round ‖ parent ‖ digest ‖ signer ‖ signature) × 2; varints: epoch 3, view 300, parent 299, signer 0.
    assert_eq!(ev.len(), 1 + 2 * (1 + 2 + 2 + 32 + 1 + crate::pq::SIG_LEN));
    assert_eq!(&ev[..6], &[CONFLICTING_NOTARIZE, 3, 0xac, 0x02, 0xab, 0x02]);
    assert_eq!(hex::encode(&ev[6..38]), "aa".repeat(32));
    assert_eq!(hex::encode(crate::crypto::hash(&ev)), "208125577c38b051782d6a6eb05f0e7c183861ae9662793924fd3968c21b22a8");
    let e = decode_evidence(&ev).unwrap();
    assert_eq!((e.kind, e.epoch, e.view, e.signer), (CONFLICTING_NOTARIZE, 3, 300, 0));
    assert!(e.activity.verify(&mut NoRng, &MlDsaScheme::verifier(b"blockula/golden", set), &commonware_parallel::Sequential));
}

// ------------------------------------------------------------------------- the active set

#[test]
fn the_set_is_the_top_stake_with_ties_by_address() {
    let alice = keys(200);
    let mut t = Setup { validators: vec![10_000 * FANG, 20_000 * FANG, 10_000 * FANG, 10_000 * FANG, 10_000 * FANG], accounts: vec![(alice.address(), 100_000 * FANG, None)], params: vec![("max_validators", 4)], ..Default::default() }.start();
    // Genesis seats all five (over the new cap): one leaves, the lowest stake, ties by address.
    let v1 = vaddr(1);
    let tie_loser = [0u8, 2, 3, 4].map(vaddr).into_iter().max().unwrap();
    t.finish_epoch();
    let s1 = t.set(1);
    assert!(s1.len() == 4 && s1.contains(&v1) && !s1.contains(&tie_loser), "{s1:?}");
    // Delegation decides: the one that left overtakes the weakest member.
    t.ok(&alice, Action::Delegate { validator: tie_loser.clone(), amount: 1_000 * FANG });
    t.finish_epoch();
    assert!(t.set(2).contains(&tie_loser) && t.set(2).len() == 4);
    assert_eq!(t.st.validators[&tie_loser].active_from, Some(2));
    // A permissionless candidate with enough stake joins too, in place of the weakest member.
    let (nv, no) = (keys(50), keys(150));
    t.ok(&alice, Action::Transfer { to: no.address(), amount: 60_000 * FANG });
    let pop = crate::chain::make_pop([50; 32], CHAIN, &no.address(), "127.0.0.1:7999", "127.0.0.1:7998");
    let reg = Action::RegisterValidator { consensus_key: hex::encode(nv.public_key()), p2p_addr: "127.0.0.1:7999".into(), http_addr: "127.0.0.1:7998".into(), bond: 50_000 * FANG, pop: pop.clone() };
    assert!(t.fails(&no, Action::RegisterValidator { consensus_key: hex::encode(nv.public_key()), p2p_addr: "127.0.0.1:7999".into(), http_addr: "127.0.0.1:7998".into(), bond: 999 * FANG, pop: pop.clone() }).contains("at least"));
    let bad_pop = crate::chain::make_pop([50; 32], CHAIN, &alice.address(), "127.0.0.1:7999", "127.0.0.1:7998");
    assert!(t.fails(&no, Action::RegisterValidator { consensus_key: hex::encode(nv.public_key()), p2p_addr: "127.0.0.1:7999".into(), http_addr: "127.0.0.1:7998".into(), bond: 50_000 * FANG, pop: bad_pop }).contains("pop"));
    t.ok(&no, reg.clone());
    assert_eq!(t.st.validators[&nv.address()].status, ValidatorStatus::Candidate);
    assert!(t.fails(&no, reg).contains("already registered"));
    t.finish_epoch();
    assert!(t.set(3).contains(&nv.address()) && t.set(3).contains(&v1), "{:?}", t.set(3));
    assert_eq!(t.st.validators[&nv.address()].status, ValidatorStatus::Active);
    // Under-bonded validators (the owner sold its self-bond) leave regardless.
    t.ok(&owner(1), Action::Undelegate { validator: v1.clone(), shares: 19_500 * FANG });
    assert!(!eligible(&t.st, &v1));
    t.finish_epoch();
    assert!(!t.set(4).contains(&v1));
    assert_eq!(t.st.validators[&v1].status, ValidatorStatus::Candidate);
}

/// Dev and test genesis files may list a validator without an operator: it has no self-bond, so
/// it leaves at the first boundary, unless it's all there is (the set never empties).
#[test]
fn genesis_validators_without_operators_have_no_self_bond() {
    let mut g = Setup::default().genesis();
    g.operators.pop();
    g.check().unwrap();
    let mut t = T { st: without_issuance(g.initial_state()), height: 1, epoch: 0, view: 0, skip: 0, skipped: vec![], tx_bytes: 0 };
    assert_eq!((t.st.staking.pools[&vaddr(3)].stake, eligible(&t.st, &vaddr(3))), (0, false));
    t.finish_epoch();
    assert_eq!(t.set(1).len(), 3);
    assert!(!t.set(1).contains(&vaddr(3)));
    let mut g = Setup { validators: vec![FANG], ..Default::default() }.genesis();
    g.operators.clear();
    let mut t = T { st: without_issuance(g.initial_state()), height: 1, epoch: 0, view: 0, skip: 0, skipped: vec![], tx_bytes: 0 };
    t.finish_epoch();
    assert_eq!(t.set(1), vec![vaddr(0)]);
}

/// The churn limit protects small sets: a stranger's registration can't make itself needed for
/// a one-validator chain's quorum. An admin of a test chain may still pin it in.
#[test]
fn small_sets_take_no_newcomers_unless_an_admin_pins_one() {
    let (admin, newbie, nk) = (keys(230), keys(150), keys(50));
    let mut t = Setup { validators: vec![10_000 * FANG], accounts: vec![(admin.address(), 100 * FANG, None), (newbie.address(), 100_000 * FANG, None)], admins: vec![admin.address()], ..Default::default() }.start();
    let pop = crate::chain::make_pop([50; 32], CHAIN, &newbie.address(), "127.0.0.1:7999", "127.0.0.1:7998");
    t.ok(&newbie, Action::RegisterValidator { consensus_key: hex::encode(nk.public_key()), p2p_addr: "127.0.0.1:7999".into(), http_addr: "127.0.0.1:7998".into(), bond: 50_000 * FANG, pop });
    t.finish_epoch();
    assert_eq!(t.set(1), vec![vaddr(0)], "a set of 2 needs both to vote: no newcomer");
    t.ok(&admin, Action::ApproveValidator { validator: nk.address() });
    t.finish_epoch();
    assert_eq!(t.set(2).len(), 2, "pinned in by the admin");
}

/// CV-4: at most ⌊(n−1)/3⌋ seats change per epoch for stake (highest stake first), while
/// ineligible members always leave; the set never empties.
#[test]
fn the_churn_limit_bounds_each_epoch() {
    let whale = keys(200);
    let mut t = Setup { validators: vec![2_000 * FANG; 7], accounts: vec![(whale.address(), 10_000_000 * FANG, None)], params: vec![("max_validators", 7)], ..Default::default() }.start();
    assert_eq!(churn_limit(7), 2);
    assert_eq!((churn_limit(1), churn_limit(3), churn_limit(4), churn_limit(31)), (0, 0, 1, 10));
    // Five bigger candidates register.
    let mut newcomers = vec![];
    for i in 0..5u8 {
        let (nv, no) = (keys(40 + i), keys(140 + i));
        t.ok(&whale, Action::Transfer { to: no.address(), amount: 100_000 * FANG });
        let pop = crate::chain::make_pop([40 + i; 32], CHAIN, &no.address(), "127.0.0.1:7999", "127.0.0.1:7998");
        t.ok(&no, Action::RegisterValidator { consensus_key: hex::encode(nv.public_key()), p2p_addr: "127.0.0.1:7999".into(), http_addr: "127.0.0.1:7998".into(), bond: (10_000 + i as u64) * FANG, pop });
        newcomers.push(nv.address());
    }
    let preview = select_next(&t.st, 0);
    assert_eq!(preview.joining.len(), 2, "two seats change, not five");
    t.finish_epoch();
    let s1 = t.set(1);
    assert_eq!(s1.len(), 7);
    // The two biggest joined.
    assert!(s1.contains(&newcomers[4]) && s1.contains(&newcomers[3]) && !s1.contains(&newcomers[2]));
    let overlap = t.set(0).iter().filter(|v| s1.contains(v)).count();
    assert!(overlap * 3 > 2 * 7, "more than two thirds stay");
    t.finish_epoch();
    t.finish_epoch();
    assert!(newcomers.iter().all(|v| t.set(3).contains(v)), "all in after three epochs");
    // Mandatory exits ignore the limit: jail four members at once; they all leave, and of the
    // free seats only one refills (from the displaced candidates): 3 carried over must stay a
    // quorum of the new set.
    let jailed: Vec<String> = t.set(3).into_iter().take(4).collect();
    let mut j = Journal::default();
    for v in &jailed {
        jail(&mut t.st, &mut j, v, t.height, 0, "test");
    }
    t.finish_epoch();
    assert_eq!(t.set(4).len(), 3 + 1);
    assert!(jailed.iter().all(|v| !t.set(4).contains(v)));
    // Never empty: with every validator jailed, nobody is eligible and the set stays as it is.
    let mut j = Journal::default();
    let all: Vec<String> = t.st.validators.keys().cloned().collect();
    for v in all {
        jail(&mut t.st, &mut j, &v, t.height, 0, "test");
    }
    let before = t.set(4);
    t.finish_epoch();
    assert_eq!(t.set(5), before);
}

// ------------------------------------------------------------------- downtime and jailing

#[test]
fn leaders_come_from_simplex_round_robin() {
    for n in [1usize, 2, 4, 7, 31] {
        for e in [0u64, 1, 5, 1_000] {
            for v in [1u64, 2, 3, 30, 31, 1_000_003] {
                assert_eq!(leader_index(None, e, v, n), ((e + v) % n as u64) as usize);
            }
        }
        // Shuffled by a beacon seed (SF-1): still a rotation, so any n consecutive views lead
        // each validator once, in the order the seeded elector itself gives.
        let seed = Some([7u8; 32]);
        let mut seen: Vec<usize> = (0..n as u64).map(|v| leader_index(seed, 3, 100 + v, n)).collect();
        seen.sort();
        assert_eq!(seen, (0..n).collect::<Vec<_>>());
    }
}

/// Downtime charges the leaders consensus elects: `leader_index` with the epoch's beacon seed
/// names the same validator as `consensus::leader` (which builds the elector from the real set),
/// in the genesis epoch, the hash-chain epoch after it and a beacon-derived epoch.
#[test]
fn downtime_elects_the_leaders_consensus_elects() {
    let mut t = Setup::default().start();
    for _ in 0..3 {
        t.finish_epoch();
    }
    for epoch in [0, 1, t.epoch] {
        assert!(crate::beacon::elector_seed(&t.st, epoch).is_some());
        for view in 1..12 {
            assert_eq!(Some(t.leader(epoch, view)), crate::consensus::leader(&t.st, epoch, view), "epoch {epoch}, view {view}");
        }
    }
}

#[test]
fn missed_leader_slots_jail_and_the_validator_may_unjail_later() {
    let mut t = Setup { params: vec![("downtime_window", 8), ("downtime_max_missed", 3), ("slash_downtime_bps", 100)], ..Default::default() }.start();
    // A gap of views: each missed view counts against its own leader.
    t.blocks(2);
    let (epoch, from) = (t.epoch, t.view);
    t.skip = 2;
    t.block();
    let order = leaders(&t.st, epoch);
    for x in [from + 1, from + 2] {
        let l = &order[t.idx(epoch, x)];
        assert_eq!(t.st.staking.liveness[l].missed, 1, "view {x}'s leader missed it");
    }
    assert_eq!(t.st.staking.liveness[&t.leader(epoch, t.view)].missed, 0);
    // One validator stays offline: past 3 misses in its window it's jailed, slashed 1% once,
    // and leaves at the boundary; nobody else is charged.
    let victim = order[t.idx(epoch, t.view + 1)].clone();
    let before = t.st.staking.liveness.get(&victim).map_or(0, |l| l.missed);
    let stake = t.stake(&victim);
    assert_eq!(t.starve(&victim, 4 - before), 4 - before);
    // It stays away (for as long as it's still in the set: the leader order is the beacon's
    // shuffle, so how many more slots it gets before the boundary varies).
    t.starve(&victim, 2);
    let jail = t.st.staking.pools[&victim].jail.clone().expect("jailed");
    assert!(jail.reason.contains("missed 4 of its last"), "{}", jail.reason);
    assert_eq!(t.stake(&victim), stake - stake / 100, "slashed once, however long it stays away");
    let others_ok = order.iter().filter(|v| **v != victim).all(|v| t.st.staking.pools[v].jail.is_none());
    assert!(others_ok, "only the absent leader was charged");
    t.finish_epoch();
    let e = t.epoch + 1;
    assert!(!t.set(e).contains(&victim), "jailed validators leave at the boundary");
    // Unjail: owner only, after jail_blocks.
    let i = (0..4u8).find(|i| vaddr(*i) == victim).unwrap();
    let early = t.fails(&owner(i), Action::Unjail { validator: victim.clone() });
    assert!(early.contains(&format!("jailed until block {}", jail.until)), "{early}");
    assert!(t.fails(&owner((i + 1) % 4), Action::Unjail { validator: victim.clone() }).contains("only validator"));
    while t.height < jail.until {
        t.block();
    }
    // Under-bonded, it would never be selected: the owner tops up first.
    let held = t.shares(&owner(i).address(), &victim) as u64;
    t.ok(&owner(i), Action::Undelegate { validator: victim.clone(), shares: held - 500 * FANG });
    assert!(t.fails(&owner(i), Action::Unjail { validator: victim.clone() }).contains("below the minimum"));
    t.ok(&owner(i), Action::Delegate { validator: victim.clone(), amount: 900 * FANG });
    t.ok(&owner(i), Action::Unjail { validator: victim.clone() });
    assert!(t.st.staking.liveness.get(&victim).is_none(), "a fresh window");
    assert!(t.fails(&owner(i), Action::Unjail { validator: victim.clone() }).contains("isn't jailed"));
    t.finish_epoch();
    assert!(t.set(t.epoch + 1).contains(&victim), "back at the next boundary");
}

#[test]
fn liveness_rings_slide_and_saturate() {
    let mut l = Liveness::new(10);
    assert_eq!(l.bits.len(), 2);
    for i in 0..10 {
        l.push(i % 3 == 0); // misses at 0, 3, 6, 9
    }
    assert_eq!((l.filled, l.missed, l.led, l.missed_total), (10, 4, 6, 4));
    // The window slides: the oldest slot (a miss) falls out as a hit comes in.
    l.push(false);
    assert_eq!((l.filled, l.missed), (10, 3));
    l.miss(2);
    assert_eq!(l.missed, 5, "two hits fell out, two misses came in");
    // A long outage fills the window, whatever its alignment.
    l.miss(25);
    assert_eq!((l.filled, l.missed, l.next, l.missed_total), (10, 10, 0, 4 + 2 + 25));
    assert_eq!(l.bits, vec![0xff, 0b11]);
    l.push(false);
    assert_eq!(l.missed, 9);
}

/// A long stall: every validator is charged once per lap of the views that were nullified, and
/// the leftover views' leaders once more.
#[test]
fn long_outages_are_charged_per_lap() {
    let mut t = Setup { params: vec![("downtime_window", 100), ("downtime_max_missed", 50)], ..Default::default() }.start();
    t.block();
    let (epoch, from) = (t.epoch, t.view);
    t.skip = 9; // views from+1 ..= from+9: two laps of 4, and one more
    t.block();
    let order = leaders(&t.st, epoch);
    let extra = &order[t.idx(epoch, from + 9)];
    for v in &order {
        let want = if v == extra { 3 } else { 2 };
        assert_eq!(t.st.staking.liveness[v].missed, want, "{v}");
    }
    assert!(order.iter().all(|v| t.st.staking.pools[v].jail.is_none()));
}

// ----------------------------------------------------------------------------- junk charge

#[test]
fn junk_costs_the_proposer_its_balance_then_its_self_bond() {
    let mut t = Setup::default().start();
    t.block();
    let (epoch, view) = (t.epoch, t.view + 1);
    let proposer = t.leader(epoch, view);
    let i = (0..4u8).find(|i| vaddr(*i) == proposer).unwrap();
    // The consensus key's account holds 1 FANG; the junk costs more than that.
    let mut j = Journal::default();
    t.st.j_mint(&mut j, FANG);
    t.st.j_credit(&mut j, &proposer, FANG);
    let sizes = vec![60_000u64; 4_000];
    let due: u64 = sizes.iter().map(|s| crate::econ::base_fee(&t.st, t.height, *s)).sum();
    assert!(due > FANG);
    let (bond, burned) = (self_bond(&t.st, &proposer), t.st.burned);
    t.skipped = sizes;
    t.block();
    assert_eq!(t.balance(&proposer), 0);
    assert_eq!(self_bond(&t.st, &proposer), bond - (due - FANG));
    assert_eq!(t.st.burned, burned + due);
    assert_eq!(t.st.staking.pools[&proposer].junk_charged, due);
    let _ = i;
    // More than it has: everything it has goes, the rest is waived (nothing goes negative), and
    // without its self-bond it's out at the boundary.
    let next = t.leader(t.epoch, t.view + 1);
    let (bond, burned) = (self_bond(&t.st, &next), t.st.burned);
    t.skipped = vec![10_000_000_000_000];
    t.block();
    assert_eq!((self_bond(&t.st, &next), t.st.burned), (0, burned + bond + t.balance(&next)));
    assert!(!eligible(&t.st, &next));
}

// --------------------------------------------------------------------- commission and caps

#[test]
fn commission_moves_within_bounds_per_epoch() {
    let mut t = Setup { params: vec![("commission_max_change_bps", 200), ("commission_max_bps", 2_000)], ..Default::default() }.start();
    let (v, o) = (vaddr(0), owner(0));
    let set = |bps| Action::UpdateValidator { commission_bps: bps, p2p_addr: String::new(), http_addr: String::new() };
    // 10% at genesis: up to 12% this epoch, in steps if you like, never past the epoch's bound.
    t.ok(&o, set(1_100));
    t.ok(&o, set(1_200));
    assert!(t.fails(&o, set(1_201)).contains("at most 2 points per epoch"));
    assert!(t.fails(&o, set(2_100)).contains("at most 20%"));
    assert_eq!(t.st.validators[&v].commission_bps, 1_200);
    t.finish_epoch();
    t.ok(&o, set(1_400));
    // Addresses change with it; a stranger can't change anything.
    t.ok(&o, Action::UpdateValidator { commission_bps: 1_400, p2p_addr: "10.0.0.1:30303".into(), http_addr: "https://v0.example".into() });
    assert_eq!((t.st.validators[&v].p2p_addr.as_str(), t.st.validators[&v].http_addr.as_str()), ("10.0.0.1:30303", "https://v0.example"));
    t.ok(&o, Action::Transfer { to: keys(250).address(), amount: 10 * FANG });
    assert!(t.fails(&keys(250), set(0)).contains("doesn't own a validator"));
    t.block();
}

#[test]
fn vesting_accounts_have_a_cap_per_pool_and_their_rewards_vest() {
    let founder = keys(210);
    let vest = Vesting { start: 0, cliff: 30, end: 60 };
    let mut t = Setup { accounts: vec![(founder.address(), 30_000_000 * FANG, Some(vest.clone()))], params: vec![("insider_delegation_cap_bps", 100)], ..Default::default() }.start();
    let (v, f) = (vaddr(0), founder.address());
    // 1% of the supply per pool while anything is unvested.
    let cap = t.st.supply / 100;
    assert!(t.fails(&founder, Action::Delegate { validator: v.clone(), amount: cap + FANG }).contains("at most"));
    t.ok(&founder, Action::Delegate { validator: v.clone(), amount: cap / 2 });
    assert!(t.fails(&founder, Action::Delegate { validator: v.clone(), amount: cap / 2 + FANG }).contains("at most"));
    t.ok(&founder, Action::Delegate { validator: vaddr(1), amount: cap / 2 });
    // Rewards on locked stake: the gain is added to the vesting lock when it's sold.
    let mut j = Journal::default();
    t.st.j_mint(&mut j, 1_000_000 * FANG);
    let paid = distribute_rewards(&mut t.st, &mut j, 1_000_000 * FANG, t.height);
    t.st.j_burn(&mut j, 1_000_000 * FANG - paid);
    let (value, principal) = (delegation_value(&t.st, &f, &v), t.st.staking.delegations[&dkey(&f, &v)].principal);
    assert!(value > principal);
    let shares = t.shares(&f, &v) as u64;
    t.ok(&founder, Action::Undelegate { validator: v.clone(), shares });
    let lock: &Lock = &t.st.account(&f).locks[0];
    assert_eq!(lock.amount, LockAmount::Vesting { total: 30_000_000 * FANG + (value - principal), start: 0, cliff: 30, end: 60 });
    // Once fully vested: no cap, and gains are liquid.
    while t.height < 60 {
        t.block();
    }
    t.ok(&founder, Action::Delegate { validator: vaddr(2), amount: cap + FANG });
}

/// An account whose every token is locked can stake them at any share price: a delegation to a
/// pool whose shares are worth more than a drop loses a drop of rounding to the pool, and that
/// isn't moving locked tokens out (`Journal::lock_slack`). Unbonding and redelegating likewise.
/// Meanwhile what the stake earned (not sold yet) doesn't cover the lock: it can't free liquid
/// tokens before the cliff (3.7; audit F03's unrealized-gains case).
#[test]
fn fully_locked_accounts_stake_at_any_share_price_and_gains_dont_unlock() {
    let founder = keys(211);
    let mut t = Setup { accounts: vec![(founder.address(), 1_000_000 * FANG, Some(Vesting { start: 0, cliff: 1_000, end: 2_000 }))], ..Default::default() }.start();
    let (v, f) = (vaddr(0), founder.address());
    // Rewards make validator 0's shares worth more than a drop each.
    let mut j = Journal::default();
    t.st.j_mint(&mut j, 777_777 * FANG + 7);
    let paid = distribute_rewards(&mut t.st, &mut j, 777_777 * FANG + 7, t.height);
    t.st.j_burn(&mut j, 777_777 * FANG + 7 - paid);
    let pool = t.st.staking.pools[&v].clone();
    assert!(pool.stake as u128 > pool.shares);
    assert_eq!(crate::econ::transferable(&t.st, &f, t.height), 0, "every token is locked");
    // 1,000 FANG and 3 drops buys shares worth a drop less: still fine.
    let amount = 1_000 * FANG + 3;
    let shares = pool.shares_for(amount).unwrap();
    assert!(Pool { stake: pool.stake + amount, shares: pool.shares + shares, ..pool.clone() }.value(shares) < amount, "the purchase loses rounding to the pool");
    t.ok(&founder, Action::Delegate { validator: v.clone(), amount });
    t.ok(&founder, Action::Delegate { validator: vaddr(1), amount: 500 * FANG });
    t.ok(&founder, Action::Redelegate { from_validator: v.clone(), to_validator: vaddr(2), shares: (t.shares(&f, &v) / 3) as u64 });
    t.ok(&founder, Action::Undelegate { validator: v.clone(), shares: (t.shares(&f, &v) / 7) as u64 });
    assert!(t.fails(&founder, Action::Transfer { to: keys(250).address(), amount: FANG }).contains("locked"));
    // Stake earns; the gain stays locked until it's sold, then vests with the allocation.
    let mut j = Journal::default();
    t.st.j_mint(&mut j, 1_000_000 * FANG);
    let paid = distribute_rewards(&mut t.st, &mut j, 1_000_000 * FANG, t.height);
    t.st.j_burn(&mut j, 1_000_000 * FANG - paid);
    let gain = delegation_value(&t.st, &f, &vaddr(1)) - t.st.staking.delegations[&dkey(&f, &vaddr(1))].principal;
    assert!(gain > 10 * FANG, "{gain}");
    assert_eq!(crate::econ::transferable(&t.st, &f, t.height), 0, "unrealized gains don't cover the vesting lock");
    assert!(t.fails(&founder, Action::Transfer { to: keys(250).address(), amount: 10 * FANG }).contains("locked"));
    t.block();
}

// ------------------------------------------------------------------------------- SF-13

#[test]
fn consensus_keys_sign_system_txs_for_their_owners() {
    let (stranger, reporter) = (keys(220), keys(203));
    let mut t = Setup { accounts: vec![(stranger.address(), 100 * FANG, None), (reporter.address(), 100 * FANG, None)], ..Default::default() }.start();
    t.finish_epoch();
    t.block();
    let (ck, o) = (keys(vseed(1)), owner(1));
    let evidence = double_notarize(&t, 2, 1, 1);
    let owner_bal = t.balance(&o.address());
    // Validator 1's consensus key reports for its owner, who pays (and gets the reward).
    let tx = t.tx_from(&ck, &o.address(), Action::ReportEquivocation { evidence: evidence.clone() });
    assert_eq!(t.apply(&tx).unwrap(), Outcome::Applied);
    assert!(t.balance(&o.address()) > owner_bal - tx.charge().unwrap(), "fee paid by the owner, reward received");
    assert_eq!(t.st.account(&ck.address()).nonce, 0);
    // Only system txs, and only for its own owner.
    let send = t.tx_from(&ck, &o.address(), Action::Transfer { to: stranger.address(), amount: FANG });
    assert!(t.apply(&send).unwrap_err().to_string().contains("isn't authorized"));
    let other = t.tx_from(&ck, &stranger.address(), Action::ReportEquivocation { evidence: double_notarize(&t, 2, 1, 2) });
    assert!(t.apply(&other).unwrap_err().to_string().contains("isn't authorized"));
    let not_mine = t.tx_from(&keys(vseed(3)), &o.address(), Action::ReportEquivocation { evidence: double_notarize(&t, 2, 1, 2) });
    assert!(t.apply(&not_mine).is_err());
    // Attestations too: the consensus key's claim check counts as validator 1's (it gets to the
    // claim, which doesn't exist here), while a stranger isn't a validator at all.
    let attest = |t: &T, k: &Keys, from: &str| t.tx_from(k, from, Action::AttestClaim { claim_id: 0, ok: false, merge_sha: String::new(), note: String::new() });
    let tx = attest(&t, &ck, &o.address());
    assert_eq!(t.apply(&tx).unwrap(), Outcome::Failed("no claim #0".into()));
    let tx = attest(&t, &stranger, &stranger.address());
    assert_eq!(t.apply(&tx).unwrap(), Outcome::Failed("only active validators can attest claims".into()));
    // It also picks which validator an owner's `UpdateValidator` means.
    let upd = t.tx_from(&ck, &o.address(), Action::UpdateValidator { commission_bps: 1_050, p2p_addr: String::new(), http_addr: String::new() });
    assert_eq!(t.apply(&upd).unwrap(), Outcome::Applied);
    assert_eq!(t.st.validators[&vaddr(1)].commission_bps, 1_050);
    // Who a signer acts for: the consensus key itself or its owner, in this epoch's set.
    let key = |k: &Keys| key_id(&k.public_key());
    assert_eq!(validator_for_signer(&t.st, &o.address(), &key(&ck), 1), Some(vaddr(1)));
    assert_eq!(validator_for_signer(&t.st, &ck.address(), &key(&ck), 1), Some(vaddr(1)));
    assert_eq!(validator_for_signer(&t.st, &o.address(), &key(&o), 1), Some(vaddr(1)), "the owner with its own key");
    assert_eq!(validator_for_signer(&t.st, &stranger.address(), &key(&ck), 1), None);
    assert_eq!(validator_for_signer(&t.st, &o.address(), &key(&ck), 99), None, "an epoch without a set");
    t.block();
}

// ---------------------------------------------------------------------------- admin levers

#[test]
fn admin_levers_exist_only_on_dev_and_test_chains() {
    let admin = keys(230);
    // No admins (mainnet's shape): approve and remove are refused; Unbond is gone everywhere.
    let mut t = Setup { accounts: vec![(admin.address(), 100 * FANG, None)], ..Default::default() }.start();
    assert!(t.fails(&admin, Action::ApproveValidator { validator: vaddr(0) }).contains("join and leave by stake"));
    assert!(t.fails(&admin, Action::RemoveValidator { validator: vaddr(0) }).contains("join and leave by stake"));
    assert!(t.fails(&owner(0), Action::Unbond { validator: vaddr(0) }).contains("Undelegate it"));
    // With admins: a pin outranks stake, and a removal is final.
    let stakes = [10_000, 20_000, 30_000, 40_000, 50_000].map(|f| f * FANG).to_vec();
    let mut t = Setup { validators: stakes, accounts: vec![(admin.address(), 100 * FANG, None)], params: vec![("max_validators", 4)], admins: vec![admin.address()] }.start();
    assert!(t.fails(&owner(0), Action::ApproveValidator { validator: vaddr(0) }).contains("only validator admins"));
    t.ok(&admin, Action::ApproveValidator { validator: vaddr(0) });
    assert!(t.fails(&admin, Action::ApproveValidator { validator: vaddr(0) }).contains("already approved"));
    t.finish_epoch();
    assert!(t.set(1).contains(&vaddr(0)) && !t.set(1).contains(&vaddr(1)), "the pinned least stake stays: {:?}", t.set(1));
    t.ok(&admin, Action::RemoveValidator { validator: vaddr(2) });
    assert_eq!(t.st.validators[&vaddr(2)].status, ValidatorStatus::Removed);
    assert!(t.fails(&admin, Action::RemoveValidator { validator: vaddr(2) }).contains("already Removed"));
    t.finish_epoch();
    assert!(!t.set(2).contains(&vaddr(2)) && t.set(2).contains(&vaddr(1)), "a removal is a mandatory exit; a candidate takes the seat");
    for i in [1u8, 3, 4] {
        t.ok(&admin, Action::RemoveValidator { validator: vaddr(i) });
    }
    assert!(t.fails(&admin, Action::RemoveValidator { validator: vaddr(0) }).contains("last validator"));
    t.block();
}

// ------------------------------------------------------------------------------ journaling

/// Every staking change goes through the journal: rolled back, the state (and the records to
/// commit) are exactly as before.
#[test]
fn staking_txs_roll_back_completely() {
    let (alice, reporter) = (keys(200), keys(203));
    let mut t = Setup { accounts: vec![(alice.address(), 100_000 * FANG, None), (reporter.address(), 100 * FANG, None)], ..Default::default() }.start();
    t.ok(&alice, Action::Delegate { validator: vaddr(0), amount: 1_000 * FANG });
    t.finish_epoch();
    t.block();
    let (root, supply) = (t.st.full_root(), t.st.supply);
    let evidence = double_notarize(&t, 3, 1, 1);
    let mut j = Journal::default();
    let mut apply = |t: &mut T, k: &Keys, a: Action| {
        let tx = t.tx_from(k, &k.address(), a);
        assert_eq!(t.st.apply_j(&mut j, &tx, CHAIN, &vaddr(0), t.height, true).unwrap(), Outcome::Applied);
    };
    apply(&mut t, &alice, Action::Delegate { validator: vaddr(1), amount: 500 * FANG });
    apply(&mut t, &alice, Action::Undelegate { validator: vaddr(0), shares: 100 * FANG });
    apply(&mut t, &alice, Action::Redelegate { from_validator: vaddr(0), to_validator: vaddr(2), shares: 100 * FANG });
    apply(&mut t, &reporter, Action::ReportEquivocation { evidence });
    apply(&mut t, &owner(1), Action::UpdateValidator { commission_bps: 1_050, p2p_addr: String::new(), http_addr: String::new() });
    jail(&mut t.st, &mut j, &vaddr(2), t.height, 50, "test");
    t.st.rollback(j);
    assert_eq!((t.st.full_root(), t.st.supply), (root, supply));
    assert!(t.st.commit_block().unwrap().records.is_empty(), "nothing left to commit");
}

// ------------------------------------------------------------------- the bootstrap delegation

/// A chain whose bootstrap account holds `amount` on a return curve over blocks 30–90, and
/// validator 3's owner is excluded (the founder's or the foundation's seat).
fn bootstrap_chain(setup: Setup, amount: u64) -> T {
    let mut g = setup.genesis();
    g.allocations.push(Allocation { address: crate::chain::bootstrap_delegation("boo"), amount, vesting: Some(Vesting { start: 30, cliff: 30, end: 90 }), label: Some("bootstrap".into()) });
    g.bootstrap_excluded = vec![owner(3).address()];
    g.check().unwrap();
    T { st: without_issuance(g.initial_state()), height: 1, epoch: 0, view: 0, skip: 0, skipped: vec![], tx_bytes: 0 }
}

/// What the bootstrap account holds: liquid, and its delegations' value.
fn boot_holds(t: &T) -> u64 {
    let boot = t.st.bootstrap();
    t.balance(&boot) + delegations_of(&t.st, &boot).iter().map(|(v, _)| delegation_value(&t.st, &boot, v)).sum::<u64>()
}

/// Audit F12: DEC-5's 10% bootstrap bucket is never stranded. Each boundary it sends its rewards
/// to the treasury (operators keep their commission), keeps what its lock still holds, spread
/// equally over the next set's members not run by an excluded owner (it leaves jailed ones at
/// once), and returns the rest to the treasury on the curve, until it holds nothing.
#[test]
fn the_bootstrap_delegation_follows_the_set_and_returns_to_the_treasury() {
    let mut t = bootstrap_chain(Setup::default(), 1_200_000 * FANG);
    let (boot, treasury) = (t.st.bootstrap(), t.st.treasury());
    let worth = |t: &T, i: u8| delegation_value(&t.st, &boot, &vaddr(i));
    let quorum_base = crate::governance::unlocked_supply(&t.st, t.height);
    t.finish_epoch();
    // Equal shares to validators 0–2; none to 3 (excluded); nothing returned before the curve.
    assert_eq!([0, 1, 2, 3].map(|i| worth(&t, i)), [400_000 * FANG, 400_000 * FANG, 400_000 * FANG, 0]);
    assert_eq!((t.balance(&boot), t.balance(&treasury)), (0, 0));
    assert_eq!(crate::governance::unlocked_supply(&t.st, t.height), quorum_base, "its stake doesn't vote");

    // Rewards: the bootstrap's part goes to the treasury at the boundary; commission stays.
    t.block();
    let mut j = Journal::default();
    t.st.j_mint(&mut j, 124_000 * FANG);
    let paid = distribute_rewards(&mut t.st, &mut j, 124_000 * FANG, t.height);
    t.st.j_credit(&mut j, &treasury, 124_000 * FANG - paid);
    let (gain, treasury_before, bond0) = ((0..3).map(|i| worth(&t, i) - 400_000 * FANG).sum::<u64>(), t.balance(&treasury), self_bond(&t.st, &vaddr(0)));
    assert!(gain > 80_000 * FANG, "the bootstrap's stake earned ({gain})");
    t.finish_epoch();
    assert_eq!(t.balance(&treasury) - treasury_before, gain, "every drop it earned");
    assert!((0..3).all(|i| (400_000 * FANG - 2..=400_000 * FANG).contains(&worth(&t, i))), "back to its share");
    assert_eq!(self_bond(&t.st, &vaddr(0)), bond0, "the operator keeps its commission");

    // A jailed validator leaves the set and the bootstrap leaves it at once, for the others.
    let held = boot_holds(&t);
    jail(&mut t.st, &mut j, &vaddr(1), t.height, 0, "test");
    t.finish_epoch();
    assert!(!t.set(t.epoch + 1).contains(&vaddr(1)));
    assert!(!t.st.staking.delegations.contains_key(&dkey(&boot, &vaddr(1))), "out of the jailed pool");
    assert!([0, 2].iter().all(|i| (held / 2 - 2..=held / 2).contains(&worth(&t, *i))), "{} {}", worth(&t, 0), worth(&t, 2));
    assert!(t.balance(&boot) < 10, "all of it staked but dust");

    // Years 2–4 (here blocks 30–90): what the lock no longer holds goes back, each boundary.
    let treasury_before = t.balance(&treasury);
    while t.height < 60 {
        t.finish_epoch();
    }
    let unvested = t.st.account(&boot).unvested_at(59);
    assert_eq!(unvested, 1_200_000 * FANG * 31 / 60);
    assert!((unvested - 4..=unvested).contains(&boot_holds(&t)), "keeps what the lock holds: {} vs {unvested}", boot_holds(&t));
    assert!(t.balance(&treasury) - treasury_before >= held - unvested, "and returned the rest");
    // Past the curve's end it holds nothing; everything it had is back in the treasury.
    while t.height < 100 {
        t.finish_epoch();
    }
    assert!(delegations_of(&t.st, &boot).is_empty() && t.balance(&boot) == 0, "{:?}", delegations_of(&t.st, &boot));
    assert!(t.balance(&treasury) - treasury_before >= held - 10, "{} of {held}", t.balance(&treasury) - treasury_before);
}

/// The bootstrap delegation follows the set; it doesn't pick it. Its stake doesn't count toward
/// the ranking, so a newcomer with more stake of its own still replaces the weakest member.
#[test]
fn the_bootstrap_delegation_doesnt_entrench_the_set() {
    let newbie = keys(150);
    // Validator 3 (excluded, no bootstrap stake) outranks the newcomer on its own.
    let setup = Setup { validators: vec![10_000 * FANG, 10_000 * FANG, 10_000 * FANG, 30_000 * FANG, 10_000 * FANG], accounts: vec![(newbie.address(), 100_000 * FANG, None)], params: vec![("max_validators", 5)], ..Default::default() };
    let mut t = bootstrap_chain(setup, 1_200_000 * FANG);
    t.finish_epoch();
    assert!(delegation_value(&t.st, &t.st.bootstrap(), &vaddr(0)) > 200_000 * FANG);
    let nk = keys(50);
    let pop = crate::chain::make_pop([50; 32], CHAIN, &newbie.address(), "127.0.0.1:7999", "127.0.0.1:7998");
    t.ok(&newbie, Action::RegisterValidator { consensus_key: hex::encode(nk.public_key()), p2p_addr: "127.0.0.1:7999".into(), http_addr: "127.0.0.1:7998".into(), bond: 20_000 * FANG, pop });
    t.finish_epoch();
    let set = t.set(t.epoch + 1);
    assert!(set.contains(&nk.address()) && set.contains(&vaddr(3)) && set.len() == 5, "{set:?}");
    // And it backs the newcomer from the next boundary on.
    assert!(delegation_value(&t.st, &t.st.bootstrap(), &nk.address()) > 200_000 * FANG);
}
