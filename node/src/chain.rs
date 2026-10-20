//! Chain state and execution. Consensus (`consensus.rs`) orders blocks; this module executes
//! finalized blocks deterministically, in order, persists each one (block logs, then the state
//! database: see `store.rs`), publishes what readers see (`view.rs`) and keeps the mempool.

use crate::crypto::{self, Keys};
use crate::params::ChainParams;
use crate::state::codec::SCHEMA_VERSION;
use crate::state::tree::{Child, Hash, Node, NodeKey, NodeStore, Smt};
use crate::state::{JMap, RecordChange, TMap};
use crate::store::{self, hash32, BlockMeta, CommitPoint, Pos, SegLog, Snap, Store};
use crate::types::*;
use crate::view::{Disk, Live, MempoolView, ReadView};
use anyhow::{anyhow, bail, ensure, Context, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Mainnet's chain id. Its genesis may not have admins or a faucet (DEC-10).
pub const MAINNET_CHAIN_ID: &str = "blockula-1";

/// The treasury's system account on a chain whose addresses use `prefix`. Keyless: only
/// `TreasurySpend` proposals move funds out.
pub fn treasury(prefix: &str) -> String {
    crypto::system_address(prefix, "treasury")
}

/// The keyless community account: `Distribution` proposals pay out of it (DEC-5, 3.5).
pub fn community(prefix: &str) -> String {
    crypto::system_address(prefix, "community")
}

/// The keyless contribution endowment, which streams into the contribution pool (DEC-5).
pub fn endowment(prefix: &str) -> String {
    crypto::system_address(prefix, "endowment")
}

/// The keyless validator bootstrap delegation (DEC-5): delegated by formula at every epoch
/// boundary, returned to the treasury on its vesting curve (years 2–4 on mainnet), its rewards
/// to the treasury (`staking::bootstrap_epoch`). (The foundation is an ordinary multisig account.)
pub fn bootstrap_delegation(prefix: &str) -> String {
    crypto::system_address(prefix, "bootstrap-delegation")
}

/// What a keyless system account is (`"treasury"`, `"community"`, `"endowment"`,
/// `"bootstrap delegation"`, `"contribution pool"`), if `addr` is one on a chain with `prefix`.
/// Nobody holds a key for them: only the protocol moves their funds.
pub fn system_account(prefix: &str, addr: &str) -> Option<&'static str> {
    let accounts = [("treasury", treasury(prefix)), ("community", community(prefix)), ("endowment", endowment(prefix)), ("bootstrap delegation", bootstrap_delegation(prefix)), ("contribution pool", crate::poc::pool_address(prefix))];
    accounts.into_iter().find(|(_, a)| a == addr).map(|(role, _)| role)
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub address: String,
    #[serde(with = "u64_str")]
    pub amount: u64,
    /// Locks the amount on a vesting curve (a lock on the account, MAINNET-DESIGN 3.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vesting: Option<Vesting>,
    /// What the allocation is ("founder", "treasury"…): display, and the vesting lock's reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A vesting schedule, in block heights: nothing vests before `cliff`, then the amount vests
/// linearly from `start` to `end` (so the cliff releases what accrued since `start`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vesting {
    pub start: u64,
    pub cliff: u64,
    pub end: u64,
}

/// A genesis validator's economic side (MAINNET-DESIGN 3.7): who owns it, its self-bond (moved
/// from the owner's allocations into the validator's bond) and commission. Its consensus key
/// must be one of `validators`, with the same addresses. Staking builds its pool from these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operator {
    pub owner: String,
    /// hex ML-DSA-65 public key.
    pub consensus_key: String,
    /// Proof of possession, as for `RegisterValidator`: the consensus key's signature over
    /// `pop_message(chain_id, owner, p2p_addr, http_addr)`.
    pub pop: String,
    pub p2p_addr: String,
    pub http_addr: String,
    #[serde(with = "u64_str")]
    pub self_bond: u64,
    pub commission_bps: u64,
    // Display only (hosting diversity, DEC-14): not state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asn: Option<u32>,
    /// ISO 3166-1 alpha-2 (`"DE"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
}

/// Soul bootstrapped at genesis (3.5: the published ledger; afterwards only juries and bounded
/// `GrantSoul` proposals change Soul).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoulEntry {
    pub account: String,
    /// milli-Soul (`poc::SOUL` per Soul).
    pub soul: u64,
    pub reason: String,
}

/// Most Soul one genesis ledger entry may grant.
pub const MAX_GENESIS_SOUL: u64 = 1_000 * crate::poc::SOUL;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatorInfo {
    /// `boo1…` account of the validator (same seed as its consensus key).
    pub address: String,
    /// hex ML-DSA-65 public key: consensus votes and p2p identity.
    pub public_key: String,
    /// host:port for authenticated p2p.
    pub p2p_addr: String,
    /// host:port of the validator's HTTP API (used by `join` and the faucet forwarder).
    pub http_addr: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Genesis {
    pub chain_id: String,
    pub token: String,
    pub decimals: u32,
    pub block_time_ms: u64,
    pub validators: Vec<ValidatorInfo>,
    pub faucet: bool,
    pub created_at_ms: u64,
    pub allocations: Vec<Allocation>,
    /// Blocks per epoch. The validator set can only change at epoch boundaries.
    #[serde(default = "default_epoch_blocks")]
    pub epoch_blocks: u64,
    /// Season 0 allowlist: accounts that may approve/remove validators and pull the admin
    /// levers. Dev and test chains only: mainnet (`blockula-1`) refuses any.
    #[serde(default)]
    pub validator_admins: Vec<String>,
    /// Proof of Contribution timings. Absent: derived from `block_time_ms` (weeks, not minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contribution: Option<crate::poc::PocTimes>,
    /// bech32m prefix of this chain's addresses: `boo` (mainnet, dev) or `tboo` (public testnets).
    #[serde(default = "default_address_prefix")]
    pub address_prefix: String,
    /// Overrides of the governance-adjustable parameters (`params::ChainParams`), by key; the
    /// rest take their defaults. JSON values are strings or numbers.
    #[serde(rename = "params", default, skip_serializing_if = "BTreeMap::is_empty", with = "crate::params::u64_map")]
    pub param_overrides: BTreeMap<String, u64>,
    /// Owners, self-bonds and commissions of genesis validators.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operators: Vec<Operator>,
    /// Names nobody can register (bare, as `normalize_name` returns them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reserved_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub soul_ledger: Vec<SoulEntry>,
    /// Features on from genesis (`upgrade::KNOWN_FEATURES`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// Protocol versions scheduled from genesis, as `ScheduleUpgrade` proposals would schedule
    /// them: dev and test chains only (`upgrade::check_genesis_upgrades`), and a chain that has
    /// any runs `upgrade::PREVIEW_PROTOCOL_VERSION` (`upgrade::supported_version`). Absent on
    /// every chain that starts at v1, so their genesis and its hash don't change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upgrades: Vec<crate::upgrade::ScheduledUpgrade>,
    /// Accounts whose validators the bootstrap delegation passes over (DEC-5: the founder's and
    /// the foundation's, now and later), sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_excluded: Vec<String>,
    /// Repos in the contribution pool from genesis (lowercase `owner/name` → weight, 1 to
    /// `poc::MAX_REPO_WEIGHT`), as a `SetRepo` proposal would put them there. Its balance is an
    /// ordinary allocation to the pool's account (`poc::pool_address`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pool_repos: BTreeMap<String, u64>,
}

fn default_epoch_blocks() -> u64 {
    600
}
fn default_address_prefix() -> String {
    crypto::ADDRESS_HRP.into()
}

/// Fewest past epochs' validator sets the state keeps (for verifying recent certificates).
pub const MIN_EPOCH_SETS_KEPT: u64 = 8;

/// Validator sets the state keeps: the evidence window's (equivocation evidence is checked
/// against its epoch's set, `evidence_max_age_epochs` back) plus the next epoch's, and at least
/// [`MIN_EPOCH_SETS_KEPT`].
pub fn epoch_sets_kept(c: &ChainParams) -> u64 {
    c.evidence_max_age_epochs.saturating_add(2).max(MIN_EPOCH_SETS_KEPT)
}

/// Genesis constants the state machine needs while applying txs (a state record, so the root
/// commits to them). What governance can change is in `State::chain_params` instead.
#[derive(Clone, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct Params {
    pub block_time_ms: u64,
    pub epoch_blocks: u64,
    pub admins: Vec<String>,
    pub poc: crate::poc::PocTimes,
    /// The chain's address prefix (see [`Params::prefix`]).
    pub address_prefix: String,
    /// Owners whose validators the bootstrap delegation passes over (`Genesis::bootstrap_excluded`).
    pub bootstrap_excluded: Vec<String>,
}

impl Params {
    /// The chain's address prefix (`boo` when unset, e.g. in a default `Params`).
    pub fn prefix(&self) -> &str {
        if self.address_prefix.is_empty() { crypto::ADDRESS_HRP } else { &self.address_prefix }
    }

    /// Longest a session key may stay valid: ~400 days of blocks.
    pub fn max_session_blocks(&self) -> u64 {
        400 * 86_400 * 1000 / self.block_time_ms.max(1)
    }

    pub fn epoch_of(&self, height: u64) -> u64 {
        height / self.epoch_blocks.max(2)
    }
    pub fn is_boundary(&self, height: u64) -> bool {
        let e = self.epoch_blocks.max(2);
        height % e == e - 1
    }
    /// The epoch's last height (saturating: an epoch past the last whole one ends at u64::MAX,
    /// so a caller's height near u64::MAX can't overflow it).
    pub fn last_of(&self, epoch: u64) -> u64 {
        let e = self.epoch_blocks.max(2);
        epoch.saturating_mul(e).saturating_add(e - 1)
    }
}

/// Where a validator stands (staking.rs). Its stake lives in its pool; jailing is a pool flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidatorStatus {
    /// Registered, with a pool: selected at an epoch boundary if eligible and its stake ranks.
    Candidate,
    /// In the latest active set.
    Active,
    /// Removed by an admin (dev and test chains): leaves at the next boundary, never selected
    /// again. Its delegators undelegate as usual.
    Removed,
    /// Double-signed: slashed, leaves at the next boundary, never selected again.
    Tombstoned,
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ValidatorRecord {
    /// `boo1…` address of the consensus key (the validator's id).
    pub address: String,
    /// Account that registered it (and owns its pool's self-bond and commission).
    pub owner: String,
    pub consensus_key: String,
    pub p2p_addr: String,
    pub http_addr: String,
    pub status: ValidatorStatus,
    pub registered_at: u64,
    /// Dev and test chains: an admin pinned it (`ApproveValidator`), ranking it ahead of stake.
    pub approved_at: Option<u64>,
    pub approved_by: Option<String>,
    /// First epoch of its latest stint in the active set.
    pub active_from: Option<u64>,
    pub removed_at: Option<u64>,
    pub removed_by: Option<String>,
    /// Last epoch in the active set (set when it leaves).
    pub active_until: Option<u64>,
    /// The owner's cut of its pool's rewards (genesis operators set it; `UpdateValidator` changes it).
    #[serde(default)]
    pub commission_bps: u64,
}

impl Genesis {
    pub fn is_validator(&self, addr: &str) -> bool {
        self.validators.iter().any(|v| v.address == addr)
    }

    pub fn params(&self) -> Params {
        let poc = self.contribution.clone().unwrap_or_else(|| crate::poc::PocTimes::for_block_time(self.block_time_ms));
        Params { block_time_ms: self.block_time_ms, epoch_blocks: self.epoch_blocks, admins: self.validator_admins.clone(), poc, address_prefix: self.address_prefix.clone(), bootstrap_excluded: self.bootstrap_excluded.clone() }
    }

    /// The governance-adjustable parameters at genesis: the defaults with `params` applied.
    pub fn chain_params(&self) -> Result<ChainParams> {
        ChainParams::with_overrides(&self.params(), &self.param_overrides)
    }

    /// The state before block 1, committed (its root is the genesis state root). Expects a
    /// genesis that passes [`Genesis::check`] (`load` runs it); anything that doesn't is skipped
    /// or clamped instead of panicking, and such a genesis never opens anyway.
    pub fn initial_state(&self) -> State {
        let params = self.params();
        let chain_params = self.chain_params().unwrap_or_else(|_| ChainParams::defaults(&params));
        let prefix = params.prefix().to_string();
        let mut state = State { params, chain_params, ..Default::default() };
        for a in &self.allocations {
            state.credit(&a.address, a.amount);
            state.supply += a.amount;
            if let Some(v) = &a.vesting {
                let acct = state.accounts.entry(a.address.clone()).or_default();
                let lock = Lock { id: crate::econ::VESTING_LOCK.into(), amount: LockAmount::Vesting { total: a.amount, start: v.start, cliff: v.cliff, end: v.end }, reason: a.label.clone().unwrap_or_else(|| "genesis allocation".into()) };
                acct.locks.push(lock);
            }
        }
        for v in &self.validators {
            state.validators.insert(
                v.address.clone(),
                ValidatorRecord {
                    address: v.address.clone(),
                    owner: v.address.clone(),
                    consensus_key: v.public_key.clone(),
                    p2p_addr: v.p2p_addr.clone(),
                    http_addr: v.http_addr.clone(),
                    status: ValidatorStatus::Active,
                    registered_at: 0,
                    approved_at: None,
                    approved_by: None,
                    active_from: Some(0),
                    removed_at: None,
                    removed_by: None,
                    active_until: None,
                    commission_bps: 0,
                },
            );
        }
        // Operators own their validators: the self-bond moves from the owner's balance into the
        // validator's stake pool as the owner's shares (staking.rs). A validator without an
        // operator (dev and test chains only) gets an empty pool.
        let mut bonds: BTreeMap<String, u64> = BTreeMap::new();
        for o in &self.operators {
            let Some(address) = hex::decode(&o.consensus_key).ok().map(|pk| crypto::address_with_prefix(&prefix, &pk)) else { continue };
            if !state.validators.contains_key(&address) {
                continue;
            }
            let bond = o.self_bond.min(state.account(&o.owner).balance);
            if let Some(acct) = state.accounts.get_mut(&o.owner) {
                acct.balance -= bond;
            }
            let v = state.validators.get_mut(&address).expect("checked above");
            v.owner = o.owner.clone();
            v.commission_bps = o.commission_bps;
            bonds.insert(address, bond);
        }
        for v in &self.validators {
            crate::staking::genesis_pool(&mut state, &v.address, bonds.get(&v.address).copied().unwrap_or(0));
        }
        let mut set: Vec<String> = self.validators.iter().map(|v| v.address.clone()).collect();
        set.sort();
        state.active_sets.insert(0, set);
        for n in &self.reserved_names {
            if let Ok(n) = normalize_name(n) {
                state.econ.reserved_names.insert(n, crate::econ::ReservedName { reserved_at: 0 });
            }
        }
        for e in &self.soul_ledger {
            state.poc.souls.insert(e.account.clone(), crate::poc::Soul { points: e.soul, at: 0, earned: e.soul });
        }
        for (repo, weight) in &self.pool_repos {
            state.poc.repos.insert(repo.to_ascii_lowercase(), *weight);
        }
        for f in &self.features {
            state.upgrade.features.insert(f.clone(), crate::upgrade::Feature { activated_at: 0 });
        }
        state.upgrade.schedule = self.upgrades.clone();
        // P-ECON: the base fee and block-size limit for the first blocks (at the floor).
        crate::econ::init_genesis(&mut state);
        crate::beacon::init_genesis(&mut state, &self.chain_id, self.created_at_ms);
        state.ghost.meta.instance = crate::ghost::chain_instance(self);
        state.commit_block().expect("an in-memory tree can't fail");
        state
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("missing {} — run `blockula summon` first", path.display()))?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        if v.get("validators").is_none() {
            bail!("{} was created by an older single-validator Blockula; re-summon it (or `blockula dev --reset`)", path.display());
        }
        for (old, new) in [("min_bond", "min_self_bond"), ("max_validators", "max_validators")] {
            if v.get(old).is_some() {
                bail!("{}: `{old}` moved to `params.{new}` (protocol v1); re-summon the chain (or `blockula dev --reset`)", path.display());
            }
        }
        let g: Genesis = serde_json::from_value(v)?;
        g.check().with_context(|| format!("{}", path.display()))?;
        Ok(g)
    }

    /// Everything a genesis must satisfy (`load` runs it): its addresses use its prefix; the
    /// parameters, allocations and vesting schedules are in bounds; operators match validators,
    /// prove possession of their keys and can fund their self-bonds; reserved names, the Soul
    /// ledger and features are well-formed; and mainnet has no admins, no faucet, and an operator
    /// for every validator. (Elsewhere a validator without an operator has no self-bond: it's
    /// ineligible, so it leaves the set at the first boundary unless that would empty it.)
    pub fn check(&self) -> Result<()> {
        use std::collections::BTreeSet;
        self.check_prefix()?;
        if let Some(t) = &self.contribution {
            t.check()?;
        }
        ensure!(self.epoch_blocks >= 2 && self.block_time_ms >= 1, "epoch_blocks must be at least 2 and block_time_ms at least 1");
        let params = self.params();
        let prefix = params.prefix();
        let cp = self.chain_params().context("params")?;
        let display = |what: &str, v: &str| -> Result<()> {
            ensure!(!v.trim().is_empty() && v.len() <= MAX_LABEL && !v.chars().any(|c| c.is_control()), "{what} must be 1–{MAX_LABEL} bytes with no control characters");
            Ok(())
        };
        let mut supply = 0u64;
        let mut held: BTreeMap<&str, u64> = BTreeMap::new();
        let mut vesting = BTreeSet::new();
        for a in &self.allocations {
            supply = supply.checked_add(a.amount).ok_or_else(|| anyhow!("allocations add up to more than u64 drops"))?;
            *held.entry(&a.address).or_default() += a.amount;
            if let Some(l) = &a.label {
                display("allocation label", l)?;
            }
            if let Some(v) = &a.vesting {
                ensure!(v.start <= v.cliff && v.cliff <= v.end && v.start < v.end, "vesting of {}: needs start ≤ cliff ≤ end and start < end", a.address);
                // One vesting lock per account: locks overlap (the largest applies), they don't add.
                ensure!(vesting.insert(&a.address), "{} has more than one vesting allocation", a.address);
            }
        }
        // Epoch 0's set is every genesis validator (a set over `max_validators` shrinks at the
        // first boundaries), so it must fit the p2p layer's peer set like any later one.
        let max_set = crate::consensus::MAX_PEERS_PER_SET;
        ensure!(!self.validators.is_empty() && self.validators.len() <= max_set, "a genesis needs 1–{max_set} validators (the p2p peer set's bound)");
        // Each validator once, with a well-formed consensus key that is its address: a validator
        // listed twice makes epoch 0's set unusable (consensus refuses duplicate keys) and bonds
        // its operator's self-bond twice (audit F33).
        let (mut addresses, mut public_keys) = (BTreeSet::new(), BTreeSet::new());
        for v in &self.validators {
            let who = format!("validator {}", v.address);
            ensure!(crypto::is_lower_hex(&v.public_key) && v.public_key.len() == crypto::PUBLIC_KEY_LEN * 2, "{who}: public_key must be an ML-DSA-65 public key in lowercase hex");
            ensure!(v.address == crypto::address_with_prefix(prefix, &hex::decode(&v.public_key)?), "{who} isn't its public key's address");
            ensure!(addresses.insert(v.address.as_str()) && public_keys.insert(v.public_key.as_str()), "{who} is listed twice");
            check_host_port(&v.p2p_addr).with_context(|| format!("{who}: p2p_addr"))?;
            check_http_addr(&v.http_addr).with_context(|| format!("{who}: http_addr"))?;
        }
        let mut keys = BTreeSet::new();
        let mut bonds: BTreeMap<&str, u64> = BTreeMap::new();
        for o in &self.operators {
            let who = format!("operator {}", o.owner);
            ensure!(crypto::parse_address(&o.owner).map(|(p, _)| p) == Some(prefix), "{who}: the owner must be a {prefix}1… address");
            ensure!(system_account(prefix, &o.owner).is_none(), "{who}: a keyless system account can't own a validator");
            ensure!(crypto::is_lower_hex(&o.consensus_key) && o.consensus_key.len() == crypto::PUBLIC_KEY_LEN * 2, "{who}: consensus_key must be an ML-DSA-65 public key in lowercase hex");
            ensure!(keys.insert(o.consensus_key.as_str()), "{who}: another operator has the same consensus key");
            let v = self.validators.iter().find(|v| v.public_key == o.consensus_key).ok_or_else(|| anyhow!("{who}: its consensus key isn't one of the genesis validators"))?;
            let pk = hex::decode(&o.consensus_key)?;
            ensure!(v.address == crypto::address_with_prefix(prefix, &pk), "{who}: validator {} isn't its consensus key's address", v.address);
            ensure!(v.p2p_addr == o.p2p_addr && v.http_addr == o.http_addr, "{who}: p2p_addr and http_addr must match its entry in validators");
            verify_pop(&self.chain_id, &o.owner, &o.consensus_key, &o.p2p_addr, &o.http_addr, &o.pop).with_context(|| who.clone())?;
            ensure!(o.self_bond >= cp.min_self_bond, "{who}: self_bond is below min_self_bond ({})", fmt_fang(cp.min_self_bond));
            ensure!(o.commission_bps <= cp.commission_max_bps, "{who}: commission_bps is above commission_max_bps ({})", cp.commission_max_bps);
            for (what, v) in [("provider", &o.provider), ("entity", &o.entity)] {
                if let Some(v) = v {
                    display(what, v).with_context(|| who.clone())?;
                }
            }
            if let Some(c) = &o.country {
                ensure!(c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()), "{who}: country must be an ISO 3166-1 alpha-2 code (\"DE\")");
            }
            let b = bonds.entry(&o.owner).or_default();
            *b = b.checked_add(o.self_bond).ok_or_else(|| anyhow!("{who}: self-bonds overflow"))?;
        }
        for (owner, bond) in bonds {
            let has = held.get(owner).copied().unwrap_or(0);
            ensure!(has >= bond, "operator {owner} self-bonds {} but its allocations hold {}", fmt_fang(bond), fmt_fang(has));
        }
        let mut names = BTreeSet::new();
        for n in &self.reserved_names {
            ensure!(normalize_name(n).ok().as_deref() == Some(n.as_str()), "reserved name {n:?} must be a bare lowercase name (no {NAME_SUFFIX})");
            ensure!(names.insert(n), "reserved name {n} is listed twice");
        }
        let mut souls = BTreeSet::new();
        for e in &self.soul_ledger {
            ensure!(crypto::parse_address(&e.account).map(|(p, _)| p) == Some(prefix), "soul_ledger: {} isn't a {prefix}1… address", e.account);
            ensure!(system_account(prefix, &e.account).is_none(), "soul_ledger: {} is a keyless system account", e.account);
            ensure!((1..=MAX_GENESIS_SOUL).contains(&e.soul), "soul_ledger: {} gets 1–{MAX_GENESIS_SOUL} milli-Soul", e.account);
            check_reason(&e.reason).with_context(|| format!("soul_ledger: {}", e.account))?;
            ensure!(souls.insert(&e.account), "soul_ledger lists {} twice", e.account);
        }
        for (repo, weight) in &self.pool_repos {
            check_repo(repo).with_context(|| format!("pool_repos: {repo}"))?;
            ensure!(*repo == repo.to_ascii_lowercase(), "pool_repos: {repo} must be lowercase (as the pool keys it)");
            ensure!((1..=crate::poc::MAX_REPO_WEIGHT).contains(weight), "pool_repos: {repo} needs a weight of 1–{}", crate::poc::MAX_REPO_WEIGHT);
        }
        for (i, a) in self.bootstrap_excluded.iter().enumerate() {
            ensure!(crypto::parse_address(a).map(|(p, _)| p) == Some(prefix), "bootstrap_excluded: {a} isn't a {prefix}1… address");
            ensure!(system_account(prefix, a).is_none(), "bootstrap_excluded: {a} is a keyless system account");
            ensure!(i == 0 || self.bootstrap_excluded[i - 1] < *a, "bootstrap_excluded must be sorted, each account once");
        }
        let mut features = BTreeSet::new();
        for f in &self.features {
            crate::upgrade::check_feature_name(f)?;
            ensure!(crate::upgrade::KNOWN_FEATURES.contains(&f.as_str()), "unknown feature {f} (known: {})", crate::upgrade::KNOWN_FEATURES.join(", "));
            ensure!(features.insert(f), "feature {f} is listed twice");
        }
        crate::upgrade::check_genesis_upgrades(&self.upgrades, &self.chain_id)?;
        if self.chain_id == MAINNET_CHAIN_ID {
            ensure!(self.validator_admins.is_empty(), "{MAINNET_CHAIN_ID} has no admin keys: validator_admins must be empty");
            ensure!(!self.faucet, "{MAINNET_CHAIN_ID} has no faucet");
            ensure!(self.validators.iter().all(|v| keys.contains(v.public_key.as_str())), "every {MAINNET_CHAIN_ID} validator needs an operator (owner, self-bond, commission)");
            // DEC-2: Ghost Mode ships disabled; only governance switches it on, after its audit.
            ensure!(!self.features.iter().any(|f| f == crate::ghost::FEATURE), "{MAINNET_CHAIN_ID} starts with Ghost Mode off: governance activates it after its audit");
            // Governance's own safety parameters start where a vote could take them at most.
            cp.check_governance_floors(&params).context(MAINNET_CHAIN_ID)?;
        }
        Ok(())
    }

    /// The prefix is a known one, and every genesis address uses it.
    pub fn check_prefix(&self) -> Result<()> {
        let p = self.address_prefix.as_str();
        if !crypto::is_prefix(p) {
            bail!("address_prefix must be one of {:?}", crypto::KNOWN_PREFIXES);
        }
        let addrs = self.validators.iter().map(|v| &v.address).chain(self.allocations.iter().map(|a| &a.address)).chain(&self.validator_admins);
        for a in addrs {
            if crypto::parse_address(a).map(|(q, _)| q) != Some(p) {
                bail!("genesis address {a} doesn't use this chain's prefix {p}");
            }
        }
        Ok(())
    }
}

/// `host:port`, where host is an IP or a DNS name.
pub fn check_host_port(s: &str) -> Result<()> {
    let (host, port) = s.rsplit_once(':').ok_or_else(|| anyhow!("expected host:port"))?;
    port.parse::<u16>().map_err(|_| anyhow!("bad port"))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || host.len() > 253 || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:".contains(c)) {
        bail!("bad host");
    }
    Ok(())
}

/// A validator's HTTP endpoint: `host:port` or a full `http(s)://…` URL.
pub fn check_http_addr(s: &str) -> Result<()> {
    if s.len() > MAX_ADDR {
        bail!("too long (max {MAX_ADDR} bytes)");
    }
    if let Some(rest) = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")) {
        let host = rest.split('/').next().unwrap_or_default();
        if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)) {
            bail!("bad URL");
        }
        return Ok(());
    }
    check_host_port(s)
}

/// `http_addr` (host:port or URL) → base URL.
pub fn http_url(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") { addr.trim_end_matches('/').to_string() } else { format!("http://{addr}") }
}

/// Checks a validator registration's proof of possession of its consensus key.
pub fn verify_pop(chain_id: &str, owner: &str, consensus_key: &str, p2p_addr: &str, http_addr: &str, pop: &str) -> Result<()> {
    use commonware_cryptography::Verifier as _;
    let pk = hex::decode(consensus_key).ok().and_then(|b| crate::pq::PublicKey::from_bytes(&b)).ok_or_else(|| anyhow!("bad consensus key"))?;
    let sig = hex::decode(pop).ok().and_then(|b| commonware_codec::DecodeExt::decode(b.as_slice()).ok()).ok_or_else(|| anyhow!("pop must be a hex ML-DSA-65 signature"))?;
    if !pk.verify(POP_CONTEXT, &pop_message(chain_id, owner, p2p_addr, http_addr), &sig) {
        bail!("pop doesn't verify: the consensus key must sign this registration (owner, addresses)");
    }
    Ok(())
}

/// Signs a proof of possession with a validator's consensus key (seed).
pub fn make_pop(seed: [u8; 32], chain_id: &str, owner: &str, p2p_addr: &str, http_addr: &str) -> String {
    use commonware_cryptography::Signer as _;
    let key = crate::pq::PrivateKey::from_seed_bytes(seed);
    hex::encode(key.sign(POP_CONTEXT, &pop_message(chain_id, owner, p2p_addr, http_addr)).as_ref())
}

/// Undo log for state changes: the old value of every entry touched (first touch wins).
#[derive(Default)]
pub struct Journal {
    accounts: JMap<String, Account>,
    names: JMap<String, NameRecord>,
    validators: JMap<String, ValidatorRecord>,
    hunts: JMap<u64, Hunt>,
    github: JMap<String, GithubLink>,
    github_by_account: JMap<String, String>,
    familiars: JMap<String, BTreeMap<String, String>>,
    scalars: Option<(u64, u64)>,
    next_hunt_id: Option<u64>,
    chain_params: Option<ChainParams>,
    pub(crate) poc: crate::poc::PocJournal,
    pub(crate) staking: crate::staking::StakingJournal,
    pub(crate) governance: crate::governance::GovernanceJournal,
    pub(crate) econ: crate::econ::EconJournal,
    pub(crate) beacon: crate::beacon::BeaconJournal,
    pub(crate) upgrade: crate::upgrade::UpgradeJournal,
    pub(crate) ghost: crate::ghost::GhostJournal,
    /// Not an undo record: how far the sender's own staking moves (delegating, unbonding,
    /// redelegating) took its transferable balance down (`staking::own_move`). They keep its
    /// tokens in its holdings, short of rounding dust that stays in a pool, so the lock check
    /// doesn't count that drop as moving locked tokens out (`econ::check_locks`).
    pub(crate) lock_slack: u64,
}

impl Journal {
    #[allow(dead_code)] // the modules' nested journals
    pub fn absorb_pub(&mut self, other: Journal) {
        self.absorb(other)
    }

    /// Accounts this journal touched, by address, with their balance before (`None`: the account
    /// didn't exist). The account-creation fee and lock checks run over these (econ.rs).
    pub(crate) fn touched_accounts(&self) -> Vec<(String, Option<u64>)> {
        let mut v: Vec<(String, Option<u64>)> = self.accounts.touched().map(|(a, old)| (a.clone(), old.map(|o| o.balance))).collect();
        v.sort_unstable();
        v
    }

    /// Folds a nested journal in, keeping the oldest values. Exhaustive, like `State::rollback`.
    fn absorb(&mut self, other: Journal) {
        let Journal { accounts, names, validators, hunts, github, github_by_account, familiars, scalars, next_hunt_id, chain_params, poc, staking, governance, econ, beacon, upgrade, ghost, lock_slack } = other;
        self.lock_slack = self.lock_slack.saturating_add(lock_slack);
        self.accounts.absorb(accounts);
        self.names.absorb(names);
        self.validators.absorb(validators);
        self.hunts.absorb(hunts);
        self.github.absorb(github);
        self.github_by_account.absorb(github_by_account);
        self.familiars.absorb(familiars);
        if self.scalars.is_none() {
            self.scalars = scalars;
        }
        if self.next_hunt_id.is_none() {
            self.next_hunt_id = next_hunt_id;
        }
        if self.chain_params.is_none() {
            self.chain_params = chain_params;
        }
        self.poc.absorb(poc);
        self.staking.absorb(staking);
        self.governance.absorb(governance);
        self.econ.absorb(econ);
        self.beacon.absorb(beacon);
        self.upgrade.absorb(upgrade);
        self.ghost.absorb(ghost);
    }
}

/// The block the end-of-block steps close ([`State::end_block`]). Execution and replay build the
/// same one from the block (replay from the block log), so the hooks see identical inputs.
// The hooks that read every field land with their packages (staking reads view, skipped…).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct BlockInfo<'a> {
    pub height: u64,
    /// The protocol version in force at `height` (`upgrade::version_at` on the state the block
    /// runs on): what every version-gated rule of the block's end-of-block steps reads.
    pub version: u32,
    pub hash: &'a str,
    /// Paid the block's tips; pays for its skipped txs.
    pub proposer: &'a str,
    /// The consensus view the block was proposed in and its parent's view (0 for an epoch's
    /// first block): every view in between was a missed leader slot (MAINNET-DESIGN 3.4).
    pub view: u64,
    pub parent_view: u64,
    /// Wire sizes of the txs the block carried that couldn't be admitted (skipped).
    pub skipped: &'a [u64],
    /// Wire bytes of every tx the block carried, skipped ones included: its fullness, which
    /// moves the base fee (econ.rs).
    pub tx_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    /// Included and charged, but the action failed (reason).
    Failed(String),
}

/// Free localhost port, picked by the OS. Used when summoning local testnets.
pub fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Account {
    #[serde(with = "u64_str")]
    pub balance: u64,
    pub nonce: u64,
    /// Authorized keys by key id. Empty means "only the key the address was derived from".
    pub keys: BTreeMap<String, KeyRecord>,
    /// Tokens the account holds but can't transfer out (vesting allocations; vote locks).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locks: Vec<Lock>,
}

impl Account {
    /// What its locks hold at `height`: what's still vesting, plus the largest vote lock
    /// (MAINNET-DESIGN 3.7: transferable = liquid − max(0, that − staked − unbonding)). econ.rs
    /// enforces it. Vote locks overlap each other (the same tokens vote on many proposals), but
    /// they hold vested tokens only (unvested ones don't vote, `econ::voting_balance`), so they
    /// come on top of the vesting lock rather than inside it: a vote can't free its tokens to
    /// vote again from another account (audit F03).
    pub fn locked_at(&self, height: u64) -> u64 {
        self.unvested_at(height).saturating_add(self.vote_locked_at(height))
    }

    /// What its vesting locks hold at `height` (unvested tokens: they don't vote).
    pub fn unvested_at(&self, height: u64) -> u64 {
        self.locks.iter().filter(|l| matches!(l.amount, LockAmount::Vesting { .. })).map(|l| l.amount.locked_at(height)).max().unwrap_or(0)
    }

    /// What its vote locks hold at `height`: they overlap, so the largest.
    pub fn vote_locked_at(&self, height: u64) -> u64 {
        self.locks.iter().filter(|l| matches!(l.amount, LockAmount::Until { .. })).map(|l| l.amount.locked_at(height)).max().unwrap_or(0)
    }
}

/// A lock on an account's tokens (MAINNET-DESIGN 3.7). Locked tokens stay in the balance and can
/// be staked, delegated and (vested) voted with, never transferred. econ.rs enforces them
/// (`econ::transferable`); governance adds vote locks (`econ::lock`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Lock {
    /// Unique among the account's locks: `"vesting"` for a genesis allocation's, `"vote:<id>"`
    /// for a vote's, and so on.
    pub id: String,
    pub amount: LockAmount,
    /// What it's for, in words ("founder", "vote on proposal #3").
    pub reason: String,
}

/// How much a lock holds, over time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LockAmount {
    /// The unvested part of `total`: all of it before `cliff`, then less linearly from `start`
    /// to `end` (see [`Vesting`]), nothing from `end` on.
    Vesting {
        #[serde(with = "u64_str")]
        total: u64,
        start: u64,
        cliff: u64,
        end: u64,
    },
    /// `amount` until (not including) height `until`: a vote's conviction lock.
    Until {
        #[serde(with = "u64_str")]
        amount: u64,
        until: u64,
    },
}

impl LockAmount {
    pub fn locked_at(&self, height: u64) -> u64 {
        match *self {
            LockAmount::Vesting { total, start, cliff, end } => {
                if height < cliff {
                    total
                } else if height >= end || end <= start {
                    0
                } else {
                    total - (total as u128 * (height - start) as u128 / (end - start) as u128) as u64
                }
            }
            LockAmount::Until { amount, until } => {
                if height < until {
                    amount
                } else {
                    0
                }
            }
        }
    }

    /// Holds nothing from `height` on, for good (`Unlock` may drop it).
    #[cfg_attr(not(test), allow(dead_code))] // econ::release_expired (P-GOV)
    pub fn expired_at(&self, height: u64) -> bool {
        match *self {
            LockAmount::Vesting { end, .. } => height >= end,
            LockAmount::Until { until, .. } => height >= until,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct KeyRecord {
    pub label: String,
    pub role: KeyRole,
    pub added_at: u64,
    /// Session keys: drops spent in the current window, and when that window started.
    #[serde(with = "u64_str")]
    pub spent: u64,
    pub window_start: u64,
}

pub const MAX_KEYS: usize = 16;
/// Most locks one account holds at once (econ.rs adds vote locks; `Unlock` clears expired ones).
/// With `MAX_KEYS`, it bounds an account record well below `state::MAX_VALUE_BYTES`.
pub const MAX_LOCKS: usize = 64;

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NameRecord {
    pub name: String,
    pub owner: String,
    /// When the owner got it: registered it, or accepted it from its last owner (so a name it was
    /// given never ranks ahead of names it registered itself for its primary name).
    pub registered_at: u64,
    /// Last height it resolves (DEC-11: a year after registering, extended by `RenewName`).
    /// Then a grace in which only the owner can renew it (`econ::grace_until`), then anyone can
    /// register it.
    pub expires_at: u64,
    /// The account the owner offered it to (`TransferName`): it's theirs once they accept it
    /// (audit F34: nobody is given a name, and so a primary name, without saying yes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_to: Option<String>,
    /// `name_grace_blocks` as it was when the name was registered or last renewed: its grace is
    /// at least that long, so a `ParamChange` lowering the parameter only shortens the grace of
    /// names registered or renewed after it (gap-6-3), while a raise lengthens every grace.
    #[serde(default)]
    pub grace_blocks: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "snake_case")]
pub enum HuntStatus {
    Open,
    Awarded,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Hunt {
    pub id: u64,
    pub creator: String,
    pub repo: String,
    pub issue: u64,
    pub title: String,
    /// Total escrowed (creator + funders).
    #[serde(with = "u64_str")]
    pub amount: u64,
    /// Contribution per funder (creator included), for pro-rata refunds.
    pub funders: BTreeMap<String, String>,
    pub created_at: u64,
    pub expires_at: u64,
    pub status: HuntStatus,
    pub awarded_to: Option<String>,
    pub awarded_by: Option<String>,
    pub awarded_at: Option<u64>,
    pub pr: Option<u64>,
    pub familiar_key: Option<String>,
    pub cancelled_at: Option<u64>,
}

impl Hunt {
    pub fn contribution(&self, who: &str) -> u64 {
        self.funders.get(who).and_then(|v| v.parse().ok()).unwrap_or(0)
    }
    pub fn self_awarded(&self) -> bool {
        self.awarded_to.as_deref() == Some(self.creator.as_str())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct GithubLink {
    pub handle: String,
    pub account: String,
    pub gist_id: String,
    pub linked_at: u64,
}

/// Most distinct funders one hunt takes (the creator included); past it only they can add to it.
/// Hunts stay in the state forever, so this bounds the record a snapshot has to carry (audit #11:
/// ~210k funders made one record exceed `state::MAX_VALUE_BYTES` and broke snapshot restore).
pub const MAX_HUNT_FUNDERS: usize = 128;

/// Longest a hunt may run: ~1 year of blocks.
pub fn max_hunt_blocks(block_time_ms: u64) -> u64 {
    365 * 86_400 * 1000 / block_time_ms.max(1)
}

/// What `State::accounts` keeps up to date as it changes: Σ balance, and the accounts holding a
/// vesting lock (only genesis allocations create one). Governance's quorum base reads both
/// instead of walking every account (`governance::unlocked_supply`).
pub struct AccountTotals;

impl crate::state::Aggregate<Account> for AccountTotals {
    fn weight(a: &Account) -> u128 {
        a.balance as u128
    }
    fn indexed(a: &Account) -> bool {
        a.locks.iter().any(|l| matches!(l.amount, LockAmount::Vesting { .. }))
    }
}

/// The chain state. Maps are `TMap`s: cloning a `State` is O(1) (structure is shared), and
/// changed keys are tracked so `commit_block` updates the root from just those. How it maps to
/// records and the root is in `state/mod.rs`.
#[derive(Clone, Default)]
pub struct State {
    pub accounts: TMap<String, Account, AccountTotals>,
    pub names: TMap<String, NameRecord>,
    pub supply: u64,
    pub burned: u64,
    /// Every validator ever registered (genesis ones included), by validator address.
    pub validators: TMap<String, ValidatorRecord>,
    /// Active set per epoch (validator addresses), for the last `EPOCH_SETS_KEPT` epochs.
    pub active_sets: TMap<u64, Vec<String>>,
    pub params: Params,
    /// Hunts (bounties on GitHub issues) by id.
    pub hunts: TMap<u64, Hunt>,
    pub next_hunt_id: u64,
    /// GitHub links by lowercase handle, and the reverse.
    pub github: TMap<String, GithubLink>,
    pub github_by_account: TMap<String, String>,
    /// Registered Familiars (agent session keys) per account: key id → label.
    pub familiars: TMap<String, BTreeMap<String, String>>,
    /// 🪦 Proof of Contribution: Souls, vouches, claims, Séances, the pool's grants.
    pub poc: crate::poc::Poc,
    /// What governance can change (params.rs); genesis constants are in `params`.
    pub chain_params: ChainParams,
    // The protocol v1 modules (MAINNET-DESIGN 3.4–3.9), each owning its slice of the state.
    pub staking: crate::staking::Staking,
    pub governance: crate::governance::Governance,
    pub econ: crate::econ::Econ,
    pub beacon: crate::beacon::Beacon,
    pub upgrade: crate::upgrade::Upgrades,
    /// 👻 Ghost Mode's shielded pool (ghost.rs), off until governance activates it.
    pub ghost: crate::ghost::Ghost,
    /// The authenticated tree over every record, as of the last `commit_block`.
    pub tree: crate::state::tree::Smt,
}

impl State {
    pub fn account(&self, addr: &str) -> Account {
        self.accounts.get(addr).cloned().unwrap_or_default()
    }

    /// This chain's treasury account.
    pub fn treasury(&self) -> String {
        treasury(self.params.prefix())
    }

    /// This chain's bootstrap delegation account.
    pub fn bootstrap(&self) -> String {
        bootstrap_delegation(self.params.prefix())
    }

    /// This chain's contribution pool account.
    pub fn pool(&self) -> String {
        crate::poc::pool_address(self.params.prefix())
    }

    /// Active set for an epoch (validator records, in set order), if known.
    pub fn active_set(&self, epoch: u64) -> Option<Vec<&ValidatorRecord>> {
        self.active_sets.get(&epoch).map(|s| s.iter().filter_map(|a| self.validators.get(a)).collect())
    }

    /// The end-of-block steps, after a block's txs. Execution, replay and verification's replay
    /// of unexecuted ancestors all run exactly this, in this order:
    ///
    /// 1. `poc_end_block`: Séances closing, and opening on their epoch-boundary cadence.
    /// 2. `econ::end_block`: the base fee and size limit 17 blocks ahead (3.6), lapsed names.
    /// 3. `staking::end_block`: the proposer's charge for the `skipped` txs (junk, CV-5), and
    ///    missed leader slots from `view`/`parent_view` (downtime, jailing).
    /// 4. `governance::end_block`: proposals whose voting ended or whose timelock is over.
    /// 5. `ghost::end_block`: the commitment tree's new root becomes the anchor; old anchors
    ///    age out.
    /// 6. At an epoch boundary: `beacon::end_epoch` (the next epoch's beacon value, jailing
    ///    non-revealers: first, so a withholder earns nothing for the epoch it's caught in and
    ///    the next set leaves it out), `econ::issue` (issuance into the pools, the contribution
    ///    pool and the treasury; the endowment's vested part into the contribution pool),
    ///    `staking::end_epoch` (the next set by stake, churn-limited), `staking::bootstrap_epoch`
    ///    (the bootstrap delegation's rewards and return to the treasury, the rest spread over the
    ///    next set), then [`State::end_epoch`] (how much set history to keep).
    ///
    /// Then the caller commits the block and, last, runs `upgrade::after_block`, which stops the
    /// node before a height its binary can't execute (3.9).
    pub fn end_block(&mut self, b: &BlockInfo) {
        #[cfg(test)]
        crate::upgrade::test_gate::saw(b.height, b.version);
        self.poc_end_block(b.height);
        crate::econ::end_block(self, b);
        crate::staking::end_block(self, b);
        crate::governance::end_block(self, b);
        crate::ghost::end_block(self, b);
        if self.params.is_boundary(b.height) {
            let epoch = self.params.epoch_of(b.height);
            #[cfg(test)]
            if crate::replay::diverge::on(crate::replay::diverge::Divergence::EndOfEpoch) {
                let (community, treasury) = (community(self.params.prefix()), treasury(self.params.prefix()));
                let mut j = Journal::default();
                if self.j_debit(&mut j, &community, 1).is_ok() {
                    self.j_credit(&mut j, &treasury, 1);
                }
            }
            crate::beacon::end_epoch(self, epoch);
            crate::econ::issue(self, epoch);
            crate::staking::end_epoch(self, epoch);
            crate::staking::bootstrap_epoch(self, epoch);
            self.end_epoch(epoch);
        }
    }

    /// Runs after the last block of `epoch`, once `staking::end_epoch` has fixed the set for
    /// `epoch + 1`: keeps the set history the evidence window needs ([`epoch_sets_kept`]).
    pub fn end_epoch(&mut self, _epoch: u64) {
        let keep = epoch_sets_kept(&self.chain_params);
        while self.active_sets.len() as u64 > keep {
            let first = *self.active_sets.keys().next().unwrap();
            self.active_sets.remove(&first);
        }
    }

    /// Runs a block's txs, then its end-of-block steps: what execution and verification's replay
    /// of unexecuted ancestors share, so both reach the same state. `each` gets every tx's result
    /// (`Err`: not admittable, so skipped; the proposer pays for it at the end of the block).
    /// `tx_bytes` (econ's fullness) counts every tx the block carried, skipped ones included.
    #[allow(clippy::too_many_arguments)]
    pub fn run_block<'t>(&mut self, chain_id: &str, height: u64, hash: &str, proposer: &str, view: u64, parent_view: u64, txs: impl IntoIterator<Item = &'t Tx>, mut each: impl FnMut(&'t Tx, Vec<String>, Result<Outcome>)) {
        // Fixed for the whole block: an upgrade its end-of-block steps schedule activates at least
        // `MIN_UPGRADE_LEAD` blocks later.
        let version = crate::upgrade::version_at(self, height);
        let mut skipped = vec![];
        let mut tx_bytes = 0u64;
        for tx in txs {
            let size = tx.size();
            tx_bytes += size;
            let named = self.named_by(tx);
            let r = self.apply(tx, chain_id, proposer, height);
            if r.is_err() {
                skipped.push(size);
            }
            each(tx, named, r);
        }
        self.end_block(&BlockInfo { height, version, hash, proposer, view, parent_view, skipped: &skipped, tx_bytes });
    }

    /// The accounts `tx`'s action names, resolved as the state is now: just before it runs, which
    /// is whom it pays (a name pays whoever owns it when the tx runs: audit gap-7-1). The block's
    /// index rows keep these, so an account's history and the API's `to` name who a sealed tx
    /// paid, not who owned the name once the whole block had run (audit review-main13-1).
    pub fn named_by(&self, tx: &Tx) -> Vec<String> {
        tx.action.addresses().into_iter().filter_map(|(_, named)| self.resolve(named).ok()).collect()
    }

    /// An address as is, or a name's owner. Expired names (DEC-11) don't resolve.
    pub fn resolve(&self, to: &str) -> Result<String> {
        if crypto::is_address(to) {
            return Ok(to.to_string());
        }
        let n = normalize_name(to)?;
        match self.names.get(&n) {
            Some(r) if crate::econ::name_active(self, r) => Ok(r.owner.clone()),
            Some(r) => bail!("{n}{NAME_SUFFIX} expired at block {}", r.expires_at),
            None => bail!("no one has claimed {n}{NAME_SUFFIX} yet"),
        }
    }

    /// First name an address registered that still resolves — shown everywhere instead of the
    /// raw address. A scan of every name: the API looks names up in the store's owner index
    /// instead (`ReadView`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn primary_name(&self, addr: &str) -> Option<String> {
        self.names.values().filter(|r| r.owner == addr && crate::econ::name_active(self, r)).min_by_key(|r| r.registered_at).map(|r| format!("{}{NAME_SUFFIX}", r.name))
    }

    /// Keys on an account, including the implicit original key for accounts that never added one.
    pub fn keys_of(&self, addr: &str) -> BTreeMap<String, KeyRecord> {
        let acct = self.account(addr);
        if !acct.keys.is_empty() {
            return acct.keys;
        }
        let original = crypto::address_bytes(addr).map(hex::encode).unwrap_or_default();
        BTreeMap::from([(original, KeyRecord { label: "original key".into(), role: KeyRole::Owner, added_at: 0, spent: 0, window_start: 0 })])
    }

    fn credit(&mut self, addr: &str, amount: u64) {
        self.accounts.entry(addr.to_string()).or_default().balance += amount;
    }

    // ---- journaled mutation helpers: record the old value on first touch, then mutate ----

    pub(crate) fn acct_mut(&mut self, j: &mut Journal, addr: &str) -> &mut Account {
        j.accounts.touch(&self.accounts, addr);
        self.accounts.entry(addr.to_string()).or_default()
    }

    pub(crate) fn validator_mut(&mut self, j: &mut Journal, addr: &str) -> Option<&mut ValidatorRecord> {
        j.validators.touch(&self.validators, addr);
        self.validators.get_mut(addr)
    }

    pub(crate) fn insert_validator(&mut self, j: &mut Journal, rec: ValidatorRecord) {
        j.validators.touch(&self.validators, &rec.address);
        self.validators.insert(rec.address.clone(), rec);
    }

    pub(crate) fn j_debit(&mut self, j: &mut Journal, addr: &str, amount: u64) -> Result<()> {
        let acct = self.acct_mut(j, addr);
        if acct.balance < amount {
            bail!("insufficient balance: has {}, needs {}", fmt_fang(acct.balance), fmt_fang(amount));
        }
        acct.balance -= amount;
        Ok(())
    }

    pub(crate) fn j_credit(&mut self, j: &mut Journal, addr: &str, amount: u64) {
        self.acct_mut(j, addr).balance += amount;
    }

    fn hunt_mut(&mut self, j: &mut Journal, id: u64) -> Option<&mut Hunt> {
        j.hunts.touch(&self.hunts, &id);
        self.hunts.get_mut(&id)
    }

    fn j_github(&mut self, j: &mut Journal, handle: &str, account: &str) {
        j.github.touch(&self.github, handle);
        j.github_by_account.touch(&self.github_by_account, account);
    }

    fn familiars_mut(&mut self, j: &mut Journal, account: &str) -> &mut BTreeMap<String, String> {
        j.familiars.touch(&self.familiars, account);
        self.familiars.entry(account.to_string()).or_default()
    }

    pub(crate) fn j_scalars(&mut self, j: &mut Journal) {
        if j.scalars.is_none() {
            j.scalars = Some((self.supply, self.burned));
        }
    }

    /// Burns `amount` from the supply (it must already be out of every balance and escrow).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn j_burn(&mut self, j: &mut Journal, amount: u64) {
        self.j_scalars(j);
        self.supply -= amount;
        self.burned += amount;
    }

    /// Mints `amount` into the supply (the caller credits it to a balance or an escrow).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn j_mint(&mut self, j: &mut Journal, amount: u64) {
        self.j_scalars(j);
        self.supply += amount;
    }

    pub(crate) fn name_mut(&mut self, j: &mut Journal, name: &str) -> Option<&mut NameRecord> {
        j.names.touch(&self.names, name);
        self.names.get_mut(name)
    }

    pub(crate) fn remove_name(&mut self, j: &mut Journal, name: &str) -> Option<NameRecord> {
        j.names.touch(&self.names, name);
        self.names.remove(name)
    }

    /// Files a name record (a registration), replacing a lapsed one.
    pub(crate) fn insert_name(&mut self, j: &mut Journal, rec: NameRecord) {
        j.names.touch(&self.names, rec.name.as_str());
        self.names.insert(rec.name.clone(), rec);
    }

    /// The governance-adjustable parameters, journaled (see [`State::exec_set_param`]).
    pub(crate) fn chain_params_mut(&mut self, j: &mut Journal) -> &mut ChainParams {
        if j.chain_params.is_none() {
            j.chain_params = Some(self.chain_params.clone());
        }
        &mut self.chain_params
    }

    /// Undoes everything recorded in `j` (values, and the dirty marks of keys it cleans up).
    pub fn rollback(&mut self, j: Journal) {
        let Journal { accounts, names, validators, hunts, github, github_by_account, familiars, scalars, next_hunt_id, chain_params, poc, staking, governance, econ, beacon, upgrade, ghost, lock_slack: _ } = j;
        accounts.restore(&mut self.accounts);
        names.restore(&mut self.names);
        validators.restore(&mut self.validators);
        if let Some((supply, burned)) = scalars {
            self.supply = supply;
            self.burned = burned;
        }
        hunts.restore(&mut self.hunts);
        github.restore(&mut self.github);
        github_by_account.restore(&mut self.github_by_account);
        familiars.restore(&mut self.familiars);
        if let Some(n) = next_hunt_id {
            self.next_hunt_id = n;
        }
        if let Some(c) = chain_params {
            self.chain_params = c;
        }
        poc.rollback(&mut self.poc);
        staking.rollback(&mut self.staking);
        governance.rollback(&mut self.governance);
        econ.rollback(&mut self.econ);
        beacon.rollback(&mut self.beacon);
        upgrade.rollback(&mut self.upgrade);
        ghost.rollback(&mut self.ghost);
    }

    /// Checks that decide whether a tx can be included at all (at `height`). No state changes.
    /// Returns the sender, the signer's key id and record, what the tx is charged, and whether a
    /// validator's consensus key signed it for its owner (SF-13).
    fn admit(&self, tx: &Tx, chain_id: &str, height: u64, full_checks: bool) -> Result<(String, String, KeyRecord, u64, bool)> {
        if tx.chain_id != chain_id {
            bail!("wrong chain_id: expected {chain_id}");
        }
        if full_checks {
            tx.check_format()?;
        }
        tx.check_prefix(self.params.prefix())?;
        check_validity(tx, height, self.chain_params.max_tx_validity_blocks)?;
        #[cfg(test)]
        if tx.memo == crate::upgrade::test_gate::MEMO && crate::upgrade::version_at(self, height) >= crate::upgrade::PREVIEW_PROTOCOL_VERSION {
            bail!("refused from protocol v{} on (the test gate)", crate::upgrade::PREVIEW_PROTOCOL_VERSION);
        }
        #[cfg(test)]
        if crate::replay::diverge::on(crate::replay::diverge::Divergence::Admission) && !tx.memo.is_empty() {
            bail!("memos are refused (a test binary that diverges from v1)");
        }
        let sender = tx.sender()?;
        let signer = tx.signer_key_id()?;
        let keys = self.keys_of(&sender);
        let (key, consensus_key) = match keys.get(&signer) {
            Some(k) => (k.clone(), false),
            // SF-13: a validator's consensus key may sign its system txs for its owner, who pays.
            None => (crate::staking::system_tx_key(self, tx, &sender, &signer).ok_or_else(|| anyhow!("this key isn't authorized on {}", short(&sender)))?, true),
        };
        // The block's base fee, fixed 17 blocks ahead (econ.rs), plus the tip (capped there).
        let charge = crate::econ::check_fee(self, tx, height)?;
        let acct = self.account(&sender);
        if tx.nonce != acct.nonce {
            bail!("bad nonce: expected {}, got {}", acct.nonce, tx.nonce);
        }
        // What a prepaid distribution claim adds to the sender's free FANG before the fee phase.
        let mut prepaid = 0i128;
        if acct.balance < charge {
            // A distribution claim may pay its fee, and the creation fee of the account it opens,
            // out of what it claims, once the claim itself checks out (governance.rs).
            let claim = match crate::governance::claim_toward_fee(self, tx, &sender, height) {
                Ok(amount) => amount,
                Err(e) if matches!(tx.action, Action::ClaimDistribution { .. }) => bail!("insufficient balance for the fee: has {}, needs {}, and the claim can't pay it: {e}", fmt_fang(acct.balance), fmt_fang(charge)),
                Err(_) => bail!("insufficient balance for the fee: has {}, needs {}", fmt_fang(acct.balance), fmt_fang(charge)),
            };
            let opening = self.creation_fee_for(&sender);
            let need = charge.saturating_add(opening);
            if acct.balance.saturating_add(claim) < need {
                bail!("insufficient balance for the fee: has {} and claims {}, needs {}", fmt_fang(acct.balance), fmt_fang(claim), fmt_fang(need));
            }
            prepaid = claim as i128 - opening as i128;
        }
        // The tip is a payment to the proposer, not a burn (audit F01): only a signer that could
        // move the account's FANG may pay one, and only out of FANG it could move.
        if tx.tip > 0 {
            // SF-13: a consensus key only signs its owner's system txs; it never moves the owner's FANG.
            if consensus_key {
                bail!("a validator's consensus key signs system txs for {} without a tip (tip must be 0)", short(&sender));
            }
            // SF-11: a multisig's owner key acts alone here (a proposal or one approval), so it
            // can't pay anyone on the account's behalf.
            if key.role == KeyRole::Owner && self.governance.multisigs.contains_key(&sender) {
                bail!("{} is a multisig: its owner keys' txs carry no tip (tip must be 0)", short(&sender));
            }
            // Locked tokens may pay the burned base fee, never the tip (3.7).
            let free = crate::econ::free(self, &sender, height) + prepaid;
            if tx.tip as i128 > free {
                bail!("the tip must come from transferable FANG: {} can move {} now (locked tokens may pay the base fee, not a tip)", short(&sender), fmt_fang(free.clamp(0, u64::MAX as i128) as u64));
            }
        }
        if let KeyRole::Session { limit, period_blocks, expires_at } = key.role {
            if height > expires_at {
                bail!("session key expired at block {expires_at}");
            }
            let amount = match &tx.action {
                // Opening the recipient's account burns the creation fee from the same budget.
                Action::Transfer { amount, to } => amount.saturating_add(self.creation_fee_for(to)),
                // An agent registered as a Familiar may claim its work for its sponsor.
                Action::SubmitClaim { .. } if self.familiars.get(&sender).is_some_and(|f| f.contains_key(&signer)) => self.claim_bond(&sender),
                _ => bail!("session keys can only send transfers (and claims, for registered Familiars)"),
            };
            let spent = if height >= key.window_start.saturating_add(period_blocks) { 0 } else { key.spent };
            if spent.saturating_add(amount).saturating_add(charge) > limit {
                bail!("session key limit reached: {} per {period_blocks} blocks", fmt_fang(limit));
            }
        }
        // SF-11: a multisig account's owner keys only propose and approve (governance.rs). A
        // validator's consensus key isn't one of them: it keeps signing its system txs for a
        // multisig owner (a small set's readiness and admissions too: they change nothing of the
        // owner's), except `UpdateValidator` (the pool's terms are the owner's call; the key can't
        // change them from the validator's own account either, `staking::owned_validator`).
        if !consensus_key || matches!(tx.action, Action::UpdateValidator { .. }) {
            crate::governance::check_multisig_admission(self, &sender, &key, &tx.action)?;
        }
        // Ghost Mode's own admission rules (on and unpaused; a shield's size, chain and proof).
        crate::ghost::admit(self, tx, height)?;
        Ok((sender, signer, key, charge, consensus_key))
    }

    /// Validates and applies a tx (signatures are checked before txs reach the state machine:
    /// at admission and in block verification).
    ///
    /// * `Err`: not includable (bad nonce, fee, key…); state untouched.
    /// * `Ok(Applied)`: done.
    /// * `Ok(Failed(reason))`: the fee was charged and the nonce used, but the action failed and
    ///   its effects were rolled back (so failing txs aren't free). Not for a validator's
    ///   consensus key signing for its owner (SF-13): such a tx whose action fails is `Err`, so
    ///   the owner pays only for duties that apply (audit F04: a stolen key can't burn the owner's
    ///   FANG, locked or a multisig's, on failing txs; a proposer that includes one pays for it
    ///   as junk).
    pub fn apply(&mut self, tx: &Tx, chain_id: &str, proposer: &str, height: u64) -> Result<Outcome> {
        let mut j = Journal::default();
        self.apply_j(&mut j, tx, chain_id, proposer, height, true)
    }

    /// `apply`, recording every change in `j` so the caller can roll back (dry runs, mempool
    /// checks, proposals) without cloning the state.
    pub fn apply_j(&mut self, j: &mut Journal, tx: &Tx, chain_id: &str, proposer: &str, height: u64, full_checks: bool) -> Result<Outcome> {
        // Unsigned ghost txs have no sender, nonce or key: their proof authorizes them (ghost.rs).
        // They never reach `admit`, so a version-gated admission rule that must cover them goes in
        // `ghost::apply_unsigned` too.
        if tx.is_unsigned() {
            return crate::ghost::apply_unsigned(self, j, tx, chain_id, proposer, height, full_checks);
        }
        let (sender, signer, key, charge, consensus_key) = self.admit(tx, chain_id, height, full_checks)?;
        let base = charge - tx.tip;
        // A distribution claim its sender can't pay for pays out of the claim (admission checked
        // the claim covers the fee and the new account's creation fee): the claim runs first, here.
        let prepaid = self.account(&sender).balance < charge;
        if prepaid {
            let Action::ClaimDistribution { distribution_id, amount, proof } = &tx.action else { bail!("insufficient balance for the fee") };
            let opening = self.creation_fee_for(&sender);
            let mut cj = Journal::default();
            let claimed = crate::governance::claim_distribution(self, &mut cj, &sender, *distribution_id, *amount, proof, height).and_then(|()| {
                if opening > 0 {
                    self.j_debit(&mut cj, &sender, opening)?;
                    self.j_burn(&mut cj, opening);
                }
                ensure!(self.account(&sender).balance >= charge, "insufficient balance for the fee, even with the claim");
                Ok(())
            });
            if let Err(e) = claimed {
                self.rollback(cj);
                return Err(e);
            }
            j.absorb(cj);
        }
        // Fee phase (always sticks once admitted, but for SF-13 below): the charge is debited
        // (not the whole `fee`), its base part burned and the tip paid to the proposer.
        let mut fj = Journal::default();
        if let KeyRole::Session { period_blocks, .. } = key.role {
            let mut keys = self.keys_of(&sender);
            let rec = keys.get_mut(&signer).unwrap();
            if height >= rec.window_start.saturating_add(period_blocks) {
                rec.spent = 0;
                rec.window_start = height;
            }
            rec.spent = rec.spent.saturating_add(charge);
            self.acct_mut(&mut fj, &sender).keys = keys;
        }
        if let Err(e) = self.j_debit(&mut fj, &sender, charge) {
            self.rollback(fj);
            return Err(e);
        }
        self.j_scalars(&mut fj);
        self.burned += base;
        self.supply -= base;
        self.j_credit(&mut fj, proposer, tx.tip);
        self.acct_mut(&mut fj, &sender).nonce += 1;
        // Action phase (rolled back on failure). Accounts it opens pay the creation fee, and no
        // account may end up moving tokens its locks hold (fees above may: they're paid first).
        let free = crate::econ::free(self, &sender, height);
        let mut aj = Journal::default();
        // A prepaid claim already ran.
        let action = if prepaid { Ok(()) } else { self.apply_action(&mut aj, tx, &sender, &signer, &key, height) };
        let result = action.and_then(|()| {
            let touched = aj.touched_accounts();
            crate::econ::charge_new_accounts(self, &mut aj, &touched, &sender, &signer, &key)?;
            crate::econ::check_locks(self, &touched, &sender, free, aj.lock_slack, height)
        });
        match result {
            Ok(()) => {
                j.absorb(fj);
                j.absorb(aj);
                Ok(Outcome::Applied)
            }
            // SF-13: the owner pays for its validator's duties, not for failed attempts (audit F04).
            Err(e) if consensus_key => {
                self.rollback(aj);
                self.rollback(fj);
                Err(anyhow!("{e} (not admitted: a validator's consensus key acting for {} pays only for txs that apply)", short(&sender)))
            }
            Err(e) => {
                self.rollback(aj);
                j.absorb(fj);
                Ok(Outcome::Failed(e.to_string()))
            }
        }
    }

    /// The action phase of `apply_j` (no fee, nonce or admission). Multisig accounts run their
    /// approved actions through it too (governance.rs).
    pub(crate) fn apply_action(&mut self, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, key: &KeyRecord, height: u64) -> Result<()> {
        let sender = sender.to_string();
        match &tx.action {
            Action::Transfer { to, amount } => {
                let to = self.resolve(to)?;
                if !self.accounts.contains_key(&to) && *amount < MIN_NEW_ACCOUNT {
                    bail!("sending to a new account needs at least {}", fmt_fang(MIN_NEW_ACCOUNT));
                }
                #[cfg(test)]
                if crate::replay::diverge::on(crate::replay::diverge::Divergence::Handler) && !self.accounts.contains_key(&to) {
                    bail!("transfers can't open accounts (a test binary that diverges from v1)");
                }
                if let KeyRole::Session { .. } = key.role {
                    let mut keys = self.account(&sender).keys;
                    if let Some(rec) = keys.get_mut(signer) {
                        rec.spent = rec.spent.saturating_add(*amount);
                    }
                    self.acct_mut(j, &sender).keys = keys;
                }
                self.j_debit(j, &sender, *amount)?;
                self.j_credit(j, &to, *amount);
            }
            // Prices, expiry and the reserved list: econ.rs (DEC-11).
            Action::RegisterName { name } => crate::econ::register_name(self, j, &sender, name, height)?,
            Action::AddKey { key_id, label, role, public_key, pop } => {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can add keys");
                }
                if key_id.len() != 64 || !crypto::is_lower_hex(key_id) {
                    bail!("key_id must be 32 bytes of lowercase hex (BLAKE3 of the public key)");
                }
                // The added key consents to this account on this chain (audit gap-7-2). The tx's
                // chain id and genesis hash are the chain's (checked before it executes); `sender`
                // is the account the key joins, a multisig's own for an approved AddKey.
                crate::types::verify_key_pop(&tx.chain_id, &tx.genesis_hash, &sender, key_id, public_key, pop)?;
                if label.len() > MAX_LABEL {
                    bail!("key label too long (max {MAX_LABEL} bytes)");
                }
                if let KeyRole::Session { period_blocks, expires_at, .. } = role {
                    if *period_blocks == 0 {
                        bail!("session period_blocks must be at least 1");
                    }
                    if expires_at.saturating_sub(height) > self.params.max_session_blocks() {
                        bail!("session keys can last at most {} blocks (~400 days)", self.params.max_session_blocks());
                    }
                }
                let mut keys = self.keys_of(&sender);
                if keys.contains_key(key_id) {
                    bail!("that key is already on this account");
                }
                if keys.len() >= MAX_KEYS {
                    bail!("accounts can hold at most {MAX_KEYS} keys");
                }
                keys.insert(key_id.clone(), KeyRecord { label: label.clone(), role: role.clone(), added_at: height, spent: 0, window_start: height });
                self.acct_mut(j, &sender).keys = keys;
            }
            Action::RemoveKey { key_id } => {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can remove keys");
                }
                let mut keys = self.keys_of(&sender);
                let removed = keys.remove(key_id).ok_or_else(|| anyhow!("no such key on this account"))?;
                if removed.role == KeyRole::Owner && !keys.values().any(|k| k.role == KeyRole::Owner) {
                    bail!("can't remove the last owner key — you'd lock yourself out");
                }
                self.acct_mut(j, &sender).keys = keys;
                if self.familiars.get(&sender).is_some_and(|f| f.contains_key(key_id)) {
                    let f = self.familiars_mut(j, &sender);
                    f.remove(key_id);
                    if f.is_empty() {
                        self.familiars.remove(&sender);
                    }
                }
            }
            // Validators (staking.rs): registration is permissionless; admins' approve and remove
            // survive only as dev and test overrides.
            Action::RegisterValidator { .. } => crate::staking::register(self, j, tx, &sender, key, height)?,
            Action::ApproveValidator { validator } => crate::staking::approve(self, j, &sender, validator, height)?,
            Action::RemoveValidator { validator } => crate::staking::remove(self, j, &sender, validator, height)?,
            Action::Unbond { .. } => bail!("Unbond is gone: a validator's bond is its owner's stake in its pool. Undelegate it, then WithdrawUnbonded once the unbonding period is over"),
            Action::LinkGithub { handle, gist_id } => {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can link GitHub");
                }
                let lower = handle.to_ascii_lowercase();
                if let Some(h) = self.github_by_account.get(&sender) {
                    bail!("this account is already linked to @{h}; unlink it first");
                }
                if let Some(l) = self.github.get(&lower) {
                    bail!("@{} is already linked to {}; it must be unlinked first", l.handle, short(&l.account));
                }
                // Audit xcut-economic-4: a squatter can't take a handle validators have seen a gist
                // prove (its first verified claim, `Poc::handles`), nor one governance holds for
                // whoever asked it to unlink a squatter (`poc::HandleHold`).
                if let Some(a) = self.poc.handles.get(&lower).filter(|a| **a != sender) {
                    bail!("@{handle}'s contributions are credited to {} (validators checked its gist); only that account can link it", short(a));
                }
                if let Some(h) = self.poc.handle_holds.get(&lower).filter(|h| h.until > height && h.account != sender) {
                    bail!("@{handle} is held for {} until block {} (governance unlinked it at their request)", short(&h.account), h.until);
                }
                if self.poc.handle_holds.contains_key(&lower) {
                    self.set_handle_hold(j, &lower, None);
                }
                self.j_github(j, &lower, &sender);
                self.github.insert(lower.clone(), GithubLink { handle: handle.clone(), account: sender.clone(), gist_id: gist_id.clone(), linked_at: height });
                self.github_by_account.insert(sender.clone(), lower);
            }
            Action::UnlinkGithub { handle } => {
                let link = self.github.get(&handle.to_ascii_lowercase()).cloned().ok_or_else(|| anyhow!("@{handle} isn't linked"))?;
                let admin = self.params.admins.contains(&sender);
                if link.account != sender && !admin {
                    bail!("only the linked account or a validator admin can unlink @{handle}");
                }
                if link.account == sender && key.role != KeyRole::Owner {
                    bail!("only owner keys can unlink GitHub");
                }
                self.exec_unlink_github(j, handle)?;
            }
            Action::CreateHunt { repo, issue, amount, expires_at, title } => {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can create hunts");
                }
                if *amount < MIN_HUNT {
                    bail!("a hunt needs at least {}", fmt_fang(MIN_HUNT));
                }
                if *expires_at <= height {
                    bail!("expires_at must be in the future");
                }
                if expires_at - height > max_hunt_blocks(self.params.block_time_ms) {
                    bail!("hunts can run at most about a year ({} blocks)", max_hunt_blocks(self.params.block_time_ms));
                }
                self.j_debit(j, &sender, *amount)?;
                if j.next_hunt_id.is_none() {
                    j.next_hunt_id = Some(self.next_hunt_id);
                }
                let id = self.next_hunt_id;
                self.next_hunt_id = id.checked_add(1).ok_or_else(|| anyhow!("hunt ids exhausted"))?;
                j.hunts.touch(&self.hunts, &id);
                self.hunts.insert(
                    id,
                    Hunt { id, creator: sender.clone(), repo: repo.clone(), issue: *issue, title: title.clone(), amount: *amount, funders: BTreeMap::from([(sender.clone(), amount.to_string())]), created_at: height, expires_at: *expires_at, status: HuntStatus::Open, awarded_to: None, awarded_by: None, awarded_at: None, pr: None, familiar_key: None, cancelled_at: None },
                );
            }
            Action::FundHunt { hunt_id, amount } => {
                if *amount < MIN_HUNT_FUNDING {
                    bail!("fund with at least {}", fmt_fang(MIN_HUNT_FUNDING));
                }
                let h = self.hunts.get(hunt_id).ok_or_else(|| anyhow!("no hunt #{hunt_id}"))?;
                if h.status != HuntStatus::Open {
                    bail!("hunt #{hunt_id} is {:?}, not open", h.status);
                }
                if height > h.expires_at {
                    bail!("hunt #{hunt_id} expired at block {}", h.expires_at);
                }
                if !h.funders.contains_key(&sender) && h.funders.len() >= MAX_HUNT_FUNDERS {
                    bail!("hunt #{hunt_id} has {MAX_HUNT_FUNDERS} funders already; only they can add to it");
                }
                let total = h.amount.checked_add(*amount).ok_or_else(|| anyhow!("bounty overflow"))?;
                let mine = h.contribution(&sender).checked_add(*amount).ok_or_else(|| anyhow!("bounty overflow"))?;
                self.j_debit(j, &sender, *amount)?;
                let h = self.hunt_mut(j, *hunt_id).unwrap();
                h.amount = total;
                h.funders.insert(sender.clone(), mine.to_string());
            }
            Action::AwardHunt { hunt_id, to, pr, familiar_key } => {
                let h = self.hunts.get(hunt_id).ok_or_else(|| anyhow!("no hunt #{hunt_id}"))?;
                if h.status != HuntStatus::Open {
                    bail!("hunt #{hunt_id} is already {:?}", h.status);
                }
                // GOV-2: only the creator awards; nobody else can direct a hunt's escrow (governance
                // can only refund its funders, `RefundHunt`).
                if h.creator != sender {
                    bail!("only the hunt's creator can award it");
                }
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can award hunts");
                }
                let to = self.resolve(to)?;
                if let Some(k) = familiar_key {
                    if !self.familiars.get(&to).is_some_and(|f| f.contains_key(k)) {
                        bail!("that key isn't a registered Familiar of {}", short(&to));
                    }
                }
                let amount = h.amount;
                self.j_credit(j, &to, amount);
                let h = self.hunt_mut(j, *hunt_id).unwrap();
                h.status = HuntStatus::Awarded;
                h.awarded_to = Some(to);
                h.awarded_by = Some(sender.clone());
                h.awarded_at = Some(height);
                h.pr = Some(*pr);
                h.familiar_key = familiar_key.clone();
            }
            Action::CancelHunt { hunt_id } => {
                let h = self.hunts.get(hunt_id).ok_or_else(|| anyhow!("no hunt #{hunt_id}"))?;
                if h.status != HuntStatus::Open {
                    bail!("hunt #{hunt_id} is already {:?}", h.status);
                }
                // Cancelling refunds every funder exactly (it can't redirect escrow). Admins exist only
                // on dev and test chains; elsewhere governance refunds (`RefundHunt`).
                let admin = self.params.admins.contains(&sender);
                if !admin {
                    if h.creator != sender {
                        bail!("only the hunt's creator (after it expires) or a governance refund can cancel it");
                    }
                    if height <= h.expires_at {
                        bail!("hunt #{hunt_id} can be cancelled by its creator after block {}", h.expires_at);
                    }
                }
                self.exec_refund_hunt(j, *hunt_id, height)?;
            }
            Action::RegisterFamiliar { key_id, label } => {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can register Familiars");
                }
                match self.keys_of(&sender).get(key_id) {
                    Some(k) if matches!(k.role, KeyRole::Session { .. }) => {}
                    Some(_) => bail!("Familiars are agent session keys; that's an owner key"),
                    None => bail!("that key isn't on this account (add it as a session key first)"),
                }
                self.familiars_mut(j, &sender).insert(key_id.clone(), label.clone());
            }
            // Protocol v1 (MAINNET-DESIGN 3.1): each module applies its own tags.
            Action::Delegate { .. } | Action::Undelegate { .. } | Action::Redelegate { .. } | Action::WithdrawUnbonded {} | Action::UpdateValidator { .. } | Action::Unjail { .. } | Action::ReportEquivocation { .. } | Action::ValidatorReady { .. } | Action::AdmitValidator { .. } => crate::staking::apply(self, j, tx, &sender, signer, key, height)?,
            Action::BeaconCommit { .. } | Action::BeaconReveal { .. } | Action::AttestRevert { .. } => crate::beacon::apply(self, j, tx, &sender, signer, key, height)?,
            Action::Propose { .. } | Action::Vote { .. } | Action::Unlock {} | Action::MultisigCreate { .. } | Action::MultisigPropose { .. } | Action::MultisigApprove { .. } | Action::ClaimDistribution { .. } => crate::governance::apply(self, j, tx, &sender, signer, key, height)?,
            Action::RenewName { .. } | Action::TransferName { .. } | Action::ReleaseName { .. } => crate::econ::apply(self, j, tx, &sender, signer, key, height)?,
            Action::Shield { .. } | Action::PrivateTransfer { .. } | Action::Unshield { .. } | Action::SetGhostAddress { .. } | Action::GhostPause {} => crate::ghost::apply(self, j, tx, &sender, signer, key, height)?,
            _ => self.apply_poc(j, tx, &sender, signer, key, height)?,
        }
        Ok(())
    }

    // ---- levers: what the former admin actions do, without any signer check. Admin-signed txs
    // (dev and test chains) call them after checking the signer; governance (P-GOV) calls them
    // when a proposal executes. The PoC ones are in poc.rs, features and upgrades in upgrade.rs.

    /// Drops a GitHub link (both directions).
    pub(crate) fn exec_unlink_github(&mut self, j: &mut Journal, handle: &str) -> Result<()> {
        let lower = handle.to_ascii_lowercase();
        let link = self.github.get(&lower).cloned().ok_or_else(|| anyhow!("@{handle} isn't linked"))?;
        self.j_github(j, &lower, &link.account);
        self.github.remove(&lower);
        self.github_by_account.remove(&link.account);
        Ok(())
    }

    /// An `UnlinkGithub` proposal (audit xcut-economic-4): drops the handle's link unless it's
    /// `proposer`'s, and holds the handle for `proposer` for one voting period (`HandleHold`), so
    /// the account that asked for it can link it before a squatter links it again. The proposer
    /// is the one asking, so it's the one the handle is held for; voters judge whether it's the
    /// handle's owner (its gist). A squatter unlinking itself before the proposal runs doesn't
    /// stop the hold. A handle validators have credited to another account (`Poc::handles`) isn't
    /// held: only that account can link it anyway. Returns what it did.
    pub(crate) fn exec_reclaim_github(&mut self, j: &mut Journal, handle: &str, proposer: &str, height: u64) -> Result<String> {
        let lower = handle.to_ascii_lowercase();
        let holder = self.github.get(&lower).map(|l| l.account.clone());
        if holder.as_deref() == Some(proposer) {
            return Ok(format!("@{handle} is already linked to the proposer"));
        }
        let mut did = match holder {
            Some(a) => {
                self.exec_unlink_github(j, handle)?;
                format!("unlinked @{handle} from {}", short(&a))
            }
            None => format!("@{handle} was no longer linked"),
        };
        match self.poc.handles.get(&lower).filter(|a| a.as_str() != proposer) {
            Some(a) => did.push_str(&format!("; its contributions are credited to {}, which alone can link it", short(a))),
            None => {
                let until = height.saturating_add(self.chain_params.voting_blocks);
                self.set_handle_hold(j, &lower, Some(crate::poc::HandleHold { account: proposer.to_string(), until }));
                did.push_str(&format!("; only {} can link it until block {until}", short(proposer)));
            }
        }
        Ok(did)
    }

    /// Cancels an open hunt and refunds every funder what they put in. Nobody can redirect a
    /// hunt's escrow: it goes back exactly where it came from.
    pub(crate) fn exec_refund_hunt(&mut self, j: &mut Journal, hunt_id: u64, height: u64) -> Result<()> {
        let h = self.hunts.get(&hunt_id).ok_or_else(|| anyhow!("no hunt #{hunt_id}"))?;
        if h.status != HuntStatus::Open {
            bail!("hunt #{hunt_id} is already {:?}", h.status);
        }
        let refunds: Vec<(String, u64)> = h.funders.iter().map(|(a, v)| (a.clone(), v.parse().unwrap_or(0))).collect();
        let total: u64 = refunds.iter().try_fold(0u64, |acc, (_, v)| acc.checked_add(*v)).ok_or_else(|| anyhow!("escrow overflow"))?;
        if total != h.amount {
            bail!("escrow accounting mismatch on hunt #{hunt_id}");
        }
        for (who, v) in refunds {
            self.j_credit(j, &who, v);
        }
        let h = self.hunt_mut(j, hunt_id).unwrap();
        h.status = HuntStatus::Cancelled;
        h.cancelled_at = Some(height);
        Ok(())
    }

    /// Sets a governance-adjustable parameter (`ParamChange`): within its bounds, and governance's
    /// own safety parameters only within their bands (`ChainParams::check_governance_change`, as
    /// of the block it runs in).
    pub(crate) fn exec_set_param(&mut self, j: &mut Journal, key: &str, value: u64) -> Result<()> {
        let params = self.params.clone();
        self.chain_params.check_governance_change(key, value, &params, self.econ.height)?;
        let mut next = self.chain_params.clone();
        next.set(key, value, &params)?;
        *self.chain_params_mut(j) = next;
        Ok(())
    }

    /// What creating `to`'s account would burn: `account_creation_fee` if `to` is an address
    /// with no account yet (and not a keyless system account), else nothing.
    pub fn creation_fee_for(&self, to: &str) -> u64 {
        match self.resolve(to) {
            Ok(a) if !self.accounts.contains_key(&a) && system_account(self.params.prefix(), &a).is_none() => self.chain_params.account_creation_fee,
            _ => 0,
        }
    }

    /// State root (hex) as of the last `commit_block`: the sparse Merkle tree over every record.
    pub fn root(&self) -> String {
        hex::encode(self.tree.root())
    }

    /// The summary with names from a scan (the fuzz harness; the API uses `describe_with`).
    #[allow(dead_code)]
    pub fn describe(&self, tx: &Tx, height: Option<u64>) -> String {
        self.describe_with(tx, height, &|a| self.primary_name(a))
    }

    /// [`State::describe`] with the primary-name lookup supplied (the API's comes from the
    /// store's owner index instead of a scan of every name).
    pub fn describe_with(&self, tx: &Tx, height: Option<u64>, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
        let who = |a: &str| primary_name(a).unwrap_or_else(|| short(a));
        let from = tx.sender().map(|s| who(&s)).unwrap_or_default();
        match &tx.action {
            Action::Transfer { to, amount } => {
                let to = if crypto::is_address(to) { who(to) } else { format!("{}{NAME_SUFFIX}", normalize_name(to).unwrap_or(to.clone())) };
                format!("{from} sent {} to {to}", fmt_fang(*amount))
            }
            Action::RegisterName { name } => {
                let sender = tx.sender().map(|s| short(&s)).unwrap_or_default();
                format!("{sender} claimed the name {}{NAME_SUFFIX}", normalize_name(name).unwrap_or(name.clone()))
            }
            Action::AddKey { label, role: KeyRole::Owner, .. } => format!("{from} added an owner key “{label}”"),
            Action::AddKey { label, role: KeyRole::Session { limit, period_blocks, .. }, .. } => {
                format!("{from} let “{label}” spend up to {} every {period_blocks} blocks", fmt_fang(*limit))
            }
            Action::RemoveKey { key_id } => format!("{from} revoked key {}…", &key_id[..key_id.len().min(10)]),
            Action::RegisterValidator { consensus_key, bond, .. } => {
                let v = hex::decode(consensus_key).map(|pk| short(&crypto::address_with_prefix(self.params.prefix(), &pk))).unwrap_or_default();
                format!("{from} bonded {} to run validator {v}, a candidate for the active set (chosen by stake at each epoch boundary)", fmt_fang(*bond))
            }
            Action::ApproveValidator { validator } => {
                let when = match self.validators.get(validator).and_then(|v| v.active_from) {
                    Some(e) => format!("active from epoch {e}"),
                    None => match height {
                        Some(h) => format!("ranked ahead of stake from epoch {}", self.params.epoch_of(h) + 1),
                        None => "ranked ahead of stake from the next epoch".into(),
                    },
                };
                format!("{from} approved validator {}, {when}", who(validator))
            }
            Action::RemoveValidator { validator } => {
                let when = match self.validators.get(validator).and_then(|v| v.active_until) {
                    Some(e) => format!("last active in epoch {e}"),
                    None => "leaves at the next epoch boundary".into(),
                };
                format!("{from} removed validator {}, {when}", who(validator))
            }
            Action::Unbond { validator } => format!("{from} tried to reclaim validator {}'s bond the old way (stake now leaves by undelegating)", who(validator)),
            Action::LinkGithub { handle, .. } => format!("{from} linked GitHub @{handle}"),
            Action::UnlinkGithub { handle } => format!("{from} unlinked GitHub @{handle}"),
            Action::CreateHunt { repo, issue, amount, title, .. } => format!("{from} put {} on {repo}#{issue}: “{title}”", fmt_fang(*amount)),
            Action::FundHunt { hunt_id, amount } => {
                let what = self.hunts.get(hunt_id).map(|h| format!("{}#{}", h.repo, h.issue)).unwrap_or_else(|| format!("hunt #{hunt_id}"));
                format!("{from} added {} to the bounty on {what}", fmt_fang(*amount))
            }
            Action::AwardHunt { hunt_id, to, pr, familiar_key } => {
                let h = self.hunts.get(hunt_id);
                let what = h.map(|h| format!("{}#{}", h.repo, h.issue)).unwrap_or_else(|| format!("hunt #{hunt_id}"));
                let to_name = if crypto::is_address(to) { who(to) } else { format!("{}{NAME_SUFFIX}", normalize_name(to).unwrap_or(to.clone())) };
                let paid = h.map(|h| format!(" {}", fmt_fang(h.amount))).unwrap_or_default();
                let fam = familiar_key.as_ref().and_then(|k| self.resolve(to).ok().and_then(|a| self.familiars.get(&a)?.get(k).cloned())).map(|l| format!(" (earned by their Familiar “{l}”)")).unwrap_or_default();
                format!("{from} awarded the{paid} bounty on {what} to {to_name} for PR #{pr}{fam}")
            }
            Action::CancelHunt { hunt_id } => {
                let what = self.hunts.get(hunt_id).map(|h| format!("{}#{}", h.repo, h.issue)).unwrap_or_else(|| format!("hunt #{hunt_id}"));
                format!("{from} cancelled the hunt on {what}; funders were refunded")
            }
            Action::RegisterFamiliar { label, .. } => format!("{from} registered the Familiar “{label}”"),
            Action::Delegate { .. } | Action::Undelegate { .. } | Action::Redelegate { .. } | Action::WithdrawUnbonded {} | Action::UpdateValidator { .. } | Action::Unjail { .. } | Action::ReportEquivocation { .. } | Action::ValidatorReady { .. } | Action::AdmitValidator { .. } => crate::staking::describe(self, tx, &from, primary_name),
            Action::BeaconCommit { .. } | Action::BeaconReveal { .. } | Action::AttestRevert { .. } => crate::beacon::describe(self, tx, &from),
            Action::Propose { .. } | Action::Vote { .. } | Action::Unlock {} | Action::MultisigCreate { .. } | Action::MultisigPropose { .. } | Action::MultisigApprove { .. } | Action::ClaimDistribution { .. } => crate::governance::describe(self, tx, &from, primary_name),
            Action::RenewName { .. } | Action::TransferName { .. } | Action::ReleaseName { .. } => crate::econ::describe(self, tx, &from, height, primary_name),
            Action::Shield { .. } | Action::PrivateTransfer { .. } | Action::Unshield { .. } | Action::SetGhostAddress { .. } | Action::GhostPause {} => crate::ghost::describe(self, tx, &from, primary_name),
            _ => self.describe_poc(tx, &from, primary_name),
        }
    }
}

/// Replay's check of a logged tx (`Chain::replay`): it must apply or fail as the log recorded
/// (`recorded`: its failure reason, if it failed). Only the kind of outcome is compared: a failure
/// reason is the action's error message, which a later release may word differently, and it
/// isn't consensus state (the state root, checked after the block, is what replay must match).
fn check_replayed(height: u64, i: usize, outcome: &Outcome, recorded: Option<&String>) -> Result<()> {
    match (outcome, recorded) {
        (Outcome::Applied, None) => Ok(()),
        (Outcome::Failed(now), Some(then)) => {
            if now != then {
                tracing::debug!(height, tx = i, recorded = %then, now = %now, "a replayed tx failed with another reason (reworded since)");
            }
            Ok(())
        }
        (Outcome::Applied, Some(then)) => bail!("replaying block {height}: tx {i} applied, but the log recorded it failing ({then})"),
        (Outcome::Failed(now), None) => bail!("replaying block {height}: tx {i} failed ({now}), but the log recorded it applying"),
    }
}

/// `height <= valid_until <= height + max_validity`: a tx past `valid_until` can never execute.
pub fn check_validity(tx: &Tx, height: u64, max_validity: u64) -> Result<()> {
    if tx.valid_until < height {
        bail!("expired at block {}", tx.valid_until);
    }
    let latest = height.saturating_add(max_validity);
    if tx.valid_until > latest {
        bail!("valid_until {} is too far ahead: at most {latest} (an hour of blocks from now)", tx.valid_until);
    }
    Ok(())
}

/// The stateless part of the fee check (HTTP, gossip, block verification before the state is
/// at hand): `fee` covers the tip plus the lowest base fee any parameters allow
/// (`econ::lowest_base_fee`). The exact check needs the block's base fee: `econ::check_fee`.
pub fn check_fee(tx: &Tx) -> Result<()> {
    let least = crate::econ::lowest_base_fee(tx.size());
    match least.checked_add(tx.tip) {
        Some(c) if c <= tx.fee => Ok(()),
        _ => bail!("fee too low: this tx costs at least {} drops (base fee at least {least} + tip {}), but its fee allows {}", least.saturating_add(tx.tip), tx.tip, tx.fee),
    }
}

pub fn short(addr: &str) -> String {
    if addr.len() > 16 { format!("{}…{}", &addr[..10], &addr[addr.len() - 6..]) } else { addr.to_string() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Validator,
    FullNode,
}

pub struct SummonOpts {
    pub chain_id: String,
    pub faucet: bool,
    pub block_time_ms: u64,
    pub validators: usize,
    /// HTTP port for the first validator (others get free ports).
    pub http_port: Option<u16>,
    /// Season 0 validator admins; defaults to the first validator's account.
    pub admins: Vec<String>,
    pub epoch_blocks: u64,
    /// Minimum validator self-bond in drops (`params.min_self_bond`). `None`: 50 FANG on faucet
    /// chains (one faucet drip covers it), the `ChainParams` default otherwise.
    pub min_bond: Option<u64>,
    /// Proof of Contribution timings (`None`: derived from the block time).
    pub contribution: Option<crate::poc::PocTimes>,
    /// Address prefix: `boo` (mainnet, dev) or `tboo` (public testnets).
    pub address_prefix: String,
    /// Genesis `params` overrides (`blockula dev` shortens durations with
    /// `ChainParams::dev_overrides`). `min_bond`, when set, wins for `min_self_bond`.
    pub params: BTreeMap<String, u64>,
}

impl SummonOpts {
    pub fn new(chain_id: &str, validators: usize) -> Self {
        Self { chain_id: chain_id.into(), faucet: true, block_time_ms: 1000, validators, http_port: None, admins: vec![], epoch_blocks: default_epoch_blocks(), min_bond: None, contribution: None, address_prefix: default_address_prefix(), params: BTreeMap::new() }
    }
}

/// How far behind the (claimed) network height still counts as synced.
pub const SYNC_SLACK: u64 = 3;
/// Certificates are stored on disk for these heights (plus every epoch's last block, and see
/// [`CERT_SPACING_BYTES`]); the last `view::RECENT_CERTS` heights' are kept in memory.
pub const CERT_CHECKPOINT: u64 = 100;
/// ... and once this many consensus-block bytes have gone by since the last one stored, so a
/// stretch between two stored certificates stays well inside one `/api/sync` response however
/// full its blocks are (at most this plus one block; F09). An empty chain stores one every
/// `CERT_CHECKPOINT` heights, a full one about every 1 MiB of blocks (a certificate is ~5-7%
/// of that).
pub const CERT_SPACING_BYTES: u64 = 1 << 20;
/// Mempool txs not included within this many blocks are dropped (they stay valid until their
/// `valid_until`, so a holder could resubmit them).
pub const MEMPOOL_TTL_BLOCKS: u64 = 600;
/// A waiting name registration keeps other accounts' registrations of the name out of the
/// mempool for this many blocks after it arrived ([`Chain::first_to_ask`]): the window the
/// suggested fee covers. A registration still waiting after that is being outbid on tips anyway,
/// and holds nothing, so a zero-tip registration can't keep a name from anyone while blocks are
/// full.
pub const NAME_HOLD_BLOCKS: u64 = crate::econ::FEE_LAG;
/// How long the node's own txs (faucet drips, attestations) stay valid.
pub const OWN_TX_VALIDITY: u64 = 600;
/// Recently failed/dropped tx ids remembered for `/api/tx/{id}`.
pub const RECENT_FAILED: usize = 10_000;
/// Max faucet drips waiting in the mempool (kept below `MAX_PENDING_PER_SENDER`).
pub const FAUCET_MAX_PENDING: usize = 32;
/// Blocks a validator keeps by default (`--prune-blocks`): about two days' worth.
pub const PRUNE_DAYS: u64 = 2;
/// A rebuild (or a replay after a crash) syncs the store at least this often.
const REPLAY_SYNC_EVERY: u64 = 1_000;
/// A scratch home ([`Chain::set_scratch`]) commits durably this often: the store keeps every
/// commit since the last durable one in memory.
const SCRATCH_SYNC_EVERY: u64 = 10_000;

/// The fewest heights a pruning node keeps: every kept snapshot's manifest (checkpoint +
/// certifying blocks; the long-lived ones go back up to two `snapshot::long_interval`s), the
/// blocks after it that a joiner who downloaded it syncs next, and its epoch anchor (the last
/// block of the epoch before, up to an epoch before the snapshot: `snapshot::anchor_height`),
/// every ancestor a proposal may still cite, and a tx's whole validity window (so the tx index
/// still catches a replayed tx). The validity window is a governance parameter
/// (`max_tx_validity_blocks`), so this is re-read from the state at every commit.
pub fn min_keep_blocks(state: &State) -> u64 {
    use crate::snapshot::{long_interval, CHECKPOINT_DELAY};
    state.params.epoch_blocks.max(2) + 2 * long_interval(&state.params) + 2 * CHECKPOINT_DELAY + crate::consensus::MAX_EXEC_LAG + state.chain_params.max_tx_validity_blocks
}

/// What a pruning validator keeps by default: `PRUNE_DAYS` of blocks at this block time.
pub fn default_keep_blocks(state: &State) -> u64 {
    (PRUNE_DAYS * 86_400_000 / state.params.block_time_ms.max(1)).max(min_keep_blocks(state))
}

/// How `Chain::open_with` lays out a new home (existing homes keep theirs).
#[derive(Clone, Debug, Default)]
pub struct OpenOpts {
    /// Heights per log segment (default `store::SEGMENT_HEIGHTS`).
    pub segment_heights: Option<u64>,
}

/// Where a test makes `execute` stop, as if the process died there (crash-injection tests).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritePoint {
    /// The consensus block is appended (not synced); the executed block isn't.
    AfterCblock,
    /// Both logs are appended; the block line isn't synced.
    AfterBlockLine,
    /// Both logs are synced; the store isn't committed.
    BeforeStoreCommit,
    /// The store is committed; consensus hasn't been acknowledged.
    AfterStoreCommit,
}

pub struct Chain {
    pub home: PathBuf,
    pub genesis: Genesis,
    genesis_arc: Arc<Genesis>,
    /// Validator key (also the faucet key on testnets). `None` on full nodes.
    pub keys: Option<Keys>,
    pub role: Role,
    pub state: State,
    pub genesis_root: String,
    /// Hash of the genesis consensus block (parent of block 1).
    pub genesis_hash: String,
    height: u64,
    last_hash: String,
    /// The last executed block (older ones are read from the log, through a bounded cache), and
    /// its tx ids.
    head: Option<Arc<Block>>,
    head_ids: std::collections::HashSet<String>,
    /// The block logs and the database, behind their own lock (see [`Writer`]).
    writer: Arc<parking_lot::Mutex<Writer>>,
    /// The database's commit point and a snapshot of it as of that commit (the tree's node store;
    /// readers get it with the view).
    store: Arc<Store>,
    point: CommitPoint,
    snap: Arc<Snap>,
    disk: Arc<Disk>,
    pub mempool: Vec<Tx>,
    /// Ids of `mempool`'s txs, index for index (so upkeep never rehashes them).
    mempool_ids: Vec<String>,
    /// Ids in the mempool.
    pool: std::collections::HashSet<String>,
    /// Each sender's queued txs: their indexes in `mempool`, in nonce order.
    pending: HashMap<String, Vec<usize>>,
    /// The validator-duty lane (`staking::duty_of`): queued ids → (validator, kind of duty).
    /// Outside `MAX_MEMPOOL`, never evicted for a tip, first in proposals (audit F32/F56). A
    /// prefix of each sender's queue ([`Chain::rebuild_lane`]).
    lane: HashMap<String, (String, &'static str)>,
    /// Every queued validator duty, in the lane or waiting for a slot in it: queued ids →
    /// (validator, kind of duty), as `staking::duty_of` said when it was admitted.
    duties: HashMap<String, (String, &'static str)>,
    /// Admission's simulated state per recently active sender (see [`Chain::submit_tx`]).
    sims: Sims,
    /// Mirror of the mempool and of recent failures, published for readers.
    mp: MempoolView,
    failed_order: std::collections::VecDeque<String>,
    /// Highest finalized height proven by a certificate we verified or saw from consensus.
    pub finalized_tip: u64,
    /// Outbound mempool gossip (validators) — every accepted tx is pushed here.
    pub gossip: Option<futures::channel::mpsc::UnboundedSender<Tx>>,
    /// Validator-set history + handoff proofs (`epochs.json`).
    pub epochs: crate::epochs::EpochRegistry,
    /// Set when this node fast-synced: history starts after this height.
    pub base: Option<Base>,
    /// Latest tip each HTTP peer *claims* (display-only; never trusted for anything else).
    claims: HashMap<String, u64>,
    /// Set while a snapshot is being written, so slow disks can't pile up writer threads.
    snapshot_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// A scratch home's chain ([`Chain::set_scratch`]): it takes no state snapshots.
    scratch: bool,
    /// What readers (the HTTP API) see: published after every block and mempool change.
    pub live: Arc<Live>,
    /// Blocks kept on disk (`None`: all of them, an archive node).
    keep_blocks: Option<u64>,
    /// Set when persisting a block failed: memory is ahead of the disk, so this instance executes
    /// nothing more (a restart recovers from the logs).
    broken: Option<String>,
    /// Blocks `open` executed again because they were past the store's commit point.
    pub replayed: u64,
    /// Writes `epochs.json` off the chain lock.
    epochs_saver: EpochsSaver,
    /// Consensus-block bytes executed since the last certificate stored on disk
    /// ([`CERT_SPACING_BYTES`]; starts full, so the first certificate after a start is kept).
    cert_bytes: u64,
    /// What this binary supports, `--halt-height`, and whether the node halted (upgrade.rs).
    pub halt: crate::upgrade::HaltControl,
    /// Tests: this node's proposals carry a junk tx (stateless-valid, never admittable), like a
    /// Byzantine leader filling blocks it doesn't pay for (CV-5).
    #[cfg(test)]
    pub junk_proposals: bool,
    /// Tests: this node's proposals cite a wrong `exec_root` (a Byzantine leader lying about the
    /// state it executed, or one that diverged).
    #[cfg(test)]
    pub wrong_exec_root: bool,
}

/// Epochs of validator-set history a node always keeps and serves: the weak-subjectivity period
/// (no joiner may start from an older root, `checkpoints::ws_period_blocks`) under the genesis
/// parameters and the current ones, whichever is longer, plus a quarter for slow blocks and late
/// joiners. Older changes and their proofs are dropped once the history passes
/// `epochs::KEEP_BYTES` (F10).
pub fn registry_keep_epochs(g: &Genesis, state: &State) -> u64 {
    let eb = state.params.epoch_blocks.max(2);
    let genesis = g.chain_params().map(|cp| crate::checkpoints::ws_period_blocks(&cp, eb)).unwrap_or(0);
    let ws = crate::checkpoints::ws_period_blocks(&state.chain_params, eb).max(genesis).div_ceil(eb);
    ws.saturating_add(ws / 4).saturating_add(2)
}

/// Writes `epochs.json` on its own thread, so a big registry never stalls the chain lock: the
/// newest registry handed to it wins, every write is durable, and dropping it (with the chain)
/// waits for the last one.
struct EpochsSaver {
    path: PathBuf,
    /// The sequence number of the registry on disk; writes take it, so they never go backwards.
    written: Arc<parking_lot::Mutex<u64>>,
    next: std::sync::atomic::AtomicU64,
    queue: Option<std::sync::mpsc::Sender<(u64, Arc<crate::epochs::EpochRegistry>)>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EpochsSaver {
    fn new(path: PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(u64, Arc<crate::epochs::EpochRegistry>)>();
        let written: Arc<parking_lot::Mutex<u64>> = Default::default();
        let (p, w) = (path.clone(), written.clone());
        let thread = std::thread::Builder::new()
            .name("epochs.json".into())
            .spawn(move || {
                while let Ok(mut latest) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        latest = newer;
                    }
                    if let Err(e) = Self::write(&p, &w, latest.0, &latest.1) {
                        tracing::warn!(error = %format!("{e:#}"), "couldn't save epochs.json");
                    }
                }
            })
            .ok();
        Self { path, written, next: Default::default(), queue: Some(tx), thread }
    }

    fn write(path: &Path, written: &parking_lot::Mutex<u64>, seq: u64, e: &crate::epochs::EpochRegistry) -> Result<()> {
        let bytes = serde_json::to_vec(e)?;
        let mut w = written.lock();
        if seq > *w {
            store::write_durably(path, &bytes)?;
            *w = seq;
        }
        Ok(())
    }

    fn seq(&self) -> u64 {
        self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }

    /// Queues a write (in this thread if the writer is gone).
    fn save(&self, e: Arc<crate::epochs::EpochRegistry>) {
        let seq = self.seq();
        if let Some(Err(std::sync::mpsc::SendError((_, e)))) = self.queue.as_ref().map(|q| q.send((seq, e.clone()))) {
            if let Err(err) = Self::write(&self.path, &self.written, seq, &e) {
                tracing::warn!(error = %format!("{err:#}"), "couldn't save epochs.json");
            }
        }
    }

    /// Writes now (a newer queued write may follow, an older one never lands after it).
    fn save_now(&self, e: &crate::epochs::EpochRegistry) -> Result<()> {
        Self::write(&self.path, &self.written, self.seq(), e)
    }
}

impl Drop for EpochsSaver {
    fn drop(&mut self) {
        drop(self.queue.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The block logs and the state database. One executor writes at a time; the consensus path
/// does it without holding the chain lock (`Chain::execute_shared`), so votes, proposals and
/// mempool admission don't wait for fsyncs.
pub struct Writer {
    store: Arc<Store>,
    /// Executed blocks (JSON lines) and the exact consensus block bytes, segmented by height.
    blocks: SegLog,
    cblocks: SegLog,
    /// The commit point as of the last commit.
    point: CommitPoint,
    /// Log segments below this height are deleted.
    files_pruned_below: u64,
    /// A scratch home's writer (`blockula replay`, [`Chain::set_scratch`]): the commit protocol
    /// (logs fsynced, then a durable commit) runs only every [`SCRATCH_SYNC_EVERY`] heights, and
    /// the blocks in between are written without fsyncs, so a crash may lose any block since.
    /// Nodes run it for every block.
    scratch: bool,
    #[cfg(test)]
    fail_at: Option<WritePoint>,
    /// A slow disk, for tests.
    #[cfg(test)]
    delay: Option<std::time::Duration>,
}

/// A block executed in memory, and everything its commit writes.
pub struct Executed {
    block: Arc<Block>,
    /// The consensus block bytes to log (`None`: a replay, already logged).
    cblock: Option<Vec<u8>>,
    records: Vec<RecordChange>,
    nodes: Vec<(NodeKey, Option<Node>)>,
    /// Index rows; the writer fills in the log positions.
    meta: BlockMeta,
    root: Hash,
    root_child: Vec<u8>,
    /// A certificate to keep on disk (checkpoints and epoch ends).
    cert: Option<Vec<u8>>,
    epoch_head: bool,
    prune_below: u64,
}

impl Writer {
    /// The commit protocol (see `store.rs`): the consensus block, then the executed block, each
    /// appended and fsynced, then one durable database transaction (a scratch home's writer: only
    /// every [`SCRATCH_SYNC_EVERY`] heights, its other blocks just appended and committed).
    fn persist(&mut self, e: &mut Executed) -> Result<(Arc<Snap>, CommitPoint)> {
        #[cfg(test)]
        if let Some(d) = self.delay {
            std::thread::sleep(d);
        }
        let durable = !self.scratch || e.block.height.is_multiple_of(SCRATCH_SYNC_EVERY);
        let cpos = self.cblocks.append(e.block.height, e.cblock.as_deref().unwrap_or_default())?;
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::AfterCblock) {
            bail!("injected crash after the consensus block append");
        }
        if durable {
            self.cblocks.sync()?;
        }
        let line = serde_json::to_vec(&*e.block)?;
        e.meta.tx_spans = store::tx_spans(&line, &e.block.txs);
        let bpos = self.blocks.append(e.block.height, &line)?;
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::AfterBlockLine) {
            bail!("injected crash after the block append");
        }
        if durable {
            self.blocks.sync()?;
        }
        (e.meta.block, e.meta.cblock) = (bpos, Some(cpos));
        let done = self.commit(e, durable)?;
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::AfterStoreCommit) {
            bail!("injected crash after the store commit");
        }
        Ok(done)
    }

    /// One database transaction for a block whose log positions are known, then a snapshot of
    /// the result. Whole log segments go once a durable commit has pruned their heights' rows (a
    /// crash may undo a commit that isn't, and the home must still have the blocks it needs).
    fn commit(&mut self, e: &Executed, durable: bool) -> Result<(Arc<Snap>, CommitPoint)> {
        let mut point = self.point.clone();
        point.height = e.block.height;
        point.block_hash = e.block.hash.clone();
        point.root = e.root;
        point.root_child = e.root_child.clone();
        point.blocks_end = e.meta.block.end();
        if let Some(c) = e.meta.cblock {
            point.cblocks_end = c.end();
        }
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::BeforeStoreCommit) {
            bail!("injected crash before the store commit");
        }
        let cert = e.cert.as_deref().map(|c| (e.block.height, c));
        self.store.commit(store::Commit { point: &mut point, records: &e.records, nodes: &e.nodes, block: Some(&e.meta), epoch_head: e.epoch_head, cert, prune_below: e.prune_below, durable })?;
        self.point = point.clone();
        if durable && point.pruned_below >= self.files_pruned_below.saturating_add(self.blocks.reader().seg_heights) {
            self.blocks.prune_below(point.pruned_below)?;
            self.cblocks.prune_below(point.pruned_below)?;
            self.files_pruned_below = point.pruned_below;
        }
        Ok((Arc::new(self.store.snapshot()?), point))
    }
}

/// The mempool checked against a new state, without the chain lock: a copy of the state (O(1))
/// and of the pending txs (shared, with their ids).
pub struct MempoolCheck {
    state: State,
    txs: MempoolView,
    next: u64,
    proposer: String,
    chain_id: String,
}

impl MempoolCheck {
    /// Txs to drop, with why: expired, past the mempool TTL, or no longer applying (each
    /// sender's earlier txs applied first, as a proposal would). Once a sender's tx goes from its
    /// nonce slot, so do its later ones: they'd wait behind a nonce gap, never executing. (A tx
    /// whose nonce another tx used already goes alone.)
    pub fn run(mut self) -> (u64, Vec<(String, String)>) {
        let next = self.next;
        let mut dropped = vec![];
        let mut broken = std::collections::HashSet::new();
        for p in self.txs.in_order() {
            let tx = &p.tx;
            let stale = !tx.is_unsigned() && tx.nonce < self.state.account(&tx.from).nonce;
            let why = if !tx.is_unsigned() && broken.contains(&tx.from) && !stale {
                Some("dropped from the mempool: an earlier transaction from the same account was dropped".to_string())
            } else if tx.valid_until < next {
                Some(format!("expired at block {}", tx.valid_until))
            } else if p.added_at + MEMPOOL_TTL_BLOCKS < next {
                // Not expired: anyone holding it may resubmit it until `valid_until`.
                Some(format!("dropped from the mempool: not included within {MEMPOOL_TTL_BLOCKS} blocks (it stays valid until block {} if resubmitted)", tx.valid_until))
            } else {
                // Kept txs stay applied (this is a private copy of the state); dropped ones don't.
                let mut j = Journal::default();
                let outcome = self.state.apply_j(&mut j, tx, &self.chain_id, &self.proposer, next, false);
                let why = match outcome {
                    Ok(Outcome::Applied) => None,
                    Ok(Outcome::Failed(r)) => Some(format!("dropped from the mempool: {r}")),
                    Err(e) => Some(format!("dropped from the mempool: {e}")),
                };
                if why.is_some() {
                    self.state.rollback(j);
                }
                why
            };
            if let Some(why) = why {
                // Unsigned ghost txs have no sender whose later txs would wait on this one.
                if !stale && !tx.is_unsigned() {
                    broken.insert(tx.from.clone());
                }
                dropped.push((p.id.clone(), why));
            }
        }
        (next, dropped)
    }
}

/// Most senders whose simulated state admission keeps (the most recently active ones). Each is
/// a copy of the state sharing everything but the records its sender's queued txs touched, so
/// this bounds the cache's memory; a sender past it is simulated again from its queue.
pub const MAX_SENDER_SIMS: usize = 128;

/// Admission's per-sender simulations ([`Chain::submit_tx`]), valid until the next block.
#[derive(Default)]
struct Sims {
    by_sender: HashMap<String, SenderSim>,
    /// The executed height they were built on.
    height: u64,
    clock: u64,
    /// Queued txs re-applied to build simulations (cache misses), for tests.
    rebuilt: u64,
}

/// A copy of the state with one sender's queued txs applied in nonce order (O(1) to take: the
/// maps share structure).
struct SenderSim {
    state: State,
    /// How many of the sender's queued txs `state` has applied.
    applied: usize,
    used: u64,
}

impl Sims {
    /// The sender's simulation, if it's current: built at `height` over exactly its `queued` txs.
    fn take(&mut self, sender: &str, height: u64, queued: usize) -> Option<SenderSim> {
        if self.height != height {
            self.by_sender.clear();
            self.height = height;
        }
        self.by_sender.remove(sender).filter(|s| s.applied == queued)
    }

    /// Keeps a simulation, dropping the least recently used one if the cache is full.
    fn keep(&mut self, sender: String, mut sim: SenderSim) {
        if self.by_sender.len() >= MAX_SENDER_SIMS && !self.by_sender.contains_key(&sender) {
            if let Some(oldest) = self.by_sender.iter().min_by_key(|(_, s)| s.used).map(|(k, _)| k.clone()) {
                self.by_sender.remove(&oldest);
            }
        }
        self.clock += 1;
        sim.used = self.clock;
        self.by_sender.insert(sender, sim);
    }

    fn forget(&mut self, sender: &str) {
        self.by_sender.remove(sender);
    }

    fn clear(&mut self) {
        self.by_sender.clear();
    }
}

/// What a mempool dry run left to keep: the sender's simulation with the new tx applied, and the
/// journal that takes it back out.
type Simulated = Option<(SenderSim, Journal)>;

/// The certified snapshot a fast-synced node started from (`base.json` + `base.state.zst`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Base {
    pub height: u64,
    pub hash: String,
    pub state_root: String,
    /// Hash of the block before the current epoch's first block, when that's before `height`
    /// (consensus anchors each epoch's engine there), certified by the manifest's anchor.
    #[serde(default)]
    pub epoch_start: Option<(u64, String)>,
}

/// A finalized block handed to `Chain::execute`, straight from consensus.
pub struct Finalized {
    pub height: u64,
    pub hash: String,
    pub parent: String,
    pub timestamp_ms: u64,
    /// The consensus view it was proposed in, its parent's view, and its protocol version.
    pub view: u64,
    pub parent_view: u64,
    pub version: u32,
    /// Newest protocol version the proposer's binary supports (display only).
    pub signal: u32,
    pub proposer: String,
    pub txs: Vec<Tx>,
    /// The exact consensus block bytes (stored so this node can serve `/api/sync`).
    pub cblock: Vec<u8>,
    /// Encoded finalization certificate, when we have it.
    pub certificate: Option<Vec<u8>>,
}

/// Writes a secret file readable only by its owner, refusing to overwrite (no chmod race).
pub fn write_secret(path: &Path, contents: &str) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// Where `open` gets its state from, and where replay starts.
struct Start {
    state: State,
    point: CommitPoint,
    snap: Arc<Snap>,
    epochs: crate::epochs::EpochRegistry,
    /// The epoch registry comes from replaying (it's merged with `epochs.json` afterwards).
    rebuilt_epochs: bool,
}

/// Feeds every record of `state` to a store sink, stopping at the first error.
fn state_records(state: &State) -> impl FnMut(store::RecordSink) -> Result<()> + '_ {
    move |sink| {
        let mut err = Ok(());
        state.for_each_record(|n, k, v| {
            if err.is_ok() {
                err = sink(n, k, v);
            }
        });
        err
    }
}

impl Chain {
    /// Creates a new chain. With one validator, `home` gets genesis.json + validator.key.
    /// With N > 1, `home/v0 … home/v{N-1}` each get the shared genesis and their own key,
    /// with localhost ports picked automatically.
    pub fn summon(home: &Path, o: &SummonOpts) -> Result<Vec<PathBuf>> {
        let (chain_id, faucet, block_time_ms, validators, http_port) = (o.chain_id.as_str(), o.faucet, o.block_time_ms, o.validators, o.http_port);
        let prefix = o.address_prefix.as_str();
        if o.epoch_blocks < 2 {
            bail!("epoch_blocks must be at least 2");
        }
        if !crypto::is_prefix(prefix) {
            bail!("address prefix must be one of {:?}", crypto::KNOWN_PREFIXES);
        }
        for a in &o.admins {
            if crypto::parse_address(a).map(|(p, _)| p) != Some(prefix) {
                bail!("admin {a} is not a lowercase {prefix}1… address");
            }
        }
        if validators == 0 {
            bail!("need at least one validator");
        }
        if let Some(t) = &o.contribution {
            t.check()?;
        }
        let keys: Vec<Keys> = (0..validators).map(|_| Keys::generate()).collect();
        let mut infos = vec![];
        for (i, k) in keys.iter().enumerate() {
            let http = match (i, http_port) {
                (0, Some(p)) => p,
                _ => free_port()?,
            };
            infos.push(ValidatorInfo { address: k.address_with(prefix), public_key: hex::encode(k.public_key()), p2p_addr: format!("127.0.0.1:{}", free_port()?), http_addr: format!("127.0.0.1:{http}") });
        }
        let per_validator = 1_000_000_000 * DROPS_PER_FANG / validators as u64;
        let mut allocations: Vec<Allocation> = keys.iter().map(|k| Allocation { address: k.address_with(prefix), amount: per_validator, vesting: None, label: None }).collect();
        allocations.push(Allocation { address: treasury(prefix), amount: 100_000_000 * DROPS_PER_FANG, vesting: None, label: Some("treasury".into()) });
        let validator_admins = if o.admins.is_empty() { vec![keys[0].address_with(prefix)] } else { o.admins.clone() };
        let mut param_overrides = o.params.clone();
        match o.min_bond {
            Some(b) => {
                param_overrides.insert("min_self_bond".into(), b);
            }
            None if faucet => {
                param_overrides.entry("min_self_bond".into()).or_insert(50 * DROPS_PER_FANG);
            }
            None => {}
        }
        let mut genesis = Genesis {
            chain_id: chain_id.to_string(),
            token: "FANG".into(),
            decimals: DECIMALS,
            block_time_ms,
            validators: infos,
            faucet,
            created_at_ms: now_ms(),
            allocations,
            epoch_blocks: o.epoch_blocks,
            validator_admins,
            contribution: o.contribution.clone(),
            address_prefix: prefix.into(),
            param_overrides,
            operators: vec![],
            reserved_names: vec![],
            soul_ledger: vec![],
            features: vec![],
            upgrades: vec![],
            bootstrap_excluded: vec![],
            pool_repos: Default::default(),
        };
        // Every validator stakes: each owns itself and bonds twice the minimum from its
        // allocation, so a light slash (downtime) doesn't leave it under-bonded.
        let self_bond = genesis.chain_params()?.min_self_bond.saturating_mul(2);
        genesis.operators = keys
            .iter()
            .zip(&genesis.validators)
            .map(|(k, v)| Operator { owner: v.address.clone(), consensus_key: v.public_key.clone(), pop: make_pop(k.seed(), chain_id, &v.address, &v.p2p_addr, &v.http_addr), p2p_addr: v.p2p_addr.clone(), http_addr: v.http_addr.clone(), self_bond, commission_bps: 0, provider: None, asn: None, country: None, entity: None })
            .collect();
        genesis.check()?;
        let homes: Vec<PathBuf> = if validators == 1 { vec![home.to_path_buf()] } else { (0..validators).map(|i| home.join(format!("v{i}"))).collect() };
        for (h, k) in homes.iter().zip(&keys) {
            fs::create_dir_all(h)?;
            fs::write(h.join("genesis.json"), serde_json::to_string_pretty(&genesis)?)?;
            write_secret(&h.join("validator.key"), &k.seed_hex())?;
        }
        if validators > 1 {
            fs::write(home.join("genesis.json"), serde_json::to_string_pretty(&genesis)?)?;
        }
        Ok(homes)
    }

    /// Opens a chain home: the state from the database as of its last commit, then the blocks
    /// logged after that commit point (if a crash came between the two) executed again.
    /// Without a usable database (missing, another format, damaged), it is rebuilt by
    /// replaying the logs from genesis, `base.state.zst` or a local snapshot. `validator.key`
    /// is optional (full nodes don't have one).
    pub fn open(home: &Path) -> Result<Self> {
        Self::open_with(home, &OpenOpts::default())
    }

    pub fn open_with(home: &Path, opts: &OpenOpts) -> Result<Self> {
        let genesis = Genesis::load(&home.join("genesis.json"))?;
        let key_path = home.join("validator.key");
        let keys = if key_path.exists() {
            let seed = hex::decode(fs::read_to_string(&key_path)?.trim())?;
            Some(Keys::from_seed(seed.try_into().map_err(|_| anyhow!("validator.key must be 32 bytes hex"))?))
        } else {
            None
        };
        let role = if keys.is_some() { Role::Validator } else { Role::FullNode };
        if home.join("blocks.jsonl").exists() || home.join("cblocks.bin").exists() {
            bail!("{} was written by an older Blockula (blocks.jsonl, storage layout 3), whose chains this binary can't replay; summon or join the chain again in a fresh home", home.display());
        }
        let base: Option<Base> = match fs::read(home.join("base.json")) {
            Ok(b) => Some(serde_json::from_slice(&b).context("base.json")?),
            Err(_) => None,
        };
        let (blocks, cblocks) = store::open_logs(home, opts.segment_heights)?;
        crate::snapshot::remove_partial(home);
        // An interrupted fast-sync download (it starts over).
        let _ = fs::remove_file(home.join("base.state.zst.part"));
        let genesis_state = genesis.initial_state();
        let genesis_root = genesis_state.root();
        let genesis_hash = hex::encode(crate::consensus::genesis_block(&genesis).digest_bytes());
        let store = Store::open(&home.join(store::DB_FILE))?;
        let mut upgraded_epochs = false;
        let saved_epochs: Option<crate::epochs::EpochRegistry> = fs::read(home.join("epochs.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).map(|mut e: crate::epochs::EpochRegistry| {
            // Proofs signed under an older handoff format no longer verify: dropped, and signed
            // again where the state still holds both sides of the change (below).
            upgraded_epochs = e.upgrade_format();
            e
        });
        let ctx = OpenCtx { home, genesis: &genesis, genesis_hash: &genesis_hash, genesis_root: &genesis_root, base: base.as_ref(), saved_epochs: saved_epochs.as_ref(), blocks: &blocks };
        let start = match ctx.load(&store) {
            Ok(Some(s)) => s,
            Ok(None) => ctx.rebuild(&store, genesis_state)?,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "the state database doesn't match the block logs; rebuilding it");
                ctx.rebuild(&store, genesis_state)?
            }
        };
        let Start { state, point, snap, epochs, rebuilt_epochs } = start;
        let disk = Arc::new(Disk::new(home.to_path_buf(), blocks.reader(), cblocks.reader()));
        let writer = Writer { store: store.clone(), blocks, cblocks, point: point.clone(), files_pruned_below: 0, scratch: false, #[cfg(test)] fail_at: None, #[cfg(test)] delay: None };
        let genesis_arc = Arc::new(genesis.clone());
        let (height, last_hash) = (point.height, point.block_hash.clone());
        let head = match snap.block_meta(height)? {
            Some(m) => disk.load_block(height, &m),
            None => None,
        };
        let view = ReadView::new(state.clone(), height, last_hash.clone(), genesis_arc.clone(), genesis_hash.clone(), genesis_root.clone(), role, base.clone(), point.pruned_below, None, head.clone(), snap.clone(), disk.clone());
        let live = Arc::new(Live::new(view, epochs.clone(), disk.clone()));
        let epochs_saver = EpochsSaver::new(home.join("epochs.json"));
        let halt = crate::upgrade::HaltControl::for_genesis(&genesis);
        let mut chain = Chain {
            home: home.into(),
            genesis,
            genesis_arc,
            keys,
            role,
            state,
            genesis_root,
            genesis_hash,
            height,
            last_hash,
            head_ids: head.as_ref().map(|b| b.txs.iter().map(Tx::id).collect()).unwrap_or_default(),
            head,
            writer: Arc::new(parking_lot::Mutex::new(writer)),
            store,
            point,
            snap,
            disk,
            mempool: vec![],
            lane: HashMap::new(),
            duties: HashMap::new(),
            mempool_ids: vec![],
            pool: Default::default(),
            pending: HashMap::new(),
            sims: Sims::default(),
            mp: MempoolView::default(),
            failed_order: Default::default(),
            finalized_tip: 0,
            gossip: None,
            epochs,
            base,
            claims: HashMap::new(),
            snapshot_busy: Default::default(),
            scratch: false,
            live,
            keep_blocks: None,
            broken: None,
            replayed: 0,
            epochs_saver,
            cert_bytes: CERT_SPACING_BYTES,
            halt,
            #[cfg(test)]
            junk_proposals: false,
            #[cfg(test)]
            wrong_exec_root: false,
        };
        chain.replay_logs()?;
        if rebuilt_epochs {
            if let Some(saved) = &saved_epochs {
                let at = chain.instance();
                chain.epochs.merge_sigs(&at, saved);
            }
            chain.save_epochs();
        }
        chain.reconcile_epochs()?;
        if upgraded_epochs {
            chain.restart_history_after_format_upgrade()?;
        }
        // A restart replays nothing, so the check `execute` runs after every block runs here too: a
        // binary that can't run the next height opens halted and executes nothing more (3.9).
        let _ = crate::upgrade::after_block(&chain, chain.height);
        chain.set_finalized_tip(chain.height());
        chain.publish();
        chain.live.publish_epochs(Arc::new(chain.epochs.clone()));
        Ok(chain)
    }

    /// Executes the blocks logged past the commit point again (a crash came between the logs
    /// and the database), then cuts off anything after the last complete block: a torn entry, or
    /// a consensus block whose executed block never made it to the log. Neither was acknowledged
    /// to consensus, so it delivers them again.
    fn replay_logs(&mut self) -> Result<()> {
        let (mut bend, mut cend) = (self.point.blocks_end, self.point.cblocks_end);
        let (breader, creader) = {
            let w = self.writer.lock();
            (w.blocks.reader(), w.cblocks.reader())
        };
        let mut cit = creader.iter_from(cend);
        let mut cnext = cit.next().transpose()?;
        let mut last_durable = true;
        for e in breader.iter_from(bend) {
            let e = e?;
            let block: Block = serde_json::from_slice(&e.payload).with_context(|| format!("the block log has a bad entry at {:?}", e.pos))?;
            if block.height <= self.height {
                // Below a snapshot this rebuild started from.
                bend = e.pos.end();
                continue;
            }
            // Its consensus block: entries are in height order, like the blocks.
            while cnext.as_ref().is_some_and(|c| c.height < block.height) {
                cend = cnext.as_ref().unwrap().pos.end();
                cnext = cit.next().transpose()?;
            }
            let cpos = match &cnext {
                Some(c) if c.height == block.height => {
                    let p = c.pos;
                    cend = p.end();
                    cnext = cit.next().transpose()?;
                    Some(p)
                }
                _ => None,
            };
            let height = block.height;
            let durable = height.is_multiple_of(REPLAY_SYNC_EVERY);
            let spans = store::tx_spans(&e.payload, &block.txs);
            self.replay(block, e.pos, cpos, spans, durable).with_context(|| format!("replaying block {height} from {}", self.home.join("blocks").display()))?;
            last_durable = durable;
            bend = e.pos.end();
            self.replayed += 1;
        }
        if !last_durable {
            self.store.commit(store::Commit { point: &mut self.point.clone(), records: &[], nodes: &[], block: None, epoch_head: false, cert: None, prune_below: 0, durable: true })?;
        }
        if self.replayed > 0 {
            tracing::info!(blocks = self.replayed, height = self.height, "executed the blocks logged past the state database's commit point");
        }
        let mut w = self.writer.lock();
        w.blocks.truncate(bend)?;
        w.cblocks.truncate(cend)?;
        (w.point.blocks_end, w.point.cblocks_end) = (bend, cend);
        (self.point.blocks_end, self.point.cblocks_end) = (bend, cend);
        Ok(())
    }

    /// Brings the validator-set history up to the next height's epoch from the sets in state
    /// (it can trail when `epochs.json` was saved before a crash), and forgets anything recorded
    /// past it: this node hasn't executed that far, so nothing there is its own (F08; a registry
    /// an older binary adopted whole from a fast-sync peer could run ahead).
    fn reconcile_epochs(&mut self) -> Result<()> {
        let want = self.state.params.epoch_of(self.height + 1);
        if self.epochs.known_through > want {
            tracing::info!(known_through = self.epochs.known_through, executed = want, "dropping validator-set history past the executed epoch");
            self.epochs.truncate(want);
            self.save_epochs();
            return Ok(());
        }
        if self.epochs.known_through == want {
            return Ok(());
        }
        for e in self.epochs.known_through + 1..=want {
            let keys: Vec<String> = self.state.active_set(e).map(|s| s.iter().map(|v| v.consensus_key.clone()).collect()).ok_or_else(|| anyhow!("epochs.json is missing epoch {e}, and the state no longer holds its set"))?;
            self.epochs.record(e, keys);
        }
        self.save_epochs();
        Ok(())
    }

    /// After a handoff-format upgrade dropped the old proofs: validators can sign a change again
    /// only while the state holds the sets on both sides of it (`epoch_sets_kept`), so changes
    /// before the oldest set it holds would stay unprovable for good, holding `proven_through`
    /// back and the handoff exchange polling for them. The history starts at that set instead: a
    /// joiner trusting an older root is told to pass a newer checkpoint (review-fix-sync-4). A
    /// chain launched at the current format never gets here.
    fn restart_history_after_format_upgrade(&mut self) -> Result<()> {
        let Some(oldest) = self.state.active_sets.keys().next().copied() else { return Ok(()) };
        let base = self.epochs.base;
        self.epochs.rebase(oldest);
        if self.epochs.base != base {
            tracing::warn!(from = base, to = oldest, "handoff proofs of an older format were dropped; the validator-set history now starts at the oldest set the state holds (joiners trusting an older root need a newer --checkpoint)");
        }
        // Written now, at the new format, so the next open doesn't take it for an old one.
        self.try_save_epochs()
    }

    /// The validator's own address (fee recipient, faucet account).
    pub fn me(&self) -> Option<String> {
        self.keys.as_ref().map(|k| k.address_with(self.state.params.prefix()))
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    /// What readers see: the state, history and indexes as of the last executed block.
    pub fn view(&self) -> Arc<ReadView> {
        self.live.view()
    }

    /// Makes this a scratch home (`blockula replay`'s): blocks are logged and committed as a
    /// node's are, but fsynced and committed durably only every [`SCRATCH_SYNC_EVERY`] heights (a
    /// crash may lose the blocks since), and it takes no state snapshots: nothing fast-syncs from
    /// it.
    pub fn set_scratch(&mut self) {
        self.scratch = true;
        self.writer.lock().scratch = true;
    }

    /// Keeps only the last `keep` blocks on disk (`None`: keep everything). Never fewer than
    /// [`min_keep_blocks`].
    pub fn set_pruning(&mut self, keep: Option<u64>) {
        self.keep_blocks = keep.map(|k| k.max(min_keep_blocks(&self.state)));
        self.publish();
    }

    #[cfg(test)]
    pub fn keep_blocks(&self) -> Option<u64> {
        self.keep_blocks
    }

    /// Records a peer's claimed tip (display only).
    pub fn note_claim(&mut self, peer: &str, tip: u64) {
        self.claims.insert(peer.to_string(), tip);
        let mut v: Vec<u64> = self.claims.values().copied().collect();
        v.sort_unstable();
        let median = if v.is_empty() { 0 } else { v[(v.len() - 1) / 2] };
        self.live.claimed.store(median, std::sync::atomic::Ordering::Relaxed);
    }

    /// The network height peers claim: the median of their latest claims, so one lying peer
    /// can't move it. Display-only (status/health read it through `Live`); verified heights come
    /// from certificates. Slashing protection's restart watch (`node::watch_network`) reads it
    /// only as a sign the network moved on, which keeps the escape shut (fail-safe), never opens it.
    pub fn network_height_claimed(&self) -> u64 {
        self.live.claimed.load(std::sync::atomic::Ordering::Relaxed).max(self.finalized_tip).max(self.height)
    }

    /// Behind the network by more than a few blocks, by the finalized height this node verified
    /// (certificates, marshal's tips: [`Chain::set_finalized_tip`]). Validator duties (beacon,
    /// evidence, attestations) wait while it's true, so only verified heights may gate them:
    /// peers' claims are display-only (`/api/status`, `/api/health`), or a majority of lying
    /// HTTP peers could keep a validator from revealing until it's jailed (audit F11).
    pub fn syncing(&self) -> bool {
        self.height + SYNC_SLACK < self.finalized_tip
    }

    /// A finalized height consensus told us about (certificates, marshal tips).
    pub fn set_finalized_tip(&mut self, height: u64) {
        self.finalized_tip = self.finalized_tip.max(height);
        self.live.finalized_tip.fetch_max(self.finalized_tip, std::sync::atomic::Ordering::Relaxed);
    }

    /// Height of the snapshot this node fast-synced from (0 = full history).
    #[cfg(test)]
    pub fn base_height(&self) -> u64 {
        self.base.as_ref().map(|b| b.height).unwrap_or(0)
    }

    pub fn last_hash(&self) -> &str {
        &self.last_hash
    }

    pub fn last_block(&self) -> Option<&Block> {
        self.head.as_deref()
    }

    /// This node's newest executed block for sync trust (`checkpoints::Trust::stale`): its height
    /// and, when the node holds it (not at a fast-sync base it hasn't built on yet), its time.
    pub fn head_time(&self) -> (u64, Option<u64>) {
        (self.height, self.head.as_ref().filter(|b| b.height == self.height).map(|b| b.timestamp_ms))
    }

    // Tests read through the chain; the API reads the published view directly.

    /// Executed block at `height`, if this node holds it.
    #[cfg(test)]
    pub fn block(&self, height: u64) -> Option<Block> {
        self.view().block(height).map(|b| (*b).clone())
    }

    /// State root after executing `height` (0 = genesis). The latest block's comes from memory
    /// (it may still be being written while readers see the previous one).
    pub fn root_at(&self, height: u64) -> Option<String> {
        if height == self.height {
            return Some(self.state.root());
        }
        self.view().root_at(height)
    }

    /// Hash of the executed block at `height`, including the fast-sync base, the anchor of the
    /// epoch we fast-synced into, and epochs' last blocks (kept through pruning).
    pub fn hash_at(&self, height: u64) -> Option<String> {
        if height == self.height {
            return Some(self.last_hash.clone());
        }
        self.view().hash_at(height)
    }

    /// Exact consensus block bytes at `height`.
    #[cfg(test)]
    pub fn cblock(&self, height: u64) -> Option<Vec<u8>> {
        self.view().cblock(height)
    }

    /// Encoded finalization certificate for `height`, if stored.
    pub fn cert(&self, height: u64) -> Option<Vec<u8>> {
        self.view().cert(height)
    }

    #[cfg(test)]
    pub fn cert_len(&self, height: u64) -> usize {
        self.view().cert_len(height)
    }

    /// Whether a tx is finalized (in a block this node still indexes).
    pub fn has_tx(&self, id: &str) -> bool {
        self.tx_location(id).is_some()
    }

    /// Where a finalized tx is: (height, index in the block).
    pub fn tx_location(&self, id: &str) -> Option<(u64, usize)> {
        if self.head_ids.contains(id) {
            let b = self.head.as_ref()?;
            return Some((b.height, b.txs.iter().position(|t| t.id() == id)?));
        }
        self.snap.tx(&hash32(id).ok()?).ok()?
    }

    /// Keeps a certificate that arrived after its block, if its block is one whose certificate
    /// stays on disk (an epoch's last block, or a checkpoint still in the window) and it isn't
    /// stored yet: execution stores the certificates consensus hands it, and this fills in what
    /// it missed (`consensus::fill_late_certs` reads them from marshal), so the epoch anchor of a
    /// fast-sync manifest stays servable. `digest` must be the executed block's hash. The
    /// database write holds the writer's lock, not the chain's. Returns whether it stored it.
    pub fn store_late_cert(chain: &parking_lot::Mutex<Chain>, height: u64, digest: &str, cert: Vec<u8>) -> Result<bool> {
        let (store, writer) = {
            let c = chain.lock();
            let kept = c.state.params.is_boundary(height) || (height.is_multiple_of(CERT_CHECKPOINT) && height >= c.point.pruned_below);
            if height > c.height() || !kept || c.view().cert(height).is_some() {
                return Ok(false);
            }
            ensure!(c.hash_at(height).as_deref() == Some(digest), "the certificate at #{height} is for another block");
            (c.store.clone(), c.writer.clone())
        };
        {
            let _commits = writer.lock();
            store.put_cert(height, &cert)?;
        }
        chain.lock().disk.remember_cert(height, cert);
        Ok(true)
    }

    /// Keeps a certificate that arrived after its block: in memory for recent heights, on disk at
    /// checkpoints and epoch ends. (Execution stores the certificates consensus hands it.)
    #[cfg(test)]
    pub fn store_cert(&mut self, height: u64, cert: Vec<u8>) -> Result<()> {
        let boundary = self.state.params.is_boundary(height);
        if (height.is_multiple_of(CERT_CHECKPOINT) && height >= self.point.pruned_below) || boundary {
            self.store.put_cert(height, &cert)?;
        }
        self.disk.remember_cert(height, cert);
        self.set_finalized_tip(height);
        Ok(())
    }

    /// Index rows for an executed block: its tx ids and the accounts whose history it joins (the
    /// sender, and every account the action names: recipients, validators, the accounts a
    /// proposal or a multisig proposal names; see [`Action::addresses`]).
    /// `named[i]`: the accounts tx `i` named, resolved just before it ran ([`State::named_by`]).
    fn block_meta(&self, block: &Block, named: &[Vec<String>], at: Pos, cblock: Option<Pos>) -> Result<BlockMeta> {
        if named.len() != block.txs.len() {
            bail!("block {}: {} txs but {} resolutions of what they named", block.height, block.txs.len(), named.len());
        }
        let mut txs = Vec::with_capacity(block.txs.len());
        let mut history = vec![];
        for (i, (tx, named)) in block.txs.iter().zip(named).enumerate() {
            txs.push(hash32(&tx.id())?);
            let mut joined: Vec<String> = tx.sender().ok().into_iter().collect();
            for a in named {
                if !joined.contains(a) {
                    joined.push(a.clone());
                }
            }
            history.extend(joined.into_iter().map(|a| (i as u32, a)));
        }
        Ok(BlockMeta {
            hash: hash32(&block.hash)?,
            prev_hash: hash32(&block.prev_hash)?,
            state_root: hash32(&block.state_root)?,
            timestamp_ms: block.timestamp_ms,
            proposer: block.proposer.clone(),
            tx_count: block.txs.len() as u32,
            failed_count: block.failed.len() as u32,
            block: at,
            cblock,
            txs,
            tx_spans: vec![],
            failed: block.failed.iter().map(|(i, r)| (*i as u32, r.clone())).collect(),
            history,
            version: block.version,
            signal: block.signal,
            base_fee: block.base_fee,
        })
    }

    /// Everything the commit of `block` writes, as of the state now in memory (the writer fills
    /// in where the block lands in the logs).
    fn prepare_commit(&self, block: Arc<Block>, named: &[Vec<String>], cblock: Option<Vec<u8>>, records: Vec<RecordChange>, cert: Option<Vec<u8>>) -> Result<Executed> {
        let meta = self.block_meta(&block, named, Pos::default(), None)?;
        let epoch_head = self.state.params.is_boundary(block.height);
        let spaced = self.cert_bytes.saturating_add(cblock.as_ref().map_or(0, |b| b.len() as u64)) >= CERT_SPACING_BYTES;
        let cert = cert.filter(|_| block.height.is_multiple_of(CERT_CHECKPOINT) || epoch_head || spaced);
        // Never below the minimum as of this block (governance may have widened the tx window).
        let prune_below = self.keep_blocks.map(|k| (block.height + 1).saturating_sub(k.max(min_keep_blocks(&self.state)))).unwrap_or(0);
        Ok(Executed { meta, root: self.state.root_hash(), root_child: self.state.tree.root_child().encode(), nodes: self.state.tree.pending_changes(), records, cert, epoch_head, prune_below, cblock, block })
    }

    /// The executed block becomes the head (readers keep seeing the previous one until `finish`).
    fn advance(&mut self, block: &Arc<Block>) {
        self.height = block.height;
        self.last_hash = block.hash.clone();
        self.head_ids = block.txs.iter().map(Tx::id).collect();
        self.head = Some(block.clone());
    }

    /// The tree moves onto the database snapshot that now holds its nodes.
    fn adopt(&mut self, snap: Arc<Snap>, point: CommitPoint) {
        self.state.tree.rebase(snap.clone());
        self.snap = snap;
        self.point = point;
    }

    fn replay(&mut self, block: Block, at: Pos, cblock: Option<Pos>, tx_spans: Vec<(u32, u32)>, durable: bool) -> Result<()> {
        if block.height != self.height() + 1 || block.prev_hash != self.last_hash() {
            bail!("the block log is not contiguous at height {}", block.height);
        }
        // A block this binary can't run was logged by a newer one (3.9).
        crate::upgrade::check_next_with(&self.state, block.height, self.halt.supported)?;
        let version = crate::upgrade::version_at(&self.state, block.height);
        let mut named = Vec::with_capacity(block.txs.len());
        for (i, tx) in block.txs.iter().enumerate() {
            named.push(self.state.named_by(tx));
            let outcome = self.state.apply(tx, &self.genesis.chain_id, &block.proposer, block.height).with_context(|| format!("replaying block {}", block.height))?;
            check_replayed(block.height, i, &outcome, block.failed.get(&i))?;
        }
        let tx_bytes = block.txs.iter().map(Tx::size).chain(block.skipped.iter().copied()).sum();
        self.state.end_block(&BlockInfo { height: block.height, version, hash: &block.hash, proposer: &block.proposer, view: block.view, parent_view: block.parent_view, skipped: &block.skipped, tx_bytes });
        if self.state.params.is_boundary(block.height) {
            self.record_epoch(block.height);
        }
        let commit = self.state.commit_block()?;
        let root = hex::encode(commit.root);
        #[cfg(test)]
        self.state.debug_check(block.height);
        if root != block.state_root {
            bail!("state root mismatch at height {}", block.height);
        }
        let height = block.height;
        let mut e = self.prepare_commit(Arc::new(block), &named, None, commit.records, None)?;
        (e.meta.block, e.meta.cblock, e.meta.tx_spans) = (at, cblock, tx_spans);
        let (snap, point) = self.writer.lock().commit(&e, durable)?;
        self.advance(&e.block);
        self.adopt(snap, point);
        let _ = height;
        Ok(())
    }

    /// Remembers why a tx failed: `ran` when it executed in a block (charged), else it was dropped
    /// from the mempool unrun (`/api/tx` says which).
    fn note_failed_as(&mut self, id: String, reason: String, height: u64, ran: bool) {
        if !self.mp.has_failure(&id) {
            self.failed_order.push_back(id.clone());
        }
        self.mp.fail(id, reason, height, ran);
        while self.failed_order.len() > RECENT_FAILED {
            if let Some(old) = self.failed_order.pop_front() {
                self.mp.unfail(&old);
            }
        }
    }

    /// A tx dropped from the mempool without running (evicted, replaced, not included, expired…).
    fn note_failed(&mut self, id: String, reason: String, height: u64) {
        self.note_failed_as(id, reason, height, false);
    }

    /// Why a tx failed or was dropped, if we remember.
    #[cfg(test)]
    pub fn failure(&self, id: &str) -> Option<&(String, u64)> {
        self.mp.failure(id)
    }

    /// Executes a finalized block: applies its txs in order (invalid ones are skipped; ones whose
    /// action fails are charged their fee), persists it (block logs, then the state database),
    /// then prunes the mempool and publishes the new view. Returning `Ok` acknowledges the block.
    pub fn execute(&mut self, f: Finalized) -> Result<()> {
        let height = f.height;
        let mut e = self.execute_in_memory(f)?;
        let written = self.writer.clone().lock().persist(&mut e);
        self.finish(e, written)?;
        self.revalidate_mempool();
        // The last step: the block is executed and stored, but if the node must stop here (it
        // can't run the next height, or reached --halt-height) the error leaves the block
        // unacknowledged, so consensus stops delivering; the node then exits or serves reads.
        crate::upgrade::after_block(self, height)
    }

    /// [`Chain::execute`] for a chain that consensus and readers share: the chain lock is held
    /// only while the block is applied in memory and adopted. Writing and fsyncing the logs, the
    /// database commit and the mempool check run without it (SF-4). One executor at a time.
    pub fn execute_shared(chain: &parking_lot::Mutex<Chain>, f: Finalized) -> Result<()> {
        let (mut e, writer) = {
            let mut c = chain.lock();
            (c.execute_in_memory(f)?, c.writer.clone())
        };
        let height = e.block.height;
        let written = writer.lock().persist(&mut e);
        chain.lock().finish(e, written)?;
        let check = chain.lock().mempool_check();
        let dropped = check.run();
        let mut c = chain.lock();
        c.drop_from_mempool(dropped);
        // As in `execute`: the block is stored, but the node may have to stop here.
        crate::upgrade::after_block(&c, height)
    }

    /// Applies a finalized block to the state in memory and prepares its commit. From here until
    /// `finish`, memory is ahead of the disk: a failure leaves this instance unusable until a
    /// restart (which recovers from the logs).
    fn execute_in_memory(&mut self, f: Finalized) -> Result<Executed> {
        if let Some(why) = &self.broken {
            bail!("a block failed to persist ({why}); restart the node to recover from its logs");
        }
        // Nothing executes past a halt, --halt-height or a version this binary doesn't run.
        crate::upgrade::before_block(self, f.height)?;
        if f.height != self.height() + 1 {
            bail!("expected block {}, got {}", self.height() + 1, f.height);
        }
        if f.parent != self.last_hash() {
            bail!("block {} does not extend our chain", f.height);
        }
        let height = f.height;
        let result = self.apply_block(f);
        if let Err(e) = &result {
            self.broken = Some(format!("block {height}: {e:#}"));
        }
        result
    }

    fn apply_block(&mut self, f: Finalized) -> Result<Executed> {
        let mut txs = vec![];
        let mut named = vec![];
        let mut failed = BTreeMap::new();
        let mut skipped = vec![];
        // What this height's txs pay, fixed before the block's own end-of-block steps run.
        let base_fee = crate::econ::slot(&self.state, f.height);
        let mut results = Vec::with_capacity(f.txs.len());
        let chain_id = self.genesis.chain_id.clone();
        self.state.run_block(&chain_id, f.height, &f.hash, &f.proposer, f.view, f.parent_view, &f.txs, |_, n, r| results.push((n, r)));
        for (tx, (n, r)) in f.txs.into_iter().zip(results) {
            match r {
                Ok(Outcome::Applied) => {
                    txs.push(tx);
                    named.push(n);
                }
                Ok(Outcome::Failed(r)) => {
                    failed.insert(txs.len(), r.clone());
                    self.note_failed_as(tx.id(), r, f.height, true);
                    txs.push(tx);
                    named.push(n);
                }
                Err(e) => {
                    skipped.push(tx.size());
                    self.note_failed(tx.id(), format!("not included: {e}"), f.height);
                }
            }
        }
        if self.state.params.is_boundary(f.height) && self.record_epoch(f.height) {
            self.sign_handoffs();
        }
        let commit = self.state.commit_block()?;
        let state_root = hex::encode(commit.root);
        #[cfg(test)]
        self.state.debug_check(f.height);
        let block = Arc::new(Block { height: f.height, prev_hash: f.parent, timestamp_ms: f.timestamp_ms, view: f.view, parent_view: f.parent_view, version: f.version, signal: f.signal, proposer: f.proposer, state_root, txs, hash: f.hash, signature: String::new(), failed, skipped, base_fee });
        if let Some(cert) = &f.certificate {
            self.disk.remember_cert(f.height, cert.clone());
        }
        let cblock_bytes = f.cblock.len() as u64;
        let e = self.prepare_commit(block, &named, Some(f.cblock), commit.records, f.certificate)?;
        self.cert_bytes = if e.cert.is_some() { 0 } else { self.cert_bytes.saturating_add(cblock_bytes) };
        self.advance(&e.block);
        Ok(e)
    }

    /// After the writer committed the block: the tree adopts the new database snapshot, readers
    /// get the new view, the mempool sheds the block's txs, and checkpoints get a snapshot.
    fn finish(&mut self, e: Executed, written: Result<(Arc<Snap>, CommitPoint)>) -> Result<()> {
        let (snap, point) = match written {
            Ok(w) if self.height == e.block.height => w,
            Ok(_) => {
                self.broken = Some(format!("block {} was written after the chain moved on", e.block.height));
                bail!("block {} was written after the chain moved on", e.block.height);
            }
            Err(err) => {
                self.broken = Some(format!("block {}: {err:#}", e.block.height));
                return Err(err);
            }
        };
        self.adopt(snap, point);
        self.disk.cache_block(e.block.clone(), e.meta.block.len as usize);
        self.set_finalized_tip(e.block.height);
        if crate::snapshot::is_checkpoint(e.block.height) && !self.scratch {
            self.take_snapshot();
        }
        self.drop_included();
        self.publish();
        Ok(())
    }

    /// Publishes the current state, head and mempool to readers.
    fn publish(&mut self) {
        let view = ReadView::new(self.state.clone(), self.height, self.last_hash.clone(), self.genesis_arc.clone(), self.genesis_hash.clone(), self.genesis_root.clone(), self.role, self.base.clone(), self.point.pruned_below, self.keep_blocks, self.head.clone(), self.snap.clone(), self.disk.clone());
        self.live.publish(view);
        self.publish_mempool();
    }

    fn publish_mempool(&self) {
        self.live.publish_mempool(self.mp.clone());
    }

    /// After executing an epoch's last block: remember the next epoch's set (saved every epoch,
    /// so `epochs.json` keeps up with the state database without a replay). Returns whether it
    /// changed (then the outgoing validators sign a handoff).
    fn record_epoch(&mut self, boundary: u64) -> bool {
        let next = self.state.params.epoch_of(boundary) + 1;
        let keys: Vec<String> = self.state.active_set(next).map(|s| s.iter().map(|v| v.consensus_key.clone()).collect()).unwrap_or_default();
        let changed = self.epochs.record(next, keys);
        // Older than the window a joiner may start from is dropped once the history is big (F10).
        self.epochs.trim(next.saturating_sub(registry_keep_epochs(&self.genesis, &self.state)), crate::epochs::KEEP_BYTES);
        self.save_epochs();
        changed
    }

    /// The sorted consensus keys this node's state holds for `epoch` (executed, or certified by
    /// the snapshot it fast-synced from).
    fn executed_set(&self, epoch: u64) -> Option<Vec<String>> {
        let mut keys: Vec<String> = self.state.active_set(epoch)?.iter().map(|v| v.consensus_key.clone()).collect();
        keys.sort();
        Some(keys)
    }

    /// The chain instance handoffs are signed for (its id and genesis hash, F02).
    pub fn instance(&self) -> crate::epochs::Instance {
        crate::epochs::Instance::new(&self.genesis.chain_id, crate::consensus::hex_32(&self.genesis_hash))
    }

    /// Signs handoffs for set changes this validator presided over (also retroactively), but only
    /// those its own state shows on both sides: never one it learned from a peer (F08).
    pub fn sign_handoffs(&mut self) -> usize {
        let Some(seed) = self.keys.as_ref().map(|k| k.seed()) else { return 0 };
        let at = self.instance();
        let mut epochs = std::mem::take(&mut self.epochs);
        let n = epochs.sign_missing(&at, seed, |e| self.executed_set(e)).len();
        self.epochs = epochs;
        if n > 0 {
            self.save_epochs();
        }
        n
    }

    /// Adds handoff signatures verified elsewhere (`epochs::verify_candidates`).
    pub fn add_handoffs(&mut self, verified: Vec<crate::epochs::Candidate>) -> usize {
        let at = self.instance();
        let n = self.epochs.add_verified(&at, verified);
        if n > 0 {
            self.save_epochs();
        }
        n
    }

    /// Publishes the registry and writes `epochs.json` in the background (durably, newest wins). A
    /// failed or lost write only means a restart rebuilds the sets from state and collects the
    /// proofs again.
    fn save_epochs(&self) {
        let e = Arc::new(self.epochs.clone());
        self.live.publish_epochs(e.clone());
        self.epochs_saver.save(e);
    }

    /// [`Chain::save_epochs`], written before it returns.
    fn try_save_epochs(&self) -> Result<()> {
        let e = Arc::new(self.epochs.clone());
        self.epochs_saver.save_now(&e)?;
        self.live.publish_epochs(e);
        Ok(())
    }

    /// Writes a snapshot of the current state in the background.
    fn take_snapshot(&self) {
        use std::sync::atomic::Ordering;
        if self.snapshot_busy.swap(true, Ordering::AcqRel) {
            tracing::warn!(height = self.height, "skipping a snapshot: the previous one is still being written");
            return;
        }
        // The records are all the writer reads; without the tree the clone doesn't pin a
        // database snapshot for as long as the writer runs.
        let mut state = self.state.clone();
        state.tree = Default::default();
        let (home, height, root, busy) = (self.home.clone(), self.height, self.state.root(), self.snapshot_busy.clone());
        std::thread::spawn(move || {
            if let Err(e) = crate::snapshot::write(&home, height, &root, &state) {
                tracing::warn!(height, error = %e, "snapshot failed");
            }
            busy.store(false, Ordering::Release);
        });
    }

    /// Installs a verified snapshot into an empty home, then executes the manifest's blocks.
    /// `packed` (the verified chunks, concatenated) becomes `base.state.zst`; `state` is what
    /// it decodes to (root already checked). Each file is in place durably before the next, and
    /// `base.json` (the switch) comes last but one: a crash before it leaves a genesis home, a
    /// crash after it a fast-synced home whose database is rebuilt from `base.state.zst`.
    pub fn install_snapshot(&mut self, v: &crate::snapshot::Verified, state: State, packed: &Path) -> Result<()> {
        if self.height != 0 || self.base.is_some() {
            bail!("fast sync needs an empty home");
        }
        let base_hash = hex::encode(v.blocks[0].parent_bytes());
        // The verified history (from the trusted root through the certified epoch, nothing past
        // it: F08) must agree with the sets the certified state holds.
        let mut epochs = v.epochs.clone();
        for (&e, addrs) in state.active_sets.range(epochs.base..=epochs.known_through) {
            let mut keys: Vec<String> = addrs.iter().filter_map(|a| state.validators.get(a)).map(|r| r.consensus_key.clone()).collect();
            keys.sort();
            ensure!(epochs.set(e) == Some(&keys), "the snapshot's state holds another validator set for epoch {e} than its verified history");
        }
        epochs.upgrade_format();
        store::install_durably(packed, &self.home.join("base.state.zst"))?;
        self.epochs = epochs;
        self.try_save_epochs()?;
        // The anchor's proof, for this node's own manifests (P-SYNC).
        crate::snapshot::save_anchor(&self.home, v.anchor.as_ref())?;
        let base = Base { height: v.height, hash: base_hash.clone(), state_root: v.state_root.clone(), epoch_start: v.epoch_start.clone() };
        store::write_durably(&self.home.join("base.json"), &serde_json::to_vec_pretty(&base)?)?;
        let mut w = self.writer.lock();
        let start = (v.height + 1, w.blocks.reader().seg_of(v.height + 1));
        let mut point = CommitPoint { height: v.height, block_hash: base_hash.clone(), root: state.root_hash(), root_child: state.tree.root_child().encode(), base: v.height, pruned_below: start.0, blocks_end: (start.1, 0), cblocks_end: (start.1, 0), ..self.point.clone() };
        self.store.reset(&mut point, &mut state_records(&state), &state.tree.pending_changes())?;
        w.point = point.clone();
        drop(w);
        self.state = state;
        self.adopt(Arc::new(self.store.snapshot()?), point);
        self.height = v.height;
        self.last_hash = base_hash;
        self.head = None;
        self.head_ids.clear();
        self.base = Some(base);
        self.set_finalized_tip(v.height);
        self.publish();
        let last = v.blocks.len() - 1;
        for (i, b) in v.blocks.iter().enumerate() {
            let cert = (i == last).then(|| v.finalization.clone());
            self.execute(crate::consensus::finalized_from(b, cert))?;
        }
        Ok(())
    }

    fn fee_sink(&self) -> String {
        self.me().unwrap_or_else(|| self.genesis.validators[0].address.clone())
    }

    fn rebuild_pending(&mut self) {
        self.pending.clear();
        // Unsigned ghost txs have no sender (or nonce) to queue under.
        for (i, tx) in self.mempool.iter().enumerate().filter(|(_, t)| !t.is_unsigned()) {
            self.pending.entry(tx.from.clone()).or_default().push(i);
        }
        self.rebuild_lane();
    }

    /// Every removal from the mempool ends here (through [`Chain::rebuild_pending`]): the lane
    /// forgets what left, and duties that waited for a slot move up into it. The lane is a prefix
    /// of each sender's queue (review of F32): a duty queued behind any other tx of its sender
    /// isn't in it, so a duty can't shield the txs ahead of it from eviction (only a sender's
    /// last queued tx can go, and the lane's never do), nor wait behind them in proposals (which
    /// rank each sender's queue by its next tx). Lane txs stay in it: removals take a sender's
    /// head (included), its tail (evicted) or everything from a nonce on (dropped), so what's
    /// left of a prefix is still one. Then, in arrival order, each sender's next txs join while
    /// they're duties whose validator has a slot free for their kind: a duty that arrived
    /// behind the sender's other txs, or past its validator's slots, moves up once those are
    /// included (a block later, under any load), instead of waiting behind the whole backlog.
    fn rebuild_lane(&mut self) {
        let pool = &self.pool;
        self.duties.retain(|id, _| pool.contains(id));
        self.lane.retain(|id, _| pool.contains(id));
        let mut used: HashMap<(String, &'static str), usize> = HashMap::new();
        let mut out = vec![];
        for q in self.pending.values() {
            let prefix = q.iter().take_while(|&&i| self.lane.contains_key(&self.mempool_ids[i])).count();
            for &i in &q[..prefix] {
                *used.entry(self.lane[&self.mempool_ids[i]].clone()).or_default() += 1;
            }
            // Never expected (see above), but a lane tx behind a tx outside it would shield it.
            out.extend(q[prefix..].iter().map(|&i| self.mempool_ids[i].clone()).filter(|id| self.lane.contains_key(id)));
        }
        for id in out {
            self.lane.remove(&id);
        }
        let mut closed: std::collections::HashSet<&str> = Default::default();
        let mut joined = vec![];
        for (t, id) in self.mempool.iter().zip(&self.mempool_ids).filter(|(t, _)| !t.is_unsigned()) {
            if closed.contains(t.from.as_str()) || self.lane.contains_key(id) {
                continue;
            }
            match self.duties.get(id) {
                Some(d) if used.get(d).copied().unwrap_or(0) < DUTY_LANE_PER_KIND => {
                    *used.entry(d.clone()).or_default() += 1;
                    joined.push((id.clone(), d.clone()));
                }
                _ => {
                    closed.insert(t.from.as_str());
                }
            }
        }
        self.lane.extend(joined);
    }

    /// Removes the txs the head block included from the mempool (cheap: ids are kept). A new
    /// head also makes every admission simulation stale.
    fn drop_included(&mut self) {
        self.sims.clear();
        if self.head_ids.is_empty() {
            return;
        }
        let included = std::mem::take(&mut self.head_ids);
        self.remove_from_mempool(|id| included.contains(id));
        for id in &included {
            self.pool.remove(id);
            self.mp.remove(id);
        }
        self.head_ids = included;
        self.rebuild_pending();
    }

    /// Removes the mempool txs whose ids match, keeping `mempool` and `mempool_ids` aligned.
    fn remove_from_mempool(&mut self, gone: impl Fn(&str) -> bool) {
        let txs = std::mem::take(&mut self.mempool);
        let ids = std::mem::take(&mut self.mempool_ids);
        for (tx, id) in txs.into_iter().zip(ids) {
            if !gone(&id) {
                self.mempool.push(tx);
                self.mempool_ids.push(id);
            }
        }
    }

    /// A copy of the state and the mempool to check against each other without the lock.
    pub fn mempool_check(&self) -> MempoolCheck {
        MempoolCheck { state: self.state.clone(), txs: self.mp.clone(), next: self.height() + 1, proposer: self.fee_sink(), chain_id: self.genesis.chain_id.clone() }
    }

    /// Drops what a [`MempoolCheck`] found (if still queued) and remembers why, with the same
    /// senders' later txs (queued since the check ran), which would wait behind a nonce gap.
    pub fn drop_from_mempool(&mut self, (height, dropped): (u64, Vec<(String, String)>)) {
        if dropped.is_empty() {
            return;
        }
        let mut gone: HashMap<String, String> = dropped.into_iter().filter(|(id, _)| self.pool.contains(id)).collect();
        // The first nonce slot each sender lost (a tx whose nonce is used already leaves no gap).
        let mut first_gone: HashMap<String, u64> = HashMap::new();
        for (t, id) in self.mempool.iter().zip(&self.mempool_ids).filter(|(t, _)| !t.is_unsigned()) {
            if gone.contains_key(id) && t.nonce >= self.state.account(&t.from).nonce {
                let n = first_gone.entry(t.from.clone()).or_insert(t.nonce);
                *n = (*n).min(t.nonce);
            }
        }
        let mut senders = std::collections::HashSet::new();
        for (t, id) in self.mempool.iter().zip(&self.mempool_ids) {
            if !t.is_unsigned() && first_gone.get(&t.from).is_some_and(|n| t.nonce > *n) && !gone.contains_key(id) {
                gone.insert(id.clone(), "dropped from the mempool: an earlier transaction from the same account was dropped".into());
            }
            if gone.contains_key(id) {
                senders.insert(t.from.clone());
            }
        }
        self.remove_from_mempool(|id| gone.contains_key(id));
        for (id, why) in gone {
            self.pool.remove(&id);
            self.mp.remove(&id);
            self.note_failed(id, why, height);
        }
        for sender in &senders {
            self.sims.forget(sender);
        }
        self.rebuild_pending();
        self.publish_mempool();
    }

    /// Drops mempool txs that expired or can no longer apply (included ones went already).
    fn revalidate_mempool(&mut self) {
        let dropped = self.mempool_check().run();
        self.drop_from_mempool(dropped);
    }

    /// Checks a tx against the current state and what its sender already has queued, then queues
    /// it (evicting the cheapest tail if the mempool is full). The caller must have verified the
    /// signature (outside the chain lock). Returns the id and whether it was new (already-known
    /// txs are accepted idempotently).
    ///
    /// The dry run applies the sender's queued txs first, in nonce order, exactly as a block
    /// would, so a tx that can't execute after them is refused instead of holding a slot it can
    /// never pay for (audit #10: an agent's transfers behind the owner's `RemoveKey`, transfers
    /// past a session key's limit). Only the sender's own txs count: nothing another account
    /// sends can make a tx unadmittable. The sender's simulated state is kept between its txs
    /// ([`Sims`]), so a tx costs one dry run whatever the size of the mempool or of the queue.
    pub fn submit_tx(&mut self, tx: Tx) -> Result<(String, bool)> {
        self.admit_to_mempool(tx, true)
    }

    pub fn submit(&mut self, tx: Tx) -> Result<String> {
        self.submit_tx(tx).map(|(id, _)| id)
    }

    /// Adds a tx that another node already accepted (full nodes track what they forwarded). The
    /// same checks as [`Chain::submit_tx`], but it never evicts: if the mempool or the sender's
    /// queue is full, it isn't tracked.
    pub fn track_pending(&mut self, tx: Tx) -> Result<()> {
        // A sender's replacement of its queued shield takes that one's place (`ghost::behind_shield`).
        let replaces = !tx.is_unsigned() && self.queued_shield(&tx.from).is_some_and(|(i, last)| last && self.mempool[i].nonce == tx.nonce);
        let full = self.regular_len() >= MAX_MEMPOOL && self.duty_slot(&tx).is_none();
        if !replaces && (full || tx.sender().is_ok_and(|s| self.pending_count(&s) >= MAX_PENDING_PER_SENDER)) {
            return Ok(());
        }
        self.admit_to_mempool(tx, false).map(|_| ())
    }

    /// Queued txs outside the validator-duty lane: what `MAX_MEMPOOL` bounds.
    fn regular_len(&self) -> usize {
        // The lane only holds queued ids (`rebuild_pending` prunes it), so this never saturates.
        self.mempool.len().saturating_sub(self.lane.len())
    }

    /// Whether queued tx `id` is in the validator-duty lane.
    #[cfg(test)]
    pub(crate) fn in_lane(&self, id: &str) -> bool {
        self.lane.contains_key(id)
    }

    /// The validator duty `tx` is (`staking::duty_of`), if any.
    fn duty_of(&self, tx: &Tx) -> Option<(String, &'static str)> {
        if tx.is_unsigned() {
            return None;
        }
        crate::staking::duty_of(&self.state, tx, self.state.params.epoch_of(self.height() + 1))
    }

    /// Whether the duty `d`, queued next by `sender`, takes a lane slot: every tx the sender has
    /// queued is in the lane (it's a prefix of each queue: [`Chain::rebuild_lane`]), and `d`'s
    /// validator has a slot free for its kind ([`DUTY_LANE_PER_KIND`]).
    fn lane_takes(&self, sender: &str, d: &(String, &'static str)) -> bool {
        let open = self.pending.get(sender).is_none_or(|q| q.iter().all(|&i| self.lane.contains_key(&self.mempool_ids[i])));
        open && self.lane.values().filter(|x| *x == d).count() < DUTY_LANE_PER_KIND
    }

    /// The lane slot `tx` would take if queued now: [`Chain::duty_of`] and [`Chain::lane_takes`].
    fn duty_slot(&self, tx: &Tx) -> Option<(String, &'static str)> {
        self.duty_of(tx).filter(|d| self.lane_takes(&tx.from, d))
    }

    /// The mempool in the order a proposal takes it ([`proposal_order`]): the validator-duty
    /// lane first, then by tip per byte.
    pub fn proposal_txs(&self) -> Vec<&Tx> {
        proposal_order(&self.mempool, |i| self.lane.contains_key(&self.mempool_ids[i]))
    }

    fn admit_to_mempool(&mut self, tx: Tx, evict: bool) -> Result<(String, bool)> {
        tx.check_chain(&self.genesis.chain_id, &self.genesis_hash)?;
        tx.check_format()?;
        let id = tx.id();
        if self.pool.contains(&id) || self.has_tx(&id) {
            return Ok((id, false));
        }
        let unsigned = tx.is_unsigned();
        let sender = if unsigned { String::new() } else { tx.sender()? };
        let next = self.height() + 1;
        let relay = crate::econ::base_fee(&self.state, next, tx.size());
        // A queued shield is its sender's last queued tx (review of F36, round 3): nothing queues
        // behind it, and its sender may replace it with a shield tipping more (`own`).
        let own = match (!unsigned).then(|| self.queued_shield(&sender)).flatten() {
            Some((i, last)) if tx.nonce >= self.mempool[i].nonce => {
                let shield = &self.mempool[i];
                crate::ghost::behind_shield(shield, last, &tx, relay, crate::econ::base_fee(&self.state, next, shield.size()), |base| crate::econ::max_tip(&self.state, base))?;
                Some(i)
            }
            _ => None,
        };
        // Ghost Mode: a bounded share of the mempool, and one queued spend per note (a high enough
        // tip displaces one). Unsigned ghost txs have no sender: no queue, nonce or simulation.
        let mut replaces = crate::ghost::mempool_admit(&self.mempool, &self.mempool_ids, &tx, own, relay, crate::econ::max_tip(&self.state, relay), evict)?;
        let asks = crate::econ::name_asked_for(&self.state, &tx);
        if let Some(name) = &asks {
            self.first_to_ask(&tx, name)?;
        }
        // The sender's txs ahead of this one (a replacement takes its shield's place).
        let count = if unsigned { 0 } else { self.pending_count(&sender) - usize::from(own.is_some()) };
        if count >= MAX_PENDING_PER_SENDER {
            bail!("too many pending transactions from {}; wait for some to finalize", short(&sender));
        }
        let expected = self.state.account(&sender).nonce + count as u64;
        if !unsigned && tx.nonce != expected {
            bail!("bad nonce: expected {expected}, got {}", tx.nonce);
        }
        let sim = self.dry_run(&tx, &sender, count)?;
        if let Some(i) = own {
            replaces.push((self.mempool_ids[i].clone(), "replaced by its sender's shield with a higher tip"));
        }
        if !replaces.is_empty() {
            self.displace(replaces, next);
        }
        // A validator duty (beacon, evidence, attestations) carries no tip, so it would never win a
        // slot in a full mempool: it takes one in the lane instead (audit F32/F56), if it's next in
        // line there (`lane_takes`). One that isn't queues like any tx (and joins the lane once
        // what's ahead of it is gone: `rebuild_lane`).
        let duty = self.duty_of(&tx);
        let lane = duty.clone().filter(|d| self.lane_takes(&sender, d));
        if lane.is_none() && self.regular_len() >= MAX_MEMPOOL {
            let victim = if evict { self.eviction_victim(&tx, &sender) } else { Err(anyhow!("mempool is full")) };
            match victim {
                Ok(idx) => self.evict(idx, next),
                Err(e) => {
                    if let Some((mut sim, j)) = sim {
                        sim.state.rollback(j);
                        self.sims.keep(sender, sim);
                    }
                    return Err(e);
                }
            }
        }
        if let Some((mut sim, _)) = sim {
            sim.applied += 1;
            self.sims.keep(sender.clone(), sim);
        }
        if let Some(g) = &self.gossip {
            let _ = g.unbounded_send(tx.clone());
        }
        if !unsigned {
            self.pending.entry(sender).or_default().push(self.mempool.len());
        }
        if let Some(d) = duty {
            self.duties.insert(id.clone(), d);
        }
        if let Some(d) = lane {
            self.lane.insert(id.clone(), d);
        }
        self.pool.insert(id.clone());
        self.mp.add(id.clone(), &tx, next, asks);
        self.mempool.push(tx);
        self.mempool_ids.push(id.clone());
        self.publish_mempool();
        Ok((id, true))
    }

    /// Names go to whoever asked first, as far as this mempool can tell (audit F35, a mitigation):
    /// a tx asking for a name (`name`, normalized: a `RegisterName`, or one wrapped in a
    /// multisig action, [`crate::econ::name_asked_for`]) that another account's registration is
    /// already waiting for isn't admitted, whatever its tip, so outbidding a registration seen in
    /// the public mempool doesn't work through this node (or any node the first one reached
    /// first). The waiting one holds the name only while it would register it in the next block:
    /// its fee covers that block's base fee, and a multisig's proposal or approval would run the
    /// registration rather than wait for more approvals
    /// ([`crate::governance::runs_when_included`]; a multisig's owner keys can't tip, so this
    /// is all a multisig's registration has against tips). And only for [`NAME_HOLD_BLOCKS`]
    /// after it arrived, so a registration that's stuck, or left waiting on purpose, keeps the
    /// name from nobody. Wrapped ones that wouldn't run hold nothing but are still held off, so
    /// approvals that only together reach the threshold, or a proposal queued with its approval,
    /// can't get past a waiting registration either. A sender's own txs never count against it.
    /// The waiting registrations are an index read (newest first, stopping at the first that's
    /// too old), not a mempool scan. A proposer can still put its own registration first in its
    /// block: PLAN.md "Known limits".
    fn first_to_ask(&self, tx: &Tx, name: &str) -> Result<()> {
        let next = self.height() + 1;
        let held = self
            .mp
            .registrations(name)
            .take_while(|p| p.added_at.saturating_add(NAME_HOLD_BLOCKS) >= next)
            .any(|p| p.tx.from != tx.from && crate::econ::check_fee(&self.state, &p.tx, next).is_ok() && crate::governance::runs_when_included(&self.state, &p.tx, next));
        if held {
            bail!("another account's registration of {name}{NAME_SUFFIX} is already waiting: names go to whoever asked first");
        }
        Ok(())
    }

    /// Dry-runs `tx` (from `sender`, at the next height) after the sender's `queued` txs; `Err`
    /// says why it can't be included. With nothing queued it runs on the live state and is
    /// rolled back (`None`). Otherwise it runs on the sender's simulation (from the cache, or
    /// built from its queue), which is returned with `tx` applied and the journal that undoes it.
    fn dry_run(&mut self, tx: &Tx, sender: &str, queued: usize) -> Result<Simulated> {
        let (next, proposer, chain_id) = (self.height() + 1, self.fee_sink(), self.genesis.chain_id.clone());
        let try_apply = |state: &mut State| -> Result<Journal> {
            let mut j = Journal::default();
            let outcome = state.apply_j(&mut j, tx, &chain_id, &proposer, next, false);
            match outcome {
                Ok(Outcome::Applied) => Ok(j),
                Ok(Outcome::Failed(r)) => {
                    state.rollback(j);
                    Err(anyhow!(r))
                }
                Err(e) => {
                    state.rollback(j);
                    Err(e)
                }
            }
        };
        if queued == 0 {
            let j = try_apply(&mut self.state)?;
            self.state.rollback(j);
            return Ok(None);
        }
        let mut sim = match self.sims.take(sender, self.height(), queued) {
            Some(sim) => sim,
            None => {
                let sim = self.simulate_queue(sender, queued, next, &proposer, &chain_id)?;
                self.sims.rebuilt += sim.applied as u64;
                sim
            }
        };
        match try_apply(&mut sim.state) {
            Ok(j) => Ok(Some((sim, j))),
            Err(e) => {
                self.sims.keep(sender.to_string(), sim);
                Err(e)
            }
        }
    }

    /// A copy of the state with `sender`'s first `queued` queued txs applied in nonce order (all
    /// of them, but for a replacement of the last: [`crate::ghost::behind_shield`]), as the block
    /// at `next` would apply them. `Err` if one no longer applies (a block changed the state
    /// since it was queued; the post-block check drops it and the txs behind it).
    fn simulate_queue(&self, sender: &str, queued: usize, next: u64, proposer: &str, chain_id: &str) -> Result<SenderSim> {
        let mut state = self.state.clone();
        let queue = self.pending.get(sender).map(Vec::as_slice).unwrap_or_default();
        let queue = &queue[..queued.min(queue.len())];
        for &i in queue {
            let t = &self.mempool[i];
            let mut j = Journal::default();
            let why = match state.apply_j(&mut j, t, chain_id, proposer, next, false) {
                Ok(Outcome::Applied) => continue,
                Ok(Outcome::Failed(r)) => r,
                Err(e) => e.to_string(),
            };
            bail!("your pending transaction with nonce {} no longer applies ({why}); wait for it to be dropped", t.nonce);
        }
        Ok(SenderSim { state, applied: queue.len(), used: 0 })
    }

    /// The tx a full mempool gives up for `tx`: the lowest tip per byte if `tx` pays more (among
    /// equals, the newest arrival). Only another sender's last queued tx can go, so nobody is
    /// left with a nonce gap, and every queued tx still applies after its predecessors. Validator
    /// duties in the lane never go.
    fn eviction_victim(&self, tx: &Tx, sender: &str) -> Result<usize> {
        let tails = self.pending.iter().filter(|(s, _)| s.as_str() != sender).filter_map(|(_, q)| q.last().copied()).filter(|&i| !self.lane.contains_key(&self.mempool_ids[i]));
        let lowest = tails.min_by(|&a, &b| TipRate::of(&self.mempool[a]).cmp(&TipRate::of(&self.mempool[b])).then(b.cmp(&a)));
        match lowest {
            Some(i) if TipRate::of(tx) > TipRate::of(&self.mempool[i]) => Ok(i),
            Some(i) => {
                let low = TipRate::of(&self.mempool[i]);
                let mut need = (low.tip as u128 * tx.size() as u128 / low.size.max(1) as u128 + 1).min(u64::MAX as u128);
                // Never ask for a tip past this tx's cap (review of fix-fees): a mempool full of
                // smaller txs tipping near theirs can't be outbid until blocks drain it or the
                // base fee (and with it every cap) rises.
                let mut cap = crate::econ::max_tip(&self.state, crate::econ::base_fee(&self.state, self.height() + 1, tx.size()));
                // An unsigned ghost tx tips a whole multiple of the fee grid (review of fix-fees-2).
                if tx.is_unsigned() {
                    let grid = crate::ghost::FEE_GRID as u128;
                    need = need.div_ceil(grid) * grid;
                    cap -= cap % crate::ghost::FEE_GRID;
                }
                if need > cap as u128 {
                    bail!("mempool is full, and its cheapest transactions tip more per byte than this one may (at most {cap} drops, {}× its base fee); try again shortly", self.state.chain_params.tip_cap_multiple);
                }
                bail!("mempool is full; resubmit with a tip of at least {need} drops (and the fee to cover it), or try again shortly")
            }
            None => bail!("mempool is full; try again shortly"),
        }
    }

    /// Removes what a ghost tx displaces (`ghost::mempool_admit`, and the shield a sender
    /// replaces: `ghost::behind_shield`). A signed one (a `Shield`) is its sender's last queued
    /// tx, as admission keeps the mempool; if it ever weren't, its sender's later txs would go
    /// with it (they'd wait behind a nonce gap; `mempool_admit` prices them in), and either way
    /// its simulation, which applied it, is forgotten (review of F36).
    fn displace(&mut self, victims: Vec<(String, &'static str)>, next: u64) {
        let mut gone: HashMap<String, String> = victims.into_iter().map(|(id, why)| (id, why.to_string())).collect();
        let mut first: HashMap<String, u64> = HashMap::new();
        for (t, _) in self.mempool.iter().zip(&self.mempool_ids).filter(|(t, id)| !t.is_unsigned() && gone.contains_key(*id)) {
            let n = first.entry(t.from.clone()).or_insert(t.nonce);
            *n = (*n).min(t.nonce);
        }
        for (t, id) in self.mempool.iter().zip(&self.mempool_ids) {
            if !t.is_unsigned() && first.get(&t.from).is_some_and(|n| t.nonce > *n) && !gone.contains_key(id) {
                gone.insert(id.clone(), "dropped from the mempool: an earlier transaction from the same account was displaced".into());
            }
        }
        self.remove_from_mempool(|id| gone.contains_key(id));
        for (id, why) in gone {
            self.pool.remove(&id);
            self.mp.remove(&id);
            self.note_failed(id, why, next);
        }
        for sender in first.keys() {
            self.sims.forget(sender);
        }
        self.rebuild_pending();
    }

    fn evict(&mut self, idx: usize, next: u64) {
        let gone = self.mempool.remove(idx);
        let eid = self.mempool_ids.remove(idx);
        self.pool.remove(&eid);
        self.mp.remove(&eid);
        self.sims.forget(&gone.from);
        self.note_failed(eid, "evicted from a full mempool by a transaction with a higher tip".into(), next);
        self.rebuild_pending();
    }

    /// Removes a tx we queued locally (e.g. a full node whose forward failed), and its sender's
    /// later txs, which can't execute without it.
    pub fn drop_pending(&mut self, id: &str) {
        let Some(i) = self.mempool_ids.iter().position(|x| x == id) else { return };
        let (from, nonce, unsigned) = (self.mempool[i].from.clone(), self.mempool[i].nonce, self.mempool[i].is_unsigned());
        // An unsigned ghost tx has no sender: it goes alone.
        let gone: std::collections::HashSet<String> = self.mempool.iter().zip(&self.mempool_ids).filter(|(t, x)| if unsigned { *x == id } else { !t.is_unsigned() && t.from == from && t.nonce >= nonce }).map(|(_, id)| id.clone()).collect();
        self.remove_from_mempool(|x| gone.contains(x));
        for id in &gone {
            self.pool.remove(id);
            self.mp.remove(id);
        }
        self.sims.forget(&from);
        self.rebuild_pending();
        self.publish_mempool();
    }

    /// `sender`'s queued shield, if any (its first), and whether it's the sender's last queued tx
    /// (always, as [`Chain::submit_tx`] keeps the mempool: [`crate::ghost::behind_shield`]).
    fn queued_shield(&self, sender: &str) -> Option<(usize, bool)> {
        let q = self.pending.get(sender)?;
        let i = q.iter().copied().find(|&i| matches!(self.mempool[i].action, Action::Shield { .. }))?;
        Some((i, q.last() == Some(&i)))
    }

    pub fn pending_count(&self, addr: &str) -> usize {
        self.pending.get(addr).map_or(0, Vec::len)
    }

    pub fn in_mempool(&self, id: &str) -> bool {
        self.pool.contains(id)
    }

    /// Next nonce for an address, counting txs already waiting in the mempool.
    pub fn next_nonce(&self, addr: &str) -> u64 {
        self.state.account(addr).nonce + self.pending_count(addr) as u64
    }

    /// Whether the faucet can take another drip right now.
    pub fn faucet_busy(&self) -> bool {
        self.me().is_some_and(|me| self.pending_count(&me) >= FAUCET_MAX_PENDING)
    }

    /// Testnet faucet: the validator's own genesis-funded account pays. Full nodes forward the
    /// request to a validator instead (see api.rs).
    pub fn faucet(&mut self, to: &str, amount: u64) -> Result<Tx> {
        if !self.genesis.faucet {
            bail!("this chain has no faucet");
        }
        if self.faucet_busy() {
            bail!("faucet busy, retry in a few seconds");
        }
        if self.keys.is_none() {
            bail!("this node has no faucet key");
        }
        let to = self.state.resolve(to)?;
        self.submit_own(Action::Transfer { to, amount }, "🦇 faucet drip")
    }

    /// Signs `action` with this validator's key, from its own account, and queues it.
    pub fn submit_own(&mut self, action: Action, memo: &str) -> Result<Tx> {
        let from = self.me().ok_or_else(|| anyhow!("this node has no validator key"))?;
        self.submit_signed(action, memo, from)
    }

    /// Who pays for this validator's system txs (attestations, evidence, beacon): its owner when
    /// that's another account (SF-13: the consensus key signs for it), else the consensus key's own
    /// account. `BLOCKULA_SYSTEM_FEES=self` makes the consensus key's account pay regardless (an
    /// owner whose account is busy elsewhere avoids nonce races with this node).
    pub fn system_fee_payer(&self) -> Option<String> {
        let me = self.me()?;
        let owner = self.state.validators.get(&me).map(|r| r.owner.clone()).filter(|o| *o != me);
        match owner {
            Some(o) if std::env::var("BLOCKULA_SYSTEM_FEES").as_deref() != Ok("self") => Some(o),
            _ => Some(me),
        }
    }

    /// Signs a validator system tx with this validator's consensus key, from
    /// [`Chain::system_fee_payer`], and queues it.
    pub fn submit_system(&mut self, action: Action, memo: &str) -> Result<Tx> {
        let from = self.system_fee_payer().ok_or_else(|| anyhow!("this node has no validator key"))?;
        self.submit_signed(action, memo, from)
    }

    fn submit_signed(&mut self, action: Action, memo: &str, from: String) -> Result<Tx> {
        let keys = self.keys.as_ref().ok_or_else(|| anyhow!("this node has no validator key"))?;
        let mut tx = Tx {
            chain_id: self.genesis.chain_id.clone(),
            genesis_hash: self.genesis_hash.clone(),
            nonce: self.next_nonce(&from),
            from,
            fee: 0,
            tip: 0,
            valid_until: self.height() + OWN_TX_VALIDITY.min(self.state.chain_params.max_tx_validity_blocks),
            action,
            memo: memo.into(),
            public_key: hex::encode(keys.public_key()),
            signature: "00".repeat(crypto::SIGNATURE_LEN),
        };
        // The highest base fee already fixed for the next blocks (only the actual one is charged).
        tx.fee = crate::econ::max_fee(&self.state, tx.size());
        let tx = tx.sign(keys);
        self.submit(tx.clone())?;
        Ok(tx)
    }

    /// Test helper: execute the mempool as the next block (what a lone validator would finalize).
    #[cfg(test)]
    pub fn produce_block(&mut self) -> Result<()> {
        let txs = self.mempool.clone();
        self.produce_block_with(txs)
    }

    #[cfg(test)]
    pub fn produce_block_with(&mut self, txs: Vec<Tx>) -> Result<()> {
        let f = self.test_block(txs);
        self.execute(f)
    }

    /// The next block with `txs`, as consensus would hand it over (tests).
    #[cfg(test)]
    pub fn test_block(&self, txs: Vec<Tx>) -> Finalized {
        let height = self.height() + 1;
        Finalized { height, hash: hex::encode(crypto::hash(format!("test-{}-{}", self.height(), self.last_hash()).as_bytes())), parent: self.last_hash().to_string(), timestamp_ms: now_ms(), view: height, parent_view: height - 1, version: crate::upgrade::version_at(&self.state, height), signal: self.halt.supported, proposer: self.fee_sink(), txs, cblock: vec![], certificate: None }
    }
}

/// What `Chain::open` needs to pick a starting state.
struct OpenCtx<'a> {
    home: &'a Path,
    genesis: &'a Genesis,
    genesis_hash: &'a str,
    genesis_root: &'a str,
    base: Option<&'a Base>,
    saved_epochs: Option<&'a crate::epochs::EpochRegistry>,
    blocks: &'a SegLog,
}

impl OpenCtx<'_> {
    fn base_height(&self) -> u64 {
        self.base.map(|b| b.height).unwrap_or(0)
    }

    /// The state as of the database's commit point, checked against the tree, the records' own
    /// root and the block log. `None`: there is no usable database (none yet, another format or
    /// schema, another chain, another base).
    fn load(&self, store: &Arc<Store>) -> Result<Option<Start>> {
        let Some(p) = store.commit_point()? else { return Ok(None) };
        if p.format != store::STORE_FORMAT || p.schema != SCHEMA_VERSION || p.genesis_hash != self.genesis_hash || p.base != self.base_height() {
            tracing::info!(format = p.format, schema = p.schema, base = p.base, "the state database was written for another format, chain or base; rebuilding it");
            return Ok(None);
        }
        let genesis_epochs = crate::epochs::EpochRegistry::genesis(self.genesis);
        let epochs = match self.saved_epochs {
            // A genesis home that hasn't executed anything holds only the genesis set: a history
            // saved there is one a fast sync verified and a crash kept from installing (or a
            // peer's), not this node's own (F08). Executing records it again.
            Some(_) if p.height == 0 && self.base.is_none() => genesis_epochs,
            Some(e) => e.clone(),
            None if p.height == 0 => genesis_epochs,
            None => bail!("epochs.json is missing"),
        };
        let snap = Arc::new(store.snapshot()?);
        let root_child = Child::decode(&p.root_child)?;
        ensure!(root_child.hash() == p.root, "the stored root slot doesn't hash to the commit point's root");
        if let Child::Node(h) = root_child {
            let node = snap.get(&NodeKey { depth: 0, prefix: [0; 32] })?.ok_or_else(|| anyhow!("the tree's root node is missing"))?;
            ensure!(node.hash() == h, "the tree's stored root node doesn't hash to its root");
        }
        let tree = Smt::open(root_child, snap.clone());
        let (state, records_root) = State::from_stored_records(store.records()?, tree)?;
        ensure!(records_root == p.root, "the stored records don't hash to the stored root");
        ensure!(state.record_count() as u64 == p.records, "the database holds {} records, its commit point says {}", state.record_count(), p.records);
        // The commit point agrees with the block log (or the base, or genesis).
        let root = hex::encode(p.root);
        if p.height > self.base_height() {
            let meta = snap.block_meta(p.height)?.ok_or_else(|| anyhow!("no index row for the committed block {}", p.height))?;
            let line = self.blocks.reader().read(meta.block).with_context(|| format!("reading block {} from the log", p.height))?;
            let b: Block = serde_json::from_slice(&line)?;
            ensure!(b.height == p.height && b.hash == p.block_hash && b.state_root == root, "block {} in the log isn't the one the database committed", p.height);
        } else if let Some(base) = self.base {
            ensure!(p.height == base.height && p.block_hash == base.hash && root == base.state_root, "the database doesn't hold base.json's state");
        } else {
            ensure!(p.height == 0 && root == self.genesis_root && p.block_hash == self.genesis_hash, "the database doesn't hold the genesis state");
        }
        Ok(Some(Start { state, point: p, snap, epochs, rebuilt_epochs: false }))
    }

    /// A fresh database with the state the logs start from: `base.state.zst`, genesis, or (logs
    /// pruned past both) the newest local snapshot they continue from. `open` then replays the
    /// logs over it.
    fn rebuild(&self, store: &Arc<Store>, genesis_state: State) -> Result<Start> {
        let first = self.first_logged_height()?;
        let (state, height, hash, epochs) = match self.base {
            Some(b) => {
                let st = crate::snapshot::decode_state_file(&self.home.join("base.state.zst")).context("base.state.zst")?;
                ensure!(st.root() == b.state_root, "base.state.zst doesn't match base.json's state root");
                (st, b.height, b.hash.clone(), self.saved_epochs.cloned().ok_or_else(|| anyhow!("epochs.json missing for a fast-synced home"))?)
            }
            None => (genesis_state, 0, self.genesis_hash.to_string(), crate::epochs::EpochRegistry::genesis(self.genesis)),
        };
        let (state, height, hash, epochs, rebuilt) = match first {
            Some(f) if f > height + 1 => {
                let (st, h, hash) = self.local_snapshot(f).with_context(|| format!("the block logs start at #{f} (older blocks were pruned), the state database is unusable, and no local snapshot continues them; join the chain again in a fresh home"))?;
                let epochs = self.saved_epochs.cloned().ok_or_else(|| anyhow!("epochs.json is missing"))?;
                (st, h, hash, epochs, false)
            }
            _ => {
                let rebuilt = self.base.is_none();
                (state, height, hash, epochs, rebuilt)
            }
        };
        tracing::info!(from = height, "rebuilding the state database from the block logs");
        let mut point = CommitPoint {
            format: store::STORE_FORMAT,
            schema: SCHEMA_VERSION,
            genesis_hash: self.genesis_hash.to_string(),
            height,
            block_hash: hash,
            root: state.root_hash(),
            root_child: state.tree.root_child().encode(),
            base: self.base_height(),
            pruned_below: height + 1,
            records: 0,
            // From the first segment (a rebuild from a local snapshot skips what it covers).
            blocks_end: (0, 0),
            cblocks_end: (0, 0),
        };
        store.reset(&mut point, &mut state_records(&state), &state.tree.pending_changes())?;
        let snap = Arc::new(store.snapshot()?);
        let mut state = state;
        state.tree.rebase(snap.clone());
        Ok(Start { state, point, snap, epochs, rebuilt_epochs: rebuilt })
    }

    /// Height of the first block in the log, if any.
    fn first_logged_height(&self) -> Result<Option<u64>> {
        match self.blocks.reader().iter_from((0, 0)).next().transpose()? {
            Some(e) => Ok(Some(serde_json::from_slice::<Block>(&e.payload).context("the first entry of the block log")?.height)),
            None => Ok(None),
        }
    }

    /// The newest local snapshot the logs continue from (its height + 1 is logged), with its
    /// block hash.
    fn local_snapshot(&self, first_logged: u64) -> Result<(State, u64, String)> {
        for h in crate::snapshot::list(self.home) {
            if h + 1 < first_logged {
                continue;
            }
            let Some(hash) = self.logged_hash(h)? else { continue };
            match crate::snapshot::read_local(self.home, h) {
                Ok(st) => {
                    tracing::warn!(height = h, "rebuilding from this node's own snapshot (older blocks were pruned)");
                    return Ok((st, h, hash));
                }
                Err(e) => tracing::warn!(height = h, error = %format!("{e:#}"), "local snapshot unusable"),
            }
        }
        bail!("no usable local snapshot")
    }

    /// The hash of block `height`, from the log (its own line, or the next one's parent).
    fn logged_hash(&self, height: u64) -> Result<Option<String>> {
        let reader = self.blocks.reader();
        for e in reader.iter_from((reader.seg_of(height), 0)) {
            let b: Block = serde_json::from_slice(&e?.payload)?;
            if b.height == height {
                return Ok(Some(b.hash));
            }
            if b.height == height + 1 {
                return Ok(Some(b.prev_hash));
            }
            if b.height > height + 1 {
                break;
            }
        }
        Ok(None)
    }
}

pub const MAX_MEMPOOL: usize = 5_000;
pub const MAX_PENDING_PER_SENDER: usize = 64;
/// Validator duties (`staking::duty_of`) the mempool queues per validator and kind of duty in a
/// lane of its own: outside `MAX_MEMPOOL`, never evicted for a higher tip, and first in proposals
/// (audit F32/F56). A validator owes one commitment and one reveal an epoch, and each fault or
/// claim is reported or attested once (the chain refuses repeats), so the lane stays small: at
/// most 2 × 64 validators × 5 kinds × this many txs (a reveal still owed by a validator that left
/// the set counts too), plus, while the set is one or two members, their admissions and one
/// candidate's readiness. Duties past it, or behind their sender's other txs (the lane is a prefix of
/// each sender's queue), queue like any tx and join the lane once what's ahead of them is gone.
pub const DUTY_LANE_PER_KIND: usize = 2;

/// Mempool txs in the order a proposal takes them: those `first` picks (the validator-duty lane,
/// by index) ahead of the rest, then the highest tip per byte (earlier arrival first among
/// equals), each sender's txs still in nonce (arrival) order.
pub fn proposal_order(mempool: &[Tx], first: impl Fn(usize) -> bool) -> Vec<&Tx> {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, VecDeque};
    let mut queues: Vec<VecDeque<usize>> = vec![];
    let mut by_sender: HashMap<&str, usize> = HashMap::new();
    for (i, t) in mempool.iter().enumerate() {
        // Unsigned ghost txs have no sender (or nonce): each is a queue of its own.
        if t.is_unsigned() {
            queues.push(VecDeque::from([i]));
            continue;
        }
        let q = *by_sender.entry(t.from.as_str()).or_insert_with(|| {
            queues.push(VecDeque::new());
            queues.len() - 1
        });
        queues[q].push_back(i);
    }
    // Max-heap of each sender's next tx: (lane, rate, earliest arrival, queue).
    let key = |i: usize, q: usize| (first(i), TipRate::of(&mempool[i]), Reverse(i), q);
    let mut heap: BinaryHeap<(bool, TipRate, Reverse<usize>, usize)> = queues.iter().enumerate().map(|(q, txs)| key(txs[0], q)).collect();
    let mut out = Vec::with_capacity(mempool.len());
    while let Some((_, _, Reverse(i), q)) = heap.pop() {
        out.push(&mempool[i]);
        queues[q].pop_front();
        if let Some(&next) = queues[q].front() {
            heap.push(key(next, q));
        }
    }
    out
}

/// A tx's tip per byte, compared exactly (cross-multiplied in u128, no floats).
#[derive(Clone, Copy, Debug)]
pub struct TipRate {
    pub tip: u64,
    pub size: u64,
}

impl TipRate {
    pub fn of(tx: &Tx) -> Self {
        Self { tip: tx.tip, size: tx.size().max(1) }
    }
}

impl Ord for TipRate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.tip as u128 * other.size as u128).cmp(&(other.tip as u128 * self.size as u128))
    }
}
impl PartialOrd for TipRate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for TipRate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for TipRate {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_and_names_survive_restart() {
        let home = std::env::temp_dir().join(format!("blockula-test-{}", now_ms()));
        Chain::summon(&home, &SummonOpts::new("test-1", 1)).unwrap();
        let alice = Keys::from_seed([1u8; 32]);
        {
            let mut c = Chain::open(&home).unwrap();
            c.faucet(&alice.address(), 50 * DROPS_PER_FANG).unwrap();
            c.produce_block().unwrap();
            let mut tx = Tx { chain_id: "test-1".into(), genesis_hash: c.genesis_hash.clone(), from: alice.address(), nonce: 0, fee: 0, tip: 0, valid_until: c.height() + 100, action: Action::RegisterName { name: "alice".into() }, memo: String::new(), public_key: String::new(), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            tx.public_key = hex::encode(alice.public_key());
            tx.fee = min_fee(tx.size());
            c.submit(tx.sign(&alice)).unwrap();
            c.produce_block().unwrap();
            assert_eq!(c.state.resolve("alice.boo").unwrap(), alice.address());
        }
        let c = Chain::open(&home).unwrap();
        assert_eq!(c.height(), 2);
        assert_eq!(c.state.primary_name(&alice.address()).unwrap(), "alice.boo");
        fs::remove_dir_all(home).ok();
    }

    fn signed(c: &Chain, from: &str, keys: &Keys, nonce: u64, action: Action) -> Tx {
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: from.into(), nonce, fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        tx.sign(keys)
    }

    /// Audit gap-7-2: AddKey needs the added key's consent to that account on this chain. Before,
    /// anyone could put any (public) key id on their own accounts as an owner, and 100 such
    /// strangers pushed the key's real account out of `/api/key`.
    #[test]
    fn adding_a_key_needs_its_consent() {
        let home = std::env::temp_dir().join(format!("blockula-pop-{}-{}", now_ms(), std::process::id()));
        Chain::summon(&home, &SummonOpts::new("test-pop", 1)).unwrap();
        let mut c = Chain::open(&home).unwrap();
        let (victim, kit, mallory) = (Keys::from_seed([1u8; 32]), Keys::from_seed([2u8; 32]), Keys::from_seed([3u8; 32]));
        let (va, ma) = (victim.address(), mallory.address());
        c.faucet(&va, 10 * DROPS_PER_FANG).unwrap();
        c.faucet(&ma, 10 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let (cid, gh) = (c.genesis.chain_id.clone(), c.genesis_hash.clone());
        // The victim adds their recovery kit with its consent: the kit's AddKey is public now.
        let theirs = crate::types::add_key(&kit, &cid, &gh, &va, "recovery kit", KeyRole::Owner);
        c.submit(signed(&c, &va, &victim, 0, theirs.clone())).unwrap();
        c.produce_block().unwrap();
        assert!(c.state.keys_of(&va).contains_key(&key_id(&kit.public_key())));
        // Mallory replays it (the kit's consent names the victim's account), forges one, or brings
        // a consent made for another chain instance: all refused at admission…
        let Action::AddKey { key_id: kid, public_key, pop, .. } = theirs.clone() else { unreachable!() };
        let forged = Action::AddKey { key_id: kid.clone(), label: "x".into(), role: KeyRole::Owner, public_key: public_key.clone(), pop: hex::encode(mallory.sign_with_context(crate::types::KEY_POP_CONTEXT, b"anything")) };
        let other_chain = crate::types::add_key(&kit, &cid, &"77".repeat(32), &ma, "x", KeyRole::Owner);
        for (what, action) in [("a replayed consent", theirs.clone()), ("a forged one", forged), ("one for another chain", other_chain)] {
            let err = c.submit(signed(&c, &ma, &mallory, 0, action)).unwrap_err().to_string();
            assert!(err.contains("proof of possession doesn't verify"), "{what}: {err}");
        }
        // …and in a block (a proposer that includes one anyway): it fails, nothing is added.
        let replay = signed(&c, &ma, &mallory, 0, theirs);
        c.produce_block_with(vec![replay.clone()]).unwrap();
        assert!(c.failure(&replay.id()).unwrap().0.contains("proof of possession"));
        assert!(!c.state.keys_of(&ma).contains_key(&kid));
        // A public key that isn't the key id's is a format error.
        let swapped = Action::AddKey { key_id: kid, label: "x".into(), role: KeyRole::Owner, public_key: hex::encode(mallory.public_key()), pop };
        assert!(c.submit(signed(&c, &ma, &mallory, 1, swapped)).unwrap_err().to_string().contains("isn't the key key_id names"));
        // With the key's consent to Mallory's own account it's fine (the kit's holder agreed).
        c.submit(signed(&c, &ma, &mallory, 1, crate::types::add_key(&kit, &cid, &gh, &ma, "kit", KeyRole::Owner))).unwrap();
        c.produce_block().unwrap();
        assert!(c.state.keys_of(&ma).contains_key(&key_id(&kit.public_key())));
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn owner_and_session_keys() {
        let home = std::env::temp_dir().join(format!("blockula-aa-{}", now_ms()));
        Chain::summon(&home, &SummonOpts::new("test-aa", 1)).unwrap();
        let mut c = Chain::open(&home).unwrap();
        let (owner, laptop, agent) = (Keys::from_seed([1u8; 32]), Keys::from_seed([2u8; 32]), Keys::from_seed([3u8; 32]));
        let acct = owner.address();
        c.faucet(&acct, 100 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();

        // A stranger's key can't act for the account.
        assert!(c.submit(signed(&c, &acct, &agent, 0, Action::Transfer { to: agent.address(), amount: 1 })).unwrap_err().to_string().contains("isn't authorized"));

        // Owner adds a second device and an agent session key: 5 FANG per 10 blocks.
        let session = KeyRole::Session { limit: 5 * DROPS_PER_FANG, period_blocks: 10, expires_at: 1_000 };
        let (cid, gh) = (c.genesis.chain_id.clone(), c.genesis_hash.clone());
        c.submit(signed(&c, &acct, &owner, 0, crate::types::add_key(&laptop, &cid, &gh, &acct, "laptop", KeyRole::Owner))).unwrap();
        c.submit(signed(&c, &acct, &owner, 1, crate::types::add_key(&agent, &cid, &gh, &acct, "claude", session))).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.keys_of(&acct).len(), 3);

        // The agent can spend within its limit, from the owner's account…
        c.submit(signed(&c, &acct, &agent, 2, Action::Transfer { to: laptop.address(), amount: 3 * DROPS_PER_FANG })).unwrap();
        c.produce_block().unwrap();
        // …but not past it, and never anything other than transfers.
        let over = c.submit(signed(&c, &acct, &agent, 3, Action::Transfer { to: laptop.address(), amount: 2 * DROPS_PER_FANG }));
        assert!(over.unwrap_err().to_string().contains("limit reached"));
        let reg = c.submit(signed(&c, &acct, &agent, 3, Action::RegisterName { name: "sneaky".into() }));
        assert!(reg.unwrap_err().to_string().contains("only send transfers"));

        // The laptop (owner) revokes the agent; the original key can't be the last owner removed.
        c.submit(signed(&c, &acct, &laptop, 3, Action::RemoveKey { key_id: key_id(&agent.public_key()) })).unwrap();
        c.submit(signed(&c, &acct, &laptop, 4, Action::RemoveKey { key_id: key_id(&owner.public_key()) })).unwrap();
        c.produce_block().unwrap();
        let last = c.submit(signed(&c, &acct, &laptop, 5, Action::RemoveKey { key_id: key_id(&laptop.public_key()) }));
        assert!(last.unwrap_err().to_string().contains("last owner"));
        assert!(c.submit(signed(&c, &acct, &agent, 5, Action::Transfer { to: laptop.address(), amount: 1 })).is_err());

        // Everything replays identically from disk.
        let root = c.state.root();
        drop(c);
        assert_eq!(Chain::open(&home).unwrap().state.root(), root);
        fs::remove_dir_all(home).ok();
    }
}

/// Regression tests for the security audit (PoCs from audit-core, now asserting the fixes).
#[cfg(test)]
mod audit {
    use super::*;
    use std::time::Instant;

    fn fresh(tag: &str) -> (PathBuf, Chain) {
        let home = std::env::temp_dir().join(format!("blockula-audit-{tag}-{}-{}", now_ms(), std::process::id()));
        Chain::summon(&home, &SummonOpts::new("audit", 1)).unwrap();
        let c = Chain::open(&home).unwrap();
        (home, c)
    }

    fn signed_fee(c: &Chain, from: &str, keys: &Keys, nonce: u64, action: Action, fee: Option<u64>) -> Tx {
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: from.into(), nonce, fee: 0, tip: 0, valid_until: c.height() + 1_000, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = fee.unwrap_or_else(|| min_fee(tx.size()));
        tx.sign(keys)
    }

    /// Unsigned (zero signature) tx on the `audit` chain whose genesis hash is given: the state
    /// machine doesn't verify signatures (admission and block verification do), so benchmarks can
    /// skip ML-DSA signing. Valid through block 1,000.
    fn unsigned(genesis_hash: &str, from: &Keys, nonce: u64, action: Action) -> Tx {
        let mut tx = Tx { chain_id: "audit".into(), genesis_hash: genesis_hash.into(), from: from.address(), nonce, fee: 0, tip: 0, valid_until: 1_000, action, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        tx
    }

    #[test]
    fn oversized_tx_rejected_and_stale_txs_expire() {
        let (_h, mut c) = fresh("big");
        let a = Keys::from_seed([5u8; 32]);
        c.faucet(&a.address(), 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        c.submit(signed_fee(&c, &a.address(), &a, 0, Action::RegisterName { name: "victim".into() }, None)).unwrap();
        c.produce_block().unwrap();
        // The old PoC: `to` padded with 60 KB of spaces used to be trimmed and accepted.
        let pad = format!("victim.boo{}", " ".repeat(62_000));
        let big = signed_fee(&c, &a.address(), &a, 1, Action::Transfer { to: pad, amount: 1 }, Some(100 * 66_000));
        assert!(big.size() as usize > MAX_TX_BYTES);
        let err = c.submit(big.clone()).unwrap_err().to_string();
        assert!(err.contains("too large"), "{err}");
        assert!(c.track_pending(big).is_err());
        assert!(c.mempool.is_empty());
        // Field caps.
        let long_label = crate::types::add_key(&Keys::from_seed([77; 32]), &c.genesis.chain_id, &c.genesis_hash, &a.address(), &"x".repeat(65), KeyRole::Owner);
        assert!(c.submit(signed_fee(&c, &a.address(), &a, 1, long_label, None)).unwrap_err().to_string().contains("label too long"));
        let mut memo = signed_fee(&c, &a.address(), &a, 1, Action::Transfer { to: c.me().unwrap(), amount: 1 }, None);
        memo.memo = "m".repeat(257);
        assert!(c.submit(memo.sign(&a)).is_err());
        // A tx that's never included leaves the mempool after the TTL instead of sitting there
        // forever. It isn't expired (it can still execute until `valid_until`), and says so.
        let later = signed_fee(&c, &a.address(), &a, 1, Action::Transfer { to: c.me().unwrap(), amount: 1 }, None);
        let id = c.submit(later.clone()).unwrap();
        for _ in 0..=MEMPOOL_TTL_BLOCKS {
            c.produce_block_with(vec![]).unwrap();
        }
        assert!(c.mempool.is_empty());
        let why = &c.failure(&id).unwrap().0;
        assert!(why.contains(&format!("not included within {MEMPOOL_TTL_BLOCKS} blocks")) && why.contains(&format!("valid until block {}", later.valid_until)), "{why}");
    }

    #[test]
    fn tx_id_not_malleable_via_hex_case() {
        let (_h, mut c) = fresh("mall");
        let a = Keys::from_seed([3u8; 32]);
        c.faucet(&a.address(), 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let tx = signed_fee(&c, &a.address(), &a, 0, Action::Transfer { to: c.me().unwrap(), amount: 1 }, None);
        let orig_id = tx.id();
        let mut m = tx.clone();
        m.signature = m.signature.to_uppercase();
        assert_eq!(m.id(), orig_id, "id is over the decoded signature bytes");
        assert!(crate::consensus::check_tx_stateless(&m, "audit", &c.genesis_hash, "boo").is_err(), "uppercase hex is rejected");
        assert!(c.submit(m).is_err());
        c.submit(tx).unwrap();
        c.produce_block().unwrap();
        assert!(c.has_tx(&orig_id));
    }

    #[test]
    fn uppercase_key_id_rejected() {
        let (_h, mut c) = fresh("kid");
        let (orig, dev) = (Keys::from_seed([1u8; 32]), Keys::from_seed([2u8; 32]));
        let acct = orig.address();
        c.faucet(&acct, 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let Action::AddKey { key_id: kid, label, role, public_key, pop } = crate::types::add_key(&dev, &c.genesis.chain_id, &c.genesis_hash, &acct, "phone", KeyRole::Owner) else { unreachable!() };
        let upper = Action::AddKey { key_id: kid.to_uppercase(), label, role, public_key, pop };
        assert!(c.submit(signed_fee(&c, &acct, &orig, 0, upper.clone(), None)).unwrap_err().to_string().contains("lowercase"));
        // Even if one got into a block, execution rejects it (no bricked account).
        let tx = signed_fee(&c, &acct, &orig, 0, upper, None);
        c.produce_block_with(vec![tx]).unwrap();
        assert_eq!(c.state.keys_of(&acct).len(), 1);
    }

    #[test]
    fn uppercase_address_alias_rejected() {
        let (_h, mut c) = fresh("case");
        let orig = Keys::from_seed([1u8; 32]);
        let upper = orig.address().to_uppercase();
        assert!(!crypto::is_address(&upper));
        c.faucet(&orig.address(), 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let to_upper = signed_fee(&c, &orig.address(), &orig, 0, Action::Transfer { to: upper.clone(), amount: DROPS_PER_FANG }, None);
        assert!(c.submit(to_upper).is_err());
        let mut from_upper = signed_fee(&c, &orig.address(), &orig, 0, Action::Transfer { to: c.me().unwrap(), amount: 1 }, None);
        from_upper.from = upper;
        assert!(c.submit(from_upper.sign(&orig)).is_err());
    }

    #[test]
    fn register_validator_requires_proof_of_possession() {
        let (_h, mut c) = fresh("squat");
        let (attacker, victim) = (Keys::from_seed([4u8; 32]), Keys::from_seed([6u8; 32]));
        c.faucet(&attacker.address(), 100 * DROPS_PER_FANG).unwrap();
        c.faucet(&victim.address(), 100 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let reg = |owner: &str, signer_seed: [u8; 32], p2p: &str| Action::RegisterValidator {
            consensus_key: hex::encode(victim.public_key()),
            p2p_addr: p2p.into(),
            http_addr: "1.2.3.4:1".into(),
            bond: 50 * DROPS_PER_FANG,
            pop: make_pop(signer_seed, "audit", owner, p2p, "1.2.3.4:1"),
        };
        // Attacker can't produce a PoP for the victim's key.
        let squat = reg(&attacker.address(), [4u8; 32], "6.6.6.6:1");
        let err = c.submit(signed_fee(&c, &attacker.address(), &attacker, 0, squat, None)).unwrap_err().to_string();
        assert!(err.contains("pop"), "{err}");
        // Nor replay the victim's PoP for its own registration (it binds the owner).
        let stolen = reg(&victim.address(), [6u8; 32], "1.2.3.4:2");
        assert!(c.submit(signed_fee(&c, &attacker.address(), &attacker, 0, stolen.clone(), None)).is_err());
        c.submit(signed_fee(&c, &victim.address(), &victim, 0, stolen, None)).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.validators[&victim.address()].p2p_addr, "1.2.3.4:2");
    }

    #[test]
    fn failed_txs_pay_their_fee_and_report_status() {
        let (home, mut c) = fresh("fail");
        let a = Keys::from_seed([8u8; 32]);
        c.faucet(&a.address(), 10 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        // Two transfers that each fit alone but not together: the second fails at execution.
        let t1 = signed_fee(&c, &a.address(), &a, 0, Action::Transfer { to: c.me().unwrap(), amount: 6 * DROPS_PER_FANG }, None);
        let t2 = signed_fee(&c, &a.address(), &a, 1, Action::Transfer { to: c.me().unwrap(), amount: 6 * DROPS_PER_FANG }, None);
        let fees = t1.fee + t2.fee;
        c.produce_block_with(vec![t1, t2.clone()]).unwrap();
        let acct = c.state.account(&a.address());
        assert_eq!(acct.nonce, 2, "failed tx used its nonce");
        assert_eq!(acct.balance, 10 * DROPS_PER_FANG - 6 * DROPS_PER_FANG - fees, "failed tx paid its fee");
        assert!(c.failure(&t2.id()).unwrap().0.contains("insufficient"));
        let (h, i) = c.tx_location(&t2.id()).unwrap();
        assert!(c.block(h).unwrap().failed.contains_key(&i));
        // New accounts need a minimum transfer.
        let dust = signed_fee(&c, &a.address(), &a, 2, Action::Transfer { to: Keys::from_seed([77; 32]).address(), amount: 1 }, None);
        assert!(c.submit(dust).unwrap_err().to_string().contains("new account"));
        // Replays identically.
        let root = c.state.root();
        drop(c);
        assert_eq!(Chain::open(&home).unwrap().state.root(), root);
    }

    /// Audit F31: replay (crash recovery, rebuilding the database from the logs) compared a failed
    /// tx's reason byte for byte with the log, so a release that reworded an error message made
    /// every node holding such a block refuse to open. It compares the kind of outcome now (the
    /// state root, checked after each block, is what must match); a tx that applies where the
    /// log says it failed still stops it.
    #[test]
    fn replay_compares_outcomes_not_failure_wording() {
        let (home, mut c) = fresh("reword");
        let a = Keys::from_seed([8u8; 32]);
        c.faucet(&a.address(), 10 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let pay = |c: &Chain, nonce: u64| signed_fee(c, &a.address(), &a, nonce, Action::Transfer { to: c.me().unwrap(), amount: 6 * DROPS_PER_FANG }, None);
        let (t1, t2) = (pay(&c, 0), pay(&c, 1));
        let failing = t2.id();
        c.produce_block_with(vec![t1, t2]).unwrap();
        let (h, i) = c.tx_location(&failing).unwrap();
        assert!(c.block(h).unwrap().failed[&i].starts_with("insufficient"));
        let (root, height) = (c.state.root(), c.height());
        drop(c);
        // Edits the block log in place (same length, so every offset stays put).
        let edit = |from: &str, to: &str| {
            assert_eq!(from.len(), to.len());
            let dir = home.join("blocks");
            let seg = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).find(|p| fs::read(p).unwrap().windows(from.len()).any(|w| w == from.as_bytes())).unwrap();
            let bytes = fs::read(&seg).unwrap();
            let at = bytes.windows(from.len()).position(|w| w == from.as_bytes()).unwrap();
            let mut out = bytes.clone();
            out[at..at + from.len()].copy_from_slice(to.as_bytes());
            fs::write(&seg, out).unwrap();
        };
        // A release that words the failure differently rebuilds the database just the same.
        edit("\"insufficient", "\"INSUFFICIENT");
        fs::remove_file(home.join(store::DB_FILE)).unwrap();
        let c = Chain::open(&home).unwrap();
        assert_eq!((c.state.root(), c.height(), c.replayed), (root, height, height));
        assert!(c.block(h).unwrap().failed[&i].starts_with("INSUFFICIENT"), "the log's record is what reads show");
        drop(c);
        // The log saying the other tx failed is a divergence.
        edit(&format!("\"failed\":{{\"{i}\""), &format!("\"failed\":{{\"{}\"", i - 1));
        fs::remove_file(home.join(store::DB_FILE)).unwrap();
        let err = format!("{:#}", Chain::open(&home).err().unwrap());
        assert!(err.contains(&format!("tx {} applied, but the log recorded it failing", i - 1)), "{err}");
        fs::remove_dir_all(home).ok();
    }

    /// review-r2-gov-1: admission dry-runs a tx on the live state when its sender has nothing
    /// queued, so a panic inside it would leave the fee debited and the nonce bumped with no
    /// rollback. The upgrade checks can't panic, whatever the schedule holds: before, once a
    /// `u32::MAX` was scheduled and withdrawn, the refusal's `spent + 1` overflowed in any later
    /// upgrade proposal's dry run (and in the block that carried it, on every validator). The
    /// withdrawal is set directly here (versions now move in small steps, so no chain gets there).
    #[test]
    fn an_upgrade_proposal_is_refused_cleanly_whatever_was_withdrawn() {
        let (_h, mut c) = fresh("upgrade-admit");
        let o = Keys::from_seed([1u8; 32]);
        let deposit = c.state.chain_params.proposal_deposit;
        c.faucet(&o.address(), deposit + 10 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        c.state.upgrade.withdrawals = crate::upgrade::Withdrawals { highest: u32::MAX, last_at: Some(1), superseded: u32::MAX };
        let at = |c: &Chain| (c.state.full_root(), c.state.account(&o.address()).balance, c.state.account(&o.address()).nonce);
        let before = at(&c);
        let cp = &c.state.chain_params;
        let activation_height = c.height() + 2 * (cp.voting_blocks + cp.timelock_blocks) + 1_000;
        for version in [2, u32::MAX] {
            let propose = Action::Propose { kind: ProposalKind::ScheduleUpgrade { version, activation_height }, deposit, title: "v2".into(), url: String::new() };
            let e = c.submit(signed_fee(&c, &o.address(), &o, 0, propose, None)).unwrap_err().to_string();
            assert!(e.contains(&format!("isn't newer than v{}", u32::MAX)) && e.contains(&format!("as v{} or later", u32::MAX)), "{e}");
            assert_eq!(at(&c), before, "the live state is as it was");
        }
        assert!(crate::upgrade::check_not_superseded(&c.state, 1, u32::MAX).unwrap_err().to_string().contains(&format!("as v{} or later", u32::MAX)));
    }

    #[test]
    fn session_key_bounds_and_names() {
        let (_h, mut c) = fresh("sess");
        let (o, s) = (Keys::from_seed([1u8; 32]), Keys::from_seed([2u8; 32]));
        c.faucet(&o.address(), 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let (cid, gh) = (c.genesis.chain_id.clone(), c.genesis_hash.clone());
        let zero = crate::types::add_key(&s, &cid, &gh, &o.address(), "a", KeyRole::Session { limit: 1, period_blocks: 0, expires_at: 10 });
        assert!(c.submit(signed_fee(&c, &o.address(), &o, 0, zero, None)).is_err());
        let forever = crate::types::add_key(&s, &cid, &gh, &o.address(), "a", KeyRole::Session { limit: 1, period_blocks: 1, expires_at: u64::MAX });
        assert!(c.submit(signed_fee(&c, &o.address(), &o, 0, forever, None)).unwrap_err().to_string().contains("at most"));
        assert!(normalize_name("\u{212A}im").is_err(), "Kelvin sign doesn't fold to k");
        assert!(normalize_name(" jack").is_err());
        assert!(normalize_name("jack ").is_err());
        assert_eq!(normalize_name("Jack.boo").unwrap(), "jack");
    }

    #[test]
    fn claimed_network_height_is_display_only_and_robust_to_one_liar() {
        let (_h, mut c) = fresh("claims");
        assert!(!c.syncing());
        c.note_claim("a", 1_000);
        assert_eq!(c.network_height_claimed(), 1_000, "shown as the network's height");
        assert!(c.live.syncing(c.height()), "the API reports it");
        assert!(!c.syncing(), "but a claim never gates validator duties");
        assert_eq!(c.finalized_tip, 0, "claims never move the verified tip");
        // One liar among three can't move the median.
        c.note_claim("a", 2);
        c.note_claim("b", 1);
        c.note_claim("liar", u64::MAX);
        assert_eq!(c.network_height_claimed(), 2);
        assert!(!c.syncing());
    }

    /// Audit F11: a majority of a validator's HTTP peers lying about the tip (once, during its
    /// startup catch-up) used to pin `syncing()` for the life of the process, so it silently
    /// skipped its beacon reveals (jailed and slashed), evidence and attestations. Duties now
    /// wait only on heights this node verified.
    #[test]
    fn lying_peers_cant_hold_back_validator_duties() {
        let (_h, mut c) = fresh("liars");
        c.note_claim("honest", c.height());
        c.note_claim("liar-1", 1 << 40);
        c.note_claim("liar-2", 1 << 40);
        assert_eq!(c.network_height_claimed(), 1 << 40, "two of three peers lie");
        for _ in 0..20 {
            c.produce_block_with(vec![]).unwrap();
            assert!(!c.syncing(), "duties carry on at height {}", c.height());
        }
        // A finalized height this node verified (a certificate, marshal's tip) does gate them.
        let h = c.height();
        c.set_finalized_tip(h + SYNC_SLACK + 1);
        assert!(c.syncing());
        c.set_finalized_tip(h + SYNC_SLACK);
        assert!(c.syncing(), "the verified tip never goes back");
        for _ in 0..=SYNC_SLACK {
            c.produce_block_with(vec![]).unwrap();
        }
        assert!(!c.syncing(), "caught up");
    }

    #[test]
    fn faucet_has_a_global_budget() {
        let (_h, mut c) = fresh("budget");
        for i in 0..FAUCET_MAX_PENDING as u8 {
            c.faucet(&Keys::from_seed([200u8.wrapping_add(i); 32]).address(), DROPS_PER_FANG).unwrap();
        }
        assert!(c.faucet_busy());
        assert!(c.faucet(&Keys::from_seed([1; 32]).address(), DROPS_PER_FANG).unwrap_err().to_string().contains("faucet busy"));
        c.produce_block().unwrap();
        assert!(!c.faucet_busy());
    }

    /// 5,000 queued txs: admitting one more must not re-apply the whole mempool.
    #[test]
    fn mempool_admission_stays_fast_when_full() {
        let (_h, mut c) = fresh("mp");
        let gh = c.genesis_hash.clone();
        let senders: Vec<Keys> = (0..79u32).map(|i| Keys::from_seed(crypto::hash(&i.to_le_bytes()))).collect();
        for k in &senders {
            c.state.accounts.insert(k.address(), Account { balance: 1_000 * DROPS_PER_FANG, ..Default::default() });
            c.state.supply += 1_000 * DROPS_PER_FANG;
        }
        let me = c.me().unwrap();
        let t = Instant::now();
        'fill: for k in &senders {
            for n in 0..MAX_PENDING_PER_SENDER as u64 {
                if c.mempool.len() >= MAX_MEMPOOL {
                    break 'fill;
                }
                c.submit(unsigned(&gh, k, n, Action::Transfer { to: me.clone(), amount: 1 })).unwrap();
            }
        }
        let fill = t.elapsed();
        assert_eq!(c.mempool.len(), MAX_MEMPOOL);
        let x = Keys::from_seed([77u8; 32]);
        c.state.accounts.insert(x.address(), Account { balance: 100 * DROPS_PER_FANG, ..Default::default() });
        c.state.supply += 100 * DROPS_PER_FANG;
        let mut tx = unsigned(&gh, &x, 0, Action::Transfer { to: me.clone(), amount: 1 });
        let lowball = Tx { tip: 0, ..tx.clone() };
        assert!(c.submit(lowball).unwrap_err().to_string().contains("mempool is full; resubmit with a tip of at least 1 drops"));
        tx.tip = 1; // outbids the cheapest (no tip), so it evicts one
        tx.fee = tx.charge().unwrap();
        let t = Instant::now();
        c.submit(tx).unwrap();
        let one = t.elapsed();
        println!("filled 5,000-tx mempool in {fill:?}; one more submit (with eviction) took {one:?}");
        assert!(one.as_millis() < 250, "submit took {one:?}");
        // The newest sender's last tx went (no one is left with a nonce gap).
        assert_eq!(c.mempool.len(), MAX_MEMPOOL);
        let mut next: HashMap<&str, u64> = HashMap::new();
        for t in &c.mempool {
            let n = next.entry(t.from.as_str()).or_insert(0);
            assert_eq!(t.nonce, *n, "nonces stay contiguous");
            *n += 1;
        }
        let last_sender = c.mempool.iter().rev().find(|t| t.from != x.address()).unwrap().from.clone();
        assert_eq!(next[last_sender.as_str()], (MAX_MEMPOOL - 64 * (MAX_MEMPOOL / 64)) as u64 - 1, "the newest arrival among the cheapest tails was evicted");
        let t = Instant::now();
        c.revalidate_mempool();
        println!("revalidating 5,000 txs: {:?}", t.elapsed());
    }

    /// An unsigned tx `from` an account, by `signer`, paying `tip` (fee = the exact charge).
    fn unsigned_as(genesis_hash: &str, from: &str, signer: &Keys, nonce: u64, tip: u64, action: Action) -> Tx {
        let mut tx = Tx { chain_id: "audit".into(), genesis_hash: genesis_hash.into(), from: from.into(), nonce, fee: 0, tip, valid_until: 1_000, action, memo: String::new(), public_key: hex::encode(signer.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = tx.charge().unwrap();
        tx
    }

    /// Funds `owner`'s account straight in the state, with `agent` as a session key spending up
    /// to `limit` per 10,000 blocks.
    fn with_agent(c: &mut Chain, owner: &Keys, agent: &Keys, limit: u64, balance: u64) -> String {
        let rec = |label: &str, role| KeyRecord { label: label.into(), role, added_at: 0, spent: 0, window_start: 0 };
        let keys = BTreeMap::from([(key_id(&owner.public_key()), rec("owner", KeyRole::Owner)), (key_id(&agent.public_key()), rec("agent", KeyRole::Session { limit, period_blocks: 10_000, expires_at: 10_000 }))]);
        c.state.accounts.insert(owner.address(), Account { balance, keys, ..Default::default() });
        c.state.supply += balance;
        owner.address()
    }

    fn fund(c: &mut Chain, k: &Keys, balance: u64) {
        c.state.accounts.insert(k.address(), Account { balance, ..Default::default() });
        c.state.supply += balance;
    }

    /// Audit #7: a relaunched chain that keeps its id (the testnet re-summoned) has another genesis,
    /// so txs signed on the old instance don't replay on it, though nonces and balances line up.
    #[test]
    fn txs_dont_replay_on_a_relaunch_with_the_same_chain_id() {
        let (_h1, mut old) = fresh("relaunch-old");
        let (_h2, mut new) = fresh("relaunch-new");
        assert_eq!(old.genesis.chain_id, new.genesis.chain_id);
        assert_ne!(old.genesis_hash, new.genesis_hash);
        let a = Keys::from_seed([3; 32]);
        for c in [&mut old, &mut new] {
            fund(c, &a, 10 * DROPS_PER_FANG);
            c.produce_block_with(vec![]).unwrap();
        }
        let pay = Action::Transfer { to: Keys::from_seed([4; 32]).address(), amount: DROPS_PER_FANG };
        let tx = signed_fee(&old, &a.address(), &a, 0, pay, None);
        crate::consensus::check_tx_stateless(&tx, "audit", &old.genesis_hash, "boo").unwrap();
        old.submit(tx.clone()).unwrap();
        // HTTP, gossip and block verification run check_tx_stateless; the mempool checks too.
        let err = crate::consensus::check_tx_stateless(&tx, "audit", &new.genesis_hash, "boo").unwrap_err().to_string();
        assert!(err.contains("another instance of audit"), "{err}");
        assert!(new.submit(tx.clone()).unwrap_err().to_string().contains("another instance"));
        // Pointing it at the new instance breaks its signature.
        let moved = Tx { genesis_hash: new.genesis_hash.clone(), ..tx };
        assert!(crate::consensus::check_tx_stateless(&moved, "audit", &new.genesis_hash, "boo").is_err());
    }

    /// Audit #10, PoC 1: the owner queues `RemoveKey(agent)`, then the agent queues transfers.
    /// Admission used to check them against the state before the removal (it only reserved
    /// balance and bumped the nonce), so 63 of 64 got in though none could ever execute. Now the
    /// dry run applies the removal first and refuses them. In the other order everything the
    /// agent queued before the removal executes.
    #[test]
    fn txs_behind_a_key_removal_are_refused() {
        let (_h, mut c) = fresh("rmkey");
        let gh = c.genesis_hash.clone();
        let (owner, agent) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]));
        let acct = with_agent(&mut c, &owner, &agent, 1_000 * DROPS_PER_FANG, 1_000 * DROPS_PER_FANG);
        c.produce_block_with(vec![]).unwrap();
        let me = c.me().unwrap();
        let pay = |signer: &Keys, n: u64| unsigned_as(&gh, &acct, signer, n, 1, Action::Transfer { to: me.clone(), amount: 0 });
        let (agent_kid, remove) = (key_id(&agent.public_key()), Action::RemoveKey { key_id: key_id(&agent.public_key()) });
        c.submit(unsigned_as(&gh, &acct, &owner, 0, 0, remove.clone())).unwrap();
        // None gets in (each is a different tx for the next nonce).
        for amount in 0..MAX_PENDING_PER_SENDER as u64 - 1 {
            let err = c.submit(unsigned_as(&gh, &acct, &agent, 1, 1, Action::Transfer { to: me.clone(), amount })).unwrap_err().to_string();
            assert!(err.contains("isn't authorized"), "{err}");
        }
        // A full node tracking a forwarded tx runs the same check.
        assert!(c.track_pending(pay(&agent, 1)).unwrap_err().to_string().contains("isn't authorized"));
        assert_eq!(c.mempool.len(), 1);
        c.submit(pay(&owner, 1)).unwrap();
        c.produce_block().unwrap();
        let b = c.block(c.height()).unwrap();
        assert_eq!((b.txs.len(), b.skipped.len(), b.failed.len()), (2, 0, 0));
        // The agent again, then 62 of its transfers and the owner's removal behind them: all in,
        // all executed.
        assert_eq!(agent_kid, key_id(&agent.public_key()));
        let add = crate::types::add_key(&agent, "audit", &gh, &acct, "agent", KeyRole::Session { limit: 1_000 * DROPS_PER_FANG, period_blocks: 100, expires_at: 10_000 });
        c.submit(unsigned_as(&gh, &acct, &owner, 2, 0, add)).unwrap();
        c.produce_block().unwrap();
        for n in 3..65 {
            c.submit(pay(&agent, n)).unwrap();
        }
        c.submit(unsigned_as(&gh, &acct, &owner, 65, 0, remove)).unwrap();
        assert_eq!(c.mempool.len(), 63);
        c.produce_block().unwrap();
        let b = c.block(c.height()).unwrap();
        assert_eq!((b.txs.len(), b.skipped.len(), b.failed.len()), (63, 0, 0));
    }

    /// Audit #10, PoC 2: a session key whose limit covers one tx's charge queues 64. They used to
    /// be admitted on the account's balance alone; now the second sees the first's spending.
    #[test]
    fn txs_past_a_session_limit_are_refused() {
        let (_h, mut c) = fresh("limit");
        let gh = c.genesis_hash.clone();
        let (owner, agent) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]));
        let me = c.me().unwrap();
        let pay = |n: u64, tip: u64| unsigned_as(&gh, &owner.address(), &agent, n, tip, Action::Transfer { to: me.clone(), amount: 0 });
        with_agent(&mut c, &owner, &agent, pay(0, 1).charge().unwrap(), 100 * DROPS_PER_FANG);
        c.produce_block_with(vec![]).unwrap();
        c.submit(pay(0, 1)).unwrap();
        // None of the next nonce's candidates gets in, whatever it tips.
        for tip in 1..MAX_PENDING_PER_SENDER as u64 {
            let err = c.submit(pay(1, tip)).unwrap_err().to_string();
            assert!(err.contains("limit reached"), "{err}");
        }
        assert_eq!(c.mempool.len(), 1);
        c.produce_block().unwrap();
        let b = c.block(c.height()).unwrap();
        assert_eq!((b.txs.len(), b.skipped.len()), (1, 0));
    }

    /// Audit #10, PoC 3: 79 accounts with one-charge session keys used to hold all 5,000 slots
    /// with txs that could never execute: the next block collected fees for 79 of them, the
    /// mempool emptied, and the attacker refilled it for free. Now each account gets one slot,
    /// honest senders get in without evicting anyone, and every queued tx executes and pays.
    #[test]
    fn phantom_txs_cannot_fill_the_mempool() {
        let (_h, mut c) = fresh("fill");
        let gh = c.genesis_hash.clone();
        let me = c.me().unwrap();
        let accounts: Vec<(Keys, Keys)> = (0..79u32).map(|i| (Keys::from_seed(crypto::hash(&i.to_le_bytes())), Keys::from_seed(crypto::hash(&(i + 500).to_le_bytes())))).collect();
        let pay = |(owner, agent): &(Keys, Keys), n: u64| unsigned_as(&gh, &owner.address(), agent, n, 1, Action::Transfer { to: me.clone(), amount: 0 });
        let limit = pay(&accounts[0], 0).charge().unwrap();
        for (owner, agent) in &accounts {
            with_agent(&mut c, owner, agent, limit, 10 * DROPS_PER_FANG);
        }
        let honest = Keys::from_seed([99; 32]);
        fund(&mut c, &honest, 100 * DROPS_PER_FANG);
        c.produce_block_with(vec![]).unwrap();
        let mut admitted = 0;
        for a in &accounts {
            for n in 0..MAX_PENDING_PER_SENDER as u64 {
                admitted += c.submit(pay(a, n)).is_ok() as usize;
            }
        }
        assert_eq!((admitted, c.mempool.len()), (79, 79));
        for n in 0..MAX_PENDING_PER_SENDER as u64 {
            c.submit(unsigned(&gh, &honest, n, Action::Transfer { to: me.clone(), amount: 1 })).unwrap();
        }
        assert_eq!(c.mempool.len(), 79 + 64, "nothing was evicted");
        let burned = c.state.burned;
        c.produce_block().unwrap();
        let b = c.block(c.height()).unwrap();
        assert_eq!((b.txs.len(), b.skipped.len(), b.failed.len()), (79 + 64, 0, 0));
        assert_eq!(c.state.burned - burned, b.txs.iter().map(|t| t.charge().unwrap() - t.tip).sum::<u64>(), "every queued tx paid");
        assert!(c.mempool.is_empty());
    }

    /// Review of F36: what a ghost tx displaces (`displace`) takes its sender's later txs along
    /// when it's signed (a same-note replacement can hit a `Shield` anywhere in its sender's
    /// queue), so nobody is left behind a nonce gap, and the sender's simulation goes with it: it
    /// can submit again at once, from the nonce that's free.
    #[test]
    fn a_displaced_signed_tx_takes_its_senders_later_txs() {
        let (_h, mut c) = fresh("displace");
        let gh = c.genesis_hash.clone();
        let me = c.me().unwrap();
        let a = Keys::from_seed([41; 32]);
        fund(&mut c, &a, 100 * DROPS_PER_FANG);
        c.produce_block_with(vec![]).unwrap();
        let pay = |n: u64| unsigned_as(&gh, &a.address(), &a, n, 0, Action::Transfer { to: me.clone(), amount: n + 1 });
        let ids: Vec<String> = (0..3).map(|n| c.submit(pay(n)).unwrap()).collect();
        let next = c.height() + 1;
        c.displace(vec![(ids[1].clone(), "replaced by a transaction spending the same note with a higher tip")], next);
        assert!(c.in_mempool(&ids[0]) && !c.in_mempool(&ids[1]) && !c.in_mempool(&ids[2]));
        assert!(c.failure(&ids[1]).unwrap().0.contains("same note") && c.failure(&ids[2]).unwrap().0.contains("earlier transaction from the same account was displaced"));
        assert_eq!(c.next_nonce(&a.address()), 1);
        let again = Tx { tip: 1, ..pay(1) };
        let again = Tx { fee: again.charge().unwrap(), ..again };
        c.submit(again).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.account(&a.address()).nonce, 2);
    }

    /// Review of fix-fees: a full mempool's refusal never asks for a tip past the newcomer's cap
    /// (`tip_cap_multiple` × its base fee). With every tail tipping its cap, the same-size
    /// newcomer would need a drop more than its own: it's told the mempool is full of txs it
    /// can't outbid, and to try again, not to pay a tip admission refuses as "tip too high".
    #[test]
    fn a_full_mempool_never_asks_for_a_tip_past_the_cap() {
        let (_h, mut c) = fresh("tipcap");
        let gh = c.genesis_hash.clone();
        let me = c.me().unwrap();
        let senders: Vec<Keys> = (0..80u32).map(|i| Keys::from_seed(crypto::hash(&(i + 9_000).to_le_bytes()))).collect();
        for k in &senders {
            fund(&mut c, k, 1_000 * DROPS_PER_FANG);
        }
        let pay = |k: &Keys, n: u64, tip: u64| unsigned_as(&gh, &k.address(), k, n, tip, Action::Transfer { to: me.clone(), amount: 1 });
        let cap = crate::econ::max_tip(&c.state, crate::econ::base_fee(&c.state, c.height() + 1, pay(&senders[0], 0, 0).size()));
        'fill: for k in &senders[..79] {
            for n in 0..MAX_PENDING_PER_SENDER as u64 {
                if c.mempool.len() >= MAX_MEMPOOL {
                    break 'fill;
                }
                c.submit(pay(k, n, cap)).unwrap();
            }
        }
        assert_eq!(c.mempool.len(), MAX_MEMPOOL);
        let x = &senders[79];
        let err = c.submit(pay(x, 0, cap)).unwrap_err().to_string();
        assert!(err.contains("tip more per byte than this one may") && err.contains(&format!("at most {cap} drops")) && !err.contains("resubmit"), "{err}");
        assert!(c.submit(pay(x, 0, cap + 1)).unwrap_err().to_string().contains("tip too high"), "what the hint used to ask for");
    }

    /// The validator-duty lane (F32/F56; the full-mempool case is in beacon/tests.rs): only a
    /// validator's own duties, signed by its consensus key, at most `DUTY_LANE_PER_KIND` queued
    /// per validator and kind, forgotten once they leave the mempool.
    #[test]
    fn the_duty_lane_is_bounded_and_only_takes_validators_duties() {
        let (_h, mut c) = fresh("lane");
        let gh = c.genesis_hash.clone();
        let me = c.me().unwrap();
        let vk = c.keys.as_ref().unwrap().clone_keys();
        let stranger = Keys::from_seed([7; 32]);
        fund(&mut c, &stranger, 100 * DROPS_PER_FANG);
        c.produce_block_with(vec![]).unwrap();
        let commit = Action::BeaconCommit { epoch: 2, commitment: "ab".repeat(32) };
        let duty = unsigned_as(&gh, &me, &vk, 0, 0, commit.clone());
        assert_eq!(c.duty_slot(&duty), Some((me.clone(), "beacon_commit")));
        // Another key, another action, or the validator's key for an account it doesn't act for:
        // no duty.
        assert_eq!(c.duty_slot(&unsigned_as(&gh, &stranger.address(), &stranger, 0, 0, commit.clone())), None);
        assert_eq!(c.duty_slot(&unsigned_as(&gh, &me, &vk, 0, 0, Action::Transfer { to: stranger.address(), amount: 1 })), None);
        assert_eq!(c.duty_slot(&unsigned_as(&gh, &stranger.address(), &vk, 0, 0, commit.clone())), None);
        // At most DUTY_LANE_PER_KIND per validator and kind.
        for i in 0..DUTY_LANE_PER_KIND {
            c.lane.insert(format!("queued-{i}"), (me.clone(), "beacon_commit"));
        }
        assert_eq!(c.duty_slot(&duty), None);
        let reveal = unsigned_as(&gh, &me, &vk, 0, 0, Action::BeaconReveal { epoch: 1, secret: "cd".repeat(32) });
        assert_eq!(c.duty_slot(&reveal), Some((me.clone(), "beacon_reveal")));
        c.lane.clear();
        // Queued in the lane, then included and forgotten.
        let id = c.submit(duty).unwrap();
        assert_eq!((c.lane.len(), c.regular_len()), (1, 0));
        assert!(c.lane.contains_key(&id));
        c.produce_block().unwrap();
        assert!(c.has_tx(&id) && c.lane.is_empty() && c.mempool.is_empty());
    }

    /// Dropping a queued tx takes its sender's later txs along (they'd wait behind a nonce gap
    /// forever), but a tx whose nonce another tx used goes alone.
    #[test]
    fn dropped_txs_take_their_successors() {
        let (_h, mut c) = fresh("gap");
        let gh = c.genesis_hash.clone();
        let (a, b) = (Keys::from_seed([5; 32]), Keys::from_seed([6; 32]));
        fund(&mut c, &a, 100 * DROPS_PER_FANG);
        fund(&mut c, &b, 100 * DROPS_PER_FANG);
        c.produce_block_with(vec![]).unwrap();
        let me = c.me().unwrap();
        let pay = |n: u64, amount: u64| unsigned(&gh, &a, n, Action::Transfer { to: me.clone(), amount });
        let ids: Vec<String> = (0..3).map(|n| c.submit(pay(n, 1)).unwrap()).collect();
        // A full node whose forward failed drops the first: all three go.
        c.drop_pending(&ids[0]);
        assert!(c.mempool.is_empty());
        assert_eq!(c.next_nonce(&a.address()), 0);
        // Another tx used nonce 0: the queued nonce-0 tx goes, the ones after it stay.
        for n in 0..3 {
            c.submit(pay(n, 1)).unwrap();
        }
        c.produce_block_with(vec![pay(0, 2)]).unwrap();
        assert_eq!(c.mempool.iter().map(|t| t.nonce).collect::<Vec<_>>(), vec![1, 2]);
        // Nonce 1 registers a name someone else takes first: it can't run, so nonce 2 goes too.
        c.drop_pending(&c.mempool_ids[0].clone());
        c.submit(unsigned(&gh, &a, 1, Action::RegisterName { name: "taken".into() })).unwrap();
        c.submit(pay(2, 1)).unwrap();
        c.produce_block_with(vec![unsigned(&gh, &b, 0, Action::RegisterName { name: "taken".into() })]).unwrap();
        assert!(c.mempool.is_empty());
        let (h, _) = (c.height(), ());
        assert!(c.failure(&pay(2, 1).id()).unwrap().0.contains("an earlier transaction from the same account was dropped"), "at {h}");
    }

    /// Admission applies a sender's queued txs before the new one, from a cached simulation:
    /// the 64th tx from a sender costs one dry run, like the first. After a block the cache is
    /// rebuilt once per sender, from its queue (at most 63 dry runs).
    #[test]
    fn admission_cost_doesnt_grow_with_the_queue() {
        let (_h, mut c) = fresh("flat");
        let gh = c.genesis_hash.clone();
        let me = c.me().unwrap();
        let (x, y) = (Keys::from_seed([40; 32]), Keys::from_seed([41; 32]));
        fund(&mut c, &x, 100 * DROPS_PER_FANG);
        fund(&mut c, &y, 100 * DROPS_PER_FANG);
        c.produce_block_with(vec![]).unwrap();
        let pay = |k: &Keys, n: u64| unsigned(&gh, k, n, Action::Transfer { to: me.clone(), amount: 1 });
        let mut times = vec![];
        for n in 0..MAX_PENDING_PER_SENDER as u64 {
            let tx = pay(&x, n);
            let t = Instant::now();
            c.submit(tx).unwrap();
            times.push(t.elapsed());
        }
        assert_eq!(c.sims.rebuilt, 1, "only the second tx simulated the queue (of one)");
        let median = |t: &[std::time::Duration]| {
            let mut t = t.to_vec();
            t.sort();
            t[t.len() / 2]
        };
        let (early, late) = (median(&times[1..9]), median(&times[56..64]));
        println!("admission: txs 2-9 of a sender {early:?} each (median), txs 57-64 {late:?}");
        for n in 0..63 {
            c.submit(pay(&y, n)).unwrap();
        }
        // A block (with neither sender's txs) makes the simulations stale: y's next tx rebuilds
        // its own from its 63 queued txs, once.
        c.produce_block_with(vec![]).unwrap();
        let rebuilt = c.sims.rebuilt;
        let t = Instant::now();
        let err = c.submit(pay(&x, 64)).unwrap_err().to_string();
        assert!(err.contains("too many pending"), "{err}");
        c.submit(pay(&y, 63)).unwrap();
        let miss = t.elapsed();
        assert_eq!(c.sims.rebuilt - rebuilt, 63);
        println!("after a block: a tx behind 63 queued ones took {miss:?} (the queue simulated once)");
        assert!(miss.as_millis() < 1_000, "{miss:?}");
    }

    /// 200k accounts and a full 512-tx block: no per-tx state clone. Timed is the execution
    /// (applying the txs, the end-of-block steps, the state commit); writing the block (fsyncs
    /// and the database commit) is printed but not bounded, since the parallel test suite's
    /// other fsyncs make it as slow as the disk is busy.
    #[test]
    fn big_state_block_execution() {
        let (_h, mut c) = fresh("clone");
        let gh = c.genesis_hash.clone();
        for i in 0..200_000u64 {
            c.state.accounts.insert(crypto::address_from_bytes(&crypto::hash(&i.to_le_bytes())), Account { balance: DROPS_PER_FANG, ..Default::default() });
        }
        let senders: Vec<Keys> = (0..8u32).map(|i| Keys::from_seed(crypto::hash(&(i + 1000).to_le_bytes()))).collect();
        for k in &senders {
            c.state.accounts.insert(k.address(), Account { balance: 1_000 * DROPS_PER_FANG, ..Default::default() });
        }
        c.state.supply += 200_000 * DROPS_PER_FANG + 8 * 1_000 * DROPS_PER_FANG;
        // The accounts were inserted behind the chain's back: an untimed block writes them to the
        // state database (one 200k-record commit), so the timed one is a block over a big state.
        c.produce_block_with(vec![]).unwrap();
        let txs: Vec<Tx> = (0..512u64).map(|i| unsigned(&gh, &senders[(i % 8) as usize], i / 8, Action::Transfer { to: crypto::address_from_bytes(&crypto::hash(&i.to_le_bytes())), amount: 1 })).collect();
        let f = c.test_block(txs);
        let t = Instant::now();
        let mut e = c.execute_in_memory(f).unwrap();
        let took = t.elapsed();
        let written = c.writer.clone().lock().persist(&mut e);
        c.finish(e, written).unwrap();
        println!("512-tx block over 200k accounts executed in {took:?}, written in {:?}", t.elapsed() - took);
        assert_eq!(c.block(c.height()).unwrap().txs.len(), 512);
        assert!(took.as_secs_f64() < if cfg!(debug_assertions) { 10.0 } else { 1.0 }, "{took:?}");
    }
}

#[cfg(test)]
mod hunts {
    use super::*;

    struct T {
        home: PathBuf,
        c: Chain,
        admin: Keys,
    }

    fn setup(tag: &str) -> T {
        let home = std::env::temp_dir().join(format!("blockula-hunts-{tag}-{}-{}", now_ms(), std::process::id()));
        Chain::summon(&home, &SummonOpts::new("hunts", 1)).unwrap();
        let seed: [u8; 32] = hex::decode(fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let mut c = Chain::open(&home).unwrap();
        for i in 1..=4u8 {
            c.faucet(&Keys::from_seed([i; 32]).address(), 100 * DROPS_PER_FANG).unwrap();
        }
        c.produce_block().unwrap();
        T { home, c, admin: Keys::from_seed(seed) }
    }

    impl T {
        fn tx(&mut self, who: &Keys, action: Action) -> Result<String> {
            let from = who.address();
            let mut tx = Tx { chain_id: "hunts".into(), genesis_hash: self.c.genesis_hash.clone(), nonce: self.c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: hex::encode(who.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size());
            let id = self.c.submit(tx.sign(who))?;
            self.c.produce_block()?;
            match self.c.failure(&id) {
                Some((r, _)) => bail!(r.clone()),
                None => Ok(id),
            }
        }
        fn bal(&self, k: &Keys) -> u64 {
            self.c.state.account(&k.address()).balance
        }
        fn blocks(&mut self, n: u64) {
            for _ in 0..n {
                self.c.produce_block_with(vec![]).unwrap();
            }
        }
    }

    fn create(repo: &str, issue: u64, amount: u64, expires_at: u64, title: &str) -> Action {
        Action::CreateHunt { repo: repo.into(), issue, amount, expires_at, title: title.into() }
    }

    #[test]
    fn create_fund_award_and_double_award() {
        let mut t = setup("award");
        let (alice, bob, carol) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]), Keys::from_seed([3; 32]));
        let h = t.c.height();
        // Validation: the seeded issues' repo/numbers work; bad inputs don't.
        assert!(t.tx(&alice, create("jackdishman/blockula", 1, 5 * DROPS_PER_FANG, h + 100, "too small")).unwrap_err().to_string().contains("at least"));
        assert!(t.tx(&alice, create("jackdishman/blockula", 1, 10 * DROPS_PER_FANG, h, "past")).is_err());
        assert!(t.tx(&alice, create("jackdishman/blockula", 1, 10 * DROPS_PER_FANG, h + 40_000_000, "forever")).unwrap_err().to_string().contains("about a year"));
        assert!(t.tx(&alice, create("not a repo", 1, 10 * DROPS_PER_FANG, h + 100, "x")).is_err());
        assert!(t.tx(&alice, create("jackdishman/blockula", 0, 10 * DROPS_PER_FANG, h + 100, "x")).is_err());
        assert!(t.tx(&alice, create("jackdishman/blockula", 1, 10 * DROPS_PER_FANG, h + 100, &"t".repeat(97))).is_err());
        let before = t.bal(&alice);
        t.tx(&alice, create("jackdishman/blockula", 1, 50 * DROPS_PER_FANG, h + 100, "Shared tx test vectors")).unwrap();
        assert!(t.bal(&alice) < before - 50 * DROPS_PER_FANG + 1, "escrow debited");
        let id = *t.c.state.hunts.keys().next().unwrap();
        t.tx(&bob, Action::FundHunt { hunt_id: id, amount: 5 * DROPS_PER_FANG }).unwrap();
        t.tx(&bob, Action::FundHunt { hunt_id: id, amount: 2 * DROPS_PER_FANG }).unwrap();
        assert_eq!(t.c.state.hunts[&id].amount, 57 * DROPS_PER_FANG);
        assert_eq!(t.c.state.hunts[&id].contribution(&bob.address()), 7 * DROPS_PER_FANG);
        assert!(t.tx(&bob, Action::FundHunt { hunt_id: id, amount: 1 }).is_err(), "min funding");
        // Only the creator awards (GOV-2): not a funder, not an admin.
        assert!(t.tx(&bob, Action::AwardHunt { hunt_id: id, to: bob.address(), pr: 9, familiar_key: None }).unwrap_err().to_string().contains("only the hunt's creator"));
        let admin = t.admin.clone_keys();
        assert!(t.tx(&admin, Action::AwardHunt { hunt_id: id, to: admin.address(), pr: 9, familiar_key: None }).unwrap_err().to_string().contains("only the hunt's creator"));
        let c0 = t.bal(&carol);
        t.tx(&alice, Action::AwardHunt { hunt_id: id, to: carol.address(), pr: 9, familiar_key: None }).unwrap();
        assert_eq!(t.bal(&carol), c0 + 57 * DROPS_PER_FANG);
        assert!(!t.c.state.hunts[&id].self_awarded());
        assert!(t.tx(&alice, Action::AwardHunt { hunt_id: id, to: bob.address(), pr: 9, familiar_key: None }).unwrap_err().to_string().contains("already"));
        assert!(t.tx(&t.admin.clone_keys(), Action::CancelHunt { hunt_id: id }).is_err(), "awarded hunts can't be cancelled");
        assert!(t.tx(&bob, Action::FundHunt { hunt_id: id, amount: DROPS_PER_FANG }).is_err());
        // Self-award is allowed (and flagged).
        t.tx(&bob, create("jackdishman/blockula", 4, 10 * DROPS_PER_FANG, t.c.height() + 50, "QR")).unwrap();
        let id2 = t.c.state.hunts.keys().next_back().copied().unwrap();
        t.tx(&bob, Action::AwardHunt { hunt_id: id2, to: bob.address(), pr: 3, familiar_key: None }).unwrap();
        assert!(t.c.state.hunts[&id2].self_awarded());
        // Everything replays to the same root.
        let root = t.c.state.root();
        drop(t.c);
        assert_eq!(Chain::open(&t.home).unwrap().state.root(), root);
        fs::remove_dir_all(&t.home).ok();
    }

    #[test]
    fn cancel_refunds_funders_pro_rata() {
        let mut t = setup("cancel");
        let (alice, bob, carol) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]), Keys::from_seed([3; 32]));
        let exp = t.c.height() + 5;
        t.tx(&alice, create("jackdishman/blockula", 2, 20 * DROPS_PER_FANG, exp, "fuzz targets")).unwrap();
        let id = *t.c.state.hunts.keys().next().unwrap();
        t.tx(&bob, Action::FundHunt { hunt_id: id, amount: 3 * DROPS_PER_FANG }).unwrap();
        t.tx(&carol, Action::FundHunt { hunt_id: id, amount: 4 * DROPS_PER_FANG }).unwrap();
        // Creator can't cancel before expiry; strangers never.
        assert!(t.tx(&alice, Action::CancelHunt { hunt_id: id }).unwrap_err().to_string().contains("after block"));
        assert!(t.tx(&bob, Action::CancelHunt { hunt_id: id }).is_err());
        t.blocks(5);
        assert!(t.tx(&bob, Action::FundHunt { hunt_id: id, amount: DROPS_PER_FANG }).unwrap_err().to_string().contains("expired"));
        let (a0, b0, c0) = (t.bal(&alice), t.bal(&bob), t.bal(&carol));
        t.tx(&alice, Action::CancelHunt { hunt_id: id }).unwrap();
        let fee_paid = a0 + 20 * DROPS_PER_FANG - t.bal(&alice);
        assert!(fee_paid < DROPS_PER_FANG / 1000, "alice got her 20 back (minus the cancel fee)");
        assert_eq!(t.bal(&bob), b0 + 3 * DROPS_PER_FANG);
        assert_eq!(t.bal(&carol), c0 + 4 * DROPS_PER_FANG);
        assert_eq!(t.c.state.hunts[&id].status, HuntStatus::Cancelled);
        assert!(t.tx(&alice, Action::AwardHunt { hunt_id: id, to: bob.address(), pr: 1, familiar_key: None }).is_err());
        // Admins can cancel open hunts any time.
        t.tx(&alice, create("jackdishman/blockula", 3, 10 * DROPS_PER_FANG, t.c.height() + 1000, "python sdk")).unwrap();
        let id2 = t.c.state.hunts.keys().next_back().copied().unwrap();
        let admin = t.admin.clone_keys();
        t.tx(&admin, Action::CancelHunt { hunt_id: id2 }).unwrap();
        fs::remove_dir_all(&t.home).ok();
    }

    /// Audit #11: a hunt takes at most `MAX_HUNT_FUNDERS` distinct funders, so its record (which
    /// stays in the state forever) can't outgrow what a snapshot may carry. Past the cap, the
    /// funders it has (the creator included) can still add to it; refunds still reach everyone.
    #[test]
    fn funders_are_capped() {
        let mut t = setup("cap");
        let alice = Keys::from_seed([1; 32]);
        t.tx(&alice, create("jackdishman/blockula", 7, 10 * DROPS_PER_FANG, t.c.height() + 1_000, "cap")).unwrap();
        let id = *t.c.state.hunts.keys().next().unwrap();
        // Fill it straight through the state machine (it doesn't check signatures; admission
        // does), one new funder per tx, then commit that as a block.
        let funders: Vec<Keys> = (0..MAX_HUNT_FUNDERS as u32).map(|i| Keys::from_seed(crypto::hash(&(i + 1_000).to_le_bytes()))).collect();
        let (height, proposer, genesis_hash) = (t.c.height() + 1, t.c.fee_sink(), t.c.genesis_hash.clone());
        let fund = |k: &Keys, nonce: u64, amount: u64| {
            let mut tx = Tx { chain_id: "hunts".into(), genesis_hash: genesis_hash.clone(), from: k.address(), nonce, fee: 0, tip: 0, valid_until: height, action: Action::FundHunt { hunt_id: id, amount }, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
            tx.fee = min_fee(tx.size());
            tx
        };
        for k in &funders {
            t.c.state.accounts.insert(k.address(), Account { balance: 10 * DROPS_PER_FANG, ..Default::default() });
            t.c.state.supply += 10 * DROPS_PER_FANG;
        }
        for k in &funders[..MAX_HUNT_FUNDERS - 1] {
            assert_eq!(t.c.state.apply(&fund(k, 0, DROPS_PER_FANG), "hunts", &proposer, height).unwrap(), Outcome::Applied);
        }
        assert_eq!(t.c.state.hunts[&id].funders.len(), MAX_HUNT_FUNDERS);
        let late = t.c.state.apply(&fund(&funders[MAX_HUNT_FUNDERS - 1], 0, DROPS_PER_FANG), "hunts", &proposer, height).unwrap();
        assert_eq!(late, Outcome::Failed(format!("hunt #{id} has {MAX_HUNT_FUNDERS} funders already; only they can add to it")));
        assert_eq!(t.c.state.apply(&fund(&funders[0], 1, DROPS_PER_FANG), "hunts", &proposer, height).unwrap(), Outcome::Applied, "an existing funder tops up");
        t.c.produce_block_with(vec![]).unwrap();
        t.tx(&alice, Action::FundHunt { hunt_id: id, amount: DROPS_PER_FANG }).unwrap();
        assert_eq!(t.c.state.hunts[&id].funders.len(), MAX_HUNT_FUNDERS);
        assert_eq!(t.c.state.hunts[&id].contribution(&funders[0].address()), 2 * DROPS_PER_FANG);
        // The admin refunds everyone exactly.
        let before: Vec<u64> = funders.iter().map(|k| t.c.state.account(&k.address()).balance).collect();
        let admin = t.admin.clone_keys();
        t.tx(&admin, Action::CancelHunt { hunt_id: id }).unwrap();
        for (k, b) in funders.iter().zip(before) {
            assert_eq!(t.c.state.account(&k.address()).balance, b + t.c.state.hunts[&id].contribution(&k.address()));
        }
        fs::remove_dir_all(&t.home).ok();
    }

    #[test]
    fn github_links_and_familiars() {
        let mut t = setup("gh");
        let (alice, bob) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]));
        assert!(t.tx(&alice, Action::LinkGithub { handle: "-bad".into(), gist_id: "abc".into() }).is_err());
        assert!(t.tx(&alice, Action::LinkGithub { handle: "vlad".into(), gist_id: "ABC".into() }).is_err(), "gist id is lowercase");
        t.tx(&alice, Action::LinkGithub { handle: "Vlad".into(), gist_id: "abc123".into() }).unwrap();
        assert!(t.tx(&alice, Action::LinkGithub { handle: "other".into(), gist_id: "abc".into() }).unwrap_err().to_string().contains("unlink it first"));
        assert!(t.tx(&bob, Action::LinkGithub { handle: "vlad".into(), gist_id: "def".into() }).unwrap_err().to_string().contains("already linked"));
        assert!(t.tx(&bob, Action::UnlinkGithub { handle: "vlad".into() }).is_err(), "only the holder or an admin");
        let admin = t.admin.clone_keys();
        t.tx(&admin, Action::UnlinkGithub { handle: "VLAD".into() }).unwrap();
        t.tx(&bob, Action::LinkGithub { handle: "vlad".into(), gist_id: "def".into() }).unwrap();
        assert_eq!(t.c.state.github["vlad"].account, bob.address());
        assert!(!t.c.state.github_by_account.contains_key(&alice.address()));
        // Familiars must be session keys on the account.
        let agent = Keys::from_seed([42; 32]);
        let kid = key_id(&agent.public_key());
        assert!(t.tx(&alice, Action::RegisterFamiliar { key_id: kid.clone(), label: "claude".into() }).unwrap_err().to_string().contains("isn't on this account"));
        let h = t.c.height();
        let add = crate::types::add_key(&agent, &t.c.genesis.chain_id, &t.c.genesis_hash, &alice.address(), "claude", KeyRole::Session { limit: DROPS_PER_FANG, period_blocks: 10, expires_at: h + 100 });
        t.tx(&alice, add).unwrap();
        t.tx(&alice, Action::RegisterFamiliar { key_id: kid.clone(), label: "claude".into() }).unwrap();
        // Awards can credit the Familiar; a non-Familiar key can't be named.
        t.tx(&bob, create("jackdishman/blockula", 5, 10 * DROPS_PER_FANG, t.c.height() + 100, "PQ handshake")).unwrap();
        let id = t.c.state.hunts.keys().next_back().copied().unwrap();
        assert!(t.tx(&bob, Action::AwardHunt { hunt_id: id, to: alice.address(), pr: 8, familiar_key: Some("ab".repeat(32)) }).unwrap_err().to_string().contains("Familiar"));
        t.tx(&bob, Action::AwardHunt { hunt_id: id, to: alice.address(), pr: 8, familiar_key: Some(kid.clone()) }).unwrap();
        assert_eq!(t.c.state.hunts[&id].familiar_key.as_deref(), Some(kid.as_str()));
        // Removing the key drops the Familiar.
        t.tx(&alice, Action::RemoveKey { key_id: kid.clone() }).unwrap();
        assert!(!t.c.state.familiars.contains_key(&alice.address()));
        fs::remove_dir_all(&t.home).ok();
    }
}

/// Protocol v1 rules at the state machine: tips and the charged fee, `valid_until`, address
/// prefixes, and the order proposals take mempool txs in.
#[cfg(test)]
mod v1 {
    use super::*;

    fn fresh(tag: &str, prefix: &str) -> (PathBuf, Chain) {
        let home = std::env::temp_dir().join(format!("blockula-v1-{tag}-{}-{}", now_ms(), std::process::id()));
        Chain::summon(&home, &SummonOpts { address_prefix: prefix.into(), ..SummonOpts::new("v1", 1) }).unwrap();
        let c = Chain::open(&home).unwrap();
        (home, c)
    }

    /// `keys` acting for `from`, paying exactly the charge (`min_fee + tip`).
    fn tx(c: &Chain, keys: &Keys, from: &str, nonce: u64, tip: u64, valid_until: u64, action: Action) -> Tx {
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: from.into(), nonce, fee: 0, tip, valid_until, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = tx.charge().unwrap();
        tx.sign(keys)
    }

    fn bal(c: &Chain, addr: &str) -> u64 {
        c.state.account(addr).balance
    }

    #[test]
    fn tip_goes_to_the_proposer_and_only_the_charge_is_debited() {
        let (home, mut c) = fresh("tip", "boo");
        let (a, friend) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]).address());
        c.faucet(&a.address(), 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let me = c.me().unwrap();
        let (a0, me0, supply0, burned0) = (bal(&c, &a.address()), bal(&c, &me), c.state.supply, c.state.burned);
        // `fee` allows more than the charge: the rest stays with the sender.
        let mut t = tx(&c, &a, &a.address(), 0, 7_000, c.height() + 10, Action::Transfer { to: friend.clone(), amount: DROPS_PER_FANG });
        t.fee += 1_000_000;
        let t = t.sign(&a);
        // An idle chain's base fee is the floor (`min_fee`); opening `friend`'s account burns the
        // creation fee on top.
        let (base, open) = (min_fee(t.size()), c.state.chain_params.account_creation_fee);
        assert_eq!(crate::econ::base_fee(&c.state, c.height() + 1, t.size()), base);
        c.submit(t.clone()).unwrap();
        c.produce_block().unwrap();
        assert_eq!(bal(&c, &a.address()), a0 - DROPS_PER_FANG - base - 7_000 - open);
        assert_eq!(bal(&c, &me), me0 + 7_000, "the tip goes to the proposer");
        assert_eq!((c.state.supply, c.state.burned), (supply0 - base - open, burned0 + base + open), "the base fee is burned");
        // A fee that doesn't cover the base fee + tip isn't admittable: not in the mempool, not in
        // a block. Statelessly it only has to cover the lowest base fee any parameters allow.
        let mut short = tx(&c, &a, &a.address(), 1, 5_000, c.height() + 10, Action::Transfer { to: friend.clone(), amount: DROPS_PER_FANG });
        short.fee -= 1;
        let short = short.sign(&a);
        assert!(c.submit(short.clone()).unwrap_err().to_string().contains("fee too low"));
        crate::consensus::check_tx_stateless(&short, "v1", &c.genesis_hash, "boo").unwrap();
        let mut shorter = short.clone();
        shorter.fee = crate::econ::lowest_base_fee(shorter.size()) + shorter.tip - 1;
        assert!(crate::consensus::check_tx_stateless(&shorter.sign(&a), "v1", &c.genesis_hash, "boo").unwrap_err().to_string().contains("fee too low"));
        c.produce_block_with(vec![short.clone()]).unwrap();
        assert!(c.failure(&short.id()).unwrap().0.contains("fee too low"));
        assert_eq!(c.state.account(&a.address()).nonce, 1);
        // A tip past its cap (`tip_cap_multiple` × the base fee; this one would overflow the
        // charge) is refused too.
        let mut huge = tx(&c, &a, &a.address(), 1, 0, c.height() + 10, Action::Transfer { to: friend, amount: 1 });
        (huge.tip, huge.fee) = (u64::MAX, u64::MAX);
        assert!(c.submit(huge.sign(&a)).unwrap_err().to_string().contains("tip too high"));
        let root = c.state.root();
        drop(c);
        assert_eq!(Chain::open(&home).unwrap().state.root(), root, "replays identically");
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn session_limits_count_the_charged_fee() {
        let (home, mut c) = fresh("sess", "boo");
        let (owner, agent, friend) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]), Keys::from_seed([3; 32]).address());
        let acct = owner.address();
        c.faucet(&acct, 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let limit = DROPS_PER_FANG;
        let role = KeyRole::Session { limit, period_blocks: 100, expires_at: c.height() + 1_000 };
        let add = crate::types::add_key(&agent, &c.genesis.chain_id, &c.genesis_hash, &acct, "claude", role);
        c.submit(tx(&c, &owner, &acct, 0, 0, c.height() + 10, add)).unwrap();
        c.produce_block().unwrap();
        // An amount that fits with the base fee (and the fee for opening `friend`'s account)
        // alone, but not with a tip on top.
        let charge0 = min_fee(tx(&c, &agent, &acct, 1, 0, c.height() + 10, Action::Transfer { to: friend.clone(), amount: 1 }).size());
        let open = c.state.chain_params.account_creation_fee;
        let amount = limit - charge0 - open - 10;
        let tipped = tx(&c, &agent, &acct, 1, 100, c.height() + 10, Action::Transfer { to: friend.clone(), amount });
        assert!(c.submit(tipped).unwrap_err().to_string().contains("limit reached"));
        c.submit(tx(&c, &agent, &acct, 1, 0, c.height() + 10, Action::Transfer { to: friend, amount })).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.keys_of(&acct)[&key_id(&agent.public_key())].spent, amount + charge0 + open, "spent = amount + charged fee + the account it opened");
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn expired_txs_never_execute_and_leave_the_mempool() {
        let (home, mut c) = fresh("exp", "boo");
        let (a, friend) = (Keys::from_seed([1; 32]), Keys::from_seed([2; 32]).address());
        c.faucet(&a.address(), 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let h = c.height();
        let send = |c: &Chain, valid_until: u64| tx(c, &a, &a.address(), 0, 0, valid_until, Action::Transfer { to: friend.clone(), amount: DROPS_PER_FANG });
        // Admission wants `next <= valid_until <= next + an hour of blocks`.
        let max = c.state.chain_params.max_tx_validity_blocks;
        assert_eq!(max, 3_600, "an hour of 1 s blocks");
        assert_eq!(c.submit(send(&c, h)).unwrap_err().to_string(), format!("expired at block {h}"));
        assert!(c.submit(send(&c, h + 1 + max + 1)).unwrap_err().to_string().contains("too far ahead"));
        // A signed tx with an unused nonce that nobody includes before `valid_until`…
        let t = send(&c, h + 3);
        c.submit(t.clone()).unwrap();
        c.produce_block_with(vec![]).unwrap();
        c.produce_block_with(vec![]).unwrap();
        assert!(c.in_mempool(&t.id()), "still valid for block h + 3");
        c.produce_block_with(vec![]).unwrap();
        // …is dropped as expired, and can't come back.
        assert!(!c.in_mempool(&t.id()));
        assert_eq!(c.failure(&t.id()).unwrap().0, format!("expired at block {}", h + 3));
        assert!(c.submit(t.clone()).unwrap_err().to_string().contains("expired"));
        // Even a proposer that includes it can't execute it: the nonce stays unused.
        c.produce_block_with(vec![t.clone()]).unwrap();
        assert!(!c.has_tx(&t.id()));
        assert_eq!(c.state.account(&a.address()).nonce, 0);
        assert!(c.failure(&t.id()).unwrap().0.contains("expired at block"));
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn testnet_prefix_is_enforced() {
        let (home, mut c) = fresh("tboo", "tboo");
        let me = c.me().unwrap();
        assert!(me.starts_with("tboo1") && c.genesis.validators[0].address == me);
        assert!(c.state.treasury().starts_with("tboo1") && c.state.pool().starts_with("tboo1"));
        assert!(c.state.accounts.contains_key(&c.state.treasury()), "the treasury allocation uses the prefix");
        let a = Keys::from_seed([1; 32]);
        let (main, test) = (a.address(), a.address_with("tboo"));
        let vu = c.height() + 10;
        // The faucet won't pay a mainnet address on a testnet.
        assert!(c.faucet(&main, DROPS_PER_FANG).unwrap_err().to_string().contains("`to` is a boo (mainnet) address"));
        c.faucet(&test, 10 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        // Acting as the mainnet form of the same key, or paying a mainnet address, is refused.
        let err = c.submit(tx(&c, &a, &main, 0, 0, vu, Action::Transfer { to: me.clone(), amount: 1 })).unwrap_err().to_string();
        assert!(err.contains("`from` is a boo (mainnet) address, but this chain's addresses start with tboo1"), "{err}");
        let pay_main = tx(&c, &a, &test, 0, 0, vu, Action::Transfer { to: Keys::from_seed([2; 32]).address(), amount: DROPS_PER_FANG });
        assert!(c.submit(pay_main.clone()).unwrap_err().to_string().contains("`to` is a boo (mainnet) address"));
        assert!(crate::consensus::check_tx_stateless(&pay_main, "v1", &c.genesis_hash, "tboo").is_err(), "verify, gossip and HTTP refuse it too");
        c.produce_block_with(vec![pay_main.clone()]).unwrap();
        assert!(!c.has_tx(&pay_main.id()), "and it can't execute");
        // Native addresses and names work.
        c.submit(tx(&c, &a, &test, 0, 0, vu, Action::RegisterName { name: "tessa".into() })).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.resolve("tessa.boo").unwrap(), test);
        assert_eq!(bal(&c, &c.state.treasury()), 100_000_000 * DROPS_PER_FANG + c.state.chain_params.name_price(5));
        // A genesis whose addresses don't match its prefix (or an unknown prefix) doesn't load.
        let path = home.join("other.json");
        for (prefix, why) in [("boo", "doesn't use this chain's prefix"), ("xyz", "must be one of")] {
            let g = Genesis { address_prefix: prefix.into(), ..c.genesis.clone() };
            fs::write(&path, serde_json::to_string(&g).unwrap()).unwrap();
            assert!(format!("{:#}", Genesis::load(&path).unwrap_err()).contains(why));
        }
        let root = c.state.root();
        drop(c);
        assert_eq!(Chain::open(&home).unwrap().state.root(), root);
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn proposals_take_the_highest_tip_per_byte_first() {
        let (home, mut c) = fresh("order", "boo");
        let ks: Vec<Keys> = (1..=4u8).map(|i| Keys::from_seed([i; 32])).collect();
        for k in &ks {
            c.state.accounts.insert(k.address(), Account { balance: 100 * DROPS_PER_FANG, ..Default::default() });
        }
        let to = Keys::from_seed([9; 32]).address();
        let vu = c.height() + 10;
        let send = |k: &Keys, nonce: u64, tip: u64, memo: &str| {
            let mut t = Tx { memo: memo.into(), ..tx(&c, k, &k.address(), nonce, tip, vu, Action::Transfer { to: to.clone(), amount: DROPS_PER_FANG }) };
            t.fee = t.charge().unwrap();
            t.sign(k)
        };
        let a0 = send(&ks[0], 0, 0, "");
        let a1 = send(&ks[0], 1, 1_000_000, ""); // a big tip, but it can't jump its own nonce 0
        let b = send(&ks[1], 0, 500, "");
        let d = send(&ks[2], 0, 500, ""); // same rate as b, arrived later
        let e = send(&ks[3], 0, 520, &"m".repeat(256)); // a bigger tip, but less per byte
        assert!(TipRate::of(&e) < TipRate::of(&b));
        for t in [&a0, &a1, &b, &d, &e] {
            c.submit(t.clone()).unwrap();
        }
        let order: Vec<String> = c.proposal_txs().iter().map(|t| t.id()).collect();
        assert_eq!(order, [&b, &d, &e, &a0, &a1].map(|t| t.id()));
        fs::remove_dir_all(home).ok();
    }
}

/// The protocol v1 scaffold: genesis fields and parameters, the levers governance will call,
/// dispatch of the new tags to their modules, and the end-of-block steps.
#[cfg(test)]
mod scaffold {
    use super::*;
    use crate::poc::SOUL;
    use crate::state::ns;

    const FANG: u64 = DROPS_PER_FANG;
    const VALIDATOR: [u8; 32] = [21; 32];
    const P2P: &str = "127.0.0.1:7001";
    const HTTP: &str = "127.0.0.1:7000";

    fn keys(seed: u8) -> Keys {
        Keys::from_seed([seed; 32])
    }

    /// A protocol v1 genesis: one validator owned by an operator, a vesting founder, the
    /// community bucket, reserved names, a Soul ledger, parameter overrides and Ghost Mode on
    /// (except on mainnet, which starts with it off).
    fn genesis(chain_id: &str) -> Genesis {
        let (v, owner, founder) = (Keys::from_seed(VALIDATOR), keys(22), keys(23));
        let alloc = |address: String, amount: u64, vesting: Option<Vesting>, label: Option<&str>| Allocation { address, amount, vesting, label: label.map(String::from) };
        Genesis {
            chain_id: chain_id.into(),
            token: "FANG".into(),
            decimals: DECIMALS,
            block_time_ms: 1000,
            validators: vec![ValidatorInfo { address: v.address(), public_key: hex::encode(v.public_key()), p2p_addr: P2P.into(), http_addr: HTTP.into() }],
            faucet: false,
            created_at_ms: 1_750_000_000_000,
            allocations: vec![
                alloc(owner.address(), 20_000 * FANG, None, Some("genesis operator")),
                alloc(founder.address(), 1_000_000 * FANG, Some(Vesting { start: 0, cliff: 100, end: 1_000 }), Some("founder")),
                alloc(community("boo"), 5_000_000 * FANG, None, Some("community")),
                alloc(treasury("boo"), 2_000_000 * FANG, Some(Vesting { start: 0, cliff: 0, end: 4_000 }), Some("treasury")),
            ],
            epoch_blocks: 600,
            validator_admins: vec![],
            contribution: None,
            address_prefix: "boo".into(),
            param_overrides: BTreeMap::from([("max_validators".into(), 40), ("proposal_deposit".into(), 50 * FANG)]),
            operators: vec![Operator {
                owner: owner.address(),
                consensus_key: hex::encode(v.public_key()),
                pop: make_pop(VALIDATOR, chain_id, &owner.address(), P2P, HTTP),
                p2p_addr: P2P.into(),
                http_addr: HTTP.into(),
                self_bond: 10_000 * FANG,
                commission_bps: 500,
                provider: Some("OVHcloud".into()),
                asn: Some(16_276),
                country: Some("FR".into()),
                entity: Some("Owner SAS".into()),
            }],
            reserved_names: vec!["blockula".into(), "foundation".into()],
            soul_ledger: vec![SoulEntry { account: owner.address(), soul: 50 * SOUL, reason: "testnet contributor".into() }],
            features: if chain_id == MAINNET_CHAIN_ID { vec![] } else { vec!["ghost".into()] },
            upgrades: vec![],
            bootstrap_excluded: vec![],
            pool_repos: Default::default(),
        }
    }

    /// A home on `g`, opened as its validator.
    fn open(tag: &str, g: &Genesis) -> (PathBuf, Chain) {
        let home = std::env::temp_dir().join(format!("blockula-scaffold-{tag}-{}-{}", now_ms(), std::process::id()));
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("genesis.json"), serde_json::to_string_pretty(g).unwrap()).unwrap();
        write_secret(&home.join("validator.key"), &hex::encode(VALIDATOR)).unwrap();
        let c = Chain::open(&home).unwrap();
        (home, c)
    }

    fn signed(c: &Chain, k: &Keys, action: Action) -> Tx {
        let from = k.address_with(c.state.params.prefix());
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce: c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        tx.sign(k)
    }

    #[test]
    fn genesis_fields_become_state_records() {
        let g = genesis("scaffold-1");
        g.check().unwrap();
        let (home, c) = open("records", &g);
        let s = &c.state;
        let (v, owner, founder) = (Keys::from_seed(VALIDATOR).address(), keys(22).address(), keys(23).address());
        // Parameters: the overrides, and defaults for the rest.
        assert_eq!((s.chain_params.max_validators, s.chain_params.proposal_deposit), (40, 50 * FANG));
        assert_eq!(s.chain_params.unbonding_blocks, 21 * 86_400);
        assert!(s.record(ns::META, crate::state::meta::CHAIN_PARAMS).is_some());
        // The operator owns its validator; the self-bond left its balance for the validator's
        // stake pool (an escrow), as the owner's shares.
        let rec = &s.validators[&v];
        assert_eq!((rec.owner.as_str(), rec.commission_bps, rec.status), (owner.as_str(), 500, ValidatorStatus::Active));
        assert_eq!((s.staking.pools[&v].stake, crate::staking::self_bond(s, &v)), (10_000 * FANG, 10_000 * FANG));
        assert_eq!(crate::staking::holdings(s, &owner), (10_000 * FANG, 0));
        assert_eq!(s.account(&owner).balance, 10_000 * FANG);
        // Vesting allocations are locks on the account, released linearly after the cliff.
        let locks = s.account(&founder).locks;
        assert_eq!(locks, vec![Lock { id: crate::econ::VESTING_LOCK.into(), amount: LockAmount::Vesting { total: 1_000_000 * FANG, start: 0, cliff: 100, end: 1_000 }, reason: "founder".into() }]);
        let f = s.account(&founder);
        assert_eq!((f.locked_at(0), f.locked_at(99), f.locked_at(100), f.locked_at(550), f.locked_at(1_000)), (1_000_000 * FANG, 1_000_000 * FANG, 900_000 * FANG, 450_000 * FANG, 0));
        assert_eq!(s.account(&s.treasury()).locked_at(1_000), 1_500_000 * FANG);
        assert_eq!(system_account("boo", &community("boo")), Some("community"));
        assert_eq!(system_account("boo", &owner), None);
        // Reserved names, the Soul ledger and features are records too.
        assert!(s.econ.reserved_names.contains_key("blockula") && s.record(ns::RESERVED_NAMES, b"foundation").is_some());
        assert_eq!(s.soul_of(&owner, 0), 50 * SOUL);
        assert!(crate::upgrade::is_active(s, "ghost") && s.record(ns::FEATURES, b"ghost").is_some());
        s.check_invariants().unwrap();
        assert_eq!(s.supply, 8_020_000 * FANG);
        // The same genesis through its JSON gives the same state.
        assert_eq!(Genesis::load(&home.join("genesis.json")).unwrap().initial_state().root(), c.genesis_root);
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn genesis_is_validated() {
        let dir = std::env::temp_dir().join(format!("blockula-scaffold-check-{}-{}", now_ms(), std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("genesis.json");
        let load = |v: &serde_json::Value| {
            fs::write(&path, serde_json::to_vec(v).unwrap()).unwrap();
            Genesis::load(&path).map(|_| ()).map_err(|e| format!("{e:#}"))
        };
        let good = serde_json::to_value(genesis("scaffold-1")).unwrap();
        load(&good).unwrap();
        let refused = |edit: &dyn Fn(&mut serde_json::Value), why: &str| {
            let mut v = good.clone();
            edit(&mut v);
            let err = load(&v).expect_err(why);
            assert!(err.contains(why), "expected “{why}”, got: {err}");
        };
        // Parameters: values as strings or numbers, within bounds, known keys only.
        let mut v = good.clone();
        v["params"]["quorum_floor"] = serde_json::json!("30000000000000000");
        v["params"]["downtime_window"] = serde_json::json!(200);
        load(&v).unwrap();
        refused(&|v| v["params"]["max_validators"] = 0.into(), "`max_validators` must be 1–64");
        refused(&|v| v["params"]["issuance_floor_ppm"] = 4_999.into(), "issuance_floor_ppm");
        refused(&|v| v["params"]["issuance_stakers_bps"] = 4_000.into(), "issuance_stakers_bps");
        refused(&|v| v["params"]["max_block_bytes"] = (8u64 << 20).into(), "max_block_bytes");
        refused(&|v| v["params"]["warp_speed"] = 1.into(), "unknown parameter `warp_speed`");
        refused(&|v| v["min_bond"] = "1".into(), "moved to `params.min_self_bond`");
        // Allocations and vesting.
        refused(&|v| v["allocations"][1]["vesting"]["cliff"] = 2_000.into(), "start ≤ cliff ≤ end");
        refused(&|v| v["allocations"][1]["vesting"]["extra"] = 1.into(), "unknown field");
        refused(&|v| v["allocations"][0]["vestng"] = serde_json::json!({}), "unknown field");
        refused(
            &|v| {
                let mut twice = v["allocations"][1].clone();
                twice["amount"] = "1".into();
                v["allocations"].as_array_mut().unwrap().push(twice);
            },
            "more than one vesting allocation",
        );
        // Operators: their validator, a proof of possession, a self-bond they can pay.
        refused(&|v| v["operators"][0]["pop"] = "00".repeat(crypto::SIGNATURE_LEN).into(), "pop");
        refused(&|v| v["operators"][0]["self_bond"] = "1".into(), "below min_self_bond");
        refused(&|v| v["operators"][0]["self_bond"] = (30_000 * FANG).to_string().into(), "allocations hold");
        refused(&|v| v["operators"][0]["commission_bps"] = 10_001.into(), "commission_max_bps");
        refused(&|v| v["operators"][0]["consensus_key"] = hex::encode(keys(99).public_key()).into(), "isn't one of the genesis validators");
        refused(&|v| v["operators"][0]["country"] = "France".into(), "ISO 3166-1");
        refused(&|v| v["operators"][0]["owner"] = treasury("boo").into(), "can't own a validator");
        // Validators: each once (audit F33: twice, epoch 0's set had no usable keys and the
        // operator's self-bond was bonded twice), keyed by their own address.
        let twice = |v: &mut serde_json::Value| {
            let again = v["validators"][0].clone();
            v["validators"].as_array_mut().unwrap().push(again);
        };
        refused(&twice, "is listed twice");
        let mut v = good.clone();
        twice(&mut v);
        let g: Genesis = serde_json::from_value(v).unwrap();
        assert_eq!(g.initial_state().active_sets[&0].len(), 2, "what the check keeps out");
        assert!(g.initial_state().check_invariants().is_err());
        refused(&|v| v["validators"][0]["address"] = keys(98).address().into(), "isn't its public key's address");
        refused(&|v| v["validators"][0]["public_key"] = "AB".repeat(crypto::PUBLIC_KEY_LEN).into(), "lowercase hex");
        refused(&|v| v["validators"][0]["p2p_addr"] = "nowhere".into(), "p2p_addr");
        // Reserved names, the Soul ledger and features.
        refused(&|v| v["reserved_names"][0] = "blockula.boo".into(), "bare lowercase name");
        refused(&|v| v["reserved_names"][1] = "blockula".into(), "listed twice");
        refused(&|v| v["soul_ledger"][0]["soul"] = 0.into(), "milli-Soul");
        refused(&|v| v["soul_ledger"][0]["account"] = community("boo").into(), "system account");
        refused(&|v| v["features"][0] = "warp".into(), "unknown feature warp");
        // The bootstrap delegation's excluded owners: addresses, not keyless, sorted, once each.
        let mut ab = [keys(30).address(), keys(31).address()];
        ab.sort();
        let [a, b] = ab;
        refused(&|v| v["bootstrap_excluded"] = serde_json::json!([b, a]), "sorted");
        refused(&|v| v["bootstrap_excluded"] = serde_json::json!([a, a]), "sorted");
        refused(&|v| v["bootstrap_excluded"] = serde_json::json!([treasury("boo")]), "keyless system account");
        refused(&|v| v["bootstrap_excluded"] = serde_json::json!(["nobody"]), "isn't a boo1");
        // Mainnet: no admin keys, no faucet, an operator for every validator.
        let mut main = serde_json::to_value(genesis(MAINNET_CHAIN_ID)).unwrap();
        load(&main).unwrap();
        main["validator_admins"] = serde_json::json!([keys(22).address()]);
        assert!(load(&main).unwrap_err().contains("no admin keys"));
        let mut main = serde_json::to_value(genesis(MAINNET_CHAIN_ID)).unwrap();
        main["faucet"] = true.into();
        assert!(load(&main).unwrap_err().contains("no faucet"));
        let mut main = serde_json::to_value(genesis(MAINNET_CHAIN_ID)).unwrap();
        main["operators"] = serde_json::json!([]);
        assert!(load(&main).unwrap_err().contains("needs an operator"));
        // … and Ghost Mode off (DEC-2: governance switches it on after its audit).
        let mut main = serde_json::to_value(genesis(MAINNET_CHAIN_ID)).unwrap();
        main["features"] = serde_json::json!(["ghost"]);
        assert!(load(&main).unwrap_err().contains("Ghost Mode off"));
        // … and governance's safety parameters inside their bands (F15, review-fix-gov-3).
        let day = 86_400u64;
        for (key, value, side) in [("timelock_blocks", 3_600, "floor"), ("supermajority_bps", 5_001, "floor"), ("treasury_large_bps", 1_000, "ceiling"), ("quorum_floor", 0, "floor"), ("supermajority_bps", 9_000, "ceiling"), ("quorum_bps", 2_000, "ceiling"), ("governance_spend_height", 366 * day, "ceiling")] {
            let mut main = serde_json::to_value(genesis(MAINNET_CHAIN_ID)).unwrap();
            main["params"][key] = value.into();
            let e = load(&main).unwrap_err();
            assert!(e.contains(&format!("`{key}`")) && e.contains(&format!("past governance's {side}")), "{key}: {e}");
        }
        // Other chains may keep admins (dev and test).
        let mut test = good.clone();
        test["validator_admins"] = serde_json::json!([keys(22).address()]);
        load(&test).unwrap();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn new_tags_dispatch_to_their_modules() {
        let (home, mut c) = open("dispatch", &genesis("scaffold-1"));
        let (owner, bob) = (keys(22), keys(24));
        // Every tag reaches its module. One the module refuses (a reveal for epoch 1, whose beacon
        // follows from genesis) is refused at submission, and a block carrying it charges its fee
        // anyway.
        let refused = signed(&c, &owner, Action::BeaconReveal { epoch: 1, secret: "ab".repeat(32) });
        assert!(c.submit(refused.clone()).unwrap_err().to_string().contains("epoch 1 has no round"));
        let before = c.state.account(&owner.address()).balance;
        c.produce_block_with(vec![refused.clone()]).unwrap();
        assert!(c.failure(&refused.id()).unwrap().0.contains("epoch 1 has no round"));
        assert_eq!(c.state.account(&owner.address()).balance, before - refused.charge().unwrap());
        // Reserved names can't be registered; names transfer and release.
        assert!(c.submit(signed(&c, &owner, Action::RegisterName { name: "blockula".into() })).unwrap_err().to_string().contains("reserved"));
        c.submit(signed(&c, &owner, Action::RegisterName { name: "owner".into() })).unwrap();
        c.produce_block().unwrap();
        let give = signed(&c, &owner, Action::TransferName { name: "owner.boo".into(), to: bob.address() });
        assert_eq!(c.state.describe(&give, None), format!("owner.boo offered owner.boo to {}", short(&bob.address())));
        c.submit(give).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.resolve("owner.boo").unwrap(), owner.address(), "an offer: the owner's until bob accepts");
        // Bob holds nothing yet: his acceptance can't even pay its fee, nor can a release of a name he doesn't own.
        assert!(c.submit(signed(&c, &bob, Action::TransferName { name: "owner".into(), to: bob.address() })).unwrap_err().to_string().contains("insufficient balance"));
        assert!(c.submit(signed(&c, &bob, Action::ReleaseName { name: "owner".into() })).unwrap_err().to_string().contains("insufficient balance"));
        // Without admins, the admin levers are closed.
        for action in [Action::SetRepo { repo: "a/b".into(), weight: 1 }, Action::GrantSoul { to: bob.address(), amount: SOUL, reason: "r".into() }] {
            assert!(c.submit(signed(&c, &owner, action)).unwrap_err().to_string().contains("only validator admins"));
        }
        let root = c.state.root();
        drop(c);
        assert_eq!(Chain::open(&home).unwrap().state.root(), root, "replays");
        fs::remove_dir_all(home).ok();
    }

    /// Every action has a plain-English summary, starting with who acted.
    #[test]
    fn every_action_is_described() {
        let st = genesis("scaffold-1").initial_state();
        let owner = keys(22);
        for action in crate::types::tests::vector_actions().into_iter().chain(crate::types::tests::v1_vector_actions()) {
            let tx = Tx { chain_id: "c".into(), genesis_hash: String::new(), from: owner.address(), nonce: 0, fee: 0, tip: 0, valid_until: 0, action, memo: String::new(), public_key: hex::encode(owner.public_key()), signature: String::new() };
            let summary = st.describe(&tx, Some(1));
            assert!(summary.starts_with(&short(&owner.address())) && summary.len() > 30, "{:?}: “{summary}”", tx.action);
        }
        let vote = |conviction| Tx { chain_id: "c".into(), genesis_hash: String::new(), from: owner.address(), nonce: 0, fee: 0, tip: 0, valid_until: 0, action: Action::Vote { proposal_id: 3, aye: false, amount: 2 * FANG, conviction, proposal_digest: "00".repeat(32) }, memo: String::new(), public_key: String::new(), signature: String::new() };
        assert!(st.describe(&vote(0), None).ends_with("voted nay on proposal #3 with 2 FANG at 0.1× conviction"));
        let spend = Tx { action: Action::Propose { kind: ProposalKind::TreasurySpend { to: keys(22).address(), amount: FANG }, deposit: 50 * FANG, title: "Grants".into(), url: String::new() }, ..vote(1) };
        assert!(st.describe(&spend, None).ends_with(&format!("proposed “Grants”: pay 1 FANG from the treasury to {}, bonding 50 FANG", short(&owner.address()))));
    }

    #[test]
    fn levers_run_without_signers_and_roll_back() {
        let (home, mut c) = open("levers", &genesis("scaffold-1"));
        let (owner, bob) = (keys(22), keys(24));
        // A hunt for the refund lever (anyone can create one).
        c.submit(signed(&c, &owner, Action::CreateHunt { repo: "a/b".into(), issue: 1, amount: 10 * FANG, expires_at: 1_000, title: "t".into() })).unwrap();
        c.produce_block().unwrap();
        let s = &mut c.state;
        let h = 5;
        let (root, supply, burned, params) = (s.full_root(), s.supply, s.burned, s.chain_params.clone());
        let mut j = Journal::default();
        // Parameters, within bounds only.
        s.exec_set_param(&mut j, "max_validators", 64).unwrap();
        assert!(s.exec_set_param(&mut j, "issuance_floor_ppm", 1).is_err());
        assert!(s.exec_set_param(&mut j, "nope", 1).is_err());
        assert_eq!(s.chain_params.max_validators, 64);
        // Supply.
        s.j_mint(&mut j, 5);
        s.j_credit(&mut j, &bob.address(), 5);
        s.j_debit(&mut j, &bob.address(), 2).unwrap();
        s.j_burn(&mut j, 2);
        assert_eq!((s.supply, s.burned), (supply + 3, burned + 2));
        // The PoC levers, no admin needed.
        s.exec_set_repo(&mut j, "a/b", 10, h).unwrap();
        s.exec_grant_soul(&mut j, &bob.address(), 5 * SOUL, h).unwrap();
        assert!(s.exec_grant_soul(&mut j, &bob.address(), crate::poc::MAX_SOUL_GRANT + 1, h).is_err());
        s.exec_flag_sybil(&mut j, &bob.address(), "proposal #1", "same person", h).unwrap();
        assert_eq!(s.soul_of(&bob.address(), h), 0);
        assert!(s.exec_clawback(&mut j, &bob.address(), h).unwrap_err().to_string().contains("no unvested rewards"));
        assert!(s.exec_reject_claim(&mut j, 0, "r", h).unwrap_err().to_string().contains("no claim"));
        assert!(s.exec_unlink_github(&mut j, "nobody").unwrap_err().to_string().contains("isn't linked"));
        let funded = s.account(&owner.address()).balance;
        s.exec_refund_hunt(&mut j, 0, h).unwrap();
        assert_eq!(s.account(&owner.address()).balance, funded + 10 * FANG);
        assert!(s.exec_refund_hunt(&mut j, 0, h).unwrap_err().to_string().contains("already Cancelled"));
        // Upgrades and features.
        crate::upgrade::exec_deactivate_feature(s, &mut j, "ghost").unwrap();
        assert!(crate::upgrade::exec_deactivate_feature(s, &mut j, "ghost").is_err());
        crate::upgrade::exec_activate_feature(s, &mut j, "ghost", h).unwrap();
        assert!(crate::upgrade::exec_activate_feature(s, &mut j, "warp", h).is_err());
        assert!(crate::upgrade::exec_schedule_upgrade(s, &mut j, 9, h, h, h).is_err(), "only after now");
        crate::upgrade::exec_schedule_upgrade(s, &mut j, 9, 100, h, h).unwrap();
        assert!(crate::upgrade::exec_schedule_upgrade(s, &mut j, 10, 80, h, h).unwrap_err().to_string().contains("must come after it"), "only after the last one");
        assert_eq!(crate::upgrade::version_at(s, 100), 9);
        s.check_invariants().unwrap();
        // Cancelling (F50): only what's scheduled, only `MIN_UPGRADE_LEAD` blocks ahead of it,
        // and the version number is spent (review-fix-ops-1): the fix ships as the next one.
        let lead = crate::upgrade::MIN_UPGRADE_LEAD;
        assert!(crate::upgrade::exec_cancel_upgrade(s, &mut j, 8, 100, h, 0, 0).unwrap_err().to_string().contains("no upgrade to v8"));
        assert!(crate::upgrade::exec_cancel_upgrade(s, &mut j, 9, 101, h, 0, 0).unwrap_err().to_string().contains("scheduled at block 100, not 101"));
        assert!(crate::upgrade::exec_cancel_upgrade(s, &mut j, 9, 100, 100 - lead + 1, 0, 0).unwrap_err().to_string().contains("too late"));
        assert_eq!(crate::upgrade::version_at(s, 100), 9, "a refused cancellation changes nothing");
        let withdrawn = crate::upgrade::exec_cancel_upgrade(s, &mut j, 9, 100, 100 - lead, 0, 0).unwrap();
        assert_eq!(withdrawn, [crate::upgrade::ScheduledUpgrade { version: 9, activation_height: 100 }]);
        assert_eq!((crate::upgrade::version_at(s, 100), s.upgrade.schedule.len()), (crate::consensus::PROTOCOL_VERSION, 0));
        s.check_invariants().unwrap();
        assert!(crate::upgrade::exec_schedule_upgrade(s, &mut j, 9, 100, h, h).unwrap_err().to_string().contains("never reused"));
        crate::upgrade::exec_schedule_upgrade(s, &mut j, 10, 100, h, h).unwrap();
        assert_eq!(crate::upgrade::version_at(s, 100), 10);
        s.check_invariants().unwrap();
        // Everything rolls back, dirty marks included.
        s.rollback(j);
        assert_eq!((s.full_root(), s.supply, s.burned, &s.chain_params), (root, supply, burned, &params));
        assert_eq!(s.commit_block().unwrap().records, vec![], "nothing left to commit");
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn skipped_txs_are_recorded_and_replayed() {
        let (home, mut c) = open("skipped", &genesis("scaffold-1"));
        let owner = keys(22);
        let ok = signed(&c, &owner, Action::Transfer { to: keys(24).address(), amount: FANG });
        let stale = Tx { nonce: 7, ..signed(&c, &owner, Action::Transfer { to: keys(24).address(), amount: FANG }) }.sign(&owner);
        c.produce_block_with(vec![ok, stale.clone()]).unwrap();
        let b = c.last_block().unwrap();
        assert_eq!((b.txs.len(), b.skipped.clone()), (1, vec![stale.size()]));
        let root = c.state.root();
        drop(c);
        // A restart reads the block back from the log; a rebuild replays it with its skipped txs.
        let c = Chain::open(&home).unwrap();
        assert_eq!((c.state.root(), c.block(1).unwrap().skipped, c.replayed), (root.clone(), vec![stale.size()], 0));
        drop(c);
        fs::remove_file(home.join(store::DB_FILE)).unwrap();
        let c = Chain::open(&home).unwrap();
        assert_eq!((c.state.root(), c.block(1).unwrap().skipped, c.replayed), (root, vec![stale.size()], 1));
        drop(c);
        fs::remove_dir_all(home).ok();
    }

    /// P7 (docs/PROGRAMS-PLAN.md): a version-gated rule, here the test-only `upgrade::test_gate`
    /// (admission refuses its memo from v2 on), flips at the activation height on every path a
    /// node runs on its own (mempool admission and revalidation, execution, the end-of-block
    /// steps, and replay from the block logs) and nowhere else. `consensus.rs` and the simulator
    /// cover propose and verify.
    #[test]
    fn a_version_gated_rule_flips_at_its_height_on_every_path() {
        const AT: u64 = 4;
        let mut g = genesis("scaffold-1");
        g.upgrades = vec![crate::upgrade::ScheduledUpgrade { version: crate::upgrade::PREVIEW_PROTOCOL_VERSION, activation_height: AT }];
        g.check().unwrap();
        let (home, mut c) = open("version-gate", &g);
        // This binary runs v2 on a chain whose genesis schedules it.
        assert_eq!(c.halt.supported, crate::upgrade::PREVIEW_PROTOCOL_VERSION);
        let (owner, bob) = (keys(22), keys(24));
        let gated = |c: &Chain, nonce_ahead: u64| {
            let mut tx = signed(c, &owner, Action::Transfer { to: bob.address(), amount: FANG });
            tx.nonce += nonce_ahead;
            tx.memo = crate::upgrade::test_gate::MEMO.into();
            tx.fee = min_fee(tx.size());
            tx.sign(&owner)
        };
        crate::upgrade::test_gate::record();
        // Below v2 the rule doesn't exist: admitted (at the next height, 1), included, applied.
        let early = gated(&c, 0);
        c.submit(early.clone()).unwrap();
        c.produce_block().unwrap();
        assert!(c.failure(&early.id()).is_none() && c.block(1).unwrap().txs.iter().any(|t| t.id() == early.id()));
        c.produce_block_with(vec![]).unwrap();
        // Admitted for block 3, but block 3 leaves it out: from block 4 (v2) on it no longer
        // applies, so revalidation drops it from the mempool.
        let late = gated(&c, 0);
        c.submit(late.clone()).unwrap();
        c.produce_block_with(vec![]).unwrap();
        assert_eq!(c.height(), AT - 1);
        assert!(!c.mempool.iter().any(|t| t.id() == late.id()), "dropped once the next height runs v2");
        assert!(c.failure(&late.id()).unwrap().0.contains("test gate"));
        // Mempool admission for block 4 refuses it outright; an ungated tx still goes in.
        assert!(c.submit(gated(&c, 0)).unwrap_err().to_string().contains("test gate"));
        let plain = signed(&c, &owner, Action::Transfer { to: bob.address(), amount: FANG });
        c.submit(plain.clone()).unwrap();
        // Execution: a block at v2 that carries one anyway skips it (unadmittable).
        let forced = gated(&c, 1);
        c.produce_block_with(vec![plain.clone(), forced.clone()]).unwrap();
        let b = c.block(AT).unwrap();
        assert!(b.txs.iter().any(|t| t.id() == plain.id()) && !b.txs.iter().any(|t| t.id() == forced.id()));
        assert_eq!(b.skipped, vec![forced.size()]);
        assert!(c.failure(&forced.id()).unwrap().0.contains("test gate"));
        c.produce_block_with(vec![]).unwrap();
        // Each block carries the version in force at its height, and its end-of-block steps saw it.
        let versions: Vec<(u64, u32)> = (1..=AT + 1).map(|h| (h, c.block(h).unwrap().version)).collect();
        assert_eq!(versions, vec![(1, 1), (2, 1), (3, 1), (4, 2), (5, 2)]);
        assert_eq!(crate::upgrade::test_gate::take(), versions);
        let root = c.state.root();
        drop(c);
        // Replay from the block logs: block 1's gated tx applies again (v1) and block 4 skips the
        // other (v2), to the same root, the end-of-block steps seeing the same versions.
        fs::remove_file(home.join(store::DB_FILE)).unwrap();
        crate::upgrade::test_gate::record();
        let c = Chain::open(&home).unwrap();
        assert_eq!((c.height(), c.state.root()), (AT + 1, root));
        assert_eq!(crate::upgrade::test_gate::take(), versions);
        drop(c);
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn a_binary_stops_before_a_version_it_cant_run() {
        let (home, mut c) = open("halt", &genesis("scaffold-1"));
        c.produce_block_with(vec![]).unwrap();
        let unsupported = crate::consensus::SUPPORTED_PROTOCOL_VERSION + 1;
        c.state.upgrade.schedule = vec![crate::upgrade::ScheduledUpgrade { version: unsupported, activation_height: 3 }];
        // Block 2 executes (and is stored); the check for block 3 stops the node.
        let err = c.produce_block_with(vec![]).unwrap_err().to_string();
        assert!(err.contains("block 3 runs protocol") && err.contains("upgrade blockula"), "{err}");
        assert_eq!(c.height(), 2);
        assert_eq!(c.halt.signal.get().map(|h| h.exit_code()), Some(crate::upgrade::EXIT_UPGRADE_REQUIRED));
        // Nothing executes past the halt.
        assert!(c.produce_block_with(vec![]).unwrap_err().to_string().contains("block 3 runs protocol"));
        drop(c);
        // A restart replays nothing and opens halted (the halt survives restarts): the API can
        // serve reads, but the node executes nothing more.
        let mut c = Chain::open(&home).unwrap();
        let err = c.halt.signal.get().expect("opens halted").to_string();
        assert!(err.contains("block 3 runs protocol") && err.contains("upgrade blockula"), "{err}");
        assert!(c.produce_block_with(vec![]).is_err());
        assert_eq!(c.height(), 2);
        // A binary that supports the version continues.
        crate::upgrade::configure(&mut c, crate::consensus::SUPPORTED_PROTOCOL_VERSION + 1, None);
        assert!(c.halt.signal.get().is_none());
        c.produce_block_with(vec![]).unwrap();
        assert_eq!((c.height(), c.block(3).unwrap().version), (3, crate::consensus::SUPPORTED_PROTOCOL_VERSION + 1));
        fs::remove_dir_all(home).ok();
    }

    /// `--halt-height H` executes `H` and stops there, through restarts, until it's raised.
    #[test]
    fn halt_height_stops_after_executing_it() {
        let (home, mut c) = open("halt-height", &genesis("scaffold-1"));
        crate::upgrade::configure(&mut c, crate::consensus::SUPPORTED_PROTOCOL_VERSION, Some(2));
        c.produce_block_with(vec![]).unwrap();
        let err = c.produce_block_with(vec![]).unwrap_err().to_string();
        assert!(err.contains("halted at block 2"), "{err}");
        assert_eq!((c.height(), c.halt.signal.get().map(|h| h.exit_code())), (2, Some(crate::upgrade::EXIT_HALT_HEIGHT)));
        assert!(c.produce_block_with(vec![]).is_err(), "block 3 never executes");
        drop(c);
        let mut c = Chain::open(&home).unwrap();
        crate::upgrade::configure(&mut c, crate::consensus::SUPPORTED_PROTOCOL_VERSION, Some(2));
        assert!(c.halt.signal.get().is_some(), "already at the halt height: halted at once");
        crate::upgrade::configure(&mut c, crate::consensus::SUPPORTED_PROTOCOL_VERSION, Some(4));
        c.produce_block_with(vec![]).unwrap();
        assert!(c.produce_block_with(vec![]).is_err());
        assert_eq!(c.height(), 4);
        fs::remove_dir_all(home).ok();
    }

    /// The consensus path (`execute_shared`, which writes without the chain lock) halts the same
    /// way: the block is stored and published, and the error leaves it unacknowledged.
    #[test]
    fn the_consensus_path_halts_too() {
        let (home, c) = open("halt-shared", &genesis("scaffold-1"));
        let chain = parking_lot::Mutex::new(c);
        let f = chain.lock().test_block(vec![]);
        Chain::execute_shared(&chain, f).unwrap();
        chain.lock().state.upgrade.schedule = vec![crate::upgrade::ScheduledUpgrade { version: crate::consensus::SUPPORTED_PROTOCOL_VERSION + 1, activation_height: 3 }];
        let f = chain.lock().test_block(vec![]);
        let err = Chain::execute_shared(&chain, f).unwrap_err().to_string();
        assert!(err.contains("block 3 runs protocol"), "{err}");
        let c = chain.into_inner();
        assert_eq!((c.height(), c.view().height), (2, 2));
        assert!(c.halt.signal.get().is_some());
        drop(c);
        assert!(Chain::open(&home).unwrap().halt.signal.get().is_some());
        fs::remove_dir_all(home).ok();
    }

    /// The scaffold's state (parameters, account locks, operators' bonds and commissions,
    /// reserved names, the Soul ledger, features, the module namespaces and scalars) persists in
    /// the state database like every other record: a restart loads it with the identical root
    /// and replays nothing, and a rebuild from the block logs reaches the same root.
    #[test]
    fn v1_genesis_restarts_from_the_store_without_replay() {
        let g = genesis("scaffold-1");
        let (home, mut c) = open("restart", &g);
        let (owner, founder, bob) = (keys(22), keys(23), keys(24));
        let v = Keys::from_seed(VALIDATOR).address();
        c.submit(signed(&c, &owner, Action::RegisterName { name: "owner".into() })).unwrap();
        // The founder's allocation is locked before its cliff (econ.rs); the operator's isn't.
        assert!(c.submit(signed(&c, &founder, Action::Transfer { to: bob.address(), amount: 5 * FANG })).unwrap_err().to_string().contains("locked (founder)"));
        c.submit(signed(&c, &owner, Action::Transfer { to: bob.address(), amount: 5 * FANG })).unwrap();
        c.produce_block().unwrap();
        // A name is offered, a module tx fails (and is charged), a stale tx is skipped.
        let give = signed(&c, &owner, Action::TransferName { name: "owner".into(), to: bob.address() });
        let delegate = Tx { nonce: give.nonce + 1, ..signed(&c, &owner, Action::Delegate { validator: v.clone(), amount: 1_000_000 * FANG }) }.sign(&owner);
        let stale = Tx { nonce: 40, ..signed(&c, &owner, Action::Transfer { to: bob.address(), amount: FANG }) }.sign(&owner);
        c.produce_block_with(vec![give.clone(), delegate.clone(), stale.clone()]).unwrap();
        assert!(c.failure(&delegate.id()).unwrap().0.contains("insufficient balance"));
        for _ in 0..3 {
            c.produce_block_with(vec![]).unwrap();
        }
        let records = |c: &Chain| {
            let mut all = vec![];
            c.state.for_each_record(|n, k, v| all.push((n, k.to_vec(), v.to_vec())));
            all
        };
        let (root, height, before) = (c.state.root(), c.height(), records(&c));
        assert!(before.iter().any(|(n, _, _)| *n == ns::RESERVED_NAMES) && before.iter().any(|(n, _, _)| *n == ns::FEATURES));
        // History names the accounts each tx names, not just senders.
        let history = |c: &Chain, who: &str| -> Vec<String> { c.view().history(who, 10).into_iter().map(|(h, i)| c.block(h).unwrap().txs[i].id()).collect() };
        assert_eq!(history(&c, &bob.address()), vec![give.id(), c.block(1).unwrap().txs[1].id()]);
        assert_eq!(history(&c, &v), vec![delegate.id()]);
        drop(c);

        let c = Chain::open(&home).unwrap();
        assert_eq!(c.replayed, 0, "nothing replayed");
        assert_eq!((c.state.root(), c.height(), c.state.full_root()), (root.clone(), height, c.state.root_hash()));
        assert_eq!(records(&c), before, "every record came back byte for byte");
        let s = &c.state;
        assert_eq!((s.chain_params.max_validators, s.chain_params.proposal_deposit), (40, 50 * FANG));
        assert_eq!(s.chain_params, g.chain_params().unwrap());
        assert_eq!(s.account(&founder.address()).locks, vec![Lock { id: crate::econ::VESTING_LOCK.into(), amount: LockAmount::Vesting { total: 1_000_000 * FANG, start: 0, cliff: 100, end: 1_000 }, reason: "founder".into() }]);
        // The validator proposed the block that carried the stale tx and holds no balance of its
        // own: its pool's self-bond paid for the junk (CV-5).
        assert_eq!((s.validators[&v].owner.as_str(), crate::staking::self_bond(s, &v), s.validators[&v].commission_bps), (owner.address().as_str(), 10_000 * FANG - crate::econ::base_fee(s, height, stale.size()), 500));
        assert!(s.econ.reserved_names.contains_key("blockula") && s.econ.reserved_names.contains_key("foundation"));
        assert_eq!(s.soul_of(&owner.address(), height), 50 * SOUL);
        assert!(crate::upgrade::is_active(s, "ghost"));
        assert_eq!((s.resolve("owner.boo").unwrap(), s.names["owner"].offered_to.clone()), (owner.address(), Some(bob.address())), "offered to bob, still the owner's");
        assert_eq!(c.block(2).unwrap().skipped, vec![stale.size()]);
        assert_eq!(history(&c, &v), vec![delegate.id()]);
        s.check_invariants().unwrap();
        drop(c);

        // Without its database, the home replays the logs from genesis to the same state.
        fs::remove_file(home.join(store::DB_FILE)).unwrap();
        let c = Chain::open(&home).unwrap();
        assert_eq!((c.state.root(), c.replayed), (root, height));
        assert_eq!(records(&c), before);
        assert_eq!(history(&c, &bob.address()), vec![give.id(), c.block(1).unwrap().txs[1].id()]);
        drop(c);
        fs::remove_dir_all(home).ok();
    }
}

/// Persistence (CV-1, CV-6): restarts read the state database instead of replaying history, a
/// crash at any write point recovers on the next start without help, and pruning keeps a window.
#[cfg(test)]
mod persistence {
    use super::*;
    use std::time::Instant;

    fn home(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("blockula-persist-{tag}-{}-{}", now_ms(), std::process::id()))
    }

    fn tx(c: &Chain, keys: &Keys, action: Action) -> Tx {
        let from = keys.address();
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), nonce: c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: c.height() + 50, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        tx.sign(keys)
    }

    /// A chain with some history: faucet drips, a name, transfers, empty blocks.
    fn active(tag: &str, blocks: u64) -> (PathBuf, Chain, Keys) {
        let h = home(tag);
        Chain::summon(&h, &SummonOpts::new(tag, 1)).unwrap();
        let mut c = Chain::open(&h).unwrap();
        let alice = Keys::from_seed([1; 32]);
        c.faucet(&alice.address(), 100 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        c.submit(tx(&c, &alice, Action::RegisterName { name: "alice".into() })).unwrap();
        c.produce_block().unwrap();
        while c.height() < blocks {
            let to = Keys::from_seed([(c.height() % 200) as u8 + 2; 32]).address();
            c.submit(tx(&c, &alice, Action::Transfer { to, amount: DROPS_PER_FANG / 10 })).unwrap();
            c.produce_block().unwrap();
        }
        (h, c, alice)
    }

    /// Entries in the block log (`cblocks`: the consensus block log), from the start.
    fn entries(c: &Chain, cblocks: bool) -> Vec<store::Entry> {
        let w = c.writer.lock();
        let log = if cblocks { &w.cblocks } else { &w.blocks };
        log.reader().iter_from((0, 0)).collect::<Result<Vec<_>>>().unwrap()
    }

    #[test]
    fn restart_reads_the_store_and_replays_nothing() {
        let (h, c, alice) = active("restart", 40);
        let (root, height, hash) = (c.state.root(), c.height(), c.last_hash().to_string());
        let ids: Vec<String> = c.block(height).unwrap().txs.iter().map(Tx::id).collect();
        drop(c);
        let c = Chain::open(&h).unwrap();
        assert_eq!(c.replayed, 0, "every block was committed; restart executes nothing");
        assert_eq!((c.state.root(), c.height(), c.last_hash()), (root, height, hash.as_str()));
        assert_eq!(c.state.full_root(), c.state.root_hash(), "records and tree agree");
        // The indexes and history came back from the database, not from a replay.
        assert_eq!(c.tx_location(&ids[0]), Some((height, 0)));
        let v = c.view();
        // A tx is read alone from its span in the block's line (nothing is cached yet).
        let m = v.block_meta(10).unwrap();
        assert_eq!((m.tx_spans.len(), m.tx_count), (1, 1));
        let (t, failed) = v.finalized_tx(10, 0).unwrap();
        assert!(v.disk.cached_block(10).is_none(), "the block wasn't parsed");
        assert_eq!((t, failed), (v.block(10).unwrap().txs[0].clone(), None));
        assert_eq!(v.history(&alice.address(), 100).len(), 40);
        assert_eq!(v.primary_name(&alice.address()).as_deref(), Some("alice.boo"));
        assert_eq!(v.block(1).unwrap().height, 1);
        assert_eq!(c.last_block().unwrap().height, height);
        assert_eq!(c.root_at(7), Some(c.block(7).unwrap().state_root));
        drop((v, c));
        fs::remove_dir_all(h).ok();
    }

    #[test]
    fn crash_at_every_write_point_recovers_unaided() {
        for point in [WritePoint::AfterCblock, WritePoint::AfterBlockLine, WritePoint::BeforeStoreCommit, WritePoint::AfterStoreCommit] {
            let (h, mut c, alice) = active(&format!("crash-{point:?}"), 6);
            let (height, before) = (c.height(), c.state.root());
            let t = tx(&c, &alice, Action::Transfer { to: c.me().unwrap(), amount: 7 });
            c.submit(t.clone()).unwrap();
            c.writer.lock().fail_at = Some(point);
            assert!(c.produce_block().is_err(), "{point:?}");
            let after = c.state.root(); // memory ran ahead of the disk
            c.writer.lock().fail_at = None;
            assert!(c.produce_block().unwrap_err().to_string().contains("restart"), "a broken instance executes nothing more");
            drop(c);
            let mut c = Chain::open(&h).unwrap();
            if point == WritePoint::AfterCblock {
                // The block never reached the block log: it's gone, and so is its consensus block.
                assert_eq!((c.height(), c.state.root(), c.replayed), (height, before, 0));
                assert_eq!(entries(&c, true).len() as u64, height, "the orphaned consensus block was cut off");
                // Consensus delivers it again.
                c.produce_block_with(vec![t.clone()]).unwrap();
                assert_eq!(c.state.root(), after);
            } else {
                assert_eq!((c.height(), c.state.root()), (height + 1, after), "{point:?}");
                assert_eq!(c.replayed, if point == WritePoint::AfterStoreCommit { 0 } else { 1 }, "{point:?}");
            }
            assert!(c.has_tx(&t.id()));
            assert_eq!(c.cblock(height + 1), Some(vec![]));
            assert_eq!(entries(&c, true).len(), entries(&c, false).len());
            // It carries on, and the next restart is clean.
            c.produce_block_with(vec![]).unwrap();
            let root = c.state.root();
            drop(c);
            let c = Chain::open(&h).unwrap();
            assert_eq!((c.state.root(), c.replayed), (root, 0));
            drop(c);
            fs::remove_dir_all(h).ok();
        }
    }

    /// A crash mid-append leaves a torn last entry; the node cuts it off and starts. A damaged
    /// entry in the middle of the log only matters to a rebuild, which refuses.
    #[test]
    fn torn_tails_are_cut_and_damaged_history_refuses_a_rebuild() {
        let (h, mut c, alice) = active("torn", 5);
        let (height, root) = (c.height(), c.state.root());
        c.submit(tx(&c, &alice, Action::Transfer { to: c.me().unwrap(), amount: 7 })).unwrap();
        c.writer.lock().fail_at = Some(WritePoint::AfterBlockLine);
        assert!(c.produce_block().is_err());
        drop(c);
        let seg = h.join("blocks").join(format!("{:012}.jsonl", 1));
        let len = fs::metadata(&seg).unwrap().len();
        OpenOptions::new().write(true).open(&seg).unwrap().set_len(len - 5).unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.height(), c.state.root(), c.replayed), (height, root.clone(), 0));
        let logged = entries(&c, false);
        assert_eq!(logged.len() as u64, height);
        assert_eq!(fs::metadata(&seg).unwrap().len(), logged.last().unwrap().pos.end().1, "the torn tail is gone");
        assert_eq!(entries(&c, true).len() as u64, height);
        drop(c);
        // Damage block 2's line: a normal restart never reads it…
        let mut bytes = fs::read(&seg).unwrap();
        let at = logged[1].pos.off as usize + 10;
        bytes[at] = b'#';
        fs::write(&seg, &bytes).unwrap();
        assert_eq!(Chain::open(&h).unwrap().state.root(), root);
        // …but rebuilding the database from the log stops there.
        fs::remove_file(h.join(store::DB_FILE)).unwrap();
        let err = format!("{:#}", Chain::open(&h).err().unwrap());
        assert!(err.contains("bad entry"), "{err}");
        fs::remove_dir_all(h).ok();
    }

    #[test]
    fn a_missing_or_damaged_database_is_rebuilt_from_the_logs() {
        let (h, c, alice) = active("rebuild", 30);
        let (root, height) = (c.state.root(), c.height());
        let id = c.block(10).unwrap().txs[0].id();
        drop(c);
        fs::remove_file(h.join(store::DB_FILE)).unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.state.root(), c.height(), c.replayed), (root.clone(), height, height));
        assert_eq!(c.tx_location(&id), Some((10, 0)));
        assert_eq!(c.view().block_meta(10).unwrap().tx_spans.len(), 1, "replayed blocks get spans too");
        assert_eq!(c.view().history(&alice.address(), 100).len() as u64, height);
        drop(c);
        // Garbage instead of a database: moved aside and rebuilt the same way.
        fs::write(h.join(store::DB_FILE), b"not a database").unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.state.root(), c.replayed), (root.clone(), height));
        assert_eq!(fs::read(h.join("state.redb.unreadable")).unwrap(), b"not a database");
        drop(c);
        // A database another process holds is an error, never a rebuild.
        let held = Store::open(&h.join(store::DB_FILE)).unwrap();
        assert!(format!("{:#}", Chain::open(&h).err().unwrap()).contains("already open"));
        drop(held);
        // A database left from another chain (another genesis) is rebuilt too.
        let other = home("other");
        Chain::summon(&other, &SummonOpts::new("other", 1)).unwrap();
        drop(Chain::open(&other).unwrap());
        fs::copy(other.join(store::DB_FILE), h.join(store::DB_FILE)).unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.state.root(), c.replayed), (root, height));
        drop(c);
        fs::remove_dir_all(other).ok();
        fs::remove_dir_all(h).ok();
    }

    /// F08: a genesis home that hasn't executed anything ignores an `epochs.json` it finds (a fast
    /// sync's verified history that a crash kept from installing, or anyone's): it starts from the
    /// genesis set, and its validator signs no handoff for the changes that file claimed.
    #[test]
    fn a_genesis_home_ignores_a_saved_history_it_never_executed() {
        let h = home("planted");
        Chain::summon(&h, &SummonOpts::new("planted", 1)).unwrap();
        let c = Chain::open(&h).unwrap();
        let genesis = crate::epochs::EpochRegistry::genesis(&c.genesis);
        assert!(c.keys.is_some(), "a validator's home");
        drop(c);
        let mut planted = genesis.clone();
        assert!(planted.record(5, vec![hex::encode(commonware_cryptography::Signer::public_key(&crate::pq::PrivateKey::from_seed_bytes([200; 32])).as_ref())]));
        fs::write(h.join("epochs.json"), serde_json::to_vec(&planted).unwrap()).unwrap();
        let mut c = Chain::open(&h).unwrap();
        assert_eq!(c.epochs, genesis);
        assert_eq!(c.sign_handoffs(), 0);
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// F08: a home that executed blocks forgets whatever its `epochs.json` records past the epoch
    /// it executed (an older binary adopted a fast-sync peer's registry whole), so a planted
    /// change neither stays in what it serves nor gets signed when its epoch comes.
    #[test]
    fn history_past_the_executed_epoch_is_dropped_on_open() {
        let (h, c, _) = active("ahead", 3);
        let executed = c.epochs.clone();
        let want = c.state.params.epoch_of(c.height() + 1);
        assert_eq!(executed.known_through, want);
        drop(c);
        let mut planted = executed.clone();
        let evil = hex::encode(commonware_cryptography::Signer::public_key(&crate::pq::PrivateKey::from_seed_bytes([200; 32])).as_ref());
        assert!(planted.record(want + 3, vec![evil]));
        planted.format = crate::epochs::HANDOFF_FORMAT;
        fs::write(h.join("epochs.json"), serde_json::to_vec(&planted).unwrap()).unwrap();
        let mut c = Chain::open(&h).unwrap();
        assert_eq!((c.epochs.known_through, c.epochs.sets.keys().next_back().copied()), (want, executed.sets.keys().next_back().copied()));
        assert_eq!(*c.live.epochs(), c.epochs, "and that's what it serves");
        assert_eq!(c.sign_handoffs(), 0);
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// review-fix-sync-4: a home saved by a binary with an older handoff format (here, a change
    /// at epochs 2 and 3, long out of the state's set window) drops the proofs on open, and the
    /// history starts at the oldest set the state holds, so no change nobody can sign again
    /// holds `proven_through` back.
    #[test]
    fn a_handoff_format_upgrade_restarts_the_history_at_the_oldest_held_set() {
        let h = home("hf-upgrade");
        let params = BTreeMap::from([("evidence_max_age_epochs".to_string(), 1)]);
        Chain::summon(&h, &SummonOpts { epoch_blocks: 2, params, ..SummonOpts::new("hf-upgrade", 1) }).unwrap();
        let mut c = Chain::open(&h).unwrap();
        while c.height() < 40 {
            c.produce_block_with(vec![]).unwrap();
        }
        let oldest = *c.state.active_sets.keys().next().unwrap();
        assert!(oldest > 4, "the state keeps a window of sets: {oldest}");
        let mut old = c.epochs.clone();
        drop(c);
        let other = vec![hex::encode(commonware_cryptography::Signer::public_key(&crate::pq::PrivateKey::from_seed_bytes([200; 32])).as_ref())];
        let genesis_set = old.sets[&0].clone();
        assert!(old.record(2, other) && old.record(3, genesis_set));
        old.proofs.insert(2, BTreeMap::from([(0, "00".into())]));
        old.format = crate::epochs::HANDOFF_FORMAT - 1;
        fs::write(h.join("epochs.json"), serde_json::to_vec(&old).unwrap()).unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.epochs.format, c.epochs.base), (crate::epochs::HANDOFF_FORMAT, oldest));
        assert!(c.epochs.proofs.is_empty() && c.epochs.changes().next().is_none());
        assert_eq!(c.epochs.proven_through(), c.epochs.known_through, "nothing left to ask peers for");
        assert_eq!(*c.live.epochs(), c.epochs);
        let saved: crate::epochs::EpochRegistry = serde_json::from_slice(&fs::read(h.join("epochs.json")).unwrap()).unwrap();
        assert_eq!(saved.base, oldest);
        drop(c);
        // Opened again (already at the format), nothing moves.
        let c = Chain::open(&h).unwrap();
        assert_eq!(c.epochs.base, oldest);
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    #[test]
    fn half_written_snapshots_and_temp_files_are_cleaned_up() {
        let (h, c, _) = active("snap", 3);
        let root = c.state.root();
        drop(c);
        let partial = crate::snapshot::dir(&h).join("100.tmp");
        fs::create_dir_all(&partial).unwrap();
        fs::write(partial.join("chunk-0"), b"half a chunk").unwrap();
        fs::write(h.join("epochs.json.tmp"), b"{").unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!(c.state.root(), root);
        assert!(!partial.exists(), "the half-written snapshot was removed");
        assert!(crate::snapshot::list(&h).is_empty());
        assert!(crate::snapshot::build_manifest(&c.live).is_err());
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// A validator keeps a window of blocks: older index rows, certificates and whole log
    /// segments go; epoch boundaries' hashes and certificates stay. Restarts still replay
    /// nothing, and a lost database is rebuilt from the node's own snapshot.
    #[test]
    fn pruning_keeps_a_window_and_rebuilds_from_a_local_snapshot() {
        let h = home("prune");
        // Minute blocks: an hour of tx validity is 60 blocks, so the minimum window is small.
        Chain::summon(&h, &SummonOpts { block_time_ms: 60_000, epoch_blocks: 100, ..SummonOpts::new("prune", 1) }).unwrap();
        let mut c = Chain::open_with(&h, &OpenOpts { segment_heights: Some(50) }).unwrap();
        c.set_pruning(Some(1));
        let keep = min_keep_blocks(&c.state);
        assert_eq!(c.keep_blocks(), Some(keep));
        let alice = Keys::from_seed([1; 32]);
        c.faucet(&alice.address(), 100 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let old_tx = c.block(1).unwrap().txs[0].id();
        for _ in 0..708 {
            c.produce_block_with(vec![]).unwrap();
            if c.height() == 200 {
                c.store_cert(99, vec![9]).unwrap(); // an epoch's last block
                c.store_cert(200, vec![2]).unwrap(); // a checkpoint
            }
        }
        let height = c.height();
        let v = c.view();
        let from = height + 1 - keep;
        assert_eq!(v.history_from(), from);
        assert!(v.block(from).is_some() && v.block(from - 1).is_none() && v.is_pruned(10));
        assert!(!c.has_tx(&old_tx), "pruned heights leave the tx index");
        assert!(v.history(&alice.address(), 10).is_empty());
        assert!(v.hash_at(99).is_some() && v.root_at(199).is_some(), "epoch boundaries stay");
        assert!(v.hash_at(150).is_none());
        assert_eq!((v.snap.cert(99).unwrap(), v.snap.cert(200).unwrap()), (Some(vec![9]), None));
        let (segs, csegs) = {
            let w = c.writer.lock();
            (w.blocks.reader().segments(), w.cblocks.reader().segments())
        };
        assert!(segs[0] > 1 && segs[0] <= from && segs[0] + 50 > from - 50, "segments below the window are deleted: {segs:?}");
        assert_eq!(csegs, segs);
        drop(v);
        let root = c.state.root();
        drop(c);
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.state.root(), c.replayed, c.view().history_from()), (root.clone(), 0, from));
        // The snapshot writer runs in the background: wait for the latest checkpoint's.
        let t = Instant::now();
        while !crate::snapshot::list(&h).contains(&700) {
            assert!(t.elapsed().as_secs() < 30, "no snapshot at 700");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(c);
        fs::remove_file(h.join(store::DB_FILE)).unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.state.root(), c.height()), (root, height));
        assert_eq!(c.replayed, height - 700, "rebuilt from snapshot 700, then the logged blocks after it");
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// The smallest window still holds the epoch anchor of every manifest the node serves, at
    /// the worst point: from the end of an epoch, the newest snapshot (late in that epoch) needs
    /// the previous epoch's last block, an epoch back. Minute blocks make the tx validity window
    /// small (60 blocks), so the epoch (300 blocks) is what the window has to cover.
    #[test]
    fn the_smallest_window_keeps_the_epoch_anchor() {
        let h = home("prune-anchor");
        Chain::summon(&h, &SummonOpts { block_time_ms: 60_000, epoch_blocks: 300, ..SummonOpts::new("prune-anchor", 1) }).unwrap();
        let mut c = Chain::open_with(&h, &OpenOpts { segment_heights: Some(50) }).unwrap();
        c.set_pruning(Some(1));
        let keep = min_keep_blocks(&c.state);
        assert!(keep >= 300 + 2 * crate::snapshot::CHECKPOINT_INTERVAL + crate::snapshot::CHECKPOINT_DELAY, "{keep}");
        // Consensus hands over certificates for epoch ends and checkpoints' certifying blocks.
        while c.height() < 899 {
            c.produce_block_with(vec![]).unwrap();
            let height = c.height();
            if c.state.params.is_boundary(height) || crate::snapshot::certifies(height).is_some() {
                c.store_cert(height, vec![(height % 251) as u8]).unwrap();
            }
        }
        let t = Instant::now();
        while !crate::snapshot::list(&h).contains(&800) {
            assert!(t.elapsed().as_secs() < 30, "no snapshot at 800");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let v = c.view();
        assert!(v.history_from() > 299, "the node prunes: history from #{}", v.history_from());
        let m = crate::snapshot::build_manifest(&c.live).expect("a manifest with its anchor");
        assert_eq!(m.height, 800);
        let anchor = m.anchor.expect("snapshot 800 is in epoch 2, after #599");
        assert_eq!(anchor.finalization, hex::encode([(599 % 251) as u8]));
        assert!(v.cblock(599).is_some());
        drop(v);
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// F63: a big state is served from the newest long-lived snapshot (every 200 blocks at
    /// 4.5 s blocks: `snapshot::long_interval`), and a joiner that took its manifest still finds
    /// its chunks and the blocks after it once newer snapshots have replaced the newest twice
    /// over, on a node keeping the smallest window. A joiner's checkpoint skips what's older.
    #[test]
    fn a_served_snapshot_outlives_newer_ones() {
        use crate::snapshot::{build_manifest_with, chunk, list, long_interval, CHECKPOINT_DELAY};
        let h = home("long-snap");
        let params = BTreeMap::from([("max_tx_validity_blocks".to_string(), 10)]);
        Chain::summon(&h, &SummonOpts { block_time_ms: 4_500, epoch_blocks: 100, params, ..SummonOpts::new("long-snap", 1) }).unwrap();
        let mut c = Chain::open_with(&h, &OpenOpts { segment_heights: Some(50) }).unwrap();
        c.set_pruning(Some(1));
        assert_eq!(long_interval(&c.state.params), 200);
        let keep = min_keep_blocks(&c.state);
        assert_eq!(keep, 100 + 2 * 200 + 2 * CHECKPOINT_DELAY + crate::consensus::MAX_EXEC_LAG + 10);
        // Consensus hands over certificates for epoch ends and checkpoints' certifying blocks.
        let run_to = |c: &mut Chain, to: u64, gone: u64| {
            while c.height() < to {
                c.produce_block_with(vec![]).unwrap();
                let height = c.height();
                if c.state.params.is_boundary(height) || crate::snapshot::certifies(height).is_some() {
                    c.store_cert(height, vec![(height % 251) as u8]).unwrap();
                }
            }
            let t = Instant::now();
            while !list(&h).contains(&(to / 100 * 100)) || list(&h).contains(&gone) {
                assert!(t.elapsed().as_secs() < 30, "snapshots: {:?}", list(&h));
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        };
        run_to(&mut c, 999, 700);
        assert_eq!(list(&h), vec![900, 800, 600]);
        let big = build_manifest_with(&c.live, 0, 0).unwrap();
        assert_eq!((big.height, big.anchor.as_ref().map(|a| a.finalization.clone())), (800, Some(hex::encode([(799 % 251) as u8]))));
        assert_eq!(crate::snapshot::build_manifest(&c.live).unwrap().height, 900, "a small state: the newest");
        // Two checkpoints later the newest (900) is gone; the long-lived 800 isn't, nor are the
        // blocks after its manifest, which the joiner syncs next.
        run_to(&mut c, 1_199, 900);
        assert_eq!(list(&h), vec![1_100, 1_000, 800]);
        assert!(chunk(&h, 800, 0).is_some());
        let v = c.view();
        assert_eq!(v.history_from(), 1_200 - keep);
        assert!(v.cblock(805).is_some() && v.block(805).is_some());
        assert_eq!(build_manifest_with(&c.live, 0, 0).unwrap().height, 1_000);
        // A joiner whose checkpoint is past 1000's manifest gets the newest that reaches it.
        assert_eq!(build_manifest_with(&c.live, 1_005, 0).unwrap().height, 1_100);
        // Past every manifest: the one reaching furthest, so the joiner sees it's behind its
        // checkpoint and waits for the next snapshot (review-fix-sync-3).
        let furthest = build_manifest_with(&c.live, 1_105, 0).unwrap();
        assert!(furthest.height == 1_100 && furthest.height + (furthest.blocks.len() as u64) < 1_105, "#{}", furthest.height);
        drop(v);
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// A certificate execution didn't get (consensus missed it) is kept afterwards for the
    /// heights that keep one: an epoch's last block, a checkpoint. Only for the block executed
    /// there, once, and not for heights that keep none.
    #[test]
    fn late_certificates_fill_in_epoch_ends() {
        let h = home("late-cert");
        Chain::summon(&h, &SummonOpts { epoch_blocks: 10, ..SummonOpts::new("late-cert", 1) }).unwrap();
        let chain = parking_lot::Mutex::new(Chain::open(&h).unwrap());
        while chain.lock().height() < 25 {
            chain.lock().produce_block_with(vec![]).unwrap();
        }
        let hash = |height: u64| chain.lock().hash_at(height).unwrap();
        assert_eq!(chain.lock().cert(19), None);
        assert!(Chain::store_late_cert(&chain, 19, &hash(18), vec![19]).unwrap_err().to_string().contains("another block"));
        assert!(!Chain::store_late_cert(&chain, 15, &hash(15), vec![15]).unwrap(), "#15 keeps no certificate");
        assert!(!Chain::store_late_cert(&chain, 29, &"00".repeat(32), vec![29]).unwrap(), "not executed yet");
        assert!(Chain::store_late_cert(&chain, 19, &hash(19), vec![19]).unwrap());
        assert!(!Chain::store_late_cert(&chain, 19, &hash(19), vec![1, 9]).unwrap(), "already kept");
        assert_eq!(chain.lock().cert(19), Some(vec![19]));
        // On disk: the next published snapshot of the store has it, and so does a restart.
        chain.lock().produce_block_with(vec![]).unwrap();
        assert_eq!(chain.lock().view().snap.cert(19).unwrap(), Some(vec![19]));
        drop(chain);
        let c = Chain::open(&h).unwrap();
        assert_eq!(c.view().snap.cert(19).unwrap(), Some(vec![19]));
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// The kept window covers a tx's whole validity window as governance sets it now: widening
    /// `max_tx_validity_blocks` pauses pruning until the window has grown to match.
    #[test]
    fn pruning_follows_the_tx_validity_parameter() {
        let h = home("prune-param");
        Chain::summon(&h, &SummonOpts { block_time_ms: 60_000, epoch_blocks: 100, ..SummonOpts::new("prune-param", 1) }).unwrap();
        let mut c = Chain::open(&h).unwrap();
        c.set_pruning(Some(1));
        let keep = min_keep_blocks(&c.state);
        while c.height() < keep + 40 {
            c.produce_block_with(vec![]).unwrap();
        }
        let pruned = c.view().history_from();
        assert_eq!(pruned, c.height() + 1 - keep);
        // A ParamChange proposal executing (the lever it calls) widens the window by 100 blocks.
        let wider = c.state.chain_params.max_tx_validity_blocks + 100;
        c.state.exec_set_param(&mut Journal::default(), "max_tx_validity_blocks", wider).unwrap();
        assert_eq!(min_keep_blocks(&c.state), keep + 100);
        for _ in 0..50 {
            c.produce_block_with(vec![]).unwrap();
        }
        assert_eq!(c.view().history_from(), pruned, "nothing more is pruned until the window is wide enough");
        for _ in 0..100 {
            c.produce_block_with(vec![]).unwrap();
        }
        assert_eq!(c.view().history_from(), c.height() + 1 - (keep + 100));
        drop(c);
        fs::remove_dir_all(h).ok();
    }

    /// Disk per block (docs/OPERATIONS.md, Disk sizing): block logs and the state database's
    /// index rows, for empty blocks and for blocks of transfers.
    /// `cargo test --release disk_per_block -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn disk_per_block() {
        fn size(p: &Path) -> u64 {
            fs::read_dir(p).map(|d| d.flatten().map(|e| if e.path().is_dir() { size(&e.path()) } else { e.metadata().map(|m| m.len()).unwrap_or(0) }).sum()).unwrap_or(0)
        }
        for per_block in [0u64, 10] {
            let (h, mut c, alice) = active(&format!("disk-{per_block}"), 3);
            let mut keys = vec![];
            for i in 0..per_block {
                let k = Keys::from_seed(crypto::hash(&i.to_le_bytes()));
                c.state.accounts.insert(k.address(), Account { balance: 1_000 * DROPS_PER_FANG, ..Default::default() });
                c.state.supply += 1_000 * DROPS_PER_FANG;
                keys.push(k);
            }
            c.produce_block_with(vec![]).unwrap();
            let before = (size(&h.join("blocks")), size(&h.join("cblocks")), fs::metadata(h.join(store::DB_FILE)).unwrap().len());
            let n = 1_000;
            for _ in 0..n {
                let txs: Vec<Tx> = keys.iter().map(|k| tx(&c, k, Action::Transfer { to: alice.address(), amount: 1 })).collect();
                // The consensus block carries the txs' wire bytes, as in production.
                let cblock: Vec<u8> = txs.iter().flat_map(|t| t.to_bytes()).collect();
                let height = c.height() + 1;
                let f = Finalized { height, hash: hex::encode(crypto::hash(&height.to_le_bytes())), parent: c.last_hash().to_string(), timestamp_ms: now_ms(), view: height, parent_view: height - 1, version: crate::consensus::PROTOCOL_VERSION, signal: crate::consensus::SUPPORTED_PROTOCOL_VERSION, proposer: c.fee_sink(), txs, cblock: [vec![0u8; 170], cblock].concat(), certificate: None };
                c.execute(f).unwrap();
            }
            let after = (size(&h.join("blocks")), size(&h.join("cblocks")), fs::metadata(h.join(store::DB_FILE)).unwrap().len());
            println!("{per_block} transfers per block: blocks/ {} B, cblocks/ {} B, state.redb {} B per block", (after.0 - before.0) / n, (after.1 - before.1) / n, after.2.saturating_sub(before.2) / n);
            drop(c);
            fs::remove_dir_all(h).ok();
        }
    }

    /// SF-4: on the consensus path the chain lock isn't held while a block is written and
    /// fsynced (here, a disk that takes 300 ms). Meanwhile consensus sees the new height and
    /// root, the mempool takes txs, and readers see the previous block until it's on disk.
    #[test]
    fn blocks_are_written_without_the_chain_lock() {
        let (h, c, alice) = active("sf4", 3);
        let t = tx(&c, &alice, Action::Transfer { to: c.me().unwrap(), amount: 7 });
        let f = c.test_block(vec![t.clone()]);
        let chain = Arc::new(parking_lot::Mutex::new(c));
        chain.lock().writer.lock().delay = Some(std::time::Duration::from_millis(300));
        let shared = chain.clone();
        let started = Instant::now();
        let exec = std::thread::spawn(move || Chain::execute_shared(&shared, f));
        while chain.lock().height() < 4 {
            assert!(started.elapsed().as_secs() < 10);
            std::thread::yield_now();
        }
        let waited = Instant::now();
        {
            let mut c = chain.lock();
            assert_eq!(c.root_at(4), Some(c.state.root()), "consensus can check the new root");
            assert!(c.has_tx(&t.id()) && c.tx_location(&t.id()) == Some((4, 0)));
            assert_eq!(c.view().height, 3, "readers see the block once it's on disk");
            let next = tx(&c, &alice, Action::Transfer { to: c.me().unwrap(), amount: 8 });
            c.submit(next).unwrap();
        }
        let held = waited.elapsed();
        assert!(!exec.is_finished(), "still writing");
        assert!(held < std::time::Duration::from_millis(250), "the chain lock was free during the write ({held:?})");
        exec.join().unwrap().unwrap();
        let total = started.elapsed();
        let c = chain.lock();
        assert_eq!((c.view().height, c.mempool.len()), (4, 1));
        println!("block written in {total:?}; the chain lock was free throughout the 300 ms write");
        drop(c);
        drop(chain);
        fs::remove_dir_all(h).ok();
    }

    /// Restart time at 1M accounts: loading the records is O(state), never O(history).
    /// `cargo test --release restart_time_at_1m_accounts -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn restart_time_at_1m_accounts() {
        let h = home("bench");
        Chain::summon(&h, &SummonOpts::new("bench", 1)).unwrap();
        let mut c = Chain::open(&h).unwrap();
        for i in 0..1_000_000u64 {
            c.state.accounts.insert(crypto::address_from_bytes(&crypto::hash(&i.to_le_bytes())), Account { balance: DROPS_PER_FANG, ..Default::default() });
        }
        c.state.supply += 1_000_000 * DROPS_PER_FANG;
        let t = Instant::now();
        c.produce_block_with(vec![]).unwrap();
        println!("committed 1M new accounts in one block: {:?}", t.elapsed());
        for _ in 0..100 {
            c.produce_block_with(vec![]).unwrap();
        }
        let root = c.state.root();
        drop(c);
        let t = Instant::now();
        let c = Chain::open(&h).unwrap();
        let took = t.elapsed();
        println!("restart with {} records: {took:?} ({} blocks replayed); state.redb is {} MB", c.state.record_count(), c.replayed, fs::metadata(h.join(store::DB_FILE)).unwrap().len() >> 20);
        assert_eq!((c.state.root(), c.replayed), (root, 0));
        drop(c);
        fs::remove_dir_all(h).ok();
    }
}
