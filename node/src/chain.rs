//! Chain state and execution. Consensus (`consensus.rs`) orders blocks; this module executes
//! finalized blocks deterministically, in order, persists each one (block logs, then the state
//! database: see `store.rs`), publishes what readers see (`view.rs`) and keeps the mempool.

use crate::crypto::{self, Keys};
use crate::state::codec::SCHEMA_VERSION;
use crate::state::tree::{Child, NodeKey, NodeStore, Smt};
use crate::state::{JMap, RecordChange, TMap};
use crate::store::{self, hash32, BlockMeta, CommitPoint, Framing, Layout, Pos, SegLog, Snap, Store};
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

/// The treasury's system account on a chain whose addresses use `prefix`.
pub fn treasury(prefix: &str) -> String {
    crypto::system_address(prefix, "treasury")
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Allocation {
    pub address: String,
    #[serde(with = "u64_str")]
    pub amount: u64,
}

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
    /// Season 0 allowlist: accounts that may approve/remove validators.
    #[serde(default)]
    pub validator_admins: Vec<String>,
    /// Minimum bond (drops) for `register_validator`.
    #[serde(default = "default_min_bond", with = "u64_str")]
    pub min_bond: u64,
    /// Cap on the active set.
    #[serde(default = "default_max_validators")]
    pub max_validators: usize,
    /// Proof of Contribution timings. Absent: derived from `block_time_ms` (weeks, not minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contribution: Option<crate::poc::PocTimes>,
    /// bech32m prefix of this chain's addresses: `boo` (mainnet, dev) or `tboo` (public testnets).
    #[serde(default = "default_address_prefix")]
    pub address_prefix: String,
}

fn default_epoch_blocks() -> u64 {
    600
}
fn default_address_prefix() -> String {
    crypto::ADDRESS_HRP.into()
}
fn default_min_bond() -> u64 {
    10_000 * DROPS_PER_FANG
}
fn default_max_validators() -> usize {
    32
}

/// How many past epochs' validator sets the state keeps (for verifying recent certificates).
pub const EPOCH_SETS_KEPT: u64 = 8;

/// Genesis constants the state machine needs while applying txs (a state record, so the root
/// commits to them).
#[derive(Clone, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct Params {
    pub block_time_ms: u64,
    pub epoch_blocks: u64,
    pub admins: Vec<String>,
    pub min_bond: u64,
    pub max_validators: usize,
    pub poc: crate::poc::PocTimes,
    /// The chain's address prefix (see [`Params::prefix`]).
    pub address_prefix: String,
}

impl Params {
    /// The chain's address prefix (`boo` when unset, e.g. in a default `Params`).
    pub fn prefix(&self) -> &str {
        if self.address_prefix.is_empty() { crypto::ADDRESS_HRP } else { &self.address_prefix }
    }

    /// How far ahead a tx's `valid_until` may be: one hour of blocks.
    pub fn max_tx_validity_blocks(&self) -> u64 {
        (3_600_000 / self.block_time_ms.max(1)).max(1)
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
}

impl Genesis {
    pub fn is_validator(&self, addr: &str) -> bool {
        self.validators.iter().any(|v| v.address == addr)
    }

    pub fn params(&self) -> Params {
        let poc = self.contribution.clone().unwrap_or_else(|| crate::poc::PocTimes::for_block_time(self.block_time_ms));
        Params { block_time_ms: self.block_time_ms, epoch_blocks: self.epoch_blocks, admins: self.validator_admins.clone(), min_bond: self.min_bond, max_validators: self.max_validators, poc, address_prefix: self.address_prefix.clone() }
    }

    /// The state before block 1, committed (its root is the genesis state root).
    pub fn initial_state(&self) -> State {
        let mut state = State { params: self.params(), ..Default::default() };
        for a in &self.allocations {
            state.credit(&a.address, a.amount);
            state.supply += a.amount;
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
                },
            );
        }
        state.active_sets.insert(0, self.validators.iter().map(|v| v.address.clone()).collect());
        state.commit_block().expect("an in-memory tree can't fail");
        state
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("missing {} — run `blockula summon` first", path.display()))?;
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        if v.get("validators").is_none() {
            bail!("{} was created by an older single-validator Blockula; re-summon it (or `blockula dev --reset`)", path.display());
        }
        let g: Genesis = serde_json::from_value(v)?;
        if let Some(t) = &g.contribution {
            t.check().with_context(|| format!("{}", path.display()))?;
        }
        g.check_prefix().with_context(|| format!("{}", path.display()))?;
        Ok(g)
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
    pub(crate) poc: crate::poc::PocJournal,
}

impl Journal {
    pub fn absorb_pub(&mut self, other: Journal) {
        self.absorb(other)
    }

    /// Folds a nested journal in, keeping the oldest values.
    fn absorb(&mut self, other: Journal) {
        self.accounts.absorb(other.accounts);
        self.names.absorb(other.names);
        self.validators.absorb(other.validators);
        self.hunts.absorb(other.hunts);
        self.github.absorb(other.github);
        self.github_by_account.absorb(other.github_by_account);
        self.familiars.absorb(other.familiars);
        if self.scalars.is_none() {
            self.scalars = other.scalars;
        }
        if self.next_hunt_id.is_none() {
            self.next_hunt_id = other.next_hunt_id;
        }
        self.poc.absorb(other.poc);
    }
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
            if next.len() >= self.params.max_validators.max(1) {
                break;
            }
            next.push(v.address.clone());
        }
        if next.is_empty() { current } else { next }
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

    /// First name an address registered — shown everywhere instead of the raw address. A scan of
    /// every name: the API looks names up in the store's owner index instead (`ReadView`).
    #[cfg_attr(not(test), allow(dead_code))]
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

    fn j_scalars(&mut self, j: &mut Journal) {
        if j.scalars.is_none() {
            j.scalars = Some((self.supply, self.burned));
        }
    }

    /// Undoes everything recorded in `j` (values, and the dirty marks of keys it cleans up).
    pub fn rollback(&mut self, j: Journal) {
        j.accounts.restore(&mut self.accounts);
        j.names.restore(&mut self.names);
        j.validators.restore(&mut self.validators);
        if let Some((supply, burned)) = j.scalars {
            self.supply = supply;
            self.burned = burned;
        }
        j.hunts.restore(&mut self.hunts);
        j.github.restore(&mut self.github);
        j.github_by_account.restore(&mut self.github_by_account);
        j.familiars.restore(&mut self.familiars);
        if let Some(n) = j.next_hunt_id {
            self.next_hunt_id = n;
        }
        j.poc.rollback(&mut self.poc);
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
        check_validity(tx, height, self.params.max_tx_validity_blocks())?;
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
                if *bond < self.params.min_bond {
                    bail!("bond must be at least {}", fmt_fang(self.params.min_bond));
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
                    ValidatorRecord { address, owner: sender.clone(), consensus_key: consensus_key.clone(), p2p_addr: p2p_addr.clone(), http_addr: http_addr.clone(), bond: *bond, status: ValidatorStatus::Pending, registered_at: height, approved_at: None, approved_by: None, active_from: None, removed_at: None, removed_by: None, active_until: None, unbond_epoch: None },
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
                let lower = handle.to_ascii_lowercase();
                let link = self.github.get(&lower).cloned().ok_or_else(|| anyhow!("@{handle} isn't linked"))?;
                let admin = self.params.admins.contains(&sender);
                if link.account != sender && !admin {
                    bail!("only the linked account or a validator admin can unlink @{handle}");
                }
                if link.account == sender && key.role != KeyRole::Owner {
                    bail!("only owner keys can unlink GitHub");
                }
                self.j_github(j, &lower, &link.account);
                self.github.remove(&lower);
                self.github_by_account.remove(&link.account);
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
                let refunds: Vec<(String, u64)> = h.funders.iter().map(|(a, v)| (a.clone(), v.parse().unwrap_or(0))).collect();
                let total: u64 = refunds.iter().try_fold(0u64, |acc, (_, v)| acc.checked_add(*v)).ok_or_else(|| anyhow!("escrow overflow"))?;
                if total != h.amount {
                    bail!("escrow accounting mismatch on hunt #{hunt_id}");
                }
                for (who, v) in refunds {
                    self.j_credit(j, &who, v);
                }
                let h = self.hunt_mut(j, *hunt_id).unwrap();
                h.status = HuntStatus::Cancelled;
                h.cancelled_at = Some(height);
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
            _ => self.apply_poc(j, tx, &sender, signer, key, height)?,
        }
        Ok(())
    }

    /// What admitting this tx reserves from the sender's balance (charge + amount moved out).
    pub fn tx_debit(tx: &Tx) -> u64 {
        tx.charge().unwrap_or(u64::MAX).saturating_add(match &tx.action {
            Action::Transfer { amount, .. } => *amount,
            Action::RegisterName { name } => normalize_name(name).map(|n| name_price(&n)).unwrap_or(0),
            Action::RegisterValidator { bond, .. } => *bond,
            Action::CreateHunt { amount, .. } | Action::FundHunt { amount, .. } => *amount,
            Action::FundPool { amount } | Action::JoinJury { bond: amount } => *amount,
            Action::Vouch { .. } => crate::poc::VOUCH_BOND,
            Action::SubmitClaim { .. } => crate::poc::CLAIM_BOND,
            _ => 0,
        })
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
            _ => self.describe_poc(tx, &from, primary_name),
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
    /// Minimum validator bond in drops. `None`: 50 FANG on faucet chains (one faucet drip
    /// covers it), 10,000 FANG otherwise.
    pub min_bond: Option<u64>,
    /// Proof of Contribution timings (`None`: derived from the block time).
    pub contribution: Option<crate::poc::PocTimes>,
    /// Address prefix: `boo` (mainnet, dev) or `tboo` (public testnets).
    pub address_prefix: String,
}

impl SummonOpts {
    pub fn new(chain_id: &str, validators: usize) -> Self {
        Self { chain_id: chain_id.into(), faucet: true, block_time_ms: 1000, validators, http_port: None, admins: vec![], epoch_blocks: default_epoch_blocks(), min_bond: None, contribution: None, address_prefix: default_address_prefix() }
    }
}

/// How far behind the (claimed) network height still counts as synced.
pub const SYNC_SLACK: u64 = 3;
/// Certificates are stored on disk for these heights (plus every epoch's last block); the last
/// `view::RECENT_CERTS` heights' are kept in memory.
pub const CERT_CHECKPOINT: u64 = 100;
/// Mempool txs not included within this many blocks are dropped (they stay valid until their
/// `valid_until`, so a holder could resubmit them).
pub const MEMPOOL_TTL_BLOCKS: u64 = 600;
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

/// The fewest heights a pruning node keeps: both kept snapshots' manifests (checkpoint +
/// certifying blocks), every ancestor a proposal may still cite, and a tx's whole validity
/// window (so the tx index still catches a replayed tx).
pub fn min_keep_blocks(params: &Params) -> u64 {
    use crate::snapshot::{CHECKPOINT_DELAY, CHECKPOINT_INTERVAL};
    2 * CHECKPOINT_INTERVAL + 2 * CHECKPOINT_DELAY + crate::consensus::MAX_EXEC_LAG + params.max_tx_validity_blocks()
}

/// What a pruning validator keeps by default: `PRUNE_DAYS` of blocks at this block time.
pub fn default_keep_blocks(params: &Params) -> u64 {
    (PRUNE_DAYS * 86_400_000 / params.block_time_ms.max(1)).max(min_keep_blocks(params))
}

#[derive(Clone, Debug)]
struct PoolEntry {
    added_at: u64,
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
    /// The last executed block (older ones are read from the log, through a bounded cache).
    head: Option<Arc<Block>>,
    /// State database (records, tree nodes, indexes), its commit point, and a snapshot of it as
    /// of that commit (the tree's node store; readers get it with the view).
    store: Arc<Store>,
    point: CommitPoint,
    snap: Arc<Snap>,
    /// Executed blocks (JSON lines) and the exact consensus block bytes, segmented by height.
    blocks: SegLog,
    cblocks: SegLog,
    disk: Arc<Disk>,
    pub mempool: Vec<Tx>,
    pool: HashMap<String, PoolEntry>,
    pending: HashMap<String, (usize, u64)>,
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
    /// What readers (the HTTP API) see: published after every block and mempool change.
    pub live: Arc<Live>,
    /// Blocks kept on disk (`None`: all of them, an archive node).
    keep_blocks: Option<u64>,
    /// Log segments below this height are deleted.
    files_pruned_below: u64,
    /// Set when persisting a block failed: memory is ahead of the disk, so this instance executes
    /// nothing more (a restart recovers from the logs).
    broken: Option<String>,
    /// Blocks `open` executed again because they were past the store's commit point.
    pub replayed: u64,
    #[cfg(test)]
    pub fail_at: Option<WritePoint>,
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

/// Where `open` gets its state from, and where replay starts.
struct Start {
    state: State,
    point: CommitPoint,
    snap: Arc<Snap>,
    epochs: crate::epochs::EpochRegistry,
    /// The epoch registry comes from replaying (it's merged with `epochs.json` afterwards).
    rebuilt_epochs: bool,
}

/// Opens the database, or deletes and recreates it if it can't be opened (it is rebuilt from
/// the logs).
fn open_store(path: &Path) -> Result<Arc<Store>> {
    match Store::open(path) {
        Ok(s) => Ok(s),
        Err(e) if path.exists() && !format!("{e:#}").contains("already open") => {
            tracing::warn!(error = %format!("{e:#}"), "the state database doesn't open; rebuilding it from the block logs");
            fs::remove_file(path)?;
            Store::open(path)
        }
        Err(e) => Err(e),
    }
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
        let mut allocations: Vec<Allocation> = keys.iter().map(|k| Allocation { address: k.address_with(prefix), amount: per_validator }).collect();
        allocations.push(Allocation { address: treasury(prefix), amount: 100_000_000 * DROPS_PER_FANG });
        let validator_admins = if o.admins.is_empty() { vec![keys[0].address_with(prefix)] } else { o.admins.clone() };
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
            min_bond: o.min_bond.unwrap_or(if faucet { 50 * DROPS_PER_FANG } else { default_min_bond() }),
            max_validators: default_max_validators(),
            contribution: o.contribution.clone(),
            address_prefix: prefix.into(),
        };
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
        let layout = Layout::load_or_create(home, opts.segment_heights.unwrap_or(store::SEGMENT_HEIGHTS))?;
        let blocks = SegLog::open(&home.join("blocks"), "jsonl", Framing::Lines, layout.segment_heights)?;
        let cblocks = SegLog::open(&home.join("cblocks"), "bin", Framing::Records, layout.segment_heights)?;
        crate::snapshot::remove_partial(home);
        // An interrupted fast-sync download (it starts over).
        let _ = fs::remove_file(home.join("base.state.zst.part"));
        let genesis_state = genesis.initial_state();
        let genesis_root = genesis_state.root();
        let genesis_hash = hex::encode(crate::consensus::genesis_block(&genesis).digest_bytes());
        let store = open_store(&home.join(store::DB_FILE))?;
        let saved_epochs: Option<crate::epochs::EpochRegistry> = fs::read(home.join("epochs.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
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
        let genesis_arc = Arc::new(genesis.clone());
        let (height, last_hash) = (point.height, point.block_hash.clone());
        let head = match snap.block_meta(height)? {
            Some(m) => disk.load_block(height, &m),
            None => None,
        };
        let view = ReadView::new(state.clone(), height, last_hash.clone(), genesis_arc.clone(), genesis_hash.clone(), genesis_root.clone(), role, base.clone(), point.pruned_below, None, head.clone(), snap.clone(), disk.clone());
        let live = Arc::new(Live::new(view, epochs.clone(), disk.clone()));
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
            head,
            store,
            point,
            snap,
            blocks,
            cblocks,
            disk,
            mempool: vec![],
            pool: HashMap::new(),
            pending: HashMap::new(),
            mp: MempoolView::default(),
            failed_order: Default::default(),
            finalized_tip: 0,
            gossip: None,
            epochs,
            base,
            claims: HashMap::new(),
            snapshot_busy: Default::default(),
            live,
            keep_blocks: None,
            files_pruned_below: 0,
            broken: None,
            replayed: 0,
            #[cfg(test)]
            fail_at: None,
        };
        chain.replay_logs()?;
        if rebuilt_epochs {
            if let Some(saved) = &saved_epochs {
                let chain_id = chain.genesis.chain_id.clone();
                chain.epochs.merge_sigs(&chain_id, saved);
            }
            chain.save_epochs();
        }
        chain.reconcile_epochs()?;
        chain.finalized_tip = chain.height();
        chain.publish();
        chain.live.publish_epochs(chain.epochs.clone());
        Ok(chain)
    }

    /// Executes the blocks logged past the commit point again (a crash came between the logs
    /// and the database), then cuts off anything after the last complete block: a torn entry, or
    /// a consensus block whose executed block never made it to the log. Neither was acknowledged
    /// to consensus, so it delivers them again.
    fn replay_logs(&mut self) -> Result<()> {
        let (mut bend, mut cend) = (self.point.blocks_end, self.point.cblocks_end);
        let creader = self.cblocks.reader();
        let mut cit = creader.iter_from(cend);
        let mut cnext = cit.next().transpose()?;
        let breader = self.blocks.reader();
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
            self.replay(block, e.pos, cpos, durable).with_context(|| format!("replaying block {height} from {}", self.home.join("blocks").display()))?;
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
        self.blocks.truncate(bend)?;
        self.cblocks.truncate(cend)?;
        self.point.blocks_end = bend;
        self.point.cblocks_end = cend;
        Ok(())
    }

    /// Brings the validator-set history up to the next height's epoch from the sets in state
    /// (it can trail when `epochs.json` was saved before a crash).
    fn reconcile_epochs(&mut self) -> Result<()> {
        let want = self.state.params.epoch_of(self.height + 1);
        if self.epochs.known_through >= want {
            return Ok(());
        }
        for e in self.epochs.known_through + 1..=want {
            let keys: Vec<String> = self.state.active_set(e).map(|s| s.iter().map(|v| v.consensus_key.clone()).collect()).ok_or_else(|| anyhow!("epochs.json is missing epoch {e}, and the state no longer holds its set"))?;
            self.epochs.record(e, keys);
        }
        self.save_epochs();
        Ok(())
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

    /// Keeps only the last `keep` blocks on disk (`None`: keep everything). Never fewer than
    /// [`min_keep_blocks`].
    pub fn set_pruning(&mut self, keep: Option<u64>) {
        self.keep_blocks = keep.map(|k| k.max(min_keep_blocks(&self.state.params)));
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
    /// can't move it. Display-only (status/health); verified heights come from certificates.
    pub fn network_height_claimed(&self) -> u64 {
        self.live.claimed.load(std::sync::atomic::Ordering::Relaxed).max(self.finalized_tip).max(self.height)
    }

    /// Behind the network by more than a few blocks (verified or claimed).
    pub fn syncing(&self) -> bool {
        self.height + SYNC_SLACK < self.network_height_claimed()
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

    // Tests read through the chain; the API reads the published view directly.

    /// Executed block at `height`, if this node holds it.
    #[cfg(test)]
    pub fn block(&self, height: u64) -> Option<Block> {
        self.view().block(height).map(|b| (*b).clone())
    }

    /// State root after executing `height` (0 = genesis).
    pub fn root_at(&self, height: u64) -> Option<String> {
        self.view().root_at(height)
    }

    /// Hash of the executed block at `height`, including the fast-sync base, the anchor of the
    /// epoch we fast-synced into, and epochs' last blocks (kept through pruning).
    pub fn hash_at(&self, height: u64) -> Option<String> {
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
        self.snap.tx(&hash32(id).ok()?).ok()?
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

    /// Index rows for an executed block: its tx ids and the accounts whose history it joins.
    fn block_meta(&self, block: &Block, at: Pos, cblock: Option<Pos>) -> Result<BlockMeta> {
        let mut txs = Vec::with_capacity(block.txs.len());
        let mut history = vec![];
        for (i, tx) in block.txs.iter().enumerate() {
            txs.push(hash32(&tx.id())?);
            let sender = tx.sender().ok();
            if let Some(s) = &sender {
                history.push((i as u32, s.clone()));
            }
            let other = match &tx.action {
                Action::Transfer { to, .. } | Action::AwardHunt { to, .. } | Action::GrantSoul { to, .. } => Some(to),
                Action::Vouch { account } | Action::FlagSybil { account, .. } | Action::Clawback { account, .. } => Some(account),
                _ => None,
            };
            if let Some(to) = other.and_then(|t| self.state.resolve(t).ok()) {
                if sender.as_ref() != Some(&to) {
                    history.push((i as u32, to));
                }
            }
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
            history,
        })
    }

    /// Commits an executed block to the database (records, tree nodes, index rows, commit point)
    /// and moves the tree onto the new snapshot. The block is in the logs already.
    fn commit_store(&mut self, block: Arc<Block>, at: Pos, cblock: Option<Pos>, records: &[RecordChange], cert: Option<&[u8]>, durable: bool) -> Result<()> {
        let meta = self.block_meta(&block, at, cblock)?;
        let mut point = self.point.clone();
        point.height = block.height;
        point.block_hash = block.hash.clone();
        point.root = self.state.root_hash();
        point.root_child = self.state.tree.root_child().encode();
        point.blocks_end = at.end();
        if let Some(c) = cblock {
            point.cblocks_end = c.end();
        }
        let boundary = self.state.params.is_boundary(block.height);
        let cert = cert.filter(|_| block.height.is_multiple_of(CERT_CHECKPOINT) || boundary).map(|c| (block.height, c));
        let prune_below = self.keep_blocks.map(|k| (block.height + 1).saturating_sub(k)).unwrap_or(0);
        let nodes = self.state.tree.pending_changes();
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::BeforeStoreCommit) {
            bail!("injected crash before the store commit");
        }
        self.store.commit(store::Commit { point: &mut point, records, nodes: &nodes, block: Some(&meta), epoch_head: boundary, cert, prune_below, durable })?;
        self.point = point;
        let snap = Arc::new(self.store.snapshot()?);
        self.state.tree.rebase(snap.clone());
        self.snap = snap;
        self.height = block.height;
        self.last_hash = block.hash.clone();
        self.head = Some(block);
        // Whole log segments go once their heights' rows are gone.
        if self.point.pruned_below >= self.files_pruned_below.saturating_add(self.blocks.reader().seg_heights) {
            self.blocks.prune_below(self.point.pruned_below)?;
            self.cblocks.prune_below(self.point.pruned_below)?;
            self.files_pruned_below = self.point.pruned_below;
        }
        Ok(())
    }

    fn replay(&mut self, block: Block, at: Pos, cblock: Option<Pos>, durable: bool) -> Result<()> {
        if block.height != self.height() + 1 || block.prev_hash != self.last_hash() {
            bail!("the block log is not contiguous at height {}", block.height);
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
        self.state.poc_end_block(block.height, &block.hash);
        if self.state.params.is_boundary(block.height) {
            self.state.end_epoch(self.state.params.epoch_of(block.height));
            self.record_epoch(block.height);
        }
        let commit = self.state.commit_block()?;
        let root = hex::encode(commit.root);
        #[cfg(test)]
        self.state.debug_check(block.height);
        if root != block.state_root {
            bail!("state root mismatch at height {}", block.height);
        }
        self.commit_store(Arc::new(block), at, cblock, &commit.records, None, durable)
    }

    fn note_failed(&mut self, id: String, reason: String, height: u64) {
        if !self.mp.has_failure(&id) {
            self.failed_order.push_back(id.clone());
        }
        self.mp.fail(id, reason, height);
        while self.failed_order.len() > RECENT_FAILED {
            if let Some(old) = self.failed_order.pop_front() {
                self.mp.unfail(&old);
            }
        }
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
        if let Some(why) = &self.broken {
            bail!("a block failed to persist ({why}); restart the node to recover from its logs");
        }
        if f.height != self.height() + 1 {
            bail!("expected block {}, got {}", self.height() + 1, f.height);
        }
        if f.parent != self.last_hash() {
            bail!("block {} does not extend our chain", f.height);
        }
        let height = f.height;
        // From here on memory changes before the disk does: any failure leaves this instance
        // unusable until a restart.
        let result = self.execute_inner(f);
        if let Err(e) = &result {
            self.broken = Some(format!("block {height}: {e:#}"));
        }
        result
    }

    fn execute_inner(&mut self, f: Finalized) -> Result<()> {
        let mut txs = vec![];
        let mut failed = BTreeMap::new();
        for tx in f.txs {
            match self.state.apply(&tx, &self.genesis.chain_id, &f.proposer, f.height) {
                Ok(Outcome::Applied) => txs.push(tx),
                Ok(Outcome::Failed(r)) => {
                    failed.insert(txs.len(), r.clone());
                    self.note_failed(tx.id(), r, f.height);
                    txs.push(tx);
                }
                Err(e) => self.note_failed(tx.id(), format!("not included: {e}"), f.height),
            }
        }
        self.state.poc_end_block(f.height, &f.hash);
        if self.state.params.is_boundary(f.height) {
            self.state.end_epoch(self.state.params.epoch_of(f.height));
            if self.record_epoch(f.height) {
                self.sign_handoffs();
            }
        }
        let commit = self.state.commit_block()?;
        let state_root = hex::encode(commit.root);
        #[cfg(test)]
        self.state.debug_check(f.height);
        let block = Block { height: f.height, prev_hash: f.parent, timestamp_ms: f.timestamp_ms, view: f.view, parent_view: f.parent_view, version: f.version, proposer: f.proposer, state_root, txs, hash: f.hash, signature: String::new(), failed };
        self.persist(block, &f.cblock, &commit.records, f.certificate.as_deref())?;
        if let Some(cert) = f.certificate {
            self.disk.remember_cert(f.height, cert);
        }
        self.set_finalized_tip(f.height);
        if crate::snapshot::is_checkpoint(f.height) {
            self.take_snapshot();
        }
        self.revalidate_mempool();
        self.publish();
        Ok(())
    }

    /// The commit protocol (see `store.rs`): consensus block, then executed block, each appended
    /// and fsynced, then one durable database transaction.
    fn persist(&mut self, block: Block, cblock: &[u8], records: &[RecordChange], cert: Option<&[u8]>) -> Result<()> {
        let cpos = self.cblocks.append(block.height, cblock)?;
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::AfterCblock) {
            bail!("injected crash after the consensus block append");
        }
        self.cblocks.sync()?;
        let line = serde_json::to_vec(&block)?;
        let bpos = self.blocks.append(block.height, &line)?;
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::AfterBlockLine) {
            bail!("injected crash after the block append");
        }
        self.blocks.sync()?;
        let block = Arc::new(block);
        self.commit_store(block.clone(), bpos, Some(cpos), records, cert, true)?;
        self.disk.cache_block(block, line.len());
        #[cfg(test)]
        if self.fail_at == Some(WritePoint::AfterStoreCommit) {
            bail!("injected crash after the store commit");
        }
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
        self.save_epochs();
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

    /// Writes `epochs.json` durably (temp file, fsync, rename, fsync the directory) and publishes
    /// the registry. A failure is logged: the registry is rebuilt from state on the next start.
    fn save_epochs(&self) {
        if let Err(e) = self.try_save_epochs() {
            tracing::warn!(error = %format!("{e:#}"), "couldn't save epochs.json");
        }
    }

    fn try_save_epochs(&self) -> Result<()> {
        store::write_durably(&self.home.join("epochs.json"), &serde_json::to_vec(&self.epochs)?)?;
        self.live.publish_epochs(self.epochs.clone());
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
    pub fn install_snapshot(&mut self, v: &crate::snapshot::Verified, state: State, packed: &Path, epoch_start: Option<(u64, String)>) -> Result<()> {
        if self.height != 0 || self.base.is_some() {
            bail!("fast sync needs an empty home");
        }
        let base_hash = hex::encode(v.blocks[0].parent_bytes());
        store::install_durably(packed, &self.home.join("base.state.zst"))?;
        self.epochs = v.epochs.clone();
        self.try_save_epochs()?;
        let base = Base { height: v.height, hash: base_hash.clone(), state_root: v.state_root.clone(), epoch_start };
        store::write_durably(&self.home.join("base.json"), &serde_json::to_vec_pretty(&base)?)?;
        let start = (v.height + 1, self.blocks.reader().seg_of(v.height + 1));
        let mut point = CommitPoint { height: v.height, block_hash: base_hash.clone(), root: state.root_hash(), root_child: state.tree.root_child().encode(), base: v.height, pruned_below: start.0, blocks_end: (start.1, 0), cblocks_end: (start.1, 0), ..self.point.clone() };
        self.store.reset(&mut point, &mut state_records(&state), &state.tree.pending_changes())?;
        self.state = state;
        let snap = Arc::new(self.store.snapshot()?);
        self.state.tree.rebase(snap.clone());
        self.snap = snap;
        self.point = point;
        self.height = v.height;
        self.last_hash = base_hash;
        self.head = None;
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
        for tx in &self.mempool {
            let e = self.pending.entry(tx.from.clone()).or_default();
            e.0 += 1;
            e.1 = e.1.saturating_add(State::tx_debit(tx));
        }
    }

    /// Drops mempool txs that were included, expired, or can no longer apply. Uses a journal
    /// over the live state (rolled back afterwards) instead of cloning it.
    fn revalidate_mempool(&mut self) {
        let next = self.height() + 1;
        let proposer = self.fee_sink();
        let chain_id = self.genesis.chain_id.clone();
        // Txs included in earlier blocks left the mempool when those executed.
        let included: std::collections::HashSet<String> = self.head.as_ref().map(|b| b.txs.iter().map(Tx::id).collect()).unwrap_or_default();
        let mut j = Journal::default();
        let mut keep = Vec::with_capacity(self.mempool.len());
        let mut dropped = vec![];
        for tx in std::mem::take(&mut self.mempool) {
            let id = tx.id();
            if included.contains(&id) {
                self.pool.remove(&id);
                self.mp.remove(&id);
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
            self.mp.remove(&id);
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
        if self.pool.contains_key(&id) || self.has_tx(&id) {
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
            self.mp.remove(&eid);
            self.note_failed(eid, "evicted from a full mempool by a transaction with a higher tip".into(), next);
            self.rebuild_pending();
        }
        if let Some(g) = &self.gossip {
            let _ = g.unbounded_send(tx.clone());
        }
        let debit = State::tx_debit(&tx);
        let e = self.pending.entry(sender.clone()).or_default();
        e.0 += 1;
        e.1 = e.1.saturating_add(debit);
        self.pool.insert(id.clone(), PoolEntry { added_at: next });
        self.mp.add(id.clone(), &tx);
        self.mempool.push(tx);
        self.publish_mempool();
        Ok((id, true))
    }

    pub fn submit(&mut self, tx: Tx) -> Result<String> {
        self.submit_tx(tx).map(|(id, _)| id)
    }

    /// Adds a tx that another node already accepted (full nodes track what they forwarded).
    pub fn track_pending(&mut self, tx: Tx) -> Result<()> {
        tx.check_format()?;
        let id = tx.id();
        if !self.pool.contains_key(&id) && !self.has_tx(&id) {
            let sender = tx.sender()?;
            if self.pending.get(&sender).is_some_and(|p| p.0 >= MAX_PENDING_PER_SENDER) || self.mempool.len() >= MAX_MEMPOOL {
                return Ok(());
            }
            let debit = State::tx_debit(&tx);
            let e = self.pending.entry(sender.clone()).or_default();
            e.0 += 1;
            e.1 = e.1.saturating_add(debit);
            self.pool.insert(id.clone(), PoolEntry { added_at: self.height() + 1 });
            self.mp.add(id, &tx);
            self.mempool.push(tx);
            self.publish_mempool();
        }
        Ok(())
    }

    /// Removes a tx we queued locally (e.g. a full node whose forward failed).
    pub fn drop_pending(&mut self, id: &str) {
        if self.pool.remove(id).is_some() {
            self.mempool.retain(|t| t.id() != id);
            self.mp.remove(id);
            self.rebuild_pending();
            self.publish_mempool();
        }
    }

    pub fn pending_count(&self, addr: &str) -> usize {
        self.pending.get(addr).map(|p| p.0).unwrap_or(0)
    }

    #[cfg(test)]
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
            valid_until: self.height() + OWN_TX_VALIDITY.min(self.state.params.max_tx_validity_blocks()),
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
        let epochs = match self.saved_epochs {
            Some(e) => e.clone(),
            None if p.height == 0 => crate::epochs::EpochRegistry::genesis(self.genesis),
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
        assert!(c.has_tx(&orig_id));
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
        let max = c.state.params.max_tx_validity_blocks();
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
        assert!(crate::consensus::check_tx_stateless(&pay_main, "v1", "tboo").is_err(), "verify, gossip and HTTP refuse it too");
        c.produce_block_with(vec![pay_main.clone()]).unwrap();
        assert!(!c.has_tx(&pay_main.id()), "and it can't execute");
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
        let mut tx = Tx { chain_id: c.genesis.chain_id.clone(), nonce: c.next_nonce(&from), from, fee: 0, tip: 0, valid_until: c.height() + 50, action, memo: String::new(), public_key: hex::encode(keys.public_key()), signature: "00".repeat(crypto::SIGNATURE_LEN) };
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

    /// Entries in a log, from the start.
    fn entries(log: &SegLog) -> Vec<store::Entry> {
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
            c.fail_at = Some(point);
            assert!(c.produce_block().is_err(), "{point:?}");
            let after = c.state.root(); // memory ran ahead of the disk
            c.fail_at = None;
            assert!(c.produce_block().unwrap_err().to_string().contains("restart"), "a broken instance executes nothing more");
            drop(c);
            let mut c = Chain::open(&h).unwrap();
            if point == WritePoint::AfterCblock {
                // The block never reached the block log: it's gone, and so is its consensus block.
                assert_eq!((c.height(), c.state.root(), c.replayed), (height, before, 0));
                assert_eq!(entries(&c.cblocks).len() as u64, height, "the orphaned consensus block was cut off");
                // Consensus delivers it again.
                c.produce_block_with(vec![t.clone()]).unwrap();
                assert_eq!(c.state.root(), after);
            } else {
                assert_eq!((c.height(), c.state.root()), (height + 1, after), "{point:?}");
                assert_eq!(c.replayed, if point == WritePoint::AfterStoreCommit { 0 } else { 1 }, "{point:?}");
            }
            assert!(c.has_tx(&t.id()));
            assert_eq!(c.cblock(height + 1), Some(vec![]));
            assert_eq!(entries(&c.cblocks).len(), entries(&c.blocks).len());
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
        c.fail_at = Some(WritePoint::AfterBlockLine);
        assert!(c.produce_block().is_err());
        drop(c);
        let seg = h.join("blocks").join(format!("{:012}.jsonl", 1));
        let len = fs::metadata(&seg).unwrap().len();
        OpenOptions::new().write(true).open(&seg).unwrap().set_len(len - 5).unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.height(), c.state.root(), c.replayed), (height, root.clone(), 0));
        let logged = entries(&c.blocks);
        assert_eq!(logged.len() as u64, height);
        assert_eq!(fs::metadata(&seg).unwrap().len(), logged.last().unwrap().pos.end().1, "the torn tail is gone");
        assert_eq!(entries(&c.cblocks).len() as u64, height);
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
        assert_eq!(c.view().history(&alice.address(), 100).len() as u64, height);
        drop(c);
        // Garbage instead of a database: deleted and rebuilt the same way.
        fs::write(h.join(store::DB_FILE), b"not a database").unwrap();
        let c = Chain::open(&h).unwrap();
        assert_eq!((c.state.root(), c.replayed), (root.clone(), height));
        drop(c);
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
        let keep = min_keep_blocks(&c.state.params);
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
        let segs = c.blocks.reader().segments();
        assert!(segs[0] > 1 && segs[0] <= from && segs[0] + 50 > from - 50, "segments below the window are deleted: {segs:?}");
        assert_eq!(c.cblocks.reader().segments(), segs);
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
                let f = Finalized { height, hash: hex::encode(crypto::hash(&height.to_le_bytes())), parent: c.last_hash().to_string(), timestamp_ms: now_ms(), view: height, parent_view: height - 1, version: crate::consensus::PROTOCOL_VERSION, proposer: c.fee_sink(), txs, cblock: [vec![0u8; 170], cblock].concat(), certificate: None };
                c.execute(f).unwrap();
            }
            let after = (size(&h.join("blocks")), size(&h.join("cblocks")), fs::metadata(h.join(store::DB_FILE)).unwrap().len());
            println!("{per_block} transfers per block: blocks/ {} B, cblocks/ {} B, state.redb {} B per block", (after.0 - before.0) / n, (after.1 - before.1) / n, after.2.saturating_sub(before.2) / n);
            drop(c);
            fs::remove_dir_all(h).ok();
        }
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
