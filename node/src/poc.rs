//! 🪦 Proof of Contribution: Souls (reputation that can't be bought), vouching, claims on merged
//! PRs that validators check against GitHub, Séances (random commit-reveal review juries) and the
//! contribution pool, paid per sponsor (a person's claims in a Séance share one win rate pooled over
//! all their comparisons; the first counts fully at it, each further one a quarter of the one
//! before) and vested with clawback.
//!
//! Lifecycle of a claim: `submit_claim` (bonded) → validators attest what GitHub says → verified
//! once a BFT quorum agrees on the merge commit → for `claim_maturity_blocks` validators watch for
//! a revert (a quorum rejects it, beacon.rs) → then it enters the next Séance → jurors compare
//! pairs of claims ("which mattered more?"), commit, then reveal → the Séance closes: scores,
//! Soul, rewards (vesting) and bonds are settled.
//!
//! Séances open only at the last block of every `n`th epoch (`State::next_seance_height`, fixed
//! in advance), and draw their jury and pairs from the randomness beacon (`beacon::seance_seed`)
//! over jurors and claims that were eligible before anyone could know it
//! (`State::seance_cutoff`). So neither the opening block's proposer nor anyone who joins,
//! leaves or times a claim can steer the draw.
//!
//! Everything here is deterministic integer math over ordered maps; the open/close of Séances runs
//! in `end_block`, after a block's txs, on every node. The constants below (and the default
//! timings) are consensus rules: changing one changes every chain that uses it.

use crate::chain::{short, Journal, State};
use crate::crypto;
use crate::state::{JMap, TMap};
use crate::types::*;
use anyhow::{anyhow, bail, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Soul is counted in milli-Soul.
pub const SOUL: u64 = 1_000;
pub const MAX_SOUL_GRANT: u64 = 100 * SOUL;
pub const MAX_REPO_WEIGHT: u64 = 100;
pub const VOUCH_MIN_SOUL: u64 = 10 * SOUL;
pub const JUROR_MIN_SOUL: u64 = 5 * SOUL;
pub const VOUCH_BOND: u64 = 10 * DROPS_PER_FANG;
pub const JUROR_BOND: u64 = 10 * DROPS_PER_FANG;
/// Claim bond is this times (1 + the claimant's open claims).
pub const CLAIM_BOND: u64 = DROPS_PER_FANG;
pub const MAX_SEANCE_CLAIMS: usize = 64;
/// A Séance's budget scales with the people whose claims it judges (claimant accounts, so a
/// sponsor and its Familiars are one), reaching the full share at this many: a two-person Séance
/// can't take a quarter of the pool, and one person's extra claims don't add to it (audit
/// xcut-economic-opus-1).
pub const FULL_BUDGET_PEOPLE: u64 = 16;
pub const JURY_SIZE: usize = 7;
/// Jurors needed to seat a Séance, and reveals needed to settle it (otherwise claims carry over).
pub const MIN_JURY: usize = 3;
pub const PAIRS_PER_JUROR: usize = 24;
/// Most grants one account holds at once. Past it, the fully vested ones are paid out to make
/// room (as `WithdrawVested` would), and a reward that still doesn't fit goes back to the pool.
/// At the default timings an account gets at most ~1,700 grants per vesting period (64 claims,
/// 64 vouch cuts and a jury seat per weekly Séance), so only odd genesis timings can reach it;
/// it keeps the grants record far below `state::MAX_VALUE_BYTES` whatever the timings.
pub const MAX_GRANTS: usize = 4_096;
/// Most validator attestations one claim records (about twice the largest active set governance
/// can set), so a claim that waits through many set changes still has a bounded record.
pub const MAX_CLAIM_ATTESTATIONS: usize = 2_048;
/// Share of the pool's balance each Séance distributes, in basis points.
pub const SEANCE_POOL_BPS: u64 = 2_500;
/// Share of a Séance's budget paid to jurors (by agreement).
pub const JUROR_SHARE_BPS: u64 = 1_000;
/// A voucher's cut of a newcomer's rewards while the vouch is young (`vouch_lock_blocks`).
pub const VOUCHER_CUT_BPS: u64 = 1_000;
pub const PPM: u64 = 1_000_000;
/// Claims scoring below this (win rate in their comparisons), or whose claimant's claims in the
/// Séance score below it together (the pooled rate), get their bond back but no reward.
pub const MIN_SCORE_PPM: u64 = 150_000;
/// Two scores closer than this count as "about equal" when grading jurors.
pub const TIE_MARGIN_PPM: u64 = 50_000;
/// Jurors at or above this agreement with the outcome gain Soul and share the jurors' budget;
/// below `BAD_AGREEMENT_PPM` they lose Soul. (Voting "=" on everything scores 50%: no reward.)
pub const GOOD_AGREEMENT_PPM: u64 = 600_000;
pub const BAD_AGREEMENT_PPM: u64 = 400_000;
/// Soul for a claim at a perfect score (scaled by its claimant's pooled rate in the Séance, and by
/// its rank among the claimant's claims there: [`CLAIM_RANK_SHIFT`]).
pub const CLAIM_SOUL: u64 = 10 * SOUL;
/// Volume barely pays (audit xcut-economic-opus-1): within one Séance a person's claims are rated
/// together, by one win rate pooled over all their comparisons (every claim the jury judged and
/// didn't mark spam, those below the threshold too), never by the best of them: each claim's own
/// rate is a noisy estimate from a handful of comparisons, and the best of k noisy rates grows
/// with k (round 3 paid it: with honest jurors who pick a side between two weak PRs, 16 weak
/// PRs out-earned a solid one in 40 of 40 draws), while a pooled rate only gets steadier. The
/// person's accepted claims then count heaviest repo first, each a quarter (`>> 2`) of the one
/// ranked above it, for payout weight and for Soul, so all of them together count for less than
/// 4/3 of one claim at the person's rate: in expectation, k claims of one quality earn at most
/// 4/3 of what one does. A claim that ties a weak field and loses to a solid PR rates under half
/// of it, so even with the full Soul boost (1.5×) against a newcomer's, 4/3 × ½ × 1.5 = 1: a
/// pile of weak claims stays below one solid PR on average (a lucky draw that rates them over
/// half overall can still edge past it).
pub const CLAIM_RANK_SHIFT: u32 = 2;
pub const JUROR_GOOD_SOUL: u64 = SOUL;
pub const JUROR_BAD_SOUL: u64 = 2 * SOUL;
/// Not revealing costs at least as much as revealing a ballot that diverges (plus the bond slash),
/// so jurors can't hold back bad ballots.
pub const NO_SHOW_SOUL: u64 = JUROR_BAD_SOUL;
pub const NO_SHOW_SLASH_BPS: u64 = 1_000;
pub const SPAM_SOUL: u64 = 20 * SOUL;
/// A spam verdict needs at least this many flags, and more than half of the jurors who saw the
/// claim (audit F23: with 64 claims and 24 pairs per seat, most claims are seen by 2–4 of the 7
/// jurors, so "2 and a majority of viewers" let two colluding seats slash competitors). A claim
/// fewer than this many jurors saw can't be marked spam: on ambiguity it's scored, never slashed.
pub const SPAM_MIN_FLAGS: u64 = 3;
pub const SYBIL_VOUCHER_SOUL: u64 = 50 * SOUL;
/// Soul at which the payout boost maxes out (1.5×).
pub const BOOST_CAP_SOUL: u64 = 100 * SOUL;

/// The contribution pool's system account. Anyone can fund it; only Séances pay out of it.
pub fn pool_address(prefix: &str) -> String {
    crypto::system_address(prefix, "contribution-pool")
}

pub fn fmt_soul(milli: u64) -> String {
    let (whole, frac) = (milli / SOUL, milli % SOUL);
    if frac == 0 { whole.to_string() } else { format!("{whole}.{}", format!("{frac:03}").trim_end_matches('0')) }
}

/// Timings, in blocks. From genesis (`contribution`), or derived from the block time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct PocTimes {
    /// Length of a Séance: the first two thirds for sealed votes, the rest for reveals.
    pub seance_blocks: u64,
    /// How long a verified claim waits (for reverts) before it can be judged.
    pub claim_maturity_blocks: u64,
    pub vesting_blocks: u64,
    pub vouch_lock_blocks: u64,
    /// Pending claims validators never settle are refunded after this long.
    pub claim_ttl_blocks: u64,
    pub soul_half_life_blocks: u64,
}

impl PocTimes {
    pub fn for_block_time(block_time_ms: u64) -> Self {
        let day = 86_400_000 / block_time_ms.max(1);
        Self { seance_blocks: 7 * day, claim_maturity_blocks: 30 * day, vesting_blocks: 90 * day, vouch_lock_blocks: 90 * day, claim_ttl_blocks: 7 * day, soul_half_life_blocks: 365 * day }
    }

    /// `blockula dev`: minutes instead of weeks, so the whole loop can be tried locally.
    pub fn dev(block_time_ms: u64) -> Self {
        Self { seance_blocks: 90, claim_maturity_blocks: 0, vesting_blocks: 600, vouch_lock_blocks: 120, claim_ttl_blocks: 600, ..Self::for_block_time(block_time_ms) }
    }

    pub fn check(&self) -> Result<()> {
        if self.seance_blocks < 3 || self.soul_half_life_blocks == 0 || self.vesting_blocks == 0 || self.claim_ttl_blocks == 0 {
            bail!("contribution timings: seance_blocks ≥ 3, and half-life, vesting and claim TTL ≥ 1");
        }
        const MAX: u64 = 1 << 40;
        if [self.seance_blocks, self.claim_maturity_blocks, self.vesting_blocks, self.vouch_lock_blocks, self.claim_ttl_blocks, self.soul_half_life_blocks].iter().any(|v| *v > MAX) {
            bail!("contribution timings must be at most {MAX} blocks");
        }
        Ok(())
    }

    pub fn commit_blocks(&self) -> u64 {
        (self.seance_blocks * 2 / 3).max(1)
    }
    pub fn reveal_blocks(&self) -> u64 {
        self.seance_blocks.saturating_sub(self.commit_blocks()).max(1)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Soul {
    /// milli-Soul as of height `at`; it decays from there (half-life `soul_half_life_blocks`).
    pub points: u64,
    pub at: u64,
    /// Lifetime milli-Soul gained (display only, never decays).
    #[serde(default)]
    pub earned: u64,
}

/// `points` after `elapsed` blocks of decay: whole half-lives as shifts, then a linear step for
/// the remainder (2^-x ≈ 1 - x/2 on [0, 1)). Integer-only, so every node agrees.
pub fn decay(points: u64, elapsed: u64, half_life: u64) -> u64 {
    let hl = half_life.max(1);
    let halvings = elapsed / hl;
    if halvings >= 64 {
        return 0;
    }
    let v = points >> halvings;
    v - (v as u128 * (elapsed % hl) as u128 / (2 * hl as u128)) as u64
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Vouch {
    pub voucher: String,
    #[serde(with = "u64_str")]
    pub bond: u64,
    pub at: u64,
}

/// A withdrawn vouch, kept while it still counts (audits xcut-economic-opus-2 and
/// review-fix-poc-2), under [`vouch_key`]. Until the vouch's lock would have ended, and while a
/// `FlagSybil` proposal names the vouchee, its voucher still answers for the vouchee: the bond
/// stays held (`bond`) and its slot taken, and a sybil flag on the vouchee costs the voucher the
/// bond and `SYBIL_VOUCHER_SOUL` as if the vouch were live. And the two stay tied for any Séance
/// whose cutoff came before the withdrawal (`State::conflicted`), so dropping a vouch once a seed
/// is public changes nobody's seat. Epoch boundaries hand back bonds that are free and forget
/// records no Séance can need (`State::sweep_dropped_vouches`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct DroppedVouch {
    /// Still held (0 once handed back).
    #[serde(with = "u64_str")]
    pub bond: u64,
    /// When the vouch was made (its lock runs `vouch_lock_blocks` from here), and withdrawn.
    pub at: u64,
    pub dropped_at: u64,
}

/// `vouchee/voucher`, the key of a [`DroppedVouch`] (addresses never contain `/`), so a vouchee's
/// records are one range.
pub fn vouch_key(vouchee: &str, voucher: &str) -> String {
    format!("{vouchee}/{voucher}")
}

/// A GitHub handle held for one account after governance unlinked it at that account's request
/// (`ProposalKind::UnlinkGithub`, audit xcut-economic-4): until `until`, only `account` can link
/// it, so the squatter can't simply link it again the next block. Keyed by lowercase handle;
/// cleared when the handle is next linked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct HandleHold {
    pub account: String,
    pub until: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Sybil {
    pub at: u64,
    pub by: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// Waiting for validators to check GitHub.
    Pending,
    /// A quorum of validators agreed it's a merged PR by the claimant's handle.
    Verified,
    InSeance,
    /// Judged; `payout` may be 0 if it scored below the threshold.
    Scored,
    /// The jury flagged it; bond slashed, Soul lost.
    Spam,
    Rejected,
}

impl ClaimStatus {
    pub fn is_open(self) -> bool {
        matches!(self, ClaimStatus::Pending | ClaimStatus::Verified | ClaimStatus::InSeance)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Attestation {
    pub ok: bool,
    pub merge_sha: String,
    pub at: u64,
    /// Why it was refuted (what the validator saw on GitHub).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Claim {
    pub id: u64,
    pub claimant: String,
    /// The claimant's linked GitHub handle when the claim was made (must be the PR's author).
    pub handle: String,
    /// Lowercase `owner/name`, as registered.
    pub repo: String,
    pub pr: u64,
    pub familiar_key: Option<String>,
    #[serde(with = "u64_str")]
    pub bond: u64,
    pub submitted_at: u64,
    pub status: ClaimStatus,
    /// By validator address.
    pub attestations: BTreeMap<String, Attestation>,
    pub merge_sha: Option<String>,
    pub verified_at: Option<u64>,
    pub seance: Option<u64>,
    pub score_ppm: Option<u64>,
    #[serde(with = "u64_str", default)]
    pub payout: u64,
    #[serde(default)]
    pub soul_awarded: u64,
    pub grant: Option<u64>,
    pub closed_at: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Juror {
    #[serde(with = "u64_str")]
    pub bond: u64,
    pub joined_at: u64,
    /// No-shows are deactivated until they join again.
    pub active: bool,
    pub served: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct JurySeat {
    /// Pairs of claim ids to compare, in the order votes refer to them.
    pub pairs: Vec<[u64; 2]>,
    pub commitment: Option<String>,
    pub votes: Option<String>,
    #[serde(default)]
    pub spam: Vec<u64>,
    pub agreement_ppm: Option<u64>,
    #[serde(with = "u64_str", default)]
    pub reward: u64,
    /// Soul change at close, in milli-Soul (negative = lost).
    #[serde(default)]
    pub soul_delta: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct SeanceResults {
    pub accepted: usize,
    pub below_threshold: usize,
    pub spam: usize,
    pub carried_over: usize,
    pub no_shows: usize,
    #[serde(with = "u64_str")]
    pub paid: u64,
    /// Budget that went back to the pool (no accepted claims, rounding).
    #[serde(with = "u64_str")]
    pub returned: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Seance {
    pub id: u64,
    pub opened_at: u64,
    /// Last height a sealed vote (commit) is accepted.
    pub commit_until: u64,
    /// Last height a reveal is accepted; the Séance closes at the end of this block.
    pub reveal_until: u64,
    /// The randomness the jury and pairs were drawn from.
    pub seed: String,
    pub claims: Vec<u64>,
    #[serde(with = "u64_str")]
    pub budget: u64,
    pub jurors: BTreeMap<String, JurySeat>,
    pub closed: bool,
    pub results: Option<SeanceResults>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GrantSource {
    Claim { claim_id: u64 },
    Jury { seance_id: u64 },
    /// A voucher's cut of the newcomer's reward for `claim_id`.
    Vouch { claim_id: u64 },
}

/// A reward that vests linearly from `start` to `end`. Unvested FANG can be clawed back.
#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Grant {
    pub id: u64,
    #[serde(with = "u64_str")]
    pub amount: u64,
    #[serde(with = "u64_str")]
    pub withdrawn: u64,
    pub start: u64,
    pub end: u64,
    pub source: GrantSource,
}

impl Grant {
    pub fn vested(&self, height: u64) -> u64 {
        if height >= self.end || self.end <= self.start {
            return self.amount;
        }
        (self.amount as u128 * height.saturating_sub(self.start) as u128 / (self.end - self.start) as u128) as u64
    }
}

/// What `Poc::claims` indexes as it changes: claims still pending or verified (not yet in a
/// Séance or settled), so the epoch-boundary steps walk those instead of every claim ever made.
pub struct OpenClaims;

impl crate::state::Aggregate<Claim> for OpenClaims {
    fn indexed(c: &Claim) -> bool {
        matches!(c.status, ClaimStatus::Pending | ClaimStatus::Verified)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Poc {
    /// Lowercase `owner/name` → weight.
    pub repos: TMap<String, u64>,
    pub souls: TMap<String, Soul>,
    /// Vouchee → the vouch.
    pub vouches: TMap<String, Vouch>,
    /// Withdrawn vouches that still count, by [`vouch_key`].
    pub dropped_vouches: TMap<String, DroppedVouch>,
    pub sybils: TMap<String, Sybil>,
    pub claims: TMap<u64, Claim, OpenClaims>,
    /// `repo#pr` → claim id, so a PR is claimed once (cleared when validators reject it).
    pub claim_by_pr: TMap<String, u64>,
    /// Lowercase GitHub handle → the account its first verified claim credited. A handle's work
    /// is always credited to one account, so relinking it can't split one person's payout.
    pub handles: TMap<String, String>,
    /// Lowercase GitHub handle → who may link it while governance holds it ([`HandleHold`]).
    pub handle_holds: TMap<String, HandleHold>,
    pub jurors: TMap<String, Juror>,
    pub seances: TMap<u64, Seance>,
    /// Account → grant id → grant.
    pub grants: TMap<String, BTreeMap<u64, Grant>>,
    pub next_claim_id: u64,
    pub next_seance_id: u64,
    pub next_grant_id: u64,
    pub open_seance: Option<u64>,
    /// The next height on the cadence where a Séance may open (display; see
    /// `State::next_seance_height`).
    pub next_seance_at: u64,
}

impl Poc {
    /// False until Proof of Contribution is first used; until then the end-of-block step
    /// (Séances) doesn't run.
    pub fn is_active(&self) -> bool {
        !(self.repos.is_empty() && self.souls.is_empty() && self.vouches.is_empty() && self.dropped_vouches.is_empty() && self.sybils.is_empty() && self.claims.is_empty() && self.jurors.is_empty() && self.grants.is_empty() && self.handles.is_empty() && self.seances.is_empty())
    }

    pub fn has_open_claims(&self, account: &str) -> usize {
        self.claims.values().filter(|c| c.claimant == account && c.status.is_open()).count()
    }
}

// ------------------------------------------------------------------------------------ journal

type Scalars = (u64, u64, u64, Option<u64>, u64);

#[derive(Default)]
pub struct PocJournal {
    repos: JMap<String, u64>,
    souls: JMap<String, Soul>,
    vouches: JMap<String, Vouch>,
    dropped_vouches: JMap<String, DroppedVouch>,
    sybils: JMap<String, Sybil>,
    claims: JMap<u64, Claim>,
    claim_by_pr: JMap<String, u64>,
    handles: JMap<String, String>,
    handle_holds: JMap<String, HandleHold>,
    jurors: JMap<String, Juror>,
    seances: JMap<u64, Seance>,
    grants: JMap<String, BTreeMap<u64, Grant>>,
    scalars: Option<Scalars>,
}

impl PocJournal {
    pub fn absorb(&mut self, o: PocJournal) {
        self.repos.absorb(o.repos);
        self.souls.absorb(o.souls);
        self.vouches.absorb(o.vouches);
        self.dropped_vouches.absorb(o.dropped_vouches);
        self.sybils.absorb(o.sybils);
        self.claims.absorb(o.claims);
        self.claim_by_pr.absorb(o.claim_by_pr);
        self.handles.absorb(o.handles);
        self.handle_holds.absorb(o.handle_holds);
        self.jurors.absorb(o.jurors);
        self.seances.absorb(o.seances);
        self.grants.absorb(o.grants);
        if self.scalars.is_none() {
            self.scalars = o.scalars;
        }
    }

    pub fn rollback(self, p: &mut Poc) {
        self.repos.restore(&mut p.repos);
        self.souls.restore(&mut p.souls);
        self.vouches.restore(&mut p.vouches);
        self.dropped_vouches.restore(&mut p.dropped_vouches);
        self.sybils.restore(&mut p.sybils);
        self.claims.restore(&mut p.claims);
        self.claim_by_pr.restore(&mut p.claim_by_pr);
        self.handles.restore(&mut p.handles);
        self.handle_holds.restore(&mut p.handle_holds);
        self.jurors.restore(&mut p.jurors);
        self.seances.restore(&mut p.seances);
        self.grants.restore(&mut p.grants);
        if let Some((a, b, c, open, next)) = self.scalars {
            (p.next_claim_id, p.next_seance_id, p.next_grant_id, p.open_seance, p.next_seance_at) = (a, b, c, open, next);
        }
    }
}

// ------------------------------------------------------------------------------- randomness

/// Deterministic stream from a Séance's seed (BLAKE3 XOF, domain-separated per use).
struct Rng(blake3::OutputReader);

impl Rng {
    fn new(seed: &str, ctx: &[u8]) -> Self {
        let mut h = blake3::Hasher::new_derive_key("blockula seance rng v1");
        h.update(seed.as_bytes());
        h.update(&(ctx.len() as u64).to_le_bytes());
        h.update(ctx);
        Rng(h.finalize_xof())
    }
    fn next(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.0.fill(&mut b);
        u64::from_le_bytes(b)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            v.swap(i, j);
        }
    }
}

/// What a claim's `weight` counts for at `rank` (0 = its claimant's first in the Séance):
/// `weight / 4^rank` ([`CLAIM_RANK_SHIFT`]), 0 once the shift passes the width.
fn ranked(weight: u128, rank: usize) -> u128 {
    u32::try_from(rank).ok().and_then(|r| r.checked_mul(CLAIM_RANK_SHIFT)).and_then(|sh| weight.checked_shr(sh)).unwrap_or(0)
}

fn bps(amount: u64, bps: u64) -> u64 {
    (amount as u128 * bps as u128 / 10_000) as u64
}

/// Splits `total` across `weights` proportionally; rounding dust goes to the largest weight.
fn split(total: u64, weights: &[(String, u128)]) -> Vec<(String, u64)> {
    let sum: u128 = weights.iter().map(|(_, w)| *w).sum();
    if sum == 0 || total == 0 {
        return vec![];
    }
    let mut out: Vec<(String, u64)> = weights.iter().map(|(k, w)| (k.clone(), (total as u128 * w / sum) as u64)).collect();
    let paid: u64 = out.iter().map(|(_, v)| *v).sum();
    if let Some(i) = weights.iter().enumerate().max_by(|a, b| a.1 .1.cmp(&b.1 .1).then(b.0.cmp(&a.0))).map(|(i, _)| i) {
        out[i].1 += total - paid;
    }
    out
}

// --------------------------------------------------------------------------------- state ops

impl State {
    /// Effective Soul (milli-Soul) at `height`, after decay.
    pub fn soul_of(&self, account: &str, height: u64) -> u64 {
        self.poc.souls.get(account).map(|s| decay(s.points, height.saturating_sub(s.at), self.params.poc.soul_half_life_blocks)).unwrap_or(0)
    }

    pub fn is_sybil(&self, account: &str) -> bool {
        self.poc.sybils.contains_key(account)
    }

    /// How many claims an account may have open at once: 3, plus one per 10 Soul, up to 20.
    pub fn max_open_claims(&self, account: &str, height: u64) -> usize {
        (3 + self.soul_of(account, height) / (10 * SOUL)).min(20) as usize
    }

    /// Active vouches an account may hold: one per 10 Soul, up to 10.
    pub fn max_vouches(&self, account: &str, height: u64) -> usize {
        (self.soul_of(account, height) / (10 * SOUL)).min(10) as usize
    }

    /// The vouches `voucher` answers for: its live ones, and the withdrawn ones whose bond is
    /// still held ([`DroppedVouch`]). They count against [`State::max_vouches`].
    pub fn vouches_held_by(&self, voucher: &str) -> usize {
        let live = self.poc.vouches.values().filter(|v| v.voucher == voucher).count();
        live + self.poc.dropped_vouches.iter().filter(|(k, d)| d.bond > 0 && k.rsplit_once('/').is_some_and(|(_, by)| by == voucher)).count()
    }

    /// A vouchee's withdrawn vouches that still count: (voucher, record).
    pub fn dropped_vouches_of(&self, vouchee: &str) -> Vec<(String, DroppedVouch)> {
        self.poc.dropped_vouches.range(format!("{vouchee}/")..format!("{vouchee}0")).filter_map(|(k, d)| Some((k.rsplit_once('/')?.1.to_string(), d.clone()))).collect()
    }

    /// Whether a `FlagSybil` proposal naming `account` is being voted on or waiting out its
    /// timelock (its account was resolved to an address when it was proposed).
    pub fn sybil_flag_proposed(&self, account: &str) -> bool {
        let g = &self.governance;
        g.open_proposals.iter().filter_map(|id| g.proposals.get(id)).any(|p| matches!(&p.kind, ProposalKind::FlagSybil { account: a, .. } if a == account))
    }

    /// The bond the next claim from `account` costs.
    pub fn claim_bond(&self, account: &str) -> u64 {
        CLAIM_BOND.saturating_mul(1 + self.poc.has_open_claims(account) as u64)
    }

    /// Unvested FANG across an account's grants, and what's withdrawable now.
    pub fn grant_totals(&self, account: &str, height: u64) -> (u64, u64, u64) {
        let mut total = 0u64;
        let mut vested = 0u64;
        let mut withdrawn = 0u64;
        for g in self.poc.grants.get(account).into_iter().flat_map(|m| m.values()) {
            total += g.amount;
            vested += g.vested(height);
            withdrawn += g.withdrawn;
        }
        (total, vested.saturating_sub(withdrawn), total.saturating_sub(vested))
    }

    fn pj<'j>(&self, j: &'j mut Journal) -> &'j mut PocJournal {
        if j.poc.scalars.is_none() {
            let p = &self.poc;
            j.poc.scalars = Some((p.next_claim_id, p.next_seance_id, p.next_grant_id, p.open_seance, p.next_seance_at));
        }
        &mut j.poc
    }

    fn soul_mut(&mut self, j: &mut Journal, account: &str, height: u64) -> &mut Soul {
        let key = account.to_string();
        self.pj(j).souls.touch(&self.poc.souls, &key);
        let hl = self.params.poc.soul_half_life_blocks;
        let s = self.poc.souls.entry(key).or_default();
        s.points = decay(s.points, height.saturating_sub(s.at), hl);
        s.at = height;
        s
    }

    /// Adds (or with a negative delta, removes, saturating) Soul. Sybils never gain any.
    fn add_soul(&mut self, j: &mut Journal, account: &str, delta: i64, height: u64) -> i64 {
        if delta > 0 && self.is_sybil(account) {
            return 0;
        }
        let s = self.soul_mut(j, account, height);
        let before = s.points;
        if delta >= 0 {
            s.points = s.points.saturating_add(delta as u64);
            s.earned = s.earned.saturating_add(delta as u64);
        } else {
            s.points = s.points.saturating_sub(delta.unsigned_abs());
        }
        s.points as i64 - before as i64
    }

    fn claim_mut(&mut self, j: &mut Journal, id: u64) -> Option<&mut Claim> {
        self.pj(j).claims.touch(&self.poc.claims, &id);
        self.poc.claims.get_mut(&id)
    }

    fn juror_mut(&mut self, j: &mut Journal, account: &str) -> Option<&mut Juror> {
        let key = account.to_string();
        self.pj(j).jurors.touch(&self.poc.jurors, &key);
        self.poc.jurors.get_mut(account)
    }

    fn seance_mut(&mut self, j: &mut Journal, id: u64) -> Option<&mut Seance> {
        self.pj(j).seances.touch(&self.poc.seances, &id);
        self.poc.seances.get_mut(&id)
    }

    fn grants_mut(&mut self, j: &mut Journal, account: &str) -> &mut BTreeMap<u64, Grant> {
        let key = account.to_string();
        self.pj(j).grants.touch(&self.poc.grants, &key);
        self.poc.grants.entry(key).or_default()
    }

    fn set_dropped_vouch(&mut self, j: &mut Journal, key: &str, d: Option<DroppedVouch>) {
        let key = key.to_string();
        self.pj(j).dropped_vouches.touch(&self.poc.dropped_vouches, &key);
        match d {
            Some(d) => self.poc.dropped_vouches.insert(key, d),
            None => self.poc.dropped_vouches.remove(&key),
        };
    }

    /// Withdraws the live vouch for `to`. Its voucher answers for `to` until the lock would have
    /// ended, and while a `FlagSybil` proposal names `to`: the bond stays held until then. Either
    /// way the withdrawal is recorded, so the two stay tied for the Séances whose draw could be
    /// known when it happened ([`DroppedVouch`]).
    fn drop_vouch(&mut self, j: &mut Journal, to: &str, height: u64) {
        let Some(v) = self.poc.vouches.get(to).cloned() else { return };
        let key = vouch_key(to, &v.voucher);
        let mut d = self.poc.dropped_vouches.get(&key).cloned().unwrap_or(DroppedVouch { bond: 0, at: v.at, dropped_at: height });
        if height < v.at.saturating_add(self.params.poc.vouch_lock_blocks) || self.sybil_flag_proposed(to) {
            d.bond += v.bond;
            d.at = d.at.max(v.at);
        } else {
            self.j_credit(j, &v.voucher, v.bond);
        }
        d.dropped_at = height;
        self.set_dropped_vouch(j, &key, Some(d));
        self.pj(j).vouches.touch(&self.poc.vouches, to);
        self.poc.vouches.remove(to);
    }

    /// Sets or clears the hold on a GitHub handle (lowercase).
    pub(crate) fn set_handle_hold(&mut self, j: &mut Journal, handle: &str, hold: Option<HandleHold>) {
        let key = handle.to_string();
        self.pj(j).handle_holds.touch(&self.poc.handle_holds, &key);
        match hold {
            Some(h) => self.poc.handle_holds.insert(key, h),
            None => self.poc.handle_holds.remove(&key),
        };
    }

    fn set_claim_by_pr(&mut self, j: &mut Journal, key: &str, id: Option<u64>) {
        let key = key.to_string();
        self.pj(j).claim_by_pr.touch(&self.poc.claim_by_pr, &key);
        match id {
            Some(id) => self.poc.claim_by_pr.insert(key, id),
            None => self.poc.claim_by_pr.remove(&key),
        };
    }

    /// Grants `amount` to `account`, vesting from `height`. `None` if the account holds
    /// `MAX_GRANTS` still-vesting grants: the amount went back to the pool instead.
    fn add_grant(&mut self, j: &mut Journal, account: &str, amount: u64, height: u64, source: GrantSource) -> Option<u64> {
        self.pj(j);
        if self.poc.grants.get(account).is_some_and(|g| g.len() >= MAX_GRANTS) {
            // Make room: pay out what has fully vested.
            let grants = self.grants_mut(j, account);
            let mut paid = 0u64;
            for g in grants.values_mut().filter(|g| g.vested(height) == g.amount) {
                paid += g.amount - g.withdrawn;
                g.withdrawn = g.amount;
            }
            grants.retain(|_, g| g.withdrawn < g.amount);
            let full = grants.len() >= MAX_GRANTS;
            if grants.is_empty() {
                self.poc.grants.remove(account);
            }
            if paid > 0 {
                self.j_credit(j, account, paid);
            }
            if full {
                self.to_pool(j, amount);
                return None;
            }
        }
        let id = self.poc.next_grant_id;
        self.poc.next_grant_id += 1;
        let end = height.saturating_add(self.params.poc.vesting_blocks);
        self.grants_mut(j, account).insert(id, Grant { id, amount, withdrawn: 0, start: height, end, source });
        Some(id)
    }

    /// Returns `amount` to the pool account.
    fn to_pool(&mut self, j: &mut Journal, amount: u64) {
        if amount > 0 {
            let pool = self.pool();
            self.j_credit(j, &pool, amount);
        }
    }

    /// Cuts an account's grants down to what has vested; the rest goes back to the pool.
    /// `only` limits it to one grant. Returns the amount clawed back.
    fn claw_back(&mut self, j: &mut Journal, account: &str, only: Option<u64>, height: u64) -> u64 {
        if !self.poc.grants.contains_key(account) {
            return 0;
        }
        let mut back = 0u64;
        for g in self.grants_mut(j, account).values_mut().filter(|g| only.is_none_or(|id| id == g.id)) {
            let vested = g.vested(height);
            back += g.amount - vested;
            g.amount = vested;
            g.end = g.end.min(height.max(g.start));
        }
        self.to_pool(j, back);
        back
    }

    fn set_status(&mut self, j: &mut Journal, id: u64, status: ClaimStatus, height: u64, reason: Option<String>) {
        if let Some(c) = self.claim_mut(j, id) {
            c.status = status;
            c.closed_at = Some(height);
            if reason.is_some() {
                c.reason = reason;
            }
        }
    }

    /// Settles a claim's bond: back to the claimant, or slashed into the pool.
    fn settle_bond(&mut self, j: &mut Journal, id: u64, refund: bool) {
        let Some(c) = self.claim_mut(j, id) else { return };
        let (bond, who) = (c.bond, c.claimant.clone());
        c.bond = 0;
        if refund { self.j_credit(j, &who, bond) } else { self.to_pool(j, bond) }
    }

    /// Applies a Proof of Contribution action.
    pub(crate) fn apply_poc(&mut self, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, key: &crate::chain::KeyRecord, height: u64) -> Result<()> {
        let admin = self.params.admins.iter().any(|a| a == sender);
        let no_admins = self.params.admins.is_empty();
        let owner = key.role == KeyRole::Owner;
        let need_owner = |what: &str| -> Result<()> {
            if !owner {
                bail!("only owner keys can {what}");
            }
            Ok(())
        };
        // GOV-3: the admin levers exist only on chains with admins (dev and test); elsewhere they
        // are governance proposals.
        let need_admin = |what: &str| -> Result<()> {
            if no_admins {
                bail!("only validator admins can {what}, and this chain has none: put it to a governance proposal");
            }
            if !admin {
                bail!("only validator admins can {what} (Season 0)");
            }
            Ok(())
        };
        match &tx.action {
            Action::SetRepo { repo, weight } => {
                need_admin("set pool repos")?;
                self.exec_set_repo(j, repo, *weight, height)?;
            }
            Action::GrantSoul { to, amount, .. } => {
                need_admin("grant Soul")?;
                self.exec_grant_soul(j, to, *amount, height)?;
            }
            Action::Vouch { account } => {
                need_owner("vouch")?;
                let to = self.resolve(account)?;
                if to == sender {
                    bail!("you can't vouch for yourself");
                }
                if self.is_sybil(sender) || self.is_sybil(&to) {
                    bail!("sybil-flagged accounts can't vouch or be vouched for");
                }
                if self.soul_of(sender, height) < VOUCH_MIN_SOUL {
                    bail!("vouching needs at least {} Soul (you have {})", fmt_soul(VOUCH_MIN_SOUL), fmt_soul(self.soul_of(sender, height)));
                }
                if let Some(v) = self.poc.vouches.get(&to) {
                    bail!("{} is already vouched for by {}", short(&to), short(&v.voucher));
                }
                // Vouches are for newcomers: someone with Soul or judged work doesn't need one
                // (and a voucher's cut of their rewards would be a free skim).
                if self.soul_of(&to, height) > 0 || self.poc.claims.values().any(|c| c.claimant == to && c.status == ClaimStatus::Scored) {
                    bail!("{} isn't a newcomer (they already have Soul or judged work)", short(&to));
                }
                // A withdrawn vouch still takes its slot while its bond is held (`DroppedVouch`).
                if self.vouches_held_by(sender) >= self.max_vouches(sender, height) {
                    bail!("your Soul allows {} active vouches (one per 10 Soul; a withdrawn vouch counts until its lock is over)", self.max_vouches(sender, height));
                }
                self.j_debit(j, sender, VOUCH_BOND)?;
                self.pj(j).vouches.touch(&self.poc.vouches, &to);
                self.poc.vouches.insert(to, Vouch { voucher: sender.to_string(), bond: VOUCH_BOND, at: height });
            }
            Action::Unvouch { account } => {
                need_owner("withdraw a vouch")?;
                let to = self.resolve(account)?;
                let v = self.poc.vouches.get(&to).cloned().ok_or_else(|| anyhow!("{} has no vouch", short(&to)))?;
                // The vouchee may decline a vouch any time; the voucher waits out the lock.
                if to != sender {
                    if v.voucher != sender {
                        bail!("only the voucher (or the account vouched for) can withdraw this vouch");
                    }
                    let unlock = v.at.saturating_add(self.params.poc.vouch_lock_blocks);
                    if height < unlock {
                        bail!("this vouch is locked until block {unlock}");
                    }
                }
                // The vouch itself can always go, so a newcomer can decline an unwanted one even
                // with a claim open (a lock would let a voucher force its cut). What it doesn't
                // undo (`DroppedVouch`): the voucher answers for the newcomer until the lock is
                // over (audit xcut-economic-opus-2: otherwise a sybil drops its vouch before the
                // flag and its voucher loses nothing), and the two stay off each other's claims
                // in any Séance whose seed could be known when it went (audits F53 and
                // review-fix-poc-2), without moving anyone's seat.
                self.drop_vouch(j, &to, height);
            }
            Action::FlagSybil { account, reason } => {
                need_admin("flag sybils")?;
                if self.params.admins.contains(&self.resolve(account)?) {
                    bail!("admins can't be flagged");
                }
                self.exec_flag_sybil(j, account, sender, reason, height)?;
            }
            Action::SubmitClaim { repo, pr, familiar_key } => {
                if self.is_sybil(sender) {
                    bail!("this account is flagged as a sybil");
                }
                let handle = self.github_by_account.get(sender).and_then(|h| self.github.get(h)).map(|l| l.handle.clone()).ok_or_else(|| anyhow!("link a GitHub handle first (link_github)"))?;
                if let Some(a) = self.poc.handles.get(&handle.to_ascii_lowercase()).filter(|a| *a != sender) {
                    bail!("@{handle}'s contributions are credited to {}; claim from that account", short(a));
                }
                let repo_key = repo.to_ascii_lowercase();
                if !self.poc.repos.contains_key(&repo_key) {
                    bail!("{repo} isn't in the contribution pool (yet)");
                }
                let pr_key = format!("{repo_key}#{pr}");
                if let Some(id) = self.poc.claim_by_pr.get(&pr_key) {
                    bail!("{repo}#{pr} is already claimed (claim #{id})");
                }
                if self.soul_of(sender, height) == 0 && !self.poc.vouches.contains_key(sender) {
                    bail!("claims need Soul or a vouch from someone with Soul");
                }
                // Session keys reach here only as registered Familiars (see `admit`).
                let familiar = match (&key.role, familiar_key) {
                    (KeyRole::Session { .. }, Some(k)) if k != signer => bail!("a Familiar can only claim as itself"),
                    (KeyRole::Session { .. }, _) => Some(signer.to_string()),
                    (KeyRole::Owner, Some(k)) => {
                        if !self.familiars.get(sender).is_some_and(|f| f.contains_key(k)) {
                            bail!("that key isn't a registered Familiar of this account");
                        }
                        Some(k.clone())
                    }
                    (KeyRole::Owner, None) => None,
                };
                let open = self.poc.has_open_claims(sender);
                let cap = self.max_open_claims(sender, height);
                if open >= cap {
                    bail!("you have {open} open claims (your Soul allows {cap}); wait for some to be judged");
                }
                let bond = self.claim_bond(sender);
                if let KeyRole::Session { .. } = key.role {
                    let mut keys = self.account(sender).keys;
                    if let Some(rec) = keys.get_mut(signer) {
                        rec.spent = rec.spent.saturating_add(bond);
                    }
                    self.acct_mut(j, sender).keys = keys;
                }
                self.j_debit(j, sender, bond)?;
                self.pj(j);
                let id = self.poc.next_claim_id;
                self.poc.next_claim_id += 1;
                self.pj(j).claims.touch(&self.poc.claims, &id);
                self.poc.claims.insert(
                    id,
                    Claim { id, claimant: sender.to_string(), handle, repo: repo_key, pr: *pr, familiar_key: familiar, bond, submitted_at: height, status: ClaimStatus::Pending, attestations: BTreeMap::new(), merge_sha: None, verified_at: None, seance: None, score_ppm: None, payout: 0, soul_awarded: 0, grant: None, closed_at: None, reason: None },
                );
                self.set_claim_by_pr(j, &pr_key, Some(id));
            }
            Action::AttestClaim { claim_id, ok, merge_sha, note } => {
                let epoch = self.params.epoch_of(height);
                let set = self.active_sets.get(&epoch).cloned().unwrap_or_default();
                // The validator this signer acts for: itself, or (SF-13) its owner paying the fee.
                let validator = crate::staking::validator_for_signer(self, sender, signer, epoch).ok_or_else(|| anyhow!("only active validators can attest claims"))?;
                let c = self.poc.claims.get(claim_id).ok_or_else(|| anyhow!("no claim #{claim_id}"))?;
                if c.status != ClaimStatus::Pending {
                    bail!("claim #{claim_id} is {:?}, not pending", c.status);
                }
                if c.attestations.contains_key(&validator) {
                    bail!("this validator already attested claim #{claim_id}");
                }
                if c.attestations.len() >= MAX_CLAIM_ATTESTATIONS {
                    bail!("claim #{claim_id} has {MAX_CLAIM_ATTESTATIONS} attestations already");
                }
                let c = self.claim_mut(j, *claim_id).unwrap();
                c.attestations.insert(validator, Attestation { ok: *ok, merge_sha: merge_sha.clone(), at: height, note: note.clone() });
                // A BFT quorum of the current set settles it either way.
                let n = set.len();
                let quorum = n - (n - 1) / 3;
                let mut by_sha: BTreeMap<String, usize> = BTreeMap::new();
                let mut bad = 0;
                for (v, a) in &c.attestations {
                    if !set.contains(v) {
                        continue;
                    }
                    if a.ok { *by_sha.entry(a.merge_sha.clone()).or_default() += 1 } else { bad += 1 }
                }
                if let Some((sha, _)) = by_sha.into_iter().find(|(_, k)| *k >= quorum) {
                    let (handle, claimant, pr_key) = (c.handle.to_ascii_lowercase(), c.claimant.clone(), format!("{}#{}", c.repo, c.pr));
                    match self.poc.handles.get(&handle) {
                        Some(a) if *a != claimant => {
                            let reason = format!("@{handle}'s contributions are credited to {}; bond refunded", short(a));
                            self.set_status(j, *claim_id, ClaimStatus::Rejected, height, Some(reason));
                            self.settle_bond(j, *claim_id, true);
                            self.set_claim_by_pr(j, &pr_key, None);
                        }
                        existing => {
                            if existing.is_none() {
                                self.pj(j).handles.touch(&self.poc.handles, &handle);
                                self.poc.handles.insert(handle, claimant);
                            }
                            let c = self.claim_mut(j, *claim_id).unwrap();
                            c.merge_sha = Some(sha);
                            c.status = ClaimStatus::Verified;
                            c.verified_at = Some(height);
                        }
                    }
                } else if bad > n - quorum {
                    let pr_key = format!("{}#{}", c.repo, c.pr);
                    let why = c.attestations.values().find(|a| !a.ok && !a.note.is_empty()).map(|a| a.note.clone()).unwrap_or_else(|| format!("not a merged PR by @{}, or the GitHub link doesn't verify", c.handle));
                    let reason = format!("validators refuted it: {why}");
                    self.set_status(j, *claim_id, ClaimStatus::Rejected, height, Some(reason));
                    self.settle_bond(j, *claim_id, false);
                    // A mistaken claim (e.g. before the merge) can be made again.
                    self.set_claim_by_pr(j, &pr_key, None);
                }
            }
            Action::RejectClaim { claim_id, reason } => {
                need_admin("reject claims")?;
                self.exec_reject_claim(j, *claim_id, reason, height)?;
            }
            Action::JoinJury { bond } => {
                need_owner("join the jury pool")?;
                if self.is_sybil(sender) {
                    bail!("this account is flagged as a sybil");
                }
                if self.soul_of(sender, height) < JUROR_MIN_SOUL {
                    bail!("jurors need at least {} Soul (you have {})", fmt_soul(JUROR_MIN_SOUL), fmt_soul(self.soul_of(sender, height)));
                }
                let existing = self.poc.jurors.get(sender).map(|r| r.bond).unwrap_or(0);
                let total = existing.checked_add(*bond).ok_or_else(|| anyhow!("bond overflow"))?;
                if total < JUROR_BOND {
                    bail!("the juror bond is at least {}", fmt_fang(JUROR_BOND));
                }
                self.j_debit(j, sender, *bond)?;
                let key = sender.to_string();
                self.pj(j).jurors.touch(&self.poc.jurors, &key);
                let r = self.poc.jurors.entry(key).or_insert(Juror { bond: 0, joined_at: height, active: true, served: 0 });
                r.bond = total;
                if !r.active {
                    // A benched juror coming back is new to the draw (`State::seance_cutoff`).
                    r.joined_at = height;
                    r.active = true;
                }
            }
            Action::LeaveJury {} => {
                need_owner("leave the jury pool")?;
                let r = self.poc.jurors.get(sender).cloned().ok_or_else(|| anyhow!("you're not in the jury pool"))?;
                if let Some(s) = self.poc.open_seance.and_then(|id| self.poc.seances.get(&id)) {
                    if s.jurors.contains_key(sender) {
                        bail!("you're on the jury of Séance #{}; leave after it closes (block {})", s.id, s.reveal_until);
                    }
                }
                let key = sender.to_string();
                self.pj(j).jurors.touch(&self.poc.jurors, &key);
                self.poc.jurors.remove(sender);
                self.j_credit(j, sender, r.bond);
            }
            Action::CommitVotes { seance_id, commitment, pairs_digest } => {
                let s = self.poc.seances.get(seance_id).ok_or_else(|| anyhow!("no Séance #{seance_id}"))?;
                if s.closed {
                    bail!("Séance #{seance_id} is closed");
                }
                let Some(seat) = s.jurors.get(sender) else { bail!("you're not on the jury of Séance #{seance_id}") };
                if height > s.commit_until {
                    bail!("sealed votes for Séance #{seance_id} closed at block {}; reveal now", s.commit_until);
                }
                // The judgment is for the pairs (and PRs) its juror compared (audit gap-7-6).
                if seat_pairs_digest(self, *seance_id, sender, &seat.pairs) != *pairs_digest {
                    bail!("the pairs this judgment was made on aren't your seat's in Séance #{seance_id} (claims or PRs differ): the node they were read from may be misreporting them");
                }
                let seat = self.seance_mut(j, *seance_id).unwrap().jurors.get_mut(sender).unwrap();
                seat.commitment = Some(commitment.clone());
            }
            Action::RevealVotes { seance_id, votes, spam, salt } => {
                let s = self.poc.seances.get(seance_id).ok_or_else(|| anyhow!("no Séance #{seance_id}"))?;
                if s.closed {
                    bail!("Séance #{seance_id} is closed");
                }
                let seat = s.jurors.get(sender).ok_or_else(|| anyhow!("you're not on the jury of Séance #{seance_id}"))?;
                if height <= s.commit_until {
                    bail!("reveals for Séance #{seance_id} open after block {}", s.commit_until);
                }
                if height > s.reveal_until {
                    bail!("reveals for Séance #{seance_id} closed at block {}", s.reveal_until);
                }
                let commitment = seat.commitment.as_ref().ok_or_else(|| anyhow!("you didn't seal a judgment for Séance #{seance_id}"))?;
                if seat.votes.is_some() {
                    bail!("you already revealed for Séance #{seance_id}");
                }
                if votes.len() != seat.pairs.len() {
                    bail!("expected {} votes (one per assigned pair), got {}", seat.pairs.len(), votes.len());
                }
                if seance_commitment(*seance_id, sender, votes, spam, salt) != *commitment {
                    bail!("this reveal doesn't match your sealed judgment");
                }
                let seen: BTreeSet<u64> = seat.pairs.iter().flatten().copied().collect();
                let mut uniq = BTreeSet::new();
                for id in spam {
                    if !seen.contains(id) || !uniq.insert(*id) {
                        bail!("spam flags must be distinct claims from your assigned pairs");
                    }
                }
                let seat = self.seance_mut(j, *seance_id).unwrap().jurors.get_mut(sender).unwrap();
                seat.votes = Some(votes.clone());
                seat.spam = spam.clone();
            }
            Action::FundPool { amount } => {
                if *amount == 0 {
                    bail!("fund with more than 0");
                }
                self.j_debit(j, sender, *amount)?;
                self.to_pool(j, *amount);
            }
            Action::WithdrawVested {} => {
                need_owner("withdraw rewards")?;
                let (_, available, _) = self.grant_totals(sender, height);
                if available == 0 {
                    bail!("nothing has vested yet");
                }
                let grants = self.grants_mut(j, sender);
                for g in grants.values_mut() {
                    g.withdrawn = g.vested(height);
                }
                grants.retain(|_, g| g.withdrawn < g.amount);
                if grants.is_empty() {
                    self.poc.grants.remove(sender);
                }
                self.j_credit(j, sender, available);
            }
            Action::Clawback { account, .. } => {
                need_admin("claw back rewards")?;
                self.exec_clawback(j, account, height)?;
            }
            _ => bail!("not a contribution action"),
        }
        Ok(())
    }

    // ---- levers: the bodies of the former admin actions, with no signer checks. Admin-signed
    // txs (dev and test chains) call them after checking the signer; governance proposals
    // (`ProposalKind`, P-GOV) call them when they execute. See chain.rs for the others.

    /// Puts a repo in the contribution pool with `weight` (1–`MAX_REPO_WEIGHT`), or with 0 takes
    /// it out, refunding and closing the claims that waited on it.
    pub(crate) fn exec_set_repo(&mut self, j: &mut Journal, repo: &str, weight: u64, height: u64) -> Result<()> {
        if weight > MAX_REPO_WEIGHT {
            bail!("repo weight must be 0–{MAX_REPO_WEIGHT} (0 removes the repo)");
        }
        let k = repo.to_ascii_lowercase();
        self.pj(j).repos.touch(&self.poc.repos, &k);
        if weight == 0 {
            self.poc.repos.remove(&k).ok_or_else(|| anyhow!("{repo} isn't in the contribution pool"))?;
            // Claims waiting on it would be stuck: refund and close them.
            let stuck: Vec<(u64, u64)> = self.poc.claims.values().filter(|c| c.repo == k && matches!(c.status, ClaimStatus::Pending | ClaimStatus::Verified)).map(|c| (c.id, c.pr)).collect();
            for (id, pr) in stuck {
                self.settle_bond(j, id, true);
                self.set_status(j, id, ClaimStatus::Rejected, height, Some("the repo left the contribution pool; bond refunded".into()));
                self.set_claim_by_pr(j, &format!("{k}#{pr}"), None);
            }
        } else {
            self.poc.repos.insert(k, weight);
        }
        Ok(())
    }

    /// Grants Soul (milli-Soul, 1–`MAX_SOUL_GRANT` per grant). Sybils get none.
    pub(crate) fn exec_grant_soul(&mut self, j: &mut Journal, to: &str, amount: u64, height: u64) -> Result<()> {
        if amount == 0 || amount > MAX_SOUL_GRANT {
            bail!("a Soul grant is 1–{MAX_SOUL_GRANT} milli-Soul");
        }
        let to = self.resolve(to)?;
        if self.is_sybil(&to) {
            bail!("{} is flagged as a sybil", short(&to));
        }
        self.add_soul(j, &to, amount as i64, height);
        Ok(())
    }

    /// Flags an account as a sybil (`by`: who decided, e.g. an admin or a proposal): zeroes its
    /// Soul, rejects its open claims, claws back its unvested rewards, slashes whoever vouched.
    pub(crate) fn exec_flag_sybil(&mut self, j: &mut Journal, account: &str, by: &str, reason: &str, height: u64) -> Result<()> {
        let who = self.resolve(account)?;
        if self.is_sybil(&who) {
            bail!("{} is already flagged", short(&who));
        }
        self.flag_sybil(j, &who, by, reason, height);
        Ok(())
    }

    /// Lifts a sybil flag (`UnflagSybil`, the appeal path for `FlagSybil`, audit F54): the account
    /// can vouch, be vouched for, claim (its GitHub handle stays credited to it), earn Soul and
    /// join juries again. Nothing the flag took comes back (Soul, bonds, rejected claims,
    /// clawed-back rewards, voided vouches); governance can grant Soul separately.
    pub(crate) fn exec_unflag_sybil(&mut self, j: &mut Journal, account: &str) -> Result<()> {
        let who = self.resolve(account)?;
        if !self.is_sybil(&who) {
            bail!("{} isn't flagged as a sybil", short(&who));
        }
        self.pj(j).sybils.touch(&self.poc.sybils, &who);
        self.poc.sybils.remove(&who);
        Ok(())
    }

    /// Rejects a claim (say, its PR was reverted): slashes the bond, or for a scored claim claws
    /// back its unvested reward (and its vouchers' cuts) and the Soul it earned.
    pub(crate) fn exec_reject_claim(&mut self, j: &mut Journal, claim_id: u64, reason: &str, height: u64) -> Result<()> {
        let c = self.poc.claims.get(&claim_id).cloned().ok_or_else(|| anyhow!("no claim #{claim_id}"))?;
        match c.status {
            // Never verified: the PR wasn't proven to be theirs, so its author can still claim it.
            ClaimStatus::Pending => {
                self.settle_bond(j, claim_id, false);
                self.set_claim_by_pr(j, &format!("{}#{}", c.repo, c.pr), None);
            }
            ClaimStatus::Verified | ClaimStatus::InSeance => self.settle_bond(j, claim_id, false),
            ClaimStatus::Scored => {
                if let Some(g) = c.grant {
                    self.claw_back(j, &c.claimant, Some(g), height);
                }
                let cuts: Vec<(String, u64)> = self.poc.grants.iter().flat_map(|(a, gs)| gs.values().filter(|g| g.source == GrantSource::Vouch { claim_id }).map(move |g| (a.clone(), g.id))).collect();
                for (voucher, g) in cuts {
                    self.claw_back(j, &voucher, Some(g), height);
                }
                self.add_soul(j, &c.claimant, -(c.soul_awarded as i64), height);
            }
            ClaimStatus::Spam | ClaimStatus::Rejected => bail!("claim #{claim_id} is already {:?}", c.status),
        }
        self.set_status(j, claim_id, ClaimStatus::Rejected, height, Some(reason.to_string()));
        Ok(())
    }

    /// Returns an account's unvested rewards to the pool. Returns what came back (never 0).
    pub(crate) fn exec_clawback(&mut self, j: &mut Journal, account: &str, height: u64) -> Result<u64> {
        let who = self.resolve(account)?;
        match self.claw_back(j, &who, None, height) {
            0 => bail!("{} has no unvested rewards", short(&who)),
            back => Ok(back),
        }
    }

    fn flag_sybil(&mut self, j: &mut Journal, who: &str, by: &str, reason: &str, height: u64) {
        let key = who.to_string();
        self.pj(j).sybils.touch(&self.poc.sybils, &key);
        self.poc.sybils.insert(key.clone(), Sybil { at: height, by: by.to_string(), reason: reason.to_string() });
        if self.poc.souls.contains_key(who) {
            self.soul_mut(j, who, height).points = 0;
        }
        let open: Vec<(u64, ClaimStatus, String)> = self.poc.claims.values().filter(|c| c.claimant == who && c.status.is_open()).map(|c| (c.id, c.status, format!("{}#{}", c.repo, c.pr))).collect();
        for (id, status, pr_key) in open {
            self.settle_bond(j, id, false);
            self.set_status(j, id, ClaimStatus::Rejected, height, Some("claimant flagged as a sybil".into()));
            if status == ClaimStatus::Pending {
                self.set_claim_by_pr(j, &pr_key, None);
            }
        }
        self.claw_back(j, who, None, height);
        if let Some(r) = self.poc.jurors.get(who).cloned() {
            self.pj(j).jurors.touch(&self.poc.jurors, &key);
            self.poc.jurors.remove(who);
            self.to_pool(j, r.bond);
        }
        // Whoever vouched for it loses the bond and Soul; vouches it gave are void. That covers
        // a vouch withdrawn while its voucher still answers for it (`DroppedVouch`, audit
        // xcut-economic-opus-2): its bond is still held.
        let touched: Vec<String> = self.poc.vouches.iter().filter(|(to, v)| *to == who || v.voucher == who).map(|(to, _)| to.clone()).collect();
        for to in touched {
            let v = self.poc.vouches.get(&to).cloned().unwrap();
            self.pj(j).vouches.touch(&self.poc.vouches, &to);
            self.poc.vouches.remove(&to);
            self.to_pool(j, v.bond);
            if to == who {
                self.add_soul(j, &v.voucher, -(SYBIL_VOUCHER_SOUL as i64), height);
            }
        }
        let dropped: Vec<(String, DroppedVouch)> = self.poc.dropped_vouches.iter().filter(|(k, _)| k.split_once('/').is_some_and(|(to, by)| to == who || by == who)).map(|(k, d)| (k.clone(), d.clone())).collect();
        for (key, d) in dropped {
            self.set_dropped_vouch(j, &key, None);
            self.to_pool(j, d.bond);
            if d.bond > 0 && key.split_once('/').is_some_and(|(to, _)| to == who) {
                self.add_soul(j, key.rsplit_once('/').unwrap().1, -(SYBIL_VOUCHER_SOUL as i64), height);
            }
        }
        // And its vouchers' cuts of its rewards go back with its own (as `exec_reject_claim`).
        let mine: BTreeSet<u64> = self.poc.claims.values().filter(|c| c.claimant == who).map(|c| c.id).collect();
        let cuts: Vec<(String, u64)> = self.poc.grants.iter().flat_map(|(a, gs)| gs.values().filter(|g| matches!(g.source, GrantSource::Vouch { claim_id } if mine.contains(&claim_id))).map(move |g| (a.clone(), g.id))).collect();
        for (voucher, g) in cuts {
            self.claw_back(j, &voucher, Some(g), height);
        }
    }

    /// Plain-English summary of a contribution tx (see `State::describe`).
    pub fn describe_poc(&self, tx: &Tx, from: &str, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
        let who = |a: &str| match self.resolve(a) {
            Ok(addr) => primary_name(&addr).unwrap_or_else(|| short(&addr)),
            Err(_) => a.to_string(),
        };
        let claim = |id: &u64| self.poc.claims.get(id).map(|c| format!("claim #{id} ({}#{})", c.repo, c.pr)).unwrap_or_else(|| format!("claim #{id}"));
        match &tx.action {
            Action::SetRepo { repo, weight: 0 } => format!("{from} removed {repo} from the contribution pool"),
            Action::SetRepo { repo, weight } => format!("{from} put {repo} in the contribution pool (weight {weight})"),
            Action::GrantSoul { to, amount, reason } => format!("{from} granted {} {} Soul: “{reason}”", who(to), fmt_soul(*amount)),
            Action::Vouch { account } => format!("{from} vouched for {}, staking {}", who(account), fmt_fang(VOUCH_BOND)),
            Action::Unvouch { account } => format!("{from} withdrew their vouch for {}", who(account)),
            Action::FlagSybil { account, reason } => format!("{from} flagged {} as a sybil: “{reason}”", who(account)),
            Action::SubmitClaim { repo, pr, familiar_key } => {
                let fam = familiar_key.as_ref().or(Some(&tx.signer_key_id().unwrap_or_default())).and_then(|k| self.familiars.get(&tx.from)?.get(k).cloned()).map(|l| format!(" (work by their Familiar “{l}”)")).unwrap_or_default();
                format!("{from} claimed {repo}#{pr} for the contribution pool{fam}")
            }
            Action::AttestClaim { claim_id, ok: true, .. } => format!("{from} attested {}: GitHub confirms the merge", claim(claim_id)),
            Action::AttestClaim { claim_id, ok: false, .. } => format!("{from} attested {}: GitHub doesn't confirm it", claim(claim_id)),
            Action::RejectClaim { claim_id, reason } => format!("{from} rejected {}: “{reason}”", claim(claim_id)),
            Action::JoinJury { bond } => format!("{from} joined the Séance jury pool, bonding {}", fmt_fang(*bond)),
            Action::LeaveJury {} => format!("{from} left the Séance jury pool"),
            Action::CommitVotes { seance_id, .. } => format!("{from} sealed their judgment for Séance #{seance_id}"),
            Action::RevealVotes { seance_id, votes, spam, .. } => {
                let flagged = if spam.is_empty() { String::new() } else { format!(", flagging {} as spam", spam.len()) };
                format!("{from} revealed their judgment for Séance #{seance_id} ({} comparisons{flagged})", votes.len())
            }
            Action::FundPool { amount } => format!("{from} added {} to the contribution pool", fmt_fang(*amount)),
            Action::WithdrawVested {} => format!("{from} withdrew their vested contribution rewards"),
            Action::Clawback { account, reason } => format!("{from} clawed back {}'s unvested rewards: “{reason}”", who(account)),
            _ => String::new(),
        }
    }

    // ------------------------------------------------------------------------------ Séances

    /// Séances may open every this many epochs: the fewest a Séance fits in, so each one closes
    /// by the next opening.
    pub fn seance_every_epochs(&self) -> u64 {
        self.params.poc.seance_blocks.div_ceil(self.params.epoch_blocks.max(2)).max(1)
    }

    /// The first height at or after `height` where a Séance may open: the last block of every
    /// [`State::seance_every_epochs`]th epoch. Fixed in advance; nothing on chain moves it.
    pub fn next_seance_height(&self, height: u64) -> u64 {
        let n = self.seance_every_epochs();
        let epoch = (self.params.epoch_of(height) + 1).div_ceil(n) * n - 1;
        self.params.last_of(epoch)
    }

    /// Jurors and claims a Séance opening at the end of `epoch` may draw on became eligible
    /// before this height (joined; revert window over): the start of `epoch - 1`. The Séance's
    /// seed, `value(epoch)`, comes from the reveals of `epoch - 1`, so whoever could still add a
    /// juror or time a claim couldn't know how they'd be drawn. Before epoch 2 nothing is.
    pub fn seance_cutoff(&self, epoch: u64) -> u64 {
        epoch.saturating_sub(1).saturating_mul(self.params.epoch_blocks.max(2))
    }

    /// After every block's txs: close the open Séance once its reveals are over; at an epoch
    /// boundary, refund stale claims and, on the cadence, open the next Séance.
    pub fn poc_end_block(&mut self, height: u64) {
        if !self.poc.is_active() {
            return;
        }
        if let Some(id) = self.poc.open_seance {
            if self.poc.seances.get(&id).is_some_and(|s| height >= s.reveal_until) {
                self.close_seance(id, height);
            }
        }
        if self.params.is_boundary(height) {
            self.refund_stale_claims(height);
            if self.poc.open_seance.is_none() && self.next_seance_height(height) == height {
                self.open_seance(height, self.params.epoch_of(height));
            }
            self.sweep_dropped_vouches(height);
        }
        if self.poc.next_seance_at <= height {
            self.poc.next_seance_at = self.next_seance_height(height + 1);
        }
    }

    /// At an epoch boundary (after any Séance opens): hands back the held bonds of withdrawn
    /// vouches whose lock is over and whose vouchee no `FlagSybil` proposal names, and forgets
    /// the records no Séance can need any more (every later opening's cutoff is after them).
    fn sweep_dropped_vouches(&mut self, height: u64) {
        if self.poc.dropped_vouches.is_empty() {
            return;
        }
        let mut j = Journal::default();
        let lock = self.params.poc.vouch_lock_blocks;
        let next_cutoff = self.seance_cutoff(self.params.epoch_of(height) + 1);
        let all: Vec<(String, DroppedVouch)> = self.poc.dropped_vouches.iter().map(|(k, d)| (k.clone(), d.clone())).collect();
        for (key, d) in all {
            let Some((to, voucher)) = key.rsplit_once('/') else { continue };
            let mut next = d.clone();
            if d.bond > 0 && height >= d.at.saturating_add(lock) && !self.sybil_flag_proposed(to) {
                self.j_credit(&mut j, voucher, d.bond);
                next.bond = 0;
            }
            if next.bond == 0 && next.dropped_at < next_cutoff {
                self.set_dropped_vouch(&mut j, &key, None);
            } else if next != d {
                self.set_dropped_vouch(&mut j, &key, Some(next));
            }
        }
    }

    /// Claims claimants can't help (validators never settled them) are refunded after
    /// `claim_ttl_blocks`.
    fn refund_stale_claims(&mut self, height: u64) {
        let mut j = Journal::default();
        let ttl = self.params.poc.claim_ttl_blocks;
        // Open claims only (an index, `OpenClaims`), in id order like the claims themselves.
        let open = self.poc.claims.indexed();
        let stale: Vec<(u64, String)> = open.iter().filter_map(|id| self.poc.claims.get(id)).filter(|c| c.status == ClaimStatus::Pending && height >= c.submitted_at.saturating_add(ttl)).map(|c| (c.id, format!("{}#{}", c.repo, c.pr))).collect();
        for (id, pr_key) in stale {
            self.settle_bond(&mut j, id, true);
            self.set_status(&mut j, id, ClaimStatus::Rejected, height, Some("validators didn't settle it in time; bond refunded".into()));
            self.set_claim_by_pr(&mut j, &pr_key, None);
        }
    }

    /// Claims a juror mustn't judge in a Séance with this `cutoff`: their own, and those of people
    /// they vouch for or who vouch for them, counting vouches withdrawn at or after the cutoff
    /// ([`DroppedVouch`]): a Séance's seed is public before it opens, so a conflict dropped then
    /// would seat a juror on a claim it mustn't judge (audit F53), and ejecting the juror instead
    /// would let the vouchee steer the draw (audit review-fix-poc-2). Conflicts are as of the
    /// cutoff, when nobody could know the draw.
    fn conflicted(&self, juror: &str, claimant: &str, cutoff: u64) -> bool {
        let tied = |vouchee: &str, voucher: &str| self.poc.vouches.get(vouchee).is_some_and(|v| v.voucher == voucher) || self.poc.dropped_vouches.get(&vouch_key(vouchee, voucher)).is_some_and(|d| d.dropped_at >= cutoff);
        juror == claimant || tied(claimant, juror) || tied(juror, claimant)
    }

    /// Opens a Séance at the end of `epoch` if claims of at least 2 people and `MIN_JURY` jurors
    /// are eligible (see [`State::seance_cutoff`]); otherwise the next chance is the next opening
    /// on the cadence. A claim is only ever compared with another person's (audit
    /// xcut-economic-opus-1: two of one person's claims tie, which would float weak claims over
    /// the threshold), so a person alone with their claims waits for company.
    fn open_seance(&mut self, height: u64, epoch: u64) {
        let mut j = Journal::default();
        let times = self.params.poc.clone();
        let cutoff = self.seance_cutoff(epoch);
        let open = self.poc.claims.indexed();
        let claims: Vec<(u64, String)> = open
            .iter()
            .filter_map(|id| self.poc.claims.get(id))
            .filter(|c| c.status == ClaimStatus::Verified && c.verified_at.is_some_and(|v| v.saturating_add(times.claim_maturity_blocks) < cutoff) && !self.is_sybil(&c.claimant) && self.poc.repos.contains_key(&c.repo))
            .take(MAX_SEANCE_CLAIMS)
            .map(|c| (c.id, c.claimant.clone()))
            .collect();
        let people = claims.iter().map(|(_, who)| who).collect::<BTreeSet<_>>().len() as u64;
        if people < 2 {
            return;
        }
        let id = self.poc.next_seance_id;
        let Some(seed) = crate::beacon::seance_seed(self, epoch, id).map(hex::encode) else { return };
        // Sortition: each eligible juror gets its own rank from the seed and the best-ranked who
        // can judge a pair sit, so a juror joining or leaving can't reshuffle anyone else.
        let mut pool: Vec<(u64, String)> = self
            .poc
            .jurors
            .iter()
            .filter(|(a, r)| r.active && r.joined_at < cutoff && !self.is_sybil(a) && self.soul_of(a, height) >= JUROR_MIN_SOUL)
            .map(|(a, _)| (Rng::new(&seed, &[b"jury".as_slice(), a.as_bytes()].concat()).next(), a.clone()))
            .collect();
        pool.sort();
        let mut seats = BTreeMap::new();
        for (_, juror) in pool {
            if seats.len() >= JURY_SIZE {
                break;
            }
            // Pairs of claims by two different people, neither of whom the juror is tied to.
            let allowed: Vec<&(u64, String)> = claims.iter().filter(|(_, who)| !self.conflicted(&juror, who, cutoff)).collect();
            let mut pairs = vec![];
            for (i, (a, who)) in allowed.iter().enumerate() {
                pairs.extend(allowed[i + 1..].iter().filter(|(_, other)| other != who).map(|(b, _)| [*a, *b]));
            }
            if pairs.is_empty() {
                continue;
            }
            let mut rng = Rng::new(&seed, &[b"pairs".as_slice(), &id.to_le_bytes(), juror.as_bytes()].concat());
            rng.shuffle(&mut pairs);
            pairs.truncate(PAIRS_PER_JUROR);
            for p in pairs.iter_mut() {
                if rng.next() & 1 == 1 {
                    p.swap(0, 1);
                }
            }
            seats.insert(juror, JurySeat { pairs, ..Default::default() });
        }
        if seats.len() < MIN_JURY {
            return;
        }
        let pool_addr = self.pool();
        let budget = (bps(self.account(&pool_addr).balance, SEANCE_POOL_BPS) as u128 * people.min(FULL_BUDGET_PEOPLE) as u128 / FULL_BUDGET_PEOPLE as u128) as u64;
        if budget > 0 {
            let _ = self.j_debit(&mut j, &pool_addr, budget);
        }
        for (cid, _) in &claims {
            if let Some(c) = self.claim_mut(&mut j, *cid) {
                c.status = ClaimStatus::InSeance;
                c.seance = Some(id);
            }
        }
        for juror in seats.keys() {
            if let Some(r) = self.juror_mut(&mut j, juror) {
                r.served += 1;
            }
        }
        let commit_until = height + times.commit_blocks();
        let reveal_until = commit_until + times.reveal_blocks();
        self.poc.seances.insert(id, Seance { id, opened_at: height, commit_until, reveal_until, seed, claims: claims.iter().map(|(c, _)| *c).collect(), budget, jurors: seats, closed: false, results: None });
        self.poc.next_seance_id += 1;
        self.poc.open_seance = Some(id);
    }

    fn close_seance(&mut self, id: u64, height: u64) {
        let mut j = Journal::default();
        let s = self.poc.seances.get(&id).cloned().expect("open Séance exists");
        // Claims still in this Séance (an admin may have rejected some since it opened).
        let live: BTreeSet<u64> = s.claims.iter().copied().filter(|c| self.poc.claims.get(c).is_some_and(|c| c.status == ClaimStatus::InSeance && c.seance == Some(id))).collect();
        let mut wins2: BTreeMap<u64, u64> = BTreeMap::new();
        let mut comps: BTreeMap<u64, u64> = BTreeMap::new();
        let mut seen: BTreeMap<u64, u64> = BTreeMap::new();
        let mut flags: BTreeMap<u64, u64> = BTreeMap::new();
        // Jurors flagged as sybils since the Séance opened don't count. Too few reveals and nothing
        // is decided: the claims carry over and the budget goes back (no-shows still pay).
        let seats: BTreeMap<&String, &JurySeat> = s.jurors.iter().filter(|(j, _)| !self.is_sybil(j)).collect();
        let quorate = seats.values().filter(|seat| seat.votes.is_some()).count() >= MIN_JURY;
        for seat in seats.values().filter(|_| quorate) {
            let Some(votes) = &seat.votes else { continue };
            let mut saw = BTreeSet::new();
            for (pair, v) in seat.pairs.iter().zip(votes.bytes()) {
                let [a, b] = *pair;
                if !live.contains(&a) || !live.contains(&b) {
                    continue;
                }
                saw.insert(a);
                saw.insert(b);
                *comps.entry(a).or_default() += 1;
                *comps.entry(b).or_default() += 1;
                match v {
                    b'a' => *wins2.entry(a).or_default() += 2,
                    b'b' => *wins2.entry(b).or_default() += 2,
                    _ => {
                        *wins2.entry(a).or_default() += 1;
                        *wins2.entry(b).or_default() += 1;
                    }
                }
            }
            for c in &saw {
                *seen.entry(*c).or_default() += 1;
            }
            for c in seat.spam.iter().filter(|c| saw.contains(c)) {
                *flags.entry(*c).or_default() += 1;
            }
        }
        let score: BTreeMap<u64, u64> = comps.iter().map(|(c, n)| (*c, wins2.get(c).copied().unwrap_or(0) * PPM / (2 * n))).collect();
        let spam_verdict = |c: &u64| {
            let f = flags.get(c).copied().unwrap_or(0);
            f >= SPAM_MIN_FLAGS && 2 * f > seen.get(c).copied().unwrap_or(0)
        };
        // Each person's win rate, pooled over every comparison of every claim of theirs the jury
        // judged and didn't mark spam (audit xcut-economic-opus-1, round 4): a person's claims are
        // rated together, never by the best of them. Each claim's score is a win rate over a
        // handful of comparisons, so the best of k claims of the same quality scores higher the
        // more there are; the pooled rate doesn't (more claims only make it steadier), and the
        // claims below the threshold count in it too, so nothing is selected.
        // Kept by claim: each live claim maps to its claimant's rate.
        let claim_rate: BTreeMap<u64, u64> = {
            let mut pooled: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
            for c in live.iter().filter(|c| !spam_verdict(c)) {
                if let Some(n) = comps.get(c) {
                    let p = pooled.entry(self.poc.claims[c].claimant.as_str()).or_default();
                    p.0 += wins2.get(c).copied().unwrap_or(0);
                    p.1 += n;
                }
            }
            live.iter().filter_map(|c| pooled.get(self.poc.claims[c].claimant.as_str()).map(|(w, n)| (*c, w * PPM / (2 * n)))).collect()
        };
        let rate_of = |c: &u64| claim_rate.get(c).copied().unwrap_or(0);
        // What a claim is paid and credited at: its claimant's pooled rate, capped at its own
        // score. The cap keeps the pooled rate a ceiling, never a lift: a weak claim can't borrow
        // the quality of the person's other claims, which may be in a lighter repo, and be paid
        // at its own repo's weight (audit review-r4-poc-2). The best claims still get no more
        // than the pooled rate, so nothing is selected.
        let paid_rate = |c: &u64| rate_of(c).min(score.get(c).copied().unwrap_or(0));
        // Each claim's rank among its claimant's accepted claims, best score first (ties by id):
        // its Soul is its `paid_rate`'s, counting `>> CLAIM_RANK_SHIFT` per rank.
        let soul_rank: BTreeMap<u64, usize> = {
            let mut by_person: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
            for c in &live {
                if let Some(sc) = score.get(c).copied().filter(|sc| *sc >= MIN_SCORE_PPM && rate_of(c) >= MIN_SCORE_PPM && !spam_verdict(c)) {
                    by_person.entry(self.poc.claims[c].claimant.as_str()).or_default().push((sc, *c));
                }
            }
            by_person
                .into_values()
                .flat_map(|mut cs| {
                    cs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                    cs.into_iter().enumerate().map(|(rank, (_, c))| (c, rank))
                })
                .collect()
        };
        let mut spam: BTreeSet<u64> = BTreeSet::new();
        let mut results = SeanceResults::default();
        let mut accepted: Vec<u64> = vec![];
        let mut soul_changes: Vec<(String, i64, Option<u64>)> = vec![];
        for c in &live {
            let claimant = self.poc.claims[c].claimant.clone();
            let Some(sc) = score.get(c).copied() else {
                // Nobody judged it: back in line for the next Séance.
                let cl = self.claim_mut(&mut j, *c).unwrap();
                cl.status = ClaimStatus::Verified;
                cl.seance = None;
                results.carried_over += 1;
                continue;
            };
            let f = flags.get(c).copied().unwrap_or(0);
            self.claim_mut(&mut j, *c).unwrap().score_ppm = Some(sc);
            if spam_verdict(c) {
                self.settle_bond(&mut j, *c, false);
                self.set_status(&mut j, *c, ClaimStatus::Spam, height, Some(format!("{f} of the jurors who saw it flagged it as spam")));
                soul_changes.push((claimant, -(SPAM_SOUL as i64), None));
                spam.insert(*c);
                results.spam += 1;
            } else if sc < MIN_SCORE_PPM {
                self.settle_bond(&mut j, *c, true);
                self.set_status(&mut j, *c, ClaimStatus::Scored, height, Some("scored below the reward threshold".into()));
                results.below_threshold += 1;
            } else if rate_of(c) < MIN_SCORE_PPM {
                self.settle_bond(&mut j, *c, true);
                self.set_status(&mut j, *c, ClaimStatus::Scored, height, Some("its claimant's claims in this Séance together scored below the reward threshold".into()));
                results.below_threshold += 1;
            } else {
                self.settle_bond(&mut j, *c, true);
                self.set_status(&mut j, *c, ClaimStatus::Scored, height, None);
                soul_changes.push((claimant, (ranked(CLAIM_SOUL as u128 * paid_rate(c) as u128, soul_rank[c]) / PPM as u128) as i64, Some(*c)));
                accepted.push(*c);
                results.accepted += 1;
            }
        }
        // Grade jurors against the outcome.
        let mut juror_weights: Vec<(String, u128)> = vec![];
        let mut seat_updates: Vec<(String, Option<u64>, i64)> = vec![];
        for (juror, seat) in seats {
            let Some(votes) = &seat.votes else {
                let slash = self.poc.jurors.get(juror).map(|r| bps(r.bond, NO_SHOW_SLASH_BPS)).unwrap_or(0);
                if let Some(r) = self.juror_mut(&mut j, juror) {
                    r.bond -= slash;
                    r.active = false;
                }
                self.to_pool(&mut j, slash);
                soul_changes.push((juror.clone(), -(NO_SHOW_SOUL as i64), None));
                seat_updates.push((juror.clone(), None, -(NO_SHOW_SOUL as i64)));
                results.no_shows += 1;
                continue;
            };
            if !quorate {
                seat_updates.push((juror.clone(), None, 0));
                continue;
            }
            let (mut points, mut total) = (0u64, 0u64);
            for (pair, v) in seat.pairs.iter().zip(votes.bytes()) {
                let (Some(sa), Some(sb)) = (score.get(&pair[0]), score.get(&pair[1])) else { continue };
                let outcome = if *sa > sb + TIE_MARGIN_PPM { b'a' } else if *sb > sa + TIE_MARGIN_PPM { b'b' } else { b'=' };
                points += if v == outcome { 2 } else if v == b'=' || outcome == b'=' { 1 } else { 0 };
                total += 2;
            }
            // Spam flags are graded too (F23): a flag on a claim enough jurors saw to decide
            // (`SPAM_MIN_FLAGS`) that the jury didn't uphold counts as a disagreement, so
            // flagging everything costs agreement, Soul and the jury reward. Upheld flags and
            // flags on claims too few saw neither add nor cost.
            let saw: BTreeSet<u64> = seat.pairs.iter().filter(|[a, b]| live.contains(a) && live.contains(b)).flatten().copied().collect();
            let overruled = seat.spam.iter().filter(|c| saw.contains(c) && score.contains_key(c) && seen.get(c).copied().unwrap_or(0) >= SPAM_MIN_FLAGS && !spam.contains(c)).count() as u64;
            total += 2 * overruled;
            if total == 0 {
                seat_updates.push((juror.clone(), None, 0));
                continue;
            }
            let agreement = points * PPM / total;
            let delta = if agreement >= GOOD_AGREEMENT_PPM { JUROR_GOOD_SOUL as i64 } else if agreement < BAD_AGREEMENT_PPM { -(JUROR_BAD_SOUL as i64) } else { 0 };
            if agreement >= GOOD_AGREEMENT_PPM {
                juror_weights.push((juror.clone(), agreement as u128));
            }
            soul_changes.push((juror.clone(), delta, None));
            seat_updates.push((juror.clone(), Some(agreement), delta));
        }
        // Rewards: jurors by agreement; claims by `paid_rate` (the claimant's pooled rate, at most
        // the claim's own score) × repo weight, per person the heaviest in full and each further
        // one a quarter of the one ranked above it (`ranked`: together under 4/3 of one claim at
        // the person's rate, so volume adds at most a third: audit xcut-economic-opus-1),
        // boosted up to 1.5× by the person's Soul. A person is the claimant account (audit
        // xcut-economic-opus-3): a sponsor's Familiars claim as the sponsor, and every handle an
        // account linked counts together (each handle credits one account, `Poc::handles`).
        let juror_budget = if juror_weights.is_empty() { 0 } else { bps(s.budget, JUROR_SHARE_BPS) };
        let claims_budget = if accepted.is_empty() { 0 } else { s.budget - juror_budget };
        let mut raw: BTreeMap<String, Vec<(u64, u128)>> = BTreeMap::new();
        for c in &accepted {
            let cl = &self.poc.claims[c];
            let w = self.poc.repos.get(&cl.repo).copied().unwrap_or(0) as u128 * paid_rate(c) as u128;
            raw.entry(cl.claimant.clone()).or_default().push((*c, w));
        }
        for cs in raw.values_mut() {
            // Heaviest first; among equal weights (one repo), the better-scored claim ranks first.
            cs.sort_by(|a, b| b.1.cmp(&a.1).then(score[&b.0].cmp(&score[&a.0])).then(a.0.cmp(&b.0)));
            for (rank, (_, w)) in cs.iter_mut().enumerate() {
                *w = ranked(*w, rank);
            }
        }
        let person_weights: Vec<(String, u128)> = raw
            .iter()
            .map(|(who, cs)| {
                let soul = self.soul_of(who, height).min(BOOST_CAP_SOUL) as u128;
                let boost = PPM as u128 + soul * (PPM as u128 / 2) / BOOST_CAP_SOUL as u128;
                (who.clone(), cs.iter().map(|(_, w)| w).sum::<u128>() * boost / PPM as u128)
            })
            .collect();
        let mut paid = 0u64;
        for (person, amount) in split(claims_budget, &person_weights) {
            let per_claim: Vec<(String, u128)> = raw[&person].iter().map(|(c, w)| (c.to_string(), *w)).collect();
            for (c, p) in split(amount, &per_claim) {
                let c: u64 = c.parse().unwrap();
                if p == 0 {
                    continue;
                }
                let who = self.poc.claims[&c].claimant.clone();
                let young_vouch = self.poc.vouches.get(&who).filter(|v| height < v.at.saturating_add(self.params.poc.vouch_lock_blocks)).map(|v| v.voucher.clone());
                let cut = if young_vouch.is_some() { bps(p, VOUCHER_CUT_BPS) } else { 0 };
                if let Some(voucher) = young_vouch.filter(|_| cut > 0) {
                    self.add_grant(&mut j, &voucher, cut, height, GrantSource::Vouch { claim_id: c });
                }
                let g = self.add_grant(&mut j, &who, p - cut, height, GrantSource::Claim { claim_id: c });
                let cl = self.claim_mut(&mut j, c).unwrap();
                cl.payout = p;
                cl.grant = g;
                paid += p;
            }
        }
        let mut juror_rewards: BTreeMap<String, u64> = BTreeMap::new();
        for (juror, amount) in split(juror_budget, &juror_weights) {
            if amount > 0 {
                self.add_grant(&mut j, &juror, amount, height, GrantSource::Jury { seance_id: id });
                juror_rewards.insert(juror, amount);
                paid += amount;
            }
        }
        for (who, delta, claim) in soul_changes {
            let d = self.add_soul(&mut j, &who, delta, height);
            if let Some(c) = claim {
                self.claim_mut(&mut j, c).unwrap().soul_awarded = d.max(0) as u64;
            }
        }
        results.paid = paid;
        results.returned = s.budget - paid;
        self.to_pool(&mut j, results.returned);
        let s = self.poc.seances.get_mut(&id).unwrap();
        for (juror, agreement, delta) in seat_updates {
            let seat = s.jurors.get_mut(&juror).unwrap();
            seat.agreement_ppm = agreement;
            seat.soul_delta = delta;
            seat.reward = juror_rewards.get(&juror).copied().unwrap_or(0);
        }
        s.closed = true;
        s.results = Some(results);
        self.poc.open_seance = None;
    }
}

/// `types::seance_pairs_digest` of a juror's seat: each assigned pair's claims with their repos
/// and PR numbers, as `CommitVotes` must bind them.
pub fn seat_pairs_digest(state: &State, seance_id: u64, juror: &str, pairs: &[[u64; 2]]) -> String {
    let claim = |id: u64| state.poc.claims.get(&id).map(|c| (id, c.repo.as_str(), c.pr)).unwrap_or((id, "", 0));
    let pairs: Vec<[(u64, &str, u64); 2]> = pairs.iter().map(|[a, b]| [claim(*a), claim(*b)]).collect();
    crate::types::seance_pairs_digest(seance_id, juror, &pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soul_decays_by_half_lives() {
        assert_eq!(decay(1000, 0, 100), 1000);
        assert_eq!(decay(1000, 100, 100), 500);
        assert_eq!(decay(1000, 200, 100), 250);
        assert_eq!(decay(1000, 50, 100), 750);
        assert_eq!(decay(1000, 6400, 100), 0);
        assert!(decay(1000, 150, 100) < 500 && decay(1000, 150, 100) > 250);
    }

    #[test]
    fn split_is_exact_and_proportional() {
        let w = vec![("a".to_string(), 1u128), ("b".to_string(), 2), ("c".to_string(), 0)];
        let out = split(100, &w);
        assert_eq!(out.iter().map(|(_, v)| v).sum::<u64>(), 100);
        assert_eq!(out[0].1, 33);
        assert_eq!(out[1].1, 67);
        assert_eq!(out[2].1, 0);
        assert!(split(100, &[]).is_empty());
        // A person's claims by rank: a quarter each, 0 once the shift runs out, never a panic.
        assert_eq!((ranked(1_000, 0), ranked(1_000, 1), ranked(1_000, 2)), (1_000, 250, 62));
        assert_eq!((ranked(u128::MAX, 63), ranked(u128::MAX, 64), ranked(u128::MAX, usize::MAX)), (u128::MAX >> 126, 0, 0));
        let w = 100 * PPM as u128;
        assert!((0..MAX_SEANCE_CLAIMS).map(|r| ranked(w, r)).sum::<u128>() * 3 < 4 * w, "all of them under 4/3 of the first");
    }

    #[test]
    fn rng_is_deterministic_and_domain_separated() {
        let (mut a, mut b, mut c) = (Rng::new("s", b"x"), Rng::new("s", b"x"), Rng::new("s", b"y"));
        let (x, y, z) = (a.next(), b.next(), c.next());
        assert_eq!(x, y);
        assert_ne!(x, z);
    }

    /// Audit #11: an account's grants are capped. At the cap, what has fully vested is paid out
    /// to make room; a reward that still doesn't fit goes back to the pool. Nothing is lost.
    #[test]
    fn grants_are_capped_and_vested_ones_make_room() {
        let mut st = State::default();
        st.params.poc.vesting_blocks = 100;
        let who = crypto::address_from_bytes(&[7; 32]);
        // MAX_GRANTS grants of 10 drops: the first 100 fully vested by height 1_000, the rest not.
        let grants = (0..MAX_GRANTS as u64).map(|id| (id, Grant { id, amount: 10, withdrawn: if id == 0 { 4 } else { 0 }, start: if id < 100 { 0 } else { 1_000 }, end: if id < 100 { 100 } else { 1_100 }, source: GrantSource::Jury { seance_id: 0 } })).collect();
        st.poc.grants.insert(who.clone(), grants);
        st.poc.next_grant_id = MAX_GRANTS as u64;
        st.supply = MAX_GRANTS as u64 * 10;
        st.accounts.insert(who.clone(), crate::chain::Account { balance: 4, ..Default::default() });
        st.check_invariants().unwrap();
        // A reward released from an escrow (as a Séance's budget is), granted at height 1_000.
        let reward = |st: &mut State, j: &mut Journal| {
            st.supply += 50;
            st.add_grant(j, &who, 50, 1_000, GrantSource::Claim { claim_id: 1 })
        };
        let mut j = Journal::default();
        let id = reward(&mut st, &mut j).expect("room was made");
        assert_eq!(st.poc.grants[&who].len(), MAX_GRANTS - 100 + 1);
        assert_eq!(st.poc.grants[&who][&id].amount, 50);
        assert_eq!(st.account(&who).balance, 4 + 100 * 10 - 4, "the vested grants were paid out");
        st.check_invariants().unwrap();
        // Fill it again with still-vesting grants: the next reward goes back to the pool.
        let ids: Vec<u64> = (0..99).map(|_| reward(&mut st, &mut j).unwrap()).collect();
        assert_eq!((st.poc.grants[&who].len(), ids.len()), (MAX_GRANTS, 99));
        let pool = st.pool();
        assert_eq!(reward(&mut st, &mut j), None);
        assert_eq!((st.poc.grants[&who].len(), st.account(&pool).balance), (MAX_GRANTS, 50));
        st.check_invariants().unwrap();
        // And all of it rolls back.
        st.rollback(j);
        assert_eq!((st.poc.grants[&who].len(), st.account(&who).balance), (MAX_GRANTS, 4));
    }
}

/// Whole-chain scenarios: Souls and vouches, claims and attestations, a full Séance.
#[cfg(test)]
mod scenarios {
    use super::*;
    use crate::chain::{now_ms, BlockInfo, Chain, Outcome, SummonOpts};
    use crate::crypto::Keys;
    use std::path::PathBuf;

    struct T {
        home: PathBuf,
        c: Chain,
        admin: Keys,
    }

    fn times() -> PocTimes {
        PocTimes { seance_blocks: 30, claim_maturity_blocks: 0, vesting_blocks: 20, vouch_lock_blocks: 100, claim_ttl_blocks: 50, soul_half_life_blocks: 1000 }
    }

    fn k(i: u8) -> Keys {
        Keys::from_seed([i; 32])
    }

    fn setup(tag: &str) -> T {
        let home = std::env::temp_dir().join(format!("blockula-poc-{tag}-{}-{}", now_ms(), std::process::id()));
        // The contribution pool holds only what these tests put in: issuance's pool share (P-ECON's
        // to test) is off, so Séance budgets are exact.
        let params = [("issuance_pool_bps".to_string(), 0)].into_iter().collect();
        Chain::summon(&home, &SummonOpts { contribution: Some(times()), epoch_blocks: 10, params, ..SummonOpts::new("poc", 1) }).unwrap();
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let mut c = Chain::open(&home).unwrap();
        for i in 1..=12u8 {
            c.faucet(&k(i).address(), 100 * DROPS_PER_FANG).unwrap();
        }
        c.produce_block().unwrap();
        T { home, c, admin: Keys::from_seed(seed) }
    }

    impl T {
        fn signed_as(&self, key: &Keys, from: &str, action: Action) -> Tx {
            let mut tx = Tx { chain_id: "poc".into(), genesis_hash: self.c.genesis_hash.clone(), nonce: self.c.next_nonce(from), from: from.into(), fee: 0, tip: 0, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: hex::encode(key.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size());
            tx.sign(key)
        }
        fn tx_as(&mut self, key: &Keys, from: &str, action: Action) -> Result<String> {
            let id = self.c.submit(self.signed_as(key, from, action))?;
            self.c.produce_block()?;
            match self.c.failure(&id) {
                Some((r, _)) => bail!(r.clone()),
                None => Ok(id),
            }
        }
        fn tx(&mut self, who: &Keys, action: Action) -> Result<String> {
            self.tx_as(who, &who.address(), action)
        }
        fn admin(&mut self, action: Action) -> Result<String> {
            let a = self.admin.clone_keys();
            self.tx(&a, action)
        }
        fn bal(&self, who: &Keys) -> u64 {
            self.c.state.account(&who.address()).balance
        }
        fn blocks(&mut self, n: u64) {
            for _ in 0..n {
                self.c.produce_block_with(vec![]).unwrap();
            }
        }
        fn h(&self) -> u64 {
            self.c.height()
        }
        fn soul(&self, who: &Keys) -> u64 {
            self.c.state.soul_of(&who.address(), self.h())
        }
        fn grant(&mut self, who: &Keys, soul: u64) {
            self.admin(Action::GrantSoul { to: who.address(), amount: soul * SOUL, reason: "Season 0 bootstrap".into() }).unwrap();
        }
        fn link(&mut self, who: &Keys, handle: &str) {
            self.tx(who, Action::LinkGithub { handle: handle.into(), gist_id: "abc123".into() }).unwrap();
        }
        fn claim(&mut self, who: &Keys, pr: u64) -> Result<u64> {
            self.tx(who, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr, familiar_key: None })?;
            Ok(self.c.state.poc.next_claim_id - 1)
        }
        fn attest(&mut self, id: u64, ok: bool) -> Result<String> {
            let sha = if ok { "ab".repeat(20) } else { String::new() };
            let note = if ok { String::new() } else { "the PR was closed without merging".into() };
            self.admin(Action::AttestClaim { claim_id: id, ok, merge_sha: sha, note })
        }
        /// Several of the validator's attestations in one block.
        fn attest_all(&mut self, ids: &[u64]) {
            let me = self.admin.address();
            for id in ids {
                let mut tx = Tx { chain_id: "poc".into(), genesis_hash: self.c.genesis_hash.clone(), nonce: self.c.next_nonce(&me), from: me.clone(), fee: 0, tip: 0, valid_until: self.c.height() + 100, action: Action::AttestClaim { claim_id: *id, ok: true, merge_sha: "cd".repeat(20), note: String::new() }, memo: String::new(), public_key: hex::encode(self.admin.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
                tx.fee = min_fee(tx.size());
                self.c.submit(tx.sign(&self.admin)).unwrap();
            }
            self.c.produce_block().unwrap();
        }
        /// Runs empty blocks until a Séance opens (at most a few cadences).
        fn wait_for_seance(&mut self) -> u64 {
            for _ in 0..200 {
                if let Some(id) = self.c.state.poc.open_seance {
                    return id;
                }
                self.blocks(1);
            }
            panic!("no Séance opened by block {}", self.h());
        }
        fn reopen_same_root(self) {
            let root = self.c.state.root();
            drop(self.c);
            assert_eq!(Chain::open(&self.home).unwrap().state.root(), root, "replay reaches the same state");
            std::fs::remove_dir_all(&self.home).ok();
        }
    }

    /// Every drop is somewhere: balances plus everything held in escrow equals the supply.
    fn conserved(s: &State) {
        s.check_invariants().expect("FANG conserved");
    }

    #[test]
    fn souls_vouches_and_sybils() {
        let mut t = setup("souls");
        let (alice, bob, carol, dave, eve) = (k(1), k(2), k(3), k(4), k(5));
        // Admin-only levers (Season 0).
        assert!(t.tx(&alice, Action::GrantSoul { to: bob.address(), amount: SOUL, reason: "x".into() }).unwrap_err().to_string().contains("only validator admins"));
        assert!(t.tx(&alice, Action::SetRepo { repo: "a/b".into(), weight: 1 }).unwrap_err().to_string().contains("only validator admins"));
        assert!(t.admin(Action::GrantSoul { to: bob.address(), amount: MAX_SOUL_GRANT + 1, reason: "x".into() }).is_err());
        assert!(!t.c.state.poc.is_active(), "nothing has happened yet, so the end-of-block step stays off");
        t.grant(&alice, 20);
        assert_eq!(t.soul(&alice), 20 * SOUL);
        t.grant(&bob, 3);
        // Soul decays (half-life 1000 blocks here): about a quarter is gone after 500.
        t.blocks(499);
        assert_eq!(t.soul(&alice), 15 * SOUL);
        t.grant(&alice, 10);
        // Vouching needs 10 Soul and is capped by Soul.
        assert!(t.tx(&bob, Action::Vouch { account: carol.address() }).unwrap_err().to_string().contains("at least 10 Soul"));
        assert!(t.tx(&alice, Action::Vouch { account: alice.address() }).is_err());
        let before = t.bal(&alice);
        t.tx(&alice, Action::Vouch { account: carol.address() }).unwrap();
        assert!(t.bal(&alice) < before - VOUCH_BOND + DROPS_PER_FANG / 100);
        assert!(t.tx(&bob, Action::Vouch { account: carol.address() }).is_err());
        assert!(t.tx(&alice, Action::Vouch { account: carol.address() }).unwrap_err().to_string().contains("already vouched"));
        t.tx(&alice, Action::Vouch { account: dave.address() }).unwrap();
        assert!(t.tx(&alice, Action::Vouch { account: eve.address() }).unwrap_err().to_string().contains("allows 2"));
        conserved(&t.c.state);
        // Locked, then withdrawable.
        assert!(t.tx(&alice, Action::Unvouch { account: dave.address() }).unwrap_err().to_string().contains("locked until"));
        assert!(t.tx(&bob, Action::Unvouch { account: dave.address() }).is_err());
        t.blocks(100);
        t.grant(&alice, 10);
        let before = t.bal(&alice);
        t.tx(&alice, Action::Unvouch { account: dave.address() }).unwrap();
        assert!(t.bal(&alice) > before + VOUCH_BOND - DROPS_PER_FANG / 100);
        // Flagging carol as a sybil costs alice her bond and Soul, and blocks carol.
        let pool0 = t.c.state.account(&t.c.state.pool()).balance;
        assert!(t.tx(&bob, Action::FlagSybil { account: carol.address(), reason: "x".into() }).is_err());
        t.admin(Action::FlagSybil { account: carol.address(), reason: "same person as dave".into() }).unwrap();
        assert_eq!(t.c.state.account(&t.c.state.pool()).balance, pool0 + VOUCH_BOND, "bond slashed into the pool");
        assert_eq!(t.soul(&alice), 0, "voucher loses 50 Soul (saturating)");
        assert!(t.c.state.poc.vouches.get(&carol.address()).is_none());
        assert!(t.admin(Action::GrantSoul { to: carol.address(), amount: SOUL, reason: "x".into() }).unwrap_err().to_string().contains("sybil"));
        assert!(t.admin(Action::FlagSybil { account: carol.address(), reason: "again".into() }).is_err());
        let adm = t.admin.address();
        assert!(t.admin(Action::FlagSybil { account: adm, reason: "x".into() }).is_err(), "admins can't be flagged");
        conserved(&t.c.state);
        t.reopen_same_root();
    }

    #[test]
    fn claims_attestations_and_familiars() {
        let mut t = setup("claims");
        let (alice, bob, agent, stranger) = (k(1), k(2), k(9), k(10));
        t.admin(Action::SetRepo { repo: "JackDishman/blockula".into(), weight: 10 }).unwrap();
        assert!(t.claim(&alice, 1).unwrap_err().to_string().contains("link a GitHub"));
        t.link(&alice, "alice");
        assert!(t.claim(&alice, 1).unwrap_err().to_string().contains("Soul or a vouch"));
        t.grant(&alice, 1);
        assert!(t.tx(&alice, Action::SubmitClaim { repo: "other/repo".into(), pr: 1, familiar_key: None }).unwrap_err().to_string().contains("isn't in the contribution pool"));
        let b0 = t.bal(&alice);
        let c1 = t.claim(&alice, 1).unwrap();
        assert!(b0 - t.bal(&alice) >= CLAIM_BOND);
        let b1 = t.bal(&alice);
        let c2 = t.claim(&alice, 2).unwrap();
        assert!(b1 - t.bal(&alice) >= 2 * CLAIM_BOND, "bond grows with open claims");
        assert!(t.claim(&alice, 1).unwrap_err().to_string().contains("already claimed"));
        assert_eq!(t.c.state.poc.claims[&c1].handle, "alice");
        // Only active validators attest; one validator is a quorum here.
        assert!(t.tx(&bob, Action::AttestClaim { claim_id: c1, ok: true, merge_sha: "ab".repeat(20), note: String::new() }).unwrap_err().to_string().contains("only active validators"));
        t.attest(c1, true).unwrap();
        assert_eq!(t.c.state.poc.claims[&c1].status, ClaimStatus::Verified);
        assert!(t.attest(c1, true).is_err(), "already settled");
        let pool0 = t.c.state.account(&t.c.state.pool()).balance;
        t.attest(c2, false).unwrap();
        assert_eq!(t.c.state.poc.claims[&c2].status, ClaimStatus::Rejected);
        assert!(t.c.state.poc.claims[&c2].reason.as_deref().unwrap().contains("closed without merging"));
        assert_eq!(t.c.state.account(&t.c.state.pool()).balance, pool0 + 2 * CLAIM_BOND, "rejected bond goes to the pool");
        let c3 = t.claim(&alice, 2).unwrap();
        assert!(c3 > c2, "a rejected PR can be claimed again");
        // Admin rejection of a verified claim (e.g. reverted) blocks re-claims.
        t.attest(c3, true).unwrap();
        t.admin(Action::RejectClaim { claim_id: c3, reason: "reverted".into() }).unwrap();
        assert!(t.claim(&alice, 2).unwrap_err().to_string().contains("already claimed"));
        // Open-claim cap: 3 at 1 Soul.
        t.claim(&alice, 3).unwrap();
        t.claim(&alice, 4).unwrap();
        assert!(t.claim(&alice, 5).unwrap_err().to_string().contains("open claims"));
        // A registered Familiar (agent session key) can claim for its sponsor; other session keys can't.
        t.link(&bob, "bob-dev");
        t.grant(&bob, 1);
        let h = t.h();
        for key in [&agent, &stranger] {
            let add = crate::types::add_key(key, &t.c.genesis.chain_id, &t.c.genesis_hash, &bob.address(), "agent", KeyRole::Session { limit: 5 * DROPS_PER_FANG, period_blocks: 1000, expires_at: h + 1000 });
            t.tx(&bob, add).unwrap();
        }
        t.tx(&bob, Action::RegisterFamiliar { key_id: key_id(&agent.public_key()), label: "claude".into() }).unwrap();
        let bob_addr = bob.address();
        assert!(t.tx_as(&stranger, &bob_addr, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr: 7, familiar_key: None }).unwrap_err().to_string().contains("only send transfers"));
        t.tx_as(&agent, &bob_addr, Action::SubmitClaim { repo: "jackdishman/blockula".into(), pr: 7, familiar_key: None }).unwrap();
        let fc = t.c.state.poc.next_claim_id - 1;
        assert_eq!(t.c.state.poc.claims[&fc].familiar_key.as_deref(), Some(key_id(&agent.public_key()).as_str()));
        assert_eq!(t.c.state.poc.claims[&fc].claimant, bob_addr, "the sponsor is the claimant");
        let spent = t.c.state.keys_of(&bob_addr)[&key_id(&agent.public_key())].spent;
        assert!(spent >= CLAIM_BOND, "the bond counts against the session limit");
        assert!(t.tx_as(&agent, &bob_addr, Action::JoinJury { bond: JUROR_BOND }).unwrap_err().to_string().contains("only send transfers"), "Familiars can't sit on juries");
        // Sybil flag rejects open claims and slashes their bonds.
        t.admin(Action::FlagSybil { account: bob_addr.clone(), reason: "test".into() }).unwrap();
        assert_eq!(t.c.state.poc.claims[&fc].status, ClaimStatus::Rejected);
        conserved(&t.c.state);
        t.reopen_same_root();
    }

    #[test]
    fn seance_judges_pays_and_vests() {
        let mut t = setup("seance");
        // jurors[2] won't show up, jurors[3] votes the reverse, jurors[4] calls everything a tie.
        let jurors = [k(1), k(2), k(3), k(4), k(8)];
        let (solid, mid) = (k(5), k(6));
        let (funder, voucher) = (k(7), k(9));
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for j in &jurors {
            t.grant(j, 10);
            assert!(t.tx(j, Action::JoinJury { bond: JUROR_BOND - 1 }).unwrap_err().to_string().contains("at least"));
            t.tx(j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        assert!(t.tx(&solid, Action::JoinJury { bond: JUROR_BOND }).unwrap_err().to_string().contains("Soul"));
        t.tx(&funder, Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
        t.grant(&solid, 1);
        // mid has no Soul, only a (young) vouch: its voucher gets a cut of mid's reward.
        t.grant(&voucher, 11);
        t.tx(&voucher, Action::Vouch { account: mid.address() }).unwrap();
        t.link(&solid, "solid");
        t.link(&mid, "mid");
        let big = t.claim(&solid, 10).unwrap();
        let tiny = t.claim(&solid, 11).unwrap();
        let ok = t.claim(&mid, 12).unwrap();
        // Verified claims, but no Séance until they're attested.
        assert!(t.c.state.poc.open_seance.is_none());
        t.attest_all(&[big, tiny, ok]);
        assert!(t.c.state.poc.claims.values().all(|c| c.status != ClaimStatus::Pending));
        conserved(&t.c.state);
        let sid = t.wait_for_seance();
        assert_eq!(t.h(), t.c.state.next_seance_height(t.h()), "Séances open on the cadence");
        let s = t.c.state.poc.seances[&sid].clone();
        assert_eq!(s.claims, vec![big, tiny, ok]);
        assert_eq!(s.jurors.len(), 5);
        assert_eq!(s.budget, 20 * DROPS_PER_FANG * 2 / FULL_BUDGET_PEOPLE, "a quarter of the pool, scaled for 2 people");
        assert!(s.jurors.values().all(|seat| seat.pairs.len() == 2), "every pair of two people's claims (solid's two are never compared)");
        assert!(t.tx(&jurors[0], Action::LeaveJury {}).unwrap_err().to_string().contains("after it closes"));
        conserved(&t.c.state);
        // Honest jurors: big > ok > tiny, and they flag tiny as spam (so does the all-ties juror).
        let rank = |c: u64| if c == big { 3 } else if c == ok { 2 } else { 1 };
        let ballot = |seat: &JurySeat, i: usize| -> String {
            seat.pairs.iter().map(|[a, b]| if i == 4 { '=' } else if (rank(*a) > rank(*b)) != (i == 3) { 'a' } else { 'b' }).collect()
        };
        let mut sealed = vec![];
        for (i, jk) in jurors.iter().enumerate() {
            if i == 2 {
                continue;
            }
            let seat = &s.jurors[&jk.address()];
            let votes = ballot(seat, i);
            let spam = if i < 2 || i == 4 { vec![tiny] } else { vec![] };
            let salt = format!("salt-{i}");
            let commitment = seance_commitment(sid, &jk.address(), &votes, &spam, &salt);
            assert!(t.tx(jk, Action::RevealVotes { seance_id: sid, votes: votes.clone(), spam: spam.clone(), salt: salt.clone() }).unwrap_err().to_string().contains("open after"));
            let pairs_digest = seat_pairs_digest(&t.c.state, sid, &jk.address(), &seat.pairs);
            if i == 0 {
                // gap-7-6: a node that showed the juror the pairs flipped (so 'a' would credit the
                // other claim), or a claim with another PR, gets the commitment refused.
                let flipped: Vec<[u64; 2]> = seat.pairs.iter().map(|[a, b]| [*b, *a]).collect();
                let shown = seat_pairs_digest(&t.c.state, sid, &jk.address(), &flipped);
                assert!(t.tx(jk, Action::CommitVotes { seance_id: sid, commitment: commitment.clone(), pairs_digest: shown }).unwrap_err().to_string().contains("aren't your seat's"));
                let [x, y] = seat.pairs[0];
                let other_pr = |id: u64| { let c = &t.c.state.poc.claims[&id]; (id, c.repo.clone(), c.pr + 1) };
                let (cx, cy) = (other_pr(x), (y, t.c.state.poc.claims[&y].repo.clone(), t.c.state.poc.claims[&y].pr));
                let mut refs: Vec<[(u64, &str, u64); 2]> = vec![[(cx.0, cx.1.as_str(), cx.2), (cy.0, cy.1.as_str(), cy.2)]];
                let rest: Vec<[(u64, String, u64); 2]> = seat.pairs[1..].iter().map(|[a, b]| [a, b].map(|id| (*id, t.c.state.poc.claims[id].repo.clone(), t.c.state.poc.claims[id].pr))).collect();
                refs.extend(rest.iter().map(|[a, b]| [(a.0, a.1.as_str(), a.2), (b.0, b.1.as_str(), b.2)]));
                let swapped_pr = crate::types::seance_pairs_digest(sid, &jk.address(), &refs);
                assert!(t.tx(jk, Action::CommitVotes { seance_id: sid, commitment: commitment.clone(), pairs_digest: swapped_pr }).unwrap_err().to_string().contains("aren't your seat's"));
            }
            t.tx(jk, Action::CommitVotes { seance_id: sid, commitment, pairs_digest }).unwrap();
            sealed.push((i, votes, spam, salt));
        }
        assert!(t.tx(&solid, Action::CommitVotes { seance_id: sid, commitment: "00".repeat(32), pairs_digest: "00".repeat(32) }).unwrap_err().to_string().contains("not on the jury"));
        while t.h() <= s.commit_until {
            t.blocks(1);
        }
        let late = seat_pairs_digest(&t.c.state, sid, &jurors[2].address(), &s.jurors[&jurors[2].address()].pairs);
        assert!(t.tx(&jurors[2], Action::CommitVotes { seance_id: sid, commitment: "00".repeat(32), pairs_digest: late }).unwrap_err().to_string().contains("closed at block"));
        for (i, votes, spam, salt) in &sealed {
            let jk = &jurors[*i];
            assert!(t.tx(jk, Action::RevealVotes { seance_id: sid, votes: votes.clone(), spam: spam.clone(), salt: "wrong".into() }).unwrap_err().to_string().contains("doesn't match"));
            t.tx(jk, Action::RevealVotes { seance_id: sid, votes: votes.clone(), spam: spam.clone(), salt: salt.clone() }).unwrap();
        }
        let souls_before: Vec<u64> = jurors.iter().map(|j| t.soul(j)).collect();
        let solid_soul = t.soul(&solid);
        while t.c.state.poc.open_seance == Some(sid) {
            t.blocks(1);
        }
        let s = t.c.state.poc.seances[&sid].clone();
        let r = s.results.clone().unwrap();
        assert!(s.closed);
        assert_eq!((r.accepted, r.spam, r.no_shows, r.carried_over), (2, 1, 1, 0));
        let claims = &t.c.state.poc.claims;
        assert_eq!(claims[&tiny].status, ClaimStatus::Spam);
        assert!(claims[&big].score_ppm > claims[&ok].score_ppm);
        assert!(claims[&big].payout > claims[&ok].payout && claims[&ok].payout > 0);
        assert_eq!(claims[&big].bond, 0, "bond refunded");
        assert_eq!(r.paid + r.returned, s.budget);
        // Jurors: the honest ones gain Soul and get paid; the contrarian loses Soul; the no-show
        // is slashed and benched.
        let seat = |i: usize| s.jurors[&jurors[i].address()].clone();
        assert!(seat(0).agreement_ppm.unwrap() >= GOOD_AGREEMENT_PPM && seat(0).reward > 0);
        assert!(seat(3).agreement_ppm.unwrap() < BAD_AGREEMENT_PPM && seat(3).reward == 0);
        assert_eq!((seat(4).agreement_ppm, seat(4).reward, seat(4).soul_delta), (Some(500_000), 0, 0), "all ties earn nothing");
        assert!(t.soul(&jurors[0]) > souls_before[0]);
        assert!(t.soul(&jurors[3]) < souls_before[3]);
        assert!(t.soul(&jurors[2]) < souls_before[2]);
        assert_eq!(t.c.state.poc.jurors[&jurors[2].address()].bond, JUROR_BOND - bps(JUROR_BOND, NO_SHOW_SLASH_BPS));
        assert!(!t.c.state.poc.jurors[&jurors[2].address()].active);
        // The spam claim cost Soul (saturating) and the accepted one earned some; net for solid:
        assert!(t.soul(&solid) < solid_soul + CLAIM_SOUL);
        conserved(&t.c.state);
        // Rewards vest linearly from the close.
        assert_eq!(t.c.state.grant_totals(&solid.address(), t.h()).1, 0);
        assert!(t.tx(&jurors[2], Action::WithdrawVested {}).unwrap_err().to_string().contains("nothing has vested"));
        t.blocks(8);
        let (total, avail, unvested) = t.c.state.grant_totals(&solid.address(), t.h() + 1);
        assert!(avail > 0 && unvested > 0 && total == claims_payout(&t, big));
        let b0 = t.bal(&solid);
        t.tx(&solid, Action::WithdrawVested {}).unwrap();
        assert!(t.bal(&solid) > b0);
        conserved(&t.c.state);
        // Clawback returns what hasn't vested to the pool.
        let pool0 = t.c.state.account(&t.c.state.pool()).balance;
        t.admin(Action::Clawback { account: solid.address(), reason: "PR reverted".into() }).unwrap();
        assert!(t.c.state.account(&t.c.state.pool()).balance > pool0);
        assert_eq!(t.c.state.grant_totals(&solid.address(), t.h() + 100).2, 0);
        conserved(&t.c.state);
        // The voucher got a cut of mid's reward; rejecting mid's claim (say it was reverted) claws
        // back both, and the Soul it earned.
        assert!(t.c.state.grant_totals(&voucher.address(), t.h()).0 > 0);
        let mid_soul = t.soul(&mid);
        t.admin(Action::RejectClaim { claim_id: ok, reason: "reverted".into() }).unwrap();
        assert_eq!(t.c.state.grant_totals(&mid.address(), t.h() + 100).2, 0);
        assert_eq!(t.c.state.grant_totals(&voucher.address(), t.h() + 100).2, 0);
        assert!(t.soul(&mid) < mid_soul);
        conserved(&t.c.state);
        // After it closes, jurors can leave and get their bond back.
        let b = t.bal(&jurors[1]);
        t.tx(&jurors[1], Action::LeaveJury {}).unwrap();
        assert!(t.bal(&jurors[1]) > b + JUROR_BOND - DROPS_PER_FANG / 100);
        conserved(&t.c.state);
        t.reopen_same_root();
    }

    /// Fixes from review: newcomer-only vouches the vouchee can decline, a GitHub handle credits
    /// one account, Séances need enough reveals, removed repos and admin-rejected pending claims
    /// don't strand bonds or PRs.
    #[test]
    fn vouches_handles_quorum_and_stuck_claims() {
        let mut t = setup("fixes");
        let (j1, j2, j3, vet, newbie, hopper, other) = (k(1), k(2), k(3), k(5), k(6), k(7), k(8));
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for j in [&j1, &j2, &j3] {
            t.grant(j, 10);
        }
        t.grant(&vet, 25);
        // Vouches are for newcomers only, and the vouchee can decline. The voucher still answers
        // for it until the lock is over (audit xcut-economic-opus-2): the bond stays held, the
        // slot taken (25 Soul allows 2).
        assert!(t.tx(&vet, Action::Vouch { account: j1.address() }).unwrap_err().to_string().contains("isn't a newcomer"));
        t.tx(&vet, Action::Vouch { account: newbie.address() }).unwrap();
        let v0 = t.bal(&vet);
        t.tx(&newbie, Action::Unvouch { account: newbie.address() }).unwrap();
        assert_eq!(t.bal(&vet), v0);
        assert_eq!(t.c.state.vouches_held_by(&vet.address()), 1);
        t.tx(&vet, Action::Vouch { account: newbie.address() }).unwrap();
        // A handle's work is credited to the first account it's verified for.
        t.link(&newbie, "newbie");
        let c1 = t.claim(&newbie, 1).unwrap();
        t.attest(c1, true).unwrap();
        assert_eq!(t.c.state.poc.handles["newbie"], newbie.address());
        t.tx(&newbie, Action::UnlinkGithub { handle: "newbie".into() }).unwrap();
        t.grant(&hopper, 1);
        // Nobody else can even link it (audit xcut-economic-4: validators have seen its gist).
        assert!(t.tx(&hopper, Action::LinkGithub { handle: "Newbie".into(), gist_id: "abc123".into() }).unwrap_err().to_string().contains("credited to"));
        t.link(&newbie, "newbie");
        // Too few reveals: nothing is decided, claims carry over, the budget goes back.
        t.grant(&other, 1);
        t.link(&other, "other");
        for j in [&j1, &j2, &j3] {
            t.tx(j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&vet, Action::FundPool { amount: 50 * DROPS_PER_FANG }).unwrap();
        let c2 = t.claim(&other, 3).unwrap();
        t.attest(c2, true).unwrap();
        let sid = t.wait_for_seance();
        let s = t.c.state.poc.seances[&sid].clone();
        let mut sealed = vec![];
        for jk in [&j1, &j2] {
            let votes: String = s.jurors[&jk.address()].pairs.iter().map(|_| 'a').collect();
            let pairs_digest = seat_pairs_digest(&t.c.state, sid, &jk.address(), &s.jurors[&jk.address()].pairs);
            t.tx(jk, Action::CommitVotes { seance_id: sid, commitment: seance_commitment(sid, &jk.address(), &votes, &[], "x"), pairs_digest }).unwrap();
            sealed.push((jk.clone_keys(), votes));
        }
        while t.h() <= s.commit_until {
            t.blocks(1);
        }
        for (jk, votes) in &sealed {
            t.tx(jk, Action::RevealVotes { seance_id: sid, votes: votes.clone(), spam: vec![], salt: "x".into() }).unwrap();
        }
        let soul_j1 = t.soul(&j1);
        while t.c.state.poc.open_seance == Some(sid) {
            t.blocks(1);
        }
        let s = t.c.state.poc.seances[&sid].clone();
        let r = s.results.clone().unwrap();
        assert_eq!((r.accepted, r.carried_over, r.no_shows, r.paid), (0, 2, 1, 0));
        assert_eq!(r.returned, s.budget);
        assert_eq!((s.jurors[&j1.address()].reward, s.jurors[&j1.address()].soul_delta), (0, 0));
        assert!(t.soul(&j1) <= soul_j1, "no Soul for an inquorate Séance");
        assert!(t.soul(&j3) < t.soul(&j2), "the no-show still pays");
        assert!(t.c.state.poc.claims.values().all(|c| c.score_ppm.is_none() && c.payout == 0));
        conserved(&t.c.state);
        // Removing a repo refunds the claims still waiting on it.
        t.admin(Action::SetRepo { repo: "other/repo".into(), weight: 5 }).unwrap();
        let c3 = t.tx(&other, Action::SubmitClaim { repo: "other/repo".into(), pr: 9, familiar_key: None }).map(|_| t.c.state.poc.next_claim_id - 1).unwrap();
        let b = t.bal(&other);
        t.admin(Action::SetRepo { repo: "other/repo".into(), weight: 0 }).unwrap();
        assert_eq!(t.c.state.poc.claims[&c3].status, ClaimStatus::Rejected);
        assert!(t.bal(&other) > b, "bond refunded");
        assert!(!t.c.state.poc.claim_by_pr.contains_key("other/repo#9"));
        // An admin rejecting an unverified claim doesn't lock the PR away from its author.
        let c4 = t.claim(&other, 4).unwrap();
        t.admin(Action::RejectClaim { claim_id: c4, reason: "squatted".into() }).unwrap();
        t.claim(&other, 4).unwrap();
        conserved(&t.c.state);
        t.reopen_same_root();
    }

    /// CRY-3: Séances open on a fixed cadence, seeded from the beacon, over jurors and claims
    /// that were eligible before its value could be known. Whatever the opening block's proposer
    /// does (reorder, censor or add txs, pick its block hash) the draw is the same, and a juror
    /// leaving frees only its own seat.
    #[test]
    fn seance_draw_is_fixed_before_the_opening_block() {
        let mut t = setup("draw");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        let jurors: Vec<Keys> = (1..=9).map(k).collect();
        for j in &jurors {
            t.grant(j, 10);
            t.tx(j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        let late = k(12);
        t.grant(&late, 10);
        let mut claims = vec![];
        for (who, handle) in [(k(10), "ten"), (k(11), "eleven")] {
            t.grant(&who, 1);
            t.link(&who, handle);
            claims.push(t.claim(&who, claims.len() as u64 + 1).unwrap());
        }
        t.attest_all(&claims);
        // Openings are the last blocks of every 3rd epoch (30-block Séances, 10-block epochs); the
        // first that may draw on these claims has its cutoff past their verification.
        let st = &t.c.state;
        assert_eq!(st.seance_every_epochs(), 3);
        let mut at = st.next_seance_height(t.h() + 1);
        while st.seance_cutoff(st.params.epoch_of(at)) <= t.h() {
            at = st.next_seance_height(at + 1);
        }
        while t.h() < at - 1 {
            t.blocks(1);
        }
        assert!(t.c.state.poc.open_seance.is_none() && t.c.state.poc.next_seance_at == at);
        // Alternative opening blocks, each on a copy of the state (dropped before the restart check).
        let pre = t.c.state.clone();
        let epoch = pre.params.epoch_of(at);
        let open = |txs: &[Tx], proposer: &str, hash: &str| -> Seance {
            let mut st = pre.clone();
            for tx in txs {
                assert_eq!(st.apply(tx, "poc", proposer, at).unwrap(), Outcome::Applied);
            }
            let tx_bytes = txs.iter().map(Tx::size).sum();
            st.end_block(&BlockInfo { height: at, version: crate::upgrade::version_at(&st, at), hash, proposer, view: at, parent_view: at - 1, skipped: &[], tx_bytes });
            st.check_invariants().unwrap();
            let id = st.poc.open_seance.expect("a Séance opens on the cadence");
            assert_eq!(st.poc.seances[&id].seed, hex::encode(crate::beacon::seance_seed(&st, epoch, id).unwrap()), "seeded from the beacon");
            st.poc.seances[&id].clone()
        };
        let json = |s: &Seance| serde_json::to_value(s).unwrap();
        fn draw(s: &Seance) -> serde_json::Value {
            serde_json::to_value((&s.seed, &s.claims, &s.jurors)).unwrap()
        }
        let join = t.signed_as(&late, &late.address(), Action::JoinJury { bond: JUROR_BOND });
        let fund = t.signed_as(&jurors[0], &jurors[0].address(), Action::FundPool { amount: 5 * DROPS_PER_FANG });
        let pay = t.signed_as(&jurors[1], &jurors[1].address(), Action::Transfer { to: jurors[2].address(), amount: DROPS_PER_FANG });
        let me = t.c.me().unwrap();
        let a = open(&[join.clone(), fund.clone(), pay.clone()], &me, "a");
        let b = open(&[pay, fund, join], &jurors[3].address(), "b");
        let c = open(&[], &me, "c");
        assert_eq!(a.opened_at, at);
        assert_eq!((a.claims.clone(), a.jurors.len()), (claims.clone(), JURY_SIZE));
        assert!(!a.jurors.contains_key(&late.address()), "joined after the cutoff");
        assert_eq!(json(&a), json(&b), "reordering the opening block, another proposer, another hash: the same Séance");
        assert_eq!(draw(&a), draw(&c), "censoring the opening block's txs changes nothing either");
        // Sortition: an unseated juror leaving changes nothing; a seated one frees only its seat.
        let unseated = jurors.iter().find(|j| !a.jurors.contains_key(&j.address())).unwrap();
        assert_eq!(draw(&open(&[t.signed_as(unseated, &unseated.address(), Action::LeaveJury {})], &me, "d")), draw(&a));
        let seated = jurors.iter().find(|j| a.jurors.contains_key(&j.address())).unwrap();
        let e = open(&[t.signed_as(seated, &seated.address(), Action::LeaveJury {})], &me, "e");
        assert!(!e.jurors.contains_key(&seated.address()) && e.jurors.len() == JURY_SIZE);
        for (juror, seat) in a.jurors.iter().filter(|(j, _)| **j != seated.address()) {
            assert_eq!(e.jurors[juror].pairs, seat.pairs, "the others keep their seats and pairs");
        }
        drop(pre);
        // The real chain opens the same Séance at that height.
        t.blocks(1);
        let id = t.c.state.poc.open_seance.unwrap();
        assert_eq!(draw(&t.c.state.poc.seances[&id]), draw(&c));
        t.reopen_same_root();
    }

    /// Audit F53 (the audit's PoC): a juror's vouchee sees from the public seed that its voucher
    /// would sit, and drops the vouch so the voucher can judge its claim. The vouch goes, but the
    /// conflict counts as of the Séance's cutoff (`DroppedVouch`), so the same Séance opens without
    /// it, and the voucher's place in the pool doesn't move (audit review-fix-poc-2).
    #[test]
    fn dropping_a_vouch_once_the_draw_is_known_seats_nobody_new() {
        let mut t = setup("vouchdraw");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        let jurors: Vec<Keys> = (1..=4).map(k).collect();
        for j in &jurors {
            t.grant(j, 20);
            t.tx(j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        let (insider, newcomer, other) = (&jurors[0], k(10), k(11));
        t.tx(insider, Action::Vouch { account: newcomer.address() }).unwrap();
        t.grant(&other, 1);
        t.link(&newcomer, "newcomer");
        t.link(&other, "other");
        let claims = [t.claim(&newcomer, 1).unwrap(), t.claim(&other, 2).unwrap()];
        t.attest_all(&claims);
        t.tx(&k(12), Action::FundPool { amount: 50 * DROPS_PER_FANG }).unwrap();
        // The first opening whose cutoff is past all that, run to where its seed is public.
        let at = {
            let st = &t.c.state;
            let mut at = st.next_seance_height(t.h() + 1);
            while st.seance_cutoff(st.params.epoch_of(at)) <= t.h() {
                at = st.next_seance_height(at + 1);
            }
            at
        };
        let epoch = t.c.state.params.epoch_of(at);
        while t.h() < epoch * t.c.state.params.epoch_blocks {
            t.blocks(1);
        }
        assert!(crate::beacon::value(&t.c.state, epoch).is_some() && t.h() < at, "the seed is public before the opening");
        let draw = |mut st: State, from: u64| -> Seance {
            for h in from..=at {
                st.end_block(&BlockInfo { height: h, version: crate::upgrade::version_at(&st, h), hash: "x", proposer: "p", view: h, parent_view: h - 1, skipped: &[], tx_bytes: 0 });
            }
            let id = st.poc.open_seance.expect("a Séance opens");
            st.poc.seances[&id].clone()
        };
        // As the rules intend: the insider can't judge the newcomer's claim, so with two claims it
        // has no pair to judge and doesn't sit.
        let a = draw(t.c.state.clone(), t.h() + 1);
        assert_eq!((a.claims.len(), a.jurors.len()), (2, 3));
        assert!(!a.jurors.contains_key(&insider.address()));
        // Knowing that, the newcomer declines the vouch (it always may). The two stay tied for
        // this Séance: the same jury sits, and the insider still doesn't judge the claim it
        // vouched for. Its seniority in the pool is untouched.
        let joined = t.c.state.poc.jurors[&insider.address()].joined_at;
        t.tx(&newcomer, Action::Unvouch { account: newcomer.address() }).unwrap();
        assert!(!t.c.state.poc.vouches.contains_key(&newcomer.address()));
        assert_eq!(t.c.state.poc.jurors[&insider.address()].joined_at, joined);
        let jury = |s: &Seance| serde_json::to_value((&s.claims, &s.jurors)).unwrap();
        assert_eq!(jury(&draw(t.c.state.clone(), t.h() + 1)), jury(&a));
        t.blocks(at - t.h());
        let id = t.c.state.poc.open_seance.expect("the real chain opens it");
        assert_eq!(jury(&t.c.state.poc.seances[&id]), jury(&a));
        conserved(&t.c.state);
        t.reopen_same_root();
    }

    /// Audit review-fix-poc-2: once the seed is public, the vouchee dropping its vouch can't take
    /// its voucher off the upcoming Séance (or, with the pool at `MIN_JURY`, stop the Séance and
    /// re-roll the seed): the conflict counts as of the cutoff, so the same jury sits with the
    /// same pairs, and the voucher still never judges its vouchee's claim.
    #[test]
    fn an_unvouch_after_the_seed_changes_no_seat() {
        let mut t = setup("vouchseat");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        let jurors: Vec<Keys> = (1..=3).map(k).collect();
        for j in &jurors {
            t.grant(j, 20);
            t.tx(j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        let (insider, newcomer) = (&jurors[0], k(10));
        t.tx(insider, Action::Vouch { account: newcomer.address() }).unwrap();
        t.link(&newcomer, "newcomer");
        let mut claims = vec![t.claim(&newcomer, 1).unwrap()];
        for (i, who) in [(11u8, "o1"), (12, "o2")] {
            t.grant(&k(i), 1);
            t.link(&k(i), who);
            claims.push(t.claim(&k(i), i as u64).unwrap());
        }
        t.attest_all(&claims);
        t.tx(&k(9), Action::FundPool { amount: 50 * DROPS_PER_FANG }).unwrap();
        let at = {
            let st = &t.c.state;
            let mut at = st.next_seance_height(t.h() + 1);
            while st.seance_cutoff(st.params.epoch_of(at)) <= t.h() {
                at = st.next_seance_height(at + 1);
            }
            at
        };
        let epoch = t.c.state.params.epoch_of(at);
        while t.h() < epoch * t.c.state.params.epoch_blocks {
            t.blocks(1);
        }
        assert!(crate::beacon::value(&t.c.state, epoch).is_some() && t.h() < at, "the seed is public before the opening");
        let draw = |mut st: State, from: u64| -> Option<Seance> {
            for h in from..=at {
                st.end_block(&BlockInfo { height: h, version: crate::upgrade::version_at(&st, h), hash: "x", proposer: "p", view: h, parent_view: h - 1, skipped: &[], tx_bytes: 0 });
            }
            st.poc.open_seance.map(|id| st.poc.seances[&id].clone())
        };
        let a = draw(t.c.state.clone(), t.h() + 1).expect("a Séance opens");
        assert_eq!(a.jurors.len(), 3);
        let seat = &a.jurors[&insider.address()];
        assert!(seat.pairs.iter().all(|p| !p.contains(&claims[0])), "the insider never judges its vouchee: {:?}", seat.pairs);
        t.tx(&newcomer, Action::Unvouch { account: newcomer.address() }).unwrap();
        assert!(!t.c.state.poc.vouches.contains_key(&newcomer.address()));
        let jury = |s: &Seance| serde_json::to_value((&s.claims, &s.jurors)).unwrap();
        assert_eq!(draw(t.c.state.clone(), t.h() + 1).map(|s| jury(&s)), Some(jury(&a)), "the same jury, the same pairs");
        t.blocks(at - t.h());
        let id = t.c.state.poc.open_seance.expect("the real chain opens it");
        assert_eq!(jury(&t.c.state.poc.seances[&id]), jury(&a));
        conserved(&t.c.state);
        t.reopen_same_root();
    }

    /// Audit xcut-economic-opus-2: a sybil dropping its vouch (a newcomer may decline one any
    /// time) doesn't get its voucher off the hook. Until the vouch's lock would have ended, the
    /// bond stays held and the slot taken; a sybil flag then costs the voucher the bond and
    /// `SYBIL_VOUCHER_SOUL`, and claws back its cut of the sybil's rewards. After the lock, with
    /// no flag, the bond comes back at an epoch boundary.
    #[test]
    fn a_dropped_vouch_still_answers_for_its_sybil() {
        let mut t = setup("sybilvouch");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        let (op, s1, s2, s3) = (k(5), k(6), k(7), k(8));
        t.grant(&op, 25);
        t.tx(&op, Action::Vouch { account: s1.address() }).unwrap();
        t.tx(&op, Action::Vouch { account: s2.address() }).unwrap();
        t.link(&s1, "s1");
        let c = t.claim(&s1, 1).unwrap();
        t.attest(c, true).unwrap();
        let before = t.bal(&op);
        t.tx(&s1, Action::Unvouch { account: s1.address() }).unwrap();
        assert_eq!(t.bal(&op), before, "the bond stays held");
        assert!(t.tx(&op, Action::Vouch { account: s3.address() }).unwrap_err().to_string().contains("allows 2"), "and its slot taken");
        conserved(&t.c.state);
        let (pool0, soul0) = (t.c.state.account(&t.c.state.pool()).balance, t.soul(&op));
        t.admin(Action::FlagSybil { account: s1.address(), reason: "a sock puppet".into() }).unwrap();
        assert_eq!(t.c.state.account(&t.c.state.pool()).balance, pool0 + VOUCH_BOND + CLAIM_BOND, "the voucher's bond and the sybil's claim bond go to the pool");
        assert_eq!(t.soul(&op), soul0.saturating_sub(SYBIL_VOUCHER_SOUL));
        assert_eq!(t.c.state.poc.claims[&c].status, ClaimStatus::Rejected);
        conserved(&t.c.state);
        // s2 declines too; nobody flags it. The bond comes back once the lock would have ended.
        t.tx(&s2, Action::Unvouch { account: s2.address() }).unwrap();
        let unlock = t.c.state.poc.dropped_vouches[&vouch_key(&s2.address(), &op.address())].at + t.c.state.params.poc.vouch_lock_blocks;
        let held = t.bal(&op);
        while t.h() < unlock {
            t.blocks(1);
        }
        assert_eq!(t.bal(&op), held, "held until the lock is over");
        t.blocks(t.c.state.params.epoch_blocks);
        assert_eq!(t.bal(&op), held + VOUCH_BOND, "then handed back at an epoch boundary");
        assert!(t.c.state.poc.dropped_vouches.is_empty(), "and forgotten once no Séance can need it");
        conserved(&t.c.state);
        t.reopen_same_root();

        // The voucher's cut of a sybil's rewards is clawed back with the sybil's own (as
        // `exec_reject_claim` does for a reverted claim).
        let mut t = setup("sybilcut");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for j in [k(1), k(2), k(3)] {
            t.grant(&j, 10);
            t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&k(9), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
        let (voucher, sybil, other) = (k(5), k(6), k(7));
        t.grant(&voucher, 20);
        t.grant(&other, 1);
        t.tx(&voucher, Action::Vouch { account: sybil.address() }).unwrap();
        t.link(&sybil, "sybil");
        t.link(&other, "other");
        let ids = [t.claim(&sybil, 1).unwrap(), t.claim(&other, 2).unwrap()];
        t.attest_all(&ids);
        let sid = t.wait_for_seance();
        let mut st = t.c.state.clone();
        judge(&mut st, sid, |c| if c == ids[0] { 2 } else { 1 });
        st.close_seance(sid, t.h());
        let h = t.h() + 1;
        assert!(st.grant_totals(&voucher.address(), h).2 > 0, "the voucher got a cut");
        let mut j = Journal::default();
        st.exec_flag_sybil(&mut j, &sybil.address(), "test", "a sock puppet", h).unwrap();
        assert_eq!((st.grant_totals(&sybil.address(), h).2, st.grant_totals(&voucher.address(), h).2), (0, 0), "both clawed back");
        st.check_invariants().unwrap();
        drop(st);
        std::fs::remove_dir_all(&t.home).ok();
    }

    /// Audit F53, the rule: a vouch can always be withdrawn (the newcomer declines any time, the
    /// voucher after the lock), so no juror can pin its vouch, and its cut, on a newcomer with a
    /// claim open. Withdrawing moves nobody's place in the jury pool (audit review-fix-poc-2): the
    /// two stay tied for any Séance whose cutoff came before it, and the voucher's bond and slot
    /// stay held until the lock is over (audit xcut-economic-opus-2).
    #[test]
    fn a_vouch_can_always_go_and_moves_no_seat() {
        let mut t = setup("vouchgo");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        let (juror, newbie, other) = (k(1), k(6), k(7));
        t.grant(&juror, 35);
        t.tx(&juror, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        let joined = |t: &T, who: &Keys| t.c.state.poc.jurors.get(&who.address()).unwrap().joined_at;
        let first = joined(&t, &juror);
        t.link(&juror, "juror");
        let mine = t.claim(&juror, 2).unwrap();
        t.attest(mine, true).unwrap();
        let b0 = t.bal(&juror);
        // A newcomer declines: the bond is held, the slot taken, the juror's place unchanged.
        t.tx(&juror, Action::Vouch { account: other.address() }).unwrap();
        t.tx(&other, Action::Unvouch { account: other.address() }).unwrap();
        assert_eq!(joined(&t, &juror), first);
        let d = t.c.state.poc.dropped_vouches[&vouch_key(&other.address(), &juror.address())].clone();
        assert_eq!((d.bond, d.dropped_at), (VOUCH_BOND, t.h()));
        // The newcomer, with a claim open (made under the vouch), declines its juror's vouch: the
        // same. Its claim stays tied to the juror for Séances whose cutoff came before.
        t.tx(&juror, Action::Vouch { account: newbie.address() }).unwrap();
        t.link(&newbie, "newbie");
        let c = t.claim(&newbie, 1).unwrap();
        t.attest(c, true).unwrap();
        t.tx(&newbie, Action::Unvouch { account: newbie.address() }).unwrap();
        let dropped = t.h();
        assert_eq!(joined(&t, &juror), first);
        assert!(t.c.state.conflicted(&juror.address(), &newbie.address(), dropped));
        assert!(!t.c.state.conflicted(&juror.address(), &newbie.address(), dropped + 1));
        // Three slots (35 Soul, decaying): two held, so one more vouch, and no fourth.
        t.tx(&juror, Action::Vouch { account: newbie.address() }).unwrap();
        assert!(t.tx(&juror, Action::Vouch { account: k(8).address() }).unwrap_err().to_string().contains("allows 3"));
        let spent = b0 - t.bal(&juror);
        assert!((3 * VOUCH_BOND..3 * VOUCH_BOND + DROPS_PER_FANG / 10).contains(&spent), "three bonds held, plus fees: {spent}");
        // The voucher withdraws past the lock, the claim still open: its bond comes straight back,
        // and the earlier held ones at an epoch boundary once their locks are over.
        assert!(t.tx(&juror, Action::Unvouch { account: newbie.address() }).unwrap_err().to_string().contains("locked until"));
        t.blocks(100);
        let b1 = t.bal(&juror);
        t.tx(&juror, Action::Unvouch { account: newbie.address() }).unwrap();
        assert!(t.bal(&juror) + DROPS_PER_FANG / 100 > b1 + VOUCH_BOND);
        assert_eq!(joined(&t, &juror), first);
        assert!(!t.c.state.poc.vouches.contains_key(&newbie.address()));
        t.blocks(t.c.state.params.epoch_blocks);
        assert_eq!(t.c.state.vouches_held_by(&juror.address()), 0, "the held bonds came back");
        assert!(t.bal(&juror) + DROPS_PER_FANG / 10 > b0, "all three");
        conserved(&t.c.state);
        // The other way round: the voucher has the claim open and the vouchee sits in the pool.
        t.tx(&juror, Action::Vouch { account: newbie.address() }).unwrap();
        t.admin(Action::RejectClaim { claim_id: c, reason: "test".into() }).unwrap();
        t.grant(&newbie, 10);
        t.tx(&newbie, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        let (j_before, n_before) = (joined(&t, &juror), joined(&t, &newbie));
        t.tx(&newbie, Action::Unvouch { account: newbie.address() }).unwrap();
        assert_eq!((joined(&t, &juror), joined(&t, &newbie)), (j_before, n_before));
        assert!(t.c.state.conflicted(&newbie.address(), &juror.address(), t.h()));
        t.admin(Action::RejectClaim { claim_id: mine, reason: "test".into() }).unwrap();
        conserved(&t.c.state);
        t.reopen_same_root();
    }

    /// Audit F23: a spam verdict needs `SPAM_MIN_FLAGS` (3) flags and a majority of the jurors who
    /// saw the claim, so two colluding seats can't slash a claim only they (or few others) saw;
    /// and a flag the jury didn't uphold counts against the flagger's agreement.
    #[test]
    fn spam_needs_three_flags_and_overruled_flags_cost_agreement() {
        let mut t = setup("spamq");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        let jurors: Vec<Keys> = (1..=5).map(k).collect();
        for j in &jurors {
            t.grant(j, 10);
            t.tx(j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&k(9), Action::FundPool { amount: 50 * DROPS_PER_FANG }).unwrap();
        let mut claims = vec![];
        for (i, who) in [(10u8, "ten"), (11, "eleven"), (12, "twelve")] {
            t.grant(&k(i), 1);
            t.link(&k(i), who);
            claims.push(t.claim(&k(i), i as u64).unwrap());
        }
        t.attest_all(&claims);
        let sid = t.wait_for_seance();
        let (x, a, b) = (claims[0], claims[1], claims[2]);
        let base = t.c.state.clone();
        let h = t.h();
        // Every juror reveals an honest ballot (a > b > x) and the `flaggers` flag `x`; with
        // `only_two_see_x`, jurors 2–4 compare only `a` and `b`.
        let close = |flaggers: &[usize], only_two_see_x: bool| -> State {
            let mut st = base.clone();
            let s = st.poc.seances.get_mut(&sid).unwrap();
            for (i, jk) in jurors.iter().enumerate() {
                let seat = s.jurors.get_mut(&jk.address()).unwrap();
                if only_two_see_x && i >= 2 {
                    seat.pairs = vec![[a, b]];
                }
                let rank = |c: u64| if c == a { 3 } else if c == b { 2 } else { 1 };
                seat.votes = Some(seat.pairs.iter().map(|[p, q]| if rank(*p) > rank(*q) { 'a' } else { 'b' }).collect());
                seat.spam = if flaggers.contains(&i) { vec![x] } else { vec![] };
            }
            st.close_seance(sid, h);
            st.check_invariants().unwrap();
            st
        };
        let agreement = |st: &State, i: usize| st.poc.seances[&sid].jurors[&jurors[i].address()].agreement_ppm.unwrap();
        // Two of the five who saw it flag it: not spam, and both flags were overruled.
        let st = close(&[0, 1], false);
        assert_eq!(st.poc.claims[&x].status, ClaimStatus::Scored);
        assert!(agreement(&st, 0) < agreement(&st, 2), "an overruled flag costs agreement: {} vs {}", agreement(&st, 0), agreement(&st, 2));
        assert_eq!(agreement(&st, 2), PPM, "honest ballot, no flags");
        // Only two jurors saw `x`, and both flagged it: a majority of its viewers, but too few to
        // decide. It's scored (on ambiguity, never slashed), and the flags aren't graded.
        let st = close(&[0, 1], true);
        assert_eq!(st.poc.seances[&sid].jurors[&jurors[3].address()].pairs, vec![[a, b]]);
        assert_eq!(st.poc.claims[&x].status, ClaimStatus::Scored);
        assert_eq!(st.poc.claims[&x].bond, 0, "bond refunded");
        assert_eq!((agreement(&st, 0), agreement(&st, 1)), (PPM, PPM));
        // Three of five: spam (bond to the pool, Soul lost); upheld flags cost nothing.
        let st = close(&[0, 1, 2], false);
        assert_eq!(st.poc.claims[&x].status, ClaimStatus::Spam);
        assert_eq!(st.poc.seances[&sid].results.as_ref().unwrap().spam, 1);
        assert_eq!(agreement(&st, 0), PPM);
        std::fs::remove_dir_all(&t.home).ok();
    }

    fn claims_payout(t: &T, c: u64) -> u64 {
        let cl = &t.c.state.poc.claims[&c];
        t.c.state.poc.grants[&cl.claimant][&cl.grant.unwrap()].amount
    }

    /// Fills every seat of Séance `sid` with an honest ballot: the claim `rank` puts higher wins
    /// its pair, equal ranks tie.
    fn judge(st: &mut State, sid: u64, rank: impl Fn(u64) -> u32) {
        let s = st.poc.seances.get_mut(&sid).unwrap();
        for seat in s.jurors.values_mut() {
            seat.votes = Some(seat.pairs.iter().map(|[a, b]| match rank(*a).cmp(&rank(*b)) { std::cmp::Ordering::Greater => 'a', std::cmp::Ordering::Less => 'b', std::cmp::Ordering::Equal => '=' }).collect());
        }
    }

    /// What an account was paid in a closed Séance (its claims' payouts) and the Soul they earned.
    fn paid(st: &State, who: &Keys) -> (u64, u64) {
        st.poc.claims.values().filter(|c| c.claimant == who.address()).fold((0, 0), |(p, s), c| (p + c.payout, s + c.soul_awarded))
    }

    /// Audit xcut-economic-opus-1: volume doesn't pay. One account's 8 small PRs, each of which
    /// the jury judges worse than one solid PR, earn less than it (here nothing): one person's
    /// claims are never compared with each other (they'd tie, which floats weak claims over the
    /// threshold), and the budget grows with the people in a Séance, not their claim count. (With
    /// weak peers in the field, see `eight_weak_prs_never_beat_one_solid_one_among_weak_peers`.)
    #[test]
    fn eight_weak_prs_that_lose_every_comparison_earn_nothing() {
        let mut t = setup("volume");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for j in [k(1), k(2), k(3)] {
            t.grant(&j, 10);
            t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&k(9), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
        // The volume farmer has the Soul for 8 open claims; the solid contributor has one claim.
        let (farmer, solid) = (k(5), k(6));
        t.grant(&farmer, 60);
        t.grant(&solid, 1);
        t.link(&farmer, "volume");
        t.link(&solid, "solid");
        let small: Vec<u64> = (1..=8).map(|pr| t.claim(&farmer, pr).unwrap()).collect();
        let big = t.claim(&solid, 100).unwrap();
        t.attest_all(&[small.as_slice(), &[big]].concat());
        let sid = t.wait_for_seance();
        let s = t.c.state.poc.seances[&sid].clone();
        let pool = t.c.state.account(&t.c.state.pool()).balance + s.budget;
        assert_eq!(s.budget, bps(pool, SEANCE_POOL_BPS) * 2 / FULL_BUDGET_PEOPLE, "two people: 2/16 of a quarter, however many claims");
        for seat in s.jurors.values() {
            assert!(seat.pairs.iter().all(|[a, b]| (*a == big) != (*b == big)), "the farmer's claims are only ever compared with someone else's: {:?}", seat.pairs);
        }
        let mut st = t.c.state.clone();
        judge(&mut st, sid, |c| if c == big { 2 } else { 1 });
        st.close_seance(sid, t.h());
        st.check_invariants().unwrap();
        let ((farmer_paid, farmer_soul), (solid_paid, solid_soul)) = (paid(&st, &farmer), paid(&st, &solid));
        assert!(solid_paid > 0 && farmer_paid < solid_paid, "8 weak PRs earned {farmer_paid}, the solid one {solid_paid}");
        assert!(farmer_soul < solid_soul, "Soul: {farmer_soul} vs {solid_soul}");
        assert!(small.iter().all(|c| st.poc.claims[c].score_ppm == Some(0)), "each lost its only comparisons");
        // With other people in the Séance too (6 medium PRs, judged between the two), the farmer
        // still earns less than the solid PR and less than any medium one.
        let mut t = setup("volume2");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for j in [k(1), k(2), k(3), k(4)] {
            t.grant(&j, 10);
            t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&k(4), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
        t.grant(&farmer, 60);
        t.grant(&solid, 1);
        t.link(&farmer, "volume");
        t.link(&solid, "solid");
        let small: Vec<u64> = (1..=8).map(|pr| t.claim(&farmer, pr).unwrap()).collect();
        let big = t.claim(&solid, 100).unwrap();
        let mut medium = vec![];
        for i in 7..=12u8 {
            t.grant(&k(i), 1);
            t.link(&k(i), &format!("medium{i}"));
            medium.push(t.claim(&k(i), 200 + i as u64).unwrap());
        }
        t.attest_all(&[small.as_slice(), &[big], &medium].concat());
        let sid = t.wait_for_seance();
        let mut st = t.c.state.clone();
        judge(&mut st, sid, |c| if c == big { 3 } else if medium.contains(&c) { 2 } else { 1 });
        st.close_seance(sid, t.h());
        st.check_invariants().unwrap();
        let farmer_paid = paid(&st, &farmer).0;
        let best_medium = (7..=12u8).map(|i| paid(&st, &k(i)).0).min().unwrap();
        assert!(farmer_paid < paid(&st, &solid).0 && farmer_paid < best_medium, "farmer {farmer_paid}, solid {}, least medium {best_medium}", paid(&st, &solid).0);
        std::fs::remove_dir_all(&t.home).ok();
    }

    /// A Séance of one volume farmer's `small` weak PRs, one solid PR by someone else, and `peers`
    /// other people's PRs as weak as the farmer's. Honest jurors put the solid PR above every
    /// other and call every other pair a tie, so each weak claim loses to the solid one and ties
    /// the rest: with peers in the field the farmer's claims score up to 50%, above the threshold.
    /// Returns (farmer's payout, Soul), (solid's payout, Soul) and the farmer's scores.
    fn volume_field(tag: &str, peers: u8, small: u64, farmer_soul: u64, solid_soul: u64) -> ((u64, u64), (u64, u64), Vec<u64>) {
        let mut t = setup(tag);
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for j in [k(1), k(2), k(3)] {
            t.grant(&j, 10);
            t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&k(9), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
        let (farmer, solid) = (k(5), k(6));
        // Bonds grow with open claims: 16 of them take 136 FANG.
        t.c.faucet(&farmer.address(), 500 * DROPS_PER_FANG).unwrap();
        t.c.produce_block().unwrap();
        let mut left = farmer_soul;
        while left > 0 {
            t.grant(&farmer, left.min(100));
            left -= left.min(100);
        }
        t.grant(&solid, solid_soul);
        t.link(&farmer, "volume");
        t.link(&solid, "solid");
        let weak: Vec<u64> = (1..=small).map(|pr| t.claim(&farmer, pr).unwrap()).collect();
        let big = t.claim(&solid, 1_000).unwrap();
        let mut others = vec![];
        for i in 0..peers {
            let who = k(20 + i);
            t.c.faucet(&who.address(), 100 * DROPS_PER_FANG).unwrap();
            t.c.produce_block().unwrap();
            t.grant(&who, 1);
            t.link(&who, &format!("peer{i}"));
            others.push(t.claim(&who, 2_000 + i as u64).unwrap());
        }
        t.attest_all(&[weak.as_slice(), &[big], &others].concat());
        let sid = t.wait_for_seance();
        let mut st = t.c.state.clone();
        judge(&mut st, sid, |c| if c == big { 2 } else { 1 });
        st.close_seance(sid, t.h());
        st.check_invariants().unwrap();
        let scores = weak.iter().map(|c| st.poc.claims[c].score_ppm.unwrap_or(0)).collect();
        let out = (paid(&st, &farmer), paid(&st, &solid), scores);
        std::fs::remove_dir_all(&t.home).ok();
        out
    }

    /// The round-3 R1 reviewer's model (audit xcut-economic-opus-1): a Séance of one farmer's
    /// `small` weak PRs, one solid PR by someone else, and `peers` other people's PRs as weak as
    /// the farmer's, judged by `jurors` honest jurors. Each puts the solid PR above every other,
    /// but doesn't call weak against weak a tie: each juror has a fixed view of each such pair
    /// (from `salt`: a third a tie, a third each way), so the farmer's claims are no better than
    /// the peers' on average, yet each one's win rate is noisy. For each salt the jurors' pairs
    /// are drawn again too (as `open_seance` draws them). Returns, per salt, the farmer's payout
    /// and Soul, then the solid contributor's.
    fn noisy_field(tag: &str, jurors: u8, peers: u8, small: u64, farmer_soul: u64, solid_soul: u64, salts: u64) -> Vec<[u64; 4]> {
        let mut t = setup(tag);
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for i in 0..jurors {
            let j = k(40 + i);
            t.c.faucet(&j.address(), 100 * DROPS_PER_FANG).unwrap();
            t.c.produce_block().unwrap();
            t.grant(&j, 10);
            t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&k(9), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
        let (farmer, solid) = (k(5), k(6));
        t.c.faucet(&farmer.address(), 500 * DROPS_PER_FANG).unwrap();
        t.c.produce_block().unwrap();
        let mut left = farmer_soul;
        while left > 0 {
            t.grant(&farmer, left.min(100));
            left -= left.min(100);
        }
        t.grant(&solid, solid_soul);
        t.link(&farmer, "volume");
        t.link(&solid, "solid");
        let weak: Vec<u64> = (1..=small).map(|pr| t.claim(&farmer, pr).unwrap()).collect();
        let big = t.claim(&solid, 1_000).unwrap();
        let mut others = vec![];
        for i in 0..peers {
            let who = k(20 + i);
            t.c.faucet(&who.address(), 100 * DROPS_PER_FANG).unwrap();
            t.c.produce_block().unwrap();
            t.grant(&who, 1);
            t.link(&who, &format!("peer{i}"));
            others.push(t.claim(&who, 2_000 + i as u64).unwrap());
        }
        t.attest_all(&[weak.as_slice(), &[big], &others].concat());
        let sid = t.wait_for_seance();
        assert_eq!(t.c.state.poc.seances[&sid].jurors.len(), jurors as usize);
        let claims: Vec<(u64, String)> = t.c.state.poc.seances[&sid].claims.iter().map(|c| (*c, t.c.state.poc.claims[c].claimant.clone())).collect();
        let mut out = vec![];
        for salt in 0..salts {
            let mut st = t.c.state.clone();
            let seed = format!("{tag}-{salt}");
            for (juror, seat) in st.poc.seances.get_mut(&sid).unwrap().jurors.iter_mut() {
                let mut pairs = vec![];
                for (i, (a, who)) in claims.iter().enumerate() {
                    pairs.extend(claims[i + 1..].iter().filter(|(_, other)| other != who).map(|(b, _)| [*a, *b]));
                }
                Rng::new(&seed, juror.as_bytes()).shuffle(&mut pairs);
                pairs.truncate(PAIRS_PER_JUROR);
                let view = |a: u64, b: u64| {
                    let (lo, hi) = (a.min(b), a.max(b));
                    let winner = match Rng::new(&seed, &[juror.as_bytes(), &lo.to_le_bytes(), &hi.to_le_bytes()].concat()).below(3) {
                        0 => return '=',
                        1 => lo,
                        _ => hi,
                    };
                    if winner == a { 'a' } else { 'b' }
                };
                seat.votes = Some(pairs.iter().map(|[a, b]| if *a == big { 'a' } else if *b == big { 'b' } else { view(*a, *b) }).collect());
                seat.pairs = pairs;
            }
            st.close_seance(sid, t.h());
            st.check_invariants().unwrap();
            let ((fp, fs), (sp, ss)) = (paid(&st, &farmer), paid(&st, &solid));
            out.push([fp, fs, sp, ss]);
        }
        std::fs::remove_dir_all(&t.home).ok();
        out
    }

    /// Audit xcut-economic-opus-1, round 4 (the round-3 R1 reviewer's bypass): with honest jurors
    /// who prefer one weak PR to another rather than calling it a tie, each weak claim's win rate
    /// is noisy, and paying a person's best claim paid the best of k noisy rates, which grows with
    /// k: 16 weak PRs out-earned the solid one in 39 of 40 of the reviewer's runs (1.27× on
    /// average), and in 40 of 40 of this model's (1.37×), 1.70× what one weak claim earned. A
    /// person's claims now share one win rate pooled over
    /// all their comparisons, so more claims don't raise it, and the ranks add at most a third:
    /// over 40 draws, k weak claims earn at most 4/3 of what one does (FANG and Soul), and the
    /// farmer's 16 with the full Soul boost stay below one solid PR by a newcomer.
    #[test]
    fn volume_doesnt_pay_against_honest_split_votes() {
        const SALTS: u64 = 40;
        let mean = |v: &[[u64; 4]], i: usize| v.iter().map(|x| x[i] as u128).sum::<u128>() / v.len() as u128;
        for (tag, jurors, peers, many, farmer_soul, solid_soul) in [("noisy7", 7u8, 7u8, 16u64, 170u64, 1u64), ("noisy3", 3, 3, 8, 60, 1), ("noisy3-even", 3, 3, 8, 60, 60)] {
            let one = noisy_field(&format!("{tag}-1"), jurors, peers, 1, farmer_soul, solid_soul, SALTS);
            let lots = noisy_field(&format!("{tag}-k"), jurors, peers, many, farmer_soul, solid_soul, SALTS);
            let beats = |v: &[[u64; 4]]| v.iter().filter(|x| x[0] > x[2]).count();
            eprintln!("{tag}: one claim {} (Soul {}), {many} claims {} (Soul {}), solid {} (Soul {}); one beat the solid PR in {}/{SALTS}, {many} in {}/{SALTS}", mean(&one, 0), mean(&one, 1), mean(&lots, 0), mean(&lots, 1), mean(&lots, 2), mean(&lots, 3), beats(&one), beats(&lots));
            assert!(3 * mean(&lots, 0) <= 4 * mean(&one, 0), "{tag}: {many} claims earned {} on average, one claim {}", mean(&lots, 0), mean(&one, 0));
            assert!(3 * mean(&lots, 1) <= 4 * mean(&one, 1), "{tag}: Soul {} vs {}", mean(&lots, 1), mean(&one, 1));
            assert!(mean(&lots, 0) < mean(&lots, 2), "{tag}: {many} weak PRs earned {} on average, the solid one {}", mean(&lots, 0), mean(&lots, 2));
            assert!(mean(&lots, 1) < mean(&lots, 3), "{tag}: Soul {} vs {}", mean(&lots, 1), mean(&lots, 3));
            // Run by run, the pile beats the solid PR only where the draw rates the farmer's weak
            // claims above half overall (at the full boost, 4/3 × 1.5 × ½ = 1): 3 draws of 40 at
            // the extreme, against all 40 under the best-claim rule.
            assert!(beats(&lots) <= SALTS as usize / 10, "{tag}: {many} weak PRs beat the solid one in {}/{SALTS} draws", beats(&lots));
        }
    }

    /// Audit xcut-economic-opus-1, round 4: a person's claims are rated together. The farmer's 4
    /// claims: one as good as a peer's (it ties the peer, loses to the solid PR, and scores over
    /// the threshold on its own), three that lose every comparison. Paying the best claim on its
    /// own score let a farmer throw many claims and be paid for the lucky one; the pooled rate
    /// counts the three too, so the four together fall below the threshold and earn nothing. The
    /// same claim alone is paid.
    #[test]
    fn a_persons_claims_are_rated_together() {
        for alone in [false, true] {
            let mut t = setup(if alone { "together1" } else { "together4" });
            t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
            for j in [k(1), k(2), k(3)] {
                t.grant(&j, 10);
                t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
            }
            t.tx(&k(9), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
            let (farmer, solid, peer) = (k(5), k(6), k(7));
            t.grant(&farmer, 60);
            t.grant(&solid, 1);
            t.grant(&peer, 1);
            t.link(&farmer, "volume");
            t.link(&solid, "solid");
            t.link(&peer, "peer");
            let mine: Vec<u64> = (1..=if alone { 1 } else { 4 }).map(|pr| t.claim(&farmer, pr).unwrap()).collect();
            let (big, theirs) = (t.claim(&solid, 100).unwrap(), t.claim(&peer, 200).unwrap());
            t.attest_all(&[mine.as_slice(), &[big, theirs]].concat());
            let sid = t.wait_for_seance();
            let mut st = t.c.state.clone();
            judge(&mut st, sid, |c| if c == big { 3 } else if c == theirs || c == mine[0] { 2 } else { 0 });
            st.close_seance(sid, t.h());
            st.check_invariants().unwrap();
            let lucky = &st.poc.claims[&mine[0]];
            assert!(lucky.score_ppm.unwrap() >= MIN_SCORE_PPM, "on its own it's over the threshold: {:?}", lucky.score_ppm);
            assert!(mine[1..].iter().all(|c| st.poc.claims[c].score_ppm == Some(0)));
            assert!(paid(&st, &solid).0 > 0 && paid(&st, &peer).0 > 0);
            if alone {
                assert!(paid(&st, &farmer).0 > 0 && lucky.reason.is_none());
            } else {
                assert_eq!(paid(&st, &farmer), (0, 0), "the farmer's claims together scored under the threshold");
                assert_eq!(lucky.status, ClaimStatus::Scored);
                assert!(lucky.reason.as_deref().unwrap().contains("together scored below the reward threshold"), "{:?}", lucky.reason);
                assert_eq!(lucky.bond, 0, "its bond came back");
            }
            std::fs::remove_dir_all(&t.home).ok();
        }
    }

    /// Audit review-r4-poc-2: the pooled rate is a ceiling, not a lift. A farmer's weak claim in a
    /// heavy repo (weight 10), bundled with three strong claims in a light one (weight 1), was paid
    /// at the strong claims' pooled rate × the heavy repo's weight: about twice what it earns alone,
    /// nearly a solid PR's payout. Each claim is now paid at its own score where that's lower, so
    /// bundled or not, the weak claim is paid in proportion to its own score.
    #[test]
    fn a_weak_claim_doesnt_borrow_another_repos_quality() {
        let field = |bundled: bool| {
            let mut t = setup(if bundled { "lift-bundled" } else { "lift-alone" });
            t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
            t.admin(Action::SetRepo { repo: "jackdishman/docs".into(), weight: 1 }).unwrap();
            for j in [k(1), k(2), k(3), k(4)] {
                t.grant(&j, 10);
                t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
            }
            t.tx(&k(4), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
            let (farmer, solid) = (k(5), k(6));
            t.grant(&farmer, 60);
            t.grant(&solid, 60);
            t.link(&farmer, "farmer");
            t.link(&solid, "solid");
            let weak = t.claim(&farmer, 1).unwrap();
            let mut docs = vec![];
            if bundled {
                for pr in 2..=4 {
                    t.tx(&farmer, Action::SubmitClaim { repo: "jackdishman/docs".into(), pr, familiar_key: None }).unwrap();
                    docs.push(t.c.state.poc.next_claim_id - 1);
                }
            }
            let big = t.claim(&solid, 100).unwrap();
            let mut peers = vec![];
            for i in 7..=10u8 {
                t.grant(&k(i), 1);
                t.link(&k(i), &format!("peer{i}"));
                peers.push(t.claim(&k(i), 200 + i as u64).unwrap());
            }
            t.attest_all(&[&[weak, big][..], &docs, &peers].concat());
            let sid = t.wait_for_seance();
            let mut st = t.c.state.clone();
            judge(&mut st, sid, |c| if c == big || docs.contains(&c) { 3 } else { 1 });
            st.close_seance(sid, t.h());
            st.check_invariants().unwrap();
            let [(weak_score, weak_paid), (big_score, big_paid)] = [weak, big].map(|c| (st.poc.claims[&c].score_ppm.unwrap() as u128, st.poc.claims[&c].payout as u128));
            let out = (weak_score, weak_paid, big_score, big_paid, paid(&st, &farmer).0);
            std::fs::remove_dir_all(&t.home).ok();
            out
        };
        // Both claims are in the heavy repo, ranked first by people with the same Soul, so each is
        // paid in proportion to the rate it's paid at: the weak one's own score, bundled or not
        // (rounding aside).
        for bundled in [false, true] {
            let (weak_score, weak_paid, big_score, big_paid, farmer_paid) = field(bundled);
            assert!(weak_score >= MIN_SCORE_PPM as u128 && weak_paid > 0 && big_score > weak_score, "bundled {bundled}: the weak claim is accepted: {weak_score} {weak_paid}");
            assert!(weak_paid * big_score <= big_paid * weak_score + big_score, "bundled {bundled}: the weak claim (score {weak_score}) earned {weak_paid}, the solid one (score {big_score}) {big_paid}");
            // The draw varies: a weak claim can tie every peer it meets and score one half.
            // Its small docs rewards then legitimately bring the bundle above half of the
            // solid claim. The score-proportional assertion above catches borrowed quality;
            // the full bundle must still earn less than the solid core PR, not less than half.
            assert!(farmer_paid < big_paid as u64, "bundled {bundled}: the farmer earned {farmer_paid}, the solid PR {big_paid}");
        }
    }

    /// Audit xcut-economic-opus-1, round 3 (the R1 reviewer's bypass): with other people's weak
    /// PRs in the field, the farmer's weak claims tie theirs and score 25–50%, over the threshold.
    /// Under a square root of their sum, 8 of them earned 1.82× the solid PR with one weak peer
    /// and 2.16× with three (and 2–3× the Soul). Now a person's claims count one after another,
    /// each a quarter of the one above, at the person's pooled rate, so when the jury calls weak
    /// against weak a tie, 8 (or 16) weak PRs never beat one solid PR, in FANG or in Soul, even
    /// when the farmer has the full Soul boost and the solid contributor almost none. (When
    /// jurors pick sides instead: `volume_doesnt_pay_against_honest_split_votes`.)
    #[test]
    fn eight_weak_prs_never_beat_one_solid_one_among_weak_peers() {
        for (tag, peers, small, farmer_soul, solid_soul) in [("weak1", 1, 8, 60, 1), ("weak3", 3, 8, 60, 1), ("weak3-even", 3, 8, 60, 60), ("weak7-boost", 7, 16, 170, 1)] {
            let ((fp, fs), (sp, ss), scores) = volume_field(tag, peers, small, farmer_soul, solid_soul);
            let best = scores.iter().copied().max().unwrap();
            assert!((MIN_SCORE_PPM..=PPM / 2).contains(&best), "{tag}: the weak claims are accepted, and none beats the field: {scores:?}");
            assert!(sp > 0 && fp < sp, "{tag}: {small} weak PRs earned {fp}, the solid one {sp} (scores {scores:?})");
            assert!(fs < ss, "{tag}: Soul {fs} vs {ss}");
            // Their Soul together is under 4/3 of what the best one alone would earn.
            assert!(3 * fs as u128 <= 4 * (CLAIM_SOUL as u128 * best as u128 / PPM as u128), "{tag}: Soul {fs}, best claim {best}");
        }
    }

    /// Audit xcut-economic-opus-3: a person is the sponsor account, so one account that relinks a
    /// second GitHub handle between claims has all of them rated and ranked together (one pooled
    /// rate, each next a quarter), and earns what one handle with the same claims would.
    #[test]
    fn relinking_a_second_handle_is_still_one_person() {
        let mut t = setup("relink");
        t.admin(Action::SetRepo { repo: "jackdishman/blockula".into(), weight: 10 }).unwrap();
        for j in [k(1), k(2), k(3)] {
            t.grant(&j, 10);
            t.tx(&j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&k(9), Action::FundPool { amount: 80 * DROPS_PER_FANG }).unwrap();
        let (a, b) = (k(5), k(6));
        t.grant(&a, 15);
        t.grant(&b, 15);
        t.link(&a, "a-one");
        let mut mine = vec![t.claim(&a, 1).unwrap(), t.claim(&a, 2).unwrap()];
        t.attest_all(&mine);
        t.tx(&a, Action::UnlinkGithub { handle: "a-one".into() }).unwrap();
        t.link(&a, "a-two");
        mine.extend([t.claim(&a, 3).unwrap(), t.claim(&a, 4).unwrap()]);
        t.link(&b, "b");
        let theirs: Vec<u64> = (5..=8).map(|pr| t.claim(&b, pr).unwrap()).collect();
        t.attest_all(&[&mine[2..], theirs.as_slice()].concat());
        assert_eq!((t.c.state.poc.handles["a-one"].clone(), t.c.state.poc.handles["a-two"].clone()), (a.address(), a.address()));
        let sid = t.wait_for_seance();
        assert_eq!(t.c.state.poc.seances[&sid].claims.len(), 8);
        // Every comparison a tie: every claim scores 50%.
        let mut st = t.c.state.clone();
        judge(&mut st, sid, |_| 0);
        st.close_seance(sid, t.h());
        st.check_invariants().unwrap();
        let (pa, pb) = (paid(&st, &a).0, paid(&st, &b).0);
        // (Their Soul boosts differ by a hair: the two grants decay from blocks a few apart.)
        assert!(pa > 0 && pa.abs_diff(pb) <= pb / 10_000, "two handles {pa}, one handle {pb}");
        std::fs::remove_dir_all(&t.home).ok();
    }
}
