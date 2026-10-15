//! On-chain governance (P-GOV, MAINNET-DESIGN 3.5): proposals with deposits, conviction voting,
//! the bootstrap validator council, the timelock, treasury spends, community distributions with
//! Merkle claims, and M-of-N multisig accounts.
//!
//! Scaffold: the sub-state and its record plumbing, a journal, the tx dispatch (every action
//! fails with "not active yet") and the end-of-block hook (a no-op). The parameters (deposit,
//! voting and timelock periods, quorum, the conviction ladder) are in params.rs.
//! Proposal kinds that replace admin levers call the signer-free bodies the admin txs use:
//!
//! | `ProposalKind` | executes |
//! |---|---|
//! | `ParamChange` | `State::exec_set_param` (bounds in params.rs) |
//! | `TreasurySpend`, `Distribution` | this module (from the keyless treasury / community accounts) |
//! | `SetRepo` | `State::exec_set_repo` |
//! | `GrantSoul` | `State::exec_grant_soul` |
//! | `FlagSybil` | `State::exec_flag_sybil` |
//! | `RejectClaim` | `State::exec_reject_claim` |
//! | `Clawback` | `State::exec_clawback` |
//! | `UnlinkGithub` | `State::exec_unlink_github` |
//! | `RefundHunt` | `State::exec_refund_hunt` |
//! | `ScheduleUpgrade`, `ActivateFeature`, `DeactivateFeature` | `upgrade::exec_schedule_upgrade`, `exec_activate_feature`, `exec_deactivate_feature` |
//! | `Text` | nothing |
//!
//! A map is added by declaring it in [`Governance`] under one of this module's namespaces
//! (`state::ns` 37–42) and journaling it in [`GovernanceJournal`].

// Scaffold: the hooks and records are filled in by P-GOV.
#![cfg_attr(not(test), allow(dead_code))]

use crate::chain::{short, BlockInfo, Journal, KeyRecord, State};
use crate::poc::fmt_soul;
use crate::state::{SubState, Table};
use crate::types::*;
use anyhow::{bail, Result};

/// Governance's slice of the state (namespaces `state::ns` 37–42, META key `governance`).
#[derive(Clone, Debug, Default)]
pub struct Governance {}

impl SubState for Governance {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Governance {} = self;
        vec![]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Governance {} = self;
        vec![]
    }
}

/// Undo log for [`Governance`] (part of `chain::Journal`).
#[derive(Default)]
pub struct GovernanceJournal {}

impl GovernanceJournal {
    pub fn absorb(&mut self, other: GovernanceJournal) {
        let GovernanceJournal {} = other;
    }

    pub fn rollback(self, governance: &mut Governance) {
        let Governance {} = governance;
    }
}

/// Tags 39–44 and 48: Propose, Vote, Unlock, MultisigCreate, MultisigPropose, MultisigApprove,
/// ClaimDistribution.
pub(crate) fn apply(_state: &mut State, _j: &mut Journal, _tx: &Tx, _sender: &str, _signer: &str, _key: &KeyRecord, _height: u64) -> Result<()> {
    bail!("not active yet")
}

/// After `staking::end_block`, every block: close voting on proposals that ended, refund or burn
/// deposits, and execute those whose timelock is over; expire distributions.
pub fn end_block(_state: &mut State, _b: &BlockInfo) {}

/// FANG governance holds outside balances (proposal deposits, open distributions).
pub fn escrow(_state: &State) -> u128 {
    0
}

/// Governance's consistency rules (part of `State::check_invariants`).
pub fn check_invariants(_state: &State) -> Result<()> {
    Ok(())
}

/// Plain-English summary of a governance tx (see `State::describe_with`; `primary_name` looks
/// up names).
pub fn describe(state: &State, tx: &Tx, from: &str, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    match &tx.action {
        Action::Propose { kind, deposit, title, .. } => format!("{from} proposed “{title}”: {}, bonding {}", describe_kind(state, kind, primary_name), fmt_fang(*deposit)),
        Action::Vote { proposal_id, aye, amount, conviction } => {
            let side = if *aye { "aye" } else { "nay" };
            let x = match conviction {
                0 => "0.1".to_string(),
                c => c.to_string(),
            };
            format!("{from} voted {side} on proposal #{proposal_id} with {} at {x}× conviction", fmt_fang(*amount))
        }
        Action::Unlock {} => format!("{from} unlocked their expired vote locks"),
        Action::MultisigCreate { owners, threshold } => format!("{from} became a {threshold}-of-{} multisig", owners.len()),
        Action::MultisigPropose { action } => {
            let inner = Tx { action: (**action).clone(), ..tx.clone() };
            format!("{from} proposed a multisig action: {}", state.describe_with(&inner, None, primary_name))
        }
        Action::MultisigApprove { id } => format!("{from} approved multisig action #{id}"),
        Action::ClaimDistribution { distribution_id, amount, .. } => format!("{from} claimed {} from community distribution #{distribution_id}", fmt_fang(*amount)),
        _ => String::new(),
    }
}

/// What a proposal would do, in a clause ("pay 5 FANG from the treasury to alice.boo").
pub fn describe_kind(state: &State, kind: &ProposalKind, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    let who = |a: &str| match state.resolve(a) {
        Ok(addr) => primary_name(&addr).unwrap_or_else(|| short(&addr)),
        Err(_) => a.to_string(),
    };
    match kind {
        ProposalKind::ParamChange { key, value } => format!("set {key} to {value}"),
        ProposalKind::TreasurySpend { to, amount } => format!("pay {} from the treasury to {}", fmt_fang(*amount), who(to)),
        ProposalKind::Distribution { total, claim_until, .. } => format!("distribute {} from the community account (claimable until block {claim_until})", fmt_fang(*total)),
        ProposalKind::SetRepo { repo, weight: 0 } => format!("remove {repo} from the contribution pool"),
        ProposalKind::SetRepo { repo, weight } => format!("put {repo} in the contribution pool (weight {weight})"),
        ProposalKind::GrantSoul { to, amount, .. } => format!("grant {} {} Soul", who(to), fmt_soul(*amount)),
        ProposalKind::FlagSybil { account, .. } => format!("flag {} as a sybil", who(account)),
        ProposalKind::RejectClaim { claim_id, .. } => format!("reject claim #{claim_id}"),
        ProposalKind::Clawback { account, .. } => format!("claw back {}'s unvested rewards", who(account)),
        ProposalKind::UnlinkGithub { handle } => format!("unlink GitHub @{handle}"),
        ProposalKind::RefundHunt { hunt_id } => format!("refund hunt #{hunt_id}'s funders"),
        ProposalKind::ScheduleUpgrade { version, activation_height } => format!("upgrade to protocol v{version} at block {activation_height}"),
        ProposalKind::ActivateFeature { feature } => format!("turn on {feature}"),
        ProposalKind::DeactivateFeature { feature } => format!("turn off {feature}"),
        ProposalKind::Text {} => "a text proposal (no on-chain effect)".into(),
    }
}

