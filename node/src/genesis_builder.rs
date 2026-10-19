//! The genesis builder (OPS-2, TOK-4, MAINNET-DESIGN 3.7; the process is docs/GENESIS_CEREMONY.md).
//!
//! * `blockula genesis operator` runs offline on an operator's key machine: from the consensus key,
//!   the owner account, the addresses, commission, self-bond and independence fields, it writes the
//!   operator's public entry (TOML, or JSON) with a proof of possession. Nothing secret leaves.
//! * `blockula genesis build --spec spec.toml` reads a reviewed spec (allocations by DEC-5 bucket,
//!   the operator files, parameters, reserved names, the Soul ledger, the contribution pool's repos,
//!   opening balance and timings) and writes a `genesis.json`
//!   that is byte-identical for the same spec and binary, and prints its genesis hash.
//!
//! The builder validates: allocations add up to `total_supply` exactly; every allocation names a
//! DEC-5 bucket and gets that bucket's vesting curve (keyless buckets get their system account);
//! each bucket holds its share (docs/TOKENOMICS.md); every operator's proof of possession, keys
//! and addresses; the independence policy (DEC-3, DEC-14: at least 22 operators; per hosting
//! provider, ASN and country at most ⌊n/4⌋; per facility at most 15%; at most 2 founder or
//! foundation seats; one seat per entity); and everything `Genesis::check` does (parameter bounds,
//! no faucet and no admins on `blockula-1`). Bucket and independence findings are warnings unless
//! the spec (or `--strict`) says `strict`; `blockula-1` is always strict.

use crate::chain::{bootstrap_delegation, check_host_port, check_http_addr, community, endowment, make_pop, system_account, treasury, Allocation, Genesis, Operator, SoulEntry, ValidatorInfo, Vesting, MAINNET_CHAIN_ID};
use crate::crypto;
use crate::params::MAX_SUPPLY;
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// An issuance year (365.25 days) in milliseconds: vesting curves are in blocks of it.
const YEAR_MS: u64 = 31_557_600_000;
/// DEC-3: a mainnet genesis has at least this many independent operators.
pub const MIN_GENESIS_OPERATORS: usize = 22;
/// DEC-3, DEC-14: seats the founder and the foundation may run at genesis.
pub const MAX_FOUNDER_SEATS: usize = 2;
/// DEC-14: most of the set one facility may host, in bps.
pub const MAX_FACILITY_BPS: usize = 1_500;

/// A DEC-5 allocation bucket (docs/TOKENOMICS.md, section 1).
pub struct Bucket {
    pub name: &'static str,
    /// Share of the total supply.
    pub bps: u64,
    /// Keyless buckets live in a system account (`crypto::system_address(prefix, label)`).
    pub system: Option<fn(&str) -> String>,
    /// `(start, cliff, end)` in months from genesis (12 to a 365.25-day year), if it vests.
    pub vesting: Option<(u64, u64, u64)>,
}

/// DEC-5. The bootstrap delegation's lock is its return to the treasury, linear over years 2–4:
/// at each epoch boundary staking delegates what it still holds by formula and sends what has
/// "vested" to the treasury (`staking::bootstrap_epoch`). The foundation gets 0% since 2026-10-18:
/// its 5% is the treasury's, for governance to fund a foundation from once one exists
/// (TOKENOMICS.md). The bucket stays so the launched testnets' specs, which gave it 5%, still
/// rebuild byte for byte (with a finding; `blockula-1`, always strict, refuses any).
pub const BUCKETS: &[Bucket] = &[
    Bucket { name: "community", bps: 3_200, system: Some(community), vesting: None },
    Bucket { name: "treasury", bps: 3_000, system: Some(treasury), vesting: Some((0, 0, 48)) },
    Bucket { name: "endowment", bps: 1_500, system: Some(endowment), vesting: Some((0, 0, 120)) },
    Bucket { name: "bootstrap", bps: 1_000, system: Some(bootstrap_delegation), vesting: Some((12, 12, 48)) },
    Bucket { name: "founder", bps: 1_000, system: None, vesting: Some((0, 12, 48)) },
    Bucket { name: "foundation", bps: 0, system: None, vesting: Some((0, 0, 48)) },
    Bucket { name: "operators", bps: 300, system: None, vesting: Some((0, 6, 24)) },
];

/// Buckets whose recipients' validators the bootstrap delegation passes over (DEC-5), with the
/// owners of seats marked `founder_or_foundation`.
pub const BOOTSTRAP_EXCLUDED_BUCKETS: &[&str] = &["founder", "foundation"];

/// Not a DEC-5 bucket: on a chain with a faucet (testnets), liquid FANG for the validators'
/// consensus-key accounts, which pay the drips (`Chain::faucet`) and their own fees. It sits
/// outside the DEC-5 table (the buckets' shares are of the supply without it), and a chain
/// without a faucet (so `blockula-1`) refuses it.
pub const FAUCET_BUCKET: &str = "faucet";

/// The label of the contribution pool's opening balance (`[contribution] opening_balance`).
pub const POOL_OPENING_LABEL: &str = "contribution pool: opening balance (endowment)";

impl Bucket {
    /// Its vesting curve in blocks at this block time.
    pub fn curve(&self, block_time_ms: u64) -> Option<Vesting> {
        let months = |m: u64| (YEAR_MS as u128 * m as u128 / 12 / block_time_ms.max(1) as u128) as u64;
        self.vesting.map(|(start, cliff, end)| Vesting { start: months(start), cliff: months(cliff), end: months(end) })
    }
}

// ---- the spec ----

/// `spec.toml`: everything a genesis is built from, as reviewed text.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub chain_id: String,
    pub address_prefix: String,
    /// Genesis time in UTC milliseconds: fixed, never "now".
    pub genesis_time_ms: u64,
    pub block_time_ms: u64,
    pub epoch_blocks: u64,
    #[serde(default)]
    pub faucet: bool,
    #[serde(default)]
    pub validator_admins: Vec<String>,
    /// FANG ("1000000000"); the allocations must add up to exactly this.
    pub total_supply: String,
    /// Bucket and independence findings are errors (always on for `blockula-1`).
    #[serde(default)]
    pub strict: bool,
    /// Operator files (TOML or JSON, from `blockula genesis operator`), relative to the spec.
    #[serde(default)]
    pub operators: Vec<String>,
    /// Overrides of the governance parameters (`params.rs`), as numbers or strings.
    #[serde(default)]
    pub params: BTreeMap<String, Num>,
    #[serde(default)]
    pub reserved_names: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default, rename = "allocation")]
    pub allocations: Vec<SpecAllocation>,
    #[serde(default, rename = "soul")]
    pub soul_ledger: Vec<SpecSoul>,
    /// Proof of Contribution from genesis: timings, the pool's repos and its opening balance.
    #[serde(default)]
    pub contribution: Option<SpecContribution>,
}

/// `[contribution]`: the contribution pool open from block 1 instead of after a `SetRepo`
/// proposal and the first epochs' issuance.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecContribution {
    /// FANG the pool holds at genesis, out of the endowment bucket: it counts toward that
    /// bucket's share and `total_supply`, so the spec's endowment allocation is that much
    /// smaller. DEC-5 releases the endowment linearly from zero, so this is a finding (a testnet
    /// shortcut, refused when strict).
    #[serde(default)]
    pub opening_balance: Option<String>,
    /// Repos in the pool from genesis (`[[contribution.repo]]`).
    #[serde(default, rename = "repo")]
    pub repos: Vec<SpecRepo>,
    /// Timings in blocks (`poc::PocTimes`); each one left out keeps its default for the block
    /// time (weeks: a Séance a week, a 30-day revert window, 90-day vesting).
    #[serde(default)]
    pub seance_blocks: Option<u64>,
    #[serde(default)]
    pub claim_maturity_blocks: Option<u64>,
    #[serde(default)]
    pub vesting_blocks: Option<u64>,
    #[serde(default)]
    pub vouch_lock_blocks: Option<u64>,
    #[serde(default)]
    pub claim_ttl_blocks: Option<u64>,
    #[serde(default)]
    pub soul_half_life_blocks: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecRepo {
    /// GitHub `owner/name` (any case; the pool keys it lowercase).
    pub repo: String,
    /// 1–`poc::MAX_REPO_WEIGHT`: a claim's payout weight is its score times this.
    pub weight: u64,
}

impl SpecContribution {
    /// The timings, if the spec sets any (`None`: the genesis derives them from the block time).
    fn times(&self, block_time_ms: u64) -> Option<crate::poc::PocTimes> {
        let set = [self.seance_blocks, self.claim_maturity_blocks, self.vesting_blocks, self.vouch_lock_blocks, self.claim_ttl_blocks, self.soul_half_life_blocks];
        if set.iter().all(Option::is_none) {
            return None;
        }
        let d = crate::poc::PocTimes::for_block_time(block_time_ms);
        Some(crate::poc::PocTimes {
            seance_blocks: self.seance_blocks.unwrap_or(d.seance_blocks),
            claim_maturity_blocks: self.claim_maturity_blocks.unwrap_or(d.claim_maturity_blocks),
            vesting_blocks: self.vesting_blocks.unwrap_or(d.vesting_blocks),
            vouch_lock_blocks: self.vouch_lock_blocks.unwrap_or(d.vouch_lock_blocks),
            claim_ttl_blocks: self.claim_ttl_blocks.unwrap_or(d.claim_ttl_blocks),
            soul_half_life_blocks: self.soul_half_life_blocks.unwrap_or(d.soul_half_life_blocks),
        })
    }
}

/// A number written as a TOML integer or a string (amounts past 2^53 read better quoted).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Num {
    Int(u64),
    Str(String),
}

impl Num {
    fn value(&self) -> Result<u64> {
        match self {
            Num::Int(v) => Ok(*v),
            Num::Str(s) => s.trim().parse().map_err(|_| anyhow!("{s:?} isn't a whole number")),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecAllocation {
    /// One of [`BUCKETS`].
    pub bucket: String,
    /// The recipient (keyless buckets: left out, their system account is used).
    #[serde(default)]
    pub address: Option<String>,
    /// FANG ("100000000" or "12.5").
    pub amount: String,
    /// Display ("founder", "audit grant: Example LLC"); defaults to the bucket.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecSoul {
    pub account: String,
    /// Soul, up to 3 decimals ("12.5").
    pub soul: String,
    pub reason: String,
}

// ---- operator files (docs/GENESIS_CEREMONY.md, section 3) ----

/// What an operator submits: written by `blockula genesis operator`, completed by hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorFile {
    pub operator: OperatorInfo,
    pub keys: OperatorKeys,
    pub network: OperatorNetwork,
    pub staking: OperatorStaking,
    pub independence: Independence,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorInfo {
    pub name: String,
    #[serde(default)]
    pub contact: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorKeys {
    /// hex ML-DSA-65 public key.
    pub consensus_public_key: String,
    pub consensus_address: String,
    /// The cold owner account: bond, rewards, commission.
    pub owner: String,
    /// The consensus key's proof of possession over `pop_message(chain_id, owner, p2p, http)`.
    pub pop: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorNetwork {
    pub p2p_addr: String,
    pub http_addr: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorStaking {
    pub commission_bps: u64,
    /// FANG, out of the owner's allocation.
    pub self_bond: String,
}

/// Who runs the validator and where (DEC-14). Only provider, ASN, country and entity enter
/// genesis (as display fields); all of it is published with the spec.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Independence {
    pub legal_entity: String,
    /// ISO 3166-1 alpha-2 of the entity.
    pub entity_jurisdiction: String,
    #[serde(default)]
    pub beneficial_owners: Vec<String>,
    pub hosting_provider: String,
    /// The ASN announcing `p2p_addr`.
    pub asn: u32,
    /// ISO 3166-1 alpha-2 of the data center.
    pub datacenter_country: String,
    /// The facility (data center) itself: at most 15% of the set in one.
    pub datacenter: String,
    /// Run by the founder or the foundation (at most 2 such seats).
    #[serde(default)]
    pub founder_or_foundation: bool,
    /// Other validators, operators or the foundation this operator is tied to.
    #[serde(default)]
    pub affiliations: Vec<String>,
    #[serde(default)]
    pub statement: String,
}

impl OperatorFile {
    /// Reads an operator file: JSON if the name ends in `.json`, else TOML.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        if path.extension().is_some_and(|e| e == "json") {
            serde_json::from_str(&text).with_context(|| format!("{}", path.display()))
        } else {
            toml::from_str(&text).with_context(|| format!("{}", path.display()))
        }
    }

    /// The genesis operator entry.
    fn entry(&self) -> Result<Operator> {
        let i = &self.independence;
        let some = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
        Ok(Operator {
            owner: self.keys.owner.clone(),
            consensus_key: self.keys.consensus_public_key.clone(),
            pop: self.keys.pop.clone(),
            p2p_addr: self.network.p2p_addr.clone(),
            http_addr: self.network.http_addr.clone(),
            self_bond: parse_fang_amount(&self.staking.self_bond).context("staking.self_bond")?,
            commission_bps: self.staking.commission_bps,
            provider: some(&i.hosting_provider),
            asn: (i.asn != 0).then_some(i.asn),
            country: some(&i.datacenter_country),
            entity: some(&i.legal_entity),
        })
    }
}

/// A genesis built from a spec.
pub struct Built {
    pub genesis: Genesis,
    /// The `genesis.json` bytes (pretty JSON, one trailing newline).
    pub json: String,
    /// hex of the genesis block's digest (what `join --genesis-hash` pins).
    pub hash: String,
    /// Bucket and independence findings that didn't stop the build (none when strict).
    pub warnings: Vec<String>,
}

fn parse_fang_amount(s: &str) -> Result<u64> {
    crate::validator_cli::parse_fang(s)
}

/// "12.5" Soul → milli-Soul.
fn parse_soul(s: &str) -> Result<u64> {
    let s = s.trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    ensure!(!whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()) && frac.len() <= 3 && frac.bytes().all(|b| b.is_ascii_digit()), "invalid Soul amount {s:?}");
    let milli: u64 = format!("{frac:0<3}").parse()?;
    whole.parse::<u64>()?.checked_mul(crate::poc::SOUL).and_then(|w| w.checked_add(milli)).ok_or_else(|| anyhow!("Soul amount too large"))
}

/// Reads `spec` and the operator files it names and builds the genesis. `strict` adds to the
/// spec's own flag.
pub fn build(spec_path: &Path, strict: bool) -> Result<Built> {
    let text = std::fs::read_to_string(spec_path).with_context(|| format!("reading {}", spec_path.display()))?;
    let spec: Spec = toml::from_str(&text).with_context(|| format!("{}", spec_path.display()))?;
    let dir = spec_path.parent().unwrap_or(Path::new("."));
    let operators = spec.operators.iter().map(|f| OperatorFile::load(&dir.join(f))).collect::<Result<Vec<_>>>()?;
    build_from(&spec, &operators, strict)
}

/// [`build`] from a parsed spec and its operator files.
pub fn build_from(spec: &Spec, operators: &[OperatorFile], strict: bool) -> Result<Built> {
    let mainnet = spec.chain_id == MAINNET_CHAIN_ID;
    let strict = strict || spec.strict || mainnet;
    let prefix = spec.address_prefix.as_str();
    let mut findings: Vec<String> = vec![];
    ensure!(!spec.chain_id.is_empty() && spec.chain_id.len() <= MAX_CHAIN_ID && spec.chain_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'), "chain_id must be 1–{MAX_CHAIN_ID} of a-z, 0-9 and '-'");
    ensure!(crypto::is_prefix(prefix), "address_prefix must be one of {:?}", crypto::KNOWN_PREFIXES);
    ensure!(spec.genesis_time_ms > 0, "genesis_time_ms must be a fixed time (UTC milliseconds)");
    if mainnet {
        ensure!(prefix == crypto::ADDRESS_HRP, "{MAINNET_CHAIN_ID} uses the {} prefix", crypto::ADDRESS_HRP);
        ensure!(!spec.faucet && spec.validator_admins.is_empty(), "{MAINNET_CHAIN_ID} has no faucet and no admin keys (DEC-10)");
    }

    // Allocations, by bucket.
    let total = parse_fang_amount(&spec.total_supply).context("total_supply")?;
    ensure!(!mainnet || total == MAX_SUPPLY, "{MAINNET_CHAIN_ID}'s supply is {} (DEC-5)", fmt_fang(MAX_SUPPLY));
    let mut allocations = vec![];
    let mut by_bucket: BTreeMap<&str, u128> = BTreeMap::new();
    let mut sum: u128 = 0;
    let mut faucet: u128 = 0;
    for (i, a) in spec.allocations.iter().enumerate() {
        let what = format!("allocation {} ({})", i + 1, a.bucket);
        if a.bucket == FAUCET_BUCKET {
            ensure!(spec.faucet, "{what}: only a chain with a faucet (`faucet = true`) funds one");
            let addr = a.address.as_ref().ok_or_else(|| anyhow!("{what}: needs an `address`"))?;
            ensure!(crypto::parse_address(addr).map(|(p, _)| p) == Some(prefix), "{what}: {addr} isn't a {prefix}1… address");
            ensure!(system_account(prefix, addr).is_none(), "{what}: {addr} is a keyless system account");
            let amount = parse_fang_amount(&a.amount).with_context(|| what.clone())?;
            ensure!(amount > 0, "{what}: amount must be more than 0");
            sum += amount as u128;
            faucet += amount as u128;
            allocations.push(Allocation { address: addr.clone(), amount, vesting: None, label: Some(a.label.clone().unwrap_or_else(|| FAUCET_BUCKET.to_string())) });
            continue;
        }
        let bucket = BUCKETS.iter().find(|b| b.name == a.bucket).ok_or_else(|| anyhow!("{what}: unknown bucket (known: {}, and {FAUCET_BUCKET} on chains with a faucet)", BUCKETS.iter().map(|b| b.name).collect::<Vec<_>>().join(", ")))?;
        let address = match (bucket.system, &a.address) {
            (Some(sys), None) => sys(prefix),
            (Some(sys), Some(addr)) if *addr == sys(prefix) => addr.clone(),
            (Some(_), Some(_)) => bail!("{what}: the {} bucket is keyless; leave `address` out", bucket.name),
            (None, Some(addr)) => {
                ensure!(crypto::parse_address(addr).map(|(p, _)| p) == Some(prefix), "{what}: {addr} isn't a {prefix}1… address");
                ensure!(system_account(prefix, addr).is_none(), "{what}: {addr} is a keyless system account");
                addr.clone()
            }
            (None, None) => bail!("{what}: needs an `address`"),
        };
        let amount = parse_fang_amount(&a.amount).with_context(|| what.clone())?;
        ensure!(amount > 0, "{what}: amount must be more than 0");
        sum += amount as u128;
        *by_bucket.entry(bucket.name).or_default() += amount as u128;
        allocations.push(Allocation { address, amount, vesting: bucket.curve(spec.block_time_ms), label: Some(a.label.clone().unwrap_or_else(|| bucket.name.to_string())) });
    }
    // The pool's opening balance: an endowment allocation paid into the pool's account, liquid.
    let contribution = spec.contribution.clone().unwrap_or_default();
    if let Some(opening) = &contribution.opening_balance {
        let amount = parse_fang_amount(opening).context("contribution.opening_balance")?;
        ensure!(amount > 0, "contribution.opening_balance must be more than 0 (or left out)");
        sum += amount as u128;
        *by_bucket.entry("endowment").or_default() += amount as u128;
        allocations.push(Allocation { address: crate::poc::pool_address(prefix), amount, vesting: None, label: Some(POOL_OPENING_LABEL.into()) });
        findings.push(format!("the contribution pool opens with {} out of the endowment, which DEC-5 releases linearly from zero (docs/TOKENOMICS.md, section 1)", fmt_fang(amount)));
    }
    ensure!(sum == total as u128, "allocations add up to {} drops, but total_supply is {} drops", sum, total);
    for b in BUCKETS {
        let want = (total as u128 - faucet) * b.bps as u128 / 10_000;
        let has = by_bucket.get(b.name).copied().unwrap_or(0);
        if has != want {
            findings.push(format!("bucket {} holds {} drops; DEC-5 gives it {} ({}% of the supply)", b.name, has, want, crate::staking::fmt_bps(b.bps)));
        }
    }

    // Operators: their entries and validators, the independence policy.
    let mut entries = vec![];
    let mut validators = vec![];
    let mut seen_p2p = BTreeSet::new();
    for o in operators {
        let who = format!("operator {:?}", o.operator.name);
        let pk = hex::decode(&o.keys.consensus_public_key).map_err(|_| anyhow!("{who}: consensus_public_key isn't hex"))?;
        ensure!(pk.len() == crypto::PUBLIC_KEY_LEN, "{who}: consensus_public_key must be an ML-DSA-65 public key");
        let address = crypto::address_with_prefix(prefix, &pk);
        ensure!(o.keys.consensus_address == address, "{who}: consensus_address is {}, but its key's address is {address}", o.keys.consensus_address);
        check_host_port(&o.network.p2p_addr).with_context(|| format!("{who}: p2p_addr"))?;
        check_http_addr(&o.network.http_addr).with_context(|| format!("{who}: http_addr"))?;
        let host = o.network.p2p_addr.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_default();
        ensure!(seen_p2p.insert(host.clone()), "{who}: another operator uses the p2p host {host}");
        entries.push(o.entry().with_context(|| who.clone())?);
        validators.push(ValidatorInfo { address, public_key: o.keys.consensus_public_key.clone(), p2p_addr: o.network.p2p_addr.clone(), http_addr: o.network.http_addr.clone() });
    }
    findings.extend(independence_findings(operators));
    // Canonical order, whatever order the files were listed in.
    validators.sort_by_key(|v| v.address.clone());
    entries.sort_by_key(|o| crypto::address_with_prefix(prefix, &hex::decode(&o.consensus_key).unwrap_or_default()));

    let mut param_overrides = BTreeMap::new();
    for (k, v) in &spec.params {
        param_overrides.insert(k.clone(), v.value().with_context(|| format!("params.{k}"))?);
    }
    let mut pool_repos = BTreeMap::new();
    for r in &contribution.repos {
        let key = r.repo.trim().to_ascii_lowercase();
        ensure!(pool_repos.insert(key.clone(), r.weight).is_none(), "contribution.repo: {key} is listed twice");
    }
    let soul_ledger = spec.soul_ledger.iter().map(|s| Ok(SoulEntry { account: s.account.clone(), soul: parse_soul(&s.soul).with_context(|| format!("soul for {}", s.account))?, reason: s.reason.clone() })).collect::<Result<Vec<_>>>()?;
    // The founder's account (and the owners of the founder's or foundation's seats) start with no
    // Soul: no jury seats or pool claims while the founder's vesting runs (docs/TOKENOMICS.md,
    // "What the founder must not do"). Founding jurors are other people.
    let excluded = bootstrap_excluded(spec, operators);
    for e in soul_ledger.iter().filter(|e| e.soul > 0) {
        ensure!(!excluded.contains(&e.account), "soul: {} is the founder's (or owns a founder's or foundation's seat), whose Soul is 0 at genesis (docs/TOKENOMICS.md)", e.account);
    }

    let genesis = Genesis {
        chain_id: spec.chain_id.clone(),
        token: "FANG".into(),
        decimals: DECIMALS,
        block_time_ms: spec.block_time_ms,
        validators,
        faucet: spec.faucet,
        created_at_ms: spec.genesis_time_ms,
        allocations,
        epoch_blocks: spec.epoch_blocks,
        validator_admins: spec.validator_admins.clone(),
        contribution: contribution.times(spec.block_time_ms),
        address_prefix: prefix.into(),
        param_overrides,
        operators: entries,
        reserved_names: spec.reserved_names.clone(),
        soul_ledger,
        features: spec.features.clone(),
        bootstrap_excluded: excluded,
        pool_repos,
    };
    ensure!(!genesis.validators.is_empty(), "a genesis needs at least one operator");
    genesis.check()?;
    if strict && !findings.is_empty() {
        bail!("the spec breaks the policy (strict):\n  - {}", findings.join("\n  - "));
    }
    let json = serde_json::to_string_pretty(&genesis)? + "\n";
    let hash = hex::encode(crate::consensus::genesis_block(&genesis).digest_bytes());
    Ok(Built { genesis, json, hash, warnings: findings })
}

/// The accounts whose validators the bootstrap delegation passes over (DEC-5): the founder's
/// (and on the launched testnets the foundation's) recipients and the owners of seats marked
/// `founder_or_foundation`. Sorted, each once.
fn bootstrap_excluded(spec: &Spec, operators: &[OperatorFile]) -> Vec<String> {
    let allocated = spec.allocations.iter().filter(|a| BOOTSTRAP_EXCLUDED_BUCKETS.contains(&a.bucket.as_str())).filter_map(|a| a.address.clone());
    let owners = operators.iter().filter(|o| o.independence.founder_or_foundation).map(|o| o.keys.owner.clone());
    allocated.chain(owners).collect::<BTreeSet<String>>().into_iter().collect()
}

/// The independence policy (DEC-3, DEC-14) over a set of operators: what breaks it, in words.
pub fn independence_findings(operators: &[OperatorFile]) -> Vec<String> {
    let n = operators.len();
    let mut out = vec![];
    if n < MIN_GENESIS_OPERATORS {
        out.push(format!("{n} operators; a mainnet genesis needs at least {MIN_GENESIS_OPERATORS} (DEC-3)"));
    }
    for o in operators {
        let i = &o.independence;
        let missing: Vec<&str> = [("legal_entity", &i.legal_entity), ("entity_jurisdiction", &i.entity_jurisdiction), ("hosting_provider", &i.hosting_provider), ("datacenter_country", &i.datacenter_country), ("datacenter", &i.datacenter)].into_iter().filter(|(_, v)| v.trim().is_empty()).map(|(k, _)| k).collect();
        if !missing.is_empty() || i.asn == 0 {
            out.push(format!("operator {:?} leaves independence fields empty: {}", o.operator.name, missing.into_iter().chain((i.asn == 0).then_some("asn")).collect::<Vec<_>>().join(", ")));
        }
    }
    let quarter = n / 4;
    let mut count = |what: &str, cap: usize, key: &dyn Fn(&Independence) -> String| {
        let mut by: BTreeMap<String, usize> = BTreeMap::new();
        for o in operators {
            let k = key(&o.independence);
            if !k.is_empty() {
                *by.entry(k).or_default() += 1;
            }
        }
        for (k, c) in by.into_iter().filter(|(_, c)| *c > cap) {
            out.push(format!("{what} {k} has {c} of the {n} operators; at most {cap} may"));
        }
    };
    let lower = |s: &str| s.trim().to_ascii_lowercase();
    // DEC-14: ⌊n/4⌋ per provider, ASN and country; 15% per facility; one seat per entity (the
    // founder's and the foundation's seats are capped together instead).
    count("hosting provider", quarter, &|i| lower(&i.hosting_provider));
    count("ASN", quarter, &|i| if i.asn == 0 { String::new() } else { i.asn.to_string() });
    count("data-center country", quarter, &|i| lower(&i.datacenter_country));
    count("facility", n * MAX_FACILITY_BPS / 10_000, &|i| lower(&i.datacenter));
    count("legal entity", 1, &|i| if i.founder_or_foundation { String::new() } else { lower(&i.legal_entity) });
    let founder = operators.iter().filter(|o| o.independence.founder_or_foundation).count();
    if founder > MAX_FOUNDER_SEATS {
        out.push(format!("{founder} seats are the founder's or the foundation's; at most {MAX_FOUNDER_SEATS} (DEC-3)"));
    }
    out
}

/// `blockula genesis operator`: an operator's public entry from its consensus key (offline).
#[allow(clippy::too_many_arguments)]
pub fn operator_file(keys: &crypto::Keys, chain_id: &str, owner: &str, p2p_addr: &str, http_addr: &str, commission_bps: u64, self_bond: &str, operator: OperatorInfo, independence: Independence) -> Result<OperatorFile> {
    let (prefix, _) = crypto::parse_address(owner).ok_or_else(|| anyhow!("--owner must be a boo1… (or tboo1…) address"))?;
    check_host_port(p2p_addr).context("--p2p-addr")?;
    check_http_addr(http_addr).context("--http-addr")?;
    ensure!(commission_bps <= 10_000, "--commission-bps is at most 10000");
    parse_fang_amount(self_bond).context("--self-bond")?;
    Ok(OperatorFile {
        operator,
        keys: OperatorKeys { consensus_public_key: hex::encode(keys.public_key()), consensus_address: keys.address_with(prefix), owner: owner.into(), pop: make_pop(keys.seed(), chain_id, owner, p2p_addr, http_addr) },
        network: OperatorNetwork { p2p_addr: p2p_addr.into(), http_addr: http_addr.into() },
        staking: OperatorStaking { commission_bps, self_bond: self_bond.into() },
        independence,
    })
}

/// `blockula genesis operator`'s inputs.
#[derive(clap::Args)]
pub struct OperatorArgs {
    /// The chain the proof of possession is for (blockula-1)
    #[arg(long)]
    pub chain_id: String,
    /// The consensus key: a validator.key (32-byte hex seed) or `blockula keygen` JSON
    #[arg(long)]
    pub key: PathBuf,
    /// The cold owner account (bond, rewards, commission)
    #[arg(long)]
    pub owner: String,
    /// Public host:port other validators dial
    #[arg(long)]
    pub p2p_addr: String,
    /// The HTTP endpoint you publish (your sentry, never the validator)
    #[arg(long)]
    pub http_addr: String,
    /// Commission on the pool's rewards, in basis points
    #[arg(long, default_value_t = 500)]
    pub commission_bps: u64,
    /// Self-bond in FANG, out of the owner's allocation
    #[arg(long, default_value = "10000")]
    pub self_bond: String,
    /// The operator's name
    #[arg(long, default_value = "")]
    pub name: String,
    /// A reachable contact (repeatable)
    #[arg(long)]
    pub contact: Vec<String>,
    #[arg(long, default_value = "")]
    pub legal_entity: String,
    /// ISO 3166-1 alpha-2 of the entity ("DE")
    #[arg(long, default_value = "")]
    pub entity_jurisdiction: String,
    #[arg(long, default_value = "")]
    pub hosting_provider: String,
    /// The ASN announcing the p2p address
    #[arg(long, default_value_t = 0)]
    pub asn: u32,
    /// ISO 3166-1 alpha-2 of the data center ("FI")
    #[arg(long, default_value = "")]
    pub datacenter_country: String,
    /// The facility (data center)
    #[arg(long, default_value = "")]
    pub datacenter: String,
    /// This seat is run by the founder or the foundation
    #[arg(long)]
    pub founder_or_foundation: bool,
    /// JSON instead of TOML
    #[arg(long)]
    pub json: bool,
    /// Write here instead of stdout
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum GenesisCmd {
    /// Offline, on the operator's key machine: write this validator's public genesis entry
    /// (consensus key, owner, addresses, proof of possession, independence fields)
    Operator(Box<OperatorArgs>),
    /// Build genesis.json from a spec (byte-identical for the same spec and binary) and print its
    /// genesis hash
    Build {
        #[arg(long)]
        spec: PathBuf,
        /// Write genesis.json here (default: print it to stdout and the hash to stderr)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Refuse a spec that breaks the bucket table or the independence policy (always on
        /// for blockula-1)
        #[arg(long)]
        strict: bool,
    },
}

pub fn run(cmd: GenesisCmd) -> Result<()> {
    match cmd {
        GenesisCmd::Operator(args) => {
            let OperatorArgs { chain_id, key, owner, p2p_addr, http_addr, commission_bps, self_bond, name, contact, legal_entity, entity_jurisdiction, hosting_provider, asn, datacenter_country, datacenter, founder_or_foundation, json, out } = *args;
            let keys = crate::validator_cli::load_seed(&key)?;
            let independence = Independence { legal_entity, entity_jurisdiction, hosting_provider, asn, datacenter_country, datacenter, founder_or_foundation, ..Default::default() };
            let file = operator_file(&keys, &chain_id, &owner, &p2p_addr, &http_addr, commission_bps, &self_bond, OperatorInfo { name, contact }, independence)?;
            let text = if json { serde_json::to_string_pretty(&file)? + "\n" } else { toml::to_string(&file)? };
            match out {
                Some(path) => {
                    std::fs::write(&path, text)?;
                    eprintln!("wrote {} (public: consensus key, owner, addresses, proof of possession; no secrets)", path.display());
                }
                None => print!("{text}"),
            }
            Ok(())
        }
        GenesisCmd::Build { spec, out, strict } => {
            let built = build(&spec, strict)?;
            for w in &built.warnings {
                eprintln!("warning: {w}");
            }
            let g = &built.genesis;
            let summary = format!(
                "chain {} ({} validators, supply {}), state root {}\ngenesis hash: {}",
                g.chain_id,
                g.validators.len(),
                fmt_fang(g.allocations.iter().map(|a| a.amount).sum()),
                g.initial_state().root(),
                built.hash
            );
            match out {
                Some(path) => {
                    std::fs::write(&path, &built.json)?;
                    println!("wrote {}: {summary}", path.display());
                }
                None => {
                    print!("{}", built.json);
                    eprintln!("{summary}");
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Keys;

    const FANG: u64 = DROPS_PER_FANG;

    /// deploy/mainnet/genesis-spec.example.toml.
    fn example() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/mainnet/genesis-spec.example.toml")
    }

    /// One example operator: (seed byte, name, entity, provider, ASN, country, facility,
    /// founder's seat).
    type ExampleOperator = (u8, &'static str, &'static str, &'static str, u32, &'static str, &'static str, bool);

    /// The example spec's operators.
    const EXAMPLE_OPERATORS: [ExampleOperator; 4] = [
        (0xe1, "Founder validator A", "Blockula Labs", "Fly.io", 40_509, "US", "fly-iad", true),
        (0xe2, "Founder validator B", "Blockula Labs", "Fly.io", 40_509, "DE", "fly-fra", true),
        (0xe3, "Example Validators GmbH", "Example Validators GmbH", "OVHcloud", 16_276, "FR", "ovh-gra3", false),
        (0xe4, "Example Staking Ltd", "Example Staking Ltd", "Latitude.sh", 396_356, "BR", "latitude-sao2", false),
    ];

    fn owner(seed: u8, prefix: &str) -> String {
        Keys::from_seed([seed.wrapping_sub(0x10); 32]).address_with(prefix)
    }

    /// Regenerates the example's operator files (fresh proofs of possession) and prints the
    /// owner and placeholder addresses the spec uses:
    /// `cargo test --manifest-path node/Cargo.toml regenerate_example_operators -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn regenerate_example_operators() {
        let dir = example().parent().unwrap().join("operators");
        std::fs::create_dir_all(&dir).unwrap();
        for (i, (seed, name, entity, provider, asn, country, facility, founder)) in EXAMPLE_OPERATORS.into_iter().enumerate() {
            let owner = owner(seed, "tboo");
            let independence = Independence { legal_entity: entity.into(), entity_jurisdiction: country.into(), hosting_provider: provider.into(), asn, datacenter_country: country.into(), datacenter: facility.into(), founder_or_foundation: founder, statement: "Example operator for the dry-run spec: throwaway keys, not a real submission.".into(), ..Default::default() };
            let info = OperatorInfo { name: name.into(), contact: vec![format!("ops@operator-{}.example", i + 1)] };
            let f = operator_file(&Keys::from_seed([seed; 32]), "blockula-dryrun-1", &owner, &format!("203.0.113.{}:30300", i + 1), &format!("https://rpc{}.operator.example", i + 1), 500, "10000", info, independence).unwrap();
            std::fs::write(dir.join(format!("example-{}.toml", i + 1)), toml::to_string(&f).unwrap()).unwrap();
            println!("operator {}: owner {owner}", i + 1);
        }
        for p in ["founder", "security-grants"] {
            println!("placeholder {p}: {}", crypto::system_address("tboo", &format!("placeholder/{p}")));
        }
    }

    /// The example spec builds to the same bytes and hash every time, encodes DEC-5 exactly
    /// (buckets, keyless accounts, vesting curves), and loads as a genesis; its dry-run
    /// shortcuts are warnings, which `--strict` refuses.
    #[test]
    fn example_spec_builds_byte_identically() {
        let a = build(&example(), false).unwrap();
        let b = build(&example(), false).unwrap();
        assert_eq!((a.json.as_bytes(), &a.hash), (b.json.as_bytes(), &b.hash), "byte-identical");
        assert!(a.json.ends_with("}\n"));
        let g = &a.genesis;
        assert_eq!(a.hash, hex::encode(crate::consensus::genesis_block(g).digest_bytes()));
        // DEC-5: shares, keyless accounts, curves.
        let total: u64 = g.allocations.iter().map(|x| x.amount).sum();
        assert_eq!(total, MAX_SUPPLY);
        let held = |addr: &str| g.allocations.iter().filter(|x| x.address == addr).map(|x| x.amount).sum::<u64>();
        assert_eq!((held(&community("tboo")), held(&treasury("tboo")), held(&endowment("tboo")), held(&bootstrap_delegation("tboo"))), (320_000_000 * FANG, 300_000_000 * FANG, 150_000_000 * FANG, 100_000_000 * FANG));
        let curve = |label: &str| g.allocations.iter().find(|x| x.label.as_deref() == Some(label)).unwrap().vesting.clone();
        assert_eq!(curve("founder"), Some(Vesting { start: 0, cliff: 31_557_600, end: 126_230_400 }));
        assert_eq!(curve("security grants"), Some(Vesting { start: 0, cliff: 15_778_800, end: 63_115_200 }));
        assert_eq!(curve("treasury"), Some(Vesting { start: 0, cliff: 0, end: 126_230_400 }));
        assert_eq!(curve("endowment"), Some(Vesting { start: 0, cliff: 0, end: 315_576_000 }));
        assert_eq!(curve("community"), None);
        assert_eq!(curve("bootstrap"), Some(Vesting { start: 31_557_600, cliff: 31_557_600, end: 126_230_400 }), "returns to the treasury over years 2–4");
        // The founder's account and the founder's or foundation's seats get no bootstrap delegation.
        let founder_seats = EXAMPLE_OPERATORS.iter().filter(|o| o.7).map(|o| owner(o.0, "tboo"));
        let mut excluded: Vec<String> = founder_seats.chain(["founder"].map(|l| g.allocations.iter().find(|x| x.label.as_deref() == Some(l)).unwrap().address.clone())).collect();
        excluded.sort();
        assert_eq!(g.bootstrap_excluded, excluded);
        assert!(!g.faucet && g.validator_admins.is_empty() && g.validators.len() == 4 && g.operators.len() == 4);
        assert!(g.validators.windows(2).all(|w| w[0].address < w[1].address), "validators in canonical order");
        // It's a genesis like any other: the file loads, and its state holds the allocation.
        let dir = std::env::temp_dir().join(format!("blockula-genesis-example-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("genesis.json"), &a.json).unwrap();
        let loaded = Genesis::load(&dir.join("genesis.json")).unwrap();
        assert_eq!(hex::encode(crate::consensus::genesis_block(&loaded).digest_bytes()), a.hash);
        let mut st = loaded.initial_state();
        st.check_invariants().unwrap();
        assert_eq!(st.supply, MAX_SUPPLY);
        assert_eq!(crate::econ::transferable(&st, &owner(0xe3, "tboo"), 0), 0, "an operator's allocation is locked (its self-bond is bonded)");
        // Audit F12: the bootstrap bucket has a mover. At the first boundary it's staked with the
        // two seats not run by the founder; once its curve has run, it's all the treasury's.
        let boot = bootstrap_delegation("tboo");
        crate::staking::end_epoch(&mut st, 0);
        crate::staking::bootstrap_epoch(&mut st, 0);
        let staked: Vec<(String, u64)> = crate::staking::delegations_of(&st, &boot).into_iter().map(|(v, _)| (v.clone(), crate::staking::delegation_value(&st, &boot, &v))).collect();
        let independent: Vec<String> = g.operators.iter().filter(|o| !g.bootstrap_excluded.contains(&o.owner)).map(|o| crypto::address_with_prefix("tboo", &hex::decode(&o.consensus_key).unwrap())).collect();
        assert_eq!(staked.iter().map(|(v, _)| v.clone()).collect::<Vec<_>>(), independent);
        assert!(staked.iter().all(|(_, w)| *w == 50_000_000 * FANG), "{staked:?}");
        let treasury_before = st.account(&treasury("tboo")).balance;
        let past_curve = st.params.epoch_of(126_230_400) + 1;
        crate::staking::bootstrap_epoch(&mut st, past_curve);
        assert!(crate::staking::delegations_of(&st, &boot).is_empty() && st.account(&boot).balance == 0);
        assert_eq!(st.account(&treasury("tboo")).balance - treasury_before, 100_000_000 * FANG);
        std::fs::remove_dir_all(dir).ok();
        // The dry run's shortcuts.
        let w = a.warnings.join("\n");
        for expected in ["4 operators; a mainnet genesis needs at least 22", "hosting provider fly.io has 2 of the 4 operators; at most 1 may", "ASN 40509 has 2", "facility fly-iad has 1 of the 4 operators; at most 0 may"] {
            assert!(w.contains(expected), "missing warning {expected:?} in:\n{w}");
        }
        assert!(!w.contains("legal entity"), "the founder's two seats count against the founder cap, not the entity rule");
        let err = format!("{:#}", build(&example(), true).err().unwrap());
        assert!(err.contains("strict") && err.contains("at least 22"), "{err}");
    }

    /// `n` operators on distinct providers, ASNs, countries and facilities, each owning an
    /// allocation of 1M FANG from the operators bucket, on chain `chain_id`.
    fn operators(chain_id: &str, prefix: &str, n: u8) -> Vec<OperatorFile> {
        (0..n)
            .map(|i| {
                let independence = Independence { legal_entity: format!("Entity {i}"), entity_jurisdiction: "CH".into(), hosting_provider: format!("Provider {i}"), asn: 64_500 + i as u32, datacenter_country: format!("C{}", (b'A' + i) as char), datacenter: format!("dc-{i}"), ..Default::default() };
                operator_file(&Keys::from_seed([0x80 + i; 32]), chain_id, &Keys::from_seed([0x40 + i; 32]).address_with(prefix), &format!("198.51.100.{i}:30300"), &format!("https://v{i}.example"), 500, "10000", OperatorInfo { name: format!("op {i}"), contact: vec![] }, independence).unwrap()
            })
            .collect()
    }

    /// A DEC-5 spec around `ops` (the operators bucket split between them and a grant).
    fn spec_for(chain_id: &str, prefix: &str, ops: &[OperatorFile], extra: &str) -> Spec {
        let mut t = format!("chain_id = \"{chain_id}\"\naddress_prefix = \"{prefix}\"\ngenesis_time_ms = 1798761600000\nblock_time_ms = 1000\nepoch_blocks = 3600\ntotal_supply = \"1000000000\"\n{extra}\n");
        let alloc = |bucket: &str, address: Option<&str>, amount: u64| format!("[[allocation]]\nbucket = \"{bucket}\"\n{}amount = \"{amount}\"\n", address.map(|a| format!("address = \"{a}\"\n")).unwrap_or_default());
        for (b, amount) in [("community", 320_000_000), ("treasury", 300_000_000), ("endowment", 150_000_000), ("bootstrap", 100_000_000)] {
            t += &alloc(b, None, amount);
        }
        t += &alloc("founder", Some(&Keys::from_seed([0x30; 32]).address_with(prefix)), 100_000_000);
        for o in ops {
            t += &alloc("operators", Some(&o.keys.owner), 1_000_000);
        }
        t += &alloc("operators", Some(&Keys::from_seed([0x32; 32]).address_with(prefix)), 30_000_000 - ops.len() as u64 * 1_000_000);
        toml::from_str(&t).unwrap()
    }

    /// A testnet with a faucet funds its validators' consensus-key accounts liquid, outside the
    /// DEC-5 table (whose shares are of the rest); a chain without a faucet refuses it.
    #[test]
    fn faucet_allocations_fund_testnet_validators() {
        let ops = operators("blockula-testnet-9", "tboo", 22);
        let faucet = |addr: &str| format!("[[allocation]]\nbucket = \"faucet\"\naddress = \"{addr}\"\namount = \"5000000\"\n");
        let mut spec = spec_for("blockula-testnet-9", "tboo", &ops, "faucet = true");
        let extra: Vec<SpecAllocation> = ops.iter().take(2).flat_map(|o| toml::from_str::<Spec>(&format!("chain_id = \"x\"\naddress_prefix = \"tboo\"\ngenesis_time_ms = 1\nblock_time_ms = 1\nepoch_blocks = 2\ntotal_supply = \"1\"\n{}", faucet(&o.keys.consensus_address))).unwrap().allocations).collect();
        spec.allocations.extend(extra.clone());
        spec.total_supply = "1010000000".into();
        let built = build_from(&spec, &ops, false).unwrap();
        assert!(built.warnings.is_empty(), "the DEC-5 shares are of the supply without the faucet: {:?}", built.warnings);
        let funded = built.genesis.allocations.iter().find(|a| a.address == ops[0].keys.consensus_address).unwrap();
        assert_eq!((funded.amount, funded.vesting.clone(), funded.label.as_deref()), (5_000_000 * FANG, None, Some("faucet")));
        let st = built.genesis.initial_state();
        assert_eq!(crate::econ::transferable(&st, &ops[0].keys.consensus_address, 0), 5_000_000 * FANG, "liquid, for drips and fees");
        let mut no_faucet = spec_for("blockula-testnet-9", "tboo", &ops, "");
        no_faucet.allocations.extend(extra);
        no_faucet.total_supply = "1010000000".into();
        assert!(format!("{:#}", build_from(&no_faucet, &ops, false).err().unwrap()).contains("only a chain with a faucet"));
    }

    /// `[contribution]`: the pool opens at genesis with its repos, an opening balance out of the
    /// endowment (a finding: DEC-5 releases the endowment from zero, so strict refuses it) and
    /// the spec's timings; the Soul ledger seats founding jurors. All of it is in the genesis
    /// state, byte-identically, and each kind of mistake is refused.
    #[test]
    fn the_contribution_pool_opens_at_genesis() {
        let ops = operators("blockula-testnet-9", "tboo", 22);
        let jurors: Vec<String> = (0..3).map(|i| Keys::from_seed([0x60 + i; 32]).address_with("tboo")).collect();
        let souls: String = jurors.iter().map(|j| format!("[[soul]]\naccount = \"{j}\"\nsoul = \"25\"\nreason = \"founding juror (testnet)\"\n")).collect();
        let pool = "[contribution]\nopening_balance = \"100000\"\nseance_blocks = 259200\nclaim_maturity_blocks = 259200\n[[contribution.repo]]\nrepo = \"JackDishman/Blockula\"\nweight = 10\n";
        let with = |extra: &str, endowment: u64| {
            let mut spec: Spec = toml::from_str(&format!("{}{souls}{extra}", toml_of(&spec_for("blockula-testnet-9", "tboo", &ops, "")))).unwrap();
            spec.allocations[2].amount = endowment.to_string();
            spec
        };
        let spec = with(pool, 149_900_000);
        let a = build_from(&spec, &ops, false).unwrap();
        assert_eq!(a.json, build_from(&spec, &ops, false).unwrap().json, "byte-identical");
        assert_eq!(a.warnings, vec!["the contribution pool opens with 100000 FANG out of the endowment, which DEC-5 releases linearly from zero (docs/TOKENOMICS.md, section 1)".to_string()], "the endowment bucket still holds its 15%");
        let g = &a.genesis;
        assert_eq!(g.pool_repos, BTreeMap::from([("jackdishman/blockula".to_string(), 10)]));
        let times = g.contribution.clone().unwrap();
        let d = crate::poc::PocTimes::for_block_time(1000);
        assert_eq!(times, crate::poc::PocTimes { seance_blocks: 259_200, claim_maturity_blocks: 259_200, ..d }, "the rest keep their defaults");
        // In the state: the repo, the pool's balance (liquid), the endowment's curve, the Soul.
        let st = g.initial_state();
        st.check_invariants().unwrap();
        assert!(st.poc.is_active());
        assert_eq!(st.poc.repos.get("jackdishman/blockula"), Some(&10));
        let pool_addr = crate::poc::pool_address("tboo");
        assert_eq!((st.account(&pool_addr).balance, crate::econ::transferable(&st, &pool_addr, 0)), (100_000 * FANG, 100_000 * FANG));
        let endow = g.allocations.iter().find(|x| x.address == endowment("tboo")).unwrap();
        assert_eq!((endow.amount, endow.vesting.clone()), (149_900_000 * FANG, Some(Vesting { start: 0, cliff: 0, end: 315_576_000 })));
        assert_eq!(g.allocations.iter().find(|x| x.address == pool_addr).unwrap().label.as_deref(), Some(POOL_OPENING_LABEL));
        for j in &jurors {
            assert!(st.soul_of(j, 0) >= crate::poc::JUROR_MIN_SOUL && st.soul_of(j, 0) >= crate::poc::VOUCH_MIN_SOUL, "a founding juror can sit and vouch");
        }
        assert_eq!(st.params.poc, times);
        // Strict (and so blockula-1) refuses the opening balance; repos alone are fine there.
        assert!(format!("{:#}", build_from(&spec, &ops, true).err().unwrap()).contains("opens with 100000 FANG"));
        let main_ops = operators("blockula-1", "boo", 22);
        let main_repos: Spec = toml::from_str(&format!("{}[contribution]\n[[contribution.repo]]\nrepo = \"jackdishman/blockula\"\nweight = 10\n", toml_of(&spec_for("blockula-1", "boo", &main_ops, "")))).unwrap();
        let main = build_from(&main_repos, &main_ops, false).unwrap();
        assert!(main.warnings.is_empty() && main.genesis.pool_repos.len() == 1 && main.genesis.contribution.is_none(), "{:?}", main.warnings);
        // Without [contribution], nothing changes: no repos, no timings, no pool allocation.
        let plain = build_from(&spec_for("blockula-testnet-9", "tboo", &ops, ""), &ops, false).unwrap();
        assert!(plain.genesis.pool_repos.is_empty() && plain.genesis.contribution.is_none() && !plain.json.contains("pool_repos"));
        // Mistakes.
        let refuse = |spec: &Spec, why: &str| {
            let err = format!("{:#}", build_from(spec, &ops, false).err().unwrap_or_else(|| panic!("expected {why:?}")));
            assert!(err.contains(why), "expected {why:?}, got: {err}");
        };
        refuse(&with(pool, 150_000_000), "allocations add up to");
        refuse(&with(&pool.replace("weight = 10", "weight = 0"), 149_900_000), "needs a weight of 1–100");
        refuse(&with(&pool.replace("weight = 10", "weight = 101"), 149_900_000), "needs a weight of 1–100");
        refuse(&with(&pool.replace("JackDishman/Blockula", "not a repo"), 149_900_000), "pool_repos");
        refuse(&with(&format!("{pool}[[contribution.repo]]\nrepo = \"jackdishman/blockula\"\nweight = 3\n"), 149_900_000), "listed twice");
        refuse(&with(&pool.replace("\"100000\"", "\"0\""), 150_000_000), "opening_balance must be more than 0");
        refuse(&with(&pool.replace("seance_blocks = 259200", "seance_blocks = 2"), 149_900_000), "seance_blocks ≥ 3");
        assert!(toml::from_str::<Spec>(&format!("{}[contribution]\nwarp = 1\n", toml_of(&spec_for("x", "tboo", &ops, "")))).is_err(), "unknown fields are refused");
        // The founder's account starts with no Soul (TOKENOMICS: no jury seats while vesting
        // runs), so it's never a founding juror.
        let mut with = spec.clone();
        with.soul_ledger.push(SpecSoul { account: Keys::from_seed([0x30; 32]).address_with("tboo"), soul: "5".into(), reason: "juror".into() });
        refuse(&with, "is the founder's");
    }

    /// A spec as TOML again (the tests build specs as values, then add sections as text).
    fn toml_of(spec: &Spec) -> String {
        let mut t = format!("chain_id = \"{}\"\naddress_prefix = \"{}\"\ngenesis_time_ms = {}\nblock_time_ms = {}\nepoch_blocks = {}\nfaucet = {}\ntotal_supply = \"{}\"\n", spec.chain_id, spec.address_prefix, spec.genesis_time_ms, spec.block_time_ms, spec.epoch_blocks, spec.faucet, spec.total_supply);
        for a in &spec.allocations {
            t += &format!("[[allocation]]\nbucket = \"{}\"\n{}amount = \"{}\"\n", a.bucket, a.address.as_ref().map(|x| format!("address = \"{x}\"\n")).unwrap_or_default(), a.amount);
        }
        t
    }

    /// A mainnet-shaped spec (22 independent operators, DEC-5 exactly) passes strict, and so
    /// does `blockula-1` itself; each kind of mistake is refused with a reason.
    #[test]
    fn builds_validate_the_spec() {
        let ops = operators("blockula-1", "boo", 22);
        let main = build_from(&spec_for("blockula-1", "boo", &ops, ""), &ops, false).unwrap();
        assert!(main.warnings.is_empty(), "{:?}", main.warnings);
        assert_eq!(main.genesis.validators.len(), 22);
        // The operator files' order doesn't matter.
        let mut shuffled = ops.clone();
        shuffled.reverse();
        assert_eq!(build_from(&spec_for("blockula-1", "boo", &ops, ""), &shuffled, false).unwrap().json, main.json);

        let refuse = |spec: &Spec, ops: &[OperatorFile], why: &str| {
            let err = format!("{:#}", build_from(spec, ops, false).err().unwrap_or_else(|| panic!("expected an error containing {why:?}")));
            assert!(err.contains(why), "expected {why:?}, got: {err}");
        };
        // Supply and buckets.
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.allocations[0].amount = "319999999".into();
        refuse(&s, &ops, "allocations add up to");
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.allocations[0].amount = "319999999".into();
        s.allocations[1].amount = "300000001".into();
        refuse(&s, &ops, "bucket community holds");
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.allocations[0].bucket = "friends".into();
        refuse(&s, &ops, "unknown bucket");
        // DEC-5: the foundation gets 0%; its 5% is the treasury's.
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.allocations[4].amount = "50000000".into();
        s.allocations.insert(5, s.allocations[4].clone());
        (s.allocations[5].bucket, s.allocations[5].address) = ("foundation".into(), Some(Keys::from_seed([0x31; 32]).address_with("boo")));
        refuse(&s, &ops, "bucket foundation holds 50000000000000000 drops; DEC-5 gives it 0 (0% of the supply)");
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.allocations[0].address = Some(Keys::from_seed([1; 32]).address());
        refuse(&s, &ops, "keyless");
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.allocations[4].address = None;
        refuse(&s, &ops, "needs an `address`");
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.allocations[4].address = Some(Keys::from_seed([0x30; 32]).address_with("tboo"));
        refuse(&s, &ops, "isn't a boo1");
        let mut s = spec_for("blockula-1", "boo", &ops, "");
        s.total_supply = "999999999".into();
        refuse(&s, &ops, "supply is 1000000000 FANG");
        // Mainnet: no faucet, no admins, its own prefix.
        refuse(&spec_for("blockula-1", "boo", &ops, "faucet = true"), &ops, "no faucet");
        refuse(&spec_for("blockula-1", "boo", &ops, &format!("validator_admins = [\"{}\"]", Keys::from_seed([2; 32]).address())), &ops, "no admin keys");
        // Parameters within their bounds.
        refuse(&spec_for("blockula-1", "boo", &ops, "[params]\nmax_block_bytes = 99999999"), &ops, "max_block_bytes");
        refuse(&spec_for("blockula-1", "boo", &ops, "[params]\nwarp = 1"), &ops, "unknown parameter");
        assert_eq!(build_from(&spec_for("blockula-1", "boo", &ops, "[params]\nquorum_floor = \"30000000000000000\""), &ops, false).unwrap().genesis.param_overrides["quorum_floor"], 30_000_000 * FANG);
        // Operators: proofs of possession, keys, addresses, self-bonds.
        let spec = spec_for("blockula-1", "boo", &ops, "");
        let mut bad = ops.clone();
        bad[3].keys.pop = ops[4].keys.pop.clone();
        refuse(&spec, &bad, "pop doesn't verify");
        let other_chain = operators("blockula-dryrun-1", "boo", 22);
        refuse(&spec, &other_chain, "pop doesn't verify");
        let mut bad = ops.clone();
        bad[0].keys.consensus_address = ops[1].keys.consensus_address.clone();
        refuse(&spec, &bad, "consensus_address");
        let mut bad = ops.clone();
        bad[1].network.p2p_addr = ops[0].network.p2p_addr.clone();
        refuse(&spec, &bad, "p2p host");
        let mut bad = ops.clone();
        bad[2].staking.self_bond = "2000000".into();
        refuse(&spec, &bad, "allocations hold");
        let mut bad = ops.clone();
        bad[2].staking.self_bond = "10".into();
        refuse(&spec, &bad, "below min_self_bond");
        // Independence is policy: a warning on a test chain, an error on blockula-1 (or strict).
        let mut crowded = ops.clone();
        for o in crowded.iter_mut().take(6) {
            o.independence.hosting_provider = "Same Cloud".into();
        }
        crowded[7].independence.founder_or_foundation = true;
        crowded[8].independence.founder_or_foundation = true;
        crowded[9].independence.founder_or_foundation = true;
        refuse(&spec, &crowded, "hosting provider same cloud has 6 of the 22 operators; at most 5 may");
        let test_ops: Vec<OperatorFile> = operators("blockula-test-1", "tboo", 22).into_iter().zip(&crowded).map(|(mut o, c)| {
            o.independence = c.independence.clone();
            o
        }).collect();
        let test = build_from(&spec_for("blockula-test-1", "tboo", &test_ops, ""), &test_ops, false).unwrap();
        assert!(test.warnings.iter().any(|w| w.contains("same cloud")) && test.warnings.iter().any(|w| w.contains("3 seats are the founder's or the foundation's; at most 2")), "{:?}", test.warnings);
        assert!(build_from(&spec_for("blockula-test-1", "tboo", &test_ops, "strict = true"), &test_ops, false).is_err());
        assert!(build_from(&spec_for("blockula-test-1", "tboo", &test_ops, ""), &test_ops, true).is_err(), "--strict");
    }

    /// The independence rules on their own: ⌊n/4⌋ per provider, ASN and country, 15% per
    /// facility, one seat per entity, 2 founder seats, and every field filled in.
    #[test]
    fn independence_policy() {
        let op = |provider: &str, asn: u32, country: &str, dc: &str, entity: &str, founder: bool| OperatorFile {
            operator: OperatorInfo { name: entity.into(), contact: vec![] },
            keys: OperatorKeys { consensus_public_key: String::new(), consensus_address: String::new(), owner: String::new(), pop: String::new() },
            network: OperatorNetwork { p2p_addr: String::new(), http_addr: String::new() },
            staking: OperatorStaking { commission_bps: 0, self_bond: "0".into() },
            independence: Independence { legal_entity: entity.into(), entity_jurisdiction: "CH".into(), hosting_provider: provider.into(), asn, datacenter_country: country.into(), datacenter: dc.into(), founder_or_foundation: founder, ..Default::default() },
        };
        let mut set: Vec<OperatorFile> = (0..28).map(|i| op(&format!("p{i}"), 100 + i, &format!("c{i}"), &format!("dc{i}"), &format!("e{i}"), false)).collect();
        assert!(independence_findings(&set).is_empty());
        // 28 operators: 7 per provider/ASN/country is fine, 8 isn't; 4 per facility (15%) is fine, 5 isn't.
        for o in set.iter_mut().take(7) {
            o.independence.hosting_provider = "Big".into();
            o.independence.asn = 1;
            o.independence.datacenter_country = "DE".into();
        }
        for o in set.iter_mut().skip(10).take(4) {
            o.independence.datacenter = "shared".into();
        }
        assert!(independence_findings(&set).is_empty());
        set[20].independence.hosting_provider = "big".into();
        set[21].independence.asn = 1;
        set[22].independence.datacenter_country = "de".into();
        set[23].independence.datacenter = "Shared".into();
        set[24].independence.legal_entity = "E0".into();
        let f = independence_findings(&set).join("\n");
        for expected in ["hosting provider big has 8", "ASN 1 has 8", "data-center country de has 8", "facility shared has 5 of the 28 operators; at most 4", "legal entity e0 has 2"] {
            assert!(f.contains(expected), "missing {expected:?} in:\n{f}");
        }
        // Founder seats share an entity without breaking the entity rule; a third is too many.
        let mut set: Vec<OperatorFile> = (0..22).map(|i| op(&format!("p{i}"), 100 + i, &format!("c{i}"), &format!("dc{i}"), &format!("e{i}"), false)).collect();
        for o in set.iter_mut().take(2) {
            o.independence.legal_entity = "Foundation".into();
            o.independence.founder_or_foundation = true;
        }
        assert!(independence_findings(&set).is_empty());
        set[2].independence.founder_or_foundation = true;
        set[3].independence.datacenter = String::new();
        set[4].independence.asn = 0;
        let f = independence_findings(&set).join("\n");
        assert!(f.contains("3 seats are the founder's") && f.contains("leaves independence fields empty: datacenter") && f.contains("empty: asn"), "{f}");
        assert!(independence_findings(&set[..21]).join(" ").contains("21 operators; a mainnet genesis needs at least 22"));
    }

    /// The operator entry: TOML and JSON round trips, the PoP binds the chain, owner and
    /// addresses, and bad inputs are refused before anything is signed.
    #[test]
    fn operator_entries() {
        let k = Keys::from_seed([0x77; 32]);
        let owner = Keys::from_seed([0x78; 32]).address();
        let f = operator_file(&k, "blockula-1", &owner, "203.0.113.9:30300", "https://rpc.example", 700, "25000", OperatorInfo { name: "Op".into(), contact: vec!["a@b.example".into()] }, Independence::default()).unwrap();
        assert_eq!(toml::from_str::<OperatorFile>(&toml::to_string(&f).unwrap()).unwrap(), f);
        assert_eq!(serde_json::from_str::<OperatorFile>(&serde_json::to_string(&f).unwrap()).unwrap(), f);
        assert_eq!(f.keys.consensus_address, k.address());
        crate::chain::verify_pop("blockula-1", &owner, &f.keys.consensus_public_key, &f.network.p2p_addr, &f.network.http_addr, &f.keys.pop).unwrap();
        assert!(crate::chain::verify_pop("blockula-2", &owner, &f.keys.consensus_public_key, &f.network.p2p_addr, &f.network.http_addr, &f.keys.pop).is_err());
        let e = f.entry().unwrap();
        assert_eq!((e.self_bond, e.commission_bps, e.provider.clone(), e.asn), (25_000 * FANG, 700, None, None));
        let toml = toml::to_string(&f).unwrap().replace("[independence]", "[independence]\nextra = 1");
        assert!(toml::from_str::<OperatorFile>(&toml).is_err(), "unknown fields are refused");
        let bad = |owner: &str, p2p: &str, http: &str, bps: u64, bond: &str| operator_file(&k, "c", owner, p2p, http, bps, bond, OperatorInfo::default(), Independence::default()).is_err();
        assert!(bad("alice", "1.2.3.4:1", "https://x", 1, "1"));
        assert!(bad(&owner, "1.2.3.4", "https://x", 1, "1"));
        assert!(bad(&owner, "1.2.3.4:1", "ftp://x y", 1, "1"));
        assert!(bad(&owner, "1.2.3.4:1", "https://x", 10_001, "1"));
        assert!(bad(&owner, "1.2.3.4:1", "https://x", 1, "lots"));
        assert_eq!((parse_soul("12.5").unwrap(), parse_soul("3").unwrap()), (12_500, 3_000));
        assert!(parse_soul("1.2345").is_err() && parse_soul("-1").is_err());
    }
}
