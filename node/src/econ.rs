//! Economics (P-ECON, MAINNET-DESIGN 3.6, 3.7, DEC-6, DEC-11): the base fee market and the block
//! size limit, issuance and the endowment stream, account locks (vesting and votes) and their
//! enforcement, the account creation fee, and names (prices, yearly registration, renewal, grace,
//! transfer, release, the reserved list).
//!
//! **Base fee.** A tx in block `h` is charged `base_fee(h, size) + tip`, at most its `fee`: the
//! base part (`flat + rate × size`, the rate in milli-drops per byte) is burned and the tip goes to
//! the proposer. The fee for `h` is fixed when block `h − FEE_LAG` executes (17 =
//! `MAX_EXEC_LAG + 1`), so a proposer whose state is within the exec lag knows it exactly: the
//! state keeps a ring of [`FeeSlot`]s for the last executed height and the 17 after it. Each block
//! moves the rate toward `target_fullness_bps` of the block-size limit (a full block raises it by
//! `base_fee_max_change_bps`, an empty one lowers it as much), never below the floor
//! (`base_fee_floor_flat + base_fee_floor_per_byte × size`, today's `min_fee`). A parameter change
//! reaches the ring 17 blocks later, like everything else in it. The block-size limit
//! (`max_block_bytes`) is fixed the same way, so a verifier that has executed `h − 17` checks a
//! block's size and every tx's fee exactly (consensus.rs), and a block with txs must cite such a
//! state. Statelessly, a fee is only checked against the lowest base fee any parameters allow.
//!
//! **Issuance.** At every epoch boundary, [`issue`] mints `S(y) × rate_ppm(y) / (10⁶ ×
//! epochs_per_year)`, where `S(y)` is the supply when year `y`'s first epoch ends (before its
//! mint) and the rate starts at `issuance_initial_ppm` and decays by `issuance_decay_ppm` a year to
//! `issuance_floor_ppm`. `issuance_stakers_bps` of it goes to `staking::distribute_rewards` (what
//! it doesn't place goes to the treasury), `issuance_pool_bps` to the contribution pool and the
//! rest to the treasury. docs/TOKENOMICS.md has the resulting table.
//!
//! **Endowment and treasury.** Both are keyless accounts whose genesis allocation carries a
//! vesting lock (10 and 4 years). Each epoch, whatever of the endowment has vested streams into
//! the contribution pool. The treasury's lock limits what `TreasurySpend` may move
//! ([`transferable`]); its inflows (issuance, name fees) are free at once.
//!
//! **Locks.** `Account::locks` overlap: the largest applies. Transferable balance = liquid −
//! max(0, largest lock − staked − unbonding), so locked tokens can be staked and delegated, never
//! moved out. Every tx's action phase is checked ([`check_locks`]): an account may not end it
//! with less transferable than zero (or less than it had, if fees already took it below). Fees
//! may come from locked tokens. Staking rewards credited to a vesting account join its lock
//! ([`lock_rewards`]). Votes lock with [`lock`]; [`voting_balance`] leaves unvested tokens out.
//!
//! **Accounts.** An action that opens an account (a transfer to a new address, a hunt awarded to
//! one…) burns `account_creation_fee` per account from the sender, on top of what it moves
//! (DEC-6). Transfers that open an account still carry at least `MIN_NEW_ACCOUNT`.
//!
//! **Names** (DEC-11). Registering costs `name_price_3/4/5` by length and lasts
//! `name_registration_blocks`; `RenewName` adds years at `name_renewal_bps` of the price each (at
//! most `MAX_RENEW_YEARS` paid ahead). An expired name doesn't resolve; for `name_grace_blocks`
//! only its owner can renew it, then anyone can register it. Lapsed records are swept a few per
//! block. Reserved names (genesis) can't be registered.
//!
//! The scalars (the fee ring, the issuance year, the sweep cursor) are one META record; the
//! reserved names are a map (`state::ns::RESERVED_NAMES`).

use crate::chain::{short, system_account, BlockInfo, Journal, KeyRecord, Lock, LockAmount, NameRecord, Params, State};
use crate::consensus::MAX_EXEC_LAG;
use crate::params::ChainParams;
use crate::state::codec::{decode, encode};
use crate::state::{meta, ns, JMap, SubState, TMap, Table};
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

/// The base fee (and block-size limit) of block `h` is fixed when block `h − FEE_LAG` executes.
pub const FEE_LAG: u64 = MAX_EXEC_LAG + 1;
/// Rates are kept in milli-drops per byte, so a 12.5% step near the 10-drop floor is exact.
pub const RATE_SCALE: u64 = 1_000;
/// Highest rate. Only there so the largest tx's base fee fits a u64 (about 2.8 × 10¹¹ drops per
/// byte: a fee nobody can pay long before).
pub const MAX_RATE: u64 = u64::MAX / MAX_TX_BYTES as u64;
/// Most locks an account may hold at once (vote locks pile up; `Unlock` drops expired ones).
pub const MAX_LOCKS: usize = 64;
/// The id of a genesis allocation's vesting lock.
pub const VESTING_LOCK: &str = "vesting";
/// Lapsed names checked for removal per block.
pub const NAME_SWEEP: usize = 32;
/// An issuance year: 365.25 days, in milliseconds.
const YEAR_MS: u128 = 31_557_600_000;

/// What block `h` charges and holds, fixed when `h − FEE_LAG` executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct FeeSlot {
    /// Drops every tx pays, whatever its size.
    #[serde(with = "u64_str")]
    pub flat: u64,
    /// Milli-drops per byte.
    #[serde(with = "u64_str")]
    pub rate: u64,
    /// Most tx bytes the block may carry.
    pub max_block_bytes: u64,
}

impl FeeSlot {
    /// The floor under these parameters.
    pub fn floor(cp: &ChainParams) -> Self {
        Self { flat: cp.base_fee_floor_flat, rate: floor_rate(cp), max_block_bytes: cp.max_block_bytes }
    }

    /// The base fee of a tx of `size` bytes: `flat + rate × size / 1000`, rounded down.
    pub fn fee(&self, size: u64) -> u64 {
        let v = self.flat as u128 + self.rate as u128 * size as u128 / RATE_SCALE as u128;
        v.min(u64::MAX as u128) as u64
    }
}

fn floor_rate(cp: &ChainParams) -> u64 {
    cp.base_fee_floor_per_byte.saturating_mul(RATE_SCALE).min(MAX_RATE)
}

/// The current issuance year.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, BorshSerialize, BorshDeserialize)]
pub struct Issuance {
    /// 1 from the first epoch on (0: nothing minted yet).
    pub year: u64,
    /// The supply when this year's first epoch ended, before its mint: what the rate applies to.
    #[serde(with = "u64_str")]
    pub base_supply: u64,
    /// This year's rate, in ppm of `base_supply` a year.
    pub rate_ppm: u64,
}

/// A name nobody can register (genesis `reserved_names`; governance may assign it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ReservedName {
    pub reserved_at: u64,
}

/// Economics' slice of the state (namespace `state::ns::RESERVED_NAMES`, META key `econ`).
#[derive(Clone, Debug, Default)]
pub struct Econ {
    /// Bare name (as `normalize_name` returns it) → since when it's reserved.
    pub reserved_names: TMap<String, ReservedName>,
    /// The last executed height (0: genesis). Txs execute next at `height + 1` ([`now`]).
    pub height: u64,
    /// Fee slots for heights `height ..= height + FEE_LAG` (empty only in states that didn't
    /// start from a genesis: unit tests).
    pub fees: Vec<FeeSlot>,
    pub issuance: Issuance,
    /// The lapsed-name sweep goes on after this name ("": from the start).
    pub name_cursor: String,
}

impl SubState for Econ {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Econ { reserved_names, height: _, fees: _, issuance: _, name_cursor: _ } = self;
        vec![(ns::RESERVED_NAMES, reserved_names)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Econ { reserved_names, height: _, fees: _, issuance: _, name_cursor: _ } = self;
        vec![(ns::RESERVED_NAMES, reserved_names)]
    }

    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        let Econ { reserved_names: _, height, fees, issuance, name_cursor } = self;
        vec![(meta::ECON, encode(&(height, fees, issuance, name_cursor)))]
    }

    fn load_meta(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        if key != meta::ECON {
            return Ok(false);
        }
        (self.height, self.fees, self.issuance, self.name_cursor) = decode(value)?;
        Ok(true)
    }
}

/// Undo log for [`Econ`] (part of `chain::Journal`). Txs change only the reserved names; the
/// scalars move in the end-of-block steps, which aren't journaled.
#[derive(Default)]
pub struct EconJournal {
    pub reserved_names: JMap<String, ReservedName>,
}

impl EconJournal {
    pub fn absorb(&mut self, other: EconJournal) {
        let EconJournal { reserved_names } = other;
        self.reserved_names.absorb(reserved_names);
    }

    pub fn rollback(self, econ: &mut Econ) {
        let EconJournal { reserved_names } = self;
        reserved_names.restore(&mut econ.reserved_names);
    }
}

// ---- the base fee (3.6) ----

/// The genesis fee ring: the floor for heights 0 ..= FEE_LAG.
pub fn init_genesis(state: &mut State) {
    state.econ.height = 0;
    state.econ.fees = vec![FeeSlot::floor(&state.chain_params); FEE_LAG as usize + 1];
}

/// Block `height`'s fee slot, if it's fixed already (the last executed height and the
/// `FEE_LAG` after it).
pub fn slot(state: &State, height: u64) -> Option<FeeSlot> {
    let i = height.checked_sub(state.econ.height)?;
    state.econ.fees.get(usize::try_from(i).ok()?).copied()
}

/// The burned part of the charge for a tx of `size` bytes in block `height`. Exact for the
/// heights [`slot`] knows; past them, the most the rate can have reached by then (the last
/// fixed rate raised by the full step each block); before them, the floor.
pub fn base_fee(state: &State, height: u64, size: u64) -> u64 {
    if let Some(s) = slot(state, height) {
        return s.fee(size);
    }
    let cp = &state.chain_params;
    let Some(last) = state.econ.fees.last() else { return FeeSlot::floor(cp).fee(size) };
    let last_height = state.econ.height + state.econ.fees.len() as u64 - 1;
    if height < last_height {
        return FeeSlot::floor(cp).fee(size);
    }
    let mut rate = last.rate as u128;
    for _ in 0..height - last_height {
        if rate >= MAX_RATE as u128 {
            break;
        }
        rate += (rate * cp.base_fee_max_change_bps as u128 / 10_000).max(1);
    }
    FeeSlot { flat: last.flat.max(cp.base_fee_floor_flat), rate: rate.min(MAX_RATE as u128) as u64, max_block_bytes: last.max_block_bytes }.fee(size)
}

/// What `tx` is charged in block `height`: the base fee plus its tip (`None` on overflow).
#[cfg_attr(not(test), allow(dead_code))] // P-STAKE (junk charges) and clients
pub fn charge(state: &State, tx: &Tx, height: u64) -> Option<u64> {
    base_fee(state, height, tx.size()).checked_add(tx.tip)
}

/// The exact fee check (admission and execution): the charge, if `fee` covers it.
pub fn check_fee(state: &State, tx: &Tx, height: u64) -> Result<u64> {
    let base = base_fee(state, height, tx.size());
    match base.checked_add(tx.tip) {
        Some(c) if c <= tx.fee => Ok(c),
        _ => bail!("fee too low: this tx costs {} drops at block {height} (base fee {base} + tip {}), but its fee allows {}", base.saturating_add(tx.tip), tx.tip, tx.fee),
    }
}

/// The highest flat part and rate among the slots fixed after the last executed height (`/api/fees`
/// serves it as `suggested`; the SDK prices txs with it).
pub fn suggested(state: &State) -> FeeSlot {
    let upcoming = state.econ.fees.iter().skip(1);
    upcoming.fold(None, |acc: Option<FeeSlot>, s| Some(match acc {
        None => *s,
        Some(a) => FeeSlot { flat: a.flat.max(s.flat), rate: a.rate.max(s.rate), max_block_bytes: a.max_block_bytes.min(s.max_block_bytes) },
    }))
    .unwrap_or_else(|| FeeSlot::floor(&state.chain_params))
}

/// A `fee` (without tip) for a tx of `size` bytes that covers the base fee of any block in the
/// next `FEE_LAG` ([`suggested`]'s). Wallets and the node's own txs use it; only the block's
/// actual base fee is charged.
pub fn max_fee(state: &State, size: u64) -> u64 {
    suggested(state).fee(size)
}

/// The lowest base fee any parameters allow (their bounds' minimum flat part and rate): the
/// stateless fee check, which can never refuse a tx some state would accept.
pub fn lowest_base_fee(size: u64) -> u64 {
    let least = |key| ChainParams::bounds(key, &Params::default()).map(|(min, _)| min).unwrap_or(0);
    least("base_fee_floor_flat").saturating_add(least("base_fee_floor_per_byte").saturating_mul(size))
}

/// The slot after `prev`: its rate moved by how full the block just executed was (`used` tx
/// bytes against `limit`, the limit it was built under), with the current floor, flat part and
/// size limit. A full block raises the rate by `base_fee_max_change_bps`, an empty one lowers it
/// as much, linearly in between around `target_fullness_bps`.
pub fn next_slot(prev: &FeeSlot, used: u64, limit: u64, cp: &ChainParams) -> FeeSlot {
    let max = limit.max(1) as u128;
    let used = (used as u128).min(max);
    let target = (max * cp.target_fullness_bps as u128 / 10_000).max(1);
    let (rate, step) = (prev.rate as u128, cp.base_fee_max_change_bps as u128);
    let next = if used > target && max > target {
        let up = rate * step * (used - target) / ((max - target) * 10_000);
        // At least one milli-drop, so tiny rates still climb.
        rate + if step > 0 { up.max(1) } else { 0 }
    } else if used < target {
        rate - rate * step * (target - used) / (target * 10_000)
    } else {
        rate
    };
    FeeSlot { flat: cp.base_fee_floor_flat, rate: next.clamp(floor_rate(cp) as u128, MAX_RATE as u128) as u64, max_block_bytes: cp.max_block_bytes }
}

/// Second end-of-block step, every block: fixes the slot for `height + FEE_LAG` from this
/// block's fullness, then sweeps a few lapsed names. Keeps this block's slot (staking charges
/// skipped txs at it after this).
pub fn end_block(state: &mut State, b: &BlockInfo) {
    let cp = state.chain_params.clone();
    let e = &mut state.econ;
    if e.fees.len() != FEE_LAG as usize + 1 || e.height + 1 != b.height {
        // Only states that didn't start from a genesis (unit tests): start the ring here.
        e.fees = vec![FeeSlot::floor(&cp); FEE_LAG as usize + 1];
        e.height = b.height.saturating_sub(1);
    }
    let limit = e.fees[1].max_block_bytes;
    let next = next_slot(e.fees.last().expect("the ring is full"), b.tx_bytes, limit, &cp);
    e.fees.remove(0);
    e.fees.push(next);
    e.height = b.height;
    sweep_names(state, b.height);
}

// ---- issuance (3.6) and the endowment stream (DEC-5) ----

/// Epochs in an issuance year (365.25 days): 8,766 at 1 s blocks and 3,600-block epochs.
pub fn epochs_per_year(p: &Params) -> u64 {
    (YEAR_MS / (p.block_time_ms.max(1) as u128 * p.epoch_blocks.max(2) as u128)).max(1) as u64
}

/// Next year's rate: this one decayed by `issuance_decay_ppm`, never below the floor.
pub fn next_rate(rate_ppm: u64, cp: &ChainParams) -> u64 {
    let decayed = rate_ppm as u128 * 1_000_000u128.saturating_sub(cp.issuance_decay_ppm as u128) / 1_000_000;
    (decayed as u64).max(cp.issuance_floor_ppm)
}

/// One epoch's mint in this issuance year.
pub fn epoch_mint(iss: &Issuance, epochs_per_year: u64) -> u64 {
    (iss.base_supply as u128 * iss.rate_ppm as u128 / (1_000_000 * epochs_per_year.max(1) as u128)) as u64
}

fn mul_bps(amount: u64, bps: u64) -> u64 {
    (amount as u128 * bps as u128 / 10_000) as u64
}

/// First step at an epoch boundary: mints the epoch's issuance into validator pools, the
/// contribution pool and the treasury, then streams what the endowment has vested into the pool.
pub fn issue(state: &mut State, epoch: u64) {
    let epy = epochs_per_year(&state.params);
    let year = epoch / epy + 1;
    let (cp, supply) = (state.chain_params.clone(), state.supply);
    let iss = &mut state.econ.issuance;
    while iss.year < year {
        iss.rate_ppm = if iss.year == 0 { cp.issuance_initial_ppm } else { next_rate(iss.rate_ppm, &cp) };
        iss.year += 1;
        iss.base_supply = supply;
    }
    let height = state.params.last_of(epoch);
    let mint = epoch_mint(&state.econ.issuance, epy).min(u64::MAX - state.supply);
    let mut j = Journal::default();
    if mint > 0 {
        state.j_mint(&mut j, mint);
        let stakers = mul_bps(mint, cp.issuance_stakers_bps);
        let pool = mul_bps(mint, cp.issuance_pool_bps);
        let placed = crate::staking::distribute_rewards(state, &mut j, stakers, height).min(stakers);
        let (pool_addr, treasury) = (state.pool(), state.treasury());
        if pool > 0 {
            state.j_credit(&mut j, &pool_addr, pool);
        }
        // The treasury's share, plus whatever of the stakers' share no pool could take.
        state.j_credit(&mut j, &treasury, mint - placed - pool);
    }
    let endowment = crate::chain::endowment(state.params.prefix());
    let vested = transferable(state, &endowment, height);
    if vested > 0 {
        let pool_addr = state.pool();
        state.j_debit(&mut j, &endowment, vested).expect("transferable is within the balance");
        state.j_credit(&mut j, &pool_addr, vested);
    }
}

// ---- locks (3.7) ----

/// What `account` can move out at `height`: liquid − max(0, largest lock − staked − unbonding).
pub fn transferable(state: &State, account: &str, height: u64) -> u64 {
    free(state, account, height).clamp(0, u64::MAX as i128) as u64
}

/// [`transferable`] before clamping: negative when fees (or a slash) took locked tokens.
pub fn free(state: &State, account: &str, height: u64) -> i128 {
    let Some(a) = state.accounts.get(account) else { return 0 };
    let locked = a.locked_at(height);
    if locked == 0 {
        return a.balance as i128;
    }
    let (staked, unbonding) = crate::staking::holdings(state, account);
    a.balance as i128 - (locked as i128 - staked as i128 - unbonding as i128).max(0)
}

/// What `account` may vote with at `height`: its holdings (liquid + staked + unbonding) minus
/// what its vesting locks still hold. Vote locks don't count: one stake votes on many proposals.
#[cfg_attr(not(test), allow(dead_code))] // P-GOV (votes)
pub fn voting_balance(state: &State, account: &str, height: u64) -> u64 {
    let Some(a) = state.accounts.get(account) else { return 0 };
    let (staked, unbonding) = crate::staking::holdings(state, account);
    let held = a.balance as u128 + staked as u128 + unbonding as u128;
    held.saturating_sub(a.unvested_at(height) as u128).min(u64::MAX as u128) as u64
}

/// Locks `amount` of `account`'s holdings until (not including) height `until` under `id` (e.g.
/// `"vote:<proposal>"`). Locks overlap: the largest applies. Locking an id again only extends it
/// (the larger amount, the later end), so a lock never shrinks before it expires.
#[cfg_attr(not(test), allow(dead_code))] // P-GOV (vote locks)
pub fn lock(state: &mut State, j: &mut Journal, account: &str, id: String, amount: u64, until: u64) -> Result<()> {
    ensure!(!id.is_empty() && id.len() <= MAX_LABEL && id != VESTING_LOCK, "lock ids are 1–{MAX_LABEL} bytes (not “{VESTING_LOCK}”)");
    let a = state.accounts.get(account).ok_or_else(|| anyhow!("{} has no account", short(account)))?;
    let (staked, unbonding) = crate::staking::holdings(state, account);
    let held = a.balance as u128 + staked as u128 + unbonding as u128;
    ensure!(amount as u128 <= held, "can't lock {}: {} holds {}", fmt_fang(amount), short(account), fmt_fang(held.min(u64::MAX as u128) as u64));
    let existing = a.locks.iter().position(|l| l.id == id);
    ensure!(existing.is_some() || a.locks.len() < MAX_LOCKS, "{} already has {MAX_LOCKS} locks; unlock expired ones first", short(account));
    let locks = &mut state.acct_mut(j, account).locks;
    match existing {
        Some(i) => match &mut locks[i].amount {
            LockAmount::Until { amount: a, until: u } => {
                *a = (*a).max(amount);
                *u = (*u).max(until);
            }
            LockAmount::Vesting { .. } => bail!("lock {id} is a vesting lock"),
        },
        None => locks.push(Lock { reason: id.clone(), id, amount: LockAmount::Until { amount, until } }),
    }
    Ok(())
}

/// Drops `account`'s locks that hold nothing from `height` on (expired votes, finished vesting).
/// Returns how many it dropped.
#[cfg_attr(not(test), allow(dead_code))] // P-GOV (`Unlock`)
pub fn release_expired(state: &mut State, j: &mut Journal, account: &str, height: u64) -> u64 {
    let n = state.accounts.get(account).map(|a| a.locks.iter().filter(|l| l.amount.expired_at(height)).count()).unwrap_or(0);
    if n > 0 {
        state.acct_mut(j, account).locks.retain(|l| !l.amount.expired_at(height));
    }
    n as u64
}

/// Staking rewards credited to `account` at `height` vest like its allocation (3.7): they join
/// its vesting lock's total, so what's still unvested of them stays locked and the rest is free.
/// No vesting lock (or a finished one): nothing to do. Staking calls it when it pays rewards out.
#[cfg_attr(not(test), allow(dead_code))] // P-STAKE (rewards paid out)
pub fn lock_rewards(state: &mut State, j: &mut Journal, account: &str, amount: u64, height: u64) {
    let vesting = |l: &Lock| matches!(l.amount, LockAmount::Vesting { end, .. } if height < end);
    if amount == 0 || !state.accounts.get(account).is_some_and(|a| a.locks.iter().any(vesting)) {
        return;
    }
    for l in state.acct_mut(j, account).locks.iter_mut() {
        if let LockAmount::Vesting { total, end, .. } = &mut l.amount {
            if height < *end {
                *total = total.saturating_add(amount);
            }
        }
    }
}

/// After a tx's action: no account it touched may hold less transferable than zero, and the
/// sender none less than it had (`free_before`, which fees may have taken below zero). Moving
/// liquid tokens into stake keeps the lock covered, so staking locked tokens passes; moving them
/// out doesn't. `touched` comes from the action's journal (sorted, so the first error is the
/// same everywhere).
pub fn check_locks(state: &State, touched: &[(String, Option<u64>)], sender: &str, free_before: i128, height: u64) -> Result<()> {
    for (addr, old_balance) in touched {
        let Some(a) = state.accounts.get(addr) else { continue };
        if a.locks.is_empty() {
            continue;
        }
        let after = free(state, addr, height);
        let (ok, could) = if addr == sender { (after >= free_before.min(0), free_before) } else { (a.balance >= old_balance.unwrap_or(0) || after >= 0, 0) };
        if !ok {
            let reason = a.locks.iter().max_by_key(|l| l.amount.locked_at(height)).map(|l| l.reason.clone()).unwrap_or_default();
            bail!("{} of {}'s FANG is locked ({reason}): locked tokens can be staked but not moved out, and it can move {} now", fmt_fang(a.locked_at(height)), short(addr), fmt_fang(could.clamp(0, u64::MAX as i128) as u64));
        }
    }
    Ok(())
}

/// Burns `account_creation_fee` from the sender for every account the action opened (`touched`
/// entries that didn't exist before; keyless system accounts are free). A session key's spend
/// counts it.
pub fn charge_new_accounts(state: &mut State, j: &mut Journal, touched: &[(String, Option<u64>)], sender: &str, signer: &str, key: &KeyRecord) -> Result<()> {
    let prefix = state.params.prefix().to_string();
    let opened = touched.iter().filter(|(a, before)| before.is_none() && state.accounts.contains_key(a) && system_account(&prefix, a).is_none()).count() as u64;
    let fee = state.chain_params.account_creation_fee.checked_mul(opened).ok_or_else(|| anyhow!("account creation fee overflow"))?;
    if fee == 0 {
        return Ok(());
    }
    state.j_debit(j, sender, fee).map_err(|e| anyhow!("opening {opened} account{} burns {}: {e}", if opened == 1 { "" } else { "s" }, fmt_fang(fee)))?;
    state.j_burn(j, fee);
    if let KeyRole::Session { .. } = key.role {
        let mut keys = state.account(sender).keys;
        if let Some(rec) = keys.get_mut(signer) {
            rec.spent = rec.spent.saturating_add(fee);
        }
        state.acct_mut(j, sender).keys = keys;
    }
    Ok(())
}

// ---- names (DEC-11) ----

/// The height txs execute at next (expiry is checked against it).
pub fn now(state: &State) -> u64 {
    state.econ.height + 1
}

/// Whether a name still resolves.
pub fn name_active(state: &State, rec: &NameRecord) -> bool {
    now(state) <= rec.expires_at
}

/// The last height its owner can still renew it; anyone can register it after.
pub fn grace_until(state: &State, rec: &NameRecord) -> u64 {
    rec.expires_at.saturating_add(state.chain_params.name_grace_blocks)
}

/// `RegisterName`: a name nobody holds (or whose grace ran out), not reserved, for a year.
pub(crate) fn register_name(state: &mut State, j: &mut Journal, sender: &str, name: &str, height: u64) -> Result<()> {
    let n = normalize_name(name)?;
    if state.econ.reserved_names.contains_key(&n) {
        bail!("{n}{NAME_SUFFIX} is reserved");
    }
    if let Some(r) = state.names.get(&n) {
        if height <= r.expires_at {
            bail!("{n}{NAME_SUFFIX} is already taken");
        }
        let grace = grace_until(state, r);
        if height <= grace {
            bail!("{n}{NAME_SUFFIX} expired at block {}; its owner can renew it until block {grace}, then anyone can register it", r.expires_at);
        }
    }
    let price = state.chain_params.name_price(n.len());
    state.j_debit(j, sender, price)?;
    let treasury = state.treasury();
    state.j_credit(j, &treasury, price);
    let expires_at = height.saturating_add(state.chain_params.name_registration_blocks);
    state.insert_name(j, NameRecord { name: n, owner: sender.to_string(), registered_at: height, expires_at });
    Ok(())
}

/// Removes up to [`NAME_SWEEP`] lapsed names (past their grace at `height`), resuming where the
/// last sweep stopped and starting over at the end.
fn sweep_names(state: &mut State, height: u64) {
    use std::ops::Bound;
    let grace = state.chain_params.name_grace_blocks;
    let from = (Bound::Excluded(state.econ.name_cursor.clone()), Bound::Unbounded);
    let batch: Vec<(String, bool)> = state.names.range(from).take(NAME_SWEEP).map(|(n, r)| (n.clone(), r.expires_at.saturating_add(grace) < height)).collect();
    state.econ.name_cursor = if batch.len() < NAME_SWEEP { String::new() } else { batch.last().map(|(n, _)| n.clone()).unwrap_or_default() };
    for (n, lapsed) in batch {
        if lapsed {
            state.names.remove(&n);
        }
    }
}

/// Tags 45–47: RenewName, TransferName, ReleaseName.
pub(crate) fn apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, _signer: &str, key: &KeyRecord, height: u64) -> Result<()> {
    let owned = |state: &State, name: &str| -> Result<(String, NameRecord)> {
        if key.role != KeyRole::Owner {
            bail!("only owner keys can manage names");
        }
        let n = normalize_name(name)?;
        let rec = state.names.get(&n).ok_or_else(|| anyhow!("{n}{NAME_SUFFIX} isn't registered"))?;
        if rec.owner != sender {
            bail!("{n}{NAME_SUFFIX} belongs to {}", short(&rec.owner));
        }
        Ok((n, rec.clone()))
    };
    match &tx.action {
        Action::RenewName { name, years } => {
            let (n, rec) = owned(state, name)?;
            let grace = grace_until(state, &rec);
            if height > grace {
                bail!("{n}{NAME_SUFFIX} lapsed after block {grace}; register it again");
            }
            let cp = &state.chain_params;
            let year = cp.name_registration_blocks;
            let expires_at = rec.expires_at.checked_add(year.saturating_mul(*years as u64)).ok_or_else(|| anyhow!("renewal overflow"))?;
            let latest = height.saturating_add(year.saturating_mul(MAX_RENEW_YEARS as u64));
            if expires_at > latest {
                bail!("a name can be paid for at most {MAX_RENEW_YEARS} years ahead (until block {latest})");
            }
            let price = cp.name_renewal_price(n.len(), *years as u64);
            if price > 0 {
                state.j_debit(j, sender, price)?;
                let treasury = state.treasury();
                state.j_credit(j, &treasury, price);
            }
            state.name_mut(j, &n).expect("checked above").expires_at = expires_at;
        }
        Action::TransferName { name, to } => {
            let (n, rec) = owned(state, name)?;
            if height > rec.expires_at {
                bail!("{n}{NAME_SUFFIX} expired at block {}; renew it first", rec.expires_at);
            }
            let to = state.resolve(to)?;
            if to == sender {
                bail!("you already own {n}{NAME_SUFFIX}");
            }
            state.name_mut(j, &n).expect("checked above").owner = to;
        }
        Action::ReleaseName { name } => {
            let (n, _) = owned(state, name)?;
            state.remove_name(j, &n);
        }
        _ => bail!("not a names action"),
    }
    Ok(())
}

/// Economics' consistency rules (part of `State::check_invariants`): reserved names are
/// canonical and unregistered; the fee ring is whole and in bounds; names expire after they're
/// registered; every account's locks are well-formed, uniquely named and few enough.
pub fn check_invariants(state: &State) -> Result<()> {
    let e = &state.econ;
    for n in e.reserved_names.keys() {
        ensure!(normalize_name(n).ok().as_deref() == Some(n.as_str()), "reserved name {n} isn't canonical");
        ensure!(!state.names.contains_key(n), "{n}{NAME_SUFFIX} is both reserved and registered");
    }
    ensure!(e.fees.is_empty() || e.fees.len() == FEE_LAG as usize + 1, "the fee ring has {} slots, not {}", e.fees.len(), FEE_LAG + 1);
    for s in &e.fees {
        ensure!((1..=MAX_RATE).contains(&s.rate) && s.max_block_bytes <= crate::params::CODEC_MAX_BLOCK_BYTES, "fee slot {s:?} is out of bounds");
    }
    ensure!(e.issuance.year == 0 || e.issuance.rate_ppm > 0, "issuance year {} has no rate", e.issuance.year);
    for (n, r) in state.names.iter() {
        ensure!(r.expires_at >= r.registered_at, "{n}{NAME_SUFFIX} expires before it was registered");
    }
    for (addr, a) in state.accounts.iter() {
        ensure!(a.locks.len() <= MAX_LOCKS, "{addr} holds {} locks", a.locks.len());
        for (i, l) in a.locks.iter().enumerate() {
            ensure!(!a.locks[..i].iter().any(|o| o.id == l.id), "{addr} has two locks named {}", l.id);
            if let LockAmount::Vesting { start, cliff, end, .. } = l.amount {
                ensure!(start <= cliff && cliff <= end && start < end, "{addr}'s vesting lock isn't a curve");
            }
        }
    }
    Ok(())
}

/// Plain-English summary of a names tx (see `State::describe_with`; `primary_name` looks up
/// names).
pub fn describe(state: &State, tx: &Tx, from: &str, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    let name = |n: &str| format!("{}{NAME_SUFFIX}", normalize_name(n).unwrap_or_else(|_| n.to_string()));
    match &tx.action {
        Action::RenewName { name: n, years } => format!("{from} renewed {} for {years} year{}", name(n), if *years == 1 { "" } else { "s" }),
        Action::TransferName { name: n, to } => {
            let n = name(n);
            // Described after the fact too, when the name may already be the recipient's.
            let to = match state.resolve(to) {
                Ok(a) => primary_name(&a).filter(|p| *p != n).unwrap_or_else(|| short(&a)),
                Err(_) => to.clone(),
            };
            format!("{from} gave {n} to {to}")
        }
        Action::ReleaseName { name: n } => format!("{from} released {}", name(n)),
        _ => String::new(),
    }
}
