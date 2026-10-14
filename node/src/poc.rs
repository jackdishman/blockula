//! 🪦 Proof of Contribution: Souls (reputation that can't be bought), vouching, claims on merged
//! PRs that validators check against GitHub, Séances (random commit-reveal review juries) and the
//! contribution pool, paid per sponsor as a square root and vested with clawback.
//!
//! Lifecycle of a claim: `submit_claim` (bonded) → validators attest what GitHub says → verified
//! once a BFT quorum agrees on the merge commit → after `claim_maturity_blocks` it enters the next
//! Séance → jurors compare pairs of claims ("which mattered more?"), commit, then reveal → the
//! Séance closes: scores, Soul, rewards (vesting) and bonds are settled.
//!
//! Everything here is deterministic integer math over `BTreeMap`s; the open/close of Séances runs
//! in `end_block`, after a block's txs, on every node. The constants below (and the default
//! timings) are consensus rules: changing one changes every chain that uses it.

use crate::chain::{short, Journal, State};
use crate::crypto;
use crate::types::*;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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
/// A Séance's budget scales with its claims, reaching the full share at this many (so a two-claim
/// Séance can't take a quarter of the pool).
pub const FULL_BUDGET_CLAIMS: u64 = 16;
pub const JURY_SIZE: usize = 7;
/// Jurors needed to seat a Séance, and reveals needed to settle it (otherwise claims carry over).
pub const MIN_JURY: usize = 3;
pub const PAIRS_PER_JUROR: usize = 24;
/// Share of the pool's balance each Séance distributes, in basis points.
pub const SEANCE_POOL_BPS: u64 = 2_500;
/// Share of a Séance's budget paid to jurors (by agreement).
pub const JUROR_SHARE_BPS: u64 = 1_000;
/// A voucher's cut of a newcomer's rewards while the vouch is young (`vouch_lock_blocks`).
pub const VOUCHER_CUT_BPS: u64 = 1_000;
pub const PPM: u64 = 1_000_000;
/// Claims scoring below this (win rate in their comparisons) get their bond back but no reward.
pub const MIN_SCORE_PPM: u64 = 150_000;
/// Two scores closer than this count as "about equal" when grading jurors.
pub const TIE_MARGIN_PPM: u64 = 50_000;
/// Jurors at or above this agreement with the outcome gain Soul and share the jurors' budget;
/// below `BAD_AGREEMENT_PPM` they lose Soul. (Voting "=" on everything scores 50%: no reward.)
pub const GOOD_AGREEMENT_PPM: u64 = 600_000;
pub const BAD_AGREEMENT_PPM: u64 = 400_000;
/// Soul for a claim at a perfect score (scaled by its score).
pub const CLAIM_SOUL: u64 = 10 * SOUL;
pub const JUROR_GOOD_SOUL: u64 = SOUL;
pub const JUROR_BAD_SOUL: u64 = 2 * SOUL;
/// Not revealing costs at least as much as revealing a ballot that diverges (plus the bond slash),
/// so jurors can't hold back bad ballots.
pub const NO_SHOW_SOUL: u64 = JUROR_BAD_SOUL;
pub const NO_SHOW_SLASH_BPS: u64 = 1_000;
pub const SPAM_SOUL: u64 = 20 * SOUL;
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// How long to wait before trying again when a Séance can't open (too few claims/jurors):
    /// about an hour for a week-long Séance.
    pub fn retry_blocks(&self) -> u64 {
        (self.seance_blocks / 168).max(1)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vouch {
    pub voucher: String,
    #[serde(with = "u64_str")]
    pub bond: u64,
    pub at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sybil {
    pub at: u64,
    pub by: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attestation {
    pub ok: bool,
    pub merge_sha: String,
    pub at: u64,
    /// Why it was refuted (what the validator saw on GitHub).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Juror {
    #[serde(with = "u64_str")]
    pub bond: u64,
    pub joined_at: u64,
    /// No-shows are deactivated until they join again.
    pub active: bool,
    pub served: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GrantSource {
    Claim { claim_id: u64 },
    Jury { seance_id: u64 },
    /// A voucher's cut of the newcomer's reward for `claim_id`.
    Vouch { claim_id: u64 },
}

/// A reward that vests linearly from `start` to `end`. Unvested FANG can be clawed back.
#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Poc {
    /// Lowercase `owner/name` → weight.
    pub repos: BTreeMap<String, u64>,
    pub souls: BTreeMap<String, Soul>,
    /// Vouchee → the vouch.
    pub vouches: BTreeMap<String, Vouch>,
    pub sybils: BTreeMap<String, Sybil>,
    pub claims: BTreeMap<u64, Claim>,
    /// `repo#pr` → claim id, so a PR is claimed once (cleared when validators reject it).
    pub claim_by_pr: BTreeMap<String, u64>,
    /// Lowercase GitHub handle → the account its first verified claim credited. A handle's work
    /// is always credited to one account, so relinking it can't split one person's √ payout.
    #[serde(default)]
    pub handles: BTreeMap<String, String>,
    pub jurors: BTreeMap<String, Juror>,
    pub seances: BTreeMap<u64, Seance>,
    /// Account → grant id → grant.
    pub grants: BTreeMap<String, BTreeMap<u64, Grant>>,
    pub next_claim_id: u64,
    pub next_seance_id: u64,
    pub next_grant_id: u64,
    /// BLAKE3 chain over every block hash since Proof of Contribution was first used.
    pub seed: String,
    pub open_seance: Option<u64>,
    /// Earliest height the next Séance may open.
    pub next_seance_at: u64,
}

impl Poc {
    /// False on chains that never used it, so their state roots are unchanged.
    pub fn is_active(&self) -> bool {
        !(self.seed.is_empty() && self.repos.is_empty() && self.souls.is_empty() && self.vouches.is_empty() && self.sybils.is_empty() && self.claims.is_empty() && self.jurors.is_empty() && self.grants.is_empty() && self.handles.is_empty())
    }

    pub fn has_open_claims(&self, account: &str) -> usize {
        self.claims.values().filter(|c| c.claimant == account && c.status.is_open()).count()
    }
}

// ------------------------------------------------------------------------------------ journal

/// Old values of touched entries, for rollback (first touch wins).
pub struct JMap<K, V>(HashMap<K, Option<V>>);

impl<K, V> Default for JMap<K, V> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: Ord + std::hash::Hash + Clone, V: Clone> JMap<K, V> {
    fn touch(&mut self, map: &BTreeMap<K, V>, k: &K) {
        if !self.0.contains_key(k) {
            self.0.insert(k.clone(), map.get(k).cloned());
        }
    }
    fn absorb(&mut self, other: Self) {
        for (k, v) in other.0 {
            self.0.entry(k).or_insert(v);
        }
    }
    fn restore(self, map: &mut BTreeMap<K, V>) {
        for (k, old) in self.0 {
            match old {
                Some(v) => {
                    map.insert(k, v);
                }
                None => {
                    map.remove(&k);
                }
            }
        }
    }
}

type Scalars = (u64, u64, u64, String, Option<u64>, u64);

#[derive(Default)]
pub struct PocJournal {
    repos: JMap<String, u64>,
    souls: JMap<String, Soul>,
    vouches: JMap<String, Vouch>,
    sybils: JMap<String, Sybil>,
    claims: JMap<u64, Claim>,
    claim_by_pr: JMap<String, u64>,
    handles: JMap<String, String>,
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
        self.sybils.absorb(o.sybils);
        self.claims.absorb(o.claims);
        self.claim_by_pr.absorb(o.claim_by_pr);
        self.handles.absorb(o.handles);
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
        self.sybils.restore(&mut p.sybils);
        self.claims.restore(&mut p.claims);
        self.claim_by_pr.restore(&mut p.claim_by_pr);
        self.handles.restore(&mut p.handles);
        self.jurors.restore(&mut p.jurors);
        self.seances.restore(&mut p.seances);
        self.grants.restore(&mut p.grants);
        if let Some((a, b, c, seed, open, next)) = self.scalars {
            (p.next_claim_id, p.next_seance_id, p.next_grant_id, p.seed, p.open_seance, p.next_seance_at) = (a, b, c, seed, open, next);
        }
    }
}

// ------------------------------------------------------------------------------- randomness

/// Deterministic stream from the Séance seed (BLAKE3 XOF, domain-separated per use).
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

/// Integer square root (Newton's method), floor.
fn isqrt(n: u128) -> u128 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = x / 2 + x % 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
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
            j.poc.scalars = Some((p.next_claim_id, p.next_seance_id, p.next_grant_id, p.seed.clone(), p.open_seance, p.next_seance_at));
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

    fn set_claim_by_pr(&mut self, j: &mut Journal, key: &str, id: Option<u64>) {
        let key = key.to_string();
        self.pj(j).claim_by_pr.touch(&self.poc.claim_by_pr, &key);
        match id {
            Some(id) => self.poc.claim_by_pr.insert(key, id),
            None => self.poc.claim_by_pr.remove(&key),
        };
    }

    fn add_grant(&mut self, j: &mut Journal, account: &str, amount: u64, height: u64, source: GrantSource) -> u64 {
        self.pj(j);
        let id = self.poc.next_grant_id;
        self.poc.next_grant_id += 1;
        let end = height.saturating_add(self.params.poc.vesting_blocks);
        self.grants_mut(j, account).insert(id, Grant { id, amount, withdrawn: 0, start: height, end, source });
        id
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
        let owner = key.role == KeyRole::Owner;
        let need_owner = |what: &str| -> Result<()> {
            if !owner {
                bail!("only owner keys can {what}");
            }
            Ok(())
        };
        let need_admin = |what: &str| -> Result<()> {
            if !admin {
                bail!("only validator admins can {what} (Season 0)");
            }
            Ok(())
        };
        match &tx.action {
            Action::SetRepo { repo, weight } => {
                need_admin("set pool repos")?;
                let k = repo.to_ascii_lowercase();
                self.pj(j).repos.touch(&self.poc.repos, &k);
                if *weight == 0 {
                    self.poc.repos.remove(&k).ok_or_else(|| anyhow!("{repo} isn't in the contribution pool"))?;
                    // Claims waiting on it would be stuck: refund and close them.
                    let stuck: Vec<(u64, u64)> = self.poc.claims.values().filter(|c| c.repo == k && matches!(c.status, ClaimStatus::Pending | ClaimStatus::Verified)).map(|c| (c.id, c.pr)).collect();
                    for (id, pr) in stuck {
                        self.settle_bond(j, id, true);
                        self.set_status(j, id, ClaimStatus::Rejected, height, Some("the repo left the contribution pool; bond refunded".into()));
                        self.set_claim_by_pr(j, &format!("{k}#{pr}"), None);
                    }
                } else {
                    self.poc.repos.insert(k, *weight);
                }
            }
            Action::GrantSoul { to, amount, .. } => {
                need_admin("grant Soul")?;
                let to = self.resolve(to)?;
                if self.is_sybil(&to) {
                    bail!("{} is flagged as a sybil", short(&to));
                }
                self.add_soul(j, &to, *amount as i64, height);
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
                let mine = self.poc.vouches.values().filter(|v| v.voucher == sender).count();
                if mine >= self.max_vouches(sender, height) {
                    bail!("your Soul allows {} active vouches (one per 10 Soul)", self.max_vouches(sender, height));
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
                self.pj(j).vouches.touch(&self.poc.vouches, &to);
                self.poc.vouches.remove(&to);
                self.j_credit(j, &v.voucher, v.bond);
            }
            Action::FlagSybil { account, reason } => {
                need_admin("flag sybils")?;
                let who = self.resolve(account)?;
                if self.is_sybil(&who) {
                    bail!("{} is already flagged", short(&who));
                }
                if self.params.admins.contains(&who) {
                    bail!("admins can't be flagged");
                }
                self.flag_sybil(j, &who, sender, reason, height);
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
                let set = self.active_sets.get(&self.params.epoch_of(height)).cloned().unwrap_or_default();
                if !set.iter().any(|v| v == sender) {
                    bail!("only active validators can attest claims");
                }
                let c = self.poc.claims.get(claim_id).ok_or_else(|| anyhow!("no claim #{claim_id}"))?;
                if c.status != ClaimStatus::Pending {
                    bail!("claim #{claim_id} is {:?}, not pending", c.status);
                }
                if c.attestations.contains_key(sender) {
                    bail!("this validator already attested claim #{claim_id}");
                }
                let c = self.claim_mut(j, *claim_id).unwrap();
                c.attestations.insert(sender.to_string(), Attestation { ok: *ok, merge_sha: merge_sha.clone(), at: height, note: note.clone() });
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
                let c = self.poc.claims.get(claim_id).cloned().ok_or_else(|| anyhow!("no claim #{claim_id}"))?;
                match c.status {
                    // Never verified: the PR wasn't proven to be theirs, so its author can still claim it.
                    ClaimStatus::Pending => {
                        self.settle_bond(j, *claim_id, false);
                        self.set_claim_by_pr(j, &format!("{}#{}", c.repo, c.pr), None);
                    }
                    ClaimStatus::Verified | ClaimStatus::InSeance => self.settle_bond(j, *claim_id, false),
                    ClaimStatus::Scored => {
                        if let Some(g) = c.grant {
                            self.claw_back(j, &c.claimant, Some(g), height);
                        }
                        let cuts: Vec<(String, u64)> = self.poc.grants.iter().flat_map(|(a, gs)| gs.values().filter(|g| g.source == GrantSource::Vouch { claim_id: *claim_id }).map(move |g| (a.clone(), g.id))).collect();
                        for (voucher, g) in cuts {
                            self.claw_back(j, &voucher, Some(g), height);
                        }
                        self.add_soul(j, &c.claimant, -(c.soul_awarded as i64), height);
                    }
                    ClaimStatus::Spam | ClaimStatus::Rejected => bail!("claim #{claim_id} is already {:?}", c.status),
                }
                self.set_status(j, *claim_id, ClaimStatus::Rejected, height, Some(reason.clone()));
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
                r.active = true;
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
            Action::CommitVotes { seance_id, commitment } => {
                let s = self.poc.seances.get(seance_id).ok_or_else(|| anyhow!("no Séance #{seance_id}"))?;
                if s.closed {
                    bail!("Séance #{seance_id} is closed");
                }
                if !s.jurors.contains_key(sender) {
                    bail!("you're not on the jury of Séance #{seance_id}");
                }
                if height > s.commit_until {
                    bail!("sealed votes for Séance #{seance_id} closed at block {}; reveal now", s.commit_until);
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
                let who = self.resolve(account)?;
                if self.claw_back(j, &who, None, height) == 0 {
                    bail!("{} has no unvested rewards", short(&who));
                }
            }
            _ => bail!("not a contribution action"),
        }
        Ok(())
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
        // Whoever vouched for it loses the bond and Soul; vouches it gave are void.
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
    }

    /// Plain-English summary of a contribution tx (see `State::describe`).
    pub fn describe_poc(&self, tx: &Tx, from: &str) -> String {
        let who = |a: &str| match self.resolve(a) {
            Ok(addr) => self.primary_name(&addr).unwrap_or_else(|| short(&addr)),
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

    /// After every block's txs: advance the randomness, then close and open Séances.
    pub fn poc_end_block(&mut self, height: u64, block_hash: &str) {
        if !self.poc.is_active() {
            return;
        }
        let mut h = blake3::Hasher::new_derive_key("blockula poc seed v1");
        h.update(self.poc.seed.as_bytes());
        h.update(block_hash.as_bytes());
        self.poc.seed = h.finalize().to_hex().to_string();
        if let Some(id) = self.poc.open_seance {
            if self.poc.seances.get(&id).is_some_and(|s| height >= s.reveal_until) {
                self.close_seance(id, height);
            }
        }
        if self.poc.open_seance.is_none() && height >= self.poc.next_seance_at {
            self.open_seance(height);
        }
    }

    /// Claims a juror mustn't judge: their own, and those of people they vouched for or who
    /// vouched for them.
    fn conflicted(&self, juror: &str, claimant: &str) -> bool {
        juror == claimant || self.poc.vouches.get(claimant).is_some_and(|v| v.voucher == juror) || self.poc.vouches.get(juror).is_some_and(|v| v.voucher == claimant)
    }

    fn open_seance(&mut self, height: u64) {
        let mut j = Journal::default();
        let times = self.params.poc.clone();
        // Pending claims validators never settled are refunded (not the claimant's fault).
        let stale: Vec<(u64, String)> = self.poc.claims.values().filter(|c| c.status == ClaimStatus::Pending && height >= c.submitted_at.saturating_add(times.claim_ttl_blocks)).map(|c| (c.id, format!("{}#{}", c.repo, c.pr))).collect();
        for (id, pr_key) in stale {
            self.settle_bond(&mut j, id, true);
            self.set_status(&mut j, id, ClaimStatus::Rejected, height, Some("validators didn't settle it in time; bond refunded".into()));
            self.set_claim_by_pr(&mut j, &pr_key, None);
        }
        let claims: Vec<(u64, String)> = self
            .poc
            .claims
            .values()
            .filter(|c| c.status == ClaimStatus::Verified && c.verified_at.is_some_and(|v| height >= v.saturating_add(times.claim_maturity_blocks)) && !self.is_sybil(&c.claimant) && self.poc.repos.contains_key(&c.repo))
            .take(MAX_SEANCE_CLAIMS)
            .map(|c| (c.id, c.claimant.clone()))
            .collect();
        let retry = |s: &mut State| s.poc.next_seance_at = height + times.retry_blocks();
        if claims.len() < 2 {
            return retry(self);
        }
        let seed = self.poc.seed.clone();
        let mut pool: Vec<String> = self.poc.jurors.iter().filter(|(a, r)| r.active && !self.is_sybil(a) && self.soul_of(a, height) >= JUROR_MIN_SOUL).map(|(a, _)| a.clone()).collect();
        let id = self.poc.next_seance_id;
        let mut rng = Rng::new(&seed, &[b"jury".as_slice(), &id.to_le_bytes()].concat());
        rng.shuffle(&mut pool);
        let mut seats = BTreeMap::new();
        for juror in pool {
            if seats.len() >= JURY_SIZE {
                break;
            }
            let allowed: Vec<u64> = claims.iter().filter(|(_, who)| !self.conflicted(&juror, who)).map(|(id, _)| *id).collect();
            if allowed.len() < 2 {
                continue;
            }
            let mut pairs = Vec::with_capacity(allowed.len() * (allowed.len() - 1) / 2);
            for (i, a) in allowed.iter().enumerate() {
                for b in &allowed[i + 1..] {
                    pairs.push([*a, *b]);
                }
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
            return retry(self);
        }
        let pool_addr = self.pool();
        let budget = (bps(self.account(&pool_addr).balance, SEANCE_POOL_BPS) as u128 * (claims.len() as u64).min(FULL_BUDGET_CLAIMS) as u128 / FULL_BUDGET_CLAIMS as u128) as u64;
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
            if f >= 2 && 2 * f > seen.get(c).copied().unwrap_or(0) {
                self.settle_bond(&mut j, *c, false);
                self.set_status(&mut j, *c, ClaimStatus::Spam, height, Some(format!("{f} of the jurors who saw it flagged it as spam")));
                soul_changes.push((claimant, -(SPAM_SOUL as i64), None));
                results.spam += 1;
            } else if sc < MIN_SCORE_PPM {
                self.settle_bond(&mut j, *c, true);
                self.set_status(&mut j, *c, ClaimStatus::Scored, height, Some("scored below the reward threshold".into()));
                results.below_threshold += 1;
            } else {
                self.settle_bond(&mut j, *c, true);
                self.set_status(&mut j, *c, ClaimStatus::Scored, height, None);
                soul_changes.push((claimant, (CLAIM_SOUL as u128 * sc as u128 / PPM as u128) as i64, Some(*c)));
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
        // Rewards: jurors by agreement; claims by score × repo weight, square-rooted per person
        // (per GitHub handle, so a sponsor's Familiars and relinked accounts are one person),
        // boosted up to 1.5× by the claimant's Soul.
        let juror_budget = if juror_weights.is_empty() { 0 } else { bps(s.budget, JUROR_SHARE_BPS) };
        let claims_budget = if accepted.is_empty() { 0 } else { s.budget - juror_budget };
        let mut raw: BTreeMap<String, Vec<(u64, u128)>> = BTreeMap::new();
        for c in &accepted {
            let cl = &self.poc.claims[c];
            let w = self.poc.repos.get(&cl.repo).copied().unwrap_or(0) as u128 * score[c] as u128;
            raw.entry(cl.handle.to_ascii_lowercase()).or_default().push((*c, w));
        }
        let person_weights: Vec<(String, u128)> = raw
            .iter()
            .map(|(handle, cs)| {
                let soul = cs.iter().map(|(c, _)| self.soul_of(&self.poc.claims[c].claimant, height)).max().unwrap_or(0).min(BOOST_CAP_SOUL) as u128;
                let boost = PPM as u128 + soul * (PPM as u128 / 2) / BOOST_CAP_SOUL as u128;
                (handle.clone(), isqrt(cs.iter().map(|(_, w)| w).sum::<u128>()) * boost / PPM as u128)
            })
            .collect();
        let mut paid = 0u64;
        for (handle, amount) in split(claims_budget, &person_weights) {
            let per_claim: Vec<(String, u128)> = raw[&handle].iter().map(|(c, w)| (c.to_string(), *w)).collect();
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
                cl.grant = Some(g);
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
        self.poc.next_seance_at = height + 1;
    }
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
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(99), 9);
        assert_eq!(isqrt(100), 10);
        assert_eq!(isqrt(u64::MAX as u128 * 4), 2 * (u32::MAX as u128 + 1) - 1);
    }

    #[test]
    fn rng_is_deterministic_and_domain_separated() {
        let (mut a, mut b, mut c) = (Rng::new("s", b"x"), Rng::new("s", b"x"), Rng::new("s", b"y"));
        let (x, y, z) = (a.next(), b.next(), c.next());
        assert_eq!(x, y);
        assert_ne!(x, z);
    }
}

/// Whole-chain scenarios: Souls and vouches, claims and attestations, a full Séance.
#[cfg(test)]
mod scenarios {
    use super::*;
    use crate::chain::{now_ms, Chain, SummonOpts};
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
        Chain::summon(&home, &SummonOpts { contribution: Some(times()), ..SummonOpts::new("poc", 1) }).unwrap();
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let mut c = Chain::open(&home).unwrap();
        for i in 1..=12u8 {
            c.faucet(&k(i).address(), 100 * DROPS_PER_FANG).unwrap();
        }
        c.produce_block().unwrap();
        T { home, c, admin: Keys::from_seed(seed) }
    }

    impl T {
        fn tx_as(&mut self, key: &Keys, from: &str, action: Action) -> Result<String> {
            let mut tx = Tx { chain_id: "poc".into(), nonce: self.c.next_nonce(from), from: from.into(), fee: 0, tip: 0, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: hex::encode(key.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size());
            let id = self.c.submit(tx.sign(key))?;
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
                let mut tx = Tx { chain_id: "poc".into(), nonce: self.c.next_nonce(&me), from: me.clone(), fee: 0, tip: 0, valid_until: self.c.height() + 100, action: Action::AttestClaim { claim_id: *id, ok: true, merge_sha: "cd".repeat(20), note: String::new() }, memo: String::new(), public_key: hex::encode(self.admin.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
                tx.fee = min_fee(tx.size());
                self.c.submit(tx.sign(&self.admin)).unwrap();
            }
            self.c.produce_block().unwrap();
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
        let mut total: u128 = s.accounts.values().map(|a| a.balance as u128).sum();
        total += s.hunts.values().filter(|h| h.status == crate::chain::HuntStatus::Open).map(|h| h.amount as u128).sum::<u128>();
        total += s.validators.values().map(|v| v.bond as u128).sum::<u128>();
        let p = &s.poc;
        total += p.vouches.values().map(|v| v.bond as u128).sum::<u128>();
        total += p.jurors.values().map(|r| r.bond as u128).sum::<u128>();
        total += p.claims.values().map(|c| c.bond as u128).sum::<u128>();
        total += p.grants.values().flat_map(|g| g.values()).map(|g| (g.amount - g.withdrawn) as u128).sum::<u128>();
        total += p.seances.values().filter(|x| !x.closed).map(|x| x.budget as u128).sum::<u128>();
        assert_eq!(total, s.supply as u128, "FANG conserved");
    }

    #[test]
    fn souls_vouches_and_sybils() {
        let mut t = setup("souls");
        let (alice, bob, carol, dave, eve) = (k(1), k(2), k(3), k(4), k(5));
        // Admin-only levers (Season 0).
        assert!(t.tx(&alice, Action::GrantSoul { to: bob.address(), amount: SOUL, reason: "x".into() }).unwrap_err().to_string().contains("only validator admins"));
        assert!(t.tx(&alice, Action::SetRepo { repo: "a/b".into(), weight: 1 }).unwrap_err().to_string().contains("only validator admins"));
        assert!(t.admin(Action::GrantSoul { to: bob.address(), amount: MAX_SOUL_GRANT + 1, reason: "x".into() }).is_err());
        assert!(!t.c.state.poc.is_active(), "nothing has happened yet, so old roots are unchanged");
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
            t.tx(&bob, Action::AddKey { key_id: key_id(&key.public_key()), label: "agent".into(), role: KeyRole::Session { limit: 5 * DROPS_PER_FANG, period_blocks: 1000, expires_at: h + 1000 } }).unwrap();
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
        for _ in 0..10 {
            if t.c.state.poc.open_seance.is_some() {
                break;
            }
            t.blocks(1);
        }
        let sid = t.c.state.poc.open_seance.expect("a Séance opens once there are claims and jurors");
        let s = t.c.state.poc.seances[&sid].clone();
        assert_eq!(s.claims, vec![big, tiny, ok]);
        assert_eq!(s.jurors.len(), 5);
        assert_eq!(s.budget, 20 * DROPS_PER_FANG * 3 / FULL_BUDGET_CLAIMS, "a quarter of the pool, scaled for 3 claims");
        assert!(s.jurors.values().all(|seat| seat.pairs.len() == 3), "3 claims → every pair");
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
            t.tx(jk, Action::CommitVotes { seance_id: sid, commitment }).unwrap();
            sealed.push((i, votes, spam, salt));
        }
        assert!(t.tx(&solid, Action::CommitVotes { seance_id: sid, commitment: "00".repeat(32) }).unwrap_err().to_string().contains("not on the jury"));
        while t.h() <= s.commit_until {
            t.blocks(1);
        }
        assert!(t.tx(&jurors[2], Action::CommitVotes { seance_id: sid, commitment: "00".repeat(32) }).unwrap_err().to_string().contains("closed at block"));
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
        // Vouches are for newcomers only, and the vouchee can decline (bond back to the voucher).
        assert!(t.tx(&vet, Action::Vouch { account: j1.address() }).unwrap_err().to_string().contains("isn't a newcomer"));
        t.tx(&vet, Action::Vouch { account: newbie.address() }).unwrap();
        let v0 = t.bal(&vet);
        t.tx(&newbie, Action::Unvouch { account: newbie.address() }).unwrap();
        assert_eq!(t.bal(&vet), v0 + VOUCH_BOND);
        t.tx(&vet, Action::Vouch { account: newbie.address() }).unwrap();
        // A handle's work is credited to the first account it's verified for.
        t.link(&newbie, "newbie");
        let c1 = t.claim(&newbie, 1).unwrap();
        t.attest(c1, true).unwrap();
        assert_eq!(t.c.state.poc.handles["newbie"], newbie.address());
        t.tx(&newbie, Action::UnlinkGithub { handle: "newbie".into() }).unwrap();
        t.grant(&hopper, 1);
        t.link(&hopper, "Newbie");
        assert!(t.claim(&hopper, 2).unwrap_err().to_string().contains("credited to"));
        // Too few reveals: nothing is decided, claims carry over, the budget goes back.
        t.grant(&other, 1);
        t.link(&other, "other");
        for j in [&j1, &j2, &j3] {
            t.tx(j, Action::JoinJury { bond: JUROR_BOND }).unwrap();
        }
        t.tx(&vet, Action::FundPool { amount: 50 * DROPS_PER_FANG }).unwrap();
        let c2 = t.claim(&other, 3).unwrap();
        t.attest(c2, true).unwrap();
        let sid = t.c.state.poc.open_seance.expect("2 claims + 3 jurors open a Séance");
        let s = t.c.state.poc.seances[&sid].clone();
        let mut sealed = vec![];
        for jk in [&j1, &j2] {
            let votes: String = s.jurors[&jk.address()].pairs.iter().map(|_| 'a').collect();
            t.tx(jk, Action::CommitVotes { seance_id: sid, commitment: seance_commitment(sid, &jk.address(), &votes, &[], "x") }).unwrap();
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

    fn claims_payout(t: &T, c: u64) -> u64 {
        let cl = &t.c.state.poc.claims[&c];
        t.c.state.poc.grants[&cl.claimant][&cl.grant.unwrap()].amount
    }
}
