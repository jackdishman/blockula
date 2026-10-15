//! Staking (P-STAKE, MAINNET-DESIGN 3.4): validator pools with share accounting, delegation,
//! unbonding, slashing for equivocation and downtime, jailing, junk-block charges and the
//! stake-ranked active set with a churn limit.
//!
//! Scaffold: the sub-state and its record plumbing, a journal, the tx dispatch (every action
//! fails with "not active yet") and the end-of-block and end-of-epoch hooks (no-ops). A map is
//! added by declaring it in [`Staking`] under one of this module's namespaces (`state::ns`
//! 32–36), listing it in `tables`/`tables_mut`, and journaling it in [`StakingJournal`]; nothing
//! outside this file changes.
//!
//! Genesis operators already record what pools start from: each genesis validator's `owner`,
//! `bond` (its self-bond, escrowed) and `commission_bps` (`chain::ValidatorRecord`).

// Scaffold: the hooks and records are filled in by P-STAKE.
#![cfg_attr(not(test), allow(dead_code))]

use crate::chain::{short, BlockInfo, Journal, KeyRecord, State};
use crate::state::{SubState, Table};
use crate::types::*;
use anyhow::{bail, Result};

/// Staking's slice of the state (namespaces `state::ns` 32–36, META key `staking`).
#[derive(Clone, Debug, Default)]
pub struct Staking {}

impl SubState for Staking {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Staking {} = self;
        vec![]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Staking {} = self;
        vec![]
    }
}

/// Undo log for [`Staking`] (part of `chain::Journal`).
#[derive(Default)]
pub struct StakingJournal {}

impl StakingJournal {
    pub fn absorb(&mut self, other: StakingJournal) {
        let StakingJournal {} = other;
    }

    pub fn rollback(self, staking: &mut Staking) {
        let Staking {} = staking;
    }
}

/// Tags 30–36: Delegate, Undelegate, Redelegate, WithdrawUnbonded, UpdateValidator, Unjail,
/// ReportEquivocation.
pub(crate) fn apply(_state: &mut State, _j: &mut Journal, _tx: &Tx, _sender: &str, _signer: &str, _key: &KeyRecord, _height: u64) -> Result<()> {
    bail!("not active yet")
}

/// After `poc_end_block` and `econ::end_block`, every block: missed leader slots from
/// `view`/`parent_view` (downtime), and the proposer's charge for `skipped` txs (junk).
pub fn end_block(_state: &mut State, _b: &BlockInfo) {}

/// At an epoch boundary, after `econ::issue` (rewards are already in the pools): jail and
/// unjail, then select the next active set (top stake, churn-limited).
pub fn end_epoch(_state: &mut State, _epoch: u64) {}

/// FANG staking holds outside balances (pool stake and unbonding entries).
pub fn escrow(_state: &State) -> u128 {
    0
}

/// Staking's consistency rules (part of `State::check_invariants`).
pub fn check_invariants(_state: &State) -> Result<()> {
    Ok(())
}

/// Plain-English summary of a staking tx (see `State::describe`).
pub fn describe(state: &State, tx: &Tx, from: &str) -> String {
    let v = |a: &str| state.primary_name(a).unwrap_or_else(|| short(a));
    match &tx.action {
        Action::Delegate { validator, amount } => format!("{from} delegated {} to validator {}", fmt_fang(*amount), v(validator)),
        Action::Undelegate { validator, shares } => format!("{from} started unbonding {shares} shares of validator {}'s pool", v(validator)),
        Action::Redelegate { from_validator, to_validator, shares } => format!("{from} moved {shares} shares from validator {} to {}", v(from_validator), v(to_validator)),
        Action::WithdrawUnbonded {} => format!("{from} withdrew their unbonded stake"),
        Action::UpdateValidator { commission_bps, p2p_addr, http_addr } => {
            let addrs = if p2p_addr.is_empty() && http_addr.is_empty() { "" } else { " and updated its addresses" };
            format!("{from} set their validator's commission to {}%{addrs}", fmt_bps(*commission_bps))
        }
        Action::Unjail { validator } => format!("{from} unjailed validator {}", v(validator)),
        Action::ReportEquivocation { evidence } => format!("{from} reported a validator for double-signing ({} bytes of evidence)", evidence.len() / 2),
        _ => String::new(),
    }
}

/// Basis points as a percentage: 250 → "2.5".
pub fn fmt_bps(bps: u64) -> String {
    let (whole, frac) = (bps / 100, bps % 100);
    if frac == 0 { whole.to_string() } else { format!("{whole}.{}", format!("{frac:02}").trim_end_matches('0')) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bps_render_as_percentages() {
        assert_eq!((super::fmt_bps(250), super::fmt_bps(500), super::fmt_bps(1), super::fmt_bps(10_000)), ("2.5".into(), "5".into(), "0.01".into(), "100".into()));
    }
}
