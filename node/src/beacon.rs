//! Randomness beacon and revert attestations (P-BEACON, MAINNET-DESIGN 3.8): active validators
//! commit to a secret for epoch `e + 2` during `e` and reveal it during `e + 1`; the beacon seeds
//! Séances (and may shuffle leaders). Validators also attest reverted claims (`AttestRevert`).
//!
//! Scaffold: the sub-state and its record plumbing, a journal, the tx dispatch (every action
//! fails with "not active yet") and the end-of-epoch hook (a no-op). A map is added by declaring
//! it in [`Beacon`] under one of this module's namespaces (`state::ns` 44–46) and journaling it
//! in [`BeaconJournal`].

// Scaffold: the hooks and records are filled in by P-BEACON.
#![cfg_attr(not(test), allow(dead_code))]

use crate::chain::{Journal, KeyRecord, State};
use crate::state::{SubState, Table};
use crate::types::*;
use anyhow::{bail, Result};

/// The beacon's slice of the state (namespaces `state::ns` 44–46, META key `beacon`).
#[derive(Clone, Debug, Default)]
pub struct Beacon {}

impl SubState for Beacon {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Beacon {} = self;
        vec![]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Beacon {} = self;
        vec![]
    }
}

/// Undo log for [`Beacon`] (part of `chain::Journal`).
#[derive(Default)]
pub struct BeaconJournal {}

impl BeaconJournal {
    pub fn absorb(&mut self, other: BeaconJournal) {
        let BeaconJournal {} = other;
    }

    pub fn rollback(self, beacon: &mut Beacon) {
        let Beacon {} = beacon;
    }
}

/// Tags 37, 38 and 49: BeaconCommit, BeaconReveal, AttestRevert.
pub(crate) fn apply(_state: &mut State, _j: &mut Journal, _tx: &Tx, _sender: &str, _signer: &str, _key: &KeyRecord, _height: u64) -> Result<()> {
    bail!("not active yet")
}

/// At an epoch boundary, after `staking::end_epoch`: derive the beacon from the reveals, and
/// jail (and lightly slash) validators that committed without revealing.
pub fn end_epoch(_state: &mut State, _epoch: u64) {}

/// The beacon's consistency rules (part of `State::check_invariants`).
pub fn check_invariants(_state: &State) -> Result<()> {
    Ok(())
}

/// Plain-English summary of a beacon tx (see `State::describe`).
pub fn describe(_state: &State, tx: &Tx, from: &str) -> String {
    match &tx.action {
        Action::BeaconCommit { epoch, .. } => format!("{from} committed to the randomness beacon for epoch {epoch}"),
        Action::BeaconReveal { epoch, .. } => format!("{from} revealed their beacon secret for epoch {epoch}"),
        Action::AttestRevert { claim_id, revert_sha } => format!("{from} attested that claim #{claim_id} was reverted (commit {})", revert_sha.chars().take(10).collect::<String>()),
        _ => String::new(),
    }
}
