//! Chain state and execution. Consensus (`consensus.rs`) orders blocks; this module executes
//! finalized blocks deterministically, in order, and keeps the mempool, indexes and `blocks.jsonl`.

use crate::crypto::{self, Keys};
use crate::params::ChainParams;
use crate::state::{JMap, TMap};
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Context, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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

/// The keyless validator bootstrap delegation (DEC-5): delegated by formula, returned to the
/// treasury in years 2–4. (The foundation is an ordinary multisig account.)
pub fn bootstrap_delegation(prefix: &str) -> String {
    crypto::system_address(prefix, "bootstrap-delegation")
}

/// What a keyless system account is (`"treasury"`, `"community"`, `"endowment"`,
/// `"bootstrap delegation"`, `"contribution pool"`), if `addr` is one on a chain with `prefix`.
/// Nobody holds a key for them: only the protocol moves their funds.
pub fn system_account(prefix: &str, addr: &str) -> Option<&'static str> {
    let roles: [(&'static str, fn(&str) -> String); 5] = [("treasury", treasury), ("community", community), ("endowment", endowment), ("bootstrap delegation", bootstrap_delegation), ("contribution pool", crate::poc::pool_address)];
    roles.into_iter().find(|(_, f)| f(prefix) == addr).map(|(role, _)| role)
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
}

fn default_epoch_blocks() -> u64 {
    600
}
fn default_address_prefix() -> String {
    crypto::ADDRESS_HRP.into()
}

/// How many past epochs' validator sets the state keeps (for verifying recent certificates).
pub const EPOCH_SETS_KEPT: u64 = 8;

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
        (height + 1) % self.epoch_blocks.max(2) == 0
    }
    pub fn last_of(&self, epoch: u64) -> u64 {
        (epoch + 1) * self.epoch_blocks.max(2) - 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidatorStatus {
    /// Registered and bonded, waiting for an admin.
    Pending,
    /// Approved; joins the set at the next epoch boundary (if there's room).
    Approved,
    Active,
    /// Removed by an admin; leaves at the next boundary, bond claimable later.
    Removed,
    /// Bond returned.
    Unbonded,
}

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct ValidatorRecord {
    /// `boo1…` address of the consensus key (the validator's id).
    pub address: String,
    /// Account that registered (and owns the bond).
    pub owner: String,
    pub consensus_key: String,
    pub p2p_addr: String,
    pub http_addr: String,
    #[serde(with = "u64_str")]
    pub bond: u64,
    pub status: ValidatorStatus,
    pub registered_at: u64,
    pub approved_at: Option<u64>,
    pub approved_by: Option<String>,
    /// First epoch it is (or was) in the active set.
    pub active_from: Option<u64>,
    pub removed_at: Option<u64>,
    pub removed_by: Option<String>,
    /// Last epoch in the active set (set when it leaves).
    pub active_until: Option<u64>,
    /// Bond can be reclaimed from this epoch on.
    pub unbond_epoch: Option<u64>,
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
        Params { block_time_ms: self.block_time_ms, epoch_blocks: self.epoch_blocks, admins: self.validator_admins.clone(), poc, address_prefix: self.address_prefix.clone() }
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
                let lock = Lock { id: acct.locks.len() as u64, amount: LockAmount::Vesting { total: a.amount, start: v.start, cliff: v.cliff, end: v.end }, reason: a.label.clone().unwrap_or_else(|| "genesis allocation".into()) };
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
                    bond: 0,
                    status: ValidatorStatus::Active,
                    registered_at: 0,
                    approved_at: Some(0),
                    approved_by: None,
                    active_from: Some(0),
                    removed_at: None,
                    removed_by: None,
                    active_until: None,
                    unbond_epoch: None,
                    commission_bps: 0,
                },
            );
        }
        // Operators own their validators: the self-bond moves from the owner's balance into the
        // validator's bond (an escrow), which is what its stake pool starts from (staking.rs).
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
            v.bond = bond;
            v.commission_bps = o.commission_bps;
        }
        state.active_sets.insert(0, self.validators.iter().map(|v| v.address.clone()).collect());
        for n in &self.reserved_names {
            if let Ok(n) = normalize_name(n) {
                state.econ.reserved_names.insert(n, crate::econ::ReservedName { reserved_at: 0 });
            }
        }
        for e in &self.soul_ledger {
            state.poc.souls.insert(e.account.clone(), crate::poc::Soul { points: e.soul, at: 0, earned: e.soul });
        }
        for f in &self.features {
            state.upgrade.features.insert(f.clone(), crate::upgrade::Feature { activated_at: 0 });
        }
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
    /// for every validator.
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
        let mut features = BTreeSet::new();
        for f in &self.features {
            crate::upgrade::check_feature_name(f)?;
            ensure!(crate::upgrade::KNOWN_FEATURES.contains(&f.as_str()), "unknown feature {f} (known: {})", crate::upgrade::KNOWN_FEATURES.join(", "));
            ensure!(features.insert(f), "feature {f} is listed twice");
        }
        if self.chain_id == MAINNET_CHAIN_ID {
            ensure!(self.validator_admins.is_empty(), "{MAINNET_CHAIN_ID} has no admin keys: validator_admins must be empty");
            ensure!(!self.faucet, "{MAINNET_CHAIN_ID} has no faucet");
            ensure!(self.validators.iter().all(|v| keys.contains(v.public_key.as_str())), "every {MAINNET_CHAIN_ID} validator needs an operator (owner, self-bond, commission)");
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
}

impl Journal {
    pub fn absorb_pub(&mut self, other: Journal) {
        self.absorb(other)
    }

    /// Folds a nested journal in, keeping the oldest values. Exhaustive, like `State::rollback`.
    fn absorb(&mut self, other: Journal) {
        let Journal { accounts, names, validators, hunts, github, github_by_account, familiars, scalars, next_hunt_id, chain_params, poc, staking, governance, econ, beacon, upgrade } = other;
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
    }
}

/// The block the end-of-block steps close ([`State::end_block`]). Execution and replay build the
/// same one from the block (replay from `blocks.jsonl`), so the hooks see identical inputs.
// The hooks that read every field land with their packages (staking reads view, skipped…).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct BlockInfo<'a> {
    pub height: u64,
    pub hash: &'a str,
    /// Paid the block's tips; pays for its skipped txs.
    pub proposer: &'a str,
    /// The consensus view the block was proposed in and its parent's view (0 for an epoch's
    /// first block): every view in between was a missed leader slot (MAINNET-DESIGN 3.4).
    pub view: u64,
    pub parent_view: u64,
    /// Wire sizes of the txs the block carried that couldn't be admitted (skipped).
    pub skipped: &'a [u64],
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
    /// What its locks hold at `height`: they overlap, so the largest applies (MAINNET-DESIGN
    /// 3.7: transferable = liquid − max(0, that − staked − unbonding)). econ.rs enforces it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn locked_at(&self, height: u64) -> u64 {
        self.locks.iter().map(|l| l.amount.locked_at(height)).max().unwrap_or(0)
    }
}

/// A lock on an account's tokens (MAINNET-DESIGN 3.7). Locked tokens stay in the balance and can
/// be staked, delegated and (vested) voted with, never transferred. econ.rs enforces them;
/// governance adds vote locks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Lock {
    /// Unique among the account's locks.
    pub id: u64,
    pub amount: LockAmount,
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
}

impl LockAmount {
    #[cfg_attr(not(test), allow(dead_code))]
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

#[derive(Clone, Debug, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct NameRecord {
    pub name: String,
    pub owner: String,
    pub registered_at: u64,
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

/// Longest a hunt may run: ~1 year of blocks.
pub fn max_hunt_blocks(block_time_ms: u64) -> u64 {
    365 * 86_400 * 1000 / block_time_ms.max(1)
}

/// The chain state. Maps are `TMap`s: cloning a `State` is O(1) (structure is shared), and
/// changed keys are tracked so `commit_block` updates the root from just those. How it maps to
/// records and the root is in `state/mod.rs`.
#[derive(Clone, Default)]
pub struct State {
    pub accounts: TMap<String, Account>,
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

    /// This chain's contribution pool account.
    pub fn pool(&self) -> String {
        crate::poc::pool_address(self.params.prefix())
    }

    /// Active set for an epoch (validator records, in set order), if known.
    pub fn active_set(&self, epoch: u64) -> Option<Vec<&ValidatorRecord>> {
        self.active_sets.get(&epoch).map(|s| s.iter().filter_map(|a| self.validators.get(a)).collect())
    }

    /// The set epoch `epoch + 1` would get if the boundary were now: current members that
    /// weren't removed, then approved validators in approval order, capped. Never empty.
    pub fn next_set(&self, epoch: u64) -> Vec<String> {
        let current = self.active_sets.get(&epoch).cloned().unwrap_or_default();
        let mut next: Vec<String> = current.iter().filter(|a| self.validators.get(*a).is_some_and(|v| v.status == ValidatorStatus::Active)).cloned().collect();
        let mut approved: Vec<&ValidatorRecord> = self.validators.values().filter(|v| v.status == ValidatorStatus::Approved).collect();
        approved.sort_by_key(|v| (v.approved_at, v.address.clone()));
        for v in approved {
            if next.len() as u64 >= self.chain_params.max_validators.max(1) {
                break;
            }
            next.push(v.address.clone());
        }
        if next.is_empty() { current } else { next }
    }

    /// The end-of-block steps, after a block's txs. Execution and replay both run exactly this,
    /// in this order (the modules' hooks are no-ops until their packages land):
    ///
    /// 1. `poc_end_block`: the PoC seed, Séances closing and opening.
    /// 2. `econ::end_block`: the base fee for a later block (3.6).
    /// 3. `staking::end_block`: missed leader slots from `view`/`parent_view` (downtime), and the
    ///    proposer's charge for the `skipped` txs (junk, 3.4).
    /// 4. `governance::end_block`: proposals whose voting ended or whose timelock is over.
    /// 5. At an epoch boundary: `econ::issue` (rewards into the pools), `staking::end_epoch`
    ///    (jailing, the next set by stake), `beacon::end_epoch`, then [`State::end_epoch`] (the
    ///    set history and the legacy admin-approved set).
    ///
    /// Then the caller commits the block and, last, runs `upgrade::check_next(height + 1)`, which
    /// stops the node before a height its binary can't execute (3.9).
    pub fn end_block(&mut self, b: &BlockInfo) {
        self.poc_end_block(b.height, b.hash);
        crate::econ::end_block(self, b);
        crate::staking::end_block(self, b);
        crate::governance::end_block(self, b);
        if self.params.is_boundary(b.height) {
            let epoch = self.params.epoch_of(b.height);
            crate::econ::issue(self, epoch);
            crate::staking::end_epoch(self, epoch);
            crate::beacon::end_epoch(self, epoch);
            self.end_epoch(epoch);
        }
    }

    /// Runs after the last block of `epoch`: fixes the set for `epoch + 1`.
    pub fn end_epoch(&mut self, epoch: u64) {
        let next = self.next_set(epoch);
        let current = self.active_sets.get(&epoch).cloned().unwrap_or_default();
        for a in &next {
            let v = self.validators.get_mut(a).expect("set member exists");
            if v.status == ValidatorStatus::Approved {
                v.status = ValidatorStatus::Active;
                v.active_from = Some(epoch + 1);
            }
        }
        for a in &current {
            if !next.contains(a) {
                if let Some(v) = self.validators.get_mut(a) {
                    v.active_until = Some(epoch);
                    if v.status == ValidatorStatus::Removed {
                        v.unbond_epoch = Some(epoch + 2);
                    }
                }
            }
        }
        self.active_sets.insert(epoch + 1, next);
        while self.active_sets.len() as u64 > EPOCH_SETS_KEPT {
            let first = *self.active_sets.keys().next().unwrap();
            self.active_sets.remove(&first);
        }
    }

    pub fn resolve(&self, to: &str) -> Result<String> {
        if crypto::is_address(to) {
            return Ok(to.to_string());
        }
        let n = normalize_name(to)?;
        self.names.get(&n).map(|r| r.owner.clone()).ok_or_else(|| anyhow!("no one has claimed {n}{NAME_SUFFIX} yet"))
    }

    /// First name an address registered — shown everywhere instead of the raw address.
    pub fn primary_name(&self, addr: &str) -> Option<String> {
        self.names.values().filter(|r| r.owner == addr).min_by_key(|r| r.registered_at).map(|r| format!("{}{NAME_SUFFIX}", r.name))
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

    fn validator_mut(&mut self, j: &mut Journal, addr: &str) -> Option<&mut ValidatorRecord> {
        j.validators.touch(&self.validators, addr);
        self.validators.get_mut(addr)
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

    /// The governance-adjustable parameters, journaled (see [`State::exec_set_param`]).
    pub(crate) fn chain_params_mut(&mut self, j: &mut Journal) -> &mut ChainParams {
        if j.chain_params.is_none() {
            j.chain_params = Some(self.chain_params.clone());
        }
        &mut self.chain_params
    }

    /// Undoes everything recorded in `j` (values, and the dirty marks of keys it cleans up).
    pub fn rollback(&mut self, j: Journal) {
        let Journal { accounts, names, validators, hunts, github, github_by_account, familiars, scalars, next_hunt_id, chain_params, poc, staking, governance, econ, beacon, upgrade } = j;
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
    }

    /// Checks that decide whether a tx can be included at all (at `height`). No state changes.
    /// Returns the sender, the signer's key id and record, and what the tx is charged.
    fn admit(&self, tx: &Tx, chain_id: &str, height: u64, full_checks: bool) -> Result<(String, String, KeyRecord, u64)> {
        if tx.chain_id != chain_id {
            bail!("wrong chain_id: expected {chain_id}");
        }
        if full_checks {
            tx.check_format()?;
        }
        tx.check_prefix(self.params.prefix())?;
        check_validity(tx, height, self.chain_params.max_tx_validity_blocks)?;
        let sender = tx.sender()?;
        let signer = tx.signer_key_id()?;
        let keys = self.keys_of(&sender);
        let key = keys.get(&signer).cloned().ok_or_else(|| anyhow!("this key isn't authorized on {}", short(&sender)))?;
        let charge = check_fee(tx)?;
        let acct = self.account(&sender);
        if tx.nonce != acct.nonce {
            bail!("bad nonce: expected {}, got {}", acct.nonce, tx.nonce);
        }
        if acct.balance < charge {
            bail!("insufficient balance for the fee: has {}, needs {}", fmt_fang(acct.balance), fmt_fang(charge));
        }
        if let KeyRole::Session { limit, period_blocks, expires_at } = key.role {
            if height > expires_at {
                bail!("session key expired at block {expires_at}");
            }
            let amount = match &tx.action {
                Action::Transfer { amount, .. } => *amount,
                // An agent registered as a Familiar may claim its work for its sponsor.
                Action::SubmitClaim { .. } if self.familiars.get(&sender).is_some_and(|f| f.contains_key(&signer)) => self.claim_bond(&sender),
                _ => bail!("session keys can only send transfers (and claims, for registered Familiars)"),
            };
            let spent = if height >= key.window_start.saturating_add(period_blocks) { 0 } else { key.spent };
            if spent.saturating_add(amount).saturating_add(charge) > limit {
                bail!("session key limit reached: {} per {period_blocks} blocks", fmt_fang(limit));
            }
        }
        Ok((sender, signer, key, charge))
    }

    /// Validates and applies a tx (signatures are checked before txs reach the state machine:
    /// at admission and in block verification).
    ///
    /// * `Err`: not includable (bad nonce, fee, key…); state untouched.
    /// * `Ok(Applied)`: done.
    /// * `Ok(Failed(reason))`: the fee was charged and the nonce used, but the action failed and
    ///   its effects were rolled back (so failing txs aren't free).
    pub fn apply(&mut self, tx: &Tx, chain_id: &str, proposer: &str, height: u64) -> Result<Outcome> {
        let mut j = Journal::default();
        self.apply_j(&mut j, tx, chain_id, proposer, height, true)
    }

    /// `apply`, recording every change in `j` so the caller can roll back (dry runs, mempool
    /// checks, proposals) without cloning the state.
    pub fn apply_j(&mut self, j: &mut Journal, tx: &Tx, chain_id: &str, proposer: &str, height: u64, full_checks: bool) -> Result<Outcome> {
        let (sender, signer, key, charge) = self.admit(tx, chain_id, height, full_checks)?;
        let base = charge - tx.tip;
        // Fee phase (always sticks once admitted): the charge is debited (not the whole `fee`),
        // its base part burned and the tip paid to the proposer.
        if let KeyRole::Session { period_blocks, .. } = key.role {
            let mut keys = self.keys_of(&sender);
            let rec = keys.get_mut(&signer).unwrap();
            if height >= rec.window_start.saturating_add(period_blocks) {
                rec.spent = 0;
                rec.window_start = height;
            }
            rec.spent = rec.spent.saturating_add(charge);
            self.acct_mut(j, &sender).keys = keys;
        }
        self.j_debit(j, &sender, charge)?;
        self.j_scalars(j);
        self.burned += base;
        self.supply -= base;
        self.j_credit(j, proposer, tx.tip);
        self.acct_mut(j, &sender).nonce += 1;
        // Action phase (rolled back on failure).
        let mut aj = Journal::default();
        match self.apply_action(&mut aj, tx, &sender, &signer, &key, height) {
            Ok(()) => {
                j.absorb(aj);
                Ok(Outcome::Applied)
            }
            Err(e) => {
                self.rollback(aj);
                Ok(Outcome::Failed(e.to_string()))
            }
        }
    }

    fn apply_action(&mut self, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, key: &KeyRecord, height: u64) -> Result<()> {
        let sender = sender.to_string();
        match &tx.action {
            Action::Transfer { to, amount } => {
                let to = self.resolve(to)?;
                if !self.accounts.contains_key(&to) && *amount < MIN_NEW_ACCOUNT {
                    bail!("sending to a new account needs at least {}", fmt_fang(MIN_NEW_ACCOUNT));
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
            Action::RegisterName { name } => {
                let n = normalize_name(name)?;
                if self.names.contains_key(&n) {
                    bail!("{n}{NAME_SUFFIX} is already taken");
                }
                if self.econ.reserved_names.contains_key(&n) {
                    bail!("{n}{NAME_SUFFIX} is reserved");
                }
                self.j_debit(j, &sender, name_price(&n))?;
                let treasury = self.treasury();
                self.j_credit(j, &treasury, name_price(&n));
                j.names.touch(&self.names, &n);
                self.names.insert(n.clone(), NameRecord { name: n, owner: sender.clone(), registered_at: height });
            }
            Action::AddKey { key_id, label, role } => {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can add keys");
                }
                if key_id.len() != 64 || !crypto::is_lower_hex(key_id) {
                    bail!("key_id must be 32 bytes of lowercase hex (BLAKE3 of the public key)");
                }
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
            Action::RegisterValidator { consensus_key, p2p_addr, http_addr, bond, pop } => {
                if key.role != KeyRole::Owner {
                    bail!("only owner keys can register validators");
                }
                if !crypto::is_lower_hex(consensus_key) {
                    bail!("consensus_key must be lowercase hex");
                }
                let pk = hex::decode(consensus_key).map_err(|_| anyhow!("consensus_key must be hex"))?;
                if pk.len() != crypto::PUBLIC_KEY_LEN {
                    bail!("consensus_key must be an ML-DSA-65 public key ({} bytes)", crypto::PUBLIC_KEY_LEN);
                }
                check_host_port(p2p_addr).context("p2p_addr")?;
                check_http_addr(http_addr).context("http_addr")?;
                if *bond < self.chain_params.min_self_bond {
                    bail!("bond must be at least {}", fmt_fang(self.chain_params.min_self_bond));
                }
                let address = crypto::address_with_prefix(self.params.prefix(), &pk);
                if let Some(v) = self.validators.get(&address) {
                    if v.status != ValidatorStatus::Unbonded {
                        bail!("that consensus key is already registered");
                    }
                }
                verify_pop(&tx.chain_id, &sender, consensus_key, p2p_addr, http_addr, pop)?;
                self.j_debit(j, &sender, *bond)?;
                self.validator_mut(j, &address);
                self.validators.insert(
                    address.clone(),
                    ValidatorRecord { address, owner: sender.clone(), consensus_key: consensus_key.clone(), p2p_addr: p2p_addr.clone(), http_addr: http_addr.clone(), bond: *bond, status: ValidatorStatus::Pending, registered_at: height, approved_at: None, approved_by: None, active_from: None, removed_at: None, removed_by: None, active_until: None, unbond_epoch: None, commission_bps: 0 },
                );
            }
            Action::ApproveValidator { validator } => {
                if !self.params.admins.contains(&sender) {
                    bail!("only validator admins can approve validators");
                }
                let v = self.validator_mut(j, validator).ok_or_else(|| anyhow!("no such validator"))?;
                if v.status != ValidatorStatus::Pending {
                    bail!("validator is {:?}, not pending", v.status);
                }
                v.status = ValidatorStatus::Approved;
                v.approved_at = Some(height);
                v.approved_by = Some(sender.clone());
            }
            Action::RemoveValidator { validator } => {
                if !self.params.admins.contains(&sender) {
                    bail!("only validator admins can remove validators");
                }
                let epoch = self.params.epoch_of(height);
                let v = self.validator_mut(j, validator).ok_or_else(|| anyhow!("no such validator"))?;
                if !matches!(v.status, ValidatorStatus::Pending | ValidatorStatus::Approved | ValidatorStatus::Active) {
                    bail!("validator is already {:?}", v.status);
                }
                let was_active = v.status == ValidatorStatus::Active;
                v.status = ValidatorStatus::Removed;
                v.removed_at = Some(height);
                v.removed_by = Some(sender.clone());
                if !was_active {
                    v.unbond_epoch = Some(epoch + 1);
                }
                // Never let the set empty out.
                if !self.validators.values().any(|v| matches!(v.status, ValidatorStatus::Active | ValidatorStatus::Approved)) {
                    bail!("can't remove the last validator");
                }
            }
            Action::Unbond { validator } => {
                let epoch = self.params.epoch_of(height);
                let v = self.validators.get(validator).ok_or_else(|| anyhow!("no such validator"))?;
                if v.owner != sender {
                    bail!("only the account that registered this validator can unbond it");
                }
                if v.status != ValidatorStatus::Removed {
                    bail!("only removed validators can unbond");
                }
                match v.unbond_epoch {
                    Some(e) if epoch >= e => {}
                    Some(e) => bail!("bond unlocks at epoch {e} (now {epoch})"),
                    None => bail!("validator hasn't left the active set yet"),
                }
                let v = self.validator_mut(j, validator).unwrap();
                let bond = v.bond;
                v.bond = 0;
                v.status = ValidatorStatus::Unbonded;
                self.j_credit(j, &sender, bond);
            }
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
                let admin = self.params.admins.contains(&sender);
                if h.creator != sender && !admin {
                    bail!("only the hunt's creator or a validator admin can award it");
                }
                if h.creator == sender && key.role != KeyRole::Owner {
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
                let admin = self.params.admins.contains(&sender);
                if !admin {
                    if h.creator != sender {
                        bail!("only the hunt's creator (after it expires) or a validator admin can cancel it");
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
            Action::Delegate { .. } | Action::Undelegate { .. } | Action::Redelegate { .. } | Action::WithdrawUnbonded {} | Action::UpdateValidator { .. } | Action::Unjail { .. } | Action::ReportEquivocation { .. } => crate::staking::apply(self, j, tx, &sender, signer, key, height)?,
            Action::BeaconCommit { .. } | Action::BeaconReveal { .. } | Action::AttestRevert { .. } => crate::beacon::apply(self, j, tx, &sender, signer, key, height)?,
            Action::Propose { .. } | Action::Vote { .. } | Action::Unlock {} | Action::MultisigCreate { .. } | Action::MultisigPropose { .. } | Action::MultisigApprove { .. } | Action::ClaimDistribution { .. } => crate::governance::apply(self, j, tx, &sender, signer, key, height)?,
            Action::RenewName { .. } | Action::TransferName { .. } | Action::ReleaseName { .. } => crate::econ::apply(self, j, tx, &sender, signer, key, height)?,
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

    /// Sets a governance-adjustable parameter, within its bounds (`ParamChange`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn exec_set_param(&mut self, j: &mut Journal, key: &str, value: u64) -> Result<()> {
        let params = self.params.clone();
        let mut next = self.chain_params.clone();
        next.set(key, value, &params)?;
        *self.chain_params_mut(j) = next;
        Ok(())
    }

    /// What admitting this tx reserves from the sender's balance (charge + amount moved out).
    /// Votes reserve nothing: they lock tokens the voter keeps.
    pub fn tx_debit(&self, tx: &Tx) -> u64 {
        tx.charge().unwrap_or(u64::MAX).saturating_add(match &tx.action {
            Action::Transfer { amount, .. } => *amount,
            Action::RegisterName { name } => normalize_name(name).map(|n| name_price(&n)).unwrap_or(0),
            Action::RegisterValidator { bond, .. } => *bond,
            Action::CreateHunt { amount, .. } | Action::FundHunt { amount, .. } => *amount,
            Action::FundPool { amount } | Action::JoinJury { bond: amount } => *amount,
            Action::Vouch { .. } => crate::poc::VOUCH_BOND,
            Action::SubmitClaim { .. } => crate::poc::CLAIM_BOND,
            Action::Delegate { amount, .. } => *amount,
            Action::Propose { deposit, .. } => *deposit,
            Action::RenewName { name, years } => normalize_name(name).map(|n| self.chain_params.name_renewal_price(n.len(), *years as u64)).unwrap_or(0),
            _ => 0,
        })
    }

    /// State root (hex) as of the last `commit_block`: the sparse Merkle tree over every record.
    pub fn root(&self) -> String {
        hex::encode(self.tree.root())
    }

    pub fn describe(&self, tx: &Tx, height: Option<u64>) -> String {
        let who = |a: &str| self.primary_name(a).unwrap_or_else(|| short(a));
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
                format!("{from} bonded {} to run validator {v} (pending admin approval)", fmt_fang(*bond))
            }
            Action::ApproveValidator { validator } => {
                let when = match self.validators.get(validator).and_then(|v| v.active_from) {
                    Some(e) => format!("active from epoch {e}"),
                    None => match height {
                        Some(h) => format!("joins at epoch {} if there's room", self.params.epoch_of(h) + 1),
                        None => "joins at the next epoch".into(),
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
            Action::Unbond { validator } => {
                let amount = self.validators.get(validator).map(|_| String::new()).unwrap_or_default();
                format!("{from} reclaimed the bond of validator {}{amount}", who(validator))
            }
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
            Action::Delegate { .. } | Action::Undelegate { .. } | Action::Redelegate { .. } | Action::WithdrawUnbonded {} | Action::UpdateValidator { .. } | Action::Unjail { .. } | Action::ReportEquivocation { .. } => crate::staking::describe(self, tx, &from),
            Action::BeaconCommit { .. } | Action::BeaconReveal { .. } | Action::AttestRevert { .. } => crate::beacon::describe(self, tx, &from),
            Action::Propose { .. } | Action::Vote { .. } | Action::Unlock {} | Action::MultisigCreate { .. } | Action::MultisigPropose { .. } | Action::MultisigApprove { .. } | Action::ClaimDistribution { .. } => crate::governance::describe(self, tx, &from),
            Action::RenewName { .. } | Action::TransferName { .. } | Action::ReleaseName { .. } => crate::econ::describe(self, tx, &from),
            _ => self.describe_poc(tx, &from),
        }
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

/// The tx's charge (`min_fee(size) + tip`), which `fee` must cover.
pub fn check_fee(tx: &Tx) -> Result<u64> {
    match tx.charge() {
        Some(charge) if charge <= tx.fee => Ok(charge),
        _ => bail!("fee too low: this tx costs {} drops (minimum {} + tip {}), but its fee allows {}", min_fee(tx.size()).saturating_add(tx.tip), min_fee(tx.size()), tx.tip, tx.fee),
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

/// Blocks kept in memory; older ones are read from `blocks.jsonl` on demand.
pub const RECENT_BLOCKS: usize = 2_048;
/// How far behind the (claimed) network height still counts as synced.
pub const SYNC_SLACK: u64 = 3;
/// Certificates are stored on disk for these heights (plus every epoch's last block) …
pub const CERT_CHECKPOINT: u64 = 100;
/// … and kept in memory for this many recent heights.
pub const RECENT_CERTS: usize = 1_024;
/// Mempool txs not included within this many blocks are dropped (they stay valid until their
/// `valid_until`, so a holder could resubmit them).
pub const MEMPOOL_TTL_BLOCKS: u64 = 600;
/// How long the node's own txs (faucet drips, attestations) stay valid.
pub const OWN_TX_VALIDITY: u64 = 600;
/// Recently failed/dropped tx ids remembered for `/api/tx/{id}`.
pub const RECENT_FAILED: usize = 10_000;
/// Max faucet drips waiting in the mempool (kept below `MAX_PENDING_PER_SENDER`).
pub const FAUCET_MAX_PENDING: usize = 32;

/// Append-only file of length-prefixed records with an in-memory offset index.
struct RecordFile {
    file: File,
    len: u64,
}

impl RecordFile {
    fn open(path: &Path) -> Result<(Self, Vec<(u64, u64, u32)>)> {
        let mut file = OpenOptions::new().create(true).read(true).append(true).open(path)?;
        let mut index = vec![];
        let mut buf = vec![];
        use std::io::{Read, Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut buf)?;
        let mut pos = 0usize;
        while pos + 12 <= buf.len() {
            let key = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap());
            let len = u32::from_le_bytes(buf[pos + 8..pos + 12].try_into().unwrap());
            if pos + 12 + len as usize > buf.len() {
                break; // torn write at the tail
            }
            index.push((key, (pos + 12) as u64, len));
            pos += 12 + len as usize;
        }
        if pos != buf.len() {
            file.set_len(pos as u64)?;
        }
        Ok((Self { file, len: pos as u64 }, index))
    }

    fn append(&mut self, key: u64, bytes: &[u8]) -> Result<(u64, u32)> {
        let mut rec = Vec::with_capacity(12 + bytes.len());
        rec.extend_from_slice(&key.to_le_bytes());
        rec.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        rec.extend_from_slice(bytes);
        self.file.write_all(&rec)?;
        let at = self.len + 12;
        self.len += rec.len() as u64;
        Ok((at, bytes.len() as u32))
    }

    fn read(&self, at: u64, len: u32) -> Option<Vec<u8>> {
        use std::os::unix::fs::FileExt;
        let mut buf = vec![0u8; len as usize];
        self.file.read_exact_at(&mut buf, at).ok()?;
        Some(buf)
    }

    fn sync(&self) -> Result<()> {
        Ok(self.file.sync_data()?)
    }
}

#[derive(Clone, Debug)]
struct PoolEntry {
    added_at: u64,
}

pub struct Chain {
    pub home: PathBuf,
    pub genesis: Genesis,
    /// Validator key (also the faucet key on testnets). `None` on full nodes.
    pub keys: Option<Keys>,
    pub role: Role,
    pub state: State,
    pub genesis_root: String,
    /// Hash of the genesis consensus block (parent of block 1).
    pub genesis_hash: String,
    /// Executed blocks: the last `RECENT_BLOCKS` in memory, the rest on disk.
    recent: std::collections::VecDeque<Block>,
    block_offsets: Vec<u64>,
    block_by_hash: HashMap<u64, u64>,
    height: u64,
    last_hash: String,
    /// Exact consensus block bytes per height (served to joining nodes).
    cblocks: RecordFile,
    cblock_index: Vec<(u64, u32)>,
    /// Finalization certificates: checkpoints on disk, recent ones in memory.
    certs: RecordFile,
    cert_index: BTreeMap<u64, (u64, u32)>,
    recent_certs: BTreeMap<u64, Vec<u8>>,
    pub tx_index: HashMap<String, (u64, usize)>,
    pub history: HashMap<String, Vec<String>>,
    pub mempool: Vec<Tx>,
    pool: HashMap<String, PoolEntry>,
    pending: HashMap<String, (usize, u64)>,
    /// id → (reason, height) for txs that failed at execution or were dropped from the mempool.
    recent_failed: HashMap<String, (String, u64)>,
    failed_order: std::collections::VecDeque<String>,
    /// Highest finalized height proven by a certificate we verified or saw from consensus.
    pub finalized_tip: u64,
    /// Outbound mempool gossip (validators) — every accepted tx is pushed here.
    pub gossip: Option<futures::channel::mpsc::UnboundedSender<Tx>>,
    log: File,
    log_len: u64,
    /// Validator-set history + handoff proofs (`epochs.json`).
    pub epochs: crate::epochs::EpochRegistry,
    /// Set when this node fast-synced: history starts after this height.
    pub base: Option<Base>,
    /// Latest tip each HTTP peer *claims* (display-only; never trusted for anything else).
    claims: HashMap<String, u64>,
    /// Set while a snapshot is being written, so slow disks can't pile up writer threads.
    snapshot_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The certified snapshot a fast-synced node started from (`base.json` + `base.state.zst`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Base {
    pub height: u64,
    pub hash: String,
    pub state_root: String,
    /// Hash of the block before the current epoch's first block, when that's before `height`
    /// (consensus anchors each epoch's engine there). From the peer, not certified.
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
        let genesis = Genesis {
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
        };
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

    /// Opens a chain home and replays `blocks.jsonl`, re-executing every block and checking
    /// state roots and hash links. `validator.key` is optional (full nodes don't have one).
    pub fn open(home: &Path) -> Result<Self> {
        let genesis = Genesis::load(&home.join("genesis.json"))?;
        let key_path = home.join("validator.key");
        let keys = if key_path.exists() {
            let seed = hex::decode(fs::read_to_string(&key_path)?.trim())?;
            Some(Keys::from_seed(seed.try_into().map_err(|_| anyhow!("validator.key must be 32 bytes hex"))?))
        } else {
            None
        };
        let role = if keys.is_some() { Role::Validator } else { Role::FullNode };
        let base: Option<Base> = match fs::read(home.join("base.json")) {
            Ok(b) => Some(serde_json::from_slice(&b)?),
            Err(_) => None,
        };
        let state = match &base {
            Some(b) => {
                let st = crate::snapshot::decode_state(&fs::read(home.join("base.state.zst")).context("base.state.zst missing")?)?;
                if st.root() != b.state_root {
                    bail!("base.state.zst doesn't match base.json's state root");
                }
                st
            }
            None => genesis.initial_state(),
        };
        let genesis_root = genesis.initial_state().root();
        let genesis_hash = hex::encode(crate::consensus::genesis_block(&genesis).digest_bytes());
        let path = home.join("blocks.jsonl");
        let log = OpenOptions::new().create(true).append(true).open(&path)?;
        let (cblocks, cidx) = RecordFile::open(&home.join("cblocks.bin"))?;
        let (certs, certidx) = RecordFile::open(&home.join("certs.bin"))?;
        let genesis_for_epochs = genesis.clone();
        let mut chain = Chain {
            home: home.into(),
            genesis,
            keys,
            role,
            state,
            genesis_root,
            last_hash: genesis_hash.clone(),
            genesis_hash,
            recent: Default::default(),
            block_offsets: vec![],
            block_by_hash: HashMap::new(),
            height: 0,
            cblocks,
            cblock_index: vec![],
            certs,
            cert_index: certidx.into_iter().map(|(h, at, len)| (h, (at, len))).collect(),
            recent_certs: BTreeMap::new(),
            tx_index: HashMap::new(),
            history: HashMap::new(),
            mempool: vec![],
            pool: HashMap::new(),
            pending: HashMap::new(),
            recent_failed: HashMap::new(),
            failed_order: Default::default(),
            finalized_tip: 0,
            gossip: None,
            log,
            log_len: 0,
            epochs: crate::epochs::EpochRegistry::genesis(&genesis_for_epochs),
            base: base.clone(),
            claims: HashMap::new(),
            snapshot_busy: Default::default(),
        };
        if let Some(b) = &base {
            chain.height = b.height;
            chain.last_hash = b.hash.clone();
        }
        let saved_epochs: Option<crate::epochs::EpochRegistry> = fs::read(home.join("epochs.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        if base.is_some() {
            // Fast-synced: the set history came verified with the snapshot.
            chain.epochs = saved_epochs.clone().ok_or_else(|| anyhow!("epochs.json missing for a fast-synced home"))?;
        }
        let mut offset = 0u64;
        let mut reader = BufReader::new(File::open(&path)?);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            if line.trim().is_empty() {
                offset += n as u64;
                continue;
            }
            // A crash mid-append can leave a torn last line. That block was never acknowledged to
            // consensus (blocks.jsonl is synced first), so drop it: it's delivered again.
            if !line.ends_with('\n') && serde_json::from_str::<Block>(&line).is_err() {
                tracing::warn!(offset, bytes = n, "dropping a torn last line in blocks.jsonl (crash during a write)");
                OpenOptions::new().write(true).open(&path)?.set_len(offset)?;
                break;
            }
            let block: Block = serde_json::from_str(&line)?;
            chain.replay(block, offset)?;
            offset += n as u64;
        }
        chain.log_len = offset;
        if let Some(last) = chain.recent.back() {
            if chain.state.root() != last.state_root {
                bail!("state root mismatch at height {} (blocks.jsonl doesn't replay)", last.height);
            }
        }
        // Consensus block bytes: keep exactly one per executed height.
        let first = chain.base_height() + 1;
        for (h, at, len) in cidx {
            if h == first + chain.cblock_index.len() as u64 && h <= chain.height {
                chain.cblock_index.push((at, len));
            }
        }
        if let Some(saved) = saved_epochs {
            let chain_id = chain.genesis.chain_id.clone();
            chain.epochs.merge_sigs(&chain_id, &saved);
        }
        chain.finalized_tip = chain.height();
        Ok(chain)
    }

    /// The validator's own address (fee recipient, faucet account).
    pub fn me(&self) -> Option<String> {
        self.keys.as_ref().map(|k| k.address_with(self.state.params.prefix()))
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    /// Records a peer's claimed tip (display only).
    pub fn note_claim(&mut self, peer: &str, tip: u64) {
        self.claims.insert(peer.to_string(), tip);
    }

    /// The network height peers claim: the median of their latest claims, so one lying peer
    /// can't move it. Display-only (status/health); verified heights come from certificates.
    pub fn network_height_claimed(&self) -> u64 {
        let mut v: Vec<u64> = self.claims.values().copied().collect();
        v.sort_unstable();
        let median = if v.is_empty() { 0 } else { v[(v.len() - 1) / 2] };
        median.max(self.finalized_tip).max(self.height)
    }

    /// Behind the network by more than a few blocks (verified or claimed).
    pub fn syncing(&self) -> bool {
        self.height + SYNC_SLACK < self.network_height_claimed()
    }

    /// Height of the snapshot this node fast-synced from (0 = full history).
    pub fn base_height(&self) -> u64 {
        self.base.as_ref().map(|b| b.height).unwrap_or(0)
    }

    pub fn last_hash(&self) -> &str {
        &self.last_hash
    }

    pub fn last_block(&self) -> Option<&Block> {
        self.recent.back()
    }

    /// Executed block at `height`, from memory or disk.
    pub fn block(&self, height: u64) -> Option<Block> {
        if height == 0 || height > self.height {
            return None;
        }
        let first_recent = self.height + 1 - self.recent.len() as u64;
        if height >= first_recent {
            return self.recent.get((height - first_recent) as usize).cloned();
        }
        if height <= self.base_height() {
            return None;
        }
        let at = *self.block_offsets.get((height - self.base_height()) as usize - 1)?;
        use std::io::{Seek, SeekFrom};
        let mut f = File::open(self.home.join("blocks.jsonl")).ok()?;
        f.seek(SeekFrom::Start(at)).ok()?;
        let mut line = String::new();
        BufReader::new(f).read_line(&mut line).ok()?;
        serde_json::from_str(&line).ok()
    }

    pub fn block_by_hash(&self, hash: &str) -> Option<Block> {
        let key = u64::from_str_radix(hash.get(..16)?, 16).ok()?;
        let b = self.block(*self.block_by_hash.get(&key)?)?;
        (b.hash == hash).then_some(b)
    }

    /// Newest-first blocks below `before`.
    pub fn blocks_before(&self, before: u64, limit: usize) -> Vec<Block> {
        let top = before.saturating_sub(1).min(self.height);
        (self.base_height() + 1..=top).rev().take(limit).filter_map(|h| self.block(h)).collect()
    }

    /// State root after executing `height` (0 = genesis).
    pub fn root_at(&self, height: u64) -> Option<String> {
        if height == 0 {
            return Some(self.genesis_root.clone());
        }
        if let Some(b) = self.base.as_ref().filter(|b| b.height == height) {
            return Some(b.state_root.clone());
        }
        self.block(height).map(|b| b.state_root)
    }

    /// Hash of the executed block at `height`, including the fast-sync base and the anchor of
    /// the epoch we fast-synced into.
    pub fn hash_at(&self, height: u64) -> Option<String> {
        if height == 0 {
            return Some(self.genesis_hash.clone());
        }
        if let Some(b) = &self.base {
            if b.height == height {
                return Some(b.hash.clone());
            }
            if let Some((h, hash)) = &b.epoch_start {
                if *h == height {
                    return Some(hash.clone());
                }
            }
        }
        self.block(height).map(|b| b.hash)
    }

    /// Exact consensus block bytes at `height`.
    pub fn cblock(&self, height: u64) -> Option<Vec<u8>> {
        let (at, len) = *self.cblock_index.get(height.checked_sub(self.base_height() + 1)? as usize)?;
        self.cblocks.read(at, len)
    }

    /// Encoded finalization certificate for `height`, if stored.
    pub fn cert(&self, height: u64) -> Option<Vec<u8>> {
        if let Some(c) = self.recent_certs.get(&height) {
            return Some(c.clone());
        }
        let (at, len) = *self.cert_index.get(&height)?;
        self.certs.read(at, len)
    }

    pub fn cert_len(&self, height: u64) -> usize {
        self.recent_certs.get(&height).map(|c| c.len()).or_else(|| self.cert_index.get(&height).map(|(_, l)| *l as usize)).unwrap_or(0)
    }

    /// Stores a certificate: always in the recent window, on disk at checkpoints/epoch ends.
    pub fn store_cert(&mut self, height: u64, cert: Vec<u8>) -> Result<()> {
        if (height % CERT_CHECKPOINT == 0 || self.state.params.is_boundary(height)) && !self.cert_index.contains_key(&height) {
            let (at, len) = self.certs.append(height, &cert)?;
            self.certs.sync()?;
            self.cert_index.insert(height, (at, len));
        }
        self.recent_certs.insert(height, cert);
        while self.recent_certs.len() > RECENT_CERTS {
            let first = *self.recent_certs.keys().next().unwrap();
            self.recent_certs.remove(&first);
        }
        self.finalized_tip = self.finalized_tip.max(height);
        Ok(())
    }

    fn index(&mut self, block: &Block) {
        for (i, tx) in block.txs.iter().enumerate() {
            let id = tx.id();
            self.tx_index.insert(id.clone(), (block.height, i));
            let sender = tx.sender().ok();
            if let Some(s) = &sender {
                self.history.entry(s.clone()).or_default().push(id.clone());
            }
            let other = match &tx.action {
                Action::Transfer { to, .. } | Action::AwardHunt { to, .. } | Action::GrantSoul { to, .. } => Some(to),
                Action::Vouch { account } | Action::FlagSybil { account, .. } | Action::Clawback { account, .. } => Some(account),
                _ => None,
            };
            if let Some(to) = other.and_then(|t| self.state.resolve(t).ok()) {
                if sender.as_ref() != Some(&to) {
                    self.history.entry(to).or_default().push(id.clone());
                }
            }
        }
    }

    fn push_block(&mut self, block: Block, offset: u64) {
        debug_assert_eq!(self.block_offsets.len() as u64 + self.base_height() + 1, block.height);
        self.block_offsets.push(offset);
        if let Ok(key) = u64::from_str_radix(&block.hash[..16.min(block.hash.len())], 16) {
            self.block_by_hash.insert(key, block.height);
        }
        self.height = block.height;
        self.last_hash = block.hash.clone();
        self.recent.push_back(block);
        while self.recent.len() > RECENT_BLOCKS {
            self.recent.pop_front();
        }
    }

    fn replay(&mut self, block: Block, offset: u64) -> Result<()> {
        if block.height != self.height() + 1 || block.prev_hash != self.last_hash() {
            bail!("blocks.jsonl is not contiguous at height {}", block.height);
        }
        for (i, tx) in block.txs.iter().enumerate() {
            let outcome = self.state.apply(tx, &self.genesis.chain_id, &block.proposer, block.height).with_context(|| format!("replaying block {}", block.height))?;
            let expected = match block.failed.get(&i) {
                Some(r) => Outcome::Failed(r.clone()),
                None => Outcome::Applied,
            };
            if outcome != expected {
                bail!("replaying block {}: tx {i} gave {outcome:?}, recorded {expected:?}", block.height);
            }
        }
        self.state.end_block(&BlockInfo { height: block.height, hash: &block.hash, proposer: &block.proposer, view: block.view, parent_view: block.parent_view, skipped: &block.skipped });
        if self.state.params.is_boundary(block.height) {
            self.record_epoch(block.height);
        }
        let root = hex::encode(self.state.commit_block()?.root);
        #[cfg(test)]
        self.state.debug_check(block.height);
        if root != block.state_root {
            bail!("state root mismatch at height {}", block.height);
        }
        let height = block.height;
        self.index(&block);
        self.push_block(block, offset);
        crate::upgrade::check_next(&self.state, height + 1)
    }

    fn note_failed(&mut self, id: String, reason: String, height: u64) {
        if self.recent_failed.insert(id.clone(), (reason, height)).is_none() {
            self.failed_order.push_back(id);
        }
        while self.failed_order.len() > RECENT_FAILED {
            if let Some(old) = self.failed_order.pop_front() {
                self.recent_failed.remove(&old);
            }
        }
    }

    /// Why a tx failed or was dropped, if we remember.
    pub fn failure(&self, id: &str) -> Option<&(String, u64)> {
        self.recent_failed.get(id)
    }

    /// Executes a finalized block: applies its txs in order (invalid ones are skipped; ones whose
    /// action fails are charged their fee), appends it durably to `blocks.jsonl`, then prunes the
    /// mempool.
    pub fn execute(&mut self, f: Finalized) -> Result<()> {
        if f.height != self.height() + 1 {
            bail!("expected block {}, got {}", self.height() + 1, f.height);
        }
        if f.parent != self.last_hash() {
            bail!("block {} does not extend our chain", f.height);
        }
        let mut txs = vec![];
        let mut failed = BTreeMap::new();
        let mut skipped = vec![];
        for tx in f.txs {
            match self.state.apply(&tx, &self.genesis.chain_id, &f.proposer, f.height) {
                Ok(Outcome::Applied) => txs.push(tx),
                Ok(Outcome::Failed(r)) => {
                    failed.insert(txs.len(), r.clone());
                    self.note_failed(tx.id(), r, f.height);
                    txs.push(tx);
                }
                Err(e) => {
                    skipped.push(tx.size());
                    self.note_failed(tx.id(), format!("not included: {e}"), f.height);
                }
            }
        }
        self.state.end_block(&BlockInfo { height: f.height, hash: &f.hash, proposer: &f.proposer, view: f.view, parent_view: f.parent_view, skipped: &skipped });
        if self.state.params.is_boundary(f.height) && self.record_epoch(f.height) {
            self.sign_handoffs();
        }
        let state_root = hex::encode(self.state.commit_block()?.root);
        #[cfg(test)]
        self.state.debug_check(f.height);
        let block = Block { height: f.height, prev_hash: f.parent, timestamp_ms: f.timestamp_ms, view: f.view, parent_view: f.parent_view, version: f.version, proposer: f.proposer, state_root, txs, hash: f.hash, signature: String::new(), failed, skipped };
        let (at, len) = self.cblocks.append(f.height, &f.cblock)?;
        self.cblock_index.push((at, len));
        let line = serde_json::to_string(&block)? + "\n";
        self.log.write_all(line.as_bytes())?;
        self.cblocks.sync()?;
        self.log.sync_data()?;
        let offset = self.log_len;
        self.log_len += line.len() as u64;
        self.index(&block);
        self.push_block(block, offset);
        if let Some(cert) = f.certificate {
            self.store_cert(f.height, cert)?;
        }
        self.finalized_tip = self.finalized_tip.max(f.height);
        if crate::snapshot::is_checkpoint(f.height) {
            self.take_snapshot();
        }
        self.revalidate_mempool();
        // The last end-of-block step: the block is executed and stored, but if this binary can't
        // run the next height, stop here (the error leaves the block unacknowledged, so consensus
        // stops delivering) instead of diverging. P-UPGRADE turns this into a clean exit.
        crate::upgrade::check_next(&self.state, f.height + 1)
    }

    /// After executing an epoch's last block: remember the next epoch's set. Returns whether it
    /// changed (then the outgoing validators sign a handoff).
    fn record_epoch(&mut self, boundary: u64) -> bool {
        let next = self.state.params.epoch_of(boundary) + 1;
        let keys: Vec<String> = self.state.active_set(next).map(|s| s.iter().map(|v| v.consensus_key.clone()).collect()).unwrap_or_default();
        let changed = self.epochs.record(next, keys);
        if changed {
            self.save_epochs();
        }
        changed
    }

    /// Signs handoffs for set changes this validator presided over (also retroactively).
    pub fn sign_handoffs(&mut self) -> usize {
        let Some(seed) = self.keys.as_ref().map(|k| k.seed()) else { return 0 };
        let chain_id = self.genesis.chain_id.clone();
        let n = self.epochs.sign_missing(&chain_id, seed).len();
        if n > 0 {
            self.save_epochs();
        }
        n
    }

    pub fn merge_epochs(&mut self, other: &crate::epochs::EpochRegistry) -> usize {
        let chain_id = self.genesis.chain_id.clone();
        let n = self.epochs.merge_sigs(&chain_id, other);
        if n > 0 {
            self.save_epochs();
        }
        n
    }

    fn save_epochs(&self) {
        if let Ok(json) = serde_json::to_vec(&self.epochs) {
            let tmp = self.home.join("epochs.json.tmp");
            if fs::write(&tmp, json).is_ok() {
                let _ = fs::rename(tmp, self.home.join("epochs.json"));
            }
        }
    }

    /// Writes a snapshot of the current state in the background.
    fn take_snapshot(&self) {
        use std::sync::atomic::Ordering;
        if self.snapshot_busy.swap(true, Ordering::AcqRel) {
            tracing::warn!(height = self.height, "skipping a snapshot: the previous one is still being written");
            return;
        }
        let (home, height, root, state, busy) = (self.home.clone(), self.height, self.state.root(), self.state.clone(), self.snapshot_busy.clone());
        std::thread::spawn(move || {
            if let Err(e) = crate::snapshot::write(&home, height, &root, &state) {
                tracing::warn!(height, error = %e, "snapshot failed");
            }
            busy.store(false, Ordering::Release);
        });
    }

    /// Installs a verified snapshot into an empty home, then executes the manifest's blocks.
    /// Installs a restored snapshot: `chunks` (the packed state, in order) become `base.state.zst`.
    pub fn install_snapshot(&mut self, v: &crate::snapshot::Verified, state: State, chunks: &[Vec<u8>], epoch_start: Option<(u64, String)>) -> Result<()> {
        if self.height != 0 || self.base.is_some() {
            bail!("fast sync needs an empty home");
        }
        let base_hash = hex::encode(v.blocks[0].parent_bytes());
        let mut packed = File::create(self.home.join("base.state.zst"))?;
        for c in chunks {
            packed.write_all(c)?;
        }
        packed.sync_all()?;
        let base = Base { height: v.height, hash: base_hash.clone(), state_root: v.state_root.clone(), epoch_start };
        self.epochs = v.epochs.clone();
        self.save_epochs();
        fs::write(self.home.join("base.json"), serde_json::to_vec_pretty(&base)?)?;
        self.state = state;
        self.height = v.height;
        self.last_hash = base_hash;
        self.base = Some(base);
        self.finalized_tip = v.height;
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
        for tx in &self.mempool {
            let e = self.pending.entry(tx.from.clone()).or_default();
            e.0 += 1;
            e.1 = e.1.saturating_add(self.state.tx_debit(tx));
        }
    }

    /// Drops mempool txs that were included, expired, or can no longer apply. Uses a journal
    /// over the live state (rolled back afterwards) instead of cloning it.
    fn revalidate_mempool(&mut self) {
        let next = self.height() + 1;
        let proposer = self.fee_sink();
        let chain_id = self.genesis.chain_id.clone();
        let mut j = Journal::default();
        let mut keep = Vec::with_capacity(self.mempool.len());
        let mut dropped = vec![];
        for tx in std::mem::take(&mut self.mempool) {
            let id = tx.id();
            if self.tx_index.contains_key(&id) {
                self.pool.remove(&id);
                continue;
            }
            if tx.valid_until < next {
                dropped.push((id, format!("expired at block {}", tx.valid_until)));
                continue;
            }
            let added = self.pool.get(&id).map(|e| e.added_at).unwrap_or(next);
            if added + MEMPOOL_TTL_BLOCKS < next {
                // Not expired: anyone holding it may resubmit it until `valid_until`.
                dropped.push((id, format!("dropped from the mempool: not included within {MEMPOOL_TTL_BLOCKS} blocks (it stays valid until block {} if resubmitted)", tx.valid_until)));
                continue;
            }
            match self.state.apply_j(&mut j, &tx, &chain_id, &proposer, next, false) {
                Ok(Outcome::Applied) => keep.push(tx),
                Ok(Outcome::Failed(r)) => dropped.push((id, format!("dropped from the mempool: {r}"))),
                Err(e) => dropped.push((id, format!("dropped from the mempool: {e}"))),
            }
        }
        self.state.rollback(j);
        self.mempool = keep;
        for (id, r) in dropped {
            self.pool.remove(&id);
            self.note_failed(id, r, next);
        }
        self.rebuild_pending();
    }

    /// Checks a tx against current state plus what the sender already has queued, then queues
    /// it. The caller must have verified the signature (outside the chain lock). Cost doesn't
    /// grow with the mempool: pending nonces and balance reservations are cached per sender.
    /// Returns the id and whether it was new (already-known txs are accepted idempotently).
    pub fn submit_tx(&mut self, tx: Tx) -> Result<(String, bool)> {
        tx.check_format()?;
        let id = tx.id();
        if self.tx_index.contains_key(&id) || self.pool.contains_key(&id) {
            return Ok((id, false));
        }
        let sender = tx.sender()?;
        let (count, reserved) = self.pending.get(&sender).copied().unwrap_or_default();
        if count >= MAX_PENDING_PER_SENDER {
            bail!("too many pending transactions from {}; wait for some to finalize", short(&sender));
        }
        let expected = self.state.account(&sender).nonce + count as u64;
        if tx.nonce != expected {
            bail!("bad nonce: expected {expected}, got {}", tx.nonce);
        }
        let next = self.height() + 1;
        let proposer = self.fee_sink();
        let chain_id = self.genesis.chain_id.clone();
        // Dry run on the live state as if the sender's queued txs had already gone through.
        let mut j = Journal::default();
        let result = (|| -> Result<Outcome> {
            if count > 0 {
                self.state.j_debit(&mut j, &sender, reserved).map_err(|_| anyhow!("insufficient balance once your pending transactions go through"))?;
                self.state.acct_mut(&mut j, &sender).nonce = tx.nonce;
            }
            self.state.apply_j(&mut j, &tx, &chain_id, &proposer, next, false)
        })();
        self.state.rollback(j);
        match result? {
            Outcome::Applied => {}
            Outcome::Failed(r) => bail!(r),
        }
        if self.mempool.len() >= MAX_MEMPOOL {
            // Full: evict the lowest tip per byte if this one pays more, else refuse. Only the
            // last queued tx of another sender can go, so no sender is left with a nonce gap.
            let mut last = HashMap::new();
            for (i, t) in self.mempool.iter().enumerate() {
                last.insert(t.from.as_str(), i);
            }
            last.remove(sender.as_str());
            // Lowest rate; among equals, the newest arrival.
            let lowest = last.into_values().min_by(|&a, &b| TipRate::of(&self.mempool[a]).cmp(&TipRate::of(&self.mempool[b])).then(b.cmp(&a)));
            let idx = match lowest {
                Some(i) if TipRate::of(&tx) > TipRate::of(&self.mempool[i]) => i,
                Some(i) => {
                    let low = TipRate::of(&self.mempool[i]);
                    let need = (low.tip as u128 * tx.size() as u128 / low.size.max(1) as u128 + 1).min(u64::MAX as u128);
                    bail!("mempool is full; resubmit with a tip of at least {need} drops (and the fee to cover it), or try again shortly");
                }
                None => bail!("mempool is full; try again shortly"),
            };
            let evicted = self.mempool.remove(idx);
            let eid = evicted.id();
            self.pool.remove(&eid);
            self.note_failed(eid, "evicted from a full mempool by a transaction with a higher tip".into(), next);
            self.rebuild_pending();
        }
        if let Some(g) = &self.gossip {
            let _ = g.unbounded_send(tx.clone());
        }
        let debit = self.state.tx_debit(&tx);
        let e = self.pending.entry(sender.clone()).or_default();
        e.0 += 1;
        e.1 = e.1.saturating_add(debit);
        self.pool.insert(id.clone(), PoolEntry { added_at: next });
        self.mempool.push(tx);
        Ok((id, true))
    }

    pub fn submit(&mut self, tx: Tx) -> Result<String> {
        self.submit_tx(tx).map(|(id, _)| id)
    }

    /// Adds a tx that another node already accepted (full nodes track what they forwarded).
    pub fn track_pending(&mut self, tx: Tx) -> Result<()> {
        tx.check_format()?;
        let id = tx.id();
        if !self.tx_index.contains_key(&id) && !self.pool.contains_key(&id) {
            let sender = tx.sender()?;
            if self.pending.get(&sender).is_some_and(|p| p.0 >= MAX_PENDING_PER_SENDER) || self.mempool.len() >= MAX_MEMPOOL {
                return Ok(());
            }
            let debit = self.state.tx_debit(&tx);
            let e = self.pending.entry(sender.clone()).or_default();
            e.0 += 1;
            e.1 = e.1.saturating_add(debit);
            self.pool.insert(id, PoolEntry { added_at: self.height() + 1 });
            self.mempool.push(tx);
        }
        Ok(())
    }

    /// Removes a tx we queued locally (e.g. a full node whose forward failed).
    pub fn drop_pending(&mut self, id: &str) {
        if self.pool.remove(id).is_some() {
            self.mempool.retain(|t| t.id() != id);
            self.rebuild_pending();
        }
    }

    pub fn pending_count(&self, addr: &str) -> usize {
        self.pending.get(addr).map(|p| p.0).unwrap_or(0)
    }

    pub fn in_mempool(&self, id: &str) -> bool {
        self.pool.contains_key(id)
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
        let keys = self.keys.as_ref().ok_or_else(|| anyhow!("this node has no validator key"))?;
        let from = keys.address_with(self.state.params.prefix());
        let mut tx = Tx {
            chain_id: self.genesis.chain_id.clone(),
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
        tx.fee = min_fee(tx.size());
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
        let height = self.height() + 1;
        let f = Finalized { height, hash: hex::encode(crypto::hash(format!("test-{}-{}", self.height(), self.last_hash()).as_bytes())), parent: self.last_hash().to_string(), timestamp_ms: now_ms(), view: height, parent_view: height - 1, version: crate::consensus::PROTOCOL_VERSION, proposer: self.fee_sink(), txs, cblock: vec![], certificate: None };
        self.execute(f)
    }
}

pub const MAX_MEMPOOL: usize = 5_000;
pub const MAX_PENDING_PER_SENDER: usize = 64;

/// Mempool txs in the order a proposal takes them: highest tip per byte first (earlier arrival
/// first among equals), each sender's txs still in nonce (arrival) order.
pub fn proposal_order(mempool: &[Tx]) -> Vec<&Tx> {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, VecDeque};
    let mut queues: Vec<VecDeque<usize>> = vec![];
    let mut by_sender: HashMap<&str, usize> = HashMap::new();
    for (i, t) in mempool.iter().enumerate() {
        let q = *by_sender.entry(t.from.as_str()).or_insert_with(|| {
            queues.push(VecDeque::new());
            queues.len() - 1
        });
        queues[q].push_back(i);
    }
    // Max-heap of each sender's next tx: (rate, earliest arrival, queue).
    let mut heap: BinaryHeap<(TipRate, Reverse<usize>, usize)> = queues.iter().enumerate().map(|(q, txs)| (TipRate::of(&mempool[txs[0]]), Reverse(txs[0]), q)).collect();
    let mut out = Vec::with_capacity(mempool.len());
    while let Some((_, Reverse(i), q)) = heap.pop() {
        out.push(&mempool[i]);
        queues[q].pop_front();
        if let Some(&next) = queues[q].front() {
            heap.push((TipRate::of(&mempool[next]), Reverse(next), q));
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
            let mut tx = Tx { chain_id: "test-1".into(), from: alice.address(), nonce: 0, fee: 0, tip: 0, valid_until: c.height() + 100, action: Action::RegisterName { name: "alice".into() }, memo: String::new(), public_key: String::new(), signature: "00".repeat(crypto::SIGNATURE_LEN) };
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

    /// A crash mid-append leaves a torn last line; the node drops it and starts, instead of
    /// needing manual repair. A corrupt line in the middle still refuses to start.
    #[test]
    fn torn_last_line_in_blocks_jsonl_is_dropped() {
        let home = std::env::temp_dir().join(format!("blockula-torn-{}-{}", now_ms(), std::process::id()));
        Chain::summon(&home, &SummonOpts::new("torn", 1)).unwrap();
        let alice = Keys::from_seed([1u8; 32]);
        let root = {
            let mut c = Chain::open(&home).unwrap();
            c.faucet(&alice.address(), 50 * DROPS_PER_FANG).unwrap();
            c.produce_block().unwrap();
            c.produce_block_with(vec![]).unwrap();
            c.state.root()
        };
        let log = home.join("blocks.jsonl");
        let good = fs::read(&log).unwrap();
        let mut f = OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(br#"{"height":3,"prev_hash":"ab"#).unwrap();
        drop(f);
        let c = Chain::open(&home).unwrap();
        assert_eq!((c.height(), c.state.root()), (2, root));
        assert_eq!(fs::read(&log).unwrap(), good, "the torn tail was truncated");
        drop(c);
        let mut bad = b"{not json}\n".to_vec();
        bad.extend_from_slice(&good);
        fs::write(&log, bad).unwrap();
        assert!(Chain::open(&home).is_err());
        fs::remove_dir_all(home).ok();
    }

    fn signed(c: &Chain, from: &str, keys: &Keys, nonce: u64, action: Action) -> Tx {
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), from: from.into(), nonce, fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = min_fee(tx.size());
        tx.sign(keys)
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
        c.submit(signed(&c, &acct, &owner, 0, Action::AddKey { key_id: key_id(&laptop.public_key()), label: "laptop".into(), role: KeyRole::Owner })).unwrap();
        c.submit(signed(&c, &acct, &owner, 1, Action::AddKey { key_id: key_id(&agent.public_key()), label: "claude".into(), role: session })).unwrap();
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
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), from: from.into(), nonce, fee: 0, tip: 0, valid_until: c.height() + 1_000, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
        tx.fee = fee.unwrap_or_else(|| min_fee(tx.size()));
        tx.sign(keys)
    }

    /// Unsigned (zero signature) tx: the state machine doesn't verify signatures (admission and
    /// block verification do), so benchmarks can skip ML-DSA signing. Valid through block 1,000.
    fn unsigned(chain_id: &str, from: &Keys, nonce: u64, action: Action) -> Tx {
        let mut tx = Tx { chain_id: chain_id.into(), from: from.address(), nonce, fee: 0, tip: 0, valid_until: 1_000, action, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
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
        let long_label = Action::AddKey { key_id: "ab".repeat(32), label: "x".repeat(65), role: KeyRole::Owner };
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
        assert!(crate::consensus::check_tx_stateless(&m, "audit", "boo").is_err(), "uppercase hex is rejected");
        assert!(c.submit(m).is_err());
        c.submit(tx).unwrap();
        c.produce_block().unwrap();
        assert!(c.tx_index.contains_key(&orig_id));
    }

    #[test]
    fn uppercase_key_id_rejected() {
        let (_h, mut c) = fresh("kid");
        let (orig, dev) = (Keys::from_seed([1u8; 32]), Keys::from_seed([2u8; 32]));
        let acct = orig.address();
        c.faucet(&acct, 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let upper = Action::AddKey { key_id: key_id(&dev.public_key()).to_uppercase(), label: "phone".into(), role: KeyRole::Owner };
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
        let (h, i) = c.tx_index[&t2.id()];
        assert!(c.block(h).unwrap().failed.contains_key(&i));
        // New accounts need a minimum transfer.
        let dust = signed_fee(&c, &a.address(), &a, 2, Action::Transfer { to: Keys::from_seed([77; 32]).address(), amount: 1 }, None);
        assert!(c.submit(dust).unwrap_err().to_string().contains("new account"));
        // Replays identically.
        let root = c.state.root();
        drop(c);
        assert_eq!(Chain::open(&home).unwrap().state.root(), root);
    }

    #[test]
    fn session_key_bounds_and_names() {
        let (_h, mut c) = fresh("sess");
        let (o, s) = (Keys::from_seed([1u8; 32]), Keys::from_seed([2u8; 32]));
        c.faucet(&o.address(), 50 * DROPS_PER_FANG).unwrap();
        c.produce_block().unwrap();
        let zero = Action::AddKey { key_id: key_id(&s.public_key()), label: "a".into(), role: KeyRole::Session { limit: 1, period_blocks: 0, expires_at: 10 } };
        assert!(c.submit(signed_fee(&c, &o.address(), &o, 0, zero, None)).is_err());
        let forever = Action::AddKey { key_id: key_id(&s.public_key()), label: "a".into(), role: KeyRole::Session { limit: 1, period_blocks: 1, expires_at: u64::MAX } };
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
        assert!(c.syncing(), "behind an honest peer's claim");
        assert_eq!(c.finalized_tip, 0, "claims never move the verified tip");
        // One liar among three can't move the median.
        c.note_claim("a", 2);
        c.note_claim("b", 1);
        c.note_claim("liar", u64::MAX);
        assert_eq!(c.network_height_claimed(), 2);
        assert!(!c.syncing());
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
                c.submit(unsigned("audit", k, n, Action::Transfer { to: me.clone(), amount: 1 })).unwrap();
            }
        }
        let fill = t.elapsed();
        assert_eq!(c.mempool.len(), MAX_MEMPOOL);
        let x = Keys::from_seed([77u8; 32]);
        c.state.accounts.insert(x.address(), Account { balance: 100 * DROPS_PER_FANG, ..Default::default() });
        c.state.supply += 100 * DROPS_PER_FANG;
        let mut tx = unsigned("audit", &x, 0, Action::Transfer { to: me.clone(), amount: 1 });
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

    /// 200k accounts and a full 512-tx block: no per-tx state clone.
    #[test]
    fn big_state_block_execution() {
        let (_h, mut c) = fresh("clone");
        for i in 0..200_000u64 {
            c.state.accounts.insert(crypto::address_from_bytes(&crypto::hash(&i.to_le_bytes())), Account { balance: DROPS_PER_FANG, ..Default::default() });
        }
        let senders: Vec<Keys> = (0..8u32).map(|i| Keys::from_seed(crypto::hash(&(i + 1000).to_le_bytes()))).collect();
        for k in &senders {
            c.state.accounts.insert(k.address(), Account { balance: 1_000 * DROPS_PER_FANG, ..Default::default() });
        }
        c.state.supply += 200_000 * DROPS_PER_FANG + 8 * 1_000 * DROPS_PER_FANG;
        let txs: Vec<Tx> = (0..512u64).map(|i| unsigned("audit", &senders[(i % 8) as usize], i / 8, Action::Transfer { to: crypto::address_from_bytes(&crypto::hash(&i.to_le_bytes())), amount: 1 })).collect();
        let t = Instant::now();
        c.produce_block_with(txs).unwrap();
        let took = t.elapsed();
        println!("512-tx block over 200k accounts executed in {took:?}");
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
            let mut tx = Tx { chain_id: "hunts".into(), nonce: self.c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: self.c.height() + 100, action, memo: String::new(), public_key: hex::encode(who.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
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
        // Only the creator or an admin awards.
        assert!(t.tx(&bob, Action::AwardHunt { hunt_id: id, to: bob.address(), pr: 9, familiar_key: None }).unwrap_err().to_string().contains("creator or a validator admin"));
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
        t.tx(&alice, Action::AddKey { key_id: kid.clone(), label: "claude".into(), role: KeyRole::Session { limit: DROPS_PER_FANG, period_blocks: 10, expires_at: h + 100 } }).unwrap();
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
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), from: from.into(), nonce, fee: 0, tip, valid_until, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
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
        let base = min_fee(t.size());
        c.submit(t.clone()).unwrap();
        c.produce_block().unwrap();
        assert_eq!(bal(&c, &a.address()), a0 - DROPS_PER_FANG - base - 7_000);
        assert_eq!(bal(&c, &me), me0 + 7_000, "the tip goes to the proposer");
        assert_eq!((c.state.supply, c.state.burned), (supply0 - base, burned0 + base), "the base fee is burned");
        // A fee that doesn't cover min_fee + tip isn't admittable: not in the mempool, not in a block.
        let mut short = tx(&c, &a, &a.address(), 1, 5_000, c.height() + 10, Action::Transfer { to: friend.clone(), amount: DROPS_PER_FANG });
        short.fee -= 1;
        let short = short.sign(&a);
        assert!(c.submit(short.clone()).unwrap_err().to_string().contains("fee too low"));
        assert!(crate::consensus::check_tx_stateless(&short, "v1", "boo").unwrap_err().to_string().contains("fee too low"));
        c.produce_block_with(vec![short.clone()]).unwrap();
        assert!(c.failure(&short.id()).unwrap().0.contains("fee too low"));
        assert_eq!(c.state.account(&a.address()).nonce, 1);
        // A tip that overflows the charge is refused too.
        let mut huge = tx(&c, &a, &a.address(), 1, 0, c.height() + 10, Action::Transfer { to: friend, amount: 1 });
        (huge.tip, huge.fee) = (u64::MAX, u64::MAX);
        assert!(c.submit(huge.sign(&a)).unwrap_err().to_string().contains("fee too low"));
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
        c.submit(tx(&c, &owner, &acct, 0, 0, c.height() + 10, Action::AddKey { key_id: key_id(&agent.public_key()), label: "claude".into(), role })).unwrap();
        c.produce_block().unwrap();
        // An amount that fits with the base fee alone, but not with a tip on top.
        let charge0 = min_fee(tx(&c, &agent, &acct, 1, 0, c.height() + 10, Action::Transfer { to: friend.clone(), amount: 1 }).size());
        let amount = limit - charge0 - 10;
        let tipped = tx(&c, &agent, &acct, 1, 100, c.height() + 10, Action::Transfer { to: friend.clone(), amount });
        assert!(c.submit(tipped).unwrap_err().to_string().contains("limit reached"));
        c.submit(tx(&c, &agent, &acct, 1, 0, c.height() + 10, Action::Transfer { to: friend, amount })).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.keys_of(&acct)[&key_id(&agent.public_key())].spent, amount + charge0, "spent = amount + charged fee");
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
        assert!(!c.tx_index.contains_key(&t.id()));
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
        assert!(crate::consensus::check_tx_stateless(&pay_main, "v1", "tboo").is_err(), "verify, gossip and HTTP refuse it too");
        c.produce_block_with(vec![pay_main.clone()]).unwrap();
        assert!(!c.tx_index.contains_key(&pay_main.id()), "and it can't execute");
        // Native addresses and names work.
        c.submit(tx(&c, &a, &test, 0, 0, vu, Action::RegisterName { name: "tessa".into() })).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.resolve("tessa.boo").unwrap(), test);
        assert_eq!(bal(&c, &c.state.treasury()), 100_000_000 * DROPS_PER_FANG + name_price("tessa"));
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
        let order: Vec<String> = proposal_order(&c.mempool).iter().map(|t| t.id()).collect();
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
    /// community bucket, reserved names, a Soul ledger, parameter overrides and Ghost Mode on.
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
            features: vec!["ghost".into()],
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
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), nonce: c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: c.height() + 100, action, memo: String::new(), public_key: hex::encode(k.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
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
        // The operator owns its validator; the self-bond left its balance for the bond (escrow).
        let rec = &s.validators[&v];
        assert_eq!((rec.owner.as_str(), rec.bond, rec.commission_bps, rec.status), (owner.as_str(), 10_000 * FANG, 500, ValidatorStatus::Active));
        assert_eq!(s.account(&owner).balance, 10_000 * FANG);
        // Vesting allocations are locks on the account, released linearly after the cliff.
        let locks = s.account(&founder).locks;
        assert_eq!(locks, vec![Lock { id: 0, amount: LockAmount::Vesting { total: 1_000_000 * FANG, start: 0, cliff: 100, end: 1_000 }, reason: "founder".into() }]);
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
        refused(&|v| v["params"]["max_validators"] = 0.into(), "`max_validators` must be 1–1000");
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
        // Reserved names, the Soul ledger and features.
        refused(&|v| v["reserved_names"][0] = "blockula.boo".into(), "bare lowercase name");
        refused(&|v| v["reserved_names"][1] = "blockula".into(), "listed twice");
        refused(&|v| v["soul_ledger"][0]["soul"] = 0.into(), "milli-Soul");
        refused(&|v| v["soul_ledger"][0]["account"] = community("boo").into(), "system account");
        refused(&|v| v["features"][0] = "warp".into(), "unknown feature warp");
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
        let v = Keys::from_seed(VALIDATOR).address();
        // Modules that haven't landed refuse their txs; a block carrying one charges its fee.
        let delegate = signed(&c, &owner, Action::Delegate { validator: v.clone(), amount: FANG });
        assert_eq!(c.submit(delegate.clone()).unwrap_err().to_string(), "not active yet");
        for action in [Action::Propose { kind: ProposalKind::Text {}, deposit: FANG, title: "t".into(), url: String::new() }, Action::BeaconCommit { epoch: 1, commitment: "ab".repeat(32) }, Action::RenewName { name: "abc".into(), years: 1 }] {
            assert_eq!(c.submit(signed(&c, &owner, action)).unwrap_err().to_string(), "not active yet");
        }
        let before = c.state.account(&owner.address()).balance;
        c.produce_block_with(vec![delegate.clone()]).unwrap();
        assert_eq!(c.failure(&delegate.id()).unwrap().0, "not active yet");
        assert_eq!(c.state.account(&owner.address()).balance, before - delegate.charge().unwrap());
        // Reserved names can't be registered; names transfer and release.
        assert!(c.submit(signed(&c, &owner, Action::RegisterName { name: "blockula".into() })).unwrap_err().to_string().contains("reserved"));
        c.submit(signed(&c, &owner, Action::RegisterName { name: "owner".into() })).unwrap();
        c.produce_block().unwrap();
        let give = signed(&c, &owner, Action::TransferName { name: "owner.boo".into(), to: bob.address() });
        assert_eq!(c.state.describe(&give, None), format!("owner.boo gave owner.boo to {}", short(&bob.address())));
        c.submit(give).unwrap();
        c.produce_block().unwrap();
        assert_eq!(c.state.resolve("owner.boo").unwrap(), bob.address());
        // Bob holds nothing yet: his release can't even pay its fee.
        assert!(c.submit(signed(&c, &bob, Action::ReleaseName { name: "owner".into() })).unwrap_err().to_string().contains("insufficient balance"));
        assert!(c.submit(signed(&c, &owner, Action::ReleaseName { name: "owner".into() })).unwrap_err().to_string().contains("belongs to"));
        // Without admins, the admin levers are closed.
        for action in [Action::SetRepo { repo: "a/b".into(), weight: 1 }, Action::GrantSoul { to: bob.address(), amount: SOUL, reason: "r".into() }] {
            assert!(c.submit(signed(&c, &owner, action)).unwrap_err().to_string().contains("only validator admins"));
        }
        // What admitting a tx reserves: amounts that leave the balance, not vote locks.
        let reserve = |a: Action| c.state.tx_debit(&signed(&c, &owner, a)) - signed(&c, &owner, Action::Unlock {}).charge().unwrap();
        assert!(reserve(Action::Delegate { validator: v.clone(), amount: 7 * FANG }) >= 7 * FANG);
        assert!(reserve(Action::Propose { kind: ProposalKind::Text {}, deposit: 9 * FANG, title: "t".into(), url: String::new() }) >= 9 * FANG);
        assert!(reserve(Action::Vote { proposal_id: 0, aye: true, amount: 1_000 * FANG, conviction: 6 }) < FANG);
        let root = c.state.root();
        drop(c);
        assert_eq!(Chain::open(&home).unwrap().state.root(), root, "replays");
        fs::remove_dir_all(home).ok();
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
        assert!(crate::upgrade::exec_schedule_upgrade(s, &mut j, 9, h, h).is_err(), "only after now");
        crate::upgrade::exec_schedule_upgrade(s, &mut j, 9, 100, h).unwrap();
        assert!(crate::upgrade::exec_schedule_upgrade(s, &mut j, 10, 50, h).is_err(), "only after the last one");
        assert_eq!(crate::upgrade::version_at(s, 100), 9);
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
        let c = Chain::open(&home).unwrap();
        assert_eq!((c.state.root(), c.block(1).unwrap().skipped), (root, vec![stale.size()]));
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
        fs::remove_dir_all(home).ok();
    }
}
