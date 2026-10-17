//! On-chain governance (P-GOV, MAINNET-DESIGN 3.5, docs/GOVERNANCE.md): proposals with deposits,
//! conviction voting, the bootstrap validator council, the timelock, treasury spends, community
//! distributions with Merkle claims, and M-of-N multisig accounts.
//!
//! **Lifecycle.** `Propose` escrows a deposit (at least `proposal_deposit`) and opens a vote that
//! takes `Vote`s through block `voting_end = created_at + voting_blocks`. At the end of that
//! block the vote closes ([`end_block`]): the deposit is refunded if the vote reached quorum
//! (or, during the bootstrap, ⅔ of the council voted) and burned otherwise; a proposal that
//! passed is queued and runs at the end of block `voting_end + timelock_blocks` (a
//! `CancelUpgrade`, which keeps the rules in force, at `voting_end + 1`). A proposal whose action
//! fails when it runs changes nothing and is marked failed.
//!
//! **Tally.** Quorum is `max(quorum_floor, quorum_bps × unlocked supply)` on the raw FANG voted
//! (aye + nay), where the unlocked supply is what could vote: what every account but the keyless
//! system ones holds (liquid, staked at its pool's value, unbonding) less what vesting locks still
//! hold of it and what still-vesting accounts' stake earned and hasn't sold (`econ::voting_balance`;
//! [`unlocked_supply`], O(vesting accounts) from running totals). A proposal passes on tokens if it reached quorum and its
//! conviction-weighted ayes beat the nays: by a simple majority, or by `supermajority_bps` of the
//! weight for upgrades (and cancelling them), feature activations, the parameters that are the
//! token holders' alone (`params::tokens_only`: governance's own and the validators' own
//! economics) and treasury spends above `treasury_large_bps` of the treasury. Until
//! `bootstrap_end_height` it also passes if ⅔ of the active set approve (a validator's council
//! vote is any `Vote` its consensus key or owner signs) and the token-weighted nays don't exceed
//! the ayes, unless it changes one of those parameters ([`council_may_pass`]).
//!
//! **Votes** lock `amount` of the voter's unlocked holdings (`econ::lock`, id `vote:<proposal>`)
//! until `conviction` lock periods after voting ends; weight is `amount` × 0.1 at conviction 0,
//! then 1×–6× (params.rs). Locks overlap across proposals. A re-vote replaces the voter's vote;
//! its lock only ever grows. `Unlock` releases expired locks.
//!
//! Proposal kinds that replace admin levers call the signer-free bodies the admin txs use:
//!
//! | `ProposalKind` | executes |
//! |---|---|
//! | `ParamChange` | `State::exec_set_param` (bounds in params.rs) |
//! | `TreasurySpend`, `Distribution` | this module (from the keyless treasury / community accounts) |
//! | `SetRepo` | `State::exec_set_repo` |
//! | `GrantSoul` | `State::exec_grant_soul` |
//! | `FlagSybil` | `State::exec_flag_sybil` |
//! | `UnflagSybil` | `State::exec_unflag_sybil` (the appeal path: lifts the flag, restores nothing) |
//! | `RejectClaim` | `State::exec_reject_claim` |
//! | `Clawback` | `State::exec_clawback` |
//! | `UnlinkGithub` | `State::exec_reclaim_github` (unlinks, then holds the handle for the proposer) |
//! | `RefundHunt` | `State::exec_refund_hunt` |
//! | `ScheduleUpgrade`, `CancelUpgrade`, `ActivateFeature`, `DeactivateFeature` | `upgrade::exec_schedule_upgrade`, `exec_cancel_upgrade`, `exec_activate_feature`, `exec_deactivate_feature` |
//! | `Text` | nothing |
//!
//! **Distributions** move `total` from the community account into an escrow claimable until
//! `claim_until` with Merkle proofs: BLAKE3 leaves over `(distribution_id, address, amount)`,
//! sorted-pair inner nodes ([`distribution_leaf`], [`merkle_parent`]). The proposer picks the id
//! its tree is built for, which no distribution may have yet (checked when proposed and when it
//! runs: the first proposal naming it to run opens it), so it doesn't depend on which proposals
//! land first, and a multisig's proposal survives its approval window (audit F45,
//! review-fix-gov-2). Ids stay small: at most [`DISTRIBUTION_ID_MARGIN`] past the proposal's own
//! id ([`max_distribution_id`], review-r2-gov-2), and `claim_until` at most
//! [`MAX_CLAIM_WINDOW_DAYS`] out. `/api/proposals` serves a free one, `next_distribution_id`.
//! What's unclaimed returns to the community account after `claim_until`. A claimant with
//! nothing to pay the fee with pays it (and its new account's creation fee) out of the claim
//! ([`claim_toward_fee`]).
//!
//! **Multisig (SF-11).** `MultisigCreate` makes an account M-of-N over its owner keys. From then
//! on its owner keys may only sign `MultisigPropose` and `MultisigApprove` (admission refuses
//! anything else, [`check_multisig_admission`]); an action runs, as the account, in the tx that
//! brings it to `threshold` approvals. Session keys keep working within their limits. Proposals
//! expire after `multisig_expiry_blocks`.

use crate::chain::{community, short, BlockInfo, Journal, KeyRecord, State};
use crate::crypto;
use crate::params::vote_weight;
use crate::poc::fmt_soul;
use crate::state::codec::{decode, encode, RecordKey};
use crate::state::{meta, ns, JMap, SubState, TMap, Table};
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::Serialize;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// Most open proposals one multisig account may have at once. Each owner key may have proposed at
/// most its share of them, ⌊32 / owner keys⌋ (at least 1), so one key can't fill the list and
/// keep the others from proposing (audit F14).
pub const MAX_MULTISIG_PENDING: usize = 32;
/// Largest action a `MultisigPropose` carries, encoded (`types::MAX_MULTISIG_ACTION_BYTES`, a
/// format rule): a nested validator registration fits, 32 KiB of evidence doesn't (audit F17).
pub use crate::types::MAX_MULTISIG_ACTION_BYTES;
/// Expired multisig proposals checked for removal per block (the sweep, [`end_block`]).
pub const MULTISIG_SWEEP: usize = 32;
/// Vote records of closed proposals removed per block (their tallies stay; audit F47).
pub const VOTE_SWEEP: usize = 256;
/// Treasury spends passed within this many days count together against `treasury_large_bps`
/// (audit F44).
pub const LARGE_SPEND_WINDOW_DAYS: u64 = 30;
/// A `Distribution` names an id at most this far past the next proposal id
/// ([`max_distribution_id`], review-r2-gov-2): room for trees built ahead (a multisig's, built
/// before its approval window), while ids stay small numbers every client holds exactly. The
/// bound only grows (proposal ids never go back), so a tree built for an id that was allowed
/// stays allowed.
pub const DISTRIBUTION_ID_MARGIN: u64 = 1_000;
/// A distribution's claims close at most this many days after it opens.
pub const MAX_CLAIM_WINDOW_DAYS: u64 = 730;
/// Domain separators of distribution Merkle trees (docs/GOVERNANCE.md).
pub const LEAF_DOMAIN: &[u8] = b"BLOCKULA_DISTRIBUTION_LEAF_V1";
pub const NODE_DOMAIN: &[u8] = b"BLOCKULA_DISTRIBUTION_NODE_V1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Taking votes through `voting_end`.
    Voting,
    /// Passed; runs at `execute_at` (the timelock).
    Queued,
    /// Didn't pass (`outcome` says why).
    Rejected,
    Executed,
    /// Passed, but its action failed when it ran (`outcome` says why); nothing changed.
    Failed,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Voting => "voting",
            ProposalStatus::Queued => "queued",
            ProposalStatus::Rejected => "rejected",
            ProposalStatus::Executed => "executed",
            ProposalStatus::Failed => "failed",
        }
    }
}

/// A validator's bootstrap-council vote, and the account whose `Vote` cast it.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CouncilVote {
    pub aye: bool,
    pub voter: String,
}

/// How a vote went, recorded when voting closes.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Tally {
    /// The unlocked supply (what could have voted, [`unlocked_supply`]) and the quorum it set.
    pub unlocked: u128,
    pub quorum: u128,
    pub quorum_reached: bool,
    /// Whether the ayes needed `supermajority_bps` of the weight.
    pub supermajority: bool,
    /// The council at the close (0 after the bootstrap): active-set size, ayes and nays in it.
    pub council_size: u64,
    pub council_ayes: u64,
    pub council_nays: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Proposal {
    pub id: u64,
    pub proposer: String,
    /// What it does. Accounts named by `.boo` name are resolved when it's proposed, so the
    /// account voters saw is the one it acts on.
    pub kind: ProposalKind,
    pub title: String,
    pub url: String,
    /// Escrowed while voting; refunded or burned when voting closes.
    pub deposit: u64,
    pub created_at: u64,
    /// Last block votes count in.
    pub voting_end: u64,
    /// When a passed proposal runs (`voting_end + timelock_blocks`; a `CancelUpgrade`,
    /// `voting_end + 1`).
    pub execute_at: Option<u64>,
    pub status: ProposalStatus,
    /// Raw FANG behind the ayes and nays (quorum counts these) …
    pub aye_amount: u128,
    pub nay_amount: u128,
    /// … and their conviction weights (the majority counts these).
    pub aye_weight: u128,
    pub nay_weight: u128,
    pub voters: u64,
    /// Bootstrap council votes, by validator.
    pub council: BTreeMap<String, CouncilVote>,
    pub tally: Option<Tally>,
    /// Why it was rejected or failed, or what it did.
    pub outcome: String,
}

/// One account's vote on one proposal.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct VoteRecord {
    pub aye: bool,
    pub amount: u64,
    pub conviction: u8,
    pub weight: u128,
    /// The vote lock (`vote:<proposal>`): `lock_amount` stays locked while the height is below
    /// `locked_until`. Re-votes only ever grow it.
    pub lock_amount: u64,
    pub locked_until: u64,
    /// The validator this vote also counted for in the bootstrap council.
    pub council: Option<String>,
    pub at: u64,
}

/// `(id, account)` record keys: votes by (proposal, voter), distribution claims by
/// (distribution, claimant). Key bytes: the id big-endian, then the account, so they sort like
/// the pair.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdKey {
    pub id: u64,
    pub account: String,
}

impl IdKey {
    pub fn new(id: u64, account: &str) -> Self {
        Self { id, account: account.to_string() }
    }
}

impl RecordKey for IdKey {
    fn key_bytes(&self) -> Cow<'_, [u8]> {
        let mut b = self.id.to_be_bytes().to_vec();
        b.extend_from_slice(self.account.as_bytes());
        Cow::Owned(b)
    }
    fn from_key_bytes(b: &[u8]) -> Result<Self> {
        ensure!(b.len() >= 8, "record key too short");
        let (id, account) = b.split_at(8);
        Ok(Self { id: u64::from_be_bytes(id.try_into().expect("8 bytes")), account: String::from_key_bytes(account)? })
    }
}

/// An M-of-N account. Its owners are its owner keys, whatever they are at the time.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Multisig {
    pub threshold: u8,
    pub created_at: u64,
    /// Open proposals (ids), at most [`MAX_MULTISIG_PENDING`].
    pub pending: BTreeSet<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MultisigProposal {
    pub id: u64,
    pub account: String,
    pub action: Action,
    /// Key ids: the proposer's first.
    pub proposed_by: String,
    pub approvals: BTreeSet<String>,
    pub created_at: u64,
    /// Last block it can be approved in.
    pub expires_at: u64,
}

/// A community distribution, under the id its proposal named.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Distribution {
    pub id: u64,
    /// The proposal that opened it.
    pub proposal: u64,
    /// Hex BLAKE3 root of the tree of `(id, address, amount)` leaves.
    pub merkle_root: String,
    pub total: u64,
    pub claimed: u64,
    pub claims: u64,
    pub opened_at: u64,
    /// Last block a claim can land in.
    pub claim_until: u64,
    /// When the rest went back to the community account, and how much.
    pub closed_at: Option<u64>,
    pub returned: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DistributionClaim {
    pub amount: u64,
    pub at: u64,
}

/// Governance's slice of the state (namespaces `state::ns` 37–42, META key `governance`).
#[derive(Clone, Debug, Default)]
pub struct Governance {
    pub proposals: TMap<u64, Proposal>,
    pub votes: TMap<IdKey, VoteRecord>,
    pub multisigs: TMap<String, Multisig>,
    pub multisig_proposals: TMap<u64, MultisigProposal>,
    pub distributions: TMap<u64, Distribution>,
    pub distribution_claims: TMap<IdKey, DistributionClaim>,
    pub scalars: Scalars,
}

/// Governance's scalars (one META record).
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Scalars {
    pub next_proposal_id: u64,
    pub next_multisig_id: u64,
    /// Proposals voting or queued, and distributions still open: all `end_block` looks at.
    pub open_proposals: BTreeSet<u64>,
    pub open_distributions: BTreeSet<u64>,
    /// `(epoch, unlocked supply)`: quorums in an epoch use the unlocked supply as of the first
    /// vote that closed in it, however many proposals close.
    pub quorum_base: Option<(u64, u128)>,
    /// Treasury spends that passed within the last [`LARGE_SPEND_WINDOW_DAYS`]: proposal id →
    /// (the height it passed at, the amount). They count together against `treasury_large_bps`.
    pub recent_spends: BTreeMap<u64, (u64, u64)>,
    /// Closed proposals whose vote records are still being removed ([`VOTE_SWEEP`] a block).
    pub closed_votes: BTreeSet<u64>,
    /// The expired-multisig-proposal sweep goes on from this id ([`MULTISIG_SWEEP`] a block).
    pub multisig_cursor: u64,
}

impl std::ops::Deref for Governance {
    type Target = Scalars;
    fn deref(&self) -> &Scalars {
        &self.scalars
    }
}

impl std::ops::DerefMut for Governance {
    fn deref_mut(&mut self) -> &mut Scalars {
        &mut self.scalars
    }
}

impl SubState for Governance {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Governance { proposals, votes, multisigs, multisig_proposals, distributions, distribution_claims, scalars: _ } = self;
        vec![(ns::GOV_PROPOSALS, proposals), (ns::GOV_VOTES, votes), (ns::GOV_MULTISIGS, multisigs), (ns::GOV_MULTISIG_PROPOSALS, multisig_proposals), (ns::GOV_DISTRIBUTIONS, distributions), (ns::GOV_DISTRIBUTION_CLAIMS, distribution_claims)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Governance { proposals, votes, multisigs, multisig_proposals, distributions, distribution_claims, scalars: _ } = self;
        vec![(ns::GOV_PROPOSALS, proposals), (ns::GOV_VOTES, votes), (ns::GOV_MULTISIGS, multisigs), (ns::GOV_MULTISIG_PROPOSALS, multisig_proposals), (ns::GOV_DISTRIBUTIONS, distributions), (ns::GOV_DISTRIBUTION_CLAIMS, distribution_claims)]
    }

    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        vec![(meta::GOVERNANCE, encode(&self.scalars))]
    }

    fn load_meta(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        if key != meta::GOVERNANCE {
            return Ok(false);
        }
        self.scalars = decode::<Scalars>(value)?;
        Ok(true)
    }
}

/// Undo log for [`Governance`] (part of `chain::Journal`).
#[derive(Default)]
pub struct GovernanceJournal {
    proposals: JMap<u64, Proposal>,
    votes: JMap<IdKey, VoteRecord>,
    multisigs: JMap<String, Multisig>,
    multisig_proposals: JMap<u64, MultisigProposal>,
    distributions: JMap<u64, Distribution>,
    distribution_claims: JMap<IdKey, DistributionClaim>,
    scalars: Option<Scalars>,
}

impl GovernanceJournal {
    pub fn absorb(&mut self, other: GovernanceJournal) {
        let GovernanceJournal { proposals, votes, multisigs, multisig_proposals, distributions, distribution_claims, scalars } = other;
        self.proposals.absorb(proposals);
        self.votes.absorb(votes);
        self.multisigs.absorb(multisigs);
        self.multisig_proposals.absorb(multisig_proposals);
        self.distributions.absorb(distributions);
        self.distribution_claims.absorb(distribution_claims);
        if self.scalars.is_none() {
            self.scalars = scalars;
        }
    }

    pub fn rollback(self, g: &mut Governance) {
        let GovernanceJournal { proposals, votes, multisigs, multisig_proposals, distributions, distribution_claims, scalars } = self;
        proposals.restore(&mut g.proposals);
        votes.restore(&mut g.votes);
        multisigs.restore(&mut g.multisigs);
        multisig_proposals.restore(&mut g.multisig_proposals);
        distributions.restore(&mut g.distributions);
        distribution_claims.restore(&mut g.distribution_claims);
        if let Some(s) = scalars {
            g.scalars = s;
        }
    }
}

// Records store a proposal kind and a multisig action in their tx encoding (types.rs), which is
// canonical and strictly decoded, as a length-prefixed byte string.
impl BorshSerialize for ProposalKind {
    fn serialize<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.to_bytes(), w)
    }
}

impl BorshDeserialize for ProposalKind {
    fn deserialize_reader<R: std::io::Read>(r: &mut R) -> std::io::Result<Self> {
        ProposalKind::from_bytes(&Vec::<u8>::deserialize_reader(r)?).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }
}

impl BorshSerialize for Action {
    fn serialize<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.to_bytes(), w)
    }
}

impl BorshDeserialize for Action {
    fn deserialize_reader<R: std::io::Read>(r: &mut R) -> std::io::Result<Self> {
        Action::from_bytes(&Vec::<u8>::deserialize_reader(r)?).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }
}

// ---- journaled access

fn save_scalars(state: &State, j: &mut Journal) {
    if j.governance.scalars.is_none() {
        j.governance.scalars = Some(state.governance.scalars.clone());
    }
}

fn proposal_mut<'a>(state: &'a mut State, j: &mut Journal, id: u64) -> &'a mut Proposal {
    j.governance.proposals.touch(&state.governance.proposals, &id);
    state.governance.proposals.get_mut(&id).expect("the caller checked the proposal exists")
}

fn put_multisig(state: &mut State, j: &mut Journal, account: &str, m: Multisig) {
    j.governance.multisigs.touch(&state.governance.multisigs, account);
    state.governance.multisigs.insert(account.to_string(), m);
}

fn put_multisig_proposal(state: &mut State, j: &mut Journal, p: Option<MultisigProposal>, id: u64) {
    j.governance.multisig_proposals.touch(&state.governance.multisig_proposals, &id);
    match p {
        Some(p) => state.governance.multisig_proposals.insert(id, p),
        None => state.governance.multisig_proposals.remove(&id),
    };
}

fn owner_only(key: &KeyRecord, what: &str) -> Result<()> {
    if key.role != KeyRole::Owner {
        bail!("only owner keys can {what}");
    }
    Ok(())
}

/// The key ids of an account's owner keys.
pub fn owner_keys(state: &State, account: &str) -> BTreeSet<String> {
    state.keys_of(account).into_iter().filter(|(_, k)| k.role == KeyRole::Owner).map(|(id, _)| id).collect()
}

/// Tags 39–44 and 48: Propose, Vote, Unlock, MultisigCreate, MultisigPropose, MultisigApprove,
/// ClaimDistribution.
pub(crate) fn apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, key: &KeyRecord, height: u64) -> Result<()> {
    match &tx.action {
        Action::Propose { kind, deposit, title, url } => propose(state, j, sender, key, kind, *deposit, title, url, height),
        Action::Vote { proposal_id, aye, amount, conviction, proposal_digest } => vote(state, j, sender, signer, key, *proposal_id, *aye, *amount, *conviction, proposal_digest, height),
        Action::Unlock {} => {
            owner_only(key, "unlock votes")?;
            if crate::econ::release_expired(state, j, sender, height) == 0 {
                bail!("no expired vote locks to release");
            }
            Ok(())
        }
        Action::MultisigCreate { owners, threshold } => multisig_create(state, j, sender, key, owners, *threshold, height),
        Action::MultisigPropose { action } => multisig_propose(state, j, tx, sender, signer, key, action, height),
        Action::MultisigApprove { id, action_digest } => multisig_approve(state, j, tx, sender, signer, key, *id, action_digest, height),
        Action::ClaimDistribution { distribution_id, amount, proof } => claim_distribution(state, j, sender, *distribution_id, *amount, proof, height),
        _ => bail!("not a governance action"),
    }
}

// ---- proposals and votes

/// Spending proposals are refused before `governance_spend_height` (3.5).
fn spend_gate(state: &State, height: u64) -> Result<()> {
    let open = state.chain_params.governance_spend_height;
    if height < open {
        bail!("spending proposals open at block {open} (governance_spend_height)");
    }
    Ok(())
}

/// The kind as it will be stored: account names resolved to addresses, and what can already be
/// known to fail refused (it's checked again when it runs).
fn prepare_kind(state: &State, kind: &ProposalKind, height: u64) -> Result<ProposalKind> {
    let cp = &state.chain_params;
    let runs_at = height.saturating_add(cp.voting_blocks).saturating_add(cp.timelock_blocks);
    let mut k = kind.clone();
    match &mut k {
        ProposalKind::ParamChange { key, value } => {
            cp.check_governance_change(key, *value, &state.params, height)?;
            cp.clone().set(key, *value, &state.params)?;
        }
        ProposalKind::TreasurySpend { to, .. } => {
            spend_gate(state, height)?;
            *to = state.resolve(to)?;
        }
        ProposalKind::Distribution { distribution_id, claim_until, .. } => {
            spend_gate(state, height)?;
            // Its tree's leaves bind the id it was built for, which it opens under: one no
            // distribution has (audit F45, review-fix-gov-2), and a small one (review-r2-gov-2).
            // Checked again when it runs.
            distribution_id_free(state, *distribution_id)?;
            distribution_id_in_range(state, *distribution_id)?;
            if *claim_until <= runs_at {
                bail!("claim_until must be after the vote and the timelock (block {runs_at})");
            }
            claim_window(state, *claim_until, runs_at)?;
        }
        ProposalKind::GrantSoul { to, .. } => *to = state.resolve(to)?,
        ProposalKind::FlagSybil { account, .. } | ProposalKind::Clawback { account, .. } => *account = state.resolve(account)?,
        ProposalKind::UnflagSybil { account, .. } => {
            *account = state.resolve(account)?;
            if !state.is_sybil(account) {
                bail!("{} isn't flagged as a sybil", short(account));
            }
        }
        ProposalKind::RejectClaim { claim_id, .. } => {
            if !state.poc.claims.contains_key(claim_id) {
                bail!("no claim #{claim_id}");
            }
        }
        ProposalKind::UnlinkGithub { handle } => {
            if !state.github.contains_key(&handle.to_ascii_lowercase()) {
                bail!("@{handle} isn't linked");
            }
        }
        ProposalKind::RefundHunt { hunt_id } => match state.hunts.get(hunt_id) {
            Some(h) if h.status == crate::chain::HuntStatus::Open => {}
            Some(h) => bail!("hunt #{hunt_id} is already {:?}", h.status),
            None => bail!("no hunt #{hunt_id}"),
        },
        ProposalKind::ScheduleUpgrade { version, activation_height } => {
            // What it could run into at the earliest block it runs in (audit F46); checked again then.
            crate::upgrade::check_schedule_upgrade(state, *version, *activation_height, runs_at).map_err(|e| anyhow!("{e} (the vote and the timelock end at block {runs_at})"))?;
            // Late enough that a `CancelUpgrade` proposed the block after this one runs still
            // lands (its vote, no timelock) `MIN_UPGRADE_LEAD` blocks before it (audit F50).
            let lead = crate::upgrade::MIN_UPGRADE_LEAD;
            let earliest = runs_at.saturating_add(cp.voting_blocks).saturating_add(lead).saturating_add(2);
            if *activation_height < earliest {
                bail!("activation_height must leave room for the vote and the timelock (it would run at block {runs_at}), then for a CancelUpgrade's vote and {lead} blocks: block {earliest} or later");
            }
        }
        ProposalKind::CancelUpgrade { version, activation_height } => {
            // Only an upgrade that's scheduled, and only if the cancellation runs (the block after
            // its vote closes: there's no timelock) at least `MIN_UPGRADE_LEAD` blocks before it.
            if let Err(e) = crate::upgrade::scheduled(state, *version, *activation_height) {
                let open = state.governance.open_proposals.iter().filter_map(|id| state.governance.proposals.get(id));
                let pending = open.filter(|p| p.kind == (ProposalKind::ScheduleUpgrade { version: *version, activation_height: *activation_height })).map(|p| (p.id, p.execute_at)).next();
                match pending {
                    Some((id, Some(at))) => bail!("v{version} at block {activation_height} isn't scheduled yet: proposal #{id} schedules it at block {at}; propose the cancellation after that"),
                    Some((id, None)) => bail!("v{version} at block {activation_height} isn't scheduled: proposal #{id}, which would schedule it, is still being voted on (vote nay on it)"),
                    None => return Err(e),
                }
            }
            let lead = crate::upgrade::MIN_UPGRADE_LEAD;
            let lands_at = height.saturating_add(cp.voting_blocks).saturating_add(1);
            if lands_at.saturating_add(lead) > *activation_height {
                bail!("v{version} activates at block {activation_height}: a cancellation proposed now would run at block {lands_at}, less than {lead} blocks before it. Stop it operationally instead (OPERATIONS.md \"Upgrades\")");
            }
        }
        ProposalKind::ActivateFeature { feature } | ProposalKind::DeactivateFeature { feature } => {
            if !crate::upgrade::KNOWN_FEATURES.contains(&feature.as_str()) {
                bail!("unknown feature {feature} (known: {})", crate::upgrade::KNOWN_FEATURES.join(", "));
            }
        }
        ProposalKind::SetRepo { .. } | ProposalKind::Text {} => {}
    }
    Ok(k)
}

#[allow(clippy::too_many_arguments)]
fn propose(state: &mut State, j: &mut Journal, sender: &str, key: &KeyRecord, kind: &ProposalKind, deposit: u64, title: &str, url: &str, height: u64) -> Result<()> {
    owner_only(key, "make proposals")?;
    let min = state.chain_params.proposal_deposit;
    if deposit < min {
        bail!("a proposal bonds at least {}", fmt_fang(min));
    }
    let kind = prepare_kind(state, kind, height)?;
    state.j_debit(j, sender, deposit)?;
    save_scalars(state, j);
    let g = &mut state.governance;
    let id = g.next_proposal_id;
    g.next_proposal_id = id.checked_add(1).ok_or_else(|| anyhow!("proposal ids exhausted"))?;
    g.open_proposals.insert(id);
    let voting_end = height.saturating_add(state.chain_params.voting_blocks);
    let p = Proposal { id, proposer: sender.to_string(), kind, title: title.to_string(), url: url.to_string(), deposit, created_at: height, voting_end, execute_at: None, status: ProposalStatus::Voting, aye_amount: 0, nay_amount: 0, aye_weight: 0, nay_weight: 0, voters: 0, council: BTreeMap::new(), tally: None, outcome: String::new() };
    j.governance.proposals.touch(&state.governance.proposals, &id);
    state.governance.proposals.insert(id, p);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// `proposal_digest` of a stored proposal: what a vote on it binds.
pub fn digest_of(p: &Proposal) -> String {
    crate::types::proposal_digest(&p.kind, &p.title, &p.url)
}

#[allow(clippy::too_many_arguments)]
fn vote(state: &mut State, j: &mut Journal, sender: &str, signer: &str, key: &KeyRecord, id: u64, aye: bool, amount: u64, conviction: u8, digest: &str, height: u64) -> Result<()> {
    owner_only(key, "vote")?;
    let p = state.governance.proposals.get(&id).ok_or_else(|| anyhow!("no proposal #{id}"))?;
    if p.status != ProposalStatus::Voting || height > p.voting_end {
        bail!("voting on proposal #{id} closed at block {}", p.voting_end);
    }
    // The vote is for the content its signer was shown (audit gap-7-6).
    if digest_of(p) != digest {
        bail!("proposal #{id} isn't the proposal this vote was signed for (its kind, title or link differ): the node it was read from may be misreporting it");
    }
    let voting_end = p.voting_end;
    let cp = &state.chain_params;
    let (weight, lock_blocks) = vote_weight(amount, conviction).zip(cp.vote_lock_blocks(conviction)).ok_or_else(|| anyhow!("conviction must be 0–{}", crate::params::MAX_CONVICTION))?;
    let available = crate::econ::voting_balance(state, sender, height);
    if amount > available {
        bail!("you can vote with up to {} (liquid, staked and unbonding FANG; unvested tokens don't vote)", fmt_fang(available));
    }
    // Locked through every block a vote can still land in, then the conviction's lock periods.
    let until = voting_end.saturating_add(1).saturating_add(lock_blocks);
    let vk = IdKey::new(id, sender);
    let prev = state.governance.votes.get(&vk).cloned();
    let (lock_amount, locked_until) = match &prev {
        Some(v) => (v.lock_amount.max(amount), v.locked_until.max(until)),
        None => (amount, until),
    };
    let council = if height < cp.bootstrap_end_height { crate::staking::validator_for_signer(state, sender, signer, state.params.epoch_of(height)) } else { None };
    crate::econ::lock(state, j, sender, format!("vote:{id}"), lock_amount, locked_until, height)?;
    let p = proposal_mut(state, j, id);
    if let Some(v) = &prev {
        let (amt, w) = if v.aye { (&mut p.aye_amount, &mut p.aye_weight) } else { (&mut p.nay_amount, &mut p.nay_weight) };
        *amt -= v.amount as u128;
        *w -= v.weight;
        p.voters -= 1;
        if let Some(val) = &v.council
            && p.council.get(val).is_some_and(|c| c.voter == sender)
        {
            p.council.remove(val);
        }
    }
    let (amt, w) = if aye { (&mut p.aye_amount, &mut p.aye_weight) } else { (&mut p.nay_amount, &mut p.nay_weight) };
    *amt += amount as u128;
    *w += weight;
    p.voters += 1;
    if let Some(val) = &council {
        p.council.insert(val.clone(), CouncilVote { aye, voter: sender.to_string() });
    }
    j.governance.votes.touch(&state.governance.votes, &vk);
    state.governance.votes.insert(vk, VoteRecord { aye, amount, conviction, weight, lock_amount, locked_until, council, at: height });
    Ok(())
}

/// The keyless system accounts, which never vote (only the bootstrap delegation stakes, by
/// formula: `staking::bootstrap_epoch`).
fn system_accounts(prefix: &str) -> [String; 5] {
    [crate::chain::treasury(prefix), community(prefix), crate::chain::endowment(prefix), crate::chain::bootstrap_delegation(prefix), crate::poc::pool_address(prefix)]
}

/// What could vote at `height` (the quorum base): the FANG every account but the keyless system
/// ones holds, liquid, in stake pools (at the pools' value) or unbonding, less what vesting
/// accounts can't vote with yet ([`still_vesting`]): the sum of every account's
/// `econ::voting_balance`. O(vesting accounts + their and the system accounts' delegations), not
/// O(accounts): balances, pool stake and unbonding are running totals their maps keep
/// (`chain::AccountTotals`, `staking::PoolStake`, `staking::UnbondingTotal`), and only genesis
/// allocations carry vesting locks. `check_invariants` compares it with
/// [`unlocked_supply_by_walk`].
pub fn unlocked_supply(state: &State, height: u64) -> u128 {
    let system = system_accounts(state.params.prefix());
    let system_balances: u128 = system.iter().filter_map(|a| state.accounts.get(a)).map(|a| a.balance as u128).sum();
    let system_stake: u128 = system.iter().map(|a| crate::staking::holdings(state, a)).map(|(b, u)| b as u128 + u as u128).sum();
    let held = state.accounts.total() - system_balances + state.staking.pools.total() + state.staking.entries.total() - system_stake;
    let vesting: u128 = state.accounts.indexed().iter().filter(|a| !system.contains(*a)).map(|a| still_vesting(state, a, height)).sum();
    held - vesting
}

/// The part of a still-vesting `account`'s holdings (stake at the pools' value) that can't vote
/// yet: all of it but `econ::voting_balance`, which leaves out what the vesting locks still hold
/// and what its locked stake earned and hasn't sold (it vests with them).
fn still_vesting(state: &State, account: &str, height: u64) -> u128 {
    let Some(a) = state.accounts.get(account) else { return 0 };
    if a.unvested_at(height) == 0 {
        return 0;
    }
    let (staked, unbonding) = crate::staking::holdings(state, account);
    let held = a.balance as u128 + staked as u128 + unbonding as u128;
    held - crate::econ::voting_balance(state, account, height) as u128
}

/// [`unlocked_supply`] from scratch, walking every account, pool and entry: O(state), for checks.
pub fn unlocked_supply_by_walk(state: &State, height: u64) -> u128 {
    let system = system_accounts(state.params.prefix());
    let accounts = state.accounts.iter().filter(|(a, _)| !system.contains(*a));
    let liquid: u128 = accounts.clone().map(|(_, acct)| acct.balance as u128).sum();
    let vesting: u128 = accounts.map(|(a, _)| still_vesting(state, a, height)).sum();
    let staked: u128 = state.staking.pools.values().map(|p| p.stake as u128).sum();
    let unbonding: u128 = state.staking.entries.values().map(|e| if let crate::staking::Entry::Unbonding { amount, .. } = e { *amount as u128 } else { 0 }).sum();
    // The bootstrap delegation's stake, walked delegation by delegation.
    let system_of = |k: &str| k.split_once('/').is_some_and(|(d, _)| system.iter().any(|s| s == d));
    let system_staked: u128 = state.staking.delegations.iter().filter(|(k, _)| system_of(k)).filter_map(|(k, d)| Some(state.staking.pools.get(k.split_once('/')?.1)?.value(d.shares) as u128)).sum();
    let system_unbonding: u128 = state.staking.entries.iter().filter(|(k, _)| system_of(k)).map(|(_, e)| if let crate::staking::Entry::Unbonding { amount, .. } = e { *amount as u128 } else { 0 }).sum();
    liquid + staked + unbonding - system_staked - system_unbonding - vesting
}

/// Whether a kind needs `supermajority_bps` of the weight: upgrades and their cancellations (a
/// simple majority can't undo what a supermajority scheduled), feature activations
/// (security-critical switches such as Ghost Mode), the parameters that are the token holders'
/// alone (`params::tokens_only`: governance's own safety parameters and the validators' own
/// economics, issuance among them), and treasury spends above `treasury_large_bps` of the
/// treasury's balance. Turning a feature off takes a simple majority.
pub fn needs_supermajority(state: &State, kind: &ProposalKind) -> bool {
    match kind {
        ProposalKind::ScheduleUpgrade { .. } | ProposalKind::CancelUpgrade { .. } | ProposalKind::ActivateFeature { .. } => true,
        ProposalKind::ParamChange { key, .. } => crate::params::tokens_only(key),
        ProposalKind::TreasurySpend { amount, .. } => {
            let treasury = state.account(&state.treasury()).balance as u128;
            (*amount as u128 + recent_spends(state, state.econ.height)) * 10_000 > state.chain_params.treasury_large_bps as u128 * treasury
        }
        _ => false,
    }
}

/// [`LARGE_SPEND_WINDOW_DAYS`] in blocks.
fn large_spend_window(state: &State) -> u64 {
    LARGE_SPEND_WINDOW_DAYS.saturating_mul(crate::params::blocks_per_day(state.params.block_time_ms))
}

/// What the treasury spends that passed in the window up to `height` add up to (those that
/// failed when they ran aside): a spend is large if it and they together are more than
/// `treasury_large_bps` of the treasury, so a large spend split into small proposals still needs
/// the supermajority (audit F44).
pub fn recent_spends(state: &State, height: u64) -> u128 {
    let window = large_spend_window(state);
    state.governance.recent_spends.values().filter(|(at, _)| at.saturating_add(window) > height).map(|(_, amount)| *amount as u128).sum()
}

/// Whether the bootstrap council may pass a kind on its own: anything but a change to a
/// parameter that's the token holders' alone (`params::tokens_only`): governance's own safety
/// parameters (`params::GOVERNANCE_KEYS`), so the council can't extend itself, open spending
/// early or weaken the vote that checks it (audit F15, F16), and the validators' own economics
/// (`params::VALIDATOR_KEYS`: issuance, the tip cap, commission bounds, the bootstrap
/// delegation's commission cap, the self-bond, slashing, jail and downtime), since the council is
/// the validators (audit F01, F12, review-integrate-merges-1).
/// Upgrades and feature switches stay the council's to pass in year 1.
pub fn council_may_pass(kind: &ProposalKind) -> bool {
    !matches!(kind, ProposalKind::ParamChange { key, .. } if crate::params::tokens_only(key))
}

/// Closes voting on proposal `id` at `height`: tally, settle the deposit, queue or reject.
fn close_vote(state: &mut State, id: u64, height: u64, unlocked: u128) {
    let mut j = Journal::default();
    let p = state.governance.proposals[&id].clone();
    let cp = state.chain_params.clone();
    let quorum = (cp.quorum_floor as u128).max(unlocked * cp.quorum_bps as u128 / 10_000);
    let turnout = p.aye_amount + p.nay_amount;
    let quorum_reached = turnout > 0 && turnout >= quorum;
    let supermajority = needs_supermajority(state, &p.kind);
    let majority = if supermajority { p.aye_weight > 0 && p.aye_weight * 10_000 >= cp.supermajority_bps as u128 * (p.aye_weight + p.nay_weight) } else { p.aye_weight > p.nay_weight };
    let by_tokens = quorum_reached && majority;
    // The bootstrap council: ⅔ of the active set at the close approve, and token nays don't
    // outweigh token ayes.
    let set: BTreeSet<String> = if height < cp.bootstrap_end_height { state.active_sets.get(&state.params.epoch_of(height)).map(|s| s.iter().cloned().collect()).unwrap_or_default() } else { BTreeSet::new() };
    let n = set.len() as u64;
    let in_set = p.council.iter().filter(|(v, _)| set.contains(*v));
    let (ayes, nays) = in_set.fold((0u64, 0u64), |(a, b), (_, c)| if c.aye { (a + 1, b) } else { (a, b + 1) });
    let by_council = n > 0 && ayes * 3 >= 2 * n && p.nay_weight <= p.aye_weight && council_may_pass(&p.kind);
    let council_quorum = n > 0 && (ayes + nays) * 3 >= 2 * n;
    // Deposit: back if the proposal drew a quorum (of tokens, or of the council), burned if not.
    if quorum_reached || council_quorum {
        state.j_credit(&mut j, &p.proposer, p.deposit);
    } else {
        state.j_burn(&mut j, p.deposit);
    }
    let passed = by_tokens || by_council;
    let runs_at = p.voting_end.saturating_add(timelock(&cp, &p.kind)).max(height + 1);
    // Why it didn't pass: the token vote's majority if it reached quorum, else why the council's
    // ⅔ (which did vote, or the deposit is burned) didn't carry it.
    let outcome = if passed {
        let at = runs_at;
        let how = if by_tokens { "by token vote" } else { "by the bootstrap council" };
        format!("passed {how}; runs at block {at}")
    } else if !quorum_reached && !council_quorum {
        format!("no quorum: {} of {} needed voted; deposit burned", fmt_fang(clamp(turnout)), fmt_fang(clamp(quorum)))
    } else if quorum_reached && supermajority {
        format!("fewer than {}% of the vote weight said aye", crate::staking::fmt_bps(cp.supermajority_bps))
    } else if quorum_reached {
        "the ayes didn't outweigh the nays".to_string()
    } else {
        let no_quorum = format!("no token quorum ({} of {} needed voted)", fmt_fang(clamp(turnout)), fmt_fang(clamp(quorum)));
        match &p.kind {
            ProposalKind::ParamChange { key, .. } if !council_may_pass(&p.kind) => format!("{no_quorum}, and the bootstrap council can't pass a change to {key}: it's the token holders' alone"),
            _ if p.nay_weight > p.aye_weight => format!("{no_quorum}, and the token nays outweighed the ayes, which the bootstrap council can't overrule"),
            _ => format!("{no_quorum}, and {ayes} of the council's {n} said aye (⅔ needed)"),
        }
    };
    save_scalars(state, &mut j);
    // Spends that passed count toward the next ones' share of the treasury for the window; spends
    // that left it don't.
    let window = large_spend_window(state);
    state.governance.recent_spends.retain(|_, (at, _)| at.saturating_add(window) > height);
    if let (true, ProposalKind::TreasurySpend { amount, .. }) = (passed, &p.kind) {
        state.governance.recent_spends.insert(id, (height, *amount));
    }
    // The tally is final: the votes behind it go, a batch a block (`end_block`).
    if p.voters > 0 {
        state.governance.closed_votes.insert(id);
    }
    let p = proposal_mut(state, &mut j, id);
    p.tally = Some(Tally { unlocked, quorum, quorum_reached, supermajority, council_size: n, council_ayes: ayes, council_nays: nays });
    p.outcome = outcome;
    if passed {
        p.status = ProposalStatus::Queued;
        p.execute_at = Some(runs_at);
    } else {
        p.status = ProposalStatus::Rejected;
        state.governance.open_proposals.remove(&id);
    }
}

/// Blocks a passed proposal waits before it runs: `timelock_blocks`, so whoever disagrees can act
/// before it takes effect, except for a `CancelUpgrade`, which keeps the rules in force and has
/// to land before the upgrade it stops.
fn timelock(cp: &crate::params::ChainParams, kind: &ProposalKind) -> u64 {
    match kind {
        ProposalKind::CancelUpgrade { .. } => 0,
        _ => cp.timelock_blocks,
    }
}

fn clamp(v: u128) -> u64 {
    v.min(u64::MAX as u128) as u64
}

/// Runs a queued proposal's action. On error nothing it did stays.
fn execute(state: &mut State, id: u64, height: u64) {
    let p = state.governance.proposals[&id].clone();
    let mut j = Journal::default();
    let result = run_kind(state, &mut j, &p, height);
    let (status, outcome) = match result {
        Ok(what) => (ProposalStatus::Executed, what),
        Err(e) => {
            state.rollback(j);
            (ProposalStatus::Failed, format!("failed when it ran: {e}"))
        }
    };
    let mut j = Journal::default();
    save_scalars(state, &mut j);
    if status == ProposalStatus::Failed {
        state.governance.recent_spends.remove(&id);
    }
    let p = proposal_mut(state, &mut j, id);
    p.status = status;
    p.outcome = outcome;
    state.governance.open_proposals.remove(&id);
}

/// A passed proposal's action (the levers have no signer checks: the vote was the check).
/// Returns what it did, for `Proposal::outcome` (accounts shortened: names are looked up by
/// readers, not scanned for in end-of-block code).
fn run_kind(state: &mut State, j: &mut Journal, p: &Proposal, height: u64) -> Result<String> {
    Ok(match &p.kind {
        ProposalKind::ParamChange { key, value } => {
            state.exec_set_param(j, key, *value)?;
            format!("{key} is now {value}")
        }
        ProposalKind::TreasurySpend { to, amount } => {
            spend_gate(state, height)?;
            let treasury = state.treasury();
            let available = crate::econ::transferable(state, &treasury, height);
            if *amount > available {
                bail!("the treasury can spend {} now", fmt_fang(available));
            }
            state.j_debit(j, &treasury, *amount)?;
            state.j_credit(j, to, *amount);
            format!("paid {} to {}", fmt_fang(*amount), short(to))
        }
        ProposalKind::Distribution { distribution_id, merkle_root, total, claim_until } => {
            let id = *distribution_id;
            distribution_id_free(state, id)?;
            distribution_id_in_range(state, id)?;
            spend_gate(state, height)?;
            if *claim_until < height {
                bail!("its claim window ended at block {claim_until}");
            }
            claim_window(state, *claim_until, height)?;
            let from = community(state.params.prefix());
            let available = crate::econ::transferable(state, &from, height);
            if *total > available {
                bail!("the community account holds {} it can move", fmt_fang(available));
            }
            state.j_debit(j, &from, *total)?;
            save_scalars(state, j);
            state.governance.open_distributions.insert(id);
            j.governance.distributions.touch(&state.governance.distributions, &id);
            state.governance.distributions.insert(id, Distribution { id, proposal: p.id, merkle_root: merkle_root.clone(), total: *total, claimed: 0, claims: 0, opened_at: height, claim_until: *claim_until, closed_at: None, returned: 0 });
            format!("opened distribution #{id} of {}, claimable until block {claim_until}", fmt_fang(*total))
        }
        ProposalKind::SetRepo { repo, weight } => {
            state.exec_set_repo(j, repo, *weight, height)?;
            if *weight == 0 { format!("removed {repo} from the pool") } else { format!("{repo} has weight {weight}") }
        }
        ProposalKind::GrantSoul { to, amount, .. } => {
            state.exec_grant_soul(j, to, *amount, height)?;
            format!("granted {} {} Soul", short(to), fmt_soul(*amount))
        }
        ProposalKind::FlagSybil { account, reason } => {
            state.exec_flag_sybil(j, account, &format!("proposal #{}", p.id), reason, height)?;
            format!("flagged {} as a sybil", short(account))
        }
        ProposalKind::RejectClaim { claim_id, reason } => {
            state.exec_reject_claim(j, *claim_id, reason, height)?;
            format!("rejected claim #{claim_id}")
        }
        ProposalKind::UnflagSybil { account, .. } => {
            state.exec_unflag_sybil(j, account)?;
            format!("lifted {}'s sybil flag", short(account))
        }
        ProposalKind::Clawback { account, .. } => {
            let back = state.exec_clawback(j, account, height)?;
            format!("clawed back {} from {}", fmt_fang(back), short(account))
        }
        ProposalKind::UnlinkGithub { handle } => state.exec_reclaim_github(j, handle, &p.proposer, height)?,
        ProposalKind::RefundHunt { hunt_id } => {
            state.exec_refund_hunt(j, *hunt_id, height)?;
            format!("refunded hunt #{hunt_id}'s funders")
        }
        ProposalKind::ScheduleUpgrade { version, activation_height } => {
            crate::upgrade::check_not_superseded(state, p.created_at, *version)?;
            crate::upgrade::exec_schedule_upgrade(state, j, *version, *activation_height, height)?;
            format!("protocol v{version} activates at block {activation_height}")
        }
        ProposalKind::ActivateFeature { feature } => {
            crate::upgrade::exec_activate_feature(state, j, feature, height)?;
            format!("{feature} is on")
        }
        ProposalKind::DeactivateFeature { feature } => {
            crate::upgrade::exec_deactivate_feature(state, j, feature)?;
            format!("{feature} is off")
        }
        ProposalKind::CancelUpgrade { version, activation_height } => {
            // Every open upgrade proposal is superseded (each was made by now). Those that passed
            // and wait to run also spend their numbers (review-fix-ops-2): their releases may be
            // out, signalled, and built on the withdrawn rules. Still-voting ones don't, or
            // filing one would spend numbers for the price of a deposit.
            let open = open_upgrade_proposals(state);
            let passed = open.iter().filter(|(_, status, _, _)| *status == ProposalStatus::Queued).map(|(_, _, v, _)| *v).max().unwrap_or(0);
            let withdrawn = crate::upgrade::exec_cancel_upgrade(state, j, *version, *activation_height, height, passed)?;
            let after: Vec<String> = withdrawn[1..].iter().map(|u| format!("v{} at block {}", u.version, u.activation_height)).collect();
            let mut what = format!("cancelled the upgrade to protocol v{version} at block {activation_height}");
            if !after.is_empty() {
                what += &format!(", and {} scheduled after it", after.join(", "));
            }
            if !open.is_empty() {
                let list: Vec<String> = open.iter().map(|(id, _, v, at)| format!("#{id} (v{v} at block {at})")).collect();
                what += &format!("; it supersedes the upgrade proposal{} still open, {}, which can't take effect", if open.len() == 1 { "" } else { "s" }, list.join(", "));
            }
            // The number to ship the fixed release as: past the spent ones and the superseded
            // proposals' own (a release may be out for any of them).
            let spent = state.upgrade.withdrawals.highest;
            let next = open.iter().map(|(_, _, v, _)| *v).fold(spent, u32::max).saturating_add(1);
            what + &format!("; versions through v{spent} are spent: ship the next upgrade as v{next} or later")
        }
        ProposalKind::Text {} => "recorded (no on-chain effect)".into(),
    })
}

/// The open `ScheduleUpgrade` proposals (id, status, version, activation height): what a
/// cancellation running now supersedes (all of them: `upgrade::check_not_superseded`).
fn open_upgrade_proposals(state: &State) -> Vec<(u64, ProposalStatus, u32, u64)> {
    let g = &state.governance;
    g.open_proposals.iter().filter_map(|id| g.proposals.get(id)).filter_map(|p| match p.kind {
        ProposalKind::ScheduleUpgrade { version, activation_height } => Some((p.id, p.status, version, activation_height)),
        _ => None,
    }).collect()
}

/// After `staking::end_block`, every block: close voting on proposals whose vote ended, run
/// those whose timelock is over, and return what's unclaimed of distributions past
/// `claim_until` to the community account.
pub fn end_block(state: &mut State, b: &BlockInfo) {
    let height = b.height;
    for id in state.governance.open_proposals.clone() {
        let p = &state.governance.proposals[&id];
        match p.status {
            ProposalStatus::Voting if height >= p.voting_end => {
                let epoch = state.params.epoch_of(height);
                let u = match state.governance.quorum_base {
                    Some((e, u)) if e == epoch => u,
                    _ => {
                        let u = unlocked_supply(state, height);
                        state.governance.quorum_base = Some((epoch, u));
                        u
                    }
                };
                close_vote(state, id, height, u);
            }
            ProposalStatus::Queued if p.execute_at.is_some_and(|at| height >= at) => execute(state, id, height),
            _ => {}
        }
    }
    for id in state.governance.open_distributions.clone() {
        let d = &state.governance.distributions[&id];
        if height <= d.claim_until {
            continue;
        }
        let rest = d.total - d.claimed;
        let mut j = Journal::default();
        state.j_credit(&mut j, &community(state.params.prefix()), rest);
        save_scalars(state, &mut j);
        state.governance.open_distributions.remove(&id);
        let d = state.governance.distributions.get_mut(&id).expect("listed as open");
        d.closed_at = Some(height);
        d.returned = rest;
    }
    sweep_votes(state);
    sweep_multisig_proposals(state, height);
}

/// Removes up to [`VOTE_SWEEP`] vote records of closed proposals (oldest proposal first): once a
/// vote closes, its tally is final and each vote's lock lives on in the voter's account
/// (`vote:<id>`), so the records go (audit F47: they'd otherwise be permanent, unbonded state).
/// What stays per account is bounded by its locks (`MAX_LOCKS`, one per proposal still voting or
/// locking).
fn sweep_votes(state: &mut State) {
    let mut budget = VOTE_SWEEP;
    while let Some(id) = state.governance.closed_votes.first().copied() {
        // One more than the budget, to know whether any are left after it.
        let batch: Vec<IdKey> = state.governance.votes.range(IdKey::new(id, "")..).take_while(|(k, _)| k.id == id).take(budget + 1).map(|(k, _)| k.clone()).collect();
        let more = batch.len() > budget;
        for k in batch.into_iter().take(budget) {
            state.governance.votes.remove(&k);
            budget -= 1;
        }
        if more {
            return;
        }
        state.governance.closed_votes.remove(&id);
    }
}

/// Removes up to [`MULTISIG_SWEEP`] expired multisig proposals, resuming where the last sweep
/// stopped and starting over at the end (audit F17: an account that files proposals and goes
/// quiet doesn't keep them in state).
fn sweep_multisig_proposals(state: &mut State, height: u64) {
    let g = &state.governance;
    let batch: Vec<(u64, String, bool)> = g.multisig_proposals.range(g.multisig_cursor..).take(MULTISIG_SWEEP).map(|(id, p)| (*id, p.account.clone(), height > p.expires_at)).collect();
    state.governance.multisig_cursor = if batch.len() < MULTISIG_SWEEP { 0 } else { batch.last().map_or(0, |b| b.0 + 1) };
    let mut j = Journal::default();
    for (id, account, expired) in batch {
        if expired {
            drop_multisig_proposal(state, &mut j, &account, id);
        }
    }
}

/// Removes multisig proposal `id` of `account` (expired, or run).
fn drop_multisig_proposal(state: &mut State, j: &mut Journal, account: &str, id: u64) {
    if let Some(mut m) = state.governance.multisigs.get(account).cloned()
        && m.pending.remove(&id)
    {
        put_multisig(state, j, account, m);
    }
    put_multisig_proposal(state, j, None, id);
}

// ---- community distributions

/// A distribution id a `Distribution` proposal may name: no distribution has it yet. Open
/// proposals naming it don't count (the first to run takes it, and the others fail), so filing a
/// proposal can't take an id from one in a multisig's approval window.
fn distribution_id_free(state: &State, id: u64) -> Result<()> {
    if let Some(d) = state.governance.distributions.get(&id) {
        bail!("distribution #{id} already exists (proposal #{}): build the tree for another id (blockula gov tree; /api/proposals' next_distribution_id is free)", d.proposal);
    }
    Ok(())
}

/// The largest id a `Distribution` proposed now may name: [`DISTRIBUTION_ID_MARGIN`] past the
/// next proposal id (review-r2-gov-2). Proposal ids only grow, so this never shrinks, and
/// distribution ids stay far below 2^53 (JSON numbers every client reads exactly; the format
/// check caps them there too).
pub fn max_distribution_id(state: &State) -> u64 {
    state.governance.next_proposal_id.saturating_add(DISTRIBUTION_ID_MARGIN).min(crate::types::MAX_JSON_INT)
}

fn distribution_id_in_range(state: &State, id: u64) -> Result<()> {
    let max = max_distribution_id(state);
    if id > max {
        bail!("distribution ids go up to #{max} now ({DISTRIBUTION_ID_MARGIN} past the next proposal id): build the tree for /api/proposals' next_distribution_id (blockula gov tree)");
    }
    Ok(())
}

/// A claim deadline at most [`MAX_CLAIM_WINDOW_DAYS`] after `opens` (when it would open).
fn claim_window(state: &State, claim_until: u64, opens: u64) -> Result<()> {
    let latest = opens.saturating_add(MAX_CLAIM_WINDOW_DAYS.saturating_mul(crate::params::blocks_per_day(state.params.block_time_ms)));
    if claim_until > latest {
        bail!("claims close at most {MAX_CLAIM_WINDOW_DAYS} days after a distribution opens: claim_until must be block {latest} or earlier");
    }
    Ok(())
}

/// A distribution id for a new tree (what `/api/proposals` serves and `blockula gov tree` builds
/// for): the smallest at or above the number of distributions that no distribution, open
/// `Distribution` proposal or multisig's stored one names, within [`max_distribution_id`]
/// (review-r2-gov-2). An outlier id can't drag it anywhere, and it's never a taken or saturated
/// one. Should multisig proposals (cheap to file) crowd every free id out of range, it skips only
/// what really holds an id.
pub fn next_distribution_id(state: &State) -> u64 {
    let g = &state.governance;
    let mut taken: BTreeSet<u64> = g.distributions.keys().copied().collect();
    taken.extend(g.open_proposals.iter().filter_map(|id| g.proposals.get(id)).filter_map(|p| match p.kind {
        ProposalKind::Distribution { distribution_id, .. } => Some(distribution_id),
        _ => None,
    }));
    let mut wanted = taken.clone();
    wanted.extend(g.multisig_proposals.values().filter_map(|m| match &m.action {
        Action::Propose { kind: ProposalKind::Distribution { distribution_id, .. }, .. } => Some(*distribution_id),
        _ => None,
    }));
    let (start, max) = (g.distributions.len() as u64, max_distribution_id(state));
    // At most `max + 1` ids in range and at most `next_proposal_id` of them taken (each by its
    // own proposal), so a free one is always there.
    let first_free = |used: &BTreeSet<u64>| (start..=max).chain(0..start.min(max)).find(|id| !used.contains(id));
    first_free(&wanted).or_else(|| first_free(&taken)).unwrap_or(max)
}

/// A distribution tree's leaf: BLAKE3(`LEAF_DOMAIN` ‖ id u64 LE ‖ address length u16 LE ‖
/// address ‖ amount u64 LE). The address is the claimant's, as its tx's `from` spells it.
pub fn distribution_leaf(distribution_id: u64, address: &str, amount: u64) -> [u8; 32] {
    let mut b = LEAF_DOMAIN.to_vec();
    b.extend_from_slice(&distribution_id.to_le_bytes());
    b.extend_from_slice(&(address.len() as u16).to_le_bytes());
    b.extend_from_slice(address.as_bytes());
    b.extend_from_slice(&amount.to_le_bytes());
    crypto::hash(&b)
}

/// An inner node: BLAKE3(`NODE_DOMAIN` ‖ the smaller child ‖ the larger), so a proof is just
/// the siblings, with no positions.
pub fn merkle_parent(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut buf = NODE_DOMAIN.to_vec();
    buf.extend_from_slice(lo);
    buf.extend_from_slice(hi);
    crypto::hash(&buf)
}

/// The root a leaf and its proof (siblings from the leaf up) lead to.
pub fn root_from_proof(leaf: [u8; 32], proof: &[[u8; 32]]) -> [u8; 32] {
    proof.iter().fold(leaf, |acc, s| merkle_parent(&acc, s))
}

/// A distribution proof: sibling hashes from the leaf up.
pub type MerklePath = Vec<[u8; 32]>;

/// Builds a distribution tree (what proposers publish with a `Distribution`): the leaves sorted,
/// paired left to right, an odd last node carried up unchanged. Returns the root and each
/// entry's proof, in input order. Addresses must be distinct.
pub fn build_distribution(distribution_id: u64, entries: &[(String, u64)]) -> Result<([u8; 32], Vec<MerklePath>)> {
    ensure!(!entries.is_empty(), "a distribution needs at least one entry");
    let mut seen = BTreeSet::new();
    for (a, amount) in entries {
        ensure!(seen.insert(a.as_str()), "{a} is listed twice");
        ensure!(*amount > 0, "{a} gets 0");
    }
    let mut leaves: Vec<([u8; 32], usize)> = entries.iter().enumerate().map(|(i, (a, amt))| (distribution_leaf(distribution_id, a, *amt), i)).collect();
    leaves.sort();
    let mut level: Vec<[u8; 32]> = leaves.iter().map(|(h, _)| *h).collect();
    // Each entry's position in the current level.
    let mut pos: Vec<usize> = vec![0; entries.len()];
    for (p, (_, i)) in leaves.iter().enumerate() {
        pos[*i] = p;
    }
    let mut proofs = vec![vec![]; entries.len()];
    while level.len() > 1 {
        for (i, p) in pos.iter_mut().enumerate() {
            let sibling = *p ^ 1;
            if sibling < level.len() {
                proofs[i].push(level[sibling]);
            }
            *p /= 2;
        }
        level = level.chunks(2).map(|c| if c.len() == 2 { merkle_parent(&c[0], &c[1]) } else { c[0] }).collect();
    }
    Ok((level[0], proofs))
}

/// Everything that decides whether `sender` may claim `amount` from distribution `id` at
/// `height`: open, not claimed yet, the proof leads to the stored root, enough left.
fn check_claim(state: &State, sender: &str, id: u64, amount: u64, proof: &[String], height: u64) -> Result<()> {
    let d = state.governance.distributions.get(&id).ok_or_else(|| anyhow!("no distribution #{id}"))?;
    if d.closed_at.is_some() || height > d.claim_until {
        bail!("distribution #{id} closed at block {}", d.claim_until);
    }
    if state.governance.distribution_claims.contains_key(&IdKey::new(id, sender)) {
        bail!("{} already claimed from distribution #{id}", short(sender));
    }
    let siblings: Vec<[u8; 32]> = proof.iter().map(|h| hex::decode(h).ok().and_then(|b| b.try_into().ok())).collect::<Option<_>>().ok_or_else(|| anyhow!("proof hashes must be 32 bytes of hex"))?;
    if hex::encode(root_from_proof(distribution_leaf(id, sender, amount), &siblings)) != d.merkle_root {
        bail!("the proof doesn't lead to distribution #{id}'s Merkle root (wrong address, amount or proof)");
    }
    let left = d.total - d.claimed;
    if amount > left {
        bail!("distribution #{id} has only {} left", fmt_fang(left));
    }
    Ok(())
}

/// What a `ClaimDistribution` tx would credit its sender at `height`, if the claim checks out
/// ([`check_claim`]): admission counts it toward the fee of a claimant that can't pay it yet (a
/// new or empty account), and the fee phase then runs the claim first (`State::apply_j`). An
/// invalid claim is no help, so it stays unadmittable rather than free.
pub fn claim_toward_fee(state: &State, tx: &Tx, sender: &str, height: u64) -> Result<u64> {
    let Action::ClaimDistribution { distribution_id, amount, proof } = &tx.action else { bail!("only a distribution claim pays its fee out of what it claims") };
    check_claim(state, sender, *distribution_id, *amount, proof, height)?;
    Ok(*amount)
}

/// Runs a checked `ClaimDistribution` (the action, or the fee phase of a claim paying its own
/// fee).
pub(crate) fn claim_distribution(state: &mut State, j: &mut Journal, sender: &str, id: u64, amount: u64, proof: &[String], height: u64) -> Result<()> {
    check_claim(state, sender, id, amount, proof, height)?;
    let ck = IdKey::new(id, sender);
    j.governance.distributions.touch(&state.governance.distributions, &id);
    let d = state.governance.distributions.get_mut(&id).expect("checked above");
    d.claimed += amount;
    d.claims += 1;
    j.governance.distribution_claims.touch(&state.governance.distribution_claims, &ck);
    state.governance.distribution_claims.insert(ck, DistributionClaim { amount, at: height });
    state.j_credit(j, sender, amount);
    Ok(())
}

// ---- multisig accounts (SF-11)

/// Admission rule for multisig accounts (`State::admit`): their owner keys may only sign
/// `MultisigPropose` and `MultisigApprove`. Session keys are unaffected.
pub fn check_multisig_admission(state: &State, sender: &str, key: &KeyRecord, action: &Action) -> Result<()> {
    if key.role != KeyRole::Owner || matches!(action, Action::MultisigPropose { .. } | Action::MultisigApprove { .. }) {
        return Ok(());
    }
    if let Some(m) = state.governance.multisigs.get(sender) {
        bail!("{} is a {}-of-{} multisig: propose this with multisig_propose", short(sender), m.threshold, owner_keys(state, sender).len());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn multisig_create(state: &mut State, j: &mut Journal, sender: &str, key: &KeyRecord, owners: &[String], threshold: u8, height: u64) -> Result<()> {
    owner_only(key, "set up a multisig")?;
    let current = owner_keys(state, sender);
    if owners.iter().cloned().collect::<BTreeSet<_>>() != current {
        bail!("owners must be exactly this account's {} owner key(s); add or remove keys first", current.len());
    }
    if threshold == 0 || threshold as usize > current.len() {
        bail!("threshold must be 1–{}", current.len());
    }
    // A multisig account changes its threshold the same way, through its own approvals (admission
    // only lets this through as a multisig action then).
    let m = match state.governance.multisigs.get(sender) {
        Some(m) => Multisig { threshold, ..m.clone() },
        None => Multisig { threshold, created_at: height, pending: BTreeSet::new() },
    };
    put_multisig(state, j, sender, m);
    Ok(())
}

/// How many of a proposal's approvals are (still) owner keys.
fn approvals(state: &State, p: &MultisigProposal) -> usize {
    let owners = owner_keys(state, &p.account);
    p.approvals.iter().filter(|k| owners.contains(*k)).count()
}

/// Runs an approved action as the account, signed (for the action's own checks) by `signer`.
/// The account can't end up with fewer owner keys than its threshold.
fn run_multisig(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, p: &MultisigProposal, height: u64) -> Result<()> {
    let inner = Tx { action: p.action.clone(), ..tx.clone() };
    let key = state.keys_of(sender).get(signer).cloned().ok_or_else(|| anyhow!("this key isn't on {}", short(sender)))?;
    state.apply_action(j, &inner, sender, signer, &key, height).map_err(|e| anyhow!("multisig action #{}: {e}", p.id))?;
    if let Some(m) = state.governance.multisigs.get(sender) {
        let n = owner_keys(state, sender).len();
        ensure!(n >= m.threshold as usize, "multisig action #{} would leave {n} owner key(s), fewer than the threshold ({})", p.id, m.threshold);
    }
    Ok(())
}

/// Removes `account`'s expired multisig proposals.
fn clear_expired(state: &mut State, j: &mut Journal, account: &str, height: u64) {
    let Some(m) = state.governance.multisigs.get(account) else { return };
    let expired: Vec<u64> = m.pending.iter().copied().filter(|id| state.governance.multisig_proposals.get(id).is_none_or(|p| height > p.expires_at)).collect();
    for id in expired {
        drop_multisig_proposal(state, j, account, id);
    }
}

/// Whether `tx`, included in the block at `height`, would run its action rather than store it
/// for approvals: a multisig action runs when its proposer's approval (`MultisigPropose`) or this
/// one (`MultisigApprove`) brings the owner keys' approvals on chain to the threshold; any other
/// action runs as it is. A mempool read (the first-to-ask name hold, `Chain::first_to_ask`):
/// execution decides.
pub fn runs_when_included(state: &State, tx: &Tx, height: u64) -> bool {
    let Ok(signer) = tx.signer_key_id() else { return true };
    let g = &state.governance;
    match &tx.action {
        Action::MultisigPropose { .. } => g.multisigs.get(&tx.from).is_some_and(|m| m.threshold <= 1 && owner_keys(state, &tx.from).contains(&signer)),
        Action::MultisigApprove { id, action_digest } => {
            let (Some(m), Some(p)) = (g.multisigs.get(&tx.from), g.multisig_proposals.get(id)) else { return false };
            p.account == tx.from && height <= p.expires_at && crate::types::action_digest(&p.action) == *action_digest && !p.approvals.contains(&signer) && owner_keys(state, &tx.from).contains(&signer) && approvals(state, p) + 1 >= m.threshold as usize
        }
        _ => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn multisig_propose(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, key: &KeyRecord, action: &Action, height: u64) -> Result<()> {
    owner_only(key, "propose multisig actions")?;
    clear_expired(state, j, sender, height);
    let mut m = state.governance.multisigs.get(sender).cloned().ok_or_else(|| anyhow!("{} isn't a multisig account (multisig_create first)", short(sender)))?;
    save_scalars(state, j);
    let id = state.governance.next_multisig_id;
    state.governance.next_multisig_id = id.checked_add(1).ok_or_else(|| anyhow!("multisig ids exhausted"))?;
    let p = MultisigProposal { id, account: sender.to_string(), action: action.clone(), proposed_by: signer.to_string(), approvals: BTreeSet::from([signer.to_string()]), created_at: height, expires_at: height.saturating_add(state.chain_params.multisig_expiry_blocks) };
    if approvals(state, &p) >= m.threshold as usize {
        return run_multisig(state, j, tx, sender, signer, &p, height);
    }
    // It waits for approvals: within the account's cap, and the proposing key's share of it.
    if m.pending.len() >= MAX_MULTISIG_PENDING {
        bail!("{} has {MAX_MULTISIG_PENDING} open multisig proposals; approve or let some expire first", short(sender));
    }
    let owners = owner_keys(state, sender).len();
    let share = (MAX_MULTISIG_PENDING / owners.max(1)).max(1);
    let mine = m.pending.iter().filter(|id| state.governance.multisig_proposals.get(*id).is_some_and(|p| p.proposed_by == signer)).count();
    if mine >= share {
        bail!("this key proposed {mine} of {}'s open multisig proposals, its share among {owners} owner keys; approve or let some expire first", short(sender));
    }
    m.pending.insert(id);
    put_multisig(state, j, sender, m);
    put_multisig_proposal(state, j, Some(p), id);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn multisig_approve(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, key: &KeyRecord, id: u64, digest: &str, height: u64) -> Result<()> {
    owner_only(key, "approve multisig actions")?;
    let mut m = state.governance.multisigs.get(sender).cloned().ok_or_else(|| anyhow!("{} isn't a multisig account", short(sender)))?;
    let mut p = state.governance.multisig_proposals.get(&id).cloned().ok_or_else(|| anyhow!("no open multisig action #{id}"))?;
    if p.account != sender {
        bail!("multisig action #{id} belongs to {}", short(&p.account));
    }
    if height > p.expires_at {
        bail!("multisig action #{id} expired at block {}", p.expires_at);
    }
    // The approval is for the action its signer was shown (audit gap-7-6).
    if crate::types::action_digest(&p.action) != digest {
        bail!("multisig action #{id} isn't the action this approval was signed for: the node it was read from may be misreporting it");
    }
    clear_expired(state, j, sender, height);
    m.pending.retain(|id| state.governance.multisig_proposals.contains_key(id));
    if !p.approvals.insert(signer.to_string()) {
        bail!("this key already approved multisig action #{id}");
    }
    if approvals(state, &p) < m.threshold as usize {
        put_multisig_proposal(state, j, Some(p), id);
        return Ok(());
    }
    // The last approval runs it; if the action fails, so does this approval (it can be retried
    // until the proposal expires).
    m.pending.remove(&id);
    put_multisig(state, j, sender, m);
    put_multisig_proposal(state, j, None, id);
    run_multisig(state, j, tx, sender, signer, &p, height)
}

// ---- accounting

/// FANG governance holds outside balances: deposits of proposals still voting, and what's left
/// in open distributions.
pub fn escrow(state: &State) -> u128 {
    let g = &state.governance;
    let deposits: u128 = g.open_proposals.iter().filter_map(|id| g.proposals.get(id)).filter(|p| p.status == ProposalStatus::Voting).map(|p| p.deposit as u128).sum();
    let distributions: u128 = g.open_distributions.iter().filter_map(|id| g.distributions.get(id)).map(|d| (d.total - d.claimed) as u128).sum();
    deposits + distributions
}

/// Governance's consistency rules (part of `State::check_invariants`): records are filed under
/// their ids, the open lists match the statuses, tallies are the sums of the votes, council
/// votes match their voters' votes, multisigs keep at least `threshold` owner keys and their
/// pending lists match the proposals, and distributions' totals match their claims.
pub fn check_invariants(state: &State) -> Result<()> {
    let g = &state.governance;
    let mut sums: BTreeMap<u64, (u128, u128, u128, u128, u64)> = BTreeMap::new();
    for (k, v) in g.votes.iter() {
        ensure!(g.proposals.contains_key(&k.id), "a vote on unknown proposal #{}", k.id);
        ensure!(vote_weight(v.amount, v.conviction) == Some(v.weight) && v.amount > 0, "{}'s vote on #{} has the wrong weight", k.account, k.id);
        ensure!(v.lock_amount >= v.amount, "{}'s vote on #{} locks less than it votes", k.account, k.id);
        let s = sums.entry(k.id).or_default();
        if v.aye {
            (s.0, s.2) = (s.0 + v.amount as u128, s.2 + v.weight);
        } else {
            (s.1, s.3) = (s.1 + v.amount as u128, s.3 + v.weight);
        }
        s.4 += 1;
    }
    for (id, p) in g.proposals.iter() {
        ensure!(*id == p.id && *id < g.next_proposal_id, "proposal #{id} is misfiled");
        let open = matches!(p.status, ProposalStatus::Voting | ProposalStatus::Queued);
        ensure!(open == g.open_proposals.contains(id), "proposal #{id} is {:?} but the open list disagrees", p.status);
        ensure!(p.deposit > 0, "proposal #{id} has no deposit");
        ensure!((p.status == ProposalStatus::Voting) == p.tally.is_none(), "proposal #{id} is {:?} with{} a tally", p.status, if p.tally.is_some() { "" } else { "out" });
        ensure!(p.status != ProposalStatus::Queued || p.execute_at.is_some(), "queued proposal #{id} has no execution height");
        if let ProposalKind::Distribution { distribution_id, .. } = p.kind {
            ensure!(distribution_id <= id.saturating_add(DISTRIBUTION_ID_MARGIN), "proposal #{id} names distribution #{distribution_id}, more than {DISTRIBUTION_ID_MARGIN} past its own id");
        }
        let s = sums.get(id).copied().unwrap_or_default();
        if p.status == ProposalStatus::Voting {
            ensure!(s == (p.aye_amount, p.nay_amount, p.aye_weight, p.nay_weight, p.voters), "proposal #{id}'s tallies aren't the sum of its votes");
            for (validator, c) in &p.council {
                let v = g.votes.get(&IdKey::new(*id, &c.voter));
                ensure!(v.is_some_and(|v| v.aye == c.aye && v.council.as_deref() == Some(validator.as_str())), "proposal #{id}'s council vote for {validator} doesn't match {}'s vote", c.voter);
            }
        } else {
            // Closed: its votes are being swept (what's left is part of the final tally).
            ensure!(s.0 <= p.aye_amount && s.1 <= p.nay_amount && s.2 <= p.aye_weight && s.3 <= p.nay_weight && s.4 <= p.voters, "closed proposal #{id} has more votes than its tally");
            ensure!(s.4 == 0 || g.closed_votes.contains(id), "closed proposal #{id} keeps votes nothing sweeps");
        }
    }
    for id in &g.open_proposals {
        ensure!(g.proposals.contains_key(id), "open proposal #{id} doesn't exist");
    }
    for id in &g.closed_votes {
        ensure!(g.proposals.get(id).is_some_and(|p| p.status != ProposalStatus::Voting), "proposal #{id} is swept but not closed");
    }
    for (id, (_, amount)) in g.recent_spends.iter() {
        ensure!(g.proposals.get(id).is_some_and(|p| matches!(p.kind, ProposalKind::TreasurySpend { amount: a, .. } if a == *amount) && matches!(p.status, ProposalStatus::Queued | ProposalStatus::Executed)), "recent spend #{id} isn't a passed treasury spend");
    }
    for (account, m) in g.multisigs.iter() {
        let owners = owner_keys(state, account).len();
        ensure!(m.threshold >= 1 && m.threshold as usize <= owners, "{account} is a {}-of-{owners} multisig", m.threshold);
        ensure!(m.pending.len() <= MAX_MULTISIG_PENDING, "{account} has too many multisig proposals");
        ensure!(m.pending.iter().all(|id| *id < g.next_multisig_id), "{account} has a multisig proposal from the future");
        for id in &m.pending {
            ensure!(g.multisig_proposals.get(id).is_some_and(|p| p.account == *account), "{account}'s multisig proposal #{id} is missing");
        }
    }
    for (id, p) in g.multisig_proposals.iter() {
        ensure!(*id == p.id && *id < g.next_multisig_id, "multisig proposal #{id} is misfiled");
        ensure!(g.multisigs.get(&p.account).is_some_and(|m| m.pending.contains(id)), "multisig proposal #{id} isn't pending on {}", p.account);
        ensure!(p.approvals.contains(&p.proposed_by), "multisig proposal #{id} lacks its proposer's approval");
        ensure!(p.action.to_bytes().len() <= MAX_MULTISIG_ACTION_BYTES, "multisig proposal #{id}'s action is too large");
    }
    let mut claimed: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    for (k, c) in g.distribution_claims.iter() {
        ensure!(g.distributions.contains_key(&k.id), "a claim on unknown distribution #{}", k.id);
        let s = claimed.entry(k.id).or_default();
        *s = (s.0 + c.amount, s.1 + 1);
    }
    for (id, d) in g.distributions.iter() {
        let named = |p: &Proposal| matches!(p.kind, ProposalKind::Distribution { distribution_id, .. } if distribution_id == *id);
        ensure!(*id == d.id && g.proposals.get(&d.proposal).is_some_and(named), "distribution #{id} is misfiled");
        ensure!(d.claimed <= d.total, "distribution #{id} paid out more than it holds");
        ensure!(claimed.get(id).copied().unwrap_or_default() == (d.claimed, d.claims), "distribution #{id}'s total isn't the sum of its claims");
        ensure!(d.closed_at.is_none() == g.open_distributions.contains(id), "distribution #{id}'s open flag disagrees");
        ensure!(d.closed_at.is_none() || d.returned == d.total - d.claimed, "closed distribution #{id} returned the wrong amount");
    }
    for id in &g.open_distributions {
        ensure!(g.distributions.contains_key(id), "open distribution #{id} doesn't exist");
    }
    // The quorum base counts pool stake and unbonding entries as voters' but the bootstrap
    // delegation's (the only keyless account that stakes, and never through an unbonding entry),
    // and its running totals match a walk over the state.
    let boot = state.bootstrap();
    for a in system_accounts(state.params.prefix()) {
        ensure!((a == boot || crate::staking::delegations_of(state, &a).is_empty()) && crate::staking::entries_of_account(state, &a).is_empty(), "keyless system account {a} holds stake");
    }
    let height = crate::econ::now(state);
    let (fast, walked) = (unlocked_supply(state, height), unlocked_supply_by_walk(state, height));
    ensure!(fast == walked, "the unlocked supply's running total is {fast}, but a walk over the state gives {walked}");
    Ok(())
}

/// Plain-English summary of a governance tx (see `State::describe_with`; `primary_name` looks
/// up names).
pub fn describe(state: &State, tx: &Tx, from: &str, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    match &tx.action {
        Action::Propose { kind, deposit, title, .. } => format!("{from} proposed “{title}”: {}, bonding {}", describe_kind(state, kind, primary_name), fmt_fang(*deposit)),
        Action::Vote { proposal_id, aye, amount, conviction, .. } => {
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
        Action::MultisigApprove { id, .. } => format!("{from} approved multisig action #{id}"),
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
        ProposalKind::UnflagSybil { account, .. } => format!("lift {}'s sybil flag", who(account)),
        ProposalKind::RejectClaim { claim_id, .. } => format!("reject claim #{claim_id}"),
        ProposalKind::Clawback { account, .. } => format!("claw back {}'s unvested rewards", who(account)),
        ProposalKind::UnlinkGithub { handle } => format!("unlink GitHub @{handle} and hold it for the proposer to link for one voting period"),
        ProposalKind::RefundHunt { hunt_id } => format!("refund hunt #{hunt_id}'s funders"),
        ProposalKind::ScheduleUpgrade { version, activation_height } => format!("upgrade to protocol v{version} at block {activation_height}"),
        ProposalKind::ActivateFeature { feature } => format!("turn on {feature}"),
        ProposalKind::DeactivateFeature { feature } => format!("turn off {feature}"),
        // What it would also withdraw and supersede depends on when it runs: a proposal's
        // summary says that while it's open ([`describe_proposal`]), and its outcome says what
        // it did (review-r2-gov-3).
        ProposalKind::CancelUpgrade { version, activation_height } => format!("cancel the upgrade to protocol v{version} at block {activation_height}"),
        ProposalKind::Text {} => "a text proposal (no on-chain effect)".into(),
    }
}

/// A stored proposal's summary: [`describe_kind`], plus, for a `CancelUpgrade` still voting or
/// waiting to run, what it would withdraw and supersede if it ran now, worded as conditional
/// (review-r2-gov-3). Once it has run, its outcome (written when it ran) says what it withdrew
/// and superseded; a rejected or failed one did neither.
pub fn describe_proposal(state: &State, p: &Proposal, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    let what = describe_kind(state, &p.kind, primary_name);
    let ProposalKind::CancelUpgrade { activation_height, .. } = p.kind else { return what };
    if !matches!(p.status, ProposalStatus::Voting | ProposalStatus::Queued) {
        return what;
    }
    let mut also = vec![];
    let after: Vec<String> = state.upgrade.schedule.iter().filter(|u| u.activation_height > activation_height).map(|u| format!("v{} at block {}", u.version, u.activation_height)).collect();
    if !after.is_empty() {
        also.push(format!("it also withdraws what's scheduled after it (now {})", after.join(", ")));
    }
    let open: Vec<String> = open_upgrade_proposals(state).iter().map(|(id, _, v, at)| format!("#{id} (v{v} at block {at})")).collect();
    let fail = "every upgrade proposal made before it runs can't take effect";
    also.push(if open.is_empty() { fail.to_string() } else { format!("{fail} (open now: {})", open.join(", ")) });
    let when = match p.execute_at {
        Some(at) if p.status == ProposalStatus::Queued => format!("when it runs at block {at}"),
        _ => "if it passes".to_string(),
    };
    format!("{what}; {when}, {}", also.join(", and "))
}

#[cfg(test)]
mod tests;
