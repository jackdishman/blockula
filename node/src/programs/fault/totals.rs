//! The totals check (docs/PROGRAMS-V0.md §7.1): an alarm for a bookkeeping bug that moves the FANG
//! programs control, which the value cap and exit-only measure.
//!
//! `check_invariants` (§4.4) recomputes every total from every entry, but only tests and the
//! fuzzer run it (`State::debug_check`). In production `Chain::apply_block` runs [`check_totals`]
//! at each epoch boundary: Σ balances of program accounts, Σ per-program `sponsored`, and Σ
//! per-program `deposit`, `record_deposit` and blob deposits, against META's `value_held`,
//! `sponsored` and `escrow`. That is O(programs + blobs): it doesn't read the entries, so it can't
//! see a record whose own totals drifted from its entries (`check_invariants` does), only a META
//! total, a balance or a record that disagrees with the rest. A mismatch logs an error and sets
//! `blockula_program_accounting_drift`: an alarm, not a silence. Node-local; nothing here is
//! consensus.

use crate::chain::State;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// Whether the last check found a mismatch: the gauge `blockula_program_accounting_drift`.
/// Process-wide, like the fault counters (a node runs one chain).
pub(super) static DRIFT: AtomicBool = AtomicBool::new(false);

/// One total that disagrees with what it is the sum of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    /// META's field: `value_held`, `sponsored` or `escrow`.
    pub total: &'static str,
    /// What META records.
    pub recorded: u128,
    /// What the accounts, records and blobs add up to.
    pub found: u128,
}

/// The totals of `state` that disagree with their sums, none for a state before programs exist
/// (no META) or a sound one. Sums are `u128`: nothing here can overflow.
pub fn drift(state: &State) -> Vec<Drift> {
    let p = &state.programs;
    let Some(meta) = &p.meta else { return vec![] };
    let (mut held, mut sponsored, mut escrow) = (0u128, 0u128, 0u128);
    for (address, record) in p.records.iter() {
        // A program without an account holds nothing: `check_invariants` calls that a violation of
        // its own; here it can only show as a short `value_held`.
        held += state.accounts.get(address).map_or(0, |a| u128::from(a.balance));
        sponsored += u128::from(record.sponsored);
        escrow += u128::from(record.deposit) + u128::from(record.record_deposit);
    }
    escrow += p.blob_refs.values().map(|b| u128::from(b.deposit)).sum::<u128>();
    [("value_held", meta.value_held, held), ("sponsored", meta.sponsored, sponsored), ("escrow", meta.escrow, escrow)]
        .into_iter()
        .filter(|(_, recorded, found)| u128::from(*recorded) != *found)
        .map(|(total, recorded, found)| Drift { total, recorded: u128::from(recorded), found })
        .collect()
}

/// Checks `state`'s program totals ([`drift`]) and sets the alarm: `true` when they agree. A
/// state without programs is `true` and touches nothing, so a v1 node pays one `Option` test per
/// epoch.
pub fn check_totals(state: &State) -> bool {
    if state.programs.meta.is_none() {
        return true;
    }
    let found = drift(state);
    for d in &found {
        tracing::error!(total = d.total, recorded = %d.recorded, found = %d.found, "program accounting drift: META's total differs from what the accounts, records and blobs add up to; the FANG programs control may be wrong");
    }
    DRIFT.store(!found.is_empty(), Relaxed);
    found.is_empty()
}

/// Whether the last [`check_totals`] found a mismatch.
pub fn accounting_drift() -> bool {
    DRIFT.load(Relaxed)
}
