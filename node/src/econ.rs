//! Economics (P-ECON, MAINNET-DESIGN 3.6, 3.7, DEC-6, DEC-11): the base fee market and the block
//! size limit, issuance and the endowment stream, account locks (vesting and votes) and their
//! enforcement, the account creation fee, and names (prices, yearly registration, renewal, grace,
//! transfer, release, the reserved list).
//!
//! **Base fee.** A tx in block `h` is charged `base_fee(h, size) + tip`, at most its `fee`: the
//! base part (`flat + rate × size`, the rate in milli-drops per byte) is burned and the tip goes to
//! the proposer. The tip is at most `tip_cap_multiple` × the base fee ([`check_tip`]), and comes
//! from transferable FANG (see Locks). The fee for `h` is fixed when block `h − FEE_LAG` executes
//! (17 = `MAX_EXEC_LAG + 1`), so a proposer whose state is within the exec lag knows it exactly: the
//! state keeps a ring of [`FeeSlot`]s for the last executed height and the 17 after it. Each block
//! moves the rate toward `target_fullness_bps` of the block-size limit (a full block raises it by
//! `base_fee_max_change_bps`, an empty one lowers it as much), never below the floor
//! (`base_fee_floor_flat + base_fee_floor_per_byte × size`, today's `min_fee`). A parameter change
//! reaches the ring 17 blocks later, like everything else in it. The block-size limit
//! (`max_block_bytes`) and Ghost Mode's per-block limits (`ghost_max_tx_bytes`,
//! `ghost_max_block_bytes`) are fixed the same way, so a verifier that has executed `h − 17`
//! checks a block's size, its ghost txs' sizes and every tx's fee exactly (consensus.rs), and a
//! block with txs must cite such a state. Statelessly, a fee is only checked against the lowest base fee any parameters allow.
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
//! **Locks.** What an account's locks hold is what's still vesting plus the largest vote lock
//! (`Account::locked_at`): vote locks overlap each other, but votes only use vested tokens, so a
//! vote lock comes on top of the vesting lock (audit F03). Transferable balance = liquid −
//! max(0, locked − staked − unbonding), so locked tokens can be staked and delegated, never moved
//! out; a vesting account's stake covers its locks at cost (each delegation at most its
//! principal: gains not sold yet vest with the allocation, `staking::holdings_at_cost`). Every
//! tx's action phase is checked ([`check_locks`]): an account may not end it with less
//! transferable than zero (or less than it had, if fees already took it below), the sender's own
//! staking moves aside (`Journal::lock_slack`: they keep its tokens). The burned base fee may come
//! from locked tokens; the tip, a payment to the proposer, may not (`State::admit`). Staking
//! rewards credited to a vesting account join its lock ([`lock_rewards`]). Votes lock with
//! [`lock`]; [`voting_balance`] leaves unvested tokens out.
//!
//! **Accounts.** An action that opens an account (a transfer to a new address, a hunt awarded to
//! one…) burns `account_creation_fee` per account from the sender, on top of what it moves
//! (DEC-6). Transfers that open an account still carry at least `MIN_NEW_ACCOUNT`.
//!
//! **Names** (DEC-11). Registering costs `name_price_3/4/5` by length and lasts
//! `name_registration_blocks`; `RenewName` adds years at `name_renewal_bps` of the price each (at
//! most `MAX_RENEW_YEARS` paid ahead). An expired name doesn't resolve; for `name_grace_blocks`
//! only its owner can renew it, then anyone can register it. The grace a name was registered or
//! renewed with is kept in its record ([`grace_until`]): lowering the parameter only shortens the
//! grace of names registered or renewed after the change (a raise lengthens every grace). Lapsed records are swept a few per
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
pub use crate::chain::MAX_LOCKS;
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
    /// Ghost Mode's limits for the block (`ghost::BlockBudget`): most bytes per ghost tx, and of
    /// ghost txs together. Fixed ahead like the rest, so a proposer within the lag knows them.
    #[serde(default)]
    pub ghost_max_tx_bytes: u64,
    #[serde(default)]
    pub ghost_max_block_bytes: u64,
}

impl FeeSlot {
    /// The floor under these parameters.
    pub fn floor(cp: &ChainParams) -> Self {
        Self { flat: cp.base_fee_floor_flat, rate: floor_rate(cp), max_block_bytes: cp.max_block_bytes, ghost_max_tx_bytes: cp.ghost_max_tx_bytes, ghost_max_block_bytes: cp.ghost_max_block_bytes }
    }

    /// A slot that only prices txs (no size limits): for computing a fee from a flat part and a
    /// rate.
    pub fn pricing(flat: u64, rate: u64) -> Self {
        Self { flat, rate, max_block_bytes: 0, ghost_max_tx_bytes: 0, ghost_max_block_bytes: 0 }
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
/// heights [`slot`] knows; past them, the most the rate can reach by then under the current
/// parameters (the last fixed rate raised by a full block's step each block); before them, the
/// floor.
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
    let (mut rate, step) = (last.rate as u128, cp.base_fee_max_change_bps as u128);
    if step > 0 {
        // At most ~150 steps reach the cap from the floor.
        for _ in 0..(height - last_height).min(1_000) {
            if rate >= MAX_RATE as u128 {
                break;
            }
            rate += (rate * step / 10_000).max(1);
        }
    }
    FeeSlot::pricing(last.flat.max(cp.base_fee_floor_flat), rate.min(MAX_RATE as u128) as u64).fee(size)
}

/// What `tx` is charged in block `height`: the base fee plus its tip (`None` on overflow).
pub fn charge(state: &State, tx: &Tx, height: u64) -> Option<u64> {
    base_fee(state, height, tx.size()).checked_add(tx.tip)
}

/// The most a tx whose base fee is `base` may tip: `tip_cap_multiple` × `base`. Priority bidding
/// works within it; one signature can't hand the proposer a balance (audit F01).
pub fn max_tip(state: &State, base: u64) -> u64 {
    (base as u128 * state.chain_params.tip_cap_multiple as u128).min(u64::MAX as u128) as u64
}

/// The tip cap ([`max_tip`]) for a tx whose base fee in block `height` is `base`.
pub fn check_tip(state: &State, tip: u64, base: u64, height: u64) -> Result<()> {
    let cap = max_tip(state, base);
    ensure!(tip <= cap, "tip too high: at most {}× the base fee, {cap} drops at block {height} (base fee {base}), but it tips {tip}", state.chain_params.tip_cap_multiple);
    Ok(())
}

/// The exact fee check (admission and execution): the charge, if `fee` covers it and the tip is
/// within its cap ([`check_tip`]).
pub fn check_fee(state: &State, tx: &Tx, height: u64) -> Result<u64> {
    let base = base_fee(state, height, tx.size());
    check_tip(state, tx.tip, base, height)?;
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
        Some(a) => FeeSlot {
            flat: a.flat.max(s.flat),
            rate: a.rate.max(s.rate),
            max_block_bytes: a.max_block_bytes.min(s.max_block_bytes),
            ghost_max_tx_bytes: a.ghost_max_tx_bytes.min(s.ghost_max_tx_bytes),
            ghost_max_block_bytes: a.ghost_max_block_bytes.min(s.ghost_max_block_bytes),
        },
    }))
    .unwrap_or_else(|| FeeSlot::floor(&state.chain_params))
}

/// The lowest flat part and rate among the slots fixed after the last executed height (`/api/fees`
/// serves it as `lowest`): a tip within `tip_cap_multiple` × the base fee at it fits the cap
/// ([`check_tip`]) in any of those blocks.
pub fn lowest(state: &State) -> FeeSlot {
    let upcoming = state.econ.fees.iter().skip(1);
    upcoming.fold(None, |acc: Option<FeeSlot>, s| Some(match acc {
        None => *s,
        Some(a) => FeeSlot { flat: a.flat.min(s.flat), rate: a.rate.min(s.rate), ..a },
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
    FeeSlot { rate: next.clamp(floor_rate(cp) as u128, MAX_RATE as u128) as u64, ..FeeSlot::floor(cp) }
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
    if iss.year < year {
        // The rate of year `iss.year + 1`, then one decay per year skipped (none on a live chain,
        // which issues every epoch) until it stops moving.
        let mut rate = if iss.year == 0 { cp.issuance_initial_ppm } else { next_rate(iss.rate_ppm, &cp) };
        for _ in iss.year + 1..year {
            let next = next_rate(rate, &cp);
            if next == rate {
                break;
            }
            rate = next;
        }
        *iss = Issuance { year, base_supply: supply, rate_ppm: rate };
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

/// What `account` can move out at `height`: liquid − max(0, locked − staked − unbonding), where
/// locked is what's still vesting plus the largest vote lock (`Account::locked_at`).
pub fn transferable(state: &State, account: &str, height: u64) -> u64 {
    free(state, account, height).clamp(0, u64::MAX as i128) as u64
}

/// [`transferable`] before clamping: negative when fees (or a slash) took locked tokens. An
/// account that's still vesting counts its stake at cost (`staking::holdings_at_cost`): what
/// staking earned on locked tokens vests with them, so it can't free liquid tokens before it
/// does (3.7).
pub fn free(state: &State, account: &str, height: u64) -> i128 {
    let Some(a) = state.accounts.get(account) else { return 0 };
    let locked = a.locked_at(height);
    if locked == 0 {
        return a.balance as i128;
    }
    let (staked, unbonding) = if a.unvested_at(height) > 0 { crate::staking::holdings_at_cost(state, account) } else { crate::staking::holdings(state, account) };
    a.balance as i128 - (locked as i128 - staked as i128 - unbonding as i128).max(0)
}

/// What `account` may vote with at `height`: its holdings (liquid + staked + unbonding) minus
/// what its vesting locks still hold. Vote locks don't count: one stake votes on many proposals.
/// An account that's still vesting counts its stake at cost, as [`free`] does: what its locked
/// stake earned and hasn't sold yet vests with it, so it doesn't vote before it vests (audit
/// xcut-economic-1).
pub fn voting_balance(state: &State, account: &str, height: u64) -> u64 {
    let Some(a) = state.accounts.get(account) else { return 0 };
    let unvested = a.unvested_at(height);
    let (staked, unbonding) = if unvested > 0 { crate::staking::holdings_at_cost(state, account) } else { crate::staking::holdings(state, account) };
    let held = a.balance as u128 + staked as u128 + unbonding as u128;
    held.saturating_sub(unvested as u128).min(u64::MAX as u128) as u64
}

/// Locks `amount` of `account`'s vested holdings ([`voting_balance`]) until (not including)
/// height `until` under `id` (e.g. `"vote:<proposal>"`). Vote locks overlap each other (the
/// largest applies) and come on top of what's still vesting. Locking an id again only extends it
/// (the larger amount, the later end), so a lock never shrinks before it expires; only the part
/// that grows it must be held now, so a re-vote never fails over what the lock already holds,
/// even after fees took the holdings below it (audit F42).
pub fn lock(state: &mut State, j: &mut Journal, account: &str, id: String, amount: u64, until: u64, height: u64) -> Result<()> {
    ensure!(!id.is_empty() && id.len() <= MAX_LABEL && id != VESTING_LOCK, "lock ids are 1–{MAX_LABEL} bytes (not “{VESTING_LOCK}”)");
    let a = state.accounts.get(account).ok_or_else(|| anyhow!("{} has no account", short(account)))?;
    let existing = a.locks.iter().position(|l| l.id == id);
    let holds = existing.map_or(0, |i| match a.locks[i].amount {
        LockAmount::Until { .. } => a.locks[i].amount.locked_at(height),
        LockAmount::Vesting { .. } => 0,
    });
    if amount > holds {
        let can = voting_balance(state, account, height);
        ensure!(amount <= can, "can't lock {}: {} holds {} it could lock (vested liquid, staked and unbonding FANG)", fmt_fang(amount), short(account), fmt_fang(can));
    }
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
pub fn lock_rewards(state: &mut State, j: &mut Journal, account: &str, amount: u64, height: u64) {
    let vesting = |l: &Lock| matches!(l.amount, LockAmount::Vesting { end, .. } if height < end);
    if amount == 0 || !state.accounts.get(account).is_some_and(|a| a.locks.iter().any(vesting)) {
        return;
    }
    for l in state.acct_mut(j, account).locks.iter_mut() {
        if let LockAmount::Vesting { total, end, .. } = &mut l.amount
            && height < *end
        {
            *total = total.saturating_add(amount);
        }
    }
}

/// After a tx's action: no account it touched may hold less transferable than zero, and the
/// sender none less than it had (`free_before`, which fees may have taken below zero). Moving
/// liquid tokens into stake keeps the lock covered, so staking locked tokens passes; moving them
/// out doesn't. `slack` is how far the sender's own staking moves took its transferable balance
/// down (`Journal::lock_slack`): they keep its tokens (short of a pool's rounding dust), so they
/// pass even when every token is locked. `touched` comes from the action's journal (sorted, so
/// the first error is the same everywhere).
pub fn check_locks(state: &State, touched: &[(String, Option<u64>)], sender: &str, free_before: i128, slack: u64, height: u64) -> Result<()> {
    for (addr, old_balance) in touched {
        let Some(a) = state.accounts.get(addr) else { continue };
        if a.locks.is_empty() {
            continue;
        }
        let after = free(state, addr, height);
        let (ok, could) = if addr == sender { (after + slack as i128 >= free_before.min(0), free_before) } else { (a.balance >= old_balance.unwrap_or(0) || after >= 0, 0) };
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

/// The last height its owner can still renew it; anyone can register it after: `expires_at`
/// plus the longer of the grace it was registered (or last renewed) with and today's
/// `name_grace_blocks`, so lowering the parameter never takes the grace of a name already
/// counting on it (gap-6-3).
pub fn grace_until(state: &State, rec: &NameRecord) -> u64 {
    rec.expires_at.saturating_add(rec.grace_blocks.max(state.chain_params.name_grace_blocks))
}

/// The name `tx` asks to register (normalized), for the mempool's first-to-ask hold
/// (`Chain::first_to_ask`, audit F35): a `RegisterName`, one wrapped in a `MultisigPropose`, or
/// a `MultisigApprove` of the sender's stored proposal to register one.
pub fn name_asked_for(state: &State, tx: &Tx) -> Option<String> {
    let action = match &tx.action {
        Action::MultisigPropose { action } => action.as_ref(),
        Action::MultisigApprove { id, .. } => &state.governance.multisig_proposals.get(id).filter(|p| p.account == tx.from)?.action,
        a => a,
    };
    match action {
        Action::RegisterName { name } => normalize_name(name).ok(),
        _ => None,
    }
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
    let grace_blocks = state.chain_params.name_grace_blocks;
    state.insert_name(j, NameRecord { name: n, owner: sender.to_string(), registered_at: height, expires_at, offered_to: None, grace_blocks });
    Ok(())
}

/// Removes up to [`NAME_SWEEP`] lapsed names (past their grace at `height`), resuming where the
/// last sweep stopped and starting over at the end.
fn sweep_names(state: &mut State, height: u64) {
    use std::ops::Bound;
    let from = (Bound::Excluded(state.econ.name_cursor.clone()), Bound::Unbounded);
    let batch: Vec<(String, bool)> = state.names.range(from).take(NAME_SWEEP).map(|(n, r)| (n.clone(), grace_until(state, r) < height)).collect();
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
            // The new term's grace is today's.
            let grace_blocks = state.chain_params.name_grace_blocks;
            let rec = state.name_mut(j, &n).expect("checked above");
            (rec.expires_at, rec.grace_blocks) = (expires_at, grace_blocks);
        }
        // A name changes hands in two steps (audit F34): its owner offers it (`to` another
        // account; offering it again replaces the offer, `to` itself withdraws it), and the account
        // it was offered to accepts it (`to` itself). Until then it stays the owner's, so nobody
        // can make a name, and so a primary name, someone else's. Accepting re-stamps
        // `registered_at`, so a name an account accepted never ranks ahead of the ones it
        // registered itself.
        Action::TransferName { name, to } => {
            let n = normalize_name(name)?;
            let to = state.resolve(to);
            if let Some(rec) = state.names.get(&n).filter(|r| r.owner != sender && r.offered_to.as_deref() == Some(sender) && to.as_deref().ok() == Some(sender)) {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can manage names");
                }
                if height > rec.expires_at {
                    bail!("{n}{NAME_SUFFIX} expired at block {}; only its owner can renew it, so it can't be accepted", rec.expires_at);
                }
                let r = state.name_mut(j, &n).expect("checked above");
                (r.owner, r.registered_at, r.offered_to) = (sender.to_string(), height, None);
                return Ok(());
            }
            let (n, rec) = owned(state, name)?;
            if height > rec.expires_at {
                bail!("{n}{NAME_SUFFIX} expired at block {}; renew it first", rec.expires_at);
            }
            let to = to?;
            if to == sender && rec.offered_to.is_none() {
                bail!("you already own {n}{NAME_SUFFIX}");
            }
            state.name_mut(j, &n).expect("checked above").offered_to = (to != sender).then_some(to);
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
        ensure!(s.ghost_max_tx_bytes <= s.ghost_max_block_bytes && s.ghost_max_block_bytes <= crate::params::CODEC_MAX_BLOCK_BYTES, "fee slot {s:?} has out-of-bounds ghost limits");
    }
    ensure!(e.issuance.year == 0 || e.issuance.rate_ppm > 0, "issuance year {} has no rate", e.issuance.year);
    for (n, r) in state.names.iter() {
        ensure!(r.expires_at >= r.registered_at, "{n}{NAME_SUFFIX} expires before it was registered");
        ensure!(r.offered_to.as_ref().is_none_or(|o| *o != r.owner && crate::crypto::is_address(o)), "{n}{NAME_SUFFIX} is offered to its owner or to no account");
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

/// Plain-English summary of a names tx (see `State::describe_with`; `height` is the block it's
/// in, if it's in one; `primary_name` looks up names).
pub fn describe(state: &State, tx: &Tx, from: &str, height: Option<u64>, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    let name = |n: &str| format!("{}{NAME_SUFFIX}", normalize_name(n).unwrap_or_else(|_| n.to_string()));
    match &tx.action {
        Action::RenewName { name: n, years } => format!("{from} renewed {} for {years} year{}", name(n), if *years == 1 { "" } else { "s" }),
        Action::TransferName { name: bare, to } => {
            let n = name(bare);
            match (state.resolve(to), tx.sender().ok()) {
                // To itself: accepting an offer, or withdrawing its own. Described after the fact
                // too: an accepted name is re-stamped with the height it changed hands at.
                (Ok(a), Some(s)) if a == s => {
                    let rec = normalize_name(bare).ok().and_then(|b| state.names.get(&b));
                    if rec.is_some_and(|r| r.offered_to.as_deref() == Some(s.as_str()) || (r.owner == s && height.is_some_and(|h| h == r.registered_at))) {
                        format!("{from} accepted {n}")
                    } else {
                        format!("{from} withdrew its offer of {n}")
                    }
                }
                (Ok(a), _) => format!("{from} offered {n} to {}", primary_name(&a).filter(|p| *p != n).unwrap_or_else(|| short(&a))),
                (Err(_), _) => format!("{from} offered {n} to {to}"),
            }
        }
        Action::ReleaseName { name: n } => format!("{from} released {}", name(n)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{community, make_pop, now_ms, write_secret, Account, Allocation, Chain, Genesis, ValidatorInfo, Vesting};
    use crate::crypto::Keys;
    use std::path::PathBuf;

    const FANG: u64 = DROPS_PER_FANG;
    const VALIDATOR: [u8; 32] = [41; 32];
    const CHAIN_ID: &str = "econ-test";

    fn keys(seed: u8) -> Keys {
        Keys::from_seed([seed; 32])
    }

    fn alloc(k: &Keys, amount: u64, vesting: Option<(u64, u64, u64)>, label: &str) -> Allocation {
        Allocation { address: k.address(), amount, vesting: vesting.map(|(start, cliff, end)| Vesting { start, cliff, end }), label: Some(label.into()) }
    }

    /// A one-validator chain (the validator holds 1M FANG and collects the tips) with these
    /// allocations and parameter overrides.
    struct T {
        home: PathBuf,
        c: Chain,
    }

    fn chain(tag: &str, mut allocations: Vec<Allocation>, params: &[(&str, u64)], epoch_blocks: u64) -> T {
        let v = Keys::from_seed(VALIDATOR);
        allocations.push(Allocation { address: v.address(), amount: 1_000_000 * FANG, vesting: None, label: None });
        let g = Genesis {
            chain_id: CHAIN_ID.into(),
            token: "FANG".into(),
            decimals: DECIMALS,
            block_time_ms: 1000,
            validators: vec![ValidatorInfo { address: v.address(), public_key: hex::encode(v.public_key()), p2p_addr: "127.0.0.1:1".into(), http_addr: "127.0.0.1:2".into() }],
            faucet: false,
            created_at_ms: 1,
            allocations,
            epoch_blocks,
            validator_admins: vec![],
            contribution: None,
            address_prefix: "boo".into(),
            param_overrides: params.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            operators: vec![],
            reserved_names: vec!["reserved".into()],
            soul_ledger: vec![],
            features: vec![],
            bootstrap_excluded: vec![],
            pool_repos: Default::default(),
        };
        let home = std::env::temp_dir().join(format!("blockula-econ-{tag}-{}-{}", now_ms(), std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(&g).unwrap()).unwrap();
        write_secret(&home.join("validator.key"), &hex::encode(VALIDATOR)).unwrap();
        let c = Chain::open(&home).unwrap();
        T { home, c }
    }

    impl T {
        /// `k`'s tx at its next nonce, unsigned (execution doesn't check signatures), paying the
        /// highest base fee fixed for the next blocks.
        fn unsigned(&self, k: &Keys, action: Action) -> Tx {
            let from = k.address();
            let mut tx = Tx { chain_id: CHAIN_ID.into(), genesis_hash: self.c.genesis_hash.clone(), nonce: self.c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            tx.fee = max_fee(&self.c.state, tx.size());
            tx
        }

        fn tx(&self, k: &Keys, action: Action) -> Tx {
            self.unsigned(k, action).sign(k)
        }

        /// Submits and executes `action` from `k`.
        fn ok(&mut self, k: &Keys, action: Action) -> Tx {
            let tx = self.tx(k, action);
            self.c.submit(tx.clone()).unwrap_or_else(|e| panic!("{:?}: {e}", tx.action));
            self.c.produce_block().unwrap();
            tx
        }

        /// What admitting `action` from `k` fails with.
        fn err(&mut self, k: &Keys, action: Action) -> String {
            let tx = self.tx(k, action);
            self.c.submit(tx.clone()).map(|_| panic!("should be refused: {:?}", tx.action)).unwrap_err().to_string()
        }

        fn bal(&self, a: &str) -> u64 {
            self.c.state.account(a).balance
        }

        fn blocks(&mut self, n: u64) {
            for _ in 0..n {
                self.c.produce_block_with(vec![]).unwrap();
            }
        }

        fn now(&self) -> u64 {
            now(&self.c.state)
        }

        fn done(self) {
            std::fs::remove_dir_all(&self.home).ok();
        }

        /// Restarting from the store, and rebuilding from the logs, reach the same state.
        fn replays(self) {
            let T { home, c } = self;
            let root = c.state.root();
            drop(c);
            assert_eq!(Chain::open(&home).unwrap().state.root(), root, "restart");
            std::fs::remove_file(home.join(crate::store::DB_FILE)).unwrap();
            assert_eq!(Chain::open(&home).unwrap().state.root(), root, "rebuild from the logs");
            std::fs::remove_dir_all(home).ok();
        }
    }

    fn info(height: u64, tx_bytes: u64) -> BlockInfo<'static> {
        BlockInfo { height, hash: "", proposer: "", view: height, parent_view: height - 1, skipped: &[], tx_bytes }
    }

    /// One block's step: a full block raises the rate by the whole step, an empty one lowers it
    /// as much, linearly around the target; never below the floor or above the cap; the next
    /// slot takes the current flat part, floor and size limit.
    #[test]
    fn fee_steps_follow_fullness() {
        let cp = ChainParams::default();
        let max = cp.max_block_bytes;
        assert_eq!((cp.base_fee_max_change_bps, cp.target_fullness_bps, max), (1_250, 5_000, 2 << 20));
        let floor = FeeSlot::floor(&cp);
        assert_eq!(floor, FeeSlot { flat: 10_000, rate: 10_000, max_block_bytes: max, ghost_max_tx_bytes: cp.ghost_max_tx_bytes, ghost_max_block_bytes: cp.ghost_max_block_bytes });
        assert_eq!(floor.fee(5_400), min_fee(5_400), "the floor is today's min_fee");
        let at = |rate: u64, used: u64| next_slot(&FeeSlot { rate, ..floor }, used, max, &cp).rate;
        assert_eq!(at(100_000, max), 112_500, "full: +12.5%");
        assert_eq!(at(100_000, 0), 87_500, "empty: −12.5%");
        assert_eq!(at(100_000, max / 2), 100_000, "at the target");
        assert_eq!(at(100_000, max / 4 * 3), 106_250);
        assert_eq!(at(100_000, max / 4), 93_750);
        assert_eq!(at(100_000, 10 * max), 112_500, "more than the limit counts as full");
        assert_eq!(at(10_000, 0), 10_000, "never below the floor");
        assert_eq!(at(10_000, max), 11_250);
        assert_eq!(at(MAX_RATE, max), MAX_RATE, "capped");
        assert_eq!(next_slot(&FeeSlot { rate: MAX_RATE, ..floor }, max, max, &cp).fee(MAX_TX_BYTES as u64), 10_000 + (MAX_RATE as u128 * MAX_TX_BYTES as u128 / 1_000) as u64, "the largest fee fits");
        // Fullness is measured against the limit the block was built under.
        assert_eq!(next_slot(&FeeSlot { rate: 100_000, ..floor }, max / 2, max / 2, &cp).rate, 112_500);
        // A zero step freezes the rate; a 100% target only ever lowers it.
        let frozen = ChainParams { base_fee_max_change_bps: 0, ..cp.clone() };
        assert_eq!((next_slot(&FeeSlot { rate: 50_000, ..floor }, max, max, &frozen).rate, next_slot(&FeeSlot { rate: 50_000, ..floor }, 0, max, &frozen).rate), (50_000, 50_000));
        let full_target = ChainParams { target_fullness_bps: 10_000, ..cp.clone() };
        assert_eq!((next_slot(&FeeSlot { rate: 50_000, ..floor }, max, max, &full_target).rate, next_slot(&FeeSlot { rate: 50_000, ..floor }, 0, max, &full_target).rate), (50_000, 43_750));
        // New parameters reach the next slot: a higher floor lifts the rate to it at once, and
        // the flat part and size limit are the current ones.
        let raised = ChainParams { base_fee_floor_per_byte: 20, base_fee_floor_flat: 7, max_block_bytes: 1 << 20, ..cp.clone() };
        assert_eq!(next_slot(&floor, 0, max, &raised), FeeSlot { flat: 7, rate: 20_000, max_block_bytes: 1 << 20, ..floor });
        // Fees round down, per tx.
        assert_eq!(FeeSlot::pricing(0, 10_999).fee(1), 10);
        assert_eq!(lowest_base_fee(5_400), 5_400, "the bounds allow 0 flat and 1 drop per byte");
    }

    /// The same vector is computed by sdk-ts (test/e2e.ts `baseFee`): the shared transfer vector at
    /// 12,656 milli-drops a byte.
    #[test]
    fn fee_slots_match_the_sdk() {
        let tx = crate::types::tests::vector_tx(Action::Transfer { to: "x.boo".into(), amount: 5 });
        assert_eq!(tx.size(), 5_373);
        assert_eq!(FeeSlot::pricing(10_000, 12_656).fee(tx.size()), 78_000);
    }

    /// Ghost Mode's per-block limits ride in the same slots: a `ParamChange` that executes at the
    /// end of block `h` reaches the slot block `h + 1` fixes (for `h + 1 + FEE_LAG`), so every
    /// height a proposer within the lag may build on keeps the limits it already knew, and
    /// `ghost::BlockBudget` reads them from the slot, not from the parameters in force.
    #[test]
    fn ghost_limits_are_fixed_17_blocks_ahead() {
        let mut st = State::default();
        init_genesis(&mut st);
        let old = FeeSlot::floor(&st.chain_params);
        let h = 5;
        for height in 1..=h {
            end_block(&mut st, &info(height, 0));
        }
        // Executed at the end of block h, after its own econ step (governance runs after it).
        st.exec_set_param(&mut Journal::default(), "ghost_max_tx_bytes", 50 << 10).unwrap();
        st.exec_set_param(&mut Journal::default(), "ghost_max_block_bytes", 100 << 10).unwrap();
        let limits = |st: &State, height: u64| slot(st, height).map(|s| (s.ghost_max_tx_bytes, s.ghost_max_block_bytes));
        assert!((h..=h + FEE_LAG).all(|x| limits(&st, x) == Some((old.ghost_max_tx_bytes, old.ghost_max_block_bytes))));
        let tx = crate::types::tests::vector_tx(Action::PrivateTransfer { bundle: crate::types::tests::vector_bundle(11) });
        let budget = |st: &State, height: u64| crate::ghost::BlockBudget::new(slot(st, height).as_ref()).take(&tx, 60 << 10).is_ok();
        assert!(budget(&st, h + FEE_LAG), "a 60 KiB ghost tx still fits the slot fixed before the change");
        end_block(&mut st, &info(h + 1, 0));
        assert_eq!(limits(&st, h + FEE_LAG), Some((old.ghost_max_tx_bytes, old.ghost_max_block_bytes)));
        assert_eq!(limits(&st, h + 1 + FEE_LAG), Some((50 << 10, 100 << 10)));
        assert!(budget(&st, h + FEE_LAG) && !budget(&st, h + 1 + FEE_LAG));
        check_invariants(&st).unwrap();
    }

    /// The fee for block `h` is fixed by block `h − 17`'s execution, from the rate before it and
    /// that block's fullness: the ring always holds the last executed height and the 17 after.
    #[test]
    fn base_fee_is_fixed_17_blocks_ahead() {
        let mut st = State::default();
        init_genesis(&mut st);
        let cp = st.chain_params.clone();
        let (max, floor) = (cp.max_block_bytes, FeeSlot::floor(&cp));
        assert_eq!(st.econ.fees.len(), 18);
        assert!((0..=17).all(|h| slot(&st, h) == Some(floor)) && slot(&st, 18).is_none());
        // Five full blocks: each raises the rate 17 blocks later, compounding.
        for h in 1..=5 {
            end_block(&mut st, &info(h, max));
            assert_eq!(st.econ.height, h);
            assert!(slot(&st, h - 1).is_none() && slot(&st, h + FEE_LAG).is_some() && slot(&st, h + FEE_LAG + 1).is_none());
        }
        let rates: Vec<u64> = (5..=22).map(|h| slot(&st, h).unwrap().rate).collect();
        assert_eq!(&rates[..13], &[10_000; 13], "heights 5–17 were fixed at genesis");
        assert_eq!(&rates[13..], &[11_250, 12_656, 14_238, 16_017, 18_019]);
        assert_eq!(base_fee(&st, 18, 5_400), 10_000 + 11_250 * 5_400 / 1_000);
        assert_eq!(base_fee(&st, 17, 5_400), min_fee(5_400));
        // Before the ring: the floor. Past it: the most the rate can reach by then, which bounds
        // whatever actually happens.
        assert_eq!(base_fee(&st, 2, 5_400), min_fee(5_400));
        let bound: Vec<u64> = (23..=30).map(|h| base_fee(&st, h, 5_400)).collect();
        assert_eq!(bound[0], 10_000 + (18_019 + 18_019 * 1_250 / 10_000) * 5_400 / 1_000);
        let mut later = st.clone();
        for h in 6..=13 {
            end_block(&mut later, &info(h, max));
        }
        assert_eq!((23..=30).map(|h| base_fee(&later, h, 5_400)).collect::<Vec<_>>(), bound, "full blocks reach the bound exactly");
        // What wallets pay to cover any of the next 17 blocks.
        assert_eq!(suggested(&st).rate, 18_019);
        assert_eq!(max_fee(&st, 5_400), 10_000 + 18_019 * 5_400 / 1_000);
        // Two histories that agree through block 10 agree on every fee through height 27, and
        // differ from 28 on.
        let (mut a, mut b) = (st.clone(), st.clone());
        for h in 6..=10 {
            end_block(&mut a, &info(h, h * 1_000));
            end_block(&mut b, &info(h, h * 1_000));
        }
        end_block(&mut a, &info(11, max));
        end_block(&mut b, &info(11, 0));
        assert!((11..=27).all(|h| slot(&a, h) == slot(&b, h)));
        assert!(slot(&a, 28).unwrap().rate > slot(&b, 28).unwrap().rate);
        // Empty blocks bring it back to the floor, and no lower.
        for h in 12..=200 {
            end_block(&mut a, &info(h, 0));
        }
        assert!(a.econ.fees.iter().all(|s| *s == floor));
        check_invariants(&a).unwrap();
        // A state that didn't start from a genesis (unit tests) starts its ring at the first block.
        let mut fresh = State::default();
        assert_eq!(base_fee(&fresh, 7, 100), FeeSlot::floor(&cp).fee(100));
        end_block(&mut fresh, &info(7, max));
        assert_eq!((fresh.econ.height, slot(&fresh, 24).unwrap().rate), (7, 11_250));
    }

    /// On a chain: txs fill blocks (the limit is lowered to 64 KiB), the base fee climbs 17
    /// blocks later exactly as computed, txs are charged and burn exactly their block's base fee,
    /// a tx priced at the old floor is refused, blocks record what they charged, and it all
    /// replays.
    #[test]
    fn full_blocks_raise_the_base_fee() {
        let senders: Vec<Keys> = (1..=11).map(keys).collect();
        let allocations = senders.iter().map(|k| alloc(k, 1_000 * FANG, None, "user")).collect();
        let mut t = chain("fullness", allocations, &[("max_block_bytes", MAX_TX_BYTES as u64)], 600);
        let me = t.c.me().unwrap();
        let limit = MAX_TX_BYTES as u64;
        for _ in 0..20 {
            let h = t.c.height() + 1;
            let txs: Vec<Tx> = senders.iter().map(|k| t.unsigned(k, Action::Transfer { to: me.clone(), amount: FANG })).collect();
            let used: u64 = txs.iter().map(Tx::size).sum();
            assert!(used > limit / 2 && used <= limit);
            let (expected, this) = (next_slot(t.c.state.econ.fees.last().unwrap(), used, limit, &t.c.state.chain_params), slot(&t.c.state, h).unwrap());
            let charged: u64 = txs.iter().map(|tx| this.fee(tx.size())).sum();
            let burned = t.c.state.burned;
            t.c.produce_block_with(txs).unwrap();
            assert_eq!(t.c.state.burned - burned, charged, "block {h} burned its base fees");
            assert_eq!(slot(&t.c.state, h + FEE_LAG), Some(expected));
            assert_eq!(t.c.block(h).unwrap().base_fee, Some(this));
        }
        let next = t.now();
        let rate = slot(&t.c.state, next).unwrap().rate;
        assert!(rate > 10_000, "busy blocks 17 back raised the fee: {rate}");
        // Priced at the floor, a tx is refused (and skipped if a proposer includes it anyway);
        // priced at `max_fee` it goes through and pays the block's base fee.
        let mut cheap = t.unsigned(&senders[0], Action::Transfer { to: me.clone(), amount: FANG });
        cheap.fee = min_fee(cheap.size());
        let cheap = cheap.sign(&senders[0]);
        assert!(t.c.submit(cheap.clone()).unwrap_err().to_string().contains("fee too low"));
        t.c.produce_block_with(vec![cheap.clone()]).unwrap();
        assert!(!t.c.has_tx(&cheap.id()) && t.c.last_block().unwrap().skipped == vec![cheap.size()]);
        let before = t.bal(&senders[0].address());
        let tx = t.ok(&senders[0], Action::Transfer { to: me.clone(), amount: FANG });
        let h = t.c.height();
        assert_eq!(before - t.bal(&senders[0].address()), FANG + slot(&t.c.state, h).unwrap().fee(tx.size()));
        // Empty blocks bring it back down to the floor.
        t.blocks(80);
        assert_eq!(max_fee(&t.c.state, 5_400), min_fee(5_400));
        t.replays();
    }

    /// Issuance follows docs/TOKENOMICS.md's table exactly for ten years: the year's rate on
    /// the supply at its first epoch, 8,766 mints a year at 1 s blocks and 1-hour epochs, the
    /// rate decaying 15% a year to its 1.5% floor; and the split.
    #[test]
    fn issuance_matches_the_tokenomics_table() {
        // (rate ppm, mint per epoch, supply at year end), in drops.
        let table: [(u64, u64, u64); 10] = [
            (60_000, 6_844_626_967_830, 1_059_999_999_999_997_780),
            (51_000, 6_167_008_898_015, 1_114_059_999_999_997_270),
            (43_350, 5_509_297_399_041, 1_162_354_500_999_990_676),
            (36_847, 4_885_840_326_071, 1_205_183_777_298_329_062),
            (31_319, 4_305_857_942_186, 1_242_928_928_019_531_538),
            (26_621, 3_774_584_872_553, 1_276_016_939_012_331_136),
            (22_627, 3_293_684_152_296, 1_304_889_374_291_357_872),
            (19_232, 2_862_837_376_953, 1_329_985_006_737_727_870),
            (16_347, 2_480_180_801_407, 1_351_726_271_642_861_632),
            (15_000, 2_313_015_523_002, 1_372_002_165_717_497_164),
        ];
        let mut st = State { params: Params { block_time_ms: 1_000, epoch_blocks: 3_600, ..Default::default() }, ..Default::default() };
        st.accounts.insert(keys(1).address(), Account { balance: MAX_SUPPLY_DROPS, ..Default::default() });
        st.supply = MAX_SUPPLY_DROPS;
        let epy = epochs_per_year(&st.params);
        assert_eq!(epy, 8_766);
        let (treasury, pool) = (st.treasury(), st.pool());
        for (y, (rate, per_epoch, supply_at_end)) in table.into_iter().enumerate() {
            for e in 0..epy {
                let before = st.supply;
                issue(&mut st, y as u64 * epy + e);
                assert_eq!(st.supply - before, per_epoch, "year {} epoch {e}", y + 1);
                if y == 0 && e == 0 {
                    // Year 1, per epoch: 70% to validators' pools (none yet: the treasury), 20%
                    // to the contribution pool, 10% (and the rounding) to the treasury.
                    assert_eq!(st.account(&pool).balance, 1_368_925_393_566);
                    assert_eq!(st.account(&treasury).balance, 4_791_238_877_481 + 684_462_696_783);
                }
            }
            assert_eq!((st.econ.issuance.year, st.econ.issuance.rate_ppm), (y as u64 + 1, rate));
            assert_eq!(st.supply, supply_at_end, "supply at the end of year {}", y + 1);
        }
        st.check_invariants().unwrap();
        assert_eq!(next_rate(15_000, &st.chain_params), 15_000, "the floor holds");
        // The year's base is its supply when its first epoch ends (after burns), before the mint.
        let mut b = State { params: st.params.clone(), ..Default::default() };
        b.accounts.insert(keys(1).address(), Account { balance: MAX_SUPPLY_DROPS, ..Default::default() });
        b.supply = MAX_SUPPLY_DROPS;
        issue(&mut b, 0);
        let mut j = Journal::default();
        b.j_debit(&mut j, &keys(1).address(), 1_000 * FANG).unwrap();
        b.j_burn(&mut j, 1_000 * FANG);
        let at_year_two = b.supply;
        issue(&mut b, epy);
        assert_eq!(b.econ.issuance.base_supply, at_year_two);
        // A state that starts issuing late (unit tests) gets that year's rate; far-off years and
        // heights cost nothing to compute.
        let mut late = State { params: st.params.clone(), ..Default::default() };
        issue(&mut late, 3 * epy);
        assert_eq!((late.econ.issuance.year, late.econ.issuance.rate_ppm), (4, 36_847));
        issue(&mut late, 1_000 * epy);
        assert_eq!(late.econ.issuance.rate_ppm, 15_000);
        let mut frozen = State::default();
        init_genesis(&mut frozen);
        frozen.chain_params.base_fee_max_change_bps = 0;
        assert_eq!(base_fee(&frozen, u64::MAX, 100), FeeSlot::floor(&frozen.chain_params).fee(100));
        frozen.chain_params.base_fee_max_change_bps = 1_250;
        assert_eq!(base_fee(&frozen, u64::MAX, 1_000), 10_000 + MAX_RATE, "capped");
        // Governance moves the curve from the next year on, within bounds: no decay, or a higher floor.
        let cp = ChainParams { issuance_decay_ppm: 0, ..ChainParams::default() };
        assert_eq!(next_rate(43_350, &cp), 43_350);
        let cp = ChainParams { issuance_floor_ppm: 50_000, ..ChainParams::default() };
        assert_eq!(next_rate(43_350, &cp), 50_000);
        // Other chain shapes.
        assert_eq!(epochs_per_year(&Params { block_time_ms: 1_000, epoch_blocks: 600, ..Default::default() }), 52_596);
        assert_eq!(epochs_per_year(&Params { block_time_ms: 250, epoch_blocks: 3_600, ..Default::default() }), 35_064);
    }

    const MAX_SUPPLY_DROPS: u64 = crate::params::MAX_SUPPLY;

    /// On a chain with 10-block epochs: each boundary mints the epoch's issuance (the treasury
    /// takes the stakers' share while no pool can, the contribution pool its 20%), and the
    /// endowment streams what has vested into the pool. Supply accounts for every drop.
    #[test]
    fn epochs_mint_and_stream_the_endowment() {
        let endow = Allocation { address: crate::chain::endowment("boo"), amount: 1_000_000 * FANG, vesting: Some(Vesting { start: 0, cliff: 0, end: 100 }), label: Some("endowment".into()) };
        let mut t = chain("issue", vec![endow], &[], 10);
        let (pool, treasury, endowment) = (t.c.state.pool(), t.c.state.treasury(), crate::chain::endowment("boo"));
        let epy = epochs_per_year(&t.c.state.params);
        assert_eq!(epy, 3_155_760);
        let genesis_supply = t.c.state.supply;
        t.blocks(8);
        assert_eq!((t.c.state.supply, t.bal(&pool)), (genesis_supply, 0), "nothing before the first boundary");
        t.blocks(1); // block 9 ends epoch 0
        let mint = (genesis_supply as u128 * 60_000 / (1_000_000 * epy as u128)) as u64;
        assert!(mint > 0);
        let to_pool = mint * 2_000 / 10_000;
        assert_eq!(t.c.state.supply, genesis_supply + mint);
        assert_eq!(t.bal(&treasury), mint - to_pool);
        assert_eq!(t.bal(&pool), to_pool + 90_000 * FANG, "9% of the endowment vested by block 9");
        assert_eq!(t.bal(&endowment), 910_000 * FANG);
        t.blocks(10); // block 19 ends epoch 1: the same year, the same mint
        assert_eq!(t.c.state.supply, genesis_supply + 2 * mint);
        assert_eq!(t.bal(&pool), 2 * to_pool + 190_000 * FANG);
        // Past its end the endowment has streamed out entirely.
        t.blocks(90);
        assert_eq!(t.bal(&endowment), 0);
        assert_eq!(t.c.state.econ.issuance, Issuance { year: 1, base_supply: genesis_supply, rate_ppm: 60_000 });
        t.replays();
    }

    /// Vesting locks: before the cliff only unlocked tokens can leave (transfers, escrows,
    /// names, pool funding), fees can still be paid, locked tokens can be bonded, the lock
    /// shrinks linearly after the cliff and is gone at the end.
    #[test]
    fn vesting_locks_hold_transfers_but_not_stake() {
        let (f, bob, node) = (keys(1), keys(2), keys(3));
        let mut t = chain("vesting", vec![alloc(&f, 1_000 * FANG, Some((0, 20, 120)), "founder"), alloc(&f, 50 * FANG, None, "grant")], &[("min_self_bond", 100 * FANG)], 600);
        let fa = f.address();
        assert_eq!(transferable(&t.c.state, &fa, t.now()), 50 * FANG, "only the unlocked grant");
        t.ok(&f, Action::Transfer { to: bob.address(), amount: 40 * FANG });
        let left = transferable(&t.c.state, &fa, t.now());
        assert!(left < 10 * FANG && left > 9 * FANG);
        // Moving locked tokens out is refused, whatever the action…
        for action in [
            Action::Transfer { to: bob.address(), amount: 10 * FANG },
            Action::CreateHunt { repo: "a/b".into(), issue: 1, amount: 10 * FANG, expires_at: 500, title: "t".into() },
            Action::FundPool { amount: 10 * FANG },
            Action::RegisterName { name: "abc".into() },
        ] {
            let e = t.err(&f, action);
            assert!(e.contains("locked (founder)") && e.contains("can be staked but not moved out"), "{e}");
        }
        // …and in a block it fails but pays its fee (fees may come from locked tokens).
        let stuck = t.tx(&f, Action::Transfer { to: bob.address(), amount: 10 * FANG });
        let before = t.bal(&fa);
        t.c.produce_block_with(vec![stuck.clone()]).unwrap();
        assert!(t.c.failure(&stuck.id()).unwrap().0.contains("locked"));
        assert_eq!(before - t.bal(&fa), base_fee(&t.c.state, t.c.height(), stuck.size()));
        // Bonding locked tokens is fine: the self-bond stays covered by the lock.
        let free0 = free(&t.c.state, &fa, t.now());
        let (p2p, http) = ("1.2.3.4:5", "1.2.3.4:6");
        let pop = make_pop(node.seed(), CHAIN_ID, &fa, p2p, http);
        let reg = t.ok(&f, Action::RegisterValidator { consensus_key: hex::encode(node.public_key()), p2p_addr: p2p.into(), http_addr: http.into(), bond: 100 * FANG, pop });
        assert_eq!(crate::staking::holdings(&t.c.state, &fa), (100 * FANG, 0));
        assert_eq!(free(&t.c.state, &fa, t.now()), free0 - base_fee(&t.c.state, t.c.height(), reg.size()) as i128, "the bond came out of locked tokens");
        // Past the cliff it vests linearly: at block 60, half the allocation is locked, and the
        // bond counts against it.
        t.blocks(59 - t.c.height());
        let liquid = t.bal(&fa);
        assert_eq!(t.c.state.account(&fa).locked_at(60), 500 * FANG);
        assert_eq!(transferable(&t.c.state, &fa, 60), liquid - 400 * FANG);
        t.ok(&f, Action::Transfer { to: bob.address(), amount: liquid - 401 * FANG });
        // One block later another 8.3 FANG has vested, not 10.
        assert!(t.err(&f, Action::Transfer { to: bob.address(), amount: 10 * FANG }).contains("locked"));
        // At the end nothing is locked; the finished lock can be dropped (`Unlock`, P-GOV).
        t.blocks(120 - t.c.height());
        assert_eq!(transferable(&t.c.state, &fa, 121), t.bal(&fa));
        {
            // A copy (it shares the store's snapshot, so it goes before the restart below).
            let mut s = t.c.state.clone();
            assert_eq!(release_expired(&mut s, &mut Journal::default(), &fa, 121), 1);
            assert!(s.account(&fa).locks.is_empty());
        }
        t.ok(&f, Action::Transfer { to: bob.address(), amount: 10 * FANG });
        t.replays();
    }

    /// Vote locks (P-GOV's): they overlap (the largest applies), only grow while they hold,
    /// expire on their own, and `Unlock` drops them; voting weight leaves unvested tokens out but
    /// not vote-locked ones; everything rolls back with the journal.
    #[test]
    fn vote_locks_overlap_and_expire() {
        let (a, v) = (keys(1).address(), keys(2).address());
        let mut st = State::default();
        st.accounts.insert(a.clone(), Account { balance: 100 * FANG, ..Default::default() });
        st.supply = 100 * FANG;
        let mut j = Journal::default();
        lock(&mut st, &mut j, &a, "vote:1".into(), 60 * FANG, 50, 1).unwrap();
        assert_eq!(transferable(&st, &a, 10), 40 * FANG);
        lock(&mut st, &mut j, &a, "vote:2".into(), 80 * FANG, 30, 1).unwrap();
        assert_eq!((transferable(&st, &a, 10), transferable(&st, &a, 30), transferable(&st, &a, 50)), (20 * FANG, 40 * FANG, 100 * FANG));
        // Locking an id again only extends it.
        lock(&mut st, &mut j, &a, "vote:1".into(), 10 * FANG, 20, 1).unwrap();
        assert_eq!(transferable(&st, &a, 40), 40 * FANG);
        lock(&mut st, &mut j, &a, "vote:1".into(), 70 * FANG, 60, 1).unwrap();
        assert_eq!(transferable(&st, &a, 55), 30 * FANG);
        assert_eq!(voting_balance(&st, &a, 10), 100 * FANG, "vote locks don't reduce voting weight");
        // Bounds.
        assert!(lock(&mut st, &mut j, &a, "vote:3".into(), 101 * FANG, 60, 1).unwrap_err().to_string().contains("can't lock"));
        assert!(lock(&mut st, &mut j, &a, VESTING_LOCK.into(), FANG, 60, 1).is_err());
        assert!(lock(&mut st, &mut j, &a, String::new(), FANG, 60, 1).is_err());
        assert!(lock(&mut st, &mut j, &v, "vote:1".into(), 0, 60, 1).unwrap_err().to_string().contains("no account"));
        for i in 2..MAX_LOCKS as u64 {
            lock(&mut st, &mut j, &a, format!("vote:{}", i + 1), FANG, 10, 1).unwrap();
        }
        assert!(lock(&mut st, &mut j, &a, "vote:999".into(), FANG, 10, 1).unwrap_err().to_string().contains("locks"));
        check_invariants(&st).unwrap();
        // Expired locks go with `Unlock`; the live one stays.
        assert_eq!(release_expired(&mut st, &mut j, &a, 30), MAX_LOCKS as u64 - 1);
        assert_eq!(st.account(&a).locks.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["vote:1"]);
        assert_eq!(release_expired(&mut st, &mut j, &a, 30), 0);
        // Unvested tokens don't vote; staked (bonded) ones do.
        st.acct_mut(&mut j, &a).locks.push(Lock { id: VESTING_LOCK.into(), amount: LockAmount::Vesting { total: 90 * FANG, start: 0, cliff: 100, end: 200 }, reason: "founder".into() });
        assert_eq!((voting_balance(&st, &a, 10), voting_balance(&st, &a, 150), voting_balance(&st, &a, 200)), (10 * FANG, 77 * FANG + FANG / 2, 100 * FANG));
        assert_eq!(voting_balance(&st, &v, 10), 0);
        // A vote lock holds vested tokens, so it comes on top of the vesting lock (F03): 90
        // unvested + 70 voted is more than the 100 held until the vote lock ends at 60.
        assert_eq!(st.account(&a).locked_at(55), 160 * FANG);
        assert_eq!((transferable(&st, &a, 55), transferable(&st, &a, 60), transferable(&st, &a, 150)), (0, 10 * FANG, 77 * FANG + FANG / 2));
        // Re-locking an id only needs the part that grows it (F42): with fees having taken the
        // holdings below the lock, the same lock (or a smaller ask) still goes through.
        st.acct_mut(&mut j, &a).balance = 75 * FANG;
        lock(&mut st, &mut j, &a, "vote:1".into(), 70 * FANG, 60, 55).unwrap();
        lock(&mut st, &mut j, &a, "vote:1".into(), FANG, 60, 55).unwrap();
        assert!(lock(&mut st, &mut j, &a, "vote:1".into(), 71 * FANG, 60, 55).unwrap_err().to_string().contains("can't lock"));
        assert!(lock(&mut st, &mut j, &a, "vote:4".into(), FANG, 60, 55).unwrap_err().to_string().contains("can't lock"), "75 held, 90 unvested: nothing to vote with");
        // All of it under one journal: rolled back, nothing is left.
        st.rollback(j);
        assert!(st.account(&a).locks.is_empty());
    }

    /// Rewards credited to a vesting account vest on its curve: all locked before the cliff,
    /// the unvested share of them later, none after the end or without a vesting lock.
    #[test]
    fn rewards_earned_by_locked_tokens_stay_locked() {
        let (a, plain) = (keys(1).address(), keys(2).address());
        let mut st = State::default();
        st.accounts.insert(a.clone(), Account { balance: 1_000 * FANG, locks: vec![Lock { id: VESTING_LOCK.into(), amount: LockAmount::Vesting { total: 1_000 * FANG, start: 0, cliff: 100, end: 200 }, reason: "founder".into() }], ..Default::default() });
        st.accounts.insert(plain.clone(), Account { balance: 10 * FANG, ..Default::default() });
        let mut j = Journal::default();
        // Before the cliff: a 50 FANG reward is locked entirely.
        st.j_credit(&mut j, &a, 50 * FANG);
        lock_rewards(&mut st, &mut j, &a, 50 * FANG, 50);
        assert_eq!(transferable(&st, &a, 50), 0);
        // Three-quarters vested: a 100 FANG reward adds 25 FANG to what's locked.
        let before = st.account(&a).locked_at(150);
        st.j_credit(&mut j, &a, 100 * FANG);
        lock_rewards(&mut st, &mut j, &a, 100 * FANG, 150);
        assert_eq!(st.account(&a).locked_at(150) - before, 25 * FANG);
        // After the end, and without a vesting lock: nothing to lock.
        let total = |st: &State| match st.account(&a).locks[0].amount {
            LockAmount::Vesting { total, .. } => total,
            _ => unreachable!(),
        };
        let t0 = total(&st);
        lock_rewards(&mut st, &mut j, &a, 100 * FANG, 200);
        assert_eq!(total(&st), t0);
        lock_rewards(&mut st, &mut j, &plain, 100 * FANG, 50);
        assert!(st.account(&plain).locks.is_empty());
    }

    /// The lock check on accounts other than the sender (a multisig's action executed by an
    /// approver, P-GOV): a touched account may not end up moving locked tokens.
    #[test]
    fn lock_checks_cover_every_touched_account() {
        let (sender, other) = (keys(1).address(), keys(2).address());
        let mut st = State::default();
        let locked = |balance: u64| Account { balance, locks: vec![Lock { id: "vote:1".into(), amount: LockAmount::Until { amount: 50 * FANG, until: 100 }, reason: "vote on proposal #1".into() }], ..Default::default() };
        st.accounts.insert(sender.clone(), Account { balance: FANG, ..Default::default() });
        st.accounts.insert(other.clone(), locked(40 * FANG));
        // `other` went from 60 to 40 FANG with 50 locked: refused. Receiving is always fine.
        let e = check_locks(&st, &[(other.clone(), Some(60 * FANG))], &sender, FANG as i128, 0, 10).unwrap_err().to_string();
        assert!(e.contains("is locked (vote on proposal #1)"), "{e}");
        check_locks(&st, &[(other.clone(), Some(30 * FANG))], &sender, FANG as i128, 0, 10).unwrap();
        check_locks(&st, &[(other.clone(), Some(60 * FANG))], &sender, FANG as i128, 0, 100).unwrap();
        // The sender may not go further below zero than fees took it.
        st.accounts.insert(sender.clone(), locked(49 * FANG));
        check_locks(&st, &[(sender.clone(), Some(49 * FANG))], &sender, -(FANG as i128), 0, 10).unwrap();
        assert!(check_locks(&st, &[(sender.clone(), Some(50 * FANG))], &sender, 0, 0, 10).is_err());
    }

    /// DEC-6: an action that opens an account burns the creation fee from the sender, on top of
    /// what it moves; existing accounts, names and keyless system accounts don't pay it; a sender
    /// that can't cover it fails (paying its fee); admission's dry run counts it.
    #[test]
    fn opening_an_account_burns_the_creation_fee() {
        let (a, poor, x, y, z) = (keys(1), keys(2), keys(3), keys(4), keys(5));
        let mut t = chain("open", vec![alloc(&a, 1_000 * FANG, None, "a"), alloc(&poor, FANG + FANG / 20, None, "poor")], &[], 600);
        let open = t.c.state.chain_params.account_creation_fee;
        assert_eq!(open, FANG / 10);
        let (a0, supply0) = (t.bal(&a.address()), t.c.state.supply);
        let tx = t.ok(&a, Action::Transfer { to: x.address(), amount: FANG });
        let base = base_fee(&t.c.state, t.c.height(), tx.size());
        assert_eq!((a0 - t.bal(&a.address()), t.bal(&x.address())), (FANG + base + open, FANG));
        assert_eq!(supply0 - t.c.state.supply, base + open, "the creation fee is burned");
        // An existing account, and a keyless system account (the protocol's), cost nothing to pay.
        for to in [x.address(), community("boo")] {
            let a0 = t.bal(&a.address());
            let tx = t.ok(&a, Action::Transfer { to: to.clone(), amount: FANG });
            assert_eq!(a0 - t.bal(&a.address()), FANG + base_fee(&t.c.state, t.c.height(), tx.size()), "{to}");
        }
        // Admission's dry run counts it; a sender that can't cover it fails and pays its fee.
        let to_y = t.tx(&poor, Action::Transfer { to: y.address(), amount: FANG });
        assert!(t.c.submit(to_y.clone()).unwrap_err().to_string().contains("opening 1 account burns 0.1 FANG"));
        t.c.produce_block_with(vec![to_y.clone()]).unwrap();
        assert!(t.c.failure(&to_y.id()).unwrap().0.contains("insufficient balance"));
        assert!(!t.c.state.accounts.contains_key(&y.address()));
        // A hunt awarded to a new account: its creator pays for opening it.
        t.ok(&a, Action::CreateHunt { repo: "a/b".into(), issue: 1, amount: 10 * FANG, expires_at: 500, title: "t".into() });
        let a0 = t.bal(&a.address());
        let award = t.ok(&a, Action::AwardHunt { hunt_id: 0, to: z.address(), pr: 1, familiar_key: None });
        assert_eq!(a0 - t.bal(&a.address()), base_fee(&t.c.state, t.c.height(), award.size()) + open);
        assert_eq!(t.bal(&z.address()), 10 * FANG);
        // Transfers that open an account still carry at least MIN_NEW_ACCOUNT.
        assert!(t.err(&a, Action::Transfer { to: keys(6).address(), amount: MIN_NEW_ACCOUNT - 1 }).contains("new account"));
        t.replays();
        // Governance can set it to zero.
        let mut free = chain("open-free", vec![alloc(&a, 10 * FANG, None, "a")], &[("account_creation_fee", 0)], 600);
        let s0 = free.c.state.supply;
        let tx = free.ok(&a, Action::Transfer { to: x.address(), amount: FANG });
        assert_eq!(s0 - free.c.state.supply, base_fee(&free.c.state, free.c.height(), tx.size()));
        free.done();
    }

    /// DEC-11 names: priced by length into the treasury; a year (here 10 blocks) of
    /// registration; renewals by the owner at 20% a year, from the old expiry, at most 10 years
    /// ahead; an expired name doesn't resolve or transfer; in grace (5 blocks) only the owner can
    /// renew; then anyone can register it; lapsed names are swept; reserved names stay reserved.
    #[test]
    fn names_expire_renew_and_lapse() {
        let (alice, bob) = (keys(1), keys(2));
        let mut t = chain("names", vec![alloc(&alice, 1_000 * FANG, None, "alice"), alloc(&bob, 1_000 * FANG, None, "bob")], &[("name_registration_blocks", 10), ("name_grace_blocks", 5)], 600);
        let (a, b, treasury) = (alice.address(), bob.address(), t.c.state.treasury());
        assert!(t.err(&bob, Action::RegisterName { name: "reserved".into() }).contains("is reserved"));
        let tr0 = t.bal(&treasury);
        t.ok(&alice, Action::RegisterName { name: "alice".into() });
        let h = t.c.height();
        assert_eq!(t.bal(&treasury) - tr0, 5 * FANG, "5+ characters: 5 FANG");
        let rec = t.c.state.names["alice"].clone();
        assert_eq!((rec.registered_at, rec.expires_at), (h, h + 10));
        t.ok(&alice, Action::RegisterName { name: "abc".into() });
        assert_eq!(t.bal(&treasury) - tr0, 105 * FANG, "3 characters: 100 FANG");
        assert!(t.err(&bob, Action::RegisterName { name: "alice".into() }).contains("already taken"));
        // Renewing: the owner only, 20% of the price a year, from the current expiry.
        assert!(t.err(&bob, Action::RenewName { name: "alice".into(), years: 1 }).contains("belongs to"));
        let tr1 = t.bal(&treasury);
        t.ok(&alice, Action::RenewName { name: "alice.boo".into(), years: 2 });
        assert_eq!((t.bal(&treasury) - tr1, t.c.state.names["alice"].expires_at), (2 * FANG, h + 30));
        let renew_all = Action::RenewName { name: "alice".into(), years: MAX_RENEW_YEARS };
        assert!(t.err(&alice, renew_all).contains("at most 10 years ahead"));
        // Expired: it no longer resolves, transfers or shows as a primary name.
        t.blocks(h + 31 - t.c.height());
        assert_eq!(t.now(), h + 32);
        assert!(t.c.state.resolve("alice.boo").unwrap_err().to_string().contains(&format!("expired at block {}", h + 30)));
        assert!(t.err(&bob, Action::Transfer { to: "alice.boo".into(), amount: FANG }).contains("expired"));
        assert!(t.err(&alice, Action::TransferName { name: "alice".into(), to: b.clone() }).contains("renew it first"));
        assert_eq!(t.c.state.primary_name(&a), None, "abc expired too");
        assert_eq!(t.c.view().primary_name(&a), None);
        // In grace: only the owner can renew, and the renewal counts from the old expiry.
        let e = t.err(&bob, Action::RegisterName { name: "alice".into() });
        assert!(e.contains(&format!("its owner can renew it until block {}", h + 35)), "{e}");
        t.ok(&alice, Action::RenewName { name: "alice".into(), years: 1 });
        assert_eq!(t.c.state.names["alice"].expires_at, h + 40);
        assert_eq!(t.c.state.resolve("alice").unwrap(), a);
        assert_eq!(t.c.view().primary_name(&a).as_deref(), Some("alice.boo"));
        // Past grace (abc's ran out at h + 16): anyone can register it, at full price.
        t.ok(&bob, Action::RegisterName { name: "abc".into() });
        assert_eq!((t.c.state.names["abc"].owner.as_str(), t.c.state.names["abc"].registered_at), (b.as_str(), t.c.height()));
        assert!(t.err(&alice, Action::RenewName { name: "abc".into(), years: 1 }).contains("belongs to"));
        // Transfer (an offer, then its acceptance) and release.
        t.ok(&alice, Action::TransferName { name: "alice".into(), to: b.clone() });
        assert_eq!(t.c.state.resolve("alice.boo").unwrap(), a, "still alice's until bob accepts");
        t.ok(&bob, Action::TransferName { name: "alice".into(), to: b.clone() });
        assert_eq!(t.c.state.resolve("alice.boo").unwrap(), b);
        t.ok(&bob, Action::ReleaseName { name: "alice".into() });
        assert!(!t.c.state.names.contains_key("alice"));
        t.ok(&alice, Action::RegisterName { name: "alice".into() });
        // A name nobody renews or re-registers is swept once its grace is over.
        t.ok(&bob, Action::RegisterName { name: "carol".into() });
        let carol = t.c.state.names["carol"].clone();
        t.blocks(carol.expires_at + 5 - t.c.height());
        assert!(t.c.state.names.contains_key("carol"), "still in grace");
        t.blocks(2);
        assert!(!t.c.state.names.contains_key("carol"), "swept");
        t.replays();
    }

    /// gap-6-3: a name's grace is fixed when it's registered or renewed. Lowering
    /// `name_grace_blocks` (to 0, here) doesn't free names already in their grace, or let the
    /// sweep take them: their owners can still renew, and nobody else can register them until the
    /// grace they were promised is over. Names registered or renewed after it get the new grace;
    /// a raise lengthens every grace at once.
    #[test]
    fn lowering_the_grace_keeps_the_grace_names_were_promised() {
        let (alice, bob) = (keys(1), keys(2));
        let mut t = chain("names-grace", vec![alloc(&alice, 1_000 * FANG, None, "alice"), alloc(&bob, 1_000 * FANG, None, "bob")], &[("name_registration_blocks", 10), ("name_grace_blocks", 50)], 600);
        t.ok(&alice, Action::RegisterName { name: "alice".into() });
        t.ok(&alice, Action::RegisterName { name: "sweepme".into() });
        let (rec, sweep) = (t.c.state.names["alice"].clone(), t.c.state.names["sweepme"].expires_at);
        assert_eq!(rec.grace_blocks, 50);
        t.blocks(sweep + 1 - t.c.height());
        let grace = |t: &T, n: &str| grace_until(&t.c.state, &t.c.state.names[n]);
        assert_eq!(grace(&t, "alice"), rec.expires_at + 50);
        assert!(t.err(&bob, Action::RegisterName { name: "alice".into() }).contains("its owner can renew it until block"));
        // Governance lowers the grace to nothing.
        t.c.state.exec_set_param(&mut Journal::default(), "name_grace_blocks", 0).unwrap();
        assert_eq!(grace(&t, "alice"), rec.expires_at + 50, "the grace it was promised");
        let e = t.err(&bob, Action::RegisterName { name: "alice".into() });
        assert!(e.contains(&format!("its owner can renew it until block {}", rec.expires_at + 50)), "{e}");
        t.blocks(3);
        assert!(t.c.state.names.contains_key("sweepme"), "the sweep keeps it through its grace too");
        // Its owner renews it: the new term has today's grace.
        t.ok(&alice, Action::RenewName { name: "alice".into(), years: 1 });
        let renewed = t.c.state.names["alice"].clone();
        assert_eq!((renewed.expires_at, renewed.grace_blocks, grace(&t, "alice")), (rec.expires_at + 10, 0, rec.expires_at + 10));
        t.ok(&bob, Action::RegisterName { name: "bob".into() });
        assert_eq!(t.c.state.names["bob"].grace_blocks, 0, "registered after the change: the new grace");
        // A raise applies to every name at once.
        t.c.state.exec_set_param(&mut Journal::default(), "name_grace_blocks", 70).unwrap();
        assert_eq!((grace(&t, "alice"), grace(&t, "sweepme")), (rec.expires_at + 80, sweep + 70));
        t.c.state.exec_set_param(&mut Journal::default(), "name_grace_blocks", 0).unwrap();
        // "sweepme" lapses after the 50 blocks it was registered with (txs admitted now run at
        // the next height), then anyone can take it.
        t.blocks(sweep + 49 - t.c.height());
        assert!(t.err(&bob, Action::RegisterName { name: "sweepme".into() }).contains("its owner can renew it until block"));
        t.blocks(1);
        t.ok(&bob, Action::RegisterName { name: "sweepme".into() });
        assert_eq!(t.c.state.names["sweepme"].owner, bob.address());
        t.done();
    }

    /// Audit F34: nobody is given a name, and so a primary name, without accepting it. A
    /// `TransferName` to another account only offers the name (it still resolves to its owner,
    /// and the owner can replace or withdraw the offer); the account it was offered to takes it
    /// with a `TransferName` to itself, and an accepted name is re-stamped, so it never ranks
    /// ahead of names the account registered itself.
    #[test]
    fn a_name_changes_hands_only_when_its_recipient_accepts() {
        let (mallory, victim, bob) = (keys(1), keys(2), keys(3));
        let mut t = chain("names-offer", vec![alloc(&mallory, 1_000 * FANG, None, "m"), alloc(&victim, 1_000 * FANG, None, "v"), alloc(&bob, 1_000 * FANG, None, "b")], &[("name_registration_blocks", 60)], 600);
        let (m, v, b) = (mallory.address(), victim.address(), bob.address());
        // Mallory's names are older than the victim's own.
        for i in 0..9 {
            t.ok(&mallory, Action::RegisterName { name: format!("scam-wallet-{i}") });
        }
        t.ok(&victim, Action::RegisterName { name: "victim".into() });
        let primary = |t: &T| (t.c.state.primary_name(&v), t.c.view().primary_name(&v));
        let own = (Some("victim.boo".to_string()), Some("victim.boo".to_string()));
        assert_eq!(primary(&t), own);
        // Offers change nothing for the victim, however many.
        for i in 0..9 {
            let offer = t.ok(&mallory, Action::TransferName { name: format!("scam-wallet-{i}"), to: v.clone() });
            assert_eq!(t.c.view().describe(&offer, Some(t.c.height())), format!("scam-wallet-0.boo offered scam-wallet-{i}.boo to victim.boo"));
        }
        assert_eq!(primary(&t), own);
        assert_eq!((t.c.state.resolve("scam-wallet-0").unwrap(), t.c.state.names["scam-wallet-0"].offered_to.as_deref()), (m.clone(), Some(v.as_str())));
        let send = t.tx(&victim, Action::Transfer { to: b.clone(), amount: FANG });
        assert_eq!(t.c.view().describe(&send, None), "victim.boo sent 1 FANG to ".to_string() + &short(&b));
        // Only the account it was offered to can accept it, for itself.
        assert!(t.err(&bob, Action::TransferName { name: "scam-wallet-0".into(), to: b.clone() }).contains("belongs to"));
        assert!(t.err(&victim, Action::TransferName { name: "scam-wallet-0".into(), to: b.clone() }).contains("belongs to"));
        assert!(t.err(&victim, Action::TransferName { name: "victim".into(), to: v.clone() }).contains("you already own"));
        // Accepted, it's the victim's, but its own older name stays its primary name.
        let accept = t.ok(&victim, Action::TransferName { name: "scam-wallet-0.boo".into(), to: v.clone() });
        let h = t.c.height();
        assert_eq!(t.c.view().describe(&accept, Some(h)), "victim.boo accepted scam-wallet-0.boo");
        let rec = t.c.state.names["scam-wallet-0"].clone();
        assert_eq!((rec.owner.as_str(), rec.registered_at, rec.offered_to), (v.as_str(), h, None));
        assert_eq!(primary(&t), own);
        assert!(t.c.view().names_of(&v, 8).ends_with(&["scam-wallet-0".to_string()]), "after the names it registered");
        // The owner can replace an offer, or withdraw it.
        t.ok(&mallory, Action::TransferName { name: "scam-wallet-1".into(), to: b.clone() });
        assert!(t.err(&victim, Action::TransferName { name: "scam-wallet-1".into(), to: v.clone() }).contains("belongs to"));
        let withdraw = t.ok(&mallory, Action::TransferName { name: "scam-wallet-1".into(), to: m.clone() });
        assert_eq!(t.c.view().describe(&withdraw, Some(t.c.height())), "scam-wallet-1.boo withdrew its offer of scam-wallet-1.boo");
        assert_eq!(t.c.state.names["scam-wallet-1"].offered_to, None);
        assert!(t.err(&bob, Action::TransferName { name: "scam-wallet-1".into(), to: b.clone() }).contains("belongs to"));
        // An expired name's offer can't be accepted.
        let expiry = t.c.state.names["scam-wallet-2"].expires_at;
        t.blocks(expiry + 1 - t.c.height());
        assert!(t.err(&victim, Action::TransferName { name: "scam-wallet-2".into(), to: v.clone() }).contains("can't be accepted"));
        assert_eq!(t.c.state.names["scam-wallet-2"].owner, m);
        t.replays();
    }

    /// Audit F35 (a mitigation): the mempool gives a name to whoever asked first. Another
    /// account's registration of a name already waiting isn't admitted, whatever its tip, unless
    /// the waiting one can no longer be included or has waited past `NAME_HOLD_BLOCKS`. The
    /// waiting registrations are indexed by name, and the index follows inclusion and drops.
    #[test]
    fn a_name_goes_to_whoever_asked_first() {
        use crate::chain::NAME_HOLD_BLOCKS;
        let (alice, mallory, carol) = (keys(1), keys(2), keys(3));
        let mut t = chain("names-first", vec![alloc(&alice, 1_000 * FANG, None, "a"), alloc(&mallory, 1_000 * FANG, None, "m"), alloc(&carol, 1_000 * FANG, None, "c")], &[], 600);
        let waiting = |t: &T, name: &str| t.c.live.mempool().registrations(name).count();
        let first = t.tx(&alice, Action::RegisterName { name: "coinbase".into() });
        t.c.submit(first.clone()).unwrap();
        assert_eq!(waiting(&t, "coinbase"), 1);
        let mut outbid = t.unsigned(&mallory, Action::RegisterName { name: "Coinbase.boo".into() });
        outbid.tip = FANG;
        outbid.fee += FANG;
        let e = t.c.submit(outbid.sign(&mallory)).unwrap_err().to_string();
        assert!(e.contains("already waiting"), "{e}");
        // Resubmitting the waiting one is idempotent, and other names aren't affected.
        t.c.submit(first).unwrap();
        t.c.submit(t.tx(&mallory, Action::RegisterName { name: "coinbase2".into() })).unwrap();
        assert_eq!((waiting(&t, "coinbase"), waiting(&t, "coinbase2")), (1, 1));
        t.c.produce_block().unwrap();
        assert_eq!(t.c.state.resolve("coinbase.boo").unwrap(), alice.address());
        assert_eq!((waiting(&t, "coinbase"), waiting(&t, "coinbase2")), (0, 0), "included registrations leave the index");
        assert!(t.err(&mallory, Action::RegisterName { name: "coinbase".into() }).contains("already taken"));

        // A registration left waiting holds the name for NAME_HOLD_BLOCKS after it arrived, then
        // nothing: (here) empty blocks stand in for full ones it's outbid in.
        t.c.submit(t.tx(&alice, Action::RegisterName { name: "binance".into() })).unwrap();
        t.blocks(NAME_HOLD_BLOCKS);
        assert!(t.err(&mallory, Action::RegisterName { name: "binance".into() }).contains("already waiting"));
        t.blocks(1);
        let late = t.tx(&mallory, Action::RegisterName { name: "binance".into() });
        t.c.submit(late.clone()).unwrap();
        assert_eq!(waiting(&t, "binance"), 2);
        // Alice's own txs are never held off by it; mallory's newer one now holds the name.
        t.c.submit(t.tx(&alice, Action::Transfer { to: mallory.address(), amount: FANG })).unwrap();
        assert!(t.err(&carol, Action::RegisterName { name: "binance".into() }).contains("already waiting"));
        // Dropping a waiting registration (here: a full node's failed forward) unindexes it.
        t.c.drop_pending(&late.id());
        assert_eq!(waiting(&t, "binance"), 1);

        // A waiting registration whose fee no longer covers the base fee holds nothing.
        let stuck = t.tx(&alice, Action::RegisterName { name: "kraken".into() });
        t.c.submit(stuck).unwrap();
        for s in t.c.state.econ.fees.iter_mut() {
            s.rate *= 100;
        }
        t.c.submit(t.tx(&mallory, Action::RegisterName { name: "kraken".into() })).unwrap();
        t.done();
    }

    /// Review of audit F35's mitigation: a registration wrapped in a multisig action asks for the
    /// name too. A 1-of-1 multisig's `MultisigPropose { RegisterName }` (it runs at once) and a
    /// `MultisigApprove` that would run a stored one are held off by another account's waiting
    /// registration, and hold the name against others while they wait (a multisig's owner keys
    /// can't tip, so without the hold any tipped registration outbids a multisig's). Wrapped
    /// registrations that wouldn't run when included (a k-of-n proposal, an approval short of the
    /// threshold) hold nothing, but are still held off, so approvals that only together reach the
    /// threshold, or a proposal and its approval queued back to back, can't get in either.
    #[test]
    fn wrapped_registrations_ask_first_too() {
        use crate::types::key_id;
        let (alice, mallory, carol, erin, frank) = (keys(1), keys(2), keys(3), keys(4), keys(5));
        let mut t = chain("names-wrapped", ["a", "m", "c", "e", "f"].iter().zip([&alice, &mallory, &carol, &erin, &frank]).map(|(l, k)| alloc(k, 1_000 * FANG, None, l)).collect(), &[], 600);
        let waiting = |t: &T, name: &str| t.c.live.mempool().registrations(name).count();
        let kid = |k: &Keys| key_id(&k.public_key());
        // `signer`'s tx for `from` (one of its accounts), at that account's next nonce.
        let as_key = |t: &T, signer: &Keys, from: &Keys, action: Action| {
            let mut tx = t.unsigned(signer, action);
            (tx.from, tx.nonce) = (from.address(), t.c.next_nonce(&from.address()));
            tx.sign(signer)
        };
        // `k`'s tx tipping ten times the base fee.
        let tipped = |t: &T, k: &Keys, action: Action| {
            let mut tx = t.unsigned(k, action);
            (tx.tip, tx.fee) = (tx.fee * 10, tx.fee * 11);
            tx.sign(k)
        };
        let wrap = |name: &str| Action::MultisigPropose { action: Box::new(Action::RegisterName { name: name.into() }) };
        // Mallory's account is a 1-of-1 multisig; erin's a 2-of-2 with frank's key.
        t.ok(&mallory, Action::MultisigCreate { owners: vec![kid(&mallory)], threshold: 1 });
        let gh = t.c.genesis_hash.clone();
        t.ok(&erin, crate::types::add_key(&frank, CHAIN_ID, &gh, &erin.address(), "frank", KeyRole::Owner));
        t.ok(&erin, Action::MultisigCreate { owners: vec![kid(&erin), kid(&frank)], threshold: 2 });

        // The reviewer's bypass: alice waits for coinbase; mallory's 1-of-1 wrap asks for it too.
        t.c.submit(t.tx(&alice, Action::RegisterName { name: "coinbase".into() })).unwrap();
        let e = t.c.submit(as_key(&t, &mallory, &mallory, wrap("Coinbase.boo"))).unwrap_err().to_string();
        assert!(e.contains("already waiting"), "{e}");
        t.c.produce_block().unwrap();
        assert_eq!(t.c.state.resolve("coinbase.boo").unwrap(), alice.address());

        // A waiting 1-of-1 wrap holds the name: carol's direct registration isn't admitted,
        // whatever its tip (a multisig can't tip back).
        t.c.submit(as_key(&t, &mallory, &mallory, wrap("kraken"))).unwrap();
        assert_eq!(waiting(&t, "kraken"), 1);
        let e = t.c.submit(tipped(&t, &carol, Action::RegisterName { name: "kraken".into() })).unwrap_err().to_string();
        assert!(e.contains("already waiting"), "{e}");
        t.c.produce_block().unwrap();
        assert_eq!((t.c.state.resolve("kraken.boo").unwrap(), waiting(&t, "kraken")), (mallory.address(), 0));

        // A 2-of-2 proposal waiting holds nothing (it doesn't register when included)…
        t.c.submit(as_key(&t, &erin, &erin, wrap("gemini"))).unwrap();
        assert_eq!(waiting(&t, "gemini"), 1, "indexed");
        t.c.submit(tipped(&t, &carol, Action::RegisterName { name: "gemini".into() })).unwrap();
        // … and a proposal of a name another account is waiting for is held off, so erin can't
        // queue a proposal and frank's approval behind it.
        t.c.submit(t.tx(&alice, Action::RegisterName { name: "bitstamp".into() })).unwrap();
        let e = t.c.submit(as_key(&t, &erin, &erin, wrap("bitstamp"))).unwrap_err().to_string();
        assert!(e.contains("already waiting"), "{e}");
        t.c.produce_block().unwrap();
        assert_eq!(t.c.state.resolve("gemini.boo").unwrap(), carol.address());
        assert_eq!((waiting(&t, "gemini"), waiting(&t, "bitstamp")), (0, 0));
        let gemini = t.c.state.governance.next_multisig_id - 1;
        assert!(matches!(&t.c.state.governance.multisig_proposals[&gemini].action, Action::RegisterName { name } if name == "gemini"));

        // A stored proposal of a free name: frank's approval would run it, so it's held off by
        // alice's waiting registration (the k-of-n variant of the bypass)…
        t.c.submit(as_key(&t, &erin, &erin, wrap("okx"))).unwrap();
        t.c.produce_block().unwrap();
        let okx = t.c.state.governance.next_multisig_id - 1;
        t.c.submit(t.tx(&alice, Action::RegisterName { name: "okx".into() })).unwrap();
        let approve = |t: &T, id: u64| Action::MultisigApprove { id, action_digest: crate::types::action_digest(&t.c.state.governance.multisig_proposals[&id].action) };
        let e = t.c.submit(as_key(&t, &frank, &erin, approve(&t, okx))).unwrap_err().to_string();
        assert!(e.contains("already waiting"), "{e}");
        t.c.produce_block().unwrap();
        assert_eq!(t.c.state.resolve("okx.boo").unwrap(), alice.address());
        // … and, waiting, it holds the name against carol.
        t.c.submit(as_key(&t, &erin, &erin, wrap("bybit"))).unwrap();
        t.c.produce_block().unwrap();
        let bybit = t.c.state.governance.next_multisig_id - 1;
        // An approval bound to another action (gap-7-6) wouldn't run the registration: it holds
        // nothing, and admission refuses it.
        let stale = as_key(&t, &frank, &erin, Action::MultisigApprove { id: bybit, action_digest: "00".repeat(32) });
        assert!(!crate::governance::runs_when_included(&t.c.state, &stale, t.c.height() + 1));
        assert!(t.c.submit(stale).unwrap_err().to_string().contains("isn't the action this approval was signed for"));
        let bound = as_key(&t, &frank, &erin, approve(&t, bybit));
        assert!(crate::governance::runs_when_included(&t.c.state, &bound, t.c.height() + 1));
        t.c.submit(bound).unwrap();
        assert_eq!(waiting(&t, "bybit"), 1);
        let e = t.c.submit(tipped(&t, &carol, Action::RegisterName { name: "bybit".into() })).unwrap_err().to_string();
        assert!(e.contains("already waiting"), "{e}");
        t.c.produce_block().unwrap();
        assert_eq!((t.c.state.resolve("bybit.boo").unwrap(), waiting(&t, "bybit")), (erin.address(), 0));
        t.done();
    }

    /// The sweep visits every name, a batch per block, and wraps around.
    #[test]
    fn the_name_sweep_wraps_around() {
        let mut st = State::default();
        for i in 0..100 {
            let n = format!("name{i:03}");
            st.names.insert(n.clone(), NameRecord { name: n, owner: keys(1).address(), registered_at: 0, expires_at: if i % 3 == 0 { 0 } else { 1_000 }, offered_to: None, grace_blocks: 0 });
        }
        let grace = st.chain_params.name_grace_blocks;
        for h in 0..4 {
            sweep_names(&mut st, grace + 1 + h);
        }
        assert_eq!(st.names.len(), 66, "every lapsed name is gone after one pass");
        assert_eq!(st.econ.name_cursor, "", "and the next pass starts over");
    }
}
