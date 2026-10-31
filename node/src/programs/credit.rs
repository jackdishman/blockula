//! The native-credit guard and the value cap (docs/PROGRAMS-V0.md §7.1, A35, A40).
//!
//! **The FANG programs control** is `value_held + sponsored` (META `programs`): Σ the balances of
//! program accounts, plus the deposits programs paid themselves (`SPONSOR`), which a program can
//! move its balance into and back out of. It's what the value cap and exit-only measure ([`control`],
//! [`check_cap`]); deposits a signer paid aren't in it, since they refund only to that signer.
//!
//! **The guard** ([`native_credit`]) runs at each native credit that pays a recipient the
//! transaction or proposal names, right after the credit and in the same journal, so an `Err` rolls
//! the credit back with the rest of the action. It takes the recipient as the address it was
//! resolved to, as every site below has it by then (admission holds every address an action names
//! to this chain's prefix): programs' records are keyed by that address, so a `.boo` name, or
//! another rendering of the same bytes, would never match a program's. A debug build asserts it.
//!
//! | Site | [`Credit`] | A program recipient |
//! |---|---|---|
//! | `Transfer` | `Transfer` | taken in while `status` is `On` and the program accepts transfers, within the cap (`value_held` grows by the amount); otherwise the transfer fails |
//! | `AwardHunt` | `Other` | refused: the award fails |
//! | `TreasurySpend` (when proposed, [`check_credit`], and when it runs) | `Other` | refused: the proposal is refused, or fails |
//! | `Unshield` (once its recipient is resolved, before its effects, [`check_credit`]) | `Other` | refused: unadmittable |
//!
//! A program has no keys and never signs (§2.1), so no other native path credits or debits one in
//! v0: its balance moves only here, at an adoption when it's created, and in its commits.
//!
//! For a recipient that isn't a program the guard is one lookup and passes; like every hook of
//! §1.2 it's gated on `status`, so while programs are dormant it reads nothing at all.

use super::params::{param, Param};
use super::state::{meta_mut, ProgramsMeta};
use super::{status, Status};
use crate::chain::{short, Journal, State};
use crate::types::fmt_fang;
use anyhow::{anyhow, bail, ensure, Result};

/// Which kind of native credit the guard runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credit {
    /// A plain `Transfer`: a program may take it in (§7.1's table).
    Transfer,
    /// Any other credit to an account a transaction or proposal names (a hunt award, a treasury
    /// spend, an unshield): never to a program.
    Other,
}

/// The FANG programs control of `m`: `value_held + sponsored` (§7.1). `Err` on an overflow, which
/// no state reaches (both are parts of the supply).
pub fn control(m: &ProgramsMeta) -> Result<u64> {
    m.value_held.checked_add(m.sponsored).ok_or_else(|| anyhow!("the FANG programs control overflows"))
}

/// The value cap on a change that takes the control from `before` to `after` (§7.1, A40): one that
/// raises it is refused if it leaves it past `program_value_cap`. One that doesn't raise it never
/// is, so payouts, and a sponsored deposit or its refund (which move FANG between `value_held` and
/// `sponsored`), go through at the cap, or above a cap lowered under them.
pub fn check_cap(state: &State, before: u64, after: u64) -> Result<()> {
    let cap = param(state, Param::ValueCap);
    if after > before && after > cap {
        bail!("programs may control at most {} (program_value_cap), and this would take them to {}", fmt_fang(cap), fmt_fang(after));
    }
    Ok(())
}

/// The guard's rule, reading only: whether `to` may be credited `amount` by a credit of `kind` at
/// `height`. For the sites that run it before the credit, or with no journal (a `TreasurySpend`
/// when it's proposed, an `Unshield`); [`native_credit`] is the same rule, then its write.
///
/// `to` is an address with this chain's prefix: the recipient once resolved, never the name a
/// transaction or proposal may give. A name isn't looked up (records are keyed by address), so
/// resolve it first.
pub fn check_credit(state: &State, to: &str, amount: u64, kind: Credit, height: u64) -> Result<()> {
    takes_in(state, to, amount, kind, height).map(|_| ())
}

/// The guard, right after a native credit of `amount` to `to` at `height`, in the credit's journal
/// `j`: nothing for a recipient that isn't a program (or while programs are dormant); for a program
/// that may take it in ([`check_credit`]'s rule), `value_held` grows by `amount`, journaled; `Err`
/// otherwise, which rolls the credit back with the action.
///
/// `to` is the address the credit paid, as for [`check_credit`]: resolve a name first.
pub fn native_credit(state: &mut State, j: &mut Journal, to: &str, amount: u64, kind: Credit, height: u64) -> Result<()> {
    if let Some(value_held) = takes_in(state, to, amount, kind, height)? {
        meta_mut(state, j).ok_or_else(|| anyhow!("programs have no state"))?.value_held = value_held;
    }
    Ok(())
}

/// The rule: `None` when it doesn't concern `to` (programs are dormant, or `to` isn't a program),
/// `Some` with `value_held` once `amount` is in when `to` is a program that may take it in, `Err`
/// when it's a program that may not.
fn takes_in(state: &State, to: &str, amount: u64, kind: Credit, height: u64) -> Result<Option<u64>> {
    // The callers' contract, checked in debug builds (dormant or not, so the v1 corpus's replay in
    // `cargo test` runs every hooked site through it): a name, or the program's bytes rendered
    // another way, would miss its record below and pass as "not a program".
    debug_assert!(crate::crypto::parse_address(to).is_some_and(|(prefix, _)| prefix == state.params.prefix()), "the native-credit guard takes the recipient's address on this chain, not {to:?}: resolve it first");
    let status = status(state, height);
    if status == Status::Dormant {
        return Ok(None);
    }
    // `programs::is_program`'s lookup, keeping the record.
    let Some(record) = state.programs.records.get(to) else { return Ok(None) };
    let to = short(to);
    if kind == Credit::Other {
        bail!("{to} is a program: programs take in native FANG only from a plain transfer, never from a hunt award, a treasury spend or an unshield");
    }
    if let Status::Paused { until } = status {
        bail!("programs are paused until block {until}: no FANG goes into a program while a pause runs");
    }
    // Neither dormant nor paused: on, or exit-only.
    ensure!(status == Status::On, "programs are exit-only: FANG may leave {to}, a program, but no transfer pays one");
    ensure!(record.accepts_transfers, "{to} is a program that doesn't accept plain transfers");
    let m = state.programs.meta.as_ref().ok_or_else(|| anyhow!("programs have no state"))?;
    let overflow = || anyhow!("the FANG programs control overflows");
    let before = control(m)?;
    let after = before.checked_add(amount).ok_or_else(overflow)?;
    check_cap(state, before, after)?;
    Ok(Some(m.value_held.checked_add(amount).ok_or_else(overflow)?))
}

#[cfg(test)]
mod tests;
