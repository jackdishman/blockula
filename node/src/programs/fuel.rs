//! Programs' fuel and fees (docs/PROGRAMS-V0.md §5): what a program transaction reserves, what that
//! costs, how a block counts it, and the congestion-priced fuel rate.
//!
//! **One charge, known at admission** (§5.1). Compute is a second base-fee dimension, charged on
//! what a transaction reserves, never refunded:
//!
//! ```text
//! exec(tx)    = UploadCode:     upload_fuel(len(wasm), len(interface))
//!             | CreateProgram:  fuel_limit                  (0 without init)
//!             | CallProgram:    fuel_limit
//!             | anything else:  0
//! load(tx)    = Σ over the distinct code D(tx) declares: load_fuel(size)
//! reserved    = exec + load                                 (≤ MAX_RESERVATION once admitted)
//! rate        = max(fuel_rate, program_fuel_floor_rate)     (as the state the tx runs on holds it)
//! fuel_fee    = rate × reserved / 1000                      (u128, floored)
//! base        = econ::base_fee(h, size) + fuel_fee          (econ::check_fee, econ::charge)
//! ```
//!
//! Every transaction but tags 62–64 reserves nothing, and while programs are dormant (below v2,
//! or before programs first switch on) nothing does, so until then every charge is exactly v1's.
//! The schedule is the one in force at the height (`meter::schedule`): `SCHEDULE_V0` from v2.
//!
//! **The block's budget** (§5.4). A block counts Σ `reserved` over the transactions admitted into
//! it ([`count_fuel`], in `apply_j`'s fee phase, so it sticks exactly when the fee does), and
//! admission keeps it within `program_max_block_fuel` (`super::admit`). The count lives in
//! `Programs::block`, keyed by height and journaled, so a rolled-back trial un-counts.
//!
//! **The rate** (§5.6). Each block's end moves META's `fuel_rate` by `econ::next_slot`'s formula on
//! the block's counted fuel against `program_max_block_fuel` ([`end_block`], [`next_rate`]), never
//! below the council's floor nor above [`MAX_FUEL_RATE`]. Every path (admission, a proposer's
//! trial, verification, execution) reads the rate from the state the transaction runs on, so they
//! agree with no ring of future rates.

use super::params::{param, Param};
use super::state::block_mut;
use super::{is_program_tx, status, Status, MAX_FUEL_RATE};
use crate::chain::{ApplyMode, BlockInfo, Journal, Params, State};
use crate::consensus::MAX_BLOCK_BYTES;
use crate::econ::{FEE_LAG, RATE_SCALE};
use crate::params::ChainParams;
use crate::types::{Action, Tx, MAX_GHOST_TX_BYTES};
use crate::upgrade;
use blockula_vm::meter::{Schedule, CALIBRATED_BLOCK_FUEL, MAX_RESERVATION, MAX_TX_FUEL, SCHEDULE_V0};
use std::collections::BTreeMap;

// A fuel fee fits a u64 at any rate the controller can reach, for any reservation admission lets
// through (`rate × reserved`, before the division).
const _: () = assert!(MAX_FUEL_RATE as u128 * MAX_RESERVATION as u128 <= u64::MAX as u128);
// A share (`share`) fits a u64 for any tx the codec takes: its bytes at the largest wire bound, its
// declared fuel at the largest a signed `fuel_limit` or an upload can be. So a tip times a share
// fits a u128, and `TipRate` compares exactly.
const _: () = assert!((MAX_GHOST_TX_BYTES as u128) * (CALIBRATED_BLOCK_FUEL as u128) < 1 << 64);
const _: () = assert!((MAX_TX_FUEL as u128) * (MAX_BLOCK_BYTES as u128) < 1 << 64);
const _: () = assert!(SCHEDULE_V0.upload_fuel(blockula_vm::config::MAX_CODE_BYTES, blockula_vm::config::MAX_INTERFACE_BYTES) <= MAX_TX_FUEL);

/// The fuel `tx` reserves for execution by its own bytes under `schedule` (§5.1's `exec`): an
/// upload's validation, test translation and hashing, priced by the size of its code and
/// interface; a create's or a call's signed `fuel_limit` (a create without `init` signs 0); 0 for
/// every other tx.
pub fn exec(tx: &Tx, schedule: &Schedule) -> u64 {
    match &tx.action {
        Action::UploadCode { wasm, interface, .. } => schedule.upload_fuel(wasm.0.len(), interface.0.len()),
        Action::CreateProgram { fuel_limit, .. } | Action::CallProgram { fuel_limit, .. } => *fuel_limit,
        _ => 0,
    }
}

/// The distinct code `tx` declares (§5.1's `D(tx)`), by hash, with each one's size in bytes: for a
/// call, the code of every call's program and every binding's; for a create, its own code and its
/// bindings' when `init` runs (nothing runs, and nothing is loaded, without it); for any other tx,
/// none. A program or code this state doesn't hold adds nothing (admission refuses the tx for it, in
/// its own words), so this never fails.
pub fn declared_code(state: &State, tx: &Tx) -> BTreeMap<[u8; 32], usize> {
    let p = &state.programs;
    let mut hashes: Vec<[u8; 32]> = vec![];
    let program_code = |address: &str| p.records.get(address).map(|r| r.wasm_hash);
    match &tx.action {
        Action::CallProgram { calls, bindings, .. } => {
            hashes.extend(calls.iter().filter_map(|c| program_code(&c.program)));
            hashes.extend(bindings.iter().filter_map(|b| program_code(&b.program)));
        }
        Action::CreateProgram { wasm_hash, init: Some(_), bindings, .. } => {
            hashes.extend(parse_hash(wasm_hash));
            hashes.extend(bindings.iter().filter_map(|b| program_code(&b.program)));
        }
        _ => {}
    }
    hashes.into_iter().filter_map(|h| p.code.get(&h).map(|c| (h, c.wasm.len()))).collect()
}

/// The load fuel of the code `tx` declares under `schedule` (§5.1's `load`): each distinct code
/// once, also when an earlier transaction in the block declared it (load is counted per
/// transaction, A3).
pub fn load(state: &State, tx: &Tx, schedule: &Schedule) -> u64 {
    declared_code(state, tx).values().fold(0, |sum, &size| sum.saturating_add(schedule.load_fuel(size)))
}

/// What `tx` reserves if it executes at `height` on `state` (§5.1): `exec + load`, under the
/// schedule in force there; 0 for every tx but tags 62–64, and 0 while programs are dormant (below
/// v2, or before programs first switch on), so every fee is exactly v1's until then and a program
/// tx meets admission's own refusal ("programs are off") rather than a fee it couldn't have known.
/// Admission holds it to `MAX_RESERVATION` (by the declared-code bound) and to the block's budget.
pub fn reserved(state: &State, tx: &Tx, height: u64) -> u64 {
    // Gated on the status, like every hook of §1.2.
    if !is_program_tx(tx) || status(state, height) == Status::Dormant {
        return 0;
    }
    // From v2 on there's always a schedule.
    let Some(schedule) = blockula_vm::meter::schedule(upgrade::version_at(state, height)) else { return 0 };
    exec(tx, schedule).saturating_add(load(state, tx, schedule))
}

/// The fuel rate a transaction executing on `state` pays, in milli-drops per fuel (§5.6):
/// `max(fuel_rate, program_fuel_floor_rate)`, so a floor the council raises applies from the next
/// block even where the controller's rate sits below it. Before programs first switch on there's no
/// META, and it's the floor's default (which nothing pays: nothing reserves fuel then).
pub fn rate(state: &State) -> u64 {
    let floor = param(state, Param::FuelFloorRate);
    state.programs.meta.as_ref().map_or(floor, |m| m.fuel_rate.max(floor))
}

/// The fuel part of `tx`'s base fee at `height` (§5.1): `rate × reserved / 1000`, floored. 0 for
/// every tx but tags 62–64, and for every tx while programs are dormant ([`reserved`]), so every v1
/// transaction's charge is unchanged. Never fails: a reservation
/// past what admission allows is priced all the same (saturating at `u64::MAX`), and admission
/// refuses the tx for it.
pub fn fuel_fee(state: &State, tx: &Tx, height: u64) -> u64 {
    let reserved = reserved(state, tx, height);
    if reserved == 0 {
        return 0;
    }
    let fee = u128::from(rate(state)) * u128::from(reserved) / u128::from(RATE_SCALE);
    u64::try_from(fee).unwrap_or(u64::MAX)
}

/// The least fuel fee `tx` can owe under any state and parameters (§5.3): its `exec` (its own
/// bytes say it) at the floor parameter's lower bound; load and any higher rate only add. 0 for
/// every tx but tags 62–64. `chain::check_fee` adds it to the stateless fee check, so gossip, HTTP
/// and verification refuse a program tx that can't pay for its declared compute anywhere, and never
/// one some state accepts. Under `SCHEDULE_V0`, the only schedule there is (a later one that prices
/// uploads lower must take the lower of the two here).
pub fn lowest_fuel_fee(tx: &Tx) -> u64 {
    let floor = Param::FuelFloorRate.bounds(&Params::default()).0;
    let fee = u128::from(exec(tx, &SCHEDULE_V0)) * u128::from(floor) / u128::from(RATE_SCALE);
    u64::try_from(fee).unwrap_or(u64::MAX)
}

/// Counts `tx`'s reservation into the block at `height` (§5.4), in the fee phase of `apply_j`
/// (journaled in the fee journal `j`, so it sticks exactly when the fee does, and a rolled-back
/// trial restores the block's count). Only in `Block` mode: a mempool simulation isn't a block. A
/// reservation of 0 (every tx but tags 62–64) writes nothing, so no other tx touches the count.
pub fn count_fuel(state: &mut State, j: &mut Journal, tx: &Tx, height: u64, mode: ApplyMode) {
    if mode != ApplyMode::Block {
        return;
    }
    let reserved = reserved(state, tx, height);
    if reserved == 0 {
        return;
    }
    // Admission held the count with this tx within `program_max_block_fuel` (≤ 60M).
    let run = block_mut(state, j, height);
    run.fuel = run.fuel.saturating_add(reserved);
}

/// `econ::next_slot`'s formula on a fuel rate (§5.6): the rate after a block that counted `used`
/// fuel against `limit`, with ChainParams' `target_fullness_bps` and `base_fee_max_change_bps`. A
/// full block raises it by the step (12.5% by default), an empty one lowers it as much, linearly in
/// between around the target, in u128, at least one milli-drop up when above the target. Unclamped:
/// [`end_block`] holds it within `[floor, MAX_FUEL_RATE]`.
pub fn next_rate(rate: u64, used: u64, limit: u64, cp: &ChainParams) -> u64 {
    let max = u128::from(limit.max(1));
    let used = u128::from(used).min(max);
    let target = (max * u128::from(cp.target_fullness_bps) / 10_000).max(1);
    let (rate, step) = (u128::from(rate), u128::from(cp.base_fee_max_change_bps));
    let next = if used > target && max > target {
        let up = rate * step * (used - target) / ((max - target) * 10_000);
        // At least one milli-drop, so tiny rates still climb.
        rate + if step > 0 { up.max(1) } else { 0 }
    } else if used < target {
        // `step` ≤ 10,000 (its bounds), so this never passes 0.
        rate.saturating_sub(rate * step * (target - used) / (target * 10_000))
    } else {
        rate
    };
    u64::try_from(next).unwrap_or(u64::MAX)
}

/// Programs' end-of-block step (`State::end_block`, right after `econ::end_block`): the fuel rate
/// the next block charges, from this block's counted fuel against `program_max_block_fuel` as the
/// block ran it (this block's `ParamChange`s run after, in `governance::end_block`):
///
/// ```text
/// fuel_rate = clamp(next_rate(max(fuel_rate, floor), used, limit), floor, MAX_FUEL_RATE)
/// ```
///
/// Returns at once while programs are dormant (v1, and v2 before programs first switch on: no META,
/// so nothing changes in state). Not journaled, like every end-of-block step.
pub fn end_block(state: &mut State, b: &BlockInfo) {
    if status(state, b.height) == Status::Dormant {
        return;
    }
    let Some(current) = state.programs.meta.as_ref().map(|m| m.fuel_rate) else { return };
    let used = state.programs.block.fuel_at(b.height);
    let (limit, floor) = (param(state, Param::MaxBlockFuel), param(state, Param::FuelFloorRate));
    // `clamp(floor, MAX_FUEL_RATE)`: the floor's bound is below the ceiling (`params.rs`'s const
    // assertion), and this form can't panic should the two ever cross.
    let next = next_rate(current.max(floor), used, limit, &state.chain_params).max(floor).min(MAX_FUEL_RATE);
    if let Some(m) = state.programs.meta.as_mut() {
        m.fuel_rate = next;
    }
}

/// The most the fuel rate can reach within `econ::FEE_LAG` blocks (§5.5): [`rate`] raised by a full
/// block's step `FEE_LAG` times (as `econ::base_fee` extrapolates the byte rate), at most
/// [`MAX_FUEL_RATE`]. A fee priced at it covers any of those blocks; only the including block's
/// rate is charged. `/api/fees` serves it (`programs::api::fees_fuel`).
pub fn suggested_rate(state: &State) -> u64 {
    let limit = param(state, Param::MaxBlockFuel);
    (0..FEE_LAG).fold(rate(state), |r, _| next_rate(r, limit, limit, &state.chain_params).min(MAX_FUEL_RATE))
}

/// `tx`'s share of the scarcer block resource it uses, for proposal order (`chain::TipRate`,
/// §5.6): `max(size × CALIBRATED_BLOCK_FUEL, exec × MAX_BLOCK_BYTES)`, its bytes against the codec's
/// block limit or the fuel its own bytes declare against the calibrated block fuel, whichever is the
/// larger fraction, scaled to whole numbers. A function of the tx alone (the mempool keys its queues
/// by it): `exec` under `SCHEDULE_V0`, no load, and the codec's `MAX_BLOCK_BYTES` rather than the
/// `max_block_bytes` parameter. For a tx that declares no fuel (all but tags 62–64) it's `size ×
/// CALIBRATED_BLOCK_FUEL`, so such txs compare exactly as tip per byte. Non-consensus.
pub fn share(tx: &Tx) -> u64 {
    let bytes = tx.size().max(1).saturating_mul(CALIBRATED_BLOCK_FUEL);
    let fuel = exec(tx, &SCHEDULE_V0).saturating_mul(MAX_BLOCK_BYTES as u64);
    bytes.max(fuel)
}

/// A hex64 code or interface hash as its 32 bytes (`None` if it isn't one: the codec refuses those).
pub(super) fn parse_hash(hex64: &str) -> Option<[u8; 32]> {
    hex::decode(hex64).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod e2e_tests;
