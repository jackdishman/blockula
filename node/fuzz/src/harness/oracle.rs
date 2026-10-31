//! The v2 fixture's oracles (docs/PROGRAMS-V0.md §10.4): what a program transaction costs, how much
//! fuel it reserves, and what its receipt and the state say it did.
//!
//! **Independent of the code they check.** An oracle that asks the node's own function for the
//! answer (`fuel::fuel_fee`, `econ::charge`, the receipt it is checking) holds the node to itself:
//! a wrong rounding moves the balance and the receipt together, and the books still balance. So
//! nothing here calls `programs::fuel`, `programs::receipt`, `programs::commit`, `econ::charge` or
//! `econ::base_fee`. Every expected value has one of three sources, and the report names which:
//!
//! - **the spec's formulas**, written out here from §5.1 (what a transaction reserves, its fuel
//!   fee), §5.2 and §5.3 (the schedule's prices and the limits, in [`spec`]), §5.6 (the fuel rate
//!   controller), §7.2 (the spans of a pause) and §8.1 (the receipt's facts and the table of
//!   outcomes), over raw state: the fee ring's slot, META's parameters and fuel rate, the code
//!   table's sizes. `tests/harness.rs` holds [`spec`]'s numbers to `SCHEDULE_V0`'s, so a schedule
//!   that moves fails there and not silently here;
//! - **deltas between the state before a transaction and the state after it**: every account's
//!   balance against the receipt's `balance_changes`, the programs a create made against its
//!   `created`, the blobs an upload stored against its `upload`, META's `state_bytes` and `escrow`
//!   against `storage`;
//! - **models of the guests** (`node/vm/guests`), which have documented storage layouts: the
//!   counter's count, the token's balances against its supply, the escrow's open deals, each
//!   compared with what the state stores after the block.
//!
//! The one place the node runs inside an oracle is [`check_refusal`], as an experiment and not as an
//! expected value: a transaction the value cap or exit-only refused is run again, on a copy of the
//! state before it, with the rule taken away, and must then raise the programs control, which is all
//! those rules refuse. (`Sm::tx_paying` also asks `econ::charge` for the exact charge, to sign it;
//! what that charge should be is this module's to say.)
//!
//! What this can't check is a successful call's `fuel.used`: it is the engine's metering, which
//! `program_exec` (node/vm/fuzz) owns. An upload's is its `upload_fuel`, and any out-of-fuel
//! failure's is the limit; those are checked.

use super::v2::GUESTS;
use super::*;
use crate::programs::receipt::{ProgramReceipt, ReceiptOutcome};
use crate::programs::state::{blob_key, storage_key, BLOB_CODE, BLOB_INTERFACE};
use std::collections::{BTreeMap, BTreeSet};

/// The spec's numbers, copied from it (§5.2, §5.3, §4.5, §7.2). `tests/harness.rs` holds them to
/// the node's `SCHEDULE_V0`, so they can't drift apart unnoticed.
pub(super) mod spec {
    /// Load, per distinct declared code: `LOAD_BASE + LOAD_PER_BYTE × size` (§5.2).
    pub const LOAD_BASE: u64 = 10_000;
    pub const LOAD_PER_BYTE: u64 = 40;
    /// An upload's `exec`: `UPLOAD_BASE + UPLOAD_PER_BYTE × (len(wasm) + len(interface))` (§5.2).
    pub const UPLOAD_BASE: u64 = 50_000;
    pub const UPLOAD_PER_BYTE: u64 = 71;
    /// Rates are milli-drops per unit: `fee = rate × amount / RATE_SCALE` (§5.1).
    pub const RATE_SCALE: u64 = 1_000;
    /// `MAX_RESERVATION` (§5.3), and the fuel rate's ceiling, `u64::MAX / MAX_RESERVATION`.
    pub const MAX_RESERVATION: u64 = 1 << 26;
    pub const MAX_FUEL_RATE: u64 = u64::MAX / MAX_RESERVATION;
    /// Parameter defaults (§4.5), for a key META's map doesn't hold.
    pub const DEFAULT_FLOOR_RATE: u64 = 250;
    pub const DEFAULT_MAX_BLOCK_FUEL: u64 = 40_000_000;
    pub const DEFAULT_MAX_TX_FUEL: u64 = 10_000_000;
    pub const DEFAULT_VALUE_CAP: u64 = 10_000_000 * crate::types::DROPS_PER_FANG;
    /// The spans of a pause, in days (§7.2).
    pub const PAUSE_DAYS: u64 = 3;
    pub const SIGN_WINDOW_DAYS: u64 = 30;
    pub const MAX_SPAN_DAYS: u64 = 30;
    /// The outcomes' numbers (§8.1's table), by code.
    pub const OUTCOMES: [(&str, u8); 12] = [
        ("trapped", 1),
        ("aborted", 2),
        ("commit", 3),
        ("value_cap", 10),
        ("exit_only", 11),
        ("storage_limit", 12),
        ("state_full", 13),
        ("invalid_code", 14),
        ("translation_refused", 15),
        ("interface_mismatch", 16),
        ("native", 20),
        ("ok", 0),
    ];
}

/// META's parameter `key`, or the spec's default.
fn param(state: &State, key: &str, default: u64) -> u64 {
    state.programs.meta.as_ref().and_then(|m| m.params.get(key)).copied().unwrap_or(default)
}

/// The value cap in force: `program_value_cap`.
pub(super) fn value_cap(state: &State) -> u64 {
    param(state, "program_value_cap", spec::DEFAULT_VALUE_CAP)
}

/// Whether `tx` is a program transaction (tags 62–64: §5.1's "anything else" reserves nothing).
pub(super) fn is_program_tx(tx: &Tx) -> bool {
    matches!(tx.action, Action::UploadCode { .. } | Action::CreateProgram { .. } | Action::CallProgram { .. })
}

/// Whether the `programs` feature is on (§1.1): the raw flag.
pub(super) fn feature_on(state: &State) -> bool {
    state.upgrade.features.contains_key("programs")
}

/// Whether a pause is running at `height` (§7.2): META's last run covers it.
pub(super) fn paused_at(state: &State, height: u64) -> bool {
    state.programs.meta.as_ref().and_then(|m| m.pause.as_ref()).is_some_and(|p| p.from <= height && height < p.until)
}

/// The FANG programs control (§7.1): `value_held + sponsored`.
pub(super) fn control(state: &State) -> u128 {
    state.programs.meta.as_ref().map_or(0, |m| u128::from(m.value_held) + u128::from(m.sponsored))
}

fn balance(state: &State, account: &str) -> u64 {
    state.accounts.get(account).map_or(0, |a| a.balance)
}

/// A 64-character hex hash as its 32 bytes.
fn hash32(hex64: &str) -> Option<[u8; 32]> {
    hex::decode(hex64).ok()?.try_into().ok()
}

// ------------------------------------------------------------------------------- the charge

/// What a program transaction is charged at `height` on the state before it, by the spec alone
/// (§5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Charge {
    /// The bytes' base fee: the fee ring's slot for the height, `flat + rate × size / 1000`.
    pub bytes: u64,
    /// `rate × reserved / 1000`, floored.
    pub fuel: u64,
    pub tip: u64,
    /// `max(fuel_rate, program_fuel_floor_rate)`.
    pub rate: u64,
    /// `exec(tx)`: an upload's `upload_fuel`, a create's or a call's `fuel_limit`.
    pub exec: u64,
    /// `load(tx)`: Σ over the distinct code it declares.
    pub load: u64,
}

impl Charge {
    pub fn reserved(&self) -> u64 {
        self.exec.saturating_add(self.load)
    }

    /// What is burned: the base fee, bytes and fuel.
    pub fn base(&self) -> u64 {
        self.bytes.saturating_add(self.fuel)
    }

    /// The base fee and the tip (`None` past a u64: no such transaction is admitted).
    pub fn total(&self) -> Option<u64> {
        self.base().checked_add(self.tip)
    }
}

/// `exec(tx)` (§5.1).
pub(super) fn exec_fuel(tx: &Tx) -> u64 {
    exec_fuel_of(&tx.action)
}

/// [`exec_fuel`] of an action.
pub(super) fn exec_fuel_of(action: &Action) -> u64 {
    match action {
        Action::UploadCode { wasm, interface, .. } => spec::UPLOAD_BASE.saturating_add(spec::UPLOAD_PER_BYTE.saturating_mul((wasm.0.len() + interface.0.len()) as u64)),
        Action::CreateProgram { fuel_limit, .. } | Action::CallProgram { fuel_limit, .. } => *fuel_limit,
        _ => 0,
    }
}

/// `load(tx)` (§5.1): the code it declares, each distinct code once, priced by its size. A call
/// declares every call's program and every binding's; a create with `init` its own code and its
/// bindings'; code or a program the state doesn't hold adds nothing.
fn load_fuel(pre: &State, tx: &Tx) -> u64 {
    let p = &pre.programs;
    let of_program = |address: &str| p.records.get(address).map(|r| r.wasm_hash);
    let mut hashes: BTreeSet<[u8; 32]> = BTreeSet::new();
    match &tx.action {
        Action::CallProgram { calls, bindings, .. } => {
            hashes.extend(calls.iter().filter_map(|c| of_program(&c.program)));
            hashes.extend(bindings.iter().filter_map(|b| of_program(&b.program)));
        }
        Action::CreateProgram { wasm_hash, init: Some(_), bindings, .. } => {
            hashes.extend(hash32(wasm_hash));
            hashes.extend(bindings.iter().filter_map(|b| of_program(&b.program)));
        }
        _ => {}
    }
    hashes.iter().filter_map(|h| p.code.get(h)).fold(0u64, |sum, c| sum.saturating_add(spec::LOAD_BASE + spec::LOAD_PER_BYTE.saturating_mul(c.wasm.len() as u64)))
}

/// The rate a transaction on `state` pays: `max(fuel_rate, program_fuel_floor_rate)` (§5.1).
fn rate_of(state: &State) -> u64 {
    let floor = param(state, "program_fuel_floor_rate", spec::DEFAULT_FLOOR_RATE);
    state.programs.meta.as_ref().map_or(floor, |m| m.fuel_rate.max(floor))
}

/// The charge of `tx` at `height` on `pre`, the state right before it (a program transaction: the
/// fuel parts are 0 for every other tag).
pub(super) fn charge(pre: &State, tx: &Tx, height: u64) -> Charge {
    let slot = height.checked_sub(pre.econ.height).and_then(|i| pre.econ.fees.get(i as usize)).unwrap_or_else(|| panic!("the fee ring holds no slot for block {height} (last executed {})", pre.econ.height));
    let bytes = u64::try_from((u128::from(slot.flat) + u128::from(slot.rate) * u128::from(tx.size()) / u128::from(spec::RATE_SCALE)).min(u128::from(u64::MAX))).expect("clamped");
    let (exec, load) = if is_program_tx(tx) { (exec_fuel(tx), load_fuel(pre, tx)) } else { (0, 0) };
    let rate = rate_of(pre);
    let reserved = exec.saturating_add(load);
    let fuel = if reserved == 0 { 0 } else { u64::try_from(u128::from(rate) * u128::from(reserved) / u128::from(spec::RATE_SCALE)).unwrap_or(u64::MAX) };
    Charge { bytes, fuel, tip: tx.tip, rate, exec, load }
}

/// The fuel rate after a block that counted `used` fuel against `limit` (§5.6): the controller's
/// formula on `current = max(fuel_rate, floor)`, clamped to `[floor, MAX_FUEL_RATE]`. The step
/// is ChainParams' `base_fee_max_change_bps` (12.5%) around `target_fullness_bps` (50%).
fn next_rate(current: u64, used: u64, limit: u64, floor: u64, cp: &crate::params::ChainParams) -> u64 {
    let max = u128::from(limit.max(1));
    let used = u128::from(used).min(max);
    let target = (max * u128::from(cp.target_fullness_bps) / 10_000).max(1);
    let (rate, step) = (u128::from(current), u128::from(cp.base_fee_max_change_bps));
    let next = if used > target && max > target {
        let up = rate * step * (used - target) / ((max - target) * 10_000);
        rate + if step > 0 { up.max(1) } else { 0 }
    } else if used < target {
        rate.saturating_sub(rate * step * (target - used) / (target * 10_000))
    } else {
        rate
    };
    u64::try_from(next).unwrap_or(u64::MAX).min(spec::MAX_FUEL_RATE).max(floor)
}

// ------------------------------------------------------------------------------ one transaction

/// What a block of the v2 fixture adds up as it runs, for the checks after it.
#[derive(Clone, Debug, Default)]
pub(super) struct BlockModel {
    /// Σ `reserved` over the program transactions the block admitted (§5.4's counted fuel).
    pub counted: u64,
    /// The parameters the block ran under (before its own `ParamChange`s): `program_max_block_fuel`,
    /// `program_fuel_floor_rate` and the rate it charged.
    pub limit: u64,
    pub floor: u64,
    pub rate: u64,
}

impl BlockModel {
    pub fn start(state: &State) -> BlockModel {
        BlockModel { counted: 0, limit: param(state, "program_max_block_fuel", spec::DEFAULT_MAX_BLOCK_FUEL), floor: param(state, "program_fuel_floor_rate", spec::DEFAULT_FLOOR_RATE), rate: rate_of(state) }
    }
}

/// A program transaction the state refused (`State::apply` returned `Err`: skipped, nobody
/// charged): a refusal that says why must have been true. Only the reasons the spec gives a
/// number for are held to it: the fee below its charge, a tip past its cap, a reservation past a
/// block or what is left of the block's budget.
pub(super) fn check_refused(pre: &State, tx: &Tx, why: &anyhow::Error, height: u64, model: &BlockModel) {
    if !is_program_tx(tx) {
        return;
    }
    let (msg, c) = (format!("{why:#}"), charge(pre, tx, height));
    let kind = tx.action.kind();
    if msg.contains("fee too low") {
        assert!(c.total().is_none_or(|t| tx.fee < t), "a {kind} was refused as too cheap, but its fee of {} drops covers its charge of {:?} ({c:?}): {msg}", tx.fee, c.total());
    }
    if msg.contains("tip too high") {
        let cap = (u128::from(c.base()) * u128::from(pre.chain_params.tip_cap_multiple)).min(u128::from(u64::MAX)) as u64;
        assert!(tx.tip > cap, "a {kind} was refused for its tip of {}, within the cap of {cap} ({c:?}): {msg}", tx.tip);
    }
    if msg.contains("larger than a block") {
        assert!(c.reserved() > model.limit, "a {kind} was refused as larger than a block, but it reserves {} of a budget of {}: {msg}", c.reserved(), model.limit);
    }
    if msg.contains("the block's program fuel is spent") {
        assert!(model.counted.saturating_add(c.reserved()) > model.limit, "a {kind} was refused for the block's fuel, but the block counted {} and it reserves {} of {}: {msg}", model.counted, c.reserved(), model.limit);
    }
}

/// What a program transaction that `State::apply` took in (applied, or failed and charged) must
/// have done, by the state before and after it and the receipt (the module docs).
pub(super) struct TxRun<'a> {
    pub pre: &'a State,
    pub post: &'a State,
    pub tx: &'a Tx,
    pub outcome: &'a Outcome,
    /// The receipt the run noted, `None` when the transaction failed before programs ran it.
    pub receipt: Option<&'a ProgramReceipt>,
    pub chain: &'a str,
    pub proposer: &'a str,
    pub height: u64,
}

pub(super) fn check_ran(v: &TxRun<'_>, model: &mut BlockModel) {
    let TxRun { pre, post, tx, outcome, receipt, chain, proposer, height } = *v;
    let kind = tx.action.kind();
    let applied = matches!(outcome, Outcome::Applied);
    let c = charge(pre, tx, height);
    let total = c.total().unwrap_or_else(|| panic!("a {kind} whose charge passes a u64 was admitted: {c:?}"));
    model.counted = model.counted.saturating_add(c.reserved());

    // Admission: the fee covers the charge, the tip is within its cap, the status lets it run.
    assert!(tx.fee >= total, "an admitted {kind} signs a fee of {} drops, below its charge of {total} ({c:?})", tx.fee);
    let cap = (u128::from(c.base()) * u128::from(pre.chain_params.tip_cap_multiple)).min(u128::from(u64::MAX)) as u64;
    assert!(tx.tip <= cap, "an admitted {kind} tips {} drops, past the cap of {cap} ({c:?})", tx.tip);
    assert!(c.reserved() <= spec::MAX_RESERVATION, "an admitted {kind} reserves {} fuel, past the bound of {}", c.reserved(), spec::MAX_RESERVATION);
    let (on, paused) = (feature_on(pre), paused_at(pre, height));
    let allowed = !paused
        && match &tx.action {
            Action::CallProgram { calls, .. } => on || calls.iter().all(|call| call.value == 0),
            _ => on,
        };
    assert!(allowed, "a {kind} ran at block {height} while programs were {} (feature {})", if paused { "paused" } else { "exit-only" }, if on { "on" } else { "off" });
    assert_ne!(tx.from, proposer, "the fixture's proposer sent a program transaction");

    // Balances: the signer pays the charge and what the receipt says it paid outside the VM, and
    // is paid what the VM's diff says; no other account moves but by the diff (the proposer also
    // gets the tip).
    let changes = |account: &str| -> i128 {
        match receipt {
            Some(r) if applied => r.balance_changes.iter().filter(|b| b.account == account).map(|b| b.change).sum(),
            _ => 0,
        }
    };
    let outside = match receipt {
        Some(r) if applied => i128::from(r.outside_vm.total()),
        _ => 0,
    };
    assert!(receipt.is_some() || !applied, "an applied {kind} left no receipt");
    let expected = i128::from(balance(pre, &tx.from)) - i128::from(total) - outside + changes(&tx.from);
    assert_eq!(i128::from(balance(post, &tx.from)), expected, "the signer of a {kind} ({outcome:?}) went from {} to {} drops, the spec says {expected} (charge {total}, outside the VM {outside}, the diff {}): {receipt:?}", balance(pre, &tx.from), balance(post, &tx.from), changes(&tx.from));
    let accounts: BTreeSet<&str> = pre.accounts.keys().chain(post.accounts.keys()).map(String::as_str).collect();
    for account in accounts.into_iter().filter(|a| *a != tx.from) {
        let tip = if account == proposer { i128::from(tx.tip) } else { 0 };
        let expected = i128::from(balance(pre, account)) + tip + changes(account);
        assert_eq!(i128::from(balance(post, account)), expected, "account {account} went from {} to {} drops through a {kind} ({outcome:?}); the receipt's balance changes say {expected}: {receipt:?}", balance(pre, account), balance(post, account));
    }

    // A native failure that says the signer lacked funds: the signer, after the fee, must have had
    // less than the most the transaction could ask of it (its values, and its deposits, which a
    // transaction's `storage_limit` bounds; a create's account is the creation fee more).
    if let Outcome::Failed(why) = outcome {
        if why.starts_with("insufficient balance:") {
            let (values, storage_limit) = match &tx.action {
                Action::CallProgram { calls, storage_limit, .. } => (calls.iter().map(|c| c.value).fold(0u64, u64::saturating_add), *storage_limit),
                Action::CreateProgram { init, storage_limit, .. } => (init.as_ref().map_or(0, |i| i.value).saturating_add(pre.chain_params.account_creation_fee), *storage_limit),
                Action::UploadCode { storage_limit, .. } => (0, *storage_limit),
                _ => (0, 0),
            };
            let (spare, most) = (balance(pre, &tx.from).saturating_sub(total), values.saturating_add(storage_limit));
            assert!(spare < most, "a {kind} failed for want of funds ({why}), though its signer held {spare} drops after the fee and it asks for at most {most}");
        }
    }
    // A failed transaction's action leaves no trace in programs' state.
    if !applied {
        assert_eq!(pre.programs.meta, post.programs.meta, "a failed {kind} changed programs' totals");
        assert!(*pre.programs.records == *post.programs.records && *pre.programs.code == *post.programs.code && *pre.programs.interfaces == *post.programs.interfaces && *pre.programs.storage == *post.programs.storage && *pre.programs.blob_refs == *post.programs.blob_refs, "a failed {kind} changed programs' records");
    }
    // No transaction raises the FANG programs control past the cap, or at all unless programs are on.
    let (before, after) = (control(pre), control(post));
    if applied && after > before {
        let value_cap = param(pre, "program_value_cap", spec::DEFAULT_VALUE_CAP);
        assert!(on && after <= u128::from(value_cap), "a {kind} raised the programs control from {before} to {after} (feature {}, cap {value_cap})", if on { "on" } else { "off" });
    }
    let Some(r) = receipt else { return };

    // The fee: its parts, and the fuel's.
    assert_eq!((r.fee.bytes, r.fee.fuel, r.fee.tip), (c.bytes, c.fuel, c.tip), "the receipt of a {kind} charged other parts than the spec's: {:?} (bytes {}, fuel {}, tip {}) for {c:?}", r.fee, c.bytes, c.fuel, c.tip);
    assert_eq!((r.fuel.limit, r.fuel.load, r.fuel.reserved, r.fuel.rate), (c.exec, c.load, c.reserved(), c.rate), "the receipt of a {kind} says {:?}, the spec says exec {} + load {} = {} at {} milli-drops per fuel", r.fuel, c.exec, c.load, c.reserved(), c.rate);
    assert!(r.fuel.used <= r.fuel.limit, "the receipt of a {kind} used {} of {} fuel", r.fuel.used, r.fuel.limit);
    let out_of_fuel = matches!(&r.outcome, ReceiptOutcome::Trapped { trap, .. } if trap.name == "out_of_fuel");
    if out_of_fuel || matches!(tx.action, Action::UploadCode { .. }) {
        assert_eq!(r.fuel.used, r.fuel.limit, "a {kind} that {} used {} of its {} fuel; the spec says all of it", if out_of_fuel { "ran out of fuel" } else { "uploaded" }, r.fuel.used, r.fuel.limit);
    }
    // The outcome's number is the table's.
    let code = r.outcome.code();
    let number = spec::OUTCOMES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n);
    assert_eq!(number, Some(r.outcome.number()), "the outcome {code} is numbered {}, the table of §8.1 numbers it {number:?}", r.outcome.number());
    // (A transaction whose program ran and then failed a native check has an ok receipt until the
    // block's receipts are taken: `for_block` makes it `native`.)
    assert!(!applied || r.outcome.is_ok(), "a {kind} applied with a receipt that says {code}");

    if applied {
        check_facts(v, r, &c);
    }
    check_refusal(v, r);
}

/// An applied transaction's receipt facts against the state's deltas (§8.1).
fn check_facts(v: &TxRun<'_>, r: &ProgramReceipt, c: &Charge) {
    let TxRun { pre, post, tx, height, .. } = *v;
    let kind = tx.action.kind();
    let (m0, m1) = (pre.programs.meta.as_ref().expect("programs have META"), post.programs.meta.as_ref().expect("programs have META"));
    assert_eq!(i128::from(r.storage.bytes_delta), i128::from(m1.state_bytes) - i128::from(m0.state_bytes), "a {kind}'s receipt says program state grew by {} bytes, META says by {}", r.storage.bytes_delta, i128::from(m1.state_bytes) - i128::from(m0.state_bytes));
    let bonded = i128::from(r.storage.bonded) - i128::from(r.storage.refunded);
    assert_eq!(bonded, i128::from(m1.escrow) - i128::from(m0.escrow), "a {kind}'s receipt says it bonded {} and refunded {}, META's escrow moved by {}", r.storage.bonded, r.storage.refunded, i128::from(m1.escrow) - i128::from(m0.escrow));
    // The programs it made, the blobs it stored.
    let made: Vec<&String> = post.programs.records.keys().filter(|a| !pre.programs.records.contains_key(*a)).collect();
    assert_eq!(m1.programs - m0.programs, made.len() as u64, "META counts {} programs more, the records {}", m1.programs - m0.programs, made.len());
    match &tx.action {
        Action::CreateProgram { wasm_hash, interface_hash, authority, upgrade_delay_blocks, policy, accepts_transfers, .. } => {
            assert_eq!(made.len(), 1, "a create made {} programs", made.len());
            assert_eq!(r.created.as_deref(), Some(made[0].as_str()), "a create's receipt names {:?}, the program it made is {}", r.created, made[0]);
            let rec = &post.programs.records[made[0]];
            assert_eq!((&rec.creator, rec.created_at, Some(rec.wasm_hash), Some(rec.interface_hash)), (&tx.from, height, hash32(wasm_hash), hash32(interface_hash)), "the program a create made isn't the one it asked for: {rec:?}");
            let policy_byte = match policy {
                crate::types::ProgramPolicy::Open => 0,
                crate::types::ProgramPolicy::Additive => 1,
            };
            assert_eq!((&rec.authority, rec.upgrade_delay_blocks, rec.policy, rec.accepts_transfers), (authority, *upgrade_delay_blocks, policy_byte, *accepts_transfers), "the program a create made has other terms than it asked: {rec:?}");
            assert_eq!((rec.record_deposit, &rec.record_payer), (r.outside_vm.record_deposit, &tx.from), "the record's deposit isn't what the receipt says it debited the creator");
            let fee = if pre.accounts.contains_key(made[0]) { 0 } else { post.chain_params.account_creation_fee };
            assert_eq!(r.outside_vm.account_creation_fee, fee, "a create's receipt says it paid {} to open the program's account, which {}", r.outside_vm.account_creation_fee, if fee == 0 { "existed" } else { "didn't" });
            assert_eq!((r.outside_vm.upload_deposit, &r.upload), (0, &None), "a create's receipt shows an upload");
        }
        Action::UploadCode { wasm, interface, .. } => {
            assert!(made.is_empty() && r.created.is_none(), "an upload made a program");
            let u = r.upload.as_ref().unwrap_or_else(|| panic!("an applied upload's receipt shows nothing stored"));
            let (code, iface) = (hash32(&u.wasm_hash).expect("a hash"), hash32(&u.interface_hash).expect("a hash"));
            assert_eq!(post.programs.code.get(&code).map(|c| c.wasm.as_slice()), Some(wasm.0.as_slice()), "the code an upload stored isn't under the hash its receipt names");
            assert_eq!(post.programs.interfaces.get(&iface).map(|i| i.bytes.as_slice()), Some(interface.0.as_slice()), "the interface an upload stored isn't under the hash its receipt names");
            assert_eq!((u.new_code, u.new_interface), (!pre.programs.code.contains_key(&code), !pre.programs.interfaces.contains_key(&iface)), "an upload's receipt says new_code/new_interface {}/{}, the state held the blobs before {}/{}", u.new_code, u.new_interface, pre.programs.code.contains_key(&code), pre.programs.interfaces.contains_key(&iface));
            let held = |kind, hash: &[u8; 32], new| if new { post.programs.blob_refs.get(&blob_key(kind, hash)).map_or(0, |b| b.deposit) } else { 0 };
            let stored = held(BLOB_CODE, &code, u.new_code) + held(BLOB_INTERFACE, &iface, u.new_interface);
            assert_eq!((u.deposit, r.outside_vm.upload_deposit), (stored, stored), "an upload's receipt says deposit {} (outside the VM {}), the blobs it stored hold {stored}", u.deposit, r.outside_vm.upload_deposit);
            assert_eq!((r.outside_vm.record_deposit, r.outside_vm.account_creation_fee), (0, 0), "an upload's receipt shows a record deposit");
        }
        _ => {
            assert!(made.is_empty() && r.created.is_none() && r.upload.is_none(), "a call made a program or stored a blob");
            assert_eq!(r.outside_vm, Default::default(), "a call's receipt shows debits outside the VM");
        }
    }
    // FANG is conserved by the facts (§8.1).
    let moved: i128 = r.balance_changes.iter().map(|b| b.change).sum();
    let escrowed = i128::from(r.storage.bonded) - i128::from(r.outside_vm.record_deposit) - i128::from(r.outside_vm.upload_deposit) - i128::from(r.storage.refunded);
    assert_eq!(moved + escrowed + i128::from(r.burned_by_programs), 0, "the facts of a {kind} don't conserve FANG: {r:?} (charge {c:?})");
}

/// A refusal the receipt reports must have been right (§7.1, §7.3): re-run the transaction where
/// the rule isn't, and see that the rule is what stopped it.
fn check_refusal(v: &TxRun<'_>, r: &ProgramReceipt) {
    let TxRun { pre, tx, chain, proposer, height, .. } = *v;
    let kind = tx.action.kind();
    let relaxed = |change: &dyn Fn(&mut State)| -> Option<State> {
        let mut alt = pre.clone();
        change(&mut alt);
        matches!(alt.apply(tx, chain, proposer, height), Ok(Outcome::Applied)).then_some(alt)
    };
    match &r.outcome {
        ReceiptOutcome::ValueCap { cap, control: left } => {
            let value_cap = param(pre, "program_value_cap", spec::DEFAULT_VALUE_CAP);
            assert_eq!(*cap, value_cap, "a {kind} was refused past a cap of {cap}, the parameter says {value_cap}");
            assert!(*left > u128::from(*cap), "a {kind} was refused for passing the cap of {cap} with a control of {left}");
            assert!(feature_on(pre), "a {kind} was refused at the value cap while programs were exit-only");
            // With no cap, it must apply and leave the control the receipt says.
            let free = relaxed(&|s: &mut State| {
                s.programs.meta.as_mut().expect("META").params.insert("program_value_cap".into(), crate::params::MAX_SUPPLY);
            });
            if let Some(alt) = free {
                assert_eq!(control(&alt), *left, "a {kind} refused at the value cap would leave a control of {}, the receipt says {left}", control(&alt));
            }
        }
        ReceiptOutcome::ExitOnly => {
            assert!(!feature_on(pre), "a {kind} was refused as exit-only while the programs feature was on");
            // Switched on again, it must apply and raise the control: that is all exit-only refuses.
            let on = relaxed(&|s: &mut State| {
                s.upgrade.features.insert("programs".into(), crate::upgrade::Feature { activated_at: 0 });
            });
            if let Some(alt) = on {
                assert!(control(&alt) > control(pre), "a {kind} was refused as exit-only, but leaves the programs control at {} (it was {}): nothing was owed the refusal", control(&alt), control(pre));
            }
        }
        _ => {}
    }
}

// --------------------------------------------------------------------------------- the block

/// The checks after a block is run and committed: the fuel it counted and the rate it left
/// (§5.4, §5.6), program accounts' balances against `value_held`, the pause's record (§7.2).
pub(super) fn check_block_end(state: &State, height: u64, model: &BlockModel, last_run: &mut Option<u64>) {
    let counted = state.programs.block.fuel_at(height);
    assert_eq!(counted, model.counted, "block {height} counted {counted} fuel, the transactions it admitted reserve {}", model.counted);
    let meta = state.programs.meta.as_ref().expect("programs have META");
    let want = next_rate(model.rate, model.counted, model.limit, model.floor, &state.chain_params);
    assert_eq!(meta.fuel_rate, want, "block {height} counted {counted} of {} fuel at a rate of {}: the next rate is {want} by §5.6, the state says {}", model.limit, model.rate, meta.fuel_rate);
    let held: u128 = state.programs.records.keys().map(|a| u128::from(balance(state, a))).sum();
    assert_eq!(held, u128::from(meta.value_held), "program accounts hold {held} drops, META's value_held says {}", meta.value_held);
    // A pause's run (§7.2): inside its cap; a new run starts no sooner than twice the cap after the last.
    if let Some(p) = &meta.pause {
        let day = (86_400_000 / state.params.block_time_ms.max(1)).max(1);
        let span = spec::MAX_SPAN_DAYS * day;
        assert!(p.started <= p.from && p.from <= p.until && p.until <= p.started + span && !p.signers.is_empty(), "a pause's run is out of shape at block {height}: {p:?} (the cap is {span} blocks)");
        if let Some(prev) = (*last_run).filter(|prev| *prev != p.started) {
            assert!(p.started > prev + 2 * span - 1, "a pause started at block {} after one that started at {prev}; runs start at least {} blocks apart", p.started, 2 * span);
        }
        *last_run = Some(p.started);
        assert!(meta.votes.values().chain(meta.pause_signed.values()).all(|at| *at <= height), "a pause signature is from the future at block {height}");
    }
}

// --------------------------------------------------------------------------------- the guests

/// What the guests' storage must read after the blocks so far (docs in `node/vm/guests/*/src`):
/// kept from the transactions that applied, compared with what the state stores.
#[derive(Clone, Debug, Default)]
pub(super) struct Guests {
    /// Per counter program: increments applied.
    counts: BTreeMap<String, u64>,
    /// Per escrow program: the ids of the deals open.
    deals: BTreeSet<(String, [u8; 8])>,
    /// Where the last pause's run began.
    pub last_run: Option<u64>,
}

/// Which of the fixture's guests the program at `address` runs (an index into [`GUESTS`]), by
/// comparing its stored code with theirs.
fn guest_of(state: &State, address: &str) -> Option<usize> {
    let rec = state.programs.records.get(address)?;
    let code = state.programs.code.get(&rec.wasm_hash)?;
    GUESTS.iter().position(|g| g.wasm == code.wasm.as_slice())
}

impl Guests {
    /// A call transaction applied: every call in it ran. The counter counts its increments, the
    /// escrow opens and settles deals by id (its input starts with the 8-byte id).
    pub fn applied(&mut self, post: &State, tx: &Tx) {
        let Action::CallProgram { calls, .. } = &tx.action else { return };
        for call in calls {
            let id: Option<[u8; 8]> = call.input.0.get(..8).and_then(|b| b.try_into().ok());
            match (guest_of(post, &call.program), call.method.as_str(), id) {
                (Some(0), "increment", _) => *self.counts.entry(call.program.clone()).or_default() += 1,
                (Some(2), "open", Some(id)) => {
                    self.deals.insert((call.program.clone(), id));
                }
                (Some(2), "release" | "refund", Some(id)) => {
                    assert!(self.deals.remove(&(call.program.clone(), id)), "a deal settled that the model never saw open: {call:?}");
                }
                _ => {}
            }
        }
    }

    /// After a block: each guest program's storage against the model.
    pub fn check(&self, state: &State, height: u64) {
        for (address, _) in state.programs.records.iter() {
            let Some(guest) = guest_of(state, address) else { continue };
            let key = crate::crypto::address_bytes(address).expect("a program's address parses");
            let entries: Vec<(&[u8], &crate::programs::state::Entry)> = state.programs.storage.range(storage_key(&key, &[])..).take_while(|(k, _)| k.0.starts_with(&key)).map(|(k, e)| (&k.0[32..], &**e)).collect();
            let u64_of = |v: &[u8]| u64::from_le_bytes(v.try_into().unwrap_or_else(|_| panic!("a u64 of {} bytes in {address}'s storage at block {height}", v.len())));
            match GUESTS[guest].name {
                "counter" => {
                    let stored = entries.iter().find(|(k, _)| *k == b"n").map_or(0, |(_, e)| u64_of(&e.value));
                    let model = self.counts.get(address).copied().unwrap_or(0);
                    assert_eq!(stored, model, "the counter {address} stores {stored} after block {height}; {model} increments applied to it");
                }
                "token" => {
                    let supply = entries.iter().find(|(k, _)| *k == b"s").map(|(_, e)| u64_of(&e.value));
                    let held: u128 = entries.iter().filter(|(k, _)| k.len() == 33 && k[0] == b'b').map(|(_, e)| u128::from(u64_of(&e.value))).sum();
                    assert_eq!(Some(held), supply.map(u128::from), "the token {address} holds {held} in balances after block {height} against a supply of {supply:?}");
                }
                "escrow" => {
                    let stored: BTreeSet<[u8; 8]> = entries.iter().filter(|(k, _)| k.len() == 9 && k[0] == b'd').map(|(k, _)| k[1..].try_into().expect("8 bytes")).collect();
                    let model: BTreeSet<[u8; 8]> = self.deals.iter().filter(|(a, _)| a == address).map(|(_, id)| *id).collect();
                    assert_eq!(stored, model, "the escrow {address} stores deals {stored:?} after block {height}; the calls that applied leave {model:?} open");
                    let owed: u128 = entries.iter().filter(|(k, _)| k.len() == 9 && k[0] == b'd').map(|(_, e)| u128::from(u64_of(e.value.get(64..72).unwrap_or(&[0; 8])))).sum();
                    assert!(u128::from(balance(state, address)) >= owed, "the escrow {address} holds {} drops against {owed} owed in open deals", balance(state, address));
                }
                _ => {}
            }
        }
    }
}
