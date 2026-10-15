//! On-chain governance (P-GOV, MAINNET-DESIGN 3.5): proposals with deposits, conviction voting,
//! the bootstrap validator council, the timelock, treasury spends, community distributions with
//! Merkle claims, and M-of-N multisig accounts.
//!
//! Scaffold: the sub-state and its record plumbing, a journal, the tx dispatch (every action
//! fails with "not active yet"), the end-of-block hook (a no-op) and the conviction ladder.
//! Proposal kinds that replace admin levers call the signer-free bodies the admin txs use:
//!
//! | `ProposalKind` | executes |
//! |---|---|
//! | `ParamChange` | `state.chain_params.set(key, value, &state.params)` |
//! | `SetRepo` | `State::exec_set_repo` |
//! | `GrantSoul` | `State::exec_grant_soul` |
//! | `FlagSybil` | `State::exec_flag_sybil` |
//! | `RejectClaim` | `State::exec_reject_claim` |
//! | `Clawback` | `State::exec_clawback` |
//! | `UnlinkGithub` | `State::exec_unlink_github` |
//! | `RefundHunt` | `State::exec_refund_hunt` |
//! | `ScheduleUpgrade`, `ActivateFeature`, `DeactivateFeature` | upgrade.rs |
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

/// Highest `Vote::conviction`.
pub const MAX_CONVICTION: u8 = 6;
/// Lock periods (`lock_period_blocks`) a vote at each conviction locks for: none at 0, then
/// 1, 2, 4, 8, 16 and 32.
pub const CONVICTION_PERIODS: [u64; 7] = [0, 1, 2, 4, 8, 16, 32];
/// Vote weight per conviction, in tenths of the amount: 0.1× with no lock, then 1×–6×.
pub const CONVICTION_TENTHS: [u64; 7] = [1, 10, 20, 30, 40, 50, 60];

/// A vote's weight: `amount` × the conviction multiplier (`None` past `MAX_CONVICTION`).
pub fn vote_weight(amount: u64, conviction: u8) -> Option<u128> {
    CONVICTION_TENTHS.get(conviction as usize).map(|t| amount as u128 * *t as u128 / 10)
}

/// How long a vote at `conviction` locks its amount.
pub fn lock_blocks(conviction: u8, lock_period_blocks: u64) -> Option<u64> {
    CONVICTION_PERIODS.get(conviction as usize).map(|p| p.saturating_mul(lock_period_blocks))
}

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

/// Plain-English summary of a governance tx (see `State::describe`).
pub fn describe(state: &State, tx: &Tx, from: &str) -> String {
    match &tx.action {
        Action::Propose { kind, deposit, title, .. } => format!("{from} proposed “{title}”: {}, bonding {}", describe_kind(state, kind), fmt_fang(*deposit)),
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
            format!("{from} proposed a multisig action: {}", state.describe(&inner, None))
        }
        Action::MultisigApprove { id } => format!("{from} approved multisig action #{id}"),
        Action::ClaimDistribution { distribution_id, amount, .. } => format!("{from} claimed {} from community distribution #{distribution_id}", fmt_fang(*amount)),
        _ => String::new(),
    }
}

/// What a proposal would do, in a clause ("pay 5 FANG from the treasury to alice.boo").
pub fn describe_kind(state: &State, kind: &ProposalKind) -> String {
    let who = |a: &str| match state.resolve(a) {
        Ok(addr) => state.primary_name(&addr).unwrap_or_else(|| short(&addr)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conviction_ladder() {
        assert_eq!(vote_weight(100, 0), Some(10));
        assert_eq!(vote_weight(100, 1), Some(100));
        assert_eq!(vote_weight(100, 6), Some(600));
        assert_eq!(vote_weight(100, 7), None);
        assert_eq!(vote_weight(u64::MAX, 6), Some(u64::MAX as u128 * 6));
        assert_eq!((lock_blocks(0, 10), lock_blocks(1, 10), lock_blocks(6, 10), lock_blocks(7, 10)), (Some(0), Some(10), Some(320), None));
        assert_eq!(CONVICTION_PERIODS.len(), MAX_CONVICTION as usize + 1);
    }
}
