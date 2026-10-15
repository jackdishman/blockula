//! Economics (P-ECON, MAINNET-DESIGN 3.6, 3.7, DEC-6, DEC-11): the base fee market, issuance,
//! account creation fees, vesting locks and name renewals.
//!
//! Scaffold: the sub-state (the genesis reserved-name list) and its record plumbing, a journal,
//! the tx dispatch, and the end-of-block and issuance hooks (no-ops). `TransferName` and
//! `ReleaseName` work; `RenewName` waits for name expiry and fails with "not active yet".
//! Vesting locks live on the account (`chain::Account::locks`, from genesis allocations); this
//! module enforces them. A map is added by declaring it in [`Econ`] under this module's namespace
//! (`state::ns` 43; take more from 64 up) and journaling it in [`EconJournal`].

// Scaffold: the hooks are filled in by P-ECON.
#![cfg_attr(not(test), allow(dead_code))]

use crate::chain::{short, BlockInfo, Journal, KeyRecord, State};
use crate::state::{ns, JMap, SubState, TMap, Table};
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

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
}

impl SubState for Econ {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Econ { reserved_names } = self;
        vec![(ns::RESERVED_NAMES, reserved_names)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Econ { reserved_names } = self;
        vec![(ns::RESERVED_NAMES, reserved_names)]
    }
}

/// Undo log for [`Econ`] (part of `chain::Journal`).
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

/// Tags 45–47: RenewName, TransferName, ReleaseName.
pub(crate) fn apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, _signer: &str, key: &KeyRecord, _height: u64) -> Result<()> {
    let owned = |state: &State, name: &str| -> Result<String> {
        if key.role != KeyRole::Owner {
            bail!("only owner keys can manage names");
        }
        let n = normalize_name(name)?;
        let rec = state.names.get(&n).ok_or_else(|| anyhow!("{n}{NAME_SUFFIX} isn't registered"))?;
        if rec.owner != sender {
            bail!("{n}{NAME_SUFFIX} belongs to {}", short(&rec.owner));
        }
        Ok(n)
    };
    match &tx.action {
        Action::TransferName { name, to } => {
            let n = owned(state, name)?;
            let to = state.resolve(to)?;
            if to == sender {
                bail!("you already own {n}{NAME_SUFFIX}");
            }
            state.name_mut(j, &n).expect("checked above").owner = to;
        }
        Action::ReleaseName { name } => {
            let n = owned(state, name)?;
            state.remove_name(j, &n);
        }
        _ => bail!("not active yet"),
    }
    Ok(())
}

/// After `poc_end_block`, every block: fix the base fee for block `height + 17` (3.6).
pub fn end_block(_state: &mut State, _b: &BlockInfo) {}

/// First step at an epoch boundary: mint the epoch's issuance into validator pools, the
/// contribution pool and the treasury.
pub fn issue(_state: &mut State, _epoch: u64) {}

/// Economics' consistency rules (part of `State::check_invariants`): reserved names are
/// canonical and unregistered.
pub fn check_invariants(state: &State) -> Result<()> {
    for n in state.econ.reserved_names.keys() {
        ensure!(normalize_name(n).ok().as_deref() == Some(n.as_str()), "reserved name {n} isn't canonical");
        ensure!(!state.names.contains_key(n), "{n}{NAME_SUFFIX} is both reserved and registered");
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
