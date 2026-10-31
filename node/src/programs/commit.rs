//! Committing a successful program run (docs/PROGRAMS-V0.md §6.1 step 6), and the measure the
//! value cap (§7.1) and exit-only (§7.3) hold commits to: **the FANG programs control**,
//! `value_held + sponsored` (A40).
//!
//! [`apply_diff`] writes a successful receipt's `Diff` into the state through the action journal,
//! in canonical order, with checked arithmetic (an overflow is the transaction's failure, never a
//! panic): storage, then balances (debits, then credits), then the accounts it opened, then
//! `value_held`. The VM has already checked the diff's conservation and who was debited
//! (`Diff::check`). [`check_control`] then refuses what raised the control where it may not rise.
//!
//! **Sponsored deposits.** A program's entries are paid by the signer or, with `SPONSOR`, by the
//! program itself (W1's host: the payer is the writing frame's program or the signer), so an entry
//! is program-paid exactly when its `payer` is its own program's 32 bytes. Commit adds each new
//! program-paid entry's deposit to the record's `sponsored` and META's, and takes off each one it
//! replaces or removes (reading the stored entry before writing over it: a `StorageChange` carries
//! only the new one). A program bonding its own balance into such an entry, or getting it back,
//! moves FANG between `value_held` and `sponsored` and leaves the control as it was.

use super::exec::Failure;
use super::params::{param, Param};
use super::state::{meta_mut, storage_key, Entry, Versioned};
use super::{status, Status};
use crate::chain::{Journal, State};
use crate::crypto;
use blockula_vm::engine::Address;
use blockula_vm::exec::Diff;
use std::collections::BTreeMap;

/// The FANG programs control (§7.1): Σ balances of program accounts (`value_held`) plus the escrow
/// programs paid themselves (`sponsored`). 0 before programs exist.
pub fn control(state: &State) -> u128 {
    state.programs.meta.as_ref().map_or(0, |m| u128::from(m.value_held) + u128::from(m.sponsored))
}

/// After a program transaction's effects (a commit, an adoption), with the control at `before`
/// when it started: if the control rose, it fails while programs are exit-only (§6.1 step 6.5,
/// §7.3), and past `program_value_cap` (step 6.6, §7.1). A transaction that doesn't raise the
/// control (payouts, a program's own deposit and its refund, a signer's deposits) is never
/// refused here, so exits and clean-ups at the cap keep working.
pub fn check_control(state: &State, before: u128, height: u64) -> Result<(), Failure> {
    let after = control(state);
    if after <= before {
        return Ok(());
    }
    if status(state, height) == Status::ExitOnly {
        return Err(Failure::ExitOnly);
    }
    let cap = param(state, Param::ValueCap);
    if after > u128::from(cap) {
        return Err(Failure::ValueCap { cap, control: after });
    }
    Ok(())
}

/// A u64 total moved by a signed change, or the failure that it would leave the range.
fn moved(total: u64, change: i128, what: &str) -> Result<u64, Failure> {
    i128::from(total).checked_add(change).and_then(|t| u64::try_from(t).ok()).ok_or_else(|| Failure::native(format!("a program commit would take {what} out of range")))
}

/// Writes a successful receipt's `diff` into `state` through `j` (§6.1 steps 6.1–6.4; the module
/// docs). An `Err` leaves `j` holding what it wrote so far: the caller fails the transaction, and
/// `apply_j` rolls the action journal back.
pub fn apply_diff(state: &mut State, j: &mut Journal, diff: &Diff) -> Result<(), Failure> {
    let prefix = state.params.prefix().to_string();
    let render = |a: &Address| crypto::render_address(&prefix, a);

    // 1. Storage: each change in its order (by program, then key), the entry it replaces read
    // first, for the sponsored deposits.
    let mut sponsored: BTreeMap<Address, i128> = BTreeMap::new();
    for change in &diff.storage {
        let key = storage_key(&change.program, &change.key);
        j.programs.storage.touch(&state.programs.storage, &key);
        let new = change.entry.as_ref().map(|e| Versioned(Entry { value: e.value.clone(), payer: e.payer, deposit: e.deposit, capacity: e.capacity }));
        let old = match new.clone() {
            Some(entry) => state.programs.storage.insert(key, entry),
            None => state.programs.storage.remove(&key),
        };
        for (entry, sign) in [(old.as_ref(), -1i128), (new.as_ref(), 1)] {
            if let Some(e) = entry.filter(|e| e.payer == change.program) {
                *sponsored.entry(change.program).or_default() += sign * i128::from(e.deposit);
            }
        }
    }
    // Each program's storage totals, as the VM derived them from the same changes.
    for p in &diff.programs {
        let address = render(&p.program);
        j.programs.records.touch(&state.programs.records, &address);
        let r = state.programs.records.get_mut(&address).ok_or_else(|| Failure::native(format!("a program commit changes the storage of {address}, which isn't a program")))?;
        r.items = moved(r.items, i128::from(p.items), "a program's entry count")?;
        r.bytes = moved(r.bytes, i128::from(p.bytes), "a program's storage bytes")?;
        r.deposit = moved(r.deposit, p.deposit, "a program's deposits")?;
    }
    let mut sponsored_change = 0i128;
    for (program, change) in sponsored.into_iter().filter(|(_, c)| *c != 0) {
        let address = render(&program);
        j.programs.records.touch(&state.programs.records, &address);
        let r = state.programs.records.get_mut(&address).ok_or_else(|| Failure::native(format!("a program commit bonds a deposit for {address}, which isn't a program")))?;
        r.sponsored = moved(r.sponsored, change, "a program's sponsored deposits")?;
        sponsored_change += change;
    }
    let m = meta_mut(state, j).ok_or_else(|| Failure::native("a program commit before programs exist"))?;
    m.state_bytes = moved(m.state_bytes, i128::from(diff.state_bytes), "program state's bytes")?;
    m.escrow = moved(m.escrow, diff.escrow, "programs' escrow")?;
    m.sponsored = moved(m.sponsored, sponsored_change, "programs' sponsored deposits")?;

    // 2. Balances: the debits, then the credits, each in address order. The VM checked every
    // debit against the balance this state holds (the fee phase's included).
    let amount = |d: i128| u64::try_from(d.unsigned_abs()).map_err(|_| Failure::native("a program commit moves more than a balance holds"));
    let mut programs_change = 0i128;
    for (account, d) in &diff.balances {
        let address = render(account);
        if super::is_program(state, &address) {
            programs_change += d;
        }
        if *d < 0 {
            state.j_debit(j, &address, amount(*d)?).map_err(Failure::native)?;
        }
    }
    for (account, d) in diff.balances.iter().filter(|(_, d)| *d > 0) {
        state.j_credit(j, &render(account), amount(*d)?);
    }

    // 3. Accounts it opened: their creation fees (in `burned`, already out of the programs that
    // paid them) are burned here, and `econ::charge_new_accounts` doesn't bill the signer again.
    for account in &diff.opened {
        j.programs.opened.insert(render(account));
    }
    let burned = u64::try_from(diff.burned).map_err(|_| Failure::native("a program commit burns more than the supply"))?;
    if burned > 0 {
        state.j_burn(j, burned);
    }

    // 4. `value_held` moves by the net change of program accounts (one this transaction created
    // included: its record is in the state already).
    let m = meta_mut(state, j).ok_or_else(|| Failure::native("a program commit before programs exist"))?;
    m.value_held = moved(m.value_held, programs_change, "the FANG programs hold")?;
    Ok(())
}
