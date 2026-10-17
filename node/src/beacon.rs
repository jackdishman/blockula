//! Randomness beacon and revert attestations (P-BEACON, MAINNET-DESIGN 3.8: CRY-3, SF-1, GOV-3).
//!
//! **The beacon** is a commit-reveal among the active validators, one round per epoch:
//!
//! * During epoch `e`, every validator active in `e` commits to a fresh 32-byte secret for epoch
//!   `e + 2`: `BeaconCommit { epoch: e + 2, commitment }`, where `commitment` is [`commitment`]
//!   (BLAKE3 over the chain id, the target epoch, the validator and the secret).
//! * During `e + 1` it reveals the secret (`BeaconReveal`).
//! * At the end of `e + 1` ([`end_epoch`]), `value(e + 2)` is [`derive`]: BLAKE3 over
//!   `value(e + 1)`, the epoch and the revealed `(validator, secret)` pairs sorted by validator.
//!   With no reveals that is BLAKE3 over the previous value and the epoch alone: a hash chain.
//!   Validators that committed and didn't reveal are jailed and slashed (`staking::jail`,
//!   [`miss_slash_bps`]: `beacon_miss_slash_bps`, times one more for each miss already on record
//!   in the kept values). Epoch 0's value comes from genesis ([`init_genesis`]); epoch 1 has no
//!   round, so its value is the hash chain's next link.
//!
//! What this guarantees: nobody can predict `value(e + 2)` before the first honest validator
//! reveals (during `e + 1`), and where or in what order reveals land changes nothing (they are
//! sorted). What it doesn't: a committer that sees everyone else's reveals may withhold its own,
//! choosing between two values. That is one bit of influence per withholding validator (k
//! colluding withholders choose among at most 2^k values), bought each time with:
//!
//! * the slash, escalating with repeat misses;
//! * the epoch it's caught in (`e + 1`): [`end_epoch`] runs first at the boundary, before
//!   issuance and set selection (chain.rs `end_block`), so a jailed pool earns nothing for it;
//! * the epoch it biased (`e + 2`): the set for it is selected after the jail, so the withholder
//!   isn't in it (`Unjail` comes after `jail_blocks`, and rejoining is churn-limited);
//! * and a jailed validator can't commit again (no fresh bits while it's on its way out).
//!
//! Its value is choosing between two leader orders for `e + 2` and, when a Séance opens at the
//! end of `e + 2`, between two jury draws: at most that Séance's budget is at stake (a quarter of
//! the contribution pool). PLAN.md "Known limits" weighs the two. If nobody takes part, the
//! beacon is the predictable hash chain.
//!
//! **Uses.** Séances open on fixed epoch boundaries and draw their jury and pairs from that
//! epoch's value ([`seance_seed`], poc.rs). Each epoch's leader order is a permutation seeded
//! from its value ([`elector_seed`], consensus.rs `elector`), so the schedule is known only once
//! the previous epoch's reveals are in.
//!
//! **Reverts (GOV-3).** While a verified claim's revert window is open (the
//! `claim_maturity_blocks` after its verification, [`revert_window_open`]), validators re-check
//! its PR on GitHub (github.rs) and attest a revert (`AttestRevert`). A BFT quorum of the current
//! set naming the same revert commit rejects the claim through `State::exec_reject_claim` (the
//! bond is slashed into the pool, any reward and Soul clawed back).
//!
//! **Validator side.** [`Secrets`] keeps a validator's secrets under its home, written (0600,
//! fsynced) before a commitment leaves the node; [`duties`] commits early in each epoch and
//! reveals in its second half (node.rs runs it every block).

use crate::chain::{short, Chain, Journal, KeyRecord, State};
use crate::poc::{Claim, ClaimStatus};
use crate::state::{ns, JMap, SubState, TMap, Table};
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Context, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Epochs whose values the state keeps: the current epoch's (leader order, Séances, downtime
/// accounting) and a few days of history at hour-long epochs.
pub const VALUES_KEPT: u64 = 64;

/// One validator's part in an epoch's round.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Entry {
    pub commitment: [u8; 32],
    pub committed_at: u64,
    /// The secret and when it was revealed (both `None` until then).
    pub secret: Option<[u8; 32]>,
    pub revealed_at: Option<u64>,
}

/// An epoch's beacon.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Value {
    pub value: [u8; 32],
    /// Reveals that went into it (0: the hash-chain fallback).
    pub reveals: u32,
    /// Validators that committed and didn't reveal (jailed for it), sorted.
    pub missed: Vec<String>,
}

/// A validator's attestation that a claim's PR was reverted.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RevertAttestation {
    /// The 40-hex commit (or revert PR merge) that undid it.
    pub revert_sha: String,
    pub at: u64,
}

/// The beacon's slice of the state (namespaces `state::ns` 44–46).
#[derive(Clone, Debug, Default)]
pub struct Beacon {
    /// Target epoch → validator → its commitment (and secret, once revealed). A round is dropped
    /// when its value is derived.
    pub rounds: TMap<u64, BTreeMap<String, Entry>>,
    /// Claim id → validator → its revert attestation, while the claim's revert window is open.
    pub reverts: TMap<u64, BTreeMap<String, RevertAttestation>>,
    /// Epoch → its beacon, for the last [`VALUES_KEPT`] epochs.
    pub values: TMap<u64, Value>,
}

impl SubState for Beacon {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Beacon { rounds, reverts, values } = self;
        vec![(ns::BEACON_COMMITS, rounds), (ns::BEACON_REVERTS, reverts), (ns::BEACON_VALUES, values)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Beacon { rounds, reverts, values } = self;
        vec![(ns::BEACON_COMMITS, rounds), (ns::BEACON_REVERTS, reverts), (ns::BEACON_VALUES, values)]
    }
}

/// Undo log for [`Beacon`] (part of `chain::Journal`).
#[derive(Default)]
pub struct BeaconJournal {
    pub rounds: JMap<u64, BTreeMap<String, Entry>>,
    pub reverts: JMap<u64, BTreeMap<String, RevertAttestation>>,
    pub values: JMap<u64, Value>,
}

impl BeaconJournal {
    pub fn absorb(&mut self, other: BeaconJournal) {
        let BeaconJournal { rounds, reverts, values } = other;
        self.rounds.absorb(rounds);
        self.reverts.absorb(reverts);
        self.values.absorb(values);
    }

    pub fn rollback(self, beacon: &mut Beacon) {
        let BeaconJournal { rounds, reverts, values } = self;
        rounds.restore(&mut beacon.rounds);
        reverts.restore(&mut beacon.reverts);
        values.restore(&mut beacon.values);
    }
}

// ------------------------------------------------------------------------------------ hashes

fn put(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

/// What a validator commits to for `epoch`: BLAKE3 over the chain id, the epoch, the validator
/// and its secret (so a commitment can't be replayed on another chain, epoch or validator).
pub fn commitment(chain_id: &str, epoch: u64, validator: &str, secret: &[u8; 32]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("blockula beacon commitment v1");
    put(&mut h, chain_id);
    h.update(&epoch.to_le_bytes());
    put(&mut h, validator);
    h.update(secret);
    *h.finalize().as_bytes()
}

/// `value(epoch)` from the previous epoch's value and the round's reveals, which must be sorted
/// by validator (a round's map already is). With no reveals: the previous value and the epoch.
pub fn derive(prev: &[u8; 32], epoch: u64, reveals: &[(String, [u8; 32])]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("blockula beacon v1");
    h.update(prev);
    h.update(&epoch.to_le_bytes());
    h.update(&(reveals.len() as u64).to_le_bytes());
    for (v, secret) in reveals {
        put(&mut h, v);
        h.update(secret);
    }
    *h.finalize().as_bytes()
}

/// Epoch 0's value: public at genesis, like everything derived from it before the first reveals.
pub fn genesis_value(chain_id: &str, created_at_ms: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("blockula beacon genesis v1");
    put(&mut h, chain_id);
    h.update(&created_at_ms.to_le_bytes());
    *h.finalize().as_bytes()
}

/// Called by `Genesis::initial_state`: epoch 0's value.
pub fn init_genesis(state: &mut State, chain_id: &str, created_at_ms: u64) {
    state.beacon.values.insert(0, Value { value: genesis_value(chain_id, created_at_ms), reveals: 0, missed: vec![] });
}

/// The beacon for `epoch`, once derived (and while kept).
pub fn value(state: &State, epoch: u64) -> Option<[u8; 32]> {
    state.beacon.values.get(&epoch).map(|v| v.value)
}

/// Seed of `epoch`'s leader permutation (SF-1): consensus shuffles simplex's round robin with it
/// (`consensus::elector`), and downtime accounting must elect leaders the same way
/// (`consensus::leader`). `None` (an epoch without a value): the unshuffled round robin.
pub fn elector_seed(state: &State, epoch: u64) -> Option<[u8; 32]> {
    value(state, epoch).map(|v| blake3::derive_key("blockula leader order v1", &v))
}

/// Seed of Séance `seance_id`, opening in `epoch` (its jury and pairs are drawn from it).
pub fn seance_seed(state: &State, epoch: u64, seance_id: u64) -> Option<[u8; 32]> {
    let v = value(state, epoch)?;
    let mut h = blake3::Hasher::new_derive_key("blockula seance seed v1");
    h.update(&v);
    h.update(&seance_id.to_le_bytes());
    Some(*h.finalize().as_bytes())
}

fn hex32(field: &str, s: &str) -> Result<[u8; 32]> {
    hex::decode(s).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| anyhow!("{field} must be 32 bytes of hex"))
}

// --------------------------------------------------------------------------------------- txs

/// Whether validators may still attest that `c` was reverted at `height`: it's verified and at
/// most `claim_maturity_blocks` past its verification (Séances only take claims whose window is
/// over). A PR that survives the window is paid for even if it's reverted later.
pub fn revert_window_open(state: &State, c: &Claim, height: u64) -> bool {
    c.status == ClaimStatus::Verified && c.verified_at.is_some_and(|v| height <= v.saturating_add(state.params.poc.claim_maturity_blocks))
}

/// Tags 37, 38 and 49: BeaconCommit, BeaconReveal, AttestRevert. Signed by a validator's
/// consensus key or by its owner (`staking::validator_for_signer`).
pub(crate) fn apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, _key: &KeyRecord, height: u64) -> Result<()> {
    let epoch = state.params.epoch_of(height);
    match &tx.action {
        Action::BeaconCommit { epoch: target, commitment } => {
            if *target != epoch + 2 {
                bail!("during epoch {epoch} validators commit to the beacon for epoch {}", epoch + 2);
            }
            let v = crate::staking::validator_for_signer(state, sender, signer, epoch).ok_or_else(|| anyhow!("only validators active in epoch {epoch} commit to the beacon"))?;
            // A jailed validator leaves the set at the next boundary: no new bits for it.
            if jailed(state, &v) {
                bail!("validator {} is jailed: it leaves the set at the end of epoch {epoch} and doesn't commit to the beacon", short(&v));
            }
            let c = hex32("commitment", commitment)?;
            if state.beacon.rounds.get(target).is_some_and(|r| r.contains_key(&v)) {
                bail!("validator {} already committed to the beacon for epoch {target}", short(&v));
            }
            j.beacon.rounds.touch(&state.beacon.rounds, target);
            state.beacon.rounds.entry(*target).or_default().insert(v, Entry { commitment: c, committed_at: height, secret: None, revealed_at: None });
        }
        Action::BeaconReveal { epoch: target, secret } => {
            if *target != epoch + 1 {
                bail!("during epoch {epoch} validators reveal their beacon secrets for epoch {}", epoch + 1);
            }
            // It committed during the previous epoch, so it must have been active then (it may
            // have left the set since; it still owes the reveal).
            let Some(prev) = epoch.checked_sub(1) else { bail!("epoch 1 has no round: its beacon follows from genesis") };
            let v = crate::staking::validator_for_signer(state, sender, signer, prev).ok_or_else(|| anyhow!("only validators that committed during epoch {prev} reveal for epoch {target}"))?;
            let entry = state.beacon.rounds.get(target).and_then(|r| r.get(&v)).ok_or_else(|| anyhow!("validator {} didn't commit to the beacon for epoch {target}", short(&v)))?;
            if entry.secret.is_some() {
                bail!("validator {} already revealed its secret for epoch {target}", short(&v));
            }
            let s = hex32("secret", secret)?;
            if commitment(&tx.chain_id, *target, &v, &s) != entry.commitment {
                bail!("that secret doesn't match validator {}'s commitment for epoch {target}", short(&v));
            }
            j.beacon.rounds.touch(&state.beacon.rounds, target);
            let e = state.beacon.rounds.get_mut(target).and_then(|r| r.get_mut(&v)).expect("checked above");
            e.secret = Some(s);
            e.revealed_at = Some(height);
        }
        Action::AttestRevert { claim_id, revert_sha } => attest_revert(state, j, sender, signer, *claim_id, revert_sha, height)?,
        _ => bail!("not a beacon action"),
    }
    Ok(())
}

/// Records a validator's revert attestation; a BFT quorum of the current set on one revert
/// commit rejects the claim.
fn attest_revert(state: &mut State, j: &mut Journal, sender: &str, signer: &str, claim_id: u64, revert_sha: &str, height: u64) -> Result<()> {
    let epoch = state.params.epoch_of(height);
    let v = crate::staking::validator_for_signer(state, sender, signer, epoch).ok_or_else(|| anyhow!("only active validators attest reverts"))?;
    let c = state.poc.claims.get(&claim_id).ok_or_else(|| anyhow!("no claim #{claim_id}"))?;
    if !revert_window_open(state, c, height) {
        bail!("claim #{claim_id} isn't in its revert window (verified claims, until {} blocks after verification)", state.params.poc.claim_maturity_blocks);
    }
    if c.merge_sha.as_deref() == Some(revert_sha) {
        bail!("{revert_sha} is the claim's own merge commit, not a revert");
    }
    if state.beacon.reverts.get(&claim_id).is_some_and(|r| r.contains_key(&v)) {
        bail!("validator {} already attested a revert of claim #{claim_id}", short(&v));
    }
    j.beacon.reverts.touch(&state.beacon.reverts, &claim_id);
    state.beacon.reverts.entry(claim_id).or_default().insert(v, RevertAttestation { revert_sha: revert_sha.to_string(), at: height });
    // Distinct members of the current set naming this revert (attestations from validators that
    // left since don't count).
    let set = state.active_sets.get(&epoch).cloned().unwrap_or_default();
    let n = set.len();
    ensure!(n > 0, "no active set for epoch {epoch}");
    let quorum = n - (n - 1) / 3;
    let agree = state.beacon.reverts[&claim_id].iter().filter(|(val, a)| a.revert_sha == revert_sha && set.contains(*val)).count();
    if agree >= quorum {
        let reason = format!("reverted by {} ({agree} of {n} validators attested)", revert_sha.get(..10).unwrap_or(revert_sha));
        state.exec_reject_claim(j, claim_id, &reason, height)?;
        state.beacon.reverts.remove(&claim_id);
    }
    Ok(())
}

fn jailed(state: &State, validator: &str) -> bool {
    state.staking.pools.get(validator).is_some_and(|p| p.jail.is_some())
}

// ----------------------------------------------------------------------------- end of epoch

/// What a missed reveal slashes: `beacon_miss_slash_bps` for a first miss, times one more for
/// each miss of `validator`'s already on record in the kept values (the last [`VALUES_KEPT`]
/// epochs), at most the parameter's ceiling (10%). Honest validators miss only through downtime
/// spanning half an epoch, which jails them anyway; a validator that withholds again and again
/// pays more each time.
pub fn miss_slash_bps(state: &State, validator: &str) -> u64 {
    let base = state.chain_params.beacon_miss_slash_bps;
    let prior = state.beacon.values.values().filter(|v| v.missed.binary_search_by(|m| m.as_str().cmp(validator)).is_ok()).count() as u64;
    let ceiling = crate::params::ChainParams::bounds("beacon_miss_slash_bps", &state.params).map_or(base, |(_, max)| max);
    base.saturating_mul(1 + prior).min(ceiling.max(base))
}

/// At the end of `epoch`, first of the epoch-boundary steps (before issuance and
/// `staking::end_epoch`, so the jail costs the withholder this epoch's rewards and its seat in
/// the next set): derive `value(epoch + 1)` from its round, jail the validators that committed
/// and didn't reveal, and drop revert attestations whose claims left their window. End-of-block
/// steps aren't rolled back, so the journal only feeds the helpers.
pub fn end_epoch(state: &mut State, epoch: u64) {
    let mut j = Journal::default();
    let target = epoch + 1;
    let height = state.params.last_of(epoch);
    let round = state.beacon.rounds.get(&target).cloned().unwrap_or_default();
    let reveals: Vec<(String, [u8; 32])> = round.iter().filter_map(|(v, e)| e.secret.map(|s| (v.clone(), s))).collect();
    let missed: Vec<String> = round.iter().filter(|(_, e)| e.secret.is_none()).map(|(v, _)| v.clone()).collect();
    let prev = value(state, epoch).unwrap_or_default();
    // Escalation counts earlier misses only (before this epoch's value is filed).
    let slashes: Vec<u64> = missed.iter().map(|v| miss_slash_bps(state, v)).collect();
    let v = Value { value: derive(&prev, target, &reveals), reveals: reveals.len() as u32, missed: missed.clone() };
    state.beacon.values.insert(target, v);
    let done: Vec<u64> = state.beacon.rounds.keys().take_while(|t| **t <= target).copied().collect();
    for t in done {
        state.beacon.rounds.remove(&t);
    }
    let old: Vec<u64> = state.beacon.values.keys().take_while(|e| **e + VALUES_KEPT <= target).copied().collect();
    for e in old {
        state.beacon.values.remove(&e);
    }
    for (v, bps) in missed.iter().zip(slashes) {
        crate::staking::jail(state, &mut j, v, height, bps, &format!("committed to the randomness beacon for epoch {target} and didn't reveal"));
    }
    let next = height + 1;
    let closed: Vec<u64> = state.beacon.reverts.keys().filter(|id| !state.poc.claims.get(*id).is_some_and(|c| revert_window_open(state, c, next))).copied().collect();
    for id in closed {
        state.beacon.reverts.remove(&id);
    }
}

/// The beacon's consistency rules (part of `State::check_invariants`).
pub fn check_invariants(state: &State) -> Result<()> {
    let b = &state.beacon;
    let last = b.values.keys().next_back().copied();
    if let (Some(first), Some(last)) = (b.values.keys().next(), last) {
        ensure!(last - first + 1 == b.values.len() as u64, "beacon values have a gap between epochs {first} and {last}");
        ensure!(b.values.len() as u64 <= VALUES_KEPT, "the state keeps {} beacon values", b.values.len());
    }
    for (e, v) in b.values.iter() {
        ensure!(v.missed.windows(2).all(|w| w[0] < w[1]), "epoch {e}'s missed list isn't sorted");
    }
    for (t, round) in b.rounds.iter() {
        ensure!(!round.is_empty(), "the beacon round for epoch {t} is empty");
        ensure!(last.is_none_or(|l| *t > l), "the beacon for epoch {t} is derived but its round is still open");
        for (v, e) in round {
            ensure!(e.secret.is_some() == e.revealed_at.is_some(), "{v}'s reveal for epoch {t} is half-recorded");
            ensure!(state.validators.contains_key(v), "unknown validator {v} committed for epoch {t}");
        }
    }
    for (id, r) in b.reverts.iter() {
        ensure!(!r.is_empty(), "empty revert attestations for claim #{id}");
        ensure!(state.poc.claims.contains_key(id), "revert attestations for unknown claim #{id}");
        for a in r.values() {
            ensure!(a.revert_sha.len() == 40 && crate::crypto::is_lower_hex(&a.revert_sha), "bad revert sha on claim #{id}");
        }
    }
    Ok(())
}

/// Plain-English summary of a beacon tx (see `State::describe`).
pub fn describe(state: &State, tx: &Tx, from: &str) -> String {
    match &tx.action {
        Action::BeaconCommit { epoch, .. } => format!("{from} committed to the randomness beacon for epoch {epoch}"),
        Action::BeaconReveal { epoch, .. } => format!("{from} revealed their beacon secret for epoch {epoch}"),
        Action::AttestRevert { claim_id, revert_sha } => {
            let claim = state.poc.claims.get(claim_id).map(|c| format!("claim #{claim_id} ({}#{})", c.repo, c.pr)).unwrap_or_else(|| format!("claim #{claim_id}"));
            format!("{from} attested that {claim} was reverted (commit {})", revert_sha.chars().take(10).collect::<String>())
        }
        _ => String::new(),
    }
}

// --------------------------------------------------------------------------- validator side

/// File under a validator's home holding its beacon secrets by target epoch.
pub const SECRETS_FILE: &str = "beacon-secrets.json";

/// A validator's beacon secrets, by target epoch. Each is written to disk (0600, fsynced, an
/// atomic replace) before its commitment is submitted, so a crash or restart can't leave a
/// commitment the node can no longer open.
pub struct Secrets {
    path: PathBuf,
    by_epoch: BTreeMap<u64, [u8; 32]>,
    /// Tests and the simulator derive secrets from this seed instead of the OS RNG, so runs
    /// replay exactly.
    deterministic: Option<[u8; 32]>,
}

impl Secrets {
    /// Loads `home/beacon-secrets.json` (missing: none yet).
    pub fn open(home: &Path) -> Result<Self> {
        let path = home.join(SECRETS_FILE);
        let by_epoch = match std::fs::read(&path) {
            Ok(bytes) => {
                let m: BTreeMap<u64, String> = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
                m.into_iter().map(|(e, s)| Ok((e, hex32("secret", &s)?))).collect::<Result<_>>()?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Secrets { path, by_epoch, deterministic: None })
    }

    #[cfg(test)]
    pub fn open_deterministic(home: &Path, seed: [u8; 32]) -> Result<Self> {
        Ok(Secrets { deterministic: Some(seed), ..Self::open(home)? })
    }

    pub fn get(&self, epoch: u64) -> Option<[u8; 32]> {
        self.by_epoch.get(&epoch).copied()
    }

    /// The secret for `epoch`, created and persisted if there isn't one yet.
    pub fn get_or_create(&mut self, epoch: u64) -> Result<[u8; 32]> {
        if let Some(s) = self.get(epoch) {
            return Ok(s);
        }
        let mut s = [0u8; 32];
        match self.deterministic {
            Some(seed) => s = blake3::keyed_hash(&seed, &epoch.to_le_bytes()).into(),
            None => getrandom::fill(&mut s).map_err(|e| anyhow!("no randomness for a beacon secret: {e}"))?,
        }
        self.by_epoch.insert(epoch, s);
        if let Err(e) = self.save() {
            self.by_epoch.remove(&epoch);
            return Err(e);
        }
        Ok(s)
    }

    /// Forgets secrets for epochs before `epoch` (their rounds are over).
    pub fn prune(&mut self, epoch: u64) -> Result<()> {
        let before = self.by_epoch.len();
        self.by_epoch.retain(|e, _| *e >= epoch);
        if self.by_epoch.len() != before { self.save() } else { Ok(()) }
    }

    fn save(&self) -> Result<()> {
        use std::io::Write;
        let m: BTreeMap<u64, String> = self.by_epoch.iter().map(|(e, s)| (*e, hex::encode(s))).collect();
        let tmp = self.path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(&serde_json::to_vec_pretty(&m)?)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("writing {}", self.path.display()))?;
        if let Some(dir) = self.path.parent().and_then(|d| std::fs::File::open(d).ok()) {
            let _ = dir.sync_all();
        }
        Ok(())
    }
}

/// One round of a validator's beacon duties against its chain (node.rs runs it every block):
/// while active, commit to a fresh secret for the epoch after next; in the second half of the
/// epoch, reveal the secret committed to for the next one (the next epoch's value, and so its
/// leader order, stays unknown until the last reveal, and half an epoch is plenty to land one).
/// A candidate that a set of one or two admitted also shows up (`staking::should_show_up`).
/// Each tx is submitted once (on-chain records and the mempool are checked first). Returns what
/// it did; a reveal it can't make (its secret is gone) is an error.
pub fn duties(chain: &mut Chain, secrets: &mut Secrets) -> Result<Vec<String>> {
    let Some(keys) = chain.keys.as_ref() else { return Ok(vec![]) };
    let me = keys.address_with(chain.state.params.prefix());
    let signer = key_id(&keys.public_key());
    let chain_id = chain.genesis.chain_id.clone();
    // Our txs land in the next block at the earliest.
    let next = chain.height() + 1;
    let epoch = chain.state.params.epoch_of(next);
    let epoch_blocks = chain.state.params.epoch_blocks.max(2);
    // Signed by this consensus key, whoever pays (SF-13: `Chain::submit_system`).
    let pk = hex::encode(keys.public_key());
    let queued = |chain: &Chain, f: &dyn Fn(&Action) -> bool| chain.mempool.iter().any(|t| t.public_key == pk && f(&t.action));
    let mut notes = vec![];
    let mut error = None;
    let reveal = epoch + 1;
    let due = next % epoch_blocks >= epoch_blocks / 2;
    if let Some(e) = chain.state.beacon.rounds.get(&reveal).and_then(|r| r.get(&me)).filter(|e| due && e.secret.is_none()) {
        let committed_to = e.commitment;
        if !queued(chain, &|a| matches!(a, Action::BeaconReveal { epoch, .. } if *epoch == reveal)) {
            match secrets.get(reveal) {
                Some(s) if commitment(&chain_id, reveal, &me, &s) == committed_to => {
                    chain.submit_system(Action::BeaconReveal { epoch: reveal, secret: hex::encode(s) }, &format!("🎲 beacon reveal for epoch {reveal}"))?;
                    notes.push(format!("revealed the beacon secret for epoch {reveal}"));
                }
                _ => error = Some(anyhow!("no secret in {} opens this validator's commitment for epoch {reveal}: it can't reveal and will be jailed", SECRETS_FILE)),
            }
        }
    }
    let commit = epoch + 2;
    let active = crate::staking::validator_for_signer(&chain.state, &me, &signer, epoch).as_deref() == Some(me.as_str()) && !jailed(&chain.state, &me);
    let committed = chain.state.beacon.rounds.get(&commit).is_some_and(|r| r.contains_key(&me));
    if active && !committed && !queued(chain, &|a| matches!(a, Action::BeaconCommit { epoch, .. } if *epoch == commit)) {
        // On disk before the commitment leaves this node.
        let s = secrets.get_or_create(commit)?;
        chain.submit_system(Action::BeaconCommit { epoch: commit, commitment: hex::encode(commitment(&chain_id, commit, &me, &s)) }, &format!("🎲 beacon commitment for epoch {commit}"))?;
        notes.push(format!("committed to the beacon for epoch {commit}"));
    }
    secrets.prune(reveal)?;
    // A candidate a set of 2 or 3 would need for every quorum joins only once a quorum of the
    // members it joins admitted it (their operators' call, never a node's) and it showed up in the
    // epoch's last tenth (`staking::showed_up`, audit review-fix-staking-2): `ValidatorReady`,
    // signed by this key, says this node is running then. It changes nothing but that record,
    // and it's a lane duty (`staking::duty_of`), so it never holds the duties behind it back.
    if crate::staking::should_show_up(&chain.state, &me, next) && !queued(chain, &|a| matches!(a, Action::ValidatorReady { epoch: e } if *e == epoch)) {
        match chain.submit_system(Action::ValidatorReady { epoch }, "🧛 ready to join the validator set") {
            Ok(_) => notes.push(format!("showed up to join the validator set (epoch {epoch})")),
            Err(e) => error = error.or(Some(e.context("couldn't show up to join the validator set"))),
        }
    }
    match error {
        Some(e) => Err(e),
        None => Ok(notes),
    }
}

#[cfg(test)]
mod tests;
