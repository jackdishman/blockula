//! Staking (P-STAKE, MAINNET-DESIGN 3.4): validator pools with share accounting, delegation and
//! unbonding, the stake-ranked active set with a churn limit, slashing for equivocation and
//! downtime, jailing, and the proposer's charge for junk txs.
//!
//! **Pools.** Every validator owns a pool ([`Pool`], `STAKE_POOLS`): `stake` drops, escrowed (in
//! no balance), split into `shares`. `Delegate` buys shares at the pool's rate (1 share per drop
//! in an empty pool); `Undelegate` sells them (a number of shares, or the shares an amount of
//! FANG buys at the pool's price when it runs, so no signer relies on a node's share price) for an
//! unbonding [`Entry`] that `WithdrawUnbonded` pays out after `unbonding_blocks`; `Redelegate`
//! moves them to another pool at once. Rewards raise `stake`, so every share is worth more; the
//! owner's commission is minted to it as new shares; slashes lower `stake`, so every delegator
//! loses alike. The owner's own shares are the validator's self-bond. Rounding always favours the pool (a sale pays at most what the shares
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
//! epoch). A set that shrank below four still grows by one newcomer per epoch on chains without
//! admins ([`newcomers_allowed`]; every member of such a set is needed for a quorum anyway).
//! Members over a lowered cap leave ⌊(n−1)/3⌋ (at least one) per epoch. The set never empties. Votes are equal-weight within the set (DEC-3). On dev
//! and test chains admins may still pin a validator (`ApproveValidator`: ranked ahead of stake,
//! and past the churn limit, at the admin's risk) or remove one (`RemoveValidator`); elsewhere
//! both are refused.
//!
//! Unbonding lasts `unbonding_blocks`, stretched (by at most an epoch) so that it never ends
//! before evidence against the epoch it began in stops being accepted.
//!
//! **Slashing.** `ReportEquivocation` carries simplex's own evidence (an encoded
//! `ConflictingNotarize`, `ConflictingFinalize` or `NullifyFinalize` activity), verified against
//! that epoch's set and the chain instance's namespace, from a view that epoch reached
//! ([`verify_evidence`]). The pool loses `slash_double_sign_bps`, and so do the unbonding and
//! redelegation entries that left it since that epoch began, once: the validator is tombstoned
//! (never selected again) and later evidence against it only reaches stake that left before the
//! punished epoch ([`Pool::slashed_from`]), so every drop answers for double-signing at most once
//! (a leaked key can't drain a pool fault by fault). The reporter gets `reporter_reward_bps` of the
//! slash, at most what the validator's owner lost (reporting yourself never pays); the rest is
//! burned. Downtime is measured from
//! leader slots: each view a block skipped (`parent_view + 1 .. view − 1`, nullified by 2f+1) is a
//! miss for that view's leader (at most one per validator per block) and the block's own view a
//! hit, with simplex's `RoundRobin` elector naming the leaders. A validator that missed more than
//! `downtime_max_missed` of its last `downtime_window` slots is jailed ([`jail`],
//! `slash_downtime_bps`) unless more than f of the set would then be jailed, leaves at the next
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
//! **The bootstrap delegation** (DEC-5, [`bootstrap_epoch`]): at each boundary the keyless
//! bootstrap account's rewards go to the treasury, what its vesting lock no longer holds goes back
//! to the treasury, and the rest is spread equally over the next set's members not owned by the
//! founder's or the foundation's accounts. It moves without unbonding, only by that formula, and
//! its stake doesn't count toward the ranking for seats.
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
    /// Equivocation already took its cut of the stake that was in this pool from this height on
    /// (the start of the earliest punished fault's epoch). A double-signer is slashed once: later
    /// evidence only reaches stake that left the pool before this height (audit F05/F07/F60).
    pub slashed_from: Option<u64>,
    /// The last height the validator's own consensus key signaled readiness (`ValidatorReady`, in
    /// an epoch's last tenth): its node was running then. A newcomer into a set of one or two
    /// joins only if it showed up in the epoch ([`showed_up`], audit review-fix-staking-2).
    pub ready_at: Option<u64>,
    /// The last height the validator's consensus key updated it for its owner (SF-13): at most
    /// once an epoch (audit F04).
    pub key_update_at: Option<u64>,
    /// The candidate this validator's consensus key last admitted (`AdmitValidator`) and the
    /// height: it counts for the boundary at the end of that height's epoch ([`admitted`]).
    pub admit: Option<(String, u64)>,
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
    /// Drops put in, less what sales (and redelegation slashes, which sell shares) took out pro
    /// rata: value above it is earnings (what stays locked for vesting accounts, 3.7). A pool
    /// slash doesn't lower it, on purpose: it doesn't lower a vesting lock either, so value the
    /// pool earns back after a slash refills locked tokens the slash took and stays under the
    /// original lock; only value above the principal is new (audit F58).
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
    /// The `downtime_max_missed` the ring was recorded under: a ring judged by another limit
    /// starts afresh, as for another window, so a lowered limit never jails for misses the old
    /// one allowed (audit gap-6-4).
    pub max_missed: u64,
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
    fn new(window: u64, max_missed: u64) -> Self {
        Self { window, max_missed, bits: vec![0; window.div_ceil(8) as usize], next: 0, filled: 0, missed: 0, led: 0, missed_total: 0 }
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

    /// Records `n` missed slots in a row (tests; a block charges at most one).
    #[cfg(test)]
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

/// Staking's slice of the state (namespaces `state::ns` 32–36 and 64–65, META key `staking`).
#[derive(Clone, Debug, Default)]
pub struct Staking {
    pub pools: TMap<String, Pool, PoolStake>,
    pub delegations: TMap<String, Delegation>,
    pub entries: TMap<String, Entry, UnbondingTotal>,
    pub punished: TMap<String, Punished>,
    pub liveness: TMap<String, Liveness>,
    /// Every entry, by the validator it left (key: `validator/started_at/delegator/id`, 20 digits
    /// each), so slashing a validator reads only its own entries, from the fault's epoch on.
    pub by_validator: TMap<String, ()>,
    /// The highest view of a block executed in each epoch (key: the epoch), for as long as the
    /// epoch's set is kept: evidence can't be from a view far beyond what consensus reached.
    pub views: TMap<u64, u64>,
    /// Id of the next unbonding or redelegation entry.
    pub next_entry_id: u64,
}

impl SubState for Staking {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Staking { pools, delegations, entries, punished, liveness, by_validator, views, next_entry_id: _ } = self;
        vec![(ns::STAKE_POOLS, pools), (ns::STAKE_DELEGATIONS, delegations), (ns::STAKE_UNBONDING, entries), (ns::STAKE_EVIDENCE, punished), (ns::STAKE_LIVENESS, liveness), (ns::STAKE_ENTRIES_BY_VALIDATOR, by_validator), (ns::STAKE_VIEWS, views)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Staking { pools, delegations, entries, punished, liveness, by_validator, views, next_entry_id: _ } = self;
        vec![(ns::STAKE_POOLS, pools), (ns::STAKE_DELEGATIONS, delegations), (ns::STAKE_UNBONDING, entries), (ns::STAKE_EVIDENCE, punished), (ns::STAKE_LIVENESS, liveness), (ns::STAKE_ENTRIES_BY_VALIDATOR, by_validator), (ns::STAKE_VIEWS, views)]
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
    by_validator: JMap<String, ()>,
    views: JMap<u64, u64>,
    next_entry_id: Option<u64>,
}

impl StakingJournal {
    pub fn absorb(&mut self, other: StakingJournal) {
        let StakingJournal { pools, delegations, entries, punished, liveness, by_validator, views, next_entry_id } = other;
        self.pools.absorb(pools);
        self.delegations.absorb(delegations);
        self.entries.absorb(entries);
        self.punished.absorb(punished);
        self.liveness.absorb(liveness);
        self.by_validator.absorb(by_validator);
        self.views.absorb(views);
        if self.next_entry_id.is_none() {
            self.next_entry_id = next_entry_id;
        }
    }

    pub fn rollback(self, staking: &mut Staking) {
        let StakingJournal { pools, delegations, entries, punished, liveness, by_validator, views, next_entry_id } = self;
        pools.restore(&mut staking.pools);
        delegations.restore(&mut staking.delegations);
        entries.restore(&mut staking.entries);
        punished.restore(&mut staking.punished);
        liveness.restore(&mut staking.liveness);
        by_validator.restore(&mut staking.by_validator);
        views.restore(&mut staking.views);
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

/// The validator an entry left (an unbonding's, a redelegation's `from`).
fn left(e: &Entry) -> &str {
    match e {
        Entry::Unbonding { validator, .. } => validator,
        Entry::Redelegation { from, .. } => from,
    }
}

/// An entry's key in [`Staking::by_validator`]: `validator/started_at/` then its own key.
fn vkey(e: &Entry, key: &str) -> String {
    format!("{}/{:020}/{key}", left(e), e.started_at())
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

/// Writes (or removes) an entry, and its [`Staking::by_validator`] key with it.
fn put_entry(state: &mut State, j: &mut Journal, key: &str, e: Option<Entry>) {
    if let Some(old) = state.staking.entries.get(key) {
        let k = vkey(old, key);
        j.staking.by_validator.touch(&state.staking.by_validator, &k);
        state.staking.by_validator.remove(&k);
    }
    j.staking.entries.touch(&state.staking.entries, key);
    match e {
        Some(e) => {
            let k = vkey(&e, key);
            j.staking.by_validator.touch(&state.staking.by_validator, &k);
            state.staking.by_validator.insert(k, ());
            state.staking.entries.insert(key.to_string(), e)
        }
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

/// What remains of `d` after `shares` of it leave, and the principal those shares carried (pro
/// rata).
fn split(d: &Delegation, shares: u128) -> (Delegation, u64) {
    let principal = if shares == d.shares { d.principal } else { mul_div(d.principal as u128, shares, d.shares).unwrap_or(0) as u64 };
    (Delegation { shares: d.shares - shares, principal: d.principal - principal }, principal)
}

/// Sells `shares` of `delegator`'s stake in `validator`'s pool. Returns what they were worth
/// (out of the pool, in nobody's balance yet) and the principal they carried.
fn unbond(state: &mut State, j: &mut Journal, delegator: &str, validator: &str, shares: u128) -> Result<(u64, u64)> {
    let key = dkey(delegator, validator);
    let d = state.staking.delegations.get(&key).cloned().ok_or_else(|| anyhow!("{} has no stake with validator {}", short(delegator), short(validator)))?;
    if shares > d.shares {
        bail!("{} holds {} shares of validator {}'s pool, not {shares}", short(delegator), d.shares, short(validator));
    }
    let pool = state.staking.pools.get(validator).ok_or_else(|| anyhow!("validator {} has no pool", short(validator)))?;
    let value = pool.value(shares);
    let (rest, principal) = split(&d, shares);
    let p = pool_mut(state, j, validator).expect("checked above");
    p.stake -= value;
    p.shares -= shares;
    put_delegation(state, j, &key, (rest.shares > 0).then_some(rest));
    Ok((value, principal))
}

/// The shares of `delegator`'s stake in `validator`'s pool an `Undelegate` of `amount` drops
/// sells: what that much buys at the pool's price now, rounded up so the sale pays at least
/// `amount` (and less than a share's worth more; the pool still pays what the shares are worth,
/// rounded down), at most all of them. Refused when the stake is worth less than `amount`: the
/// signer named FANG, so the chain never unbonds more or less than that (audit gap-7-5).
fn shares_worth(state: &State, delegator: &str, validator: &str, amount: u64) -> Result<u128> {
    let d = state.staking.delegations.get(&dkey(delegator, validator)).ok_or_else(|| anyhow!("{} has no stake with validator {}", short(delegator), short(validator)))?;
    let pool = state.staking.pools.get(validator).ok_or_else(|| anyhow!("validator {} has no pool", short(validator)))?;
    let worth = pool.value(d.shares);
    if amount > worth {
        bail!("{}'s stake with validator {} is worth {}, less than the {} asked for (undelegate all its shares to take all of it)", short(delegator), short(validator), fmt_fang(worth), fmt_fang(amount));
    }
    // worth ≥ amount > 0, so the pool has stake and shares, and the quotient is at most its shares.
    Ok(mul_div_ceil(amount as u128, pool.shares, pool.stake as u128).map_or(d.shares, |s| s.min(d.shares)))
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
    let (rest, _) = split(&d, shares);
    let p = pool_mut(state, j, validator).expect("checked above");
    p.stake -= taken;
    p.shares -= shares;
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
/// ReportEquivocation; 60–61: ValidatorReady, AdmitValidator. Session keys never get here
/// (admission lets them send transfers only).
pub(crate) fn apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, _key: &KeyRecord, height: u64) -> Result<()> {
    match &tx.action {
        Action::Delegate { validator, amount } => own_move(state, j, sender, height, |state, j| {
            accepting(state, validator)?;
            if !state.staking.delegations.contains_key(&dkey(sender, validator)) && *amount < MIN_DELEGATION {
                bail!("a new delegation is at least {}", fmt_fang(MIN_DELEGATION));
            }
            state.j_debit(j, sender, *amount)?;
            bond(state, j, sender, validator, *amount, *amount)?;
            check_insider_cap(state, sender, validator, height)
        }),
        Action::Undelegate { validator, shares, amount } => own_move(state, j, sender, height, |state, j| {
            room_for_entry(state, sender)?;
            // An amount is converted here, at the pool's price now: never at a price some node
            // reported to the signer (audit gap-7-5).
            let shares = if *amount > 0 { shares_worth(state, sender, validator, *amount)? } else { *shares as u128 };
            let (value, principal) = unbond(state, j, sender, validator, shares)?;
            lock_gains(state, j, sender, value.saturating_sub(principal), height);
            if value > 0 {
                let release_at = release_height(state, height);
                add_entry(state, j, sender, Entry::Unbonding { validator: validator.clone(), amount: value, started_at: height, release_at });
            }
            Ok(())
        }),
        Action::Redelegate { from_validator: from, to_validator: to, shares } => own_move(state, j, sender, height, |state, j| {
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
        }),
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
            // Signed by its own consensus key for its owner (SF-13, the owner pays): once an epoch
            // (audit F04). The owner is `sender` here (`owned_validator`).
            let for_owner = validator_of_key(state, signer).as_deref() == Some(v.as_str()) && !state.keys_of(sender).contains_key(signer);
            if for_owner && pool.key_update_at.is_some_and(|h| state.params.epoch_of(h) == epoch) {
                bail!("validator {}'s consensus key already updated it for its owner in epoch {epoch} (once an epoch; its owner's own key isn't limited)", short(&v));
            }
            // Measured from at most the cap: a validator above a lowered cap steps down to it at once.
            let base = if pool.commission_epoch == epoch { pool.commission_base } else { current }.min(max);
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
                if for_owner {
                    p.key_update_at = Some(height);
                }
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
            // Its owner's call, never its consensus key's from the validator's own account (when
            // that's another account than the owner's: a multisig owner's approvals would be
            // bypassed; audit review-r2-integrate-compose-1).
            if rec.owner != sender {
                bail!("only validator {}'s owner ({}) can unjail it", short(validator), short(&rec.owner));
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
            // `tx.genesis_hash` is this chain's: admission, gossip and block verification check it.
            let fault = verify_evidence(state, &tx.chain_id, &tx.genesis_hash, &bytes, height)?;
            let key = pkey(&fault.validator, fault.epoch, fault.view);
            if state.staking.punished.contains_key(&key) {
                bail!("validator {} was already punished for epoch {}, view {}", short(&fault.validator), fault.epoch, fault.view);
            }
            // Stake that was in the pool when the epoch began answers for it, wherever it went
            // since: once. A double-signer is slashed once (and tombstoned); a fault from an earlier
            // epoch, reported later, only reaches the stake that left before the first one's.
            let since = fault.epoch.saturating_mul(state.params.epoch_blocks.max(2));
            let earlier = state.staking.pools.get(&fault.validator).and_then(|p| p.slashed_from);
            if let Some(from) = earlier.filter(|from| since >= *from) {
                bail!("validator {} was already slashed for double-signing (its stake from block {from} on answered for it): this fault adds nothing", short(&fault.validator));
            }
            let cut = slash(state, j, &fault.validator, since, earlier, earlier.is_none(), state.chain_params.slash_double_sign_bps);
            let owner = state.validators.get(&fault.validator).map(|r| r.owner.clone()).unwrap_or_default();
            // No reward for reporting yourself, and never more than the owner's own stake lost:
            // reporting yourself from another account can't pay.
            let reward = if sender == owner || sender == fault.validator { 0 } else { bps_of(cut.total, state.chain_params.reporter_reward_bps).min(cut.owner) };
            state.j_credit(j, sender, reward);
            state.j_burn(j, cut.total - reward);
            tombstone(state, j, &fault.validator, height);
            if let Some(p) = pool_mut(state, j, &fault.validator) {
                p.slashed_from = Some(since);
            }
            put_punished(state, j, &key, Some(Punished { kind: fault.kind, height, reporter: sender.to_string(), slashed: cut.total }));
            Ok(())
        }
        Action::ValidatorReady { epoch } => {
            let v = signing_validator(state, sender, signer, "signals readiness")?;
            in_epoch(state, *epoch, height, "readiness")?;
            let from = ready_from(&state.params, *epoch);
            if height < from {
                bail!("readiness counts only in the epoch's last tenth: from block {from} on");
            }
            check_ready(state, &v, *epoch)?;
            pool_mut(state, j, &v).expect("check_ready found it").ready_at = Some(height);
            Ok(())
        }
        Action::AdmitValidator { validator, epoch } => {
            let m = signing_validator(state, sender, signer, "admits a newcomer")?;
            in_epoch(state, *epoch, height, "an admission")?;
            check_admit(state, &m, validator, *epoch)?;
            pool_mut(state, j, &m).expect("check_admit found it").admit = Some((validator.clone(), height));
            Ok(())
        }
        _ => bail!("not a staking action"),
    }
}

/// The validator whose consensus key signed a `ValidatorReady` or `AdmitValidator`, from its own
/// account or its owner's (SF-13: admission checked the key is the validator's).
fn signing_validator(state: &State, sender: &str, signer: &str, what: &str) -> Result<String> {
    let v = validator_of_key(state, signer).ok_or_else(|| anyhow!("only a validator's consensus key {what}"))?;
    let rec = &state.validators[&v];
    if sender != v && sender != rec.owner {
        bail!("validator {}'s consensus key {what} from its own account or its owner's", short(&v));
    }
    Ok(v)
}

/// `epoch`, named by a `ValidatorReady` or `AdmitValidator`, is the one it lands in.
fn in_epoch(state: &State, epoch: u64, height: u64, what: &str) -> Result<()> {
    let now = state.params.epoch_of(height);
    if epoch != now {
        bail!("{what} for epoch {epoch} can't land in epoch {now}");
    }
    Ok(())
}

/// The validator an `UpdateValidator` means, which `sender` must own: the one whose consensus key
/// signed it (for its owner), else the one the sender is (owning itself), else the only one the
/// sender owns. A validator's terms are its owner's call: from the validator's own account its
/// consensus key changes them only when that account is the owner (audit
/// review-r2-integrate-compose-1: a multisig owner's approvals and F04's once-an-epoch limit
/// would be bypassed otherwise).
fn owned_validator(state: &State, sender: &str, signer: &str) -> Result<String> {
    if let Some((v, r)) = validator_of_key(state, signer).and_then(|v| state.validators.get(&v).map(|r| (v, r))) {
        if r.owner == sender {
            return Ok(v);
        }
        if v == sender {
            bail!("validator {}'s terms are its owner's call: its consensus key signs them for its owner {} (from = the owner), not from the validator's own account", short(&v), short(&r.owner));
        }
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
    own_move(state, j, sender, height, |state, j| {
        state.j_debit(j, sender, *self_bond)?;
        let epoch = state.params.epoch_of(height);
        let rec = ValidatorRecord { address: address.clone(), owner: sender.to_string(), consensus_key: consensus_key.clone(), p2p_addr: p2p_addr.clone(), http_addr: http_addr.clone(), status: ValidatorStatus::Candidate, registered_at: height, approved_at: None, approved_by: None, active_from: None, removed_at: None, removed_by: None, active_until: None, commission_bps: 0 };
        state.insert_validator(j, rec);
        put_pool(state, j, &address, Pool { commission_epoch: epoch, ..Default::default() });
        bond(state, j, sender, &address, *self_bond, *self_bond)?;
        check_insider_cap(state, sender, &address, height)
    })
}

/// Runs one of `sender`'s own staking moves (delegating, unbonding, redelegating, registering a
/// validator) and records how far it took the sender's transferable balance down in
/// `Journal::lock_slack`. The tokens stay in the sender's holdings: liquid ones become stake,
/// stake becomes an unbonding entry or other stake, and gains being sold join the vesting lock
/// (`lock_gains`); only rounding dust of under a share's worth stays in a pool, since rounding
/// favours the pool. So the lock check (`econ::check_locks`) doesn't count that drop as moving
/// locked tokens out: an account whose tokens are all locked can still stake them at any share
/// price.
fn own_move(state: &mut State, j: &mut Journal, sender: &str, height: u64, f: impl FnOnce(&mut State, &mut Journal) -> Result<()>) -> Result<()> {
    let before = crate::econ::free(state, sender, height);
    f(state, j)?;
    let drop = before - crate::econ::free(state, sender, height);
    j.lock_slack = j.lock_slack.saturating_add(drop.clamp(0, u64::MAX as i128) as u64);
    Ok(())
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

/// What a slash took: the total (out of every balance and escrow, for the caller to burn or
/// pay part of to a reporter), and how much of it was the validator owner's own stake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cut {
    pub total: u64,
    pub owner: u64,
}

/// Takes `bps` of the stake that answers for an infraction at height `since`: `validator`'s pool
/// (if `pool`), and each unbonding and redelegation entry that left it at or after `since` and
/// before `until` (all of them from `since` on if `None`), read through the
/// [`Staking::by_validator`] index (never a scan of every entry). Each entry is cut on what's left
/// of it: an unbonding's amount, a redelegation's slashable remainder (what moved, less earlier
/// cuts; taken from the delegator's stake in the pool it moved to, then from that stake's own way
/// out), so repeated cuts never take more than the entry held (MAINNET-DESIGN 3.4).
fn slash(state: &mut State, j: &mut Journal, validator: &str, since: u64, until: Option<u64>, pool: bool, bps: u64) -> Cut {
    let mut cut = Cut::default();
    if bps == 0 {
        return cut;
    }
    let owner = state.validators.get(validator).map(|r| r.owner.clone()).unwrap_or_default();
    if let Some(p) = state.staking.pools.get(validator).filter(|_| pool) {
        let c = bps_of(p.stake, bps);
        if c > 0 {
            let before = delegation_value(state, &owner, validator);
            let p = pool_mut(state, j, validator).expect("checked above");
            p.stake -= c;
            p.slashed += c;
            cut.total += c;
            cut.owner += before - delegation_value(state, &owner, validator);
        }
    }
    let to = until.map_or_else(|| format!("{validator}0"), |u| format!("{validator}/{u:020}"));
    let hits: Vec<String> = state.staking.by_validator.range(format!("{validator}/{since:020}")..to).filter_map(|(k, _)| k.splitn(3, '/').nth(2).map(str::to_string)).collect();
    for key in hits {
        let Some(e) = state.staking.entries.get(&key).cloned() else { continue };
        let delegator = key.split('/').next().unwrap_or_default().to_string();
        let taken = match e {
            Entry::Unbonding { validator: v, amount, started_at, release_at } => {
                let c = bps_of(amount, bps);
                if c > 0 {
                    put_entry(state, j, &key, Some(Entry::Unbonding { validator: v, amount: amount - c, started_at, release_at }));
                }
                c
            }
            Entry::Redelegation { from, to, amount, started_at, release_at } => {
                let due = bps_of(amount, bps);
                if due == 0 {
                    continue;
                }
                // From the stake it moved into `to`, then from that stake's own way out.
                let mut taken = take_from_delegation(state, j, &delegator, &to, due);
                if taken < due {
                    let later: Vec<(String, Entry)> = entries_of(state, &delegator)
                        .filter(|(_, e)| matches!(e, Entry::Unbonding { validator: v, started_at: s, .. } if *v == to && *s >= started_at))
                        .map(|(k, e)| (k.clone(), e.clone()))
                        .collect();
                    for (k, e) in later {
                        let Entry::Unbonding { validator: v, amount: a, started_at: s, release_at: r } = e else { continue };
                        let c = a.min(due - taken);
                        put_entry(state, j, &k, Some(Entry::Unbonding { validator: v, amount: a - c, started_at: s, release_at: r }));
                        taken += c;
                        if taken == due {
                            break;
                        }
                    }
                }
                // The moved stake answered for this much of itself: less is left to answer later.
                put_entry(state, j, &key, Some(Entry::Redelegation { from, to, amount: amount - due, started_at, release_at }));
                taken
            }
        };
        cut.total += taken;
        if delegator == owner {
            cut.owner += taken;
        }
    }
    cut
}

/// Jails `validator`: slashes `slash_bps` of its pool (burned), and it leaves the set at the
/// next boundary; `Unjail` brings it back after `jail_blocks`. Tombstoned or missing validators
/// are left alone; jailing a jailed validator slashes again and keeps the later release.
/// Downtime calls it, and so does the beacon (P-BEACON) for commitments never revealed.
pub fn jail(state: &mut State, j: &mut Journal, validator: &str, height: u64, slash_bps: u64, reason: &str) {
    if state.staking.pools.get(validator).is_none_or(|p| p.tombstoned_at.is_some()) {
        return;
    }
    let slashed = slash(state, j, validator, height, None, true, slash_bps);
    state.j_burn(j, slashed.total);
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

/// Views past the highest one a block of the epoch executed in that evidence may still be from:
/// votes for views not executed yet, and the epoch's tail after its last block. Beyond it, a
/// "fault" is in a view consensus never reached (made up by the key's holder): refused.
pub const VIEW_SLACK: u64 = 1_000;

/// The newest view evidence for `epoch` may be from (see [`VIEW_SLACK`]).
pub fn max_evidence_view(state: &State, epoch: u64) -> u64 {
    state.staking.views.get(&epoch).copied().unwrap_or(0).saturating_add(VIEW_SLACK)
}

/// Checks equivocation evidence at `height`: well-formed ([`decode_evidence`]), from an epoch no
/// later than now and at most `evidence_max_age_epochs` old whose set the state still holds, from
/// a view that epoch reached (within [`VIEW_SLACK`] of its highest executed block), and both
/// votes signed by the same member of that set under this chain instance's consensus
/// namespace (`consensus::namespace(chain_id, genesis_hash)`: votes from another genesis under
/// the same chain id never verify).
pub fn verify_evidence(state: &State, chain_id: &str, genesis_hash: &str, bytes: &[u8], height: u64) -> Result<Fault> {
    let e = decode_evidence(bytes)?;
    let now = state.params.epoch_of(height);
    if e.epoch > now {
        bail!("the evidence is from epoch {}, after this one ({now})", e.epoch);
    }
    let max_age = state.chain_params.evidence_max_age_epochs;
    if now - e.epoch > max_age {
        bail!("the evidence is from epoch {}: too old (at most {max_age} epochs back)", e.epoch);
    }
    let reached = max_evidence_view(state, e.epoch);
    if e.view > reached {
        bail!("the evidence is from view {} of epoch {}, beyond any view it reached (at most {reached} so far)", e.view, e.epoch);
    }
    let set = participants(state, e.epoch)?;
    let pk = set.get(e.signer).ok_or_else(|| anyhow!("signer {} isn't in epoch {}'s set of {}", e.signer, e.epoch, set.len()))?.clone();
    let scheme = MlDsaScheme::verifier(&crate::consensus::namespace(chain_id, genesis_hash), set);
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
    /// That fault, or one by the same validator that already took this fault's stake.
    Punished,
    /// From an epoch (or a view of this epoch) this state hasn't reached: ask again later.
    NotYet,
    /// Malformed, forged, or too old: it never will count.
    Never,
}

pub fn evidence_status(state: &State, chain_id: &str, genesis_hash: &str, bytes: &[u8], height: u64) -> EvidenceStatus {
    let Ok(e) = decode_evidence(bytes) else { return EvidenceStatus::Never };
    let now = state.params.epoch_of(height);
    if e.epoch > now || (e.epoch == now && e.view > max_evidence_view(state, e.epoch)) {
        return EvidenceStatus::NotYet;
    }
    let since = |f: &Fault| f.epoch.saturating_mul(state.params.epoch_blocks.max(2));
    match verify_evidence(state, chain_id, genesis_hash, bytes, height) {
        Ok(f) if state.staking.punished.contains_key(&pkey(&f.validator, f.epoch, f.view)) => EvidenceStatus::Punished,
        Ok(f) if state.staking.pools.get(&f.validator).and_then(|p| p.slashed_from).is_some_and(|from| since(&f) >= from) => EvidenceStatus::Punished,
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
    note_view(state, &mut j, b);
    score_leader_slots(state, &mut j, b);
}

/// Records the block's view as its epoch's highest (evidence can't be from far beyond it).
fn note_view(state: &mut State, j: &mut Journal, b: &BlockInfo) {
    let epoch = state.params.epoch_of(b.height);
    if b.view > state.staking.views.get(&epoch).copied().unwrap_or(0) {
        j.staking.views.touch(&state.staking.views, &epoch);
        state.staking.views.insert(epoch, b.view);
    }
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
/// the block's own view is a hit for its leader. One block charges a validator at most one miss
/// (audit F59): a stall of many views, whatever its cause (lagging verifiers, a partition, an
/// upgrade gone wrong), costs every leader in it one slot, not one per lap of the set, so it
/// can't jail the whole set in one block. A validator over `downtime_max_missed` in its window
/// is jailed, while that leaves at most f = ⌊(n−1)/3⌋ of the epoch's set jailed: more members
/// looking offline at once is an incident of the network, not of the validators (more than f
/// truly offline would have halted the chain), and they're charged again later.
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
    let gap = b.view - b.parent_view - 1;
    // Any n consecutive views lead every validator once.
    let mut missed = vec![gap >= n as u64; n];
    if gap < n as u64 {
        for x in b.view - gap..b.view {
            missed[leader_index(seed, epoch, x, n)] = true;
        }
    }
    let hit = leader_index(seed, epoch, b.view, n);
    let (window, max_missed) = (state.chain_params.downtime_window.max(1), state.chain_params.downtime_max_missed);
    let out = |p: &Pool| p.jail.is_some() || p.tombstoned_at.is_some();
    let mut jailed = order.iter().filter(|v| state.staking.pools.get(*v).is_some_and(out)).count();
    let f = (n - 1) / 3;
    for (i, v) in order.iter().enumerate() {
        if !missed[i] && i != hit {
            continue;
        }
        let Some(pool) = state.staking.pools.get(v) else { continue };
        // Jailed or tombstoned: already on its way out.
        if out(pool) {
            continue;
        }
        // A ring recorded under another window or limit starts afresh (audit gap-6-4).
        let mut l = state.staking.liveness.get(v).filter(|l| (l.window, l.max_missed) == (window, max_missed)).cloned().unwrap_or_else(|| Liveness::new(window, max_missed));
        if missed[i] {
            l.push(true);
        }
        if i == hit {
            l.push(false);
        }
        let over = l.missed > max_missed;
        let reason = format!("missed {} of its last {} leader slots", l.missed, l.filled);
        put_liveness(state, j, v, Some(l));
        if over && jailed < f {
            jail(state, j, v, b.height, state.chain_params.slash_downtime_bps, &reason);
            jailed += 1;
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

/// What counts toward a validator's rank for a seat: its pool's stake without the bootstrap
/// delegation's (`boot`: it follows the set, it doesn't pick it; [`bootstrap_epoch`]) and without
/// what accounts still vesting (`insiders`) other than its owner hold in it: locked stake earns
/// like any other, but picks no seat except its own holder's (audit xcut-economic-2: an insider
/// can't rank the validators of an accomplice, or of its own other accounts, with locked tokens).
fn ranking_stake(state: &State, boot: &str, insiders: &[String], validator: &str) -> u64 {
    let Some(p) = state.staking.pools.get(validator) else { return 0 };
    let owner = state.validators.get(validator).map(|r| r.owner.as_str()).unwrap_or_default();
    let locked = insiders.iter().filter(|a| a.as_str() != owner).map(|a| delegation_value(state, a, validator)).fold(0u64, u64::saturating_add);
    p.stake.saturating_sub(delegation_value(state, boot, validator)).saturating_sub(locked)
}

/// Ranks for the set at `height`, worked out once per selection: admin-pinned first (dev and test
/// chains), then [`ranking_stake`], then address (ascending). Greater is better.
struct Ranks(BTreeMap<String, (bool, u64)>);

impl Ranks {
    fn new<'a>(state: &State, height: u64, validators: impl Iterator<Item = &'a String>) -> Self {
        let (boot, insiders) = (state.bootstrap(), insiders(state, height));
        Ranks(validators.map(|v| (v.clone(), (state.validators.get(v).is_some_and(|r| r.approved_at.is_some()), ranking_stake(state, &boot, &insiders, v)))).collect())
    }

    fn of<'a>(&self, validator: &'a str) -> (bool, u64, std::cmp::Reverse<&'a str>) {
        let (pinned, stake) = self.0.get(validator).copied().unwrap_or_default();
        (pinned, stake, std::cmp::Reverse(validator))
    }

    /// Best first.
    fn sort(&self, vs: &mut [String]) {
        vs.sort_by(|a, b| self.of(b).cmp(&self.of(a)));
    }
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
/// sync. For a full set that's ⌊(n−1)/3⌋ replacements per epoch; a set of 1 or 2 takes none
/// under it (see [`newcomers_allowed`] for how such a set still grows).
pub fn churn_limit(m: usize) -> usize {
    m.saturating_sub(1) / 3
}

/// Newcomers the next set may take when it carries over `kept` members and would have `size`
/// with this newcomer (`free`: into a free seat rather than replacing a member). The churn limit,
/// except that on chains without admins a set carrying over one to three members grows by one per
/// epoch (audit F24): under the churn limit alone a set that shrank to one or two could never grow
/// again, with no fault tolerance (f = 0) for good, and mainnet has no other way to add one.
/// Past the churn limit the newcomer is needed for every quorum (a set of 2 or 3), so stake alone
/// can't buy the seat (one that never votes would halt the chain for good): [`select_next`] takes
/// one past the limit only if a quorum of the members it joins admitted it ([`admitted`]) and its
/// consensus key showed up in the epoch's last tenth ([`showed_up`]; audit review-fix-staking-2).
/// Safety is unchanged. Dev and test chains keep small sets closed to strangers (a stranger that
/// joined and went offline would stall them): their admins pin newcomers.
pub fn newcomers_allowed(state: &State, kept: usize, size: usize, free: bool) -> usize {
    let limit = churn_limit(size);
    if free && (1..4).contains(&kept) && state.params.admins.is_empty() { limit.max(1) } else { limit }
}

/// The first height of `epoch`'s last tenth (at least its last block): where a candidate's
/// readiness counts ([`showed_up`]), so a node that showed up early and then stopped doesn't.
pub fn ready_from(params: &crate::chain::Params, epoch: u64) -> u64 {
    let e = params.epoch_blocks.max(2);
    params.last_of(epoch).saturating_sub((e / 10).max(1) - 1)
}

/// Whether `validator`'s own consensus key signaled readiness in `epoch`'s last tenth
/// (`ValidatorReady`, [`Pool::ready_at`]): its node was running then. What a newcomer that every
/// quorum of a set of 2 or 3 would need must show before it joins (its node sends it by itself
/// once the set's members admitted it: `beacon::duties`).
pub fn showed_up(state: &State, validator: &str, epoch: u64) -> bool {
    state.staking.pools.get(validator).and_then(|p| p.ready_at).is_some_and(|h| state.params.epoch_of(h) == epoch && h >= ready_from(&state.params, epoch))
}

/// The members of `epoch`'s set that would carry over to the next one now (eligible), if the
/// next newcomer would be past the churn limit: one or two of them on a chain without admins.
/// Such a newcomer is needed for every quorum of the set it makes (2 or 3), so it joins only once
/// a quorum of these members admitted it ([`admitted`]) and it showed up ([`showed_up`]). `None`
/// otherwise: a set carrying over three or more takes its newcomers on stake, within the churn
/// limit, and dev and test chains' admins pin theirs.
pub fn members_admitting(state: &State, epoch: u64) -> Option<Vec<String>> {
    let set = state.active_sets.get(&epoch)?;
    let members: Vec<String> = set.iter().filter(|v| eligible(state, v)).cloned().collect();
    (state.params.admins.is_empty() && (1..=2).contains(&members.len())).then_some(members)
}

/// Whether a quorum of `members` (those carrying over, [`members_admitting`]) admitted `candidate`
/// in `epoch` (`AdmitValidator`, [`Pool::admit`]): their operators decided, on purpose, to take a
/// newcomer every quorum will need (nodes never send it by themselves).
pub fn admitted(state: &State, members: &[String], candidate: &str, epoch: u64) -> bool {
    let yes = members.iter().filter(|m| state.staking.pools.get(*m).and_then(|p| p.admit.as_ref()).is_some_and(|(c, h)| c == candidate && state.params.epoch_of(*h) == epoch)).count();
    !members.is_empty() && yes >= crate::epochs::quorum(members.len())
}

/// `ValidatorReady`'s rules but the window: `validator` is an eligible candidate outside `epoch`'s
/// set, which needs its members' admission for the next newcomer ([`members_admitting`]); a
/// quorum of them admitted it; and it hasn't signaled yet this epoch. So at most one candidate an
/// epoch may (each member admits one), once.
fn check_ready(state: &State, validator: &str, epoch: u64) -> Result<()> {
    let set = state.active_sets.get(&epoch).ok_or_else(|| anyhow!("no set for epoch {epoch}"))?;
    if set.iter().any(|v| v == validator) {
        bail!("validator {} is in the set already", short(validator));
    }
    if !eligible(state, validator) {
        bail!("validator {} isn't eligible to join (jailed, tombstoned, removed or below the minimum self-bond)", short(validator));
    }
    let Some(members) = members_admitting(state, epoch) else { bail!("the set takes its next newcomer without a readiness signal (only a set of one or two members needs one, on chains without admins)") };
    if !admitted(state, &members, validator, epoch) {
        bail!("validator {} needs {} of the set's {} members to admit it first (`blockula validator admit`, epoch {epoch})", short(validator), crate::epochs::quorum(members.len()), members.len());
    }
    if state.staking.pools.get(validator).and_then(|p| p.ready_at).is_some_and(|h| state.params.epoch_of(h) == epoch) {
        bail!("validator {} already signaled readiness in epoch {epoch}", short(validator));
    }
    Ok(())
}

/// `AdmitValidator`'s rules: `member` is an eligible member of `epoch`'s set, which needs its
/// members' admission for the next newcomer ([`members_admitting`]), `candidate` is an eligible
/// candidate outside it, and `member` hasn't admitted one yet this epoch.
fn check_admit(state: &State, member: &str, candidate: &str, epoch: u64) -> Result<()> {
    let set = state.active_sets.get(&epoch).ok_or_else(|| anyhow!("no set for epoch {epoch}"))?;
    if !set.iter().any(|v| v == member) || !eligible(state, member) {
        bail!("only an eligible member of epoch {epoch}'s set admits newcomers; validator {} isn't one", short(member));
    }
    if members_admitting(state, epoch).is_none() {
        bail!("the set takes its next newcomer without its members' admission (only a set of one or two members needs it, on chains without admins)");
    }
    if set.iter().any(|v| v == candidate) || !eligible(state, candidate) {
        bail!("{} isn't an eligible candidate outside the set", short(candidate));
    }
    if let Some((c, h)) = state.staking.pools.get(member).and_then(|p| p.admit.as_ref()).filter(|(_, h)| state.params.epoch_of(*h) == epoch) {
        bail!("validator {} already admitted {} in epoch {epoch} (at block {h}; once an epoch)", short(member), short(c));
    }
    Ok(())
}

/// Whether `validator` may signal readiness in `epoch` (`ValidatorReady`'s rules but the window).
pub fn may_show_up(state: &State, validator: &str, epoch: u64) -> bool {
    check_ready(state, validator, epoch).is_ok()
}

/// Whether `validator`'s node should signal readiness in the block at `height`
/// ([`showed_up`]): it's in its epoch's last tenth ([`ready_from`]) and [`may_show_up`]. The
/// node sends `ValidatorReady` then (`beacon::duties`); it changes nothing but the readiness
/// record.
pub fn should_show_up(state: &State, validator: &str, height: u64) -> bool {
    let epoch = state.params.epoch_of(height);
    height >= ready_from(&state.params, epoch) && may_show_up(state, validator, epoch)
}

/// Most seats validators owned by `Params::bootstrap_excluded` accounts (the founder's and the
/// foundation's) may hold in a set of `m` once they're newcomers: `MAX_FOUNDER_SEATS` (DEC-3), and
/// fewer than f = ⌊(m−1)/3⌋, so together they can't halt the set or veto its council, even with
/// one more member down (audit xcut-economic-2). Seats they hold already aren't taken away.
pub fn excluded_seats_cap(m: usize) -> usize {
    crate::genesis_builder::MAX_FOUNDER_SEATS.min(churn_limit(m).saturating_sub(1))
}

/// Whether the set `trial` (with newcomer `c` in it) keeps locked insiders within their seats
/// (audit xcut-economic-2): an account with unvested tokens owns at most one seat (its locked
/// tokens can fund one validator that ranks, not one per 1% of the supply), and the founder's and
/// the foundation's accounts no more than [`excluded_seats_cap`] together. Only a newcomer is
/// held to it: members carried over keep their seats.
fn insiders_within_seats(state: &State, height: u64, trial: &[String], c: &str) -> bool {
    let owner = |v: &str| state.validators.get(v).map(|r| r.owner.clone()).unwrap_or_default();
    let mine = owner(c);
    if has_unvested_lock(state, &mine, height) && trial.iter().filter(|v| owner(v) == mine).count() > 1 {
        return false;
    }
    let excluded = &state.params.bootstrap_excluded;
    !excluded.contains(&mine) || trial.iter().filter(|v| excluded.contains(&owner(v))).count() <= excluded_seats_cap(trial.len())
}

/// Picks the next set (see the module docs). Pure: the API previews it.
pub fn select_next(state: &State, epoch: u64) -> Selection {
    let current: Vec<String> = state.active_sets.get(&epoch).cloned().unwrap_or_default();
    let cap = state.chain_params.max_validators.max(1) as usize;
    let height = state.params.last_of(epoch);
    let mut newcomers: Vec<String> = state.validators.keys().filter(|v| !current.contains(v) && eligible(state, v)).cloned().collect();
    let ranks = Ranks::new(state, height, current.iter().chain(&newcomers));
    let pinned = |v: &str| ranks.of(v).0;
    // Members that may stay; the ineligible leave whatever the limit.
    let mut keep: Vec<String> = current.iter().filter(|v| eligible(state, v)).cloned().collect();
    ranks.sort(&mut keep);
    // Over the cap (governance lowered it): the lowest-ranked leave, ⌊(n−1)/3⌋ (at least one)
    // per epoch.
    let mut shrink = churn_limit(current.len()).max(1);
    while keep.len() > cap && shrink > 0 {
        keep.pop();
        shrink -= 1;
    }
    // The members carried over: who admits a newcomer past the churn limit.
    let carried = keep.clone();
    // Newcomers, best first: into a free seat, or in place of the lowest-ranked member they
    // outrank, while newcomers stay within the churn limit of the resulting set and insiders
    // within their seats. Admin-pinned validators (dev and test chains) skip both: that's the
    // admin's call.
    ranks.sort(&mut newcomers);
    let mut joined = 0;
    for c in newcomers {
        let free = keep.len() < cap;
        if !free && !keep.last().is_some_and(|last| ranks.of(&c) > ranks.of(last)) {
            break;
        }
        let size = if free { keep.len() + 1 } else { keep.len() };
        if !pinned(&c) && joined + 1 > churn_limit(size) {
            if joined + 1 > newcomers_allowed(state, keep.len() - joined, size, free) {
                break;
            }
            // Past the churn limit (a set of 2 or 3): only a newcomer a quorum of the members it
            // joins admitted, and that showed up. A later-ranked one may be.
            if !admitted(state, &carried, &c, epoch) || !showed_up(state, &c, epoch) {
                continue;
            }
        }
        let mut trial = keep.clone();
        if !free {
            trial.pop();
        }
        trial.push(c.clone());
        if !pinned(&c) && !insiders_within_seats(state, height, &trial, &c) {
            continue;
        }
        keep = trial;
        ranks.sort(&mut keep);
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
    // Punished faults and epochs' views live as long as the epochs' sets (`State::end_epoch`
    // keeps the next `epoch_sets_kept`), so no window governance sets can reopen a fault whose
    // record is gone (and `Pool::slashed_from` refuses it anyway).
    let oldest = (epoch + 2).saturating_sub(crate::chain::epoch_sets_kept(&state.chain_params));
    let stale: Vec<String> = state.staking.punished.keys().filter(|k| k.split('/').nth(1).and_then(|e| e.parse::<u64>().ok()).is_some_and(|e| e < oldest)).cloned().collect();
    for k in stale {
        put_punished(state, &mut j, &k, None);
    }
    let old_views: Vec<u64> = state.staking.views.keys().copied().take_while(|e| *e < oldest).collect();
    for e in old_views {
        j.staking.views.touch(&state.staking.views, &e);
        state.staking.views.remove(&e);
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
    Ranks::new(state, state.params.last_of(epoch), c.iter()).sort(&mut c);
    c.truncate(limit);
    c
}

// ------------------------------------------------------------------- the bootstrap delegation

/// DEC-5's validator bootstrap delegation (docs/TOKENOMICS.md, section 1), run at every epoch
/// boundary once [`end_epoch`] has fixed the next set. The keyless bootstrap account
/// (`chain::bootstrap_delegation`):
///
/// 1. sends what its stake earned since the last boundary to the treasury (it sells shares worth
///    the gain; operators keep their commission, minted to them as shares) and books a slash as a
///    loss: afterwards each of its delegations' principal is what it's worth;
/// 2. keeps what its vesting lock still holds (mainnet: a curve from year 1 to year 4) and
///    returns whatever it holds beyond that to the treasury. Without a lock everything returns at
///    the first boundary, so the bucket is never stranded (audit F12);
/// 3. spreads what it keeps equally over the next set's members whose owner isn't in
///    `Params::bootstrap_excluded` (the founder's and the foundation's accounts): it leaves every
///    other pool and moves each delegation to its share, leaving alone one within
///    [`MIN_DELEGATION`] of it so rounding dust doesn't trade every epoch. What doesn't divide
///    evenly stays liquid.
///
/// The protocol moves this stake at once, without unbonding entries, but only at boundaries and
/// only by the formula. Stake it took out of a pool isn't reached by evidence for an earlier
/// epoch that arrives later (the operator's and every other delegator's stake still is), which
/// only spares the treasury. Its stake earns rewards like any other but doesn't count toward the
/// set's ranking ([`rank`]) or governance's quorum base (`governance::unlocked_supply`).
pub fn bootstrap_epoch(state: &mut State, epoch: u64) {
    let boot = state.bootstrap();
    if state.account(&boot).balance == 0 && delegations_of(state, &boot).is_empty() {
        return;
    }
    let mut j = Journal::default();
    let (height, treasury) = (state.params.last_of(epoch), state.treasury());
    // 1. Gains to the treasury, losses booked.
    for (v, d) in delegations_of(state, &boot) {
        let worth = delegation_value(state, &boot, &v);
        if worth > d.principal {
            let taken = take_from_delegation(state, &mut j, &boot, &v, worth - d.principal);
            if taken > 0 {
                state.j_credit(&mut j, &treasury, taken);
            }
        }
        settle_principal(state, &mut j, &boot, &v);
    }
    // 2. What it keeps.
    let staked = delegations_of(state, &boot).iter().map(|(v, _)| delegation_value(state, &boot, v)).fold(0u64, u64::saturating_add);
    let holds = state.account(&boot).balance.saturating_add(staked);
    let keep = state.account(&boot).unvested_at(height).min(holds);
    // 3. Its share for each member it backs: out of the pools above it, back to the treasury,
    //    then into the pools below it.
    let excluded = |v: &String| state.validators.get(v).is_none_or(|r| state.params.bootstrap_excluded.contains(&r.owner));
    let targets: Vec<String> = state.active_sets.get(&(epoch + 1)).map(|set| set.iter().filter(|v| eligible(state, v) && !excluded(v)).cloned().collect()).unwrap_or_default();
    let share = if targets.is_empty() { 0 } else { keep / targets.len() as u64 };
    for (v, d) in delegations_of(state, &boot) {
        let worth = delegation_value(state, &boot, &v);
        let sold = if share == 0 || !targets.contains(&v) {
            unbond(state, &mut j, &boot, &v, d.shares).map_or(0, |(value, _)| value)
        } else if worth > share.saturating_add(MIN_DELEGATION) {
            take_from_delegation(state, &mut j, &boot, &v, worth - share)
        } else {
            0
        };
        if sold > 0 {
            state.j_credit(&mut j, &boot, sold);
        }
        settle_principal(state, &mut j, &boot, &v);
    }
    let back = (holds - keep).min(state.account(&boot).balance);
    if back > 0 {
        state.j_debit(&mut j, &boot, back).expect("within the balance");
        state.j_credit(&mut j, &treasury, back);
    }
    for v in &targets {
        let room = share.saturating_sub(delegation_value(state, &boot, v));
        let amount = room.min(state.account(&boot).balance);
        if room < MIN_DELEGATION || amount == 0 || bond(state, &mut j, &boot, v, amount, amount).is_err() {
            continue;
        }
        state.j_debit(&mut j, &boot, amount).expect("within the balance");
        settle_principal(state, &mut j, &boot, v);
    }
}

/// Sets `delegator`'s principal in `validator`'s pool to what the delegation is worth now (the
/// bootstrap delegation's gains and losses are settled every epoch).
fn settle_principal(state: &mut State, j: &mut Journal, delegator: &str, validator: &str) {
    let key = dkey(delegator, validator);
    let worth = delegation_value(state, delegator, validator);
    if let Some(d) = state.staking.delegations.get(&key).filter(|d| d.principal != worth).cloned() {
        put_delegation(state, j, &key, Some(Delegation { principal: worth, ..d }));
    }
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
    holdings_valued(state, account, false)
}

/// [`holdings`], each delegation counted at most at its principal (what was put in): what covers
/// a vesting account's locks (econ.rs). Gains not sold yet are rewards earned by locked tokens,
/// which vest with them (3.7: `lock_gains` adds them to the vesting lock when they're sold), so
/// they can't cover the lock before that.
pub fn holdings_at_cost(state: &State, account: &str) -> (u64, u64) {
    holdings_valued(state, account, true)
}

fn holdings_valued(state: &State, account: &str, at_cost: bool) -> (u64, u64) {
    let worth = |p: &Pool, d: &Delegation| if at_cost { p.value(d.shares).min(d.principal) } else { p.value(d.shares) };
    let bonded = owned_by(&state.staking.delegations, account).filter_map(|(k, d)| state.staking.pools.get(k.split_once('/')?.1).map(|p| worth(p, d))).fold(0u64, u64::saturating_add);
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
/// owner gets its commission as new shares ([`credit_reward`]). Returns what was paid: the caller
/// burns the rest or sends it to the treasury.
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
            credit_reward(state, j, &v, reward, height);
            paid += reward;
        }
    }
    paid
}

/// The commission a validator charges now: its own, at most `commission_max_bps` (a lowered cap
/// binds every validator at once; audit gap-5-1, gap-6-2).
pub fn commission_rate(state: &State, validator: &str) -> u64 {
    state.validators.get(validator).map_or(0, |r| r.commission_bps.min(state.chain_params.commission_max_bps))
}

/// Accounts with unvested tokens at `height` (insiders, 3.7), the keyless bootstrap delegation
/// aside. Only genesis allocations carry vesting locks, so this is O(vesting accounts).
pub fn insiders(state: &State, height: u64) -> Vec<String> {
    let boot = state.bootstrap();
    state.accounts.indexed().into_iter().filter(|a| *a != boot && has_unvested_lock(state, a, height)).collect()
}

/// Adds `reward` to a pool at `height`: every share gains, less the commission ([`commission_rate`]
/// of the reward), which every share pays alike, so shares stay one price. The owner gets it as new
/// shares worth (at most) it, except the part paid on stake that may not pay an owner it chose,
/// which goes to the treasury:
///
/// * stake of accounts still vesting (other than the owner's own): none to the owner, so locked
///   tokens yield no liquid income through a validator their holder picked, its own non-vesting
///   account or an accomplice's at 100% (3.7, audit xcut-economic-3);
/// * the bootstrap delegation's stake: at most `bootstrap_commission_cap_bps` to the owner (DEC-5:
///   its rewards are the treasury's; audit F12, review-fix-gov-1).
fn credit_reward(state: &mut State, j: &mut Journal, validator: &str, reward: u64, height: u64) {
    let (owner, bps) = (state.validators.get(validator).map(|r| r.owner.clone()).unwrap_or_default(), commission_rate(state, validator));
    let commission = bps_of(reward, bps);
    let to_owner = if commission == 0 { 0 } else { owners_commission(state, validator, &owner, reward, bps, height).min(commission) };
    let to_treasury = commission - to_owner;
    let p = pool_mut(state, j, validator).expect("rewarded pools exist");
    p.stake += reward - to_treasury;
    p.rewards += reward - to_treasury;
    // Shares that make `to_owner` of the new stake: to_owner × shares / (the rest of it).
    let minted = mul_div(to_owner as u128, p.shares, (p.stake - to_owner) as u128).unwrap_or(0);
    if minted > 0 {
        p.shares += minted;
        let key = dkey(&owner, validator);
        let mut d = state.staking.delegations.get(&key).cloned().unwrap_or_default();
        d.shares += minted;
        put_delegation(state, j, &key, Some(d));
    }
    if to_treasury > 0 {
        let treasury = state.treasury();
        state.j_credit(j, &treasury, to_treasury);
    }
}

/// What of a `reward`'s commission (at `bps`) the pool's owner gets (see [`credit_reward`]): the
/// full rate on the part earned by shares that aren't insiders' or the bootstrap delegation's, at
/// most `bootstrap_commission_cap_bps` on the bootstrap's part, nothing on insiders' parts.
fn owners_commission(state: &State, validator: &str, owner: &str, reward: u64, bps: u64, height: u64) -> u64 {
    let Some(shares) = state.staking.pools.get(validator).map(|p| p.shares).filter(|s| *s > 0) else { return 0 };
    let held = |a: &str| if a == owner { 0 } else { state.staking.delegations.get(&dkey(a, validator)).map_or(0, |d| d.shares) };
    let part = |s: u128| mul_div(reward as u128, s, shares).unwrap_or(0).min(reward as u128) as u64;
    let boot = part(held(&state.bootstrap()));
    let locked = part(insiders(state, height).iter().map(|a| held(a)).sum());
    let rest = reward.saturating_sub(boot).saturating_sub(locked);
    bps_of(rest, bps) + bps_of(boot, bps.min(state.chain_params.bootstrap_commission_cap_bps))
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

/// SF-13: a validator's system txs (attestations, beacon commitments and reveals, evidence,
/// `UpdateValidator`, whose target only the signer can name, and a small set's readiness and
/// admissions) may be signed by its consensus key with `from` = its owner, who pays the fee, so
/// the consensus key only needs a fee float, if that. Admission calls this when the signer isn't
/// a key on `from`: the key record such a signature acts with, or `None` (not allowed).
///
/// What the owner pays for is bounded by the validator's duties (audit F04: the key is hot, and
/// protocol v1 can't revoke it): such a tx whose action fails isn't admitted, so it's never
/// charged (`State::apply_j`); it updates the validator at most once an epoch (`UpdateValidator`),
/// signals readiness and admits a newcomer at most once an epoch each, and only while the set is
/// one or two members; and a tombstoned or removed validator's key signs only a reveal for a
/// commitment it made while active (the beacon checks there is one), never anything else again.
pub fn system_tx_key(state: &State, tx: &Tx, sender: &str, signer: &str) -> Option<KeyRecord> {
    let system = matches!(tx.action, Action::AttestClaim { .. } | Action::AttestRevert { .. } | Action::BeaconCommit { .. } | Action::BeaconReveal { .. } | Action::ReportEquivocation { .. } | Action::UpdateValidator { .. } | Action::ValidatorReady { .. } | Action::AdmitValidator { .. });
    if !system {
        return None;
    }
    let v = validator_of_key(state, signer)?;
    let rec = state.validators.get(&v).filter(|r| r.owner == sender && r.consensus_key == tx.public_key)?;
    if matches!(rec.status, ValidatorStatus::Tombstoned | ValidatorStatus::Removed) && !matches!(tx.action, Action::BeaconReveal { .. }) {
        return None;
    }
    Some(KeyRecord { label: "validator consensus key".into(), role: KeyRole::Owner, added_at: rec.registered_at, spent: 0, window_start: 0 })
}

/// A validator duty (audit F32/F56): a beacon commitment or reveal, equivocation evidence, or a
/// claim or revert attestation, signed by a validator's consensus key from its own account or its
/// owner's (SF-13), while the validator is in the active set of `epoch` or the next. A reveal is
/// one whatever the set: the validator owes it once it committed (it may have left the set since,
/// and `beacon::end_epoch` jails and slashes a committer that doesn't reveal), so it's a duty while
/// the validator has an unopened commitment for `epoch + 1` (review of F32/F56). So are a small
/// set's readiness and admissions for `epoch` while they'd apply (audit review-r2-consensus-2:
/// the node sends the readiness itself, from the account that pays its beacon duties, so it must
/// not hold them out of the lane): at most one candidate an epoch may signal readiness, once, and
/// each of the set's one or two members admit once. Returns the validator and the kind of duty. A
/// full mempool must not refuse these for want of a tip (they carry none from an owner's account:
/// `State::admit`), so the mempool queues them in a small lane of their own and proposals take
/// them first (`Chain::admit_to_mempool`). Mempool policy only: blocks are verified the same
/// either way.
pub fn duty_of(state: &State, tx: &Tx, epoch: u64) -> Option<(String, &'static str)> {
    let kind = match tx.action {
        Action::BeaconCommit { .. } => "beacon_commit",
        Action::BeaconReveal { .. } => "beacon_reveal",
        Action::ReportEquivocation { .. } => "report_equivocation",
        Action::AttestClaim { .. } => "attest_claim",
        Action::AttestRevert { .. } => "attest_revert",
        Action::ValidatorReady { .. } => "validator_ready",
        Action::AdmitValidator { .. } => "admit_validator",
        _ => return None,
    };
    let v = validator_of_key(state, &tx.signer_key_id().ok()?)?;
    let rec = state.validators.get(&v).filter(|r| r.consensus_key == tx.public_key && (tx.from == v || tx.from == r.owner))?;
    let due = match &tx.action {
        Action::ValidatorReady { epoch: e } => *e == epoch && may_show_up(state, &v, epoch),
        Action::AdmitValidator { validator, epoch: e } => *e == epoch && check_admit(state, &v, validator, epoch).is_ok(),
        _ => {
            let active = [epoch, epoch + 1].iter().any(|e| state.active_sets.get(e).is_some_and(|s| s.contains(&rec.address)));
            let owed = matches!(&tx.action, Action::BeaconReveal { epoch: target, .. } if *target == epoch + 1 && state.beacon.rounds.get(target).and_then(|r| r.get(&rec.address)).is_some_and(|e| e.secret.is_none()));
            active || owed
        }
    };
    due.then_some((v, kind))
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
        ensure!(p.slashed_from.is_none() || p.tombstoned_at.is_some(), "pool {v} was slashed for double-signing but isn't tombstoned");
        ensure!(p.admit.as_ref().is_none_or(|(c, _)| state.validators.contains_key(c) && c != v), "pool {v} admitted no registered newcomer");
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
        ensure!(s.by_validator.contains_key(&vkey(e, k)), "entry {k} is missing from the by-validator index");
    }
    ensure!(s.by_validator.len() == s.entries.len(), "the by-validator index lists entries that don't exist");
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
        Action::Undelegate { validator, amount, .. } if *amount > 0 => format!("{from} started unbonding {} of their stake in validator {}'s pool", fmt_fang(*amount), v(validator)),
        Action::Undelegate { validator, shares, .. } => {
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
        Action::ValidatorReady { epoch } => format!("{} is ready to join the validator set (epoch {epoch})", signed_by(state, tx, from, &v)),
        Action::AdmitValidator { validator, epoch } => format!("{} admitted validator {} into the validator set at the end of epoch {epoch}", signed_by(state, tx, from, &v), v(validator)),
        _ => String::new(),
    }
}

/// "validator X" when a validator's consensus key signed `tx`, else `from`.
fn signed_by(state: &State, tx: &Tx, from: &str, v: &dyn Fn(&str) -> String) -> String {
    tx.signer_key_id().ok().and_then(|k| validator_of_key(state, &k)).map(|a| format!("validator {}", v(&a))).unwrap_or_else(|| from.to_string())
}

/// Basis points as a percentage: 250 → "2.5".
pub fn fmt_bps(bps: u64) -> String {
    let (whole, frac) = (bps / 100, bps % 100);
    if frac == 0 { whole.to_string() } else { format!("{whole}.{}", format!("{frac:02}").trim_end_matches('0')) }
}

#[cfg(test)]
mod tests;
