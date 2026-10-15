//! Sync trust (CRY-2, MAINNET-DESIGN 3.10): the roots a node trusts before it has verified
//! anything, and how old they may be.
//!
//! A joining node checks everything it downloads against a validator set, and learns each set
//! from the one before it (handoff signatures, `epochs.rs`) back to genesis. That chain is only as
//! good as its roots: the genesis hash, and **checkpoints** `(height, block hash, validator-set
//! hash)` that the node already trusts. They come from two places:
//!
//! - **This binary**: `node/checkpoints/<chain id>.json`, compiled in, so the release signature
//!   covers them. Each release appends a recent checkpoint (OPERATIONS.md "Weak subjectivity").
//! - **The operator**: `join --genesis-hash` and `--checkpoint HEIGHT:HASH[:SET_HASH]`, saved in
//!   `join.json` and enforced on every start.
//!
//! `join` refuses a genesis that neither pins, and a node refuses every snapshot, epoch anchor and
//! sync batch that contradicts a checkpoint.
//!
//! **Weak subjectivity.** Keys of a set that has since unbonded can sign anything at no cost,
//! retroactive handoffs to a made-up set included, so a joiner whose newest root predates them can
//! be fed an alternate history. Stake stays slashable for the evidence window
//! (`evidence_max_age_epochs`, at most `unbonding_blocks`), so a joiner needs a checkpoint younger
//! than [`ws_period_blocks`]: half that window, leaving the other half to report a forgery and
//! slash it before the forgers' stake is gone. A pinned validator set makes anything a set before
//! it signed irrelevant: every later set must descend from the pinned one.

use crate::chain::{Genesis, Params};
use crate::epochs::{self, EpochRegistry};
use crate::params::ChainParams;
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The embedded trust files, one per known chain id. A new chain gets a file here (and a line).
const EMBEDDED: &[&str] = &[include_str!("../checkpoints/blockula-1.json")];

/// A block a node trusts without verifying it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub height: u64,
    /// The consensus block's digest (hex); at height 0, the genesis hash.
    pub hash: String,
    /// `epochs::set_hash` (hex) of the set that certifies block `height + 1`: the next epoch's set
    /// when `height` ends an epoch, otherwise the current one. Optional for operator checkpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_hash: Option<String>,
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && crate::crypto::is_lower_hex(s)
}

impl Checkpoint {
    /// `HEIGHT:HASH` or `HEIGHT:HASH:SET_HASH` (`--checkpoint`; hex in either case).
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.trim().split(':').collect();
        ensure!(matches!(parts.len(), 2 | 3), "a checkpoint is HEIGHT:HASH or HEIGHT:HASH:SET_HASH, got `{s}`");
        let height = parts[0].parse().with_context(|| format!("checkpoint height `{}`", parts[0]))?;
        let cp = Checkpoint { height, hash: parts[1].to_ascii_lowercase(), set_hash: parts.get(2).map(|h| h.to_ascii_lowercase()) };
        cp.check()?;
        Ok(cp)
    }

    fn check(&self) -> Result<()> {
        ensure!(is_hash(&self.hash), "checkpoint #{}: the hash must be 64 hex characters", self.height);
        ensure!(self.set_hash.as_deref().is_none_or(is_hash), "checkpoint #{}: the set hash must be 64 hex characters", self.height);
        Ok(())
    }

    /// The `--checkpoint` form.
    pub fn to_flag(&self) -> String {
        match &self.set_hash {
            Some(s) => format!("{}:{}:{s}", self.height, self.hash),
            None => format!("{}:{}", self.height, self.hash),
        }
    }
}

/// What this binary embeds for a known chain.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Known {
    pub chain_id: String,
    /// Empty until the chain launches: a placeholder, and the chain is refused.
    pub genesis_hash: String,
    #[serde(default)]
    pub checkpoints: Vec<Checkpoint>,
    /// Where the entries come from (or, for a placeholder, what it waits for). For people.
    #[serde(default)]
    #[cfg_attr(not(test), allow(dead_code))]
    pub note: String,
}

/// The embedded table. The files are part of the source, so a malformed one is a build bug
/// (caught by `embedded_files_parse_and_mainnet_waits_for_its_ceremony`).
pub fn embedded() -> &'static [Known] {
    static TABLE: OnceLock<Vec<Known>> = OnceLock::new();
    TABLE.get_or_init(|| EMBEDDED.iter().map(|s| serde_json::from_str(s).expect("node/checkpoints/*.json must parse")).collect())
}

/// What an operator pinned at `join` (saved in `join.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genesis_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoints: Vec<Checkpoint>,
    /// Start from an empty home even when the newest checkpoint is older than the period.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_stale_checkpoint: bool,
}

/// Weak-subjectivity period: half the evidence window (`evidence_max_age_epochs` epochs, at most
/// `unbonding_blocks`). A root older than this can't protect a joiner (module docs).
pub fn ws_period_blocks(cp: &ChainParams, epoch_blocks: u64) -> u64 {
    let evidence = cp.unbonding_blocks.min(cp.evidence_max_age_epochs.saturating_mul(epoch_blocks.max(1)));
    (evidence / 2).max(1)
}

/// `blocks` as days at `block_time_ms`, to one decimal (display only).
fn days(blocks: u64, block_time_ms: u64) -> String {
    let tenths = blocks as u128 * block_time_ms as u128 / 8_640_000;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// The genesis hash of `g` (hex digest of its genesis block).
pub fn genesis_hash(g: &Genesis) -> String {
    hex::encode(crate::consensus::genesis_block(g).digest_bytes())
}

/// The checkpoint at height 0: the genesis hash and the genesis set.
pub fn genesis_checkpoint(g: &Genesis) -> Checkpoint {
    let set = EpochRegistry::genesis(g).sets.remove(&0).unwrap_or_default();
    Checkpoint { height: 0, hash: genesis_hash(g), set_hash: Some(hex::encode(epochs::set_hash(&set))) }
}

/// Everything a node trusts for its chain: the genesis hash and the checkpoints, embedded or
/// pinned by its operator, checked against each other.
#[derive(Clone, Debug)]
pub struct Trust {
    pub genesis_hash: String,
    checkpoints: BTreeMap<u64, Checkpoint>,
    params: Params,
    ws_blocks: u64,
    expected_from: (u64, u64),
    allow_stale: bool,
}

impl Trust {
    /// Trust for `g`: what this binary embeds for its chain id plus `pins`. A known chain's genesis
    /// must hash to the embedded value (a placeholder refuses the chain); `--genesis-hash` must
    /// match; with `require_pin` (a `join`), an unknown chain needs `--genesis-hash`.
    pub fn new(g: &Genesis, pins: &Pins, require_pin: bool) -> Result<Self> {
        Self::with_table(g, pins, require_pin, embedded())
    }

    fn with_table(g: &Genesis, pins: &Pins, require_pin: bool, table: &[Known]) -> Result<Self> {
        let hash = genesis_hash(g);
        let known = table.iter().find(|k| k.chain_id == g.chain_id);
        if let Some(k) = known {
            // TODO(ceremony): blockula-1's entry is a placeholder until the genesis ceremony; the
            // release that launches mainnet fills it in (node/checkpoints/blockula-1.json).
            ensure!(
                !k.genesis_hash.is_empty(),
                "this build doesn't know `{}`'s genesis yet, so it refuses that chain: it hasn't launched, or this binary predates its launch. Install the release that launched it.",
                k.chain_id
            );
            ensure!(k.genesis_hash == hash, "this genesis for `{}` hashes to {hash}, but this release knows `{}` as {}: a wrong or tampered genesis", g.chain_id, k.chain_id, k.genesis_hash);
        }
        match &pins.genesis_hash {
            Some(want) if want.to_ascii_lowercase() != hash => bail!("genesis hash mismatch: expected {want}, got {hash} — wrong chain or tampered genesis"),
            Some(_) => {}
            None if known.is_none() && require_pin => bail!(
                "refusing to join `{}` without a pinned genesis: pass --genesis-hash. This genesis hashes to {hash}; compare that with the hash the chain's operators publish (`blockula summon` prints it, every node shows it in /api/status). A genesis is only as trustworthy as the source of its hash.",
                g.chain_id
            ),
            None => {}
        }
        let params = g.params();
        let cp = g.chain_params().unwrap_or_else(|_| ChainParams::defaults(&params));
        let mut trust = Trust { genesis_hash: hash, checkpoints: BTreeMap::new(), ws_blocks: ws_period_blocks(&cp, params.epoch_blocks), params, expected_from: (g.created_at_ms, g.block_time_ms), allow_stale: pins.allow_stale_checkpoint };
        let genesis = genesis_checkpoint(g);
        for c in known.map(|k| k.checkpoints.as_slice()).unwrap_or_default().iter().chain(&pins.checkpoints) {
            c.check()?;
            if c.height == 0 {
                ensure!(c.hash == genesis.hash, "checkpoint #0 must be the genesis hash {}, got {}", genesis.hash, c.hash);
                ensure!(c.set_hash.is_none() || c.set_hash == genesis.set_hash, "checkpoint #0's set hash must be the genesis set's, {}", genesis.set_hash.as_deref().unwrap_or_default());
            }
            trust.add(c.clone())?;
        }
        Ok(trust)
    }

    /// Adds a checkpoint; two at one height must agree (a set hash fills in a missing one).
    fn add(&mut self, c: Checkpoint) -> Result<()> {
        match self.checkpoints.get_mut(&c.height) {
            None => {
                self.checkpoints.insert(c.height, c);
            }
            Some(have) => {
                let sets_differ = matches!((&have.set_hash, &c.set_hash), (Some(a), Some(b)) if a != b);
                ensure!(have.hash == c.hash && !sets_differ, "conflicting checkpoints at #{}: {} and {}", c.height, have.to_flag(), c.to_flag());
                if have.set_hash.is_none() {
                    have.set_hash = c.set_hash;
                }
            }
        }
        Ok(())
    }

    /// Every checkpoint, oldest first.
    pub fn checkpoints(&self) -> impl Iterator<Item = &Checkpoint> {
        self.checkpoints.values()
    }

    /// Height of the newest checkpoint (0: only the genesis).
    pub fn newest(&self) -> u64 {
        self.checkpoints.keys().next_back().copied().unwrap_or(0)
    }

    /// The height the chain would have reached at `now_ms` at its nominal block time. Missed
    /// slots only make the real height lower, so ages measured from it err towards "stale".
    pub fn expected_height(&self, now_ms: u64) -> u64 {
        let (created, block_time) = self.expected_from;
        now_ms.saturating_sub(created) / block_time.max(1)
    }

    /// The epoch of `height` on this chain.
    pub fn epoch_of(&self, height: u64) -> u64 {
        self.params.epoch_of(height)
    }

    /// The epoch whose validator set `c.set_hash` pins (the one certifying block `height + 1`).
    pub fn pinned_epoch(&self, c: &Checkpoint) -> u64 {
        self.params.epoch_of(c.height.saturating_add(1))
    }

    /// Block `height` must have `hash` if a checkpoint names that height.
    pub fn check_block(&self, height: u64, hash: &str) -> Result<()> {
        match self.checkpoints.get(&height) {
            Some(c) if c.hash != hash => bail!("block #{height} is {hash}, but the trusted checkpoint says {}: this peer serves another chain", c.hash),
            _ => Ok(()),
        }
    }

    /// The set that certifies `epoch`'s blocks (consensus keys, any order) must hash to what every
    /// checkpoint pinning that epoch says.
    pub fn check_set(&self, epoch: u64, keys: &[String]) -> Result<()> {
        let mut sorted = keys.to_vec();
        sorted.sort();
        let have = hex::encode(epochs::set_hash(&sorted));
        for c in self.checkpoints.values().filter(|c| self.pinned_epoch(c) == epoch) {
            if let Some(want) = &c.set_hash {
                ensure!(*want == have, "the validator set for epoch {epoch} hashes to {have}, but checkpoint #{} pins {want}: a forged validator-set history", c.height);
            }
        }
        Ok(())
    }

    /// Why a node whose newest verified block is `own_height` can't rely on what it trusts at
    /// `now_ms`, if it can't: its newest root (checkpoint or own block) is older than the
    /// weak-subjectivity period, measured against the chain's expected height.
    pub fn stale(&self, own_height: u64, now_ms: u64) -> Option<String> {
        let block_time = self.expected_from.1;
        let expected = self.expected_height(now_ms);
        let newest = self.newest().max(own_height);
        let age = expected.saturating_sub(newest);
        (age > self.ws_blocks).then(|| {
            format!(
                "the newest block this node trusts is #{newest}, about {age} blocks (~{} days) behind the chain's expected height #{expected}: older than the weak-subjectivity period ({} blocks, ~{} days). Keys of validators that have since unbonded could forge a history from there. Pass a recent --checkpoint HEIGHT:HASH:SET_HASH from sources you trust (`blockula checkpoint --peer … --peer …`, OPERATIONS.md \"Weak subjectivity\")",
                days(age, block_time),
                self.ws_blocks,
                days(self.ws_blocks, block_time)
            )
        })
    }

    /// [`Trust::stale`] for a node about to sync from `own_height`: an error for an empty home
    /// (`own_height` 0) unless the operator allowed stale checkpoints, otherwise the warning.
    pub fn check_fresh(&self, own_height: u64, now_ms: u64) -> Result<Option<String>> {
        match self.stale(own_height, now_ms) {
            Some(why) if own_height == 0 && !self.allow_stale => Err(anyhow!("{why}, or pass --allow-stale-checkpoint to accept the risk")),
            other => Ok(other),
        }
    }
}

/// `blockula checkpoint`: the checkpoint at `height` (default: the newest epoch boundary every
/// peer has finalized) that all `peers` agree on. Each peer's genesis must match the others' and
/// its validator-set history must verify from that genesis (`epochs::verify_chain`). Ask nodes run
/// by independent operators: agreement among them is what a checkpoint attests.
pub async fn fetch(peers: &[String], height: Option<u64>) -> Result<(Genesis, Checkpoint)> {
    ensure!(!peers.is_empty(), "name at least one --peer (several independent ones)");
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let get = |url: String| {
        let client = client.clone();
        async move {
            let body = crate::node::get_capped(&client, &url, 64 << 20).await.with_context(|| url.clone())?;
            serde_json::from_slice::<serde_json::Value>(&body).with_context(|| url.clone())
        }
    };
    let urls: Vec<String> = peers.iter().map(|p| crate::chain::http_url(p).trim_end_matches('/').to_string()).collect();
    let mut genesis: Option<Genesis> = None;
    let mut finalized = u64::MAX;
    for u in &urls {
        let g: Genesis = serde_json::from_value(get(format!("{u}/api/genesis")).await?).with_context(|| format!("{u}/api/genesis"))?;
        let status = get(format!("{u}/api/status")).await?;
        finalized = finalized.min(status["finalized_height"].as_u64().ok_or_else(|| anyhow!("{u}/api/status has no finalized_height"))?);
        match &genesis {
            Some(first) if genesis_hash(first) != genesis_hash(&g) => bail!("{u} serves another genesis ({}) than {} ({})", genesis_hash(&g), urls[0], genesis_hash(first)),
            Some(_) => {}
            None => genesis = Some(g),
        }
    }
    let g = genesis.expect("at least one peer");
    let params = g.params();
    let height = match height {
        Some(h) => {
            ensure!(h <= finalized, "#{h} isn't finalized on every peer yet (the lowest is at #{finalized})");
            h
        }
        None => match params.epoch_of(finalized.saturating_add(1)) {
            0 => 0,
            e => params.last_of(e - 1),
        },
    };
    if height == 0 {
        return Ok((g.clone(), genesis_checkpoint(&g)));
    }
    let epoch = params.epoch_of(height.saturating_add(1));
    let mut agreed: Option<(String, Checkpoint)> = None;
    for u in &urls {
        let block = get(format!("{u}/api/block/{height}")).await?;
        let hash = block["hash"].as_str().ok_or_else(|| anyhow!("{u}/api/block/{height} has no hash"))?.to_string();
        let reg: EpochRegistry = serde_json::from_value(get(format!("{u}/api/epochs")).await?).with_context(|| format!("{u}/api/epochs"))?;
        let set = epochs::verify_chain(&g, &reg, epoch).with_context(|| format!("{u}: validator-set history through epoch {epoch} (just after a boundary, its handoff signatures may still be arriving; retry, or pass an older --height)"))?;
        let cp = Checkpoint { height, hash, set_hash: Some(hex::encode(epochs::set_hash(&set))) };
        match &agreed {
            Some((first, c)) if *c != cp => bail!("peers disagree at #{height}: {first} says {}, {u} says {}", c.to_flag(), cp.to_flag()),
            Some(_) => {}
            None => agreed = Some((u.clone(), cp)),
        }
    }
    Ok((g, agreed.expect("at least one peer").1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{Chain, SummonOpts};

    fn summoned(tag: &str, n: usize) -> (std::path::PathBuf, Vec<std::path::PathBuf>, Genesis) {
        summoned_with(SummonOpts::new(tag, n))
    }

    fn summoned_with(opts: SummonOpts) -> (std::path::PathBuf, Vec<std::path::PathBuf>, Genesis) {
        let root = std::env::temp_dir().join(format!("blockula-trust-{}-{}-{}", opts.chain_id, crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &opts).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        (root, homes, g)
    }

    fn known(g: &Genesis, hash: &str, checkpoints: Vec<Checkpoint>) -> Vec<Known> {
        vec![Known { chain_id: g.chain_id.clone(), genesis_hash: hash.into(), checkpoints, note: String::new() }]
    }

    #[test]
    fn embedded_files_parse_and_mainnet_waits_for_its_ceremony() {
        assert_eq!(embedded().len(), EMBEDDED.len());
        for k in embedded() {
            assert!(k.genesis_hash.is_empty() || is_hash(&k.genesis_hash), "{}: genesis_hash", k.chain_id);
            for c in &k.checkpoints {
                c.check().unwrap();
                assert!(c.set_hash.is_some(), "{} #{}: embedded checkpoints pin a validator set", k.chain_id, c.height);
            }
        }
        let main = embedded().iter().find(|k| k.chain_id == crate::chain::MAINNET_CHAIN_ID).expect("blockula-1 has an entry");
        // TODO(ceremony): when the launch release fills the hash in, this flips: delete it then.
        assert!(main.genesis_hash.is_empty() && main.note.contains("TODO(ceremony)"));
        let (root, _, mut g) = summoned("main", 1);
        g.chain_id = crate::chain::MAINNET_CHAIN_ID.into();
        let pins = Pins { genesis_hash: Some(genesis_hash(&g)), ..Default::default() };
        for (p, require) in [(&pins, true), (&Pins::default(), false)] {
            let err = Trust::new(&g, p, require).unwrap_err().to_string();
            assert!(err.contains("doesn't know `blockula-1`'s genesis yet"), "{err}");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_genesis_must_be_pinned_or_known() {
        let (root, _, g) = summoned("pin", 1);
        let hash = genesis_hash(&g);
        // Unknown chain: `join` needs --genesis-hash, and it must match.
        assert!(Trust::new(&g, &Pins::default(), true).unwrap_err().to_string().contains(&format!("hashes to {hash}")));
        assert!(Trust::new(&g, &Pins { genesis_hash: Some("00".repeat(32)), ..Default::default() }, true).unwrap_err().to_string().contains("mismatch"));
        let t = Trust::new(&g, &Pins { genesis_hash: Some(hash.to_uppercase()), ..Default::default() }, true).unwrap();
        assert_eq!((t.genesis_hash.as_str(), t.newest()), (hash.as_str(), 0));
        // A home that joined already (`run`) or a validator doesn't need the pin again.
        Trust::new(&g, &Pins::default(), false).unwrap();
        // A known chain needs no pin, but its genesis must be the embedded one.
        Trust::with_table(&g, &Pins::default(), true, &known(&g, &hash, vec![])).unwrap();
        let err = Trust::with_table(&g, &Pins::default(), false, &known(&g, &"ab".repeat(32), vec![])).unwrap_err().to_string();
        assert!(err.contains("tampered"), "{err}");
        assert!(Trust::with_table(&g, &Pins { genesis_hash: Some("00".repeat(32)), ..Default::default() }, true, &known(&g, &hash, vec![])).is_err(), "the operator's pin still applies");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn checkpoints_parse_merge_and_check_blocks_and_sets() {
        let (root, _, g) = summoned("cps", 4);
        let hash = genesis_hash(&g);
        let (a, b) = ("aa".repeat(32), "bb".repeat(32));
        assert_eq!(Checkpoint::parse(&format!("7:{}", a.to_uppercase())).unwrap(), Checkpoint { height: 7, hash: a.clone(), set_hash: None });
        assert_eq!(Checkpoint::parse(&format!("7:{a}:{b}")).unwrap().to_flag(), format!("7:{a}:{b}"));
        for bad in ["7", &format!("x:{a}"), "7:abc", &format!("7:{a}:zz"), &format!("7:{a}:{b}:{b}"), &format!("-1:{a}")] {
            assert!(Checkpoint::parse(bad).is_err(), "{bad}");
        }
        let pins = |cps: Vec<Checkpoint>| Pins { genesis_hash: Some(hash.clone()), checkpoints: cps, ..Default::default() };
        let cp = |h: u64, hash: &str, set: Option<&str>| Checkpoint { height: h, hash: hash.into(), set_hash: set.map(str::to_string) };
        // Height 0 is the genesis: its hash and set.
        let genesis = genesis_checkpoint(&g);
        Trust::new(&g, &pins(vec![genesis.clone()]), true).unwrap();
        assert!(Trust::new(&g, &pins(vec![cp(0, &a, None)]), true).unwrap_err().to_string().contains("genesis hash"));
        assert!(Trust::new(&g, &pins(vec![cp(0, &hash, Some(&b))]), true).unwrap_err().to_string().contains("genesis set"));
        // Embedded and operator checkpoints merge; at one height they must agree.
        let table = known(&g, &hash, vec![cp(700, &a, Some(&b))]);
        let t = Trust::with_table(&g, &pins(vec![cp(700, &a, None), cp(900, &b, None)]), true, &table).unwrap();
        assert_eq!(t.checkpoints().map(Checkpoint::to_flag).collect::<Vec<_>>(), vec![format!("700:{a}:{b}"), format!("900:{b}")]);
        assert_eq!(t.newest(), 900);
        assert!(Trust::with_table(&g, &pins(vec![cp(700, &b, None)]), true, &table).unwrap_err().to_string().contains("conflicting"));
        assert!(Trust::with_table(&g, &pins(vec![cp(700, &a, Some(&a))]), true, &table).unwrap_err().to_string().contains("conflicting"));
        // Blocks: only a checkpoint's height is constrained.
        t.check_block(700, &a).unwrap();
        t.check_block(701, &b).unwrap();
        assert!(t.check_block(700, &b).unwrap_err().to_string().contains("another chain"));
        // Sets: 700 is mid-epoch (600-block epochs), so it pins epoch 1's set.
        assert_eq!(t.pinned_epoch(t.checkpoints().next().unwrap()), 1);
        let genesis_keys = EpochRegistry::genesis(&g).sets[&0].clone();
        let pinned = Trust::new(&g, &pins(vec![cp(700, &a, genesis.set_hash.as_deref())]), true).unwrap();
        let mut shuffled = genesis_keys.clone();
        shuffled.reverse();
        pinned.check_set(1, &shuffled).unwrap();
        pinned.check_set(2, &genesis_keys[..1]).unwrap();
        assert!(pinned.check_set(1, &genesis_keys[..3]).unwrap_err().to_string().contains("forged"));
        // At an epoch's last block, the next epoch's set.
        let boundary = Trust::new(&g, &pins(vec![cp(1199, &a, Some(&b))]), true).unwrap();
        assert_eq!(boundary.pinned_epoch(boundary.checkpoints().next().unwrap()), 2);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn stale_checkpoints_are_refused_for_empty_homes() {
        let (root, _, mut g) = summoned("stale", 1);
        let day = 86_400_000 / g.block_time_ms;
        let ws = ws_period_blocks(&g.chain_params().unwrap(), g.epoch_blocks);
        assert_eq!(ws, 21 * day / 2, "half of 21 days of unbonding");
        assert_eq!(ws_period_blocks(&ChainParams::default(), 3_600), 907_200, "mainnet: 10.5 days of 1 s blocks");
        let created = g.created_at_ms;
        let now = |days: u64| created + days * 86_400_000;
        let hash = genesis_hash(&g);
        let pins = Pins { genesis_hash: Some(hash.clone()), ..Default::default() };
        // A young chain: the genesis itself is recent enough.
        let t = Trust::new(&g, &pins, true).unwrap();
        assert_eq!(t.expected_height(now(3)), 3 * day);
        assert_eq!(t.check_fresh(0, now(10)).unwrap(), None);
        // Three weeks in, the genesis alone is stale: refused for an empty home, a warning otherwise.
        let err = t.check_fresh(0, now(21)).unwrap_err().to_string();
        assert!(err.contains("weak-subjectivity period") && err.contains("~21.0 days"), "{err}");
        assert!(t.check_fresh(20 * day, now(21)).unwrap().is_none(), "the node's own recent head counts");
        assert!(t.check_fresh(day, now(21)).unwrap().unwrap().contains("--checkpoint"));
        // A recent checkpoint, or the operator's explicit choice, lets it start.
        let fresh = Pins { checkpoints: vec![Checkpoint { height: 18 * day, hash: "cd".repeat(32), set_hash: None }], ..pins.clone() };
        assert_eq!(Trust::new(&g, &fresh, true).unwrap().check_fresh(0, now(21)).unwrap(), None);
        let allowed = Pins { allow_stale_checkpoint: true, ..pins.clone() };
        assert!(Trust::new(&g, &allowed, true).unwrap().check_fresh(0, now(21)).unwrap().is_some());
        // Slower than nominal blocks only make it look older (never fresher).
        g.created_at_ms = created - 30 * 86_400_000;
        assert!(Trust::new(&g, &Pins { genesis_hash: Some(genesis_hash(&g)), ..Default::default() }, true).unwrap().check_fresh(0, now(0)).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// `blockula checkpoint` reads several nodes and insists they agree.
    #[tokio::test(flavor = "multi_thread")]
    async fn checkpoint_command_cross_checks_peers() {
        use axum::{routing::get, Json, Router};
        let (root, homes, g) = summoned_with(SummonOpts { epoch_blocks: 10, ..SummonOpts::new("fetch", 1) });
        let chain = std::sync::Arc::new(parking_lot::Mutex::new(Chain::open(&homes[0]).unwrap()));
        let epoch_blocks = g.epoch_blocks;
        for _ in 0..epoch_blocks + 5 {
            chain.lock().produce_block().unwrap();
        }
        let node = crate::api::Node::new(chain.clone(), None, vec![], Default::default());
        let honest = crate::api::router(node).into_make_service_with_connect_info::<std::net::SocketAddr>();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, honest).await.unwrap() });
        // Default: the newest epoch boundary every peer finalized, with the next epoch's set.
        let boundary = epoch_blocks - 1;
        let (got, cp) = fetch(&[url.clone(), url.clone()], None).await.unwrap();
        assert_eq!(genesis_hash(&got), genesis_hash(&g));
        assert_eq!(cp.height, boundary);
        assert_eq!(Some(cp.hash.clone()), chain.lock().hash_at(boundary));
        assert_eq!(cp.set_hash, genesis_checkpoint(&g).set_hash, "the set didn't change");
        assert_eq!(fetch(std::slice::from_ref(&url), Some(3)).await.unwrap().1.hash, chain.lock().hash_at(3).unwrap());
        assert_eq!(fetch(std::slice::from_ref(&url), Some(0)).await.unwrap().1, genesis_checkpoint(&g));
        assert!(fetch(std::slice::from_ref(&url), Some(epoch_blocks + 100)).await.unwrap_err().to_string().contains("isn't finalized"));
        // A peer that serves another block at that height is called out.
        let real = url.clone();
        let liar = Router::new()
            .route("/api/block/{h}", get(|| async { Json(serde_json::json!({ "hash": "ee".repeat(32) })) }))
            .fallback(move |req: axum::extract::Request| {
                let real = real.clone();
                async move { reqwest::get(format!("{real}{}", req.uri())).await.unwrap().text().await.unwrap() }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let liar_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, liar).await.unwrap() });
        let err = fetch(&[url.clone(), liar_url], Some(3)).await.unwrap_err().to_string();
        assert!(err.contains("disagree"), "{err}");
        drop(chain);
        std::fs::remove_dir_all(root).ok();
    }
}
