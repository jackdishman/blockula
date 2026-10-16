//! Staking (P-STAKE, MAINNET-DESIGN 3.4): validator pools with share accounting, delegation and
//! unbonding, the stake-ranked active set with a churn limit, slashing for equivocation and
//! downtime, jailing, and the proposer's charge for junk txs.
//!
//! **Pools.** Every validator owns a pool ([`Pool`], `STAKE_POOLS`): `stake` drops, escrowed (in
//! no balance), split into `shares`. `Delegate` buys shares at the pool's rate (1 share per drop
//! in an empty pool); `Undelegate` sells them for an unbonding [`Entry`] that `WithdrawUnbonded`
//! pays out after `unbonding_blocks`; `Redelegate` moves them to another pool at once. Rewards
//! raise `stake`, so every share is worth more; the owner's commission is minted to it as new
//! shares; slashes lower `stake`, so every delegator loses alike. The owner's own shares are the
//! validator's self-bond. Rounding always favours the pool (a sale pays at most what the shares
//! are worth, a purchase buys at most what the amount is worth), so no operation creates FANG;
//! products of shares and amounts go through [`mul_div`]'s 256-bit intermediate.
//!
//! **The set.** Registration is permissionless (a self-bond of at least `min_self_bond` and a
//! proof of possession). At each epoch boundary [`select_next`] picks the next set from eligible
//! validators (not jailed, tombstoned or removed, self-bond at least `min_self_bond`), ranked by
//! pool stake, ties by address, at most `max_validators`. Ineligible members always leave.
//! Newcomers, highest stake first, fill free seats or replace lower-ranked members, but make up
//! at most ⌊(m−1)/3⌋ of the new set of `m` ([`churn_limit`]): the members carried over are a
//! quorum on their own, so newcomers that are still syncing can't stall it, and each set
//! overlaps the last by more than two thirds (for a full set: ⌊(n−1)/3⌋ replacements per
//! epoch; a set of one or two takes none). Members over a lowered cap leave ⌊(n−1)/3⌋ (at least
//! one) per epoch. The set never empties. Votes are equal-weight within the set (DEC-3). On dev
//! and test chains admins may still pin a validator (`ApproveValidator`: ranked ahead of stake,
//! and past the churn limit, at the admin's risk) or remove one (`RemoveValidator`); elsewhere
//! both are refused.
//!
//! Unbonding lasts `unbonding_blocks`, stretched (by at most an epoch) so that it never ends
//! before evidence against the epoch it began in stops being accepted.
//!
//! **Slashing.** `ReportEquivocation` carries simplex's own evidence (an encoded
//! `ConflictingNotarize`, `ConflictingFinalize` or `NullifyFinalize` activity), verified against
//! that epoch's set and namespace ([`verify_evidence`]). The pool loses `slash_double_sign_bps`
//! once per (validator, epoch, view), and so do the unbonding and redelegation entries that left
//! it since that epoch began; the reporter gets `reporter_reward_bps` of the slash, the rest is
//! burned, and the validator is tombstoned (never selected again). Downtime is measured from
//! leader slots: each view a block skipped (`parent_view + 1 .. view − 1`, nullified by 2f+1) is a
//! miss for that view's leader and the block's own view a hit, with simplex's `RoundRobin`
//! elector naming the leaders. A validator that missed more than `downtime_max_missed` of its
//! last `downtime_window` slots is jailed ([`jail`], `slash_downtime_bps`), leaves at the next
//! boundary, and may `Unjail` after `jail_blocks` (with its self-bond at the minimum again).
//!
//! **Junk** (CV-5). Verifiers replay a proposal's unexecuted ancestors exactly and reject it if
//! any of its txs can't be admitted (consensus.rs). A block finalized anyway (verifiers that
//! couldn't replay abstain) still costs its proposer `econ::base_fee` for each such tx's size at
//! execution, from its account and then its pool's self-bond, burned.
//!
//! **Locked tokens** (3.7). Vesting-locked tokens can be staked. What staking earns on them stays
//! locked on the same curve: when an account with an unvested vesting lock undelegates more than
//! it put in, the gain is added to that lock's total, so it vests with the rest (this counts the
//! account's whole gain, a conservative reading when it also staked liquid tokens). Such an account
//! may hold at most `insider_delegation_cap_bps` of the supply in any one pool.
//!
//! Slashing isn't gated by a flag: it ships together with P-UPGRADE's slashing protection (3.9),
//! so a restored disk can't get an honest operator slashed. A chain that wants it off sets
//! `slash_double_sign_bps` and `slash_downtime_bps` to 0 (tombstoning and jailing still apply).

use crate::chain::{short, BlockInfo, Journal, KeyRecord, State, ValidatorRecord, ValidatorStatus};
use crate::pq::MlDsaScheme;
use crate::state::codec::{decode, encode};
use crate::state::{meta, ns, JMap, SubState, TMap, Table};
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Context, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use commonware_consensus::simplex::types::{Activity, Attributable as _};
use commonware_consensus::{Epochable as _, Viewable as _};
use commonware_cryptography::blake3::Digest;
use std::collections::BTreeMap;

/// A delegator may have this many unbonding and redelegation entries at once.
pub const MAX_ENTRIES: usize = 32;
/// Least stake that opens a new delegation (top-ups may be smaller): every delegation is a record.
pub const MIN_DELEGATION: u64 = DROPS_PER_FANG;

/// The `Activity` tags simplex gives its fault evidence (the first byte of a `ReportEquivocation`).
pub const CONFLICTING_NOTARIZE: u8 = 7;
pub const CONFLICTING_FINALIZE: u8 = 8;
pub const NULLIFY_FINALIZE: u8 = 9;

// ------------------------------------------------------------------------------------- records

/// A validator's stake pool (key: the validator's address).
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Pool {
    /// Drops backing the pool: delegations, rewards and commission, less slashes and sales.
    pub stake: u64,
    /// Shares outstanding; a share is worth `stake / shares` drops.
    pub shares: u128,
    /// The commission at the start of `commission_epoch`: changes within that epoch stay within
    /// `commission_max_change_bps` of it.
    pub commission_base: u64,
    pub commission_epoch: u64,
    /// Out of the set from the next boundary until it unjails.
    pub jail: Option<Jail>,
    /// Double-signed at this height: never selected again.
    pub tombstoned_at: Option<u64>,
    /// Totals, for display.
    pub slashed: u64,
    pub rewards: u64,
    pub junk_charged: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Jail {
    pub at: u64,
    /// `Unjail` is accepted from this height on.
    pub until: u64,
    pub reason: String,
}

/// One delegator's shares of one pool (key: `delegator/validator`).
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Delegation {
    pub shares: u128,
    /// Drops put in, less what sales and slashes took out pro rata: value above it is earnings
    /// (what stays locked for vesting accounts).
    pub principal: u64,
}

/// Stake on its way out or recently moved (key: `delegator/id`, 20 digits). Both kinds stay
/// slashable by the validator they left for infractions in epochs that began before they did.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum Entry {
    /// Escrowed `amount`, paid out by `WithdrawUnbonded` from `release_at` on.
    Unbonding { validator: String, amount: u64, started_at: u64, release_at: u64 },
    /// `amount` moved from `from` to `to` (no escrow of its own: until `release_at`, `from`'s
    /// slashes reach the delegator's stake in `to`, and it can't move on from `to`).
    Redelegation { from: String, to: String, amount: u64, started_at: u64, release_at: u64 },
}

impl Entry {
    pub fn release_at(&self) -> u64 {
        match self {
            Entry::Unbonding { release_at, .. } | Entry::Redelegation { release_at, .. } => *release_at,
        }
    }
    pub fn started_at(&self) -> u64 {
        match self {
            Entry::Unbonding { started_at, .. } | Entry::Redelegation { started_at, .. } => *started_at,
        }
    }
}

/// A validator's recent leader slots (key: the validator's address): a ring of `window` bits,
/// 1 for a missed slot.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Liveness {
    pub window: u64,
    pub bits: Vec<u8>,
    /// Where the next slot goes in the ring.
    pub next: u64,
    /// Slots recorded (at most `window`) and misses among them.
    pub filled: u64,
    pub missed: u64,
    /// Since the record started, for display.
    pub led: u64,
    pub missed_total: u64,
}

impl Liveness {
    fn new(window: u64) -> Self {
        Self { window, bits: vec![0; window.div_ceil(8) as usize], next: 0, filled: 0, missed: 0, led: 0, missed_total: 0 }
    }

    fn bit(&self, i: u64) -> bool {
        self.bits[(i / 8) as usize] & (1 << (i % 8)) != 0
    }

    fn push(&mut self, missed: bool) {
        let i = self.next;
        if self.filled == self.window {
            if self.bit(i) {
                self.missed -= 1;
            }
        } else {
            self.filled += 1;
        }
        let (byte, mask) = ((i / 8) as usize, 1u8 << (i % 8));
        if missed {
            self.bits[byte] |= mask;
            self.missed += 1;
            self.missed_total += 1;
        } else {
            self.bits[byte] &= !mask;
            self.led += 1;
        }
        self.next = (i + 1) % self.window;
    }

    /// Records `n` missed slots in a row.
    fn miss(&mut self, n: u64) {
        if n >= self.window {
            let total = self.missed_total + n;
            *self = Self { bits: vec![0xff; self.bits.len()], filled: self.window, missed: self.window, missed_total: total, ..self.clone() };
            if !self.window.is_multiple_of(8) {
                *self.bits.last_mut().unwrap() = (1u8 << (self.window % 8)) - 1;
            }
            self.next = 0;
            return;
        }
        for _ in 0..n {
            self.push(true);
        }
    }
}

/// A punished equivocation (key: `validator/epoch/view`, 20 digits each): one slash per key.
/// Pruned once evidence for its epoch can no longer be submitted.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Punished {
    /// The evidence's `Activity` tag ([`CONFLICTING_NOTARIZE`] …).
    pub kind: u8,
    pub height: u64,
    pub reporter: String,
    pub slashed: u64,
}

/// Running total of pool stake (`Staking::pools`), so governance's quorum base is O(1).
pub struct PoolStake;

impl crate::state::Aggregate<Pool> for PoolStake {
    fn weight(p: &Pool) -> u128 {
        p.stake as u128
    }
}

/// Running total of unbonding entries' amounts (`Staking::entries`).
pub struct UnbondingTotal;

impl crate::state::Aggregate<Entry> for UnbondingTotal {
    fn weight(e: &Entry) -> u128 {
        match e {
            Entry::Unbonding { amount, .. } => *amount as u128,
            Entry::Redelegation { .. } => 0,
        }
    }
}

/// Staking's slice of the state (namespaces `state::ns` 32–36, META key `staking`).
#[derive(Clone, Debug, Default)]
pub struct Staking {
    pub pools: TMap<String, Pool, PoolStake>,
    pub delegations: TMap<String, Delegation>,
    pub entries: TMap<String, Entry, UnbondingTotal>,
    pub punished: TMap<String, Punished>,
    pub liveness: TMap<String, Liveness>,
    /// Id of the next unbonding or redelegation entry.
    pub next_entry_id: u64,
}

impl SubState for Staking {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Staking { pools, delegations, entries, punished, liveness, next_entry_id: _ } = self;
        vec![(ns::STAKE_POOLS, pools), (ns::STAKE_DELEGATIONS, delegations), (ns::STAKE_UNBONDING, entries), (ns::STAKE_EVIDENCE, punished), (ns::STAKE_LIVENESS, liveness)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Staking { pools, delegations, entries, punished, liveness, next_entry_id: _ } = self;
        vec![(ns::STAKE_POOLS, pools), (ns::STAKE_DELEGATIONS, delegations), (ns::STAKE_UNBONDING, entries), (ns::STAKE_EVIDENCE, punished), (ns::STAKE_LIVENESS, liveness)]
    }

    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        vec![(meta::STAKING, encode(&self.next_entry_id))]
    }

    fn load_meta(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        if key != meta::STAKING {
            return Ok(false);
        }
        self.next_entry_id = decode(value)?;
        Ok(true)
    }
}

/// Undo log for [`Staking`] (part of `chain::Journal`).
#[derive(Default)]
pub struct StakingJournal {
    pools: JMap<String, Pool>,
    delegations: JMap<String, Delegation>,
    entries: JMap<String, Entry>,
    punished: JMap<String, Punished>,
    liveness: JMap<String, Liveness>,
    next_entry_id: Option<u64>,
}

impl StakingJournal {
    pub fn absorb(&mut self, other: StakingJournal) {
        let StakingJournal { pools, delegations, entries, punished, liveness, next_entry_id } = other;
        self.pools.absorb(pools);
        self.delegations.absorb(delegations);
        self.entries.absorb(entries);
        self.punished.absorb(punished);
        self.liveness.absorb(liveness);
        if self.next_entry_id.is_none() {
            self.next_entry_id = next_entry_id;
        }
    }

    pub fn rollback(self, staking: &mut Staking) {
        let StakingJournal { pools, delegations, entries, punished, liveness, next_entry_id } = self;
        pools.restore(&mut staking.pools);
        delegations.restore(&mut staking.delegations);
        entries.restore(&mut staking.entries);
        punished.restore(&mut staking.punished);
        liveness.restore(&mut staking.liveness);
        if let Some(n) = next_entry_id {
            staking.next_entry_id = n;
        }
    }
}

// ----------------------------------------------------------------------------------- keys

fn dkey(delegator: &str, validator: &str) -> String {
    format!("{delegator}/{validator}")
}

fn ekey(delegator: &str, id: u64) -> String {
    format!("{delegator}/{id:020}")
}

fn pkey(validator: &str, epoch: u64, view: u64) -> String {
    format!("{validator}/{epoch:020}/{view:020}")
}

/// The records of a map keyed `account/…`, in key order. Addresses never contain `/`, and `0`
/// follows `/`, so the range holds exactly `account`'s keys.
fn owned_by<'m, V: Clone, A>(map: &'m TMap<String, V, A>, account: &str) -> impl Iterator<Item = (&'m String, &'m V)> + 'm {
    map.range(format!("{account}/")..format!("{account}0"))
}

// ----------------------------------------------------------------------------------- math

/// `a × b` as (high, low) 128-bit halves.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    const M: u128 = u64::MAX as u128;
    let (a0, a1, b0, b1) = (a & M, a >> 64, b & M, b >> 64);
    let (p00, p01, p10, p11) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
    let mid = (p00 >> 64) + (p01 & M) + (p10 & M);
    ((p11 + (p01 >> 64) + (p10 >> 64) + (mid >> 64)), (p00 & M) | (mid << 64))
}

/// `a × b / d` rounded down, and the remainder, through a 256-bit product. `None` if `d` is 0
/// or the quotient doesn't fit 128 bits.
pub fn mul_div_rem(a: u128, b: u128, d: u128) -> Option<(u128, u128)> {
    if d == 0 {
        return None;
    }
    if let Some(p) = a.checked_mul(b) {
        return Some((p / d, p % d));
    }
    let (hi, lo) = mul_wide(a, b);
    if hi >= d {
        return None;
    }
    // Long division, one bit at a time: the remainder stays below `d` (a carry out of bit 127
    // means it passed it).
    let (mut rem, mut q) = (hi, 0u128);
    for i in (0..128).rev() {
        let carry = rem >> 127;
        rem = (rem << 1) | ((lo >> i) & 1);
        q <<= 1;
        if carry == 1 || rem >= d {
            rem = rem.wrapping_sub(d);
            q |= 1;
        }
    }
    Some((q, rem))
}

/// `a × b / d` rounded down (see [`mul_div_rem`]).
pub fn mul_div(a: u128, b: u128, d: u128) -> Option<u128> {
    mul_div_rem(a, b, d).map(|(q, _)| q)
}

/// `a × b / d` rounded up.
pub fn mul_div_ceil(a: u128, b: u128, d: u128) -> Option<u128> {
    mul_div_rem(a, b, d).and_then(|(q, r)| if r == 0 { Some(q) } else { q.checked_add(1) })
}

/// `amount × bps / 10_000`, rounded down.
fn bps_of(amount: u64, bps: u64) -> u64 {
    (amount as u128 * bps.min(10_000) as u128 / 10_000) as u64
}

impl Pool {
    /// What `shares` of this pool are worth, rounded down (all of them: exactly `stake`).
    pub fn value(&self, shares: u128) -> u64 {
        if self.shares == 0 {
            return 0;
        }
        mul_div(shares, self.stake as u128, self.shares).map_or(self.stake, |v| v.min(self.stake as u128) as u64)
    }

    /// Shares `amount` buys, rounded down: 1 per drop in an empty pool. `None` for a pool slashed
    /// to nothing (its shares are worth 0, so any amount would buy unboundedly many).
    pub fn shares_for(&self, amount: u64) -> Option<u128> {
        if self.shares == 0 {
            return Some(amount as u128);
        }
        if self.stake == 0 {
            return None;
        }
        mul_div(amount as u128, self.shares, self.stake as u128).filter(|s| self.shares.checked_add(*s).is_some())
    }
}

// ----------------------------------------------------------------------- journaled mutation

fn pool_mut<'s>(state: &'s mut State, j: &mut Journal, v: &str) -> Option<&'s mut Pool> {
    j.staking.pools.touch(&state.staking.pools, v);
    state.staking.pools.get_mut(v)
}

fn put_pool(state: &mut State, j: &mut Journal, v: &str, pool: Pool) {
    j.staking.pools.touch(&state.staking.pools, v);
    state.staking.pools.insert(v.to_string(), pool);
}

fn put_delegation(state: &mut State, j: &mut Journal, key: &str, d: Option<Delegation>) {
    j.staking.delegations.touch(&state.staking.delegations, key);
    match d {
        Some(d) => state.staking.delegations.insert(key.to_string(), d),
        None => state.staking.delegations.remove(key),
    };
}

fn put_entry(state: &mut State, j: &mut Journal, key: &str, e: Option<Entry>) {
    j.staking.entries.touch(&state.staking.entries, key);
    match e {
        Some(e) => state.staking.entries.insert(key.to_string(), e),
        None => state.staking.entries.remove(key),
    };
}

fn add_entry(state: &mut State, j: &mut Journal, delegator: &str, e: Entry) {
    if j.staking.next_entry_id.is_none() {
        j.staking.next_entry_id = Some(state.staking.next_entry_id);
    }
    let id = state.staking.next_entry_id;
    state.staking.next_entry_id += 1;
    put_entry(state, j, &ekey(delegator, id), Some(e));
}

fn put_liveness(state: &mut State, j: &mut Journal, v: &str, l: Option<Liveness>) {
    j.staking.liveness.touch(&state.staking.liveness, v);
    match l {
        Some(l) => state.staking.liveness.insert(v.to_string(), l),
        None => state.staking.liveness.remove(v),
    };
}

fn put_punished(state: &mut State, j: &mut Journal, key: &str, p: Option<Punished>) {
    j.staking.punished.touch(&state.staking.punished, key);
    match p {
        Some(p) => state.staking.punished.insert(key.to_string(), p),
        None => state.staking.punished.remove(key),
    };
}

// ----------------------------------------------------------------------------- pool moves

/// Buys shares of `validator`'s pool with `amount` drops (already out of the delegator's
/// balance), carrying `principal` (see [`Delegation::principal`]). Returns the shares.
fn bond(state: &mut State, j: &mut Journal, delegator: &str, validator: &str, amount: u64, principal: u64) -> Result<u128> {
    let pool = state.staking.pools.get(validator).ok_or_else(|| anyhow!("validator {} has no pool", short(validator)))?;
    let shares = pool.shares_for(amount).ok_or_else(|| anyhow!("validator {}'s pool was slashed to nothing; it can't take stake", short(validator)))?;
    if shares == 0 {
        bail!("{} is too little to buy a share of validator {}'s pool", fmt_fang(amount), short(validator));
    }
    let stake = pool.stake.checked_add(amount).ok_or_else(|| anyhow!("pool overflow"))?;
    let p = pool_mut(state, j, validator).expect("checked above");
    p.stake = stake;
    p.shares += shares;
    let key = dkey(delegator, validator);
    let mut d = state.staking.delegations.get(&key).cloned().unwrap_or_default();
    d.shares += shares;
    d.principal = d.principal.saturating_add(principal);
    put_delegation(state, j, &key, Some(d));
    Ok(shares)
}

/// Sells `shares` of `delegator`'s stake in `validator`'s pool. Returns what they were worth
/// (out of the pool, in nobody's balance yet) and the principal they carried.
fn unbond(state: &mut State, j: &mut Journal, delegator: &str, validator: &str, shares: u128) -> Result<(u64, u64)> {
    let key = dkey(delegator, validator);
    let d = state.staking.delegations.get(&key).cloned().ok_or_else(|| anyhow!("{} has no stake with validator {}", short(delegator), short(validator)))?;
    if shares > d.shares {
        bail!("{} holds {} shares of validator {}'s pool, not {shares}", short(delegator), d.shares, short(validator));
    }
    let value = state.staking.pools.get(validator).map(|p| p.value(shares)).unwrap_or(0);
    let p = pool_mut(state, j, validator).ok_or_else(|| anyhow!("validator {} has no pool", short(validator)))?;
    p.stake -= value;
    p.shares -= shares;
    let principal = if shares == d.shares { d.principal } else { mul_div(d.principal as u128, shares, d.shares).unwrap_or(0) as u64 };
    let rest = Delegation { shares: d.shares - shares, principal: d.principal - principal };
    put_delegation(state, j, &key, (rest.shares > 0).then_some(rest));
    Ok((value, principal))
}

/// Takes up to `want` drops of `delegator`'s stake in `validator`'s pool by burning its shares
/// (rounded up, so the others' shares lose nothing). Returns what it took (out of the pool).
fn take_from_delegation(state: &mut State, j: &mut Journal, delegator: &str, validator: &str, want: u64) -> u64 {
    let key = dkey(delegator, validator);
    let (Some(d), Some(pool)) = (state.staking.delegations.get(&key).cloned(), state.staking.pools.get(validator)) else { return 0 };
    let worth = pool.value(d.shares);
    if worth == 0 || want == 0 {
        return 0;
    }
    let (shares, taken) = if want >= worth { (d.shares, worth) } else { (mul_div_ceil(want as u128, pool.shares, pool.stake as u128).unwrap_or(d.shares).min(d.shares), want) };
    let p = pool_mut(state, j, validator).expect("checked above");
    p.stake -= taken;
    p.shares -= shares;
    let principal = if shares == d.shares { d.principal } else { mul_div(d.principal as u128, shares, d.shares).unwrap_or(0) as u64 };
    let rest = Delegation { shares: d.shares - shares, principal: d.principal - principal };
    put_delegation(state, j, &key, (rest.shares > 0).then_some(rest));
    taken
}

/// Whether `validator`'s pool takes new stake: registered, not removed or tombstoned, not slashed
/// to nothing.
fn accepting(state: &State, validator: &str) -> Result<()> {
    let rec = state.validators.get(validator).ok_or_else(|| anyhow!("no validator {}", short(validator)))?;
    match rec.status {
        ValidatorStatus::Removed => bail!("validator {} was removed", short(validator)),
        ValidatorStatus::Tombstoned => bail!("validator {} double-signed and is tombstoned", short(validator)),
        ValidatorStatus::Candidate | ValidatorStatus::Active => {}
    }
    let pool = state.staking.pools.get(validator).ok_or_else(|| anyhow!("validator {} has no pool", short(validator)))?;
    if pool.shares > 0 && pool.stake == 0 {
        bail!("validator {}'s pool was slashed to nothing; it can't take stake", short(validator));
    }
    Ok(())
}

/// Whether `account` holds a vesting lock that still locks something at `height`.
fn has_unvested_lock(state: &State, account: &str, height: u64) -> bool {
    // Vesting locks only: vote locks (`LockAmount::Until`) don't make an account an insider.
    state.accounts.get(account).is_some_and(|a| a.unvested_at(height) > 0)
}

/// 3.7: an account with unvested tokens holds at most `insider_delegation_cap_bps` of the
/// supply in any one pool.
fn check_insider_cap(state: &State, account: &str, validator: &str, height: u64) -> Result<()> {
    if !has_unvested_lock(state, account, height) {
        return Ok(());
    }
    let bps = state.chain_params.insider_delegation_cap_bps;
    let cap = bps_of(state.supply, bps);
    let held = delegation_value(state, account, validator);
    if held > cap {
        bail!("an account with unvested tokens can stake at most {} ({}% of the supply) with one validator", fmt_fang(cap), fmt_bps(bps));
    }
    Ok(())
}

/// 3.7: what staking earned on an account with an unvested vesting lock vests with it (added to
/// the lock's total, so it unlocks on the same curve: `econ::lock_rewards`).
fn lock_gains(state: &mut State, j: &mut Journal, account: &str, gain: u64, height: u64) {
    crate::econ::lock_rewards(state, j, account, gain, height);
}

/// When stake leaving at `height` is free: after `unbonding_blocks`, and never before evidence
/// against the epoch it left in can no longer arrive (it answers for that epoch's faults: at
/// most one more epoch, since `evidence_max_age_epochs` epochs fit in the unbonding period).
fn release_height(state: &State, height: u64) -> u64 {
    let e = state.params.epoch_blocks.max(2);
    let window_closes = (state.params.epoch_of(height).saturating_add(state.chain_params.evidence_max_age_epochs).saturating_add(1)).saturating_mul(e);
    height.saturating_add(state.chain_params.unbonding_blocks).max(window_closes)
}

fn entries_of<'s>(state: &'s State, delegator: &str) -> impl Iterator<Item = (&'s String, &'s Entry)> + 's {
    owned_by(&state.staking.entries, delegator)
}

fn room_for_entry(state: &State, delegator: &str) -> Result<()> {
    if entries_of(state, delegator).count() >= MAX_ENTRIES {
        bail!("{} has {MAX_ENTRIES} unbonding and redelegation entries; withdraw the finished ones (WithdrawUnbonded) or wait", short(delegator));
    }
    Ok(())
}

// --------------------------------------------------------------------------------- actions

/// Tags 30–36: Delegate, Undelegate, Redelegate, WithdrawUnbonded, UpdateValidator, Unjail,
/// ReportEquivocation. Session keys never get here (admission lets them send transfers only).
pub(crate) fn apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, _key: &KeyRecord, height: u64) -> Result<()> {
    match &tx.action {
        Action::Delegate { validator, amount } => {
            accepting(state, validator)?;
            if !state.staking.delegations.contains_key(&dkey(sender, validator)) && *amount < MIN_DELEGATION {
                bail!("a new delegation is at least {}", fmt_fang(MIN_DELEGATION));
            }
            state.j_debit(j, sender, *amount)?;
            bond(state, j, sender, validator, *amount, *amount)?;
            check_insider_cap(state, sender, validator, height)
        }
        Action::Undelegate { validator, shares } => {
            room_for_entry(state, sender)?;
            let (value, principal) = unbond(state, j, sender, validator, *shares as u128)?;
            lock_gains(state, j, sender, value.saturating_sub(principal), height);
            if value > 0 {
                let release_at = release_height(state, height);
                add_entry(state, j, sender, Entry::Unbonding { validator: validator.clone(), amount: value, started_at: height, release_at });
            }
            Ok(())
        }
        Action::Redelegate { from_validator: from, to_validator: to, shares } => {
            accepting(state, to)?;
            if let Some(e) = entries_of(state, sender).find_map(|(_, e)| match e {
                Entry::Redelegation { to: t, release_at, .. } if t == from && *release_at > height => Some(*release_at),
                _ => None,
            }) {
                bail!("your stake with validator {} arrived by redelegation; it can't move on before block {e}", short(from));
            }
            room_for_entry(state, sender)?;
            let (value, principal) = unbond(state, j, sender, from, *shares as u128)?;
            if value == 0 {
                bail!("those shares are worth nothing");
            }
            bond(state, j, sender, to, value, principal)?;
            check_insider_cap(state, sender, to, height)?;
            let release_at = release_height(state, height);
            add_entry(state, j, sender, Entry::Redelegation { from: from.clone(), to: to.clone(), amount: value, started_at: height, release_at });
            Ok(())
        }
        Action::WithdrawUnbonded {} => {
            let ready: Vec<(String, Entry)> = entries_of(state, sender).filter(|(_, e)| e.release_at() <= height).map(|(k, e)| (k.clone(), e.clone())).collect();
            if ready.is_empty() {
                match entries_of(state, sender).map(|(_, e)| e.release_at()).min() {
                    Some(h) => bail!("nothing has finished unbonding yet (the next entry is released at block {h})"),
                    None => bail!("{} has nothing unbonding", short(sender)),
                }
            }
            for (key, e) in ready {
                if let Entry::Unbonding { amount, .. } = e {
                    state.j_credit(j, sender, amount);
                }
                put_entry(state, j, &key, None);
            }
            Ok(())
        }
        Action::UpdateValidator { commission_bps, p2p_addr, http_addr } => {
            let v = owned_validator(state, sender, signer)?;
            let (max, step) = (state.chain_params.commission_max_bps, state.chain_params.commission_max_change_bps);
            if *commission_bps > max {
                bail!("commission can be at most {}%", fmt_bps(max));
            }
            let epoch = state.params.epoch_of(height);
            let current = state.validators[&v].commission_bps;
            let pool = state.staking.pools.get(&v).cloned().unwrap_or_default();
            let base = if pool.commission_epoch == epoch { pool.commission_base } else { current };
            if commission_bps.abs_diff(base) > step {
                bail!("commission can move at most {} points per epoch: {}%–{}% this epoch", fmt_bps(step), fmt_bps(base.saturating_sub(step)), fmt_bps((base + step).min(max)));
            }
            if !p2p_addr.is_empty() {
                crate::chain::check_host_port(p2p_addr).context("p2p_addr")?;
            }
            if !http_addr.is_empty() {
                crate::chain::check_http_addr(http_addr).context("http_addr")?;
            }
            if let Some(p) = pool_mut(state, j, &v) {
                (p.commission_base, p.commission_epoch) = (base, epoch);
            }
            let rec = state.validator_mut(j, &v).expect("owned_validator checked it");
            rec.commission_bps = *commission_bps;
            if !p2p_addr.is_empty() {
                rec.p2p_addr = p2p_addr.clone();
            }
            if !http_addr.is_empty() {
                rec.http_addr = http_addr.clone();
            }
            Ok(())
        }
        Action::Unjail { validator } => {
            let rec = state.validators.get(validator).ok_or_else(|| anyhow!("no validator {}", short(validator)))?;
            if rec.owner != sender && validator != sender {
                bail!("only validator {}'s owner can unjail it", short(validator));
            }
            let pool = state.staking.pools.get(validator).cloned().unwrap_or_default();
            if pool.tombstoned_at.is_some() {
                bail!("validator {} double-signed: it's tombstoned and can never return", short(validator));
            }
            let Some(jail) = &pool.jail else { bail!("validator {} isn't jailed", short(validator)) };
            if height < jail.until {
                bail!("validator {} is jailed until block {}", short(validator), jail.until);
            }
            // Unjailed but under-bonded, it would never be selected: say so now.
            let (bond, min) = (self_bond(state, validator), state.chain_params.min_self_bond);
            if bond < min {
                bail!("validator {}'s self-bond ({}) is below the minimum ({}): its owner must delegate to it first", short(validator), fmt_fang(bond), fmt_fang(min));
            }
            pool_mut(state, j, validator).expect("checked above").jail = None;
            // A fresh window: its old misses don't jail it again at once.
            put_liveness(state, j, validator, None);
            Ok(())
        }
        Action::ReportEquivocation { evidence } => {
            let bytes = hex::decode(evidence).map_err(|_| anyhow!("evidence must be hex"))?;
            let fault = verify_evidence(state, &tx.chain_id, &bytes, height)?;
            let key = pkey(&fault.validator, fault.epoch, fault.view);
            if state.staking.punished.contains_key(&key) {
                bail!("validator {} was already punished for epoch {}, view {}", short(&fault.validator), fault.epoch, fault.view);
            }
            // Stake that was in the pool when the epoch began answers for it, wherever it went since.
            let since = fault.epoch.saturating_mul(state.params.epoch_blocks.max(2));
            let slashed = slash(state, j, &fault.validator, since, state.chain_params.slash_double_sign_bps);
            let owner = state.validators.get(&fault.validator).map(|r| r.owner.clone()).unwrap_or_default();
            // No reward for reporting yourself.
            let reward = if sender == owner || sender == fault.validator { 0 } else { bps_of(slashed, state.chain_params.reporter_reward_bps) };
            state.j_credit(j, sender, reward);
            state.j_burn(j, slashed - reward);
            tombstone(state, j, &fault.validator, height);
            put_punished(state, j, &key, Some(Punished { kind: fault.kind, height, reporter: sender.to_string(), slashed }));
            Ok(())
        }
        _ => bail!("not a staking action"),
    }
}

/// The validator an `UpdateValidator` means: the one whose consensus key signed it (for itself or
/// its owner), else the one the sender is, else the only one the sender owns.
fn owned_validator(state: &State, sender: &str, signer: &str) -> Result<String> {
    if let Some(v) = validator_of_key(state, signer).filter(|v| state.validators.get(v).is_some_and(|r| *v == sender || r.owner == sender)) {
        return Ok(v);
    }
    if state.validators.get(sender).is_some_and(|r| r.owner == sender) {
        return Ok(sender.to_string());
    }
    let owned: Vec<&ValidatorRecord> = state.validators.values().filter(|r| r.owner == sender && !matches!(r.status, ValidatorStatus::Removed | ValidatorStatus::Tombstoned)).collect();
    match owned.as_slice() {
        [] => bail!("{} doesn't own a validator", short(sender)),
        [one] => Ok(one.address.clone()),
        _ => bail!("{} owns several validators: sign with the consensus key of the one to change", short(sender)),
    }
}

/// The registered validator whose consensus key has this key id, if any.
fn validator_of_key(state: &State, key_id: &str) -> Option<String> {
    let bytes: [u8; 32] = hex::decode(key_id).ok()?.try_into().ok()?;
    let v = crate::crypto::render_address(state.params.prefix(), &bytes);
    state.validators.contains_key(&v).then_some(v)
}

/// `RegisterValidator` (tag 4): a candidate that owns a pool with the sender's self-bond.
/// Permissionless: it joins the set at an epoch boundary if its stake ranks.
pub(crate) fn register(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, key: &KeyRecord, height: u64) -> Result<()> {
    let Action::RegisterValidator { consensus_key, p2p_addr, http_addr, bond: self_bond, pop } = &tx.action else { bail!("not a registration") };
    if key.role != KeyRole::Owner {
        bail!("only owner keys can register validators");
    }
    if !crate::crypto::is_lower_hex(consensus_key) {
        bail!("consensus_key must be lowercase hex");
    }
    let pk = hex::decode(consensus_key).map_err(|_| anyhow!("consensus_key must be hex"))?;
    if pk.len() != crate::crypto::PUBLIC_KEY_LEN {
        bail!("consensus_key must be an ML-DSA-65 public key ({} bytes)", crate::crypto::PUBLIC_KEY_LEN);
    }
    crate::chain::check_host_port(p2p_addr).context("p2p_addr")?;
    crate::chain::check_http_addr(http_addr).context("http_addr")?;
    if *self_bond < state.chain_params.min_self_bond {
        bail!("bond must be at least {}", fmt_fang(state.chain_params.min_self_bond));
    }
    let address = crate::crypto::address_with_prefix(state.params.prefix(), &pk);
    if state.validators.contains_key(&address) {
        bail!("that consensus key is already registered (its owner can delegate to it to bond more)");
    }
    crate::chain::verify_pop(&tx.chain_id, sender, consensus_key, p2p_addr, http_addr, pop)?;
    state.j_debit(j, sender, *self_bond)?;
    let epoch = state.params.epoch_of(height);
    let rec = ValidatorRecord { address: address.clone(), owner: sender.to_string(), consensus_key: consensus_key.clone(), p2p_addr: p2p_addr.clone(), http_addr: http_addr.clone(), status: ValidatorStatus::Candidate, registered_at: height, approved_at: None, approved_by: None, active_from: None, removed_at: None, removed_by: None, active_until: None, commission_bps: 0 };
    state.insert_validator(j, rec);
    put_pool(state, j, &address, Pool { commission_epoch: epoch, ..Default::default() });
    bond(state, j, sender, &address, *self_bond, *self_bond)?;
    check_insider_cap(state, sender, &address, height)
}

/// A genesis operator's pool: the self-bond (already out of the owner's balance) as the owner's
/// shares, and the operator's commission. Genesis only (no journal).
pub fn genesis_pool(state: &mut State, validator: &str, self_bond: u64) {
    let mut j = Journal::default();
    let (owner, commission) = state.validators.get(validator).map(|r| (r.owner.clone(), r.commission_bps)).expect("a genesis validator");
    put_pool(state, &mut j, validator, Pool { commission_base: commission, ..Default::default() });
    if self_bond > 0 {
        bond(state, &mut j, &owner, validator, self_bond, self_bond).expect("an empty pool takes any amount");
    }
}

/// The admin levers of dev and test chains (`ApproveValidator`, `RemoveValidator`): on chains
/// without admins (mainnet) both are refused, since validators join and leave by stake.
fn need_admin(state: &State, sender: &str, what: &str) -> Result<()> {
    if state.params.admins.is_empty() {
        bail!("validators join and leave by stake on this chain: register and delegate (admins {what} only on dev and test chains)");
    }
    if !state.params.admins.iter().any(|a| a == sender) {
        bail!("only validator admins can {what}");
    }
    Ok(())
}

/// `ApproveValidator` (dev and test chains): pins a validator, ranking it ahead of stake.
pub(crate) fn approve(state: &mut State, j: &mut Journal, sender: &str, validator: &str, height: u64) -> Result<()> {
    need_admin(state, sender, "approve validators")?;
    let rec = state.validators.get(validator).ok_or_else(|| anyhow!("no such validator"))?;
    if !matches!(rec.status, ValidatorStatus::Candidate | ValidatorStatus::Active) {
        bail!("validator is {:?}", rec.status);
    }
    if rec.approved_at.is_some() {
        bail!("validator {} is already approved", short(validator));
    }
    let rec = state.validator_mut(j, validator).expect("checked above");
    rec.approved_at = Some(height);
    rec.approved_by = Some(sender.to_string());
    Ok(())
}

/// `RemoveValidator` (dev and test chains): out at the next boundary, never selected again. Its
/// delegators undelegate as usual.
pub(crate) fn remove(state: &mut State, j: &mut Journal, sender: &str, validator: &str, height: u64) -> Result<()> {
    need_admin(state, sender, "remove validators")?;
    let rec = state.validators.get(validator).ok_or_else(|| anyhow!("no such validator"))?;
    if !matches!(rec.status, ValidatorStatus::Candidate | ValidatorStatus::Active) {
        bail!("validator is already {:?}", rec.status);
    }
    if !state.validators.values().any(|r| r.address != validator && matches!(r.status, ValidatorStatus::Candidate | ValidatorStatus::Active)) {
        bail!("can't remove the last validator");
    }
    let rec = state.validator_mut(j, validator).expect("checked above");
    rec.status = ValidatorStatus::Removed;
    rec.removed_at = Some(height);
    rec.removed_by = Some(sender.to_string());
    Ok(())
}

// --------------------------------------------------------------------------------- slashing

/// Takes `bps` of `validator`'s pool, and of the unbonding and redelegation entries that left it
/// at or after height `since`. Returns the total, which is out of every balance and escrow: the
/// caller burns it (or pays part of it to a reporter).
fn slash(state: &mut State, j: &mut Journal, validator: &str, since: u64, bps: u64) -> u64 {
    if bps == 0 {
        return 0;
    }
    let mut total = 0u64;
    if let Some(cut) = state.staking.pools.get(validator).map(|p| bps_of(p.stake, bps)).filter(|c| *c > 0) {
        let p = pool_mut(state, j, validator).expect("checked above");
        p.stake -= cut;
        p.slashed += cut;
        total += cut;
    }
    // Entries are few and slashes rare: a scan.
    let hits: Vec<(String, Entry)> = state
        .staking
        .entries
        .iter()
        .filter(|(_, e)| match e {
            Entry::Unbonding { validator: v, started_at, .. } => v == validator && *started_at >= since,
            Entry::Redelegation { from, started_at, .. } => from == validator && *started_at >= since,
        })
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    for (key, e) in hits {
        let delegator = key.split('/').next().unwrap_or_default().to_string();
        match e {
            Entry::Unbonding { validator: v, amount, started_at, release_at } => {
                let cut = bps_of(amount, bps);
                if cut > 0 {
                    put_entry(state, j, &key, Some(Entry::Unbonding { validator: v, amount: amount - cut, started_at, release_at }));
                    total += cut;
                }
            }
            Entry::Redelegation { to, amount, started_at, .. } => {
                // From the stake it moved into `to`, then from that stake's own way out.
                let due = bps_of(amount, bps);
                let mut taken = take_from_delegation(state, j, &delegator, &to, due);
                if taken < due {
                    let later: Vec<(String, Entry)> = entries_of(state, &delegator)
                        .filter(|(_, e)| matches!(e, Entry::Unbonding { validator: v, started_at: s, .. } if *v == to && *s >= started_at))
                        .map(|(k, e)| (k.clone(), e.clone()))
                        .collect();
                    for (k, e) in later {
                        let Entry::Unbonding { validator: v, amount: a, started_at: s, release_at: r } = e else { continue };
                        let cut = a.min(due - taken);
                        put_entry(state, j, &k, Some(Entry::Unbonding { validator: v, amount: a - cut, started_at: s, release_at: r }));
                        taken += cut;
                        if taken == due {
                            break;
                        }
                    }
                }
                total += taken;
            }
        }
    }
    total
}

/// Jails `validator`: slashes `slash_bps` of its pool (burned), and it leaves the set at the
/// next boundary; `Unjail` brings it back after `jail_blocks`. Tombstoned or missing validators
/// are left alone; jailing a jailed validator slashes again and keeps the later release.
/// Downtime calls it, and so does the beacon (P-BEACON) for commitments never revealed.
pub fn jail(state: &mut State, j: &mut Journal, validator: &str, height: u64, slash_bps: u64, reason: &str) {
    if state.staking.pools.get(validator).is_none_or(|p| p.tombstoned_at.is_some()) {
        return;
    }
    let slashed = slash(state, j, validator, height, slash_bps);
    state.j_burn(j, slashed);
    let until = height.saturating_add(state.chain_params.jail_blocks);
    let p = pool_mut(state, j, validator).expect("checked above");
    let until = p.jail.as_ref().map_or(until, |old| old.until.max(until));
    p.jail = Some(Jail { at: height, until, reason: reason.chars().take(MAX_REASON).collect() });
}

fn tombstone(state: &mut State, j: &mut Journal, validator: &str, height: u64) {
    if let Some(p) = pool_mut(state, j, validator) {
        p.tombstoned_at.get_or_insert(height);
    }
    if let Some(r) = state.validator_mut(j, validator) {
        r.status = ValidatorStatus::Tombstoned;
    }
}

// ---------------------------------------------------------------------------------- evidence

/// The rng `MlDsaScheme` verification takes and ignores (its signatures are deterministic and
/// never batched), so checking evidence is a pure function of the state.
pub struct NoRng;

impl rand_core::TryRng for NoRng {
    type Error = std::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(0)
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(0)
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        dst.fill(0);
        Ok(())
    }
}
impl rand_core::TryCryptoRng for NoRng {}

/// Equivocation evidence, decoded: an encoded simplex `Activity` of one of the three fault kinds.
pub struct Evidence {
    pub kind: u8,
    pub epoch: u64,
    pub view: u64,
    /// The offender's index in the epoch's participants (sorted consensus keys).
    pub signer: usize,
    pub activity: Activity<MlDsaScheme, Digest>,
}

/// Decodes evidence with Commonware's codec (a consensus format now: see the golden vector in
/// the tests): one of the three fault kinds, both votes, nothing after. Bounded: a fault holds two
/// votes of fixed-size parts. Signatures aren't checked here.
pub fn decode_evidence(bytes: &[u8]) -> Result<Evidence> {
    use commonware_codec::Decode as _;
    ensure!(bytes.len() <= MAX_EVIDENCE_BYTES, "evidence is at most {MAX_EVIDENCE_BYTES} bytes");
    let kind = *bytes.first().ok_or_else(|| anyhow!("empty evidence"))?;
    if !(CONFLICTING_NOTARIZE..=NULLIFY_FINALIZE).contains(&kind) {
        bail!("evidence must be a ConflictingNotarize, ConflictingFinalize or NullifyFinalize");
    }
    // The certificate size bound only applies to certificate activities, which are refused above.
    let activity = Activity::<MlDsaScheme, Digest>::decode_cfg(bytes, &1).map_err(|e| anyhow!("malformed evidence: {e}"))?;
    let (epoch, view, signer) = match &activity {
        Activity::ConflictingNotarize(c) => (c.epoch(), c.view(), c.signer()),
        Activity::ConflictingFinalize(c) => (c.epoch(), c.view(), c.signer()),
        Activity::NullifyFinalize(c) => (c.epoch(), c.view(), c.signer()),
        _ => bail!("not fault evidence"),
    };
    Ok(Evidence { kind, epoch: epoch.get(), view: view.get(), signer: usize::from(signer), activity })
}

/// Who double-signed, when, and how (verified).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub kind: u8,
    pub validator: String,
    pub epoch: u64,
    pub view: u64,
}

/// The epoch's participants as simplex orders them (sorted consensus keys), with their addresses.
fn participants(state: &State, epoch: u64) -> Result<commonware_utils::ordered::Set<crate::pq::PublicKey>> {
    let set = state.active_sets.get(&epoch).ok_or_else(|| anyhow!("epoch {epoch}'s validator set is no longer in state"))?;
    let keys: Vec<&str> = set.iter().filter_map(|a| state.validators.get(a)).map(|v| v.consensus_key.as_str()).collect();
    ensure!(keys.len() == set.len(), "epoch {epoch}'s set names an unknown validator");
    crate::consensus::keys_of(keys.into_iter()).ok_or_else(|| anyhow!("epoch {epoch}'s set has a bad consensus key"))
}

/// Checks equivocation evidence at `height`: well-formed ([`decode_evidence`]), from an epoch no
/// later than now and at most `evidence_max_age_epochs` old whose set the state still holds, and
/// both votes signed by the same member of that set under this chain's consensus namespace.
pub fn verify_evidence(state: &State, chain_id: &str, bytes: &[u8], height: u64) -> Result<Fault> {
    let e = decode_evidence(bytes)?;
    let now = state.params.epoch_of(height);
    if e.epoch > now {
        bail!("the evidence is from epoch {}, after this one ({now})", e.epoch);
    }
    let max_age = state.chain_params.evidence_max_age_epochs;
    if now - e.epoch > max_age {
        bail!("the evidence is from epoch {}: too old (at most {max_age} epochs back)", e.epoch);
    }
    let set = participants(state, e.epoch)?;
    let pk = set.get(e.signer).ok_or_else(|| anyhow!("signer {} isn't in epoch {}'s set of {}", e.signer, e.epoch, set.len()))?.clone();
    let scheme = MlDsaScheme::verifier(&crate::consensus::namespace(chain_id), set);
    if !e.activity.verify(&mut NoRng, &scheme, &commonware_parallel::Sequential) {
        bail!("the evidence's signatures don't verify against epoch {}'s validators", e.epoch);
    }
    Ok(Fault { kind: e.kind, validator: pk.address(state.params.prefix()), epoch: e.epoch, view: e.view })
}

/// Where a piece of evidence stands at `height` (the node's evidence task asks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceStatus {
    /// Valid, and the fault it proves isn't punished yet: submit it.
    Open(Fault),
    Punished,
    /// From an epoch this state hasn't reached: ask again later.
    NotYet,
    /// Malformed, forged, or too old: it never will count.
    Never,
}

pub fn evidence_status(state: &State, chain_id: &str, bytes: &[u8], height: u64) -> EvidenceStatus {
    let Ok(e) = decode_evidence(bytes) else { return EvidenceStatus::Never };
    if e.epoch > state.params.epoch_of(height) {
        return EvidenceStatus::NotYet;
    }
    match verify_evidence(state, chain_id, bytes, height) {
        Ok(f) if state.staking.punished.contains_key(&pkey(&f.validator, f.epoch, f.view)) => EvidenceStatus::Punished,
        Ok(f) => EvidenceStatus::Open(f),
        Err(_) => EvidenceStatus::Never,
    }
}

// --------------------------------------------------------------------------- end of block

/// After `poc_end_block` and `econ::end_block`, every block: the proposer pays for the block's
/// `skipped` txs (junk), then the leader slots since the parent's view are scored (downtime).
pub fn end_block(state: &mut State, b: &BlockInfo) {
    let mut j = Journal::default();
    charge_junk(state, &mut j, b);
    score_leader_slots(state, &mut j, b);
}

/// CV-5: every tx the proposer included that couldn't be admitted costs it the base fee for its
/// size, from its account first, then its pool's self-bond; burned. What it can't pay is waived.
fn charge_junk(state: &mut State, j: &mut Journal, b: &BlockInfo) {
    if b.skipped.is_empty() {
        return;
    }
    let due = b.skipped.iter().fold(0u64, |acc, size| acc.saturating_add(crate::econ::base_fee(state, b.height, *size)));
    let from_balance = due.min(state.account(b.proposer).balance);
    if from_balance > 0 {
        state.j_debit(j, b.proposer, from_balance).expect("checked the balance");
    }
    let mut taken = from_balance;
    if let Some(owner) = state.validators.get(b.proposer).map(|r| r.owner.clone()).filter(|_| taken < due) {
        taken += take_from_delegation(state, j, &owner, b.proposer, due - taken);
    }
    if let Some(p) = pool_mut(state, j, b.proposer) {
        p.junk_charged = p.junk_charged.saturating_add(taken);
    }
    state.j_burn(j, taken);
}

/// The leader of `(epoch, view)` among `n` participants: an index into the epoch's sorted
/// consensus keys, from simplex's own `RoundRobin` elector shuffled by the epoch's beacon
/// (`consensus::elector(seed)`, the one the engines run; `seed` is `beacon::elector_seed`), so
/// the chain charges exactly the validators consensus waited for. Electors depend only on the
/// seed and the set's size; they're built once and kept (a few epochs' worth).
pub fn leader_index(seed: Option<[u8; 32]>, epoch: u64, view: u64, n: usize) -> usize {
    use commonware_consensus::simplex::elector::{Config as _, Elector as _, RoundRobinElector};
    use commonware_consensus::types::{Epoch, Round, View};
    type Electors = BTreeMap<(Option<[u8; 32]>, usize), RoundRobinElector<MlDsaScheme>>;
    static KEYS: std::sync::Mutex<BTreeMap<usize, commonware_utils::ordered::Set<crate::pq::PublicKey>>> = std::sync::Mutex::new(BTreeMap::new());
    static ELECTORS: std::sync::Mutex<Electors> = std::sync::Mutex::new(BTreeMap::new());
    assert!(n > 0, "no participants");
    let mut electors = ELECTORS.lock().unwrap_or_else(|e| e.into_inner());
    if electors.len() >= 64 && !electors.contains_key(&(seed, n)) {
        electors.clear();
    }
    let elector = electors.entry((seed, n)).or_insert_with(|| {
        // Any n distinct keys: a round-robin elector only looks at how many there are.
        let mut keys = KEYS.lock().unwrap_or_else(|e| e.into_inner());
        let set = keys.entry(n).or_insert_with(|| commonware_utils::ordered::Set::from_iter_dedup((0..n).map(|i| commonware_cryptography::Signer::public_key(&crate::pq::PrivateKey::from_seed_bytes(crate::crypto::hash(&(i as u64).to_le_bytes()))))));
        crate::consensus::elector(seed).build(set)
    });
    usize::from(elector.elect(Round::new(Epoch::new(epoch), View::new(view)), None))
}

/// The epoch's validators in simplex's participant order (by consensus key).
fn leaders(state: &State, epoch: u64) -> Vec<String> {
    let mut set: Vec<(&str, &String)> = state.active_sets.get(&epoch).map(|s| s.iter().filter_map(|a| state.validators.get(a).map(|v| (v.consensus_key.as_str(), a))).collect()).unwrap_or_default();
    // Lowercase hex sorts like the raw bytes `PublicKey`s compare by.
    set.sort();
    set.into_iter().map(|(_, a)| a.clone()).collect()
}

/// Downtime (3.4): views `parent_view + 1 .. view − 1` were nullified, each a miss for its leader;
/// the block's own view is a hit for its leader. Within an epoch, any n consecutive views lead
/// each validator once, so long gaps cost O(n). A validator over `downtime_max_missed` in its
/// window is jailed.
fn score_leader_slots(state: &mut State, j: &mut Journal, b: &BlockInfo) {
    if b.view == 0 || b.view <= b.parent_view {
        return; // no consensus view (genesis, or a block built outside consensus)
    }
    let epoch = state.params.epoch_of(b.height);
    let order = leaders(state, epoch);
    let n = order.len();
    if n == 0 {
        return;
    }
    let seed = crate::beacon::elector_seed(state, epoch);
    let mut misses = vec![0u64; n];
    let gap = b.view - b.parent_view - 1;
    let (laps, rest) = (gap / n as u64, gap % n as u64);
    for m in misses.iter_mut() {
        *m = laps;
    }
    for x in b.view - rest..b.view {
        misses[leader_index(seed, epoch, x, n)] += 1;
    }
    let hit = leader_index(seed, epoch, b.view, n);
    let (window, max_missed) = (state.chain_params.downtime_window.max(1), state.chain_params.downtime_max_missed);
    for (i, v) in order.iter().enumerate() {
        if misses[i] == 0 && i != hit {
            continue;
        }
        let Some(pool) = state.staking.pools.get(v) else { continue };
        // Jailed or tombstoned: already on its way out.
        if pool.jail.is_some() || pool.tombstoned_at.is_some() {
            continue;
        }
        let mut l = state.staking.liveness.get(v).filter(|l| l.window == window).cloned().unwrap_or_else(|| Liveness::new(window));
        l.miss(misses[i]);
        if i == hit {
            l.push(false);
        }
        let over = l.missed > max_missed;
        let reason = format!("missed {} of its last {} leader slots", l.missed, l.filled);
        put_liveness(state, j, v, Some(l));
        if over {
            jail(state, j, v, b.height, state.chain_params.slash_downtime_bps, &reason);
        }
    }
}

// --------------------------------------------------------------------------------- the set

/// Whether a validator may be selected: registered, not removed or tombstoned, not jailed, and
/// its self-bond at least `min_self_bond`.
pub fn eligible(state: &State, validator: &str) -> bool {
    let Some(rec) = state.validators.get(validator) else { return false };
    let Some(pool) = state.staking.pools.get(validator) else { return false };
    matches!(rec.status, ValidatorStatus::Candidate | ValidatorStatus::Active) && pool.jail.is_none() && pool.tombstoned_at.is_none() && self_bond(state, validator) >= state.chain_params.min_self_bond
}

/// What the owner's shares of its own pool are worth.
pub fn self_bond(state: &State, validator: &str) -> u64 {
    state.validators.get(validator).map(|r| delegation_value(state, &r.owner, validator)).unwrap_or(0)
}

/// What `delegator`'s shares of `validator`'s pool are worth.
pub fn delegation_value(state: &State, delegator: &str, validator: &str) -> u64 {
    match (state.staking.delegations.get(&dkey(delegator, validator)), state.staking.pools.get(validator)) {
        (Some(d), Some(p)) => p.value(d.shares),
        _ => 0,
    }
}

/// Ranking for the set: admin-pinned first (dev and test chains), then pool stake, then address
/// (ascending). Greater is better.
fn rank<'a>(state: &State, validator: &'a str) -> (bool, u64, std::cmp::Reverse<&'a str>) {
    let pinned = state.validators.get(validator).is_some_and(|r| r.approved_at.is_some());
    (pinned, state.staking.pools.get(validator).map_or(0, |p| p.stake), std::cmp::Reverse(validator))
}

/// The set for `epoch + 1` if `epoch` ended now, with who joins and who leaves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Sorted by address.
    pub next: Vec<String>,
    pub joining: Vec<String>,
    pub leaving: Vec<String>,
}

/// The churn limit (CV-4): most members of a set of `m` that may be newcomers, ⌊(m−1)/3⌋, so the
/// members it carries over (a quorum, m − ⌊(m−1)/3⌋) finalize on their own while newcomers
/// sync. For a full set that's ⌊(n−1)/3⌋ replacements per epoch; a set of 1 or 2 takes none.
pub fn churn_limit(m: usize) -> usize {
    m.saturating_sub(1) / 3
}

/// Picks the next set (see the module docs). Pure: the API previews it.
pub fn select_next(state: &State, epoch: u64) -> Selection {
    let current: Vec<String> = state.active_sets.get(&epoch).cloned().unwrap_or_default();
    let cap = state.chain_params.max_validators.max(1) as usize;
    let by_rank = |a: &String, b: &String| rank(state, b).cmp(&rank(state, a));
    let pinned = |v: &str| state.validators.get(v).is_some_and(|r| r.approved_at.is_some());
    // Members that may stay; the ineligible leave whatever the limit.
    let mut keep: Vec<String> = current.iter().filter(|v| eligible(state, v)).cloned().collect();
    keep.sort_by(by_rank);
    // Over the cap (governance lowered it): the lowest-ranked leave, ⌊(n−1)/3⌋ (at least one)
    // per epoch.
    let mut shrink = churn_limit(current.len()).max(1);
    while keep.len() > cap && shrink > 0 {
        keep.pop();
        shrink -= 1;
    }
    // Newcomers, best first: into a free seat, or in place of the lowest-ranked member they
    // outrank, while newcomers stay within the churn limit of the resulting set. Admin-pinned
    // validators (dev and test chains) skip the limit: that's the admin's call.
    let mut newcomers: Vec<String> = state.validators.keys().filter(|v| !current.contains(v) && eligible(state, v)).cloned().collect();
    newcomers.sort_by(by_rank);
    let mut joined = 0;
    for c in newcomers {
        let free = keep.len() < cap;
        if !free && !keep.last().is_some_and(|last| rank(state, &c) > rank(state, last)) {
            break;
        }
        let size = if free { keep.len() + 1 } else { keep.len() };
        if !pinned(&c) && joined + 1 > churn_limit(size) {
            break;
        }
        if !free {
            keep.pop();
        }
        keep.push(c);
        keep.sort_by(by_rank);
        joined += 1;
    }
    if keep.is_empty() {
        keep = current.clone();
    }
    keep.sort();
    let joining = keep.iter().filter(|v| !current.contains(v)).cloned().collect();
    let leaving = current.iter().filter(|v| !keep.contains(v)).cloned().collect();
    Selection { next: keep, joining, leaving }
}

/// At an epoch boundary, after `econ::issue` (rewards are already in the pools): forget punished
/// faults whose evidence can no longer arrive, then fix the next set and its members' statuses.
pub fn end_epoch(state: &mut State, epoch: u64) {
    let mut j = Journal::default();
    // Evidence for epoch e is accepted through epoch e + evidence_max_age_epochs.
    let oldest = (epoch + 1).saturating_sub(state.chain_params.evidence_max_age_epochs);
    let stale: Vec<String> = state.staking.punished.keys().filter(|k| k.split('/').nth(1).and_then(|e| e.parse::<u64>().ok()).is_some_and(|e| e < oldest)).cloned().collect();
    for k in stale {
        put_punished(state, &mut j, &k, None);
    }
    let sel = select_next(state, epoch);
    for v in &sel.leaving {
        if let Some(r) = state.validator_mut(&mut j, v) {
            r.active_until = Some(epoch);
            if r.status == ValidatorStatus::Active {
                r.status = ValidatorStatus::Candidate;
            }
        }
    }
    for v in &sel.next {
        let joining = sel.joining.contains(v);
        if let Some(r) = state.validator_mut(&mut j, v) {
            if r.status == ValidatorStatus::Candidate {
                r.status = ValidatorStatus::Active;
            }
            if joining {
                r.active_from = Some(epoch + 1);
                r.active_until = None;
            }
        }
    }
    state.active_sets.insert(epoch + 1, sel.next);
}

/// Candidates most likely to join next (eligible, not in `epoch`'s set), best first, at most
/// `limit`: the peers a validator lets connect besides the set, so they can sync before they vote.
pub fn likely_candidates(state: &State, epoch: u64, limit: usize) -> Vec<String> {
    let current = state.active_sets.get(&epoch).cloned().unwrap_or_default();
    let mut c: Vec<String> = state.validators.keys().filter(|v| !current.contains(v) && eligible(state, v)).cloned().collect();
    c.sort_by(|a, b| rank(state, b).cmp(&rank(state, a)));
    c.truncate(limit);
    c
}

// ------------------------------------------------------------------ for the other modules

/// `account`'s delegations, by validator.
pub fn delegations_of(state: &State, account: &str) -> Vec<(String, Delegation)> {
    owned_by(&state.staking.delegations, account).filter_map(|(k, d)| Some((k.split_once('/')?.1.to_string(), d.clone()))).collect()
}

/// `account`'s unbonding and redelegation entries, by id.
pub fn entries_of_account(state: &State, account: &str) -> Vec<(u64, Entry)> {
    entries_of(state, account).filter_map(|(k, e)| Some((k.split_once('/')?.1.parse().ok()?, e.clone()))).collect()
}

/// What `account` has staked: its shares' value across pools, and its unbonding entries' total.
pub fn holdings(state: &State, account: &str) -> (u64, u64) {
    let bonded = owned_by(&state.staking.delegations, account).filter_map(|(k, d)| state.staking.pools.get(k.split_once('/')?.1).map(|p| p.value(d.shares))).fold(0u64, u64::saturating_add);
    let unbonding = entries_of(state, account)
        .filter_map(|(_, e)| match e {
            Entry::Unbonding { amount, .. } => Some(*amount),
            Entry::Redelegation { .. } => None,
        })
        .fold(0u64, u64::saturating_add);
    (bonded, unbonding)
}

/// Pays `amount` of issuance (already minted by the caller) to the pools of the validators
/// active at `height`, pro rata to stake; jailed and tombstoned validators get none. Each pool's
/// owner gets its commission as new shares. Returns what was paid: the caller burns the rest or
/// sends it to the treasury.
pub fn distribute_rewards(state: &mut State, j: &mut Journal, amount: u64, height: u64) -> u64 {
    let epoch = state.params.epoch_of(height);
    let mut paid_to: Vec<(String, u64)> = state
        .active_sets
        .get(&epoch)
        .map(|s| s.iter().filter_map(|v| state.staking.pools.get(v).filter(|p| p.jail.is_none() && p.tombstoned_at.is_none() && p.shares > 0 && p.stake > 0).map(|p| (v.clone(), p.stake))).collect())
        .unwrap_or_default();
    paid_to.sort();
    let total: u128 = paid_to.iter().map(|(_, s)| *s as u128).sum();
    if total == 0 {
        return 0;
    }
    let mut paid = 0u64;
    for (v, stake) in paid_to {
        let reward = (amount as u128 * stake as u128 / total) as u64;
        if reward > 0 {
            credit_reward(state, j, &v, reward);
            paid += reward;
        }
    }
    paid
}

/// Adds `reward` to a pool: every share gains, and the owner gets its commission as new shares
/// worth (at most) the commission.
fn credit_reward(state: &mut State, j: &mut Journal, validator: &str, reward: u64) {
    let (owner, bps) = state.validators.get(validator).map(|r| (r.owner.clone(), r.commission_bps)).unwrap_or_default();
    let commission = bps_of(reward, bps);
    let p = pool_mut(state, j, validator).expect("rewarded pools exist");
    p.stake += reward;
    p.rewards += reward;
    // Shares that make `commission` of the new stake: commission × shares / (the rest of it).
    let minted = mul_div(commission as u128, p.shares, (p.stake - commission) as u128).unwrap_or(0);
    if minted == 0 {
        return;
    }
    p.shares += minted;
    let key = dkey(&owner, validator);
    let mut d = state.staking.delegations.get(&key).cloned().unwrap_or_default();
    d.shares += minted;
    put_delegation(state, j, &key, Some(d));
}

/// The active validator (in `epoch`) the tx sender or signer acts for: the validator whose
/// consensus key signed, when the sender is that validator or its owner; else, for a key on the
/// sender's own account, a validator the sender is or owns (the first by address). Council
/// votes, beacon commitments and attestations count for it.
///
/// A consensus key speaks only for its own validator: the key of an owner's other validator
/// that isn't in the set (retired, jailed out, removed) acts for nobody, never for a sibling
/// (audit F22: it could otherwise commit an unopenable beacon commitment in the active one's
/// name, getting it jailed every epoch, or cast its attestations).
pub fn validator_for_signer(state: &State, account: &str, signer_key_id: &str, epoch: u64) -> Option<String> {
    let set = state.active_sets.get(&epoch)?;
    let acts_for = |v: &String| v == account || state.validators.get(v).is_some_and(|r| r.owner == account);
    if let Some(v) = validator_of_key(state, signer_key_id).filter(|v| set.contains(v) && acts_for(v)) {
        return Some(v);
    }
    // The owner's own keys pick its validator; anything else (another validator's consensus key,
    // admitted through `system_tx_key`) doesn't.
    if !state.keys_of(account).contains_key(signer_key_id) {
        return None;
    }
    set.iter().filter(|v| acts_for(v)).min().cloned()
}

/// SF-13: a validator's system txs (attestations, beacon commitments and reveals, evidence, and
/// `UpdateValidator`, whose target only the signer can name) may be signed by its consensus key
/// with `from` = its owner, who pays the fee, so the consensus key only needs a fee float, if
/// that. Admission calls this when the signer isn't a key on `from`: the key record such a
/// signature acts with, or `None` (not allowed).
pub fn system_tx_key(state: &State, tx: &Tx, sender: &str, signer: &str) -> Option<KeyRecord> {
    let system = matches!(tx.action, Action::AttestClaim { .. } | Action::AttestRevert { .. } | Action::BeaconCommit { .. } | Action::BeaconReveal { .. } | Action::ReportEquivocation { .. } | Action::UpdateValidator { .. });
    if !system {
        return None;
    }
    let v = validator_of_key(state, signer)?;
    let rec = state.validators.get(&v).filter(|r| r.owner == sender && r.consensus_key == tx.public_key)?;
    Some(KeyRecord { label: "validator consensus key".into(), role: KeyRole::Owner, added_at: rec.registered_at, spent: 0, window_start: 0 })
}

/// FANG staking holds outside balances: pool stake and unbonding entries.
pub fn escrow(state: &State) -> u128 {
    let pools: u128 = state.staking.pools.values().map(|p| p.stake as u128).sum();
    let unbonding: u128 = state
        .staking
        .entries
        .values()
        .map(|e| match e {
            Entry::Unbonding { amount, .. } => *amount as u128,
            Entry::Redelegation { .. } => 0,
        })
        .sum();
    pools + unbonding
}

/// Staking's consistency rules (part of `State::check_invariants`): every validator has a pool;
/// a pool's shares are exactly its delegations'; a pool without shares holds nothing; keys are
/// well-formed and point at existing records; entry ids are below the counter and each delegator
/// has at most [`MAX_ENTRIES`]; liveness rings add up; set membership and statuses agree.
pub fn check_invariants(state: &State) -> Result<()> {
    let s = &state.staking;
    let mut shares: BTreeMap<&str, u128> = BTreeMap::new();
    for (k, d) in s.delegations.iter() {
        let (delegator, v) = k.split_once('/').ok_or_else(|| anyhow!("delegation key {k} is malformed"))?;
        ensure!(crate::crypto::is_address(delegator) && s.pools.contains_key(v), "delegation {k} names no account or pool");
        ensure!(d.shares > 0, "delegation {k} holds no shares");
        let t = shares.entry(v).or_default();
        *t = t.checked_add(d.shares).ok_or_else(|| anyhow!("pool {v}'s shares overflow"))?;
    }
    for (v, p) in s.pools.iter() {
        ensure!(state.validators.contains_key(v), "pool {v} has no validator");
        ensure!(shares.get(v.as_str()).copied().unwrap_or(0) == p.shares, "pool {v}'s shares aren't its delegations'");
        ensure!(p.shares > 0 || p.stake == 0, "pool {v} holds stake without shares");
        ensure!(p.tombstoned_at.is_none() || state.validators[v].status == ValidatorStatus::Tombstoned, "pool {v} is tombstoned but its validator isn't");
    }
    for v in state.validators.keys() {
        ensure!(s.pools.contains_key(v), "validator {v} has no pool");
    }
    let mut per: BTreeMap<&str, usize> = BTreeMap::new();
    for (k, e) in s.entries.iter() {
        let (delegator, id) = k.split_once('/').ok_or_else(|| anyhow!("entry key {k} is malformed"))?;
        ensure!(id.len() == 20 && id.parse::<u64>().is_ok_and(|id| id < s.next_entry_id), "entry {k} has a bad id");
        ensure!(e.release_at() >= e.started_at(), "entry {k} is released before it started");
        if let Entry::Unbonding { validator, .. } = e {
            ensure!(state.validators.contains_key(validator), "entry {k} unbonds from an unknown validator");
        }
        let n = per.entry(delegator).or_default();
        *n += 1;
        ensure!(*n <= MAX_ENTRIES, "{delegator} has more than {MAX_ENTRIES} entries");
    }
    for (v, l) in s.liveness.iter() {
        ensure!(state.validators.contains_key(v), "liveness of unknown validator {v}");
        ensure!(l.window > 0 && l.bits.len() as u64 == l.window.div_ceil(8) && l.next < l.window, "{v}'s liveness ring is malformed");
        let ones: u64 = (0..l.window).filter(|i| l.bit(*i)).count() as u64;
        ensure!(l.missed <= l.filled && l.filled <= l.window && ones == l.missed, "{v}'s liveness counts don't match its ring");
    }
    for k in s.punished.keys() {
        let parts: Vec<&str> = k.split('/').collect();
        ensure!(parts.len() == 3 && state.validators.contains_key(parts[0]) && parts[1..].iter().all(|p| p.len() == 20 && p.parse::<u64>().is_ok()), "punished key {k} is malformed");
    }
    if let Some((_, latest)) = state.active_sets.iter().next_back() {
        ensure!(!latest.is_empty(), "the latest validator set is empty");
        for v in latest {
            ensure!(state.validators.get(v).is_some_and(|r| r.status != ValidatorStatus::Candidate), "set member {v} is marked a candidate");
        }
        for r in state.validators.values().filter(|r| r.status == ValidatorStatus::Active) {
            ensure!(latest.contains(&r.address), "{} is marked active but isn't in the latest set", r.address);
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------------- display

/// Plain-English summary of a staking tx (see `State::describe_with`; `primary_name` looks up
/// names).
pub fn describe(state: &State, tx: &Tx, from: &str, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    let v = |a: &str| primary_name(a).unwrap_or_else(|| short(a));
    match &tx.action {
        Action::Delegate { validator, amount } => format!("{from} delegated {} to validator {}", fmt_fang(*amount), v(validator)),
        Action::Undelegate { validator, shares } => {
            // At the pool's current price (none once a sale emptied it).
            let worth = state.staking.pools.get(validator).filter(|p| p.shares > 0).map(|p| format!(" (about {})", fmt_fang(p.value(*shares as u128)))).unwrap_or_default();
            format!("{from} started unbonding {shares} shares{worth} of validator {}'s pool", v(validator))
        }
        Action::Redelegate { from_validator, to_validator, shares } => format!("{from} moved {shares} shares from validator {} to {}", v(from_validator), v(to_validator)),
        Action::WithdrawUnbonded {} => format!("{from} withdrew their unbonded stake"),
        Action::UpdateValidator { commission_bps, p2p_addr, http_addr } => {
            let addrs = if p2p_addr.is_empty() && http_addr.is_empty() { "" } else { " and updated its addresses" };
            format!("{from} set their validator's commission to {}%{addrs}", fmt_bps(*commission_bps))
        }
        Action::Unjail { validator } => format!("{from} unjailed validator {}", v(validator)),
        Action::ReportEquivocation { evidence } => {
            let what = hex::decode(evidence).ok().and_then(|b| decode_evidence(&b).ok()).map(|e| {
                let kind = match e.kind {
                    CONFLICTING_NOTARIZE => "two different blocks",
                    CONFLICTING_FINALIZE => "two different finalizations",
                    _ => "both a nullify and a finalize",
                };
                let who = state.active_sets.get(&e.epoch).map(|_| leaders(state, e.epoch)).and_then(|l| l.get(e.signer).cloned()).map(|a| format!("validator {}", v(&a))).unwrap_or_else(|| "a validator".into());
                format!("{who} for signing {kind} in epoch {}, view {}", e.epoch, e.view)
            });
            format!("{from} reported {}", what.unwrap_or_else(|| format!("a validator for double-signing ({} bytes of evidence)", evidence.len() / 2)))
        }
        _ => String::new(),
    }
}

/// Basis points as a percentage: 250 → "2.5".
pub fn fmt_bps(bps: u64) -> String {
    let (whole, frac) = (bps / 100, bps % 100);
    if frac == 0 { whole.to_string() } else { format!("{whole}.{}", format!("{frac:02}").trim_end_matches('0')) }
}

#[cfg(test)]
mod tests;
