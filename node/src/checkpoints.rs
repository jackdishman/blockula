//! Sync trust (CRY-2, MAINNET-DESIGN 3.10): the roots a node trusts before it has verified
//! anything, and how old they may be.
//!
//! A joining node checks everything it downloads against a validator set, and learns each set
//! from the one before it (handoff signatures, `epochs.rs`) back to genesis. That chain is only as
//! good as its roots: the genesis hash, and **checkpoints** `(height, block hash, validator-set
//! hash, block time)` that the node already trusts. They come from two places:
//!
//! - **This binary**: `node/checkpoints/<chain id>.json`, compiled in, so the release signature
//!   covers them. Each release appends a recent checkpoint (OPERATIONS.md "Weak subjectivity").
//! - **The operator**: `join --genesis-hash` and
//!   `--checkpoint HEIGHT:HASH[:SET_HASH[:TIMESTAMP_MS]]`, saved in `join.json` and enforced on
//!   every start.
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
//!
//! Both parameters are governance's, so the period a node enforces is the shortest it knows of
//! (gap-6-1): the genesis parameters', the one this release recorded with its newest checkpoint
//! (`Known::weak_subjectivity_blocks`), for a node that has executed the chain the one its own
//! state's parameters set ([`Trust::follow`]), and for an empty home the shortest any of its peers
//! reports in `/api/status` ([`peer_periods`], [`Trust::heed`]): a joiner has no state it can
//! trust yet, so a shortening after its release reaches it only that way, and one honest peer is
//! enough. A peer can only shorten the period, and never below the protocol's floor
//! (`params::EVIDENCE_WINDOW_FLOOR_DAYS`: governance can't shorten the evidence window below two
//! weeks, so the period never falls below a week), so a lying peer can at worst ask for a
//! checkpoint a week old.
//!
//! A root's age is measured in blocks against the highest the chain can have reached by the wall
//! clock, counted from the newest block whose time the node trusts ([`Trust::expected_height`]):
//! the newest checkpoint that records its block's time, or the genesis. Counted from the genesis
//! alone, every slot the chain ever missed would make even a checkpoint at the tip look older
//! (review-r3-sync-1), and an old chain would refuse every checkpoint once the deficit passed the
//! period; counted from the checkpoint's own time, only slots missed since then count.

use crate::chain::{Genesis, Params};
use crate::epochs::{self, EpochRegistry};
use crate::params::{evidence_floor_epochs, ChainParams};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The embedded trust files, one per known chain id. A new chain gets a file here (and a line).
const EMBEDDED: &[&str] = &[include_str!("../checkpoints/blockula-1.json"), include_str!("../checkpoints/blockula-testnet-2.json")];

/// A block a node trusts without verifying it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub height: u64,
    /// The consensus block's digest (hex); at height 0, the genesis hash.
    pub hash: String,
    /// `epochs::set_hash` (hex) of the set that certifies block `height + 1`: the next epoch's set
    /// when `height` ends an epoch, otherwise the current one. Optional for operator checkpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_hash: Option<String>,
    /// The block's `timestamp_ms` (its hash covers it; at height 0, the genesis' `created_at_ms`).
    /// With it, a root's age is measured from its own time, so slots the chain missed before it
    /// don't count ([`Trust::expected_height`], review-r3-sync-1), and a block at this height
    /// carrying another time is refused like another hash. Optional (older checkpoints have none);
    /// `blockula checkpoint` prints it, and embedded checkpoints carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && crate::crypto::is_lower_hex(s)
}

impl Checkpoint {
    /// `HEIGHT:HASH`, `HEIGHT:HASH:SET_HASH` or `HEIGHT:HASH:SET_HASH:TIMESTAMP_MS` (`--checkpoint`;
    /// hex in any case; the last form may leave SET_HASH empty).
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.trim().split(':').collect();
        ensure!(matches!(parts.len(), 2..=4), "a checkpoint is HEIGHT:HASH, HEIGHT:HASH:SET_HASH or HEIGHT:HASH:SET_HASH:TIMESTAMP_MS (what `blockula checkpoint` prints), got `{s}`");
        let height = parts[0].parse().with_context(|| format!("checkpoint height `{}`", parts[0]))?;
        let set_hash = match parts.get(2) {
            Some(h) if h.is_empty() && parts.len() == 4 => None,
            other => other.map(|h| h.to_ascii_lowercase()),
        };
        let timestamp_ms = parts.get(3).map(|t| t.parse().with_context(|| format!("checkpoint #{height}: the timestamp `{t}` must be the block's timestamp_ms (milliseconds since 1970)"))).transpose()?;
        let cp = Checkpoint { height, hash: parts[1].to_ascii_lowercase(), set_hash, timestamp_ms };
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
        match (&self.set_hash, self.timestamp_ms) {
            (Some(s), Some(t)) => format!("{}:{}:{s}:{t}", self.height, self.hash),
            (None, Some(t)) => format!("{}:{}::{t}", self.height, self.hash),
            (Some(s), None) => format!("{}:{}:{s}", self.height, self.hash),
            (None, None) => format!("{}:{}", self.height, self.hash),
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
    /// The weak-subjectivity period in force when the newest checkpoint was taken (`/api/status`
    /// `weak_subjectivity_blocks`), when governance has shortened it below the genesis one: this
    /// release's nodes enforce the shorter of the two (gap-6-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weak_subjectivity_blocks: Option<u64>,
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

/// The shortest weak-subjectivity period a chain that started with `genesis` can ever have: the
/// evidence window at governance's floor (`ChainParams::governance_band`), or the genesis' own
/// when it starts below that (it then only moves up). `unbonding_blocks` can't be shorter than
/// the window, so the window alone sets it.
pub fn ws_floor_blocks(genesis: &ChainParams, p: &Params) -> u64 {
    let lowest = ChainParams { evidence_max_age_epochs: genesis.evidence_max_age_epochs.min(evidence_floor_epochs(p)), ..genesis.clone() };
    ws_period_blocks(&lowest, p.epoch_blocks)
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

/// The checkpoint at height 0: the genesis hash, set and time.
pub fn genesis_checkpoint(g: &Genesis) -> Checkpoint {
    let set = EpochRegistry::genesis(g).sets.remove(&0).unwrap_or_default();
    Checkpoint { height: 0, hash: genesis_hash(g), set_hash: Some(hex::encode(epochs::set_hash(&set))), timestamp_ms: Some(g.created_at_ms) }
}

/// The earliest a block at `height` can carry, given one at `from` that carries `time`: consensus
/// refuses a block less than `block_time` after its parent (`consensus::App::verify`).
fn earliest(from: (u64, u64), height: u64, block_time: u64) -> u128 {
    from.1 as u128 + height.saturating_sub(from.0) as u128 * block_time as u128
}

/// Everything a node trusts for its chain: the genesis hash and the checkpoints, embedded or
/// pinned by its operator, checked against each other.
#[derive(Clone, Debug)]
pub struct Trust {
    pub genesis_hash: String,
    /// The genesis validator set, the root when no checkpoint pins a newer one.
    genesis_root: epochs::Root,
    checkpoints: BTreeMap<u64, Checkpoint>,
    params: Params,
    ws_blocks: u64,
    /// Where `ws_blocks` comes from when it isn't the genesis parameters' (for messages).
    ws_source: Option<String>,
    /// No source can make the period shorter than this ([`ws_floor_blocks`]).
    ws_floor: u64,
    /// The genesis' `created_at_ms` (block 0's time) and the chain's block time.
    genesis_time: u64,
    block_time: u64,
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
        // The genesis period, or the release's when governance shortened it since (gap-6-1), but
        // never below what the protocol lets the chain have.
        let ws_floor = ws_floor_blocks(&cp, &params);
        let genesis_ws = ws_period_blocks(&cp, params.epoch_blocks);
        let released = known.and_then(|k| k.weak_subjectivity_blocks).map(|w| w.max(ws_floor)).filter(|w| *w < genesis_ws);
        let ws_source = released.map(|_| "as this release recorded it".to_string());
        let ws_blocks = released.unwrap_or(genesis_ws);
        let mut trust = Trust { genesis_hash: hash, genesis_root: epochs::Root::genesis(g), checkpoints: BTreeMap::new(), ws_blocks, ws_source, ws_floor, params, genesis_time: g.created_at_ms, block_time: g.block_time_ms.max(1), allow_stale: pins.allow_stale_checkpoint };
        let genesis = genesis_checkpoint(g);
        for c in known.map(|k| k.checkpoints.as_slice()).unwrap_or_default().iter().chain(&pins.checkpoints) {
            c.check()?;
            if c.height == 0 {
                ensure!(c.hash == genesis.hash, "checkpoint #0 must be the genesis hash {}, got {}", genesis.hash, c.hash);
                ensure!(c.set_hash.is_none() || c.set_hash == genesis.set_hash, "checkpoint #0's set hash must be the genesis set's, {}", genesis.set_hash.as_deref().unwrap_or_default());
                ensure!(c.timestamp_ms.is_none() || c.timestamp_ms == genesis.timestamp_ms, "checkpoint #0's time must be the genesis' created_at_ms, {}", g.created_at_ms);
            }
            trust.add(c.clone())?;
        }
        trust.check_times()?;
        Ok(trust)
    }

    /// Recorded times must be ones the chain could have: no block is less than a block time after
    /// its parent, so none is earlier than the genesis (or a timed checkpoint before it) allows. A
    /// time in seconds, or another chain's, is refused here.
    fn check_times(&self) -> Result<()> {
        let mut before = (0, self.genesis_time);
        for c in self.checkpoints.values() {
            let Some(t) = c.timestamp_ms else { continue };
            let least = earliest(before, c.height, self.block_time);
            ensure!(
                t as u128 >= least,
                "checkpoint #{} records its block's time as {t}, earlier than the chain allows ({least}: {} ms after #{}'s time {}, blocks being at least {} ms apart): a wrong checkpoint (the time is the block's timestamp_ms, in milliseconds)",
                c.height,
                least - before.1 as u128,
                before.0,
                before.1,
                self.block_time
            );
            before = (c.height, t);
        }
        Ok(())
    }

    /// The newest checkpoint, as (height, hash), when it records no time (an older form): its
    /// age then counts from the genesis, or from an older timed checkpoint ([`peer_block_time`]
    /// can read its time from the block).
    pub fn untimed_newest(&self) -> Option<(u64, String)> {
        self.checkpoints.values().next_back().filter(|c| c.height > 0 && c.timestamp_ms.is_none()).map(|c| (c.height, c.hash.clone()))
    }

    /// Records `time` as the time of the checkpoint at `height`, which records none: the time its
    /// block carries ([`peer_block_time`] checked the block hashes to the checkpoint's hash, which
    /// covers the time). Refused, changing nothing, when the checkpoints' times can't all be the
    /// chain's (an earlier checkpoint records a wrong time).
    pub fn learn_time(&mut self, height: u64, time: u64) -> Result<()> {
        let mut next = self.clone();
        let c = next.checkpoints.get_mut(&height).ok_or_else(|| anyhow!("no trusted checkpoint at #{height}"))?;
        if c.timestamp_ms.is_some() {
            return Ok(());
        }
        c.timestamp_ms = Some(time);
        next.check_times()?;
        *self = next;
        Ok(())
    }

    /// Adds a checkpoint; two at one height must agree (a set hash fills in a missing one).
    fn add(&mut self, c: Checkpoint) -> Result<()> {
        match self.checkpoints.get_mut(&c.height) {
            None => {
                self.checkpoints.insert(c.height, c);
            }
            Some(have) => {
                let sets_differ = matches!((&have.set_hash, &c.set_hash), (Some(a), Some(b)) if a != b);
                let times_differ = matches!((have.timestamp_ms, c.timestamp_ms), (Some(a), Some(b)) if a != b);
                ensure!(have.hash == c.hash && !sets_differ && !times_differ, "conflicting checkpoints at #{}: {} and {}", c.height, have.to_flag(), c.to_flag());
                if have.set_hash.is_none() {
                    have.set_hash = c.set_hash;
                }
                if have.timestamp_ms.is_none() {
                    have.timestamp_ms = c.timestamp_ms;
                }
            }
        }
        Ok(())
    }

    /// The weak-subjectivity period this node enforces, in blocks: the shortest of the genesis
    /// parameters', this release's and (once [`Trust::follow`]ed) the executed state's.
    pub fn ws_blocks(&self) -> u64 {
        self.ws_blocks
    }

    /// Enforces the period the parameters `live` set (those in force in a state this node has
    /// executed) when it's shorter than the one enforced so far: governance can shorten the
    /// evidence window (`evidence_max_age_epochs`, `unbonding_blocks`), and a root is only safe
    /// for half the window in force, whatever the genesis said (gap-6-1). It never lengthens the
    /// period, so a state claiming a longer window changes nothing. Returns whether it shortened.
    pub fn follow(&mut self, live: &ChainParams) -> bool {
        let ws = ws_period_blocks(live, self.params.epoch_blocks);
        self.shorten(ws, || "as the parameters in force in this node's own state set it".into())
    }

    /// For a node with nothing of its own to go by yet (an empty home, `join`): enforces the
    /// shortest period any peer reports for the chain's parameters in force (`reports`: a peer
    /// and its `/api/status` `weak_subjectivity_blocks`, [`peer_periods`]) when it's shorter than
    /// the one enforced so far, so a governance change after this release reaches the joiner if
    /// one honest peer is listed (gap-6-1). A report never lengthens the period, and never
    /// shortens it below [`ws_floor_blocks`] (no chain can have less, so a lower report is a lie):
    /// a lying peer can at worst ask for a fresher checkpoint. Returns whether it shortened.
    pub fn heed(&mut self, reports: &[(String, u64)]) -> bool {
        let Some((peer, reported)) = reports.iter().min_by_key(|(_, w)| *w) else { return false };
        let ws = (*reported).max(self.ws_floor);
        let floor = if ws > *reported { format!(", the protocol's floor; it reports {reported}") } else { String::new() };
        self.shorten(ws, || format!("in force on the chain as {peer} reports it{floor}: governance shortened the evidence window after this release"))
    }

    fn shorten(&mut self, ws: u64, source: impl FnOnce() -> String) -> bool {
        if ws >= self.ws_blocks {
            return false;
        }
        self.ws_blocks = ws;
        self.ws_source = Some(source());
        true
    }

    /// Every checkpoint, oldest first.
    pub fn checkpoints(&self) -> impl Iterator<Item = &Checkpoint> {
        self.checkpoints.values()
    }

    /// Height of the newest checkpoint (0: only the genesis).
    pub fn newest(&self) -> u64 {
        self.checkpoints.keys().next_back().copied().unwrap_or(0)
    }

    /// The blocks whose time this node trusts, as (height, timestamp_ms): the genesis, and the
    /// checkpoints that record one.
    fn timed(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        std::iter::once((0, self.genesis_time)).chain(self.checkpoints.values().filter_map(|c| Some((c.height, c.timestamp_ms?))))
    }

    /// The highest the chain can be at `now_ms`, from a block at `from.0` with time `from.1`:
    /// consensus refuses a block less than a block time after its parent, so it can't have grown
    /// faster than that since.
    fn reach(&self, from: (u64, u64), now_ms: u64) -> u64 {
        from.0.saturating_add(now_ms.saturating_sub(from.1) / self.block_time)
    }

    /// The height the chain would have reached at `now_ms` at its nominal block time, counted from
    /// the newest block whose time it trusts (the newest checkpoint that records one, or the
    /// genesis): the most it can be. Missed slots only make the real height lower, so ages
    /// measured from it err towards "stale"; slots missed before that block don't count, so on an
    /// aging chain (every missed slot since the genesis adds up) a checkpoint is as old as its own
    /// time says (review-r3-sync-1).
    pub fn expected_height(&self, now_ms: u64) -> u64 {
        self.expected_from(None, now_ms)
    }

    /// [`Trust::expected_height`], also counting from `own` (this node's head: height and time).
    fn expected_from(&self, own: Option<(u64, u64)>, now_ms: u64) -> u64 {
        self.timed().chain(own).map(|b| self.reach(b, now_ms)).min().unwrap_or(0)
    }

    /// The epoch of `height` on this chain.
    pub fn epoch_of(&self, height: u64) -> u64 {
        self.params.epoch_of(height)
    }

    /// The epoch whose validator set `c.set_hash` pins (the one certifying block `height + 1`).
    pub fn pinned_epoch(&self, c: &Checkpoint) -> u64 {
        self.params.epoch_of(c.height.saturating_add(1))
    }

    /// The newest validator set trusted at or before `epoch`: the newest checkpoint pinning one,
    /// or the genesis set. Histories are verified from it; nothing a set before it signed matters.
    pub fn root(&self, epoch: u64) -> epochs::Root {
        self.checkpoints
            .values()
            .rev()
            .filter(|c| self.pinned_epoch(c) <= epoch)
            .find_map(|c| {
                let hash: [u8; 32] = hex::decode(c.set_hash.as_deref()?).ok()?.try_into().ok()?;
                Some(epochs::Root { epoch: self.pinned_epoch(c), set_hash: hash })
            })
            .unwrap_or_else(|| self.genesis_root.clone())
    }

    /// Block `height` must have `hash`, and carry the time (`time`: its `timestamp_ms`, when the
    /// caller holds the block itself) a checkpoint at that height records; and no block may be
    /// earlier than a timed checkpoint before it allows (blocks are at least a block time apart).
    /// The hash covers the time, so a block with the checkpoint's hash and another time means the
    /// checkpoint's recorded time is wrong: it's refused too, as the freshness it vouched for was.
    pub fn check_block(&self, height: u64, hash: &str, time: Option<u64>) -> Result<()> {
        if let Some(c) = self.checkpoints.get(&height) {
            ensure!(c.hash == hash, "block #{height} is {hash}, but the trusted checkpoint says {}: this peer serves another chain", c.hash);
            if let (Some(want), Some(have)) = (c.timestamp_ms, time) {
                ensure!(want == have, "block #{height} ({hash}) carries the time {have}, but the trusted checkpoint records {want}: the checkpoint's time is wrong (take it from `blockula checkpoint`, which reads it from the block)");
            }
        }
        let Some(have) = time else { return Ok(()) };
        let newest_before = self.checkpoints.range(..height).rev().find_map(|(h, c)| Some((*h, c.timestamp_ms?)));
        if let Some((h, t)) = newest_before {
            let least = earliest((h, t), height, self.block_time);
            ensure!(have as u128 >= least, "block #{height} carries the time {have}, earlier than the trusted checkpoint #{h} (time {t}) allows ({least}, blocks being at least {} ms apart): that checkpoint's recorded time is wrong, or this isn't its chain", self.block_time);
        }
        Ok(())
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

    /// Why a node whose newest verified block is `own` (its height, 0 for an empty home, and its
    /// time when the node holds it) can't rely on what it trusts at `now_ms`, if it can't: its
    /// newest root (checkpoint or own block) is older than the weak-subjectivity period, measured
    /// against the chain's expected height ([`Trust::expected_height`], also counted from `own`);
    /// or a checkpoint records a time past this machine's clock (a wrong checkpoint, or clock).
    pub fn stale(&self, own: (u64, Option<u64>), now_ms: u64) -> Option<String> {
        let block_time = self.block_time;
        let slack = crate::snapshot::FUTURE_SLACK_MS;
        if let Some((h, t)) = self.timed().skip(1).find(|(_, t)| *t > now_ms.saturating_add(slack)) {
            return Some(format!(
                "trusted checkpoint #{h} records its block's time as {t}, {} s past this machine's clock ({now_ms}): a block can't be from the future, so the checkpoint is wrong (the time is the block's timestamp_ms, in milliseconds) or this clock is. Fix it, or pass a correct --checkpoint HEIGHT:HASH:SET_HASH:TIMESTAMP_MS (`blockula checkpoint` prints one)",
                (t - now_ms) / 1000
            ));
        }
        let (own_height, own_time) = own;
        let expected = self.expected_from(own_time.filter(|_| own_height > 0).map(|t| (own_height, t)), now_ms);
        let newest = self.newest().max(own_height);
        let age = expected.saturating_sub(newest);
        (age > self.ws_blocks).then(|| {
            let source = self.ws_source.as_ref().map(|s| format!(", {s}")).unwrap_or_default();
            format!(
                "the newest block this node trusts is #{newest}, about {age} blocks (~{} days) behind the chain's expected height #{expected}: older than the weak-subjectivity period ({} blocks, ~{} days{source}). Keys of validators that have since unbonded could forge a history from there. Pass a recent --checkpoint HEIGHT:HASH:SET_HASH:TIMESTAMP_MS from sources you trust (`blockula checkpoint --peer … --peer …`, OPERATIONS.md \"Weak subjectivity\")",
                days(age, block_time),
                self.ws_blocks,
                days(self.ws_blocks, block_time)
            )
        })
    }

    /// [`Trust::stale`] for a node about to sync from `own`: an error for an empty home (height
    /// 0) unless the operator allowed stale checkpoints, otherwise the warning.
    pub fn check_fresh(&self, own: (u64, Option<u64>), now_ms: u64) -> Result<Option<String>> {
        match self.stale(own, now_ms) {
            Some(why) if own.0 == 0 && !self.allow_stale => Err(anyhow!("{why}, or pass --allow-stale-checkpoint to accept the risk")),
            other => Ok(other),
        }
    }
}

/// What `peers` report as the weak-subjectivity period in force (`/api/status`
/// `weak_subjectivity_blocks`), asked at once, each within `timeout`: the peers that answered for
/// this chain instance (`genesis_hash`), with their reports. For [`Trust::heed`] (gap-6-1).
pub async fn peer_periods(peers: &[String], genesis_hash: &str, timeout: std::time::Duration) -> Vec<(String, u64)> {
    let Ok(client) = reqwest::Client::builder().timeout(timeout).build() else { return vec![] };
    let ask = |peer: String| {
        let client = client.clone();
        async move {
            let url = format!("{}/api/status", peer.trim_end_matches('/'));
            let body = crate::node::get_capped(&client, &url, 1 << 20).await.ok()?;
            let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
            if v["genesis_hash"].as_str() != Some(genesis_hash) {
                return None;
            }
            Some((peer, v["weak_subjectivity_blocks"].as_u64()?))
        }
    };
    futures::future::join_all(peers.iter().cloned().map(ask)).await.into_iter().flatten().collect()
}

/// The time of the block at `height` whose hash is `hash` (a trusted checkpoint that records no
/// time), as a peer that serves that block (`/api/sync`: the consensus block itself) shows it:
/// the first to answer of `peers`, asked a few at a time, each within `timeout`. The hash covers
/// the time, so no peer can make it another; one that pruned the block, serves another or junk is
/// passed over. Returns the peer and the time. For [`Trust::learn_time`] (review-r3-sync-1).
pub async fn peer_block_time(peers: &[String], height: u64, hash: &str, timeout: std::time::Duration) -> Option<(String, u64)> {
    use commonware_codec::DecodeExt;
    use futures::StreamExt;
    let client = reqwest::Client::builder().timeout(timeout).build().ok()?;
    let ask = |peer: String| {
        let client = client.clone();
        async move {
            let url = format!("{}/api/sync?from={height}&limit=1", peer.trim_end_matches('/'));
            let body = crate::node::get_capped(&client, &url, crate::node::MAX_SYNC_RESPONSE).await.ok()?;
            let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
            let block = crate::consensus::CBlock::decode(hex::decode(v["blocks"].get(0)?.as_str()?).ok()?.as_slice()).ok()?;
            (block.height == height && hex::encode(block.digest_bytes()) == hash).then_some((peer, block.timestamp_ms))
        }
    };
    // A few at a time: each answer may be up to a sync response's size.
    let answers = futures::stream::iter(peers.iter().cloned().map(ask)).buffer_unordered(4).filter_map(std::future::ready);
    std::pin::pin!(answers).next().await
}

/// `blockula checkpoint`: the checkpoint at `height` (default: the newest epoch boundary every
/// peer has finalized) that all `peers` agree on. Each peer's genesis must match the others' and
/// its validator-set history must verify (`epochs::verify_from`) from the newest root this binary
/// trusts (an embedded checkpoint, or the genesis), or, when the peer no longer keeps history
/// that old (nodes keep about the weak-subjectivity period), from the oldest set it keeps. Ask
/// nodes run by independent operators: agreement among them is what a checkpoint attests. With
/// `pinned` (a genesis hash), the peers must serve that genesis: a joiner that already holds the
/// chain's genesis (an operator rebuilding its own node) can't be handed another chain's.
pub async fn fetch(peers: &[String], height: Option<u64>, pinned: Option<&str>) -> Result<(Genesis, Checkpoint)> {
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
    // The shortest weak-subjectivity period the peers report (their parameters in force).
    let mut reported_ws = u64::MAX;
    for u in &urls {
        let g: Genesis = serde_json::from_value(get(format!("{u}/api/genesis")).await?).with_context(|| format!("{u}/api/genesis"))?;
        let status = get(format!("{u}/api/status")).await?;
        finalized = finalized.min(status["finalized_height"].as_u64().ok_or_else(|| anyhow!("{u}/api/status has no finalized_height"))?);
        reported_ws = reported_ws.min(status["weak_subjectivity_blocks"].as_u64().unwrap_or(u64::MAX));
        match &genesis {
            Some(first) if genesis_hash(first) != genesis_hash(&g) => bail!("{u} serves another genesis ({}) than {} ({})", genesis_hash(&g), urls[0], genesis_hash(first)),
            Some(_) => {}
            None => genesis = Some(g),
        }
    }
    let g = genesis.expect("at least one peer");
    if let Some(want) = pinned {
        let have = genesis_hash(&g);
        ensure!(have == want.trim().to_ascii_lowercase(), "the peers serve genesis {have}, not the pinned {}: another chain (or another instance of {})", want.trim(), g.chain_id);
    }
    let params = g.params();
    // Governance shortened the evidence window since this release (gap-6-1): say how fresh a
    // checkpoint must be now, since this binary's joiners only know the longer period.
    if let Some(t) = Trust::new(&g, &Pins::default(), false).ok().filter(|t| reported_ws < t.ws_blocks()) {
        eprintln!(
            "⚠️  the peers' parameters in force put the weak-subjectivity period at {reported_ws} blocks (~{} days), shorter than this release's {} (~{} days): a checkpoint is safe for that long only",
            days(reported_ws, g.block_time_ms),
            t.ws_blocks(),
            days(t.ws_blocks(), g.block_time_ms)
        );
    }
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
    let trusted = Trust::new(&g, &Pins::default(), false).map(|t| t.root(epoch)).unwrap_or_else(|_| epochs::Root::genesis(&g));
    let mut agreed: Option<(String, Checkpoint)> = None;
    for u in &urls {
        let block = get(format!("{u}/api/block/{height}")).await?;
        let hash = block["hash"].as_str().ok_or_else(|| anyhow!("{u}/api/block/{height} has no hash"))?.to_string();
        // Its time (the hash covers it): joiners measure the checkpoint's age from it.
        let timestamp_ms = Some(block["timestamp_ms"].as_u64().ok_or_else(|| anyhow!("{u}/api/block/{height} has no timestamp_ms"))?);
        let reg = crate::snapshot::fetch_history(&client, u, trusted.epoch, epoch, &crate::snapshot::DownloadLimits::default()).await?;
        let root = match reg.sets.get(&reg.base) {
            Some(set) if reg.base > trusted.epoch => {
                eprintln!("⚠️  {u} keeps validator-set history from epoch {} on, after this release's newest trusted set (epoch {}): checking it from there; only the peers' agreement vouches for that set", reg.base, trusted.epoch);
                epochs::Root { epoch: reg.base, set_hash: epochs::set_hash(set) }
            }
            _ => trusted.clone(),
        };
        let set = epochs::verify_from(&g, &reg, &root, epoch).with_context(|| format!("{u}: validator-set history through epoch {epoch} (just after a boundary, its handoff signatures may still be arriving; retry, or pass an older --height)"))?;
        let cp = Checkpoint { height, hash, set_hash: Some(hex::encode(epochs::set_hash(&set))), timestamp_ms };
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
        vec![Known { chain_id: g.chain_id.clone(), genesis_hash: hash.into(), checkpoints, note: String::new(), weak_subjectivity_blocks: None }]
    }

    #[test]
    fn embedded_files_parse_and_mainnet_waits_for_its_ceremony() {
        assert_eq!(embedded().len(), EMBEDDED.len());
        for k in embedded() {
            assert!(k.genesis_hash.is_empty() || is_hash(&k.genesis_hash), "{}: genesis_hash", k.chain_id);
            for c in &k.checkpoints {
                c.check().unwrap();
                assert!(c.set_hash.is_some(), "{} #{}: embedded checkpoints pin a validator set", k.chain_id, c.height);
                // So joiners measure its age from its own time, not the chain's lifetime of missed
                // slots (review-r3-sync-1): `blockula checkpoint --json` prints it.
                assert!(c.timestamp_ms.is_some(), "{} #{}: embedded checkpoints record their block's timestamp_ms", k.chain_id, c.height);
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

    /// Every genesis a release ships (`genesis/<chain id>/genesis.json`, attached by release.yml
    /// as `genesis-<chain id>.json`) is its directory's chain and hashes to the genesis this
    /// binary pins for it (scripts/pin-network.sh writes both), so the release PR can't ship a
    /// genesis its own nodes would refuse; with its spec and operator files next to it, they
    /// rebuild it byte for byte.
    #[test]
    fn shipped_genesis_files_are_the_pinned_ones() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../genesis");
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        for e in entries {
            let e = e.unwrap();
            let chain = e.file_name().to_string_lossy().into_owned();
            let path = e.path().join("genesis.json");
            let g = Genesis::load(&path).unwrap_or_else(|err| panic!("{}: {err:#}", path.display()));
            assert_eq!(g.chain_id, chain, "{} is another chain's genesis", path.display());
            let known = embedded().iter().find(|k| k.chain_id == chain).unwrap_or_else(|| panic!("genesis/{chain}/ ships, but node/checkpoints/{chain}.json isn't embedded (scripts/pin-network.sh)"));
            assert_eq!(genesis_hash(&g), known.genesis_hash, "genesis/{chain}/genesis.json isn't the genesis this binary pins");
            // Parsing each field isn't enough: the table must also obey this genesis's height-0
            // hashes/time and inter-checkpoint spacing, or the released node refuses to start.
            Trust::new(&g, &Pins::default(), false).unwrap_or_else(|err| panic!("genesis/{chain}/ cannot start with its embedded checkpoints: {err:#}"));
            if e.path().join("spec.toml").exists() {
                let built = crate::genesis_builder::build(&e.path().join("spec.toml"), false).unwrap();
                assert_eq!(built.json, std::fs::read_to_string(&path).unwrap(), "genesis/{chain}/spec.toml doesn't rebuild genesis.json");
            }
        }
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
        assert_eq!(Checkpoint::parse(&format!("7:{}", a.to_uppercase())).unwrap(), Checkpoint { height: 7, hash: a.clone(), set_hash: None, timestamp_ms: None });
        assert_eq!(Checkpoint::parse(&format!("7:{a}:{b}")).unwrap().to_flag(), format!("7:{a}:{b}"));
        // With the block's time (review-r3-sync-1), with or without a set hash.
        let timed = Checkpoint::parse(&format!("7:{a}:{b}:1700000000123")).unwrap();
        assert_eq!(timed, Checkpoint { height: 7, hash: a.clone(), set_hash: Some(b.clone()), timestamp_ms: Some(1_700_000_000_123) });
        assert_eq!(timed.to_flag(), format!("7:{a}:{b}:1700000000123"));
        let no_set = Checkpoint::parse(&format!("7:{a}::42")).unwrap();
        assert_eq!((no_set.set_hash.clone(), no_set.timestamp_ms, no_set.to_flag()), (None, Some(42), format!("7:{a}::42")));
        assert_eq!(serde_json::from_str::<Checkpoint>(&serde_json::to_string(&timed).unwrap()).unwrap(), timed);
        assert!(serde_json::to_string(&timed).unwrap().contains("\"timestamp_ms\":1700000000123"));
        for bad in ["7", &format!("x:{a}"), "7:abc", &format!("7:{a}:zz"), &format!("7:{a}:{b}:{b}"), &format!("-1:{a}"), &format!("7:{a}:"), &format!("7:{a}::"), &format!("7:{a}:{b}:-5"), &format!("7:{a}:{b}:1:2"), &format!("7:{a}:zz:1")] {
            assert!(Checkpoint::parse(bad).is_err(), "{bad}");
        }
        let pins = |cps: Vec<Checkpoint>| Pins { genesis_hash: Some(hash.clone()), checkpoints: cps, ..Default::default() };
        let cp = |h: u64, hash: &str, set: Option<&str>| Checkpoint { height: h, hash: hash.into(), set_hash: set.map(str::to_string), timestamp_ms: None };
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
        t.check_block(700, &a, None).unwrap();
        t.check_block(701, &b, None).unwrap();
        assert!(t.check_block(700, &b, None).unwrap_err().to_string().contains("another chain"));
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
        assert_eq!(t.check_fresh((0, None), now(10)).unwrap(), None);
        // Three weeks in, the genesis alone is stale: refused for an empty home, a warning otherwise.
        let err = t.check_fresh((0, None), now(21)).unwrap_err().to_string();
        assert!(err.contains("weak-subjectivity period") && err.contains("~21.0 days"), "{err}");
        assert!(t.check_fresh((20 * day, None), now(21)).unwrap().is_none(), "the node's own recent head counts");
        assert!(t.check_fresh((day, None), now(21)).unwrap().unwrap().contains("--checkpoint"));
        // A recent checkpoint, or the operator's explicit choice, lets it start.
        let fresh = Pins { checkpoints: vec![Checkpoint { height: 18 * day, hash: "cd".repeat(32), set_hash: None, timestamp_ms: None }], ..pins.clone() };
        assert_eq!(Trust::new(&g, &fresh, true).unwrap().check_fresh((0, None), now(21)).unwrap(), None);
        let allowed = Pins { allow_stale_checkpoint: true, ..pins.clone() };
        assert!(Trust::new(&g, &allowed, true).unwrap().check_fresh((0, None), now(21)).unwrap().is_some());
        // Slower than nominal blocks only make it look older (never fresher).
        g.created_at_ms = created - 30 * 86_400_000;
        assert!(Trust::new(&g, &Pins { genesis_hash: Some(genesis_hash(&g)), ..Default::default() }, true).unwrap().check_fresh((0, None), now(0)).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// review-r3-sync-1: blocks are never closer than the block time and are further apart after
    /// every missed slot, so on an aging chain the deficit since the genesis only grows. Measured
    /// from the genesis, a checkpoint taken at the tip looked as old as that whole deficit: past
    /// a week of it, one lying peer's report (held to the week's floor) refused every checkpoint
    /// there is, and past the genesis' 10.5 days, honest parameters did. A checkpoint that records
    /// its block's time is as old as that time says: only slots missed since then count.
    #[test]
    fn an_aging_chain_measures_a_checkpoint_from_its_own_time() {
        let (root, _, mut g) = summoned("aging", 1);
        let bt = g.block_time_ms;
        let day = 86_400_000 / bt;
        let now = crate::chain::now_ms();
        let days_ago = |d: u64| now - d * 86_400_000;
        // A year old, with 8 days of nominal blocks lost to timed-out views (a 2.2% deficit).
        g.created_at_ms = days_ago(365);
        let hash = genesis_hash(&g);
        let tip = 357 * day;
        let at = |height: u64, time: Option<u64>| Pins { genesis_hash: Some(hash.clone()), checkpoints: vec![Checkpoint { height, hash: "cd".repeat(32), set_hash: None, timestamp_ms: time }], ..Default::default() };
        let liar = [("http://one-genesis-validator".to_string(), 1)];
        // Without its time, the checkpoint at the tip is 8 days behind the genesis' line: fresh by
        // the genesis' 10.5 days, refused once a liar pulls the period to a week.
        let mut untimed = Trust::new(&g, &at(tip, None), true).unwrap();
        assert_eq!(untimed.check_fresh((0, None), now).unwrap(), None);
        assert!(untimed.heed(&liar));
        let err = untimed.check_fresh((0, None), now).unwrap_err().to_string();
        assert!(err.contains("~8.0 days") && err.contains("the protocol's floor; it reports 1"), "{err}");
        // With it (taken a minute ago), it's a minute old, whatever any peer says.
        let mut timed = Trust::new(&g, &at(tip, Some(now - 60_000)), true).unwrap();
        assert_eq!(timed.expected_height(now), tip + 60_000 / bt, "counted from the checkpoint's own time");
        assert!(timed.heed(&liar) && timed.ws_blocks() == 7 * day);
        assert_eq!(timed.check_fresh((0, None), now).unwrap(), None, "a checkpoint at the tip survives any lie");
        // Six days old: fresh under the floor. Eight days old by its own time: refused, and the
        // refusal counts only those eight days.
        let mut six = Trust::new(&g, &at(tip - 6 * day, Some(days_ago(6))), true).unwrap();
        assert!(six.heed(&liar));
        assert_eq!(six.check_fresh((0, None), now).unwrap(), None, "a week-old checkpoint survives any lie");
        let mut eight = Trust::new(&g, &at(tip - 8 * day, Some(days_ago(8))), true).unwrap();
        assert_eq!(eight.check_fresh((0, None), now).unwrap(), None, "by the genesis' period");
        assert!(eight.heed(&liar));
        assert!(eight.check_fresh((0, None), now).unwrap_err().to_string().contains("~8.0 days"));
        // Twelve days of deficit (past the genesis' own period): the untimed tip is refused with
        // no lie at all, the timed one is fresh, and so is it after honest governance shortens
        // the window to its floor.
        let tip = 353 * day;
        assert!(Trust::new(&g, &at(tip, None), true).unwrap().check_fresh((0, None), now).unwrap_err().to_string().contains("~12.0 days"));
        let mut timed = Trust::new(&g, &at(tip, Some(now - 60_000)), true).unwrap();
        assert_eq!(timed.check_fresh((0, None), now).unwrap(), None);
        let mut floor = g.chain_params().unwrap();
        floor.evidence_max_age_epochs = evidence_floor_epochs(&g.params());
        floor.unbonding_blocks = floor.evidence_max_age_epochs * g.epoch_blocks;
        assert!(timed.follow(&floor) && timed.ws_blocks() == 7 * day);
        assert_eq!(timed.check_fresh((0, None), now).unwrap(), None);
        // A synced node's own head counts from its own time too: no warning at the tip of an
        // aging chain (only the height, as from a fast-sync base it hasn't built on: warned).
        let genesis_only = Trust::new(&g, &Pins { genesis_hash: Some(hash.clone()), ..Default::default() }, true).unwrap();
        assert_eq!(genesis_only.check_fresh((tip, Some(now - 2_000)), now).unwrap(), None);
        assert!(genesis_only.check_fresh((tip, None), now).unwrap().unwrap().contains("~12.0 days"));
        assert!(genesis_only.check_fresh((tip - 11 * day, Some(days_ago(11))), now).unwrap().unwrap().contains("~11.0 days"), "a head that's really old still warns");
        // A time far enough ahead of the line is impossible (a forged root can't hide behind it):
        // it's refused below the genesis' line and above this machine's clock.
        let below = Trust::new(&g, &at(tip, Some(g.created_at_ms + tip * bt - 1)), true).unwrap_err().to_string();
        assert!(below.contains("earlier than the chain allows"), "{below}");
        let seconds = Trust::new(&g, &at(tip, Some(now / 1000)), true).unwrap_err().to_string();
        assert!(seconds.contains("in milliseconds"), "{seconds}");
        let micros = Trust::new(&g, &at(tip, Some(now * 1000)), true).unwrap();
        let err = micros.check_fresh((0, None), now).unwrap_err().to_string();
        assert!(err.contains("past this machine's clock") && err.contains("TIMESTAMP_MS"), "{err}");
        assert!(micros.check_fresh((tip, Some(now)), now).unwrap().unwrap().contains("past this machine's clock"), "a synced node is warned");
        Trust::new(&g, &at(tip, Some(now + 60_000)), true).unwrap().check_fresh((0, None), now).unwrap();
        // Two times the chain can't both have, and two at one height, are refused.
        let pair = |a: (u64, u64), b: (u64, u64)| Pins { genesis_hash: Some(hash.clone()), checkpoints: vec![Checkpoint { height: a.0, hash: "cd".repeat(32), set_hash: None, timestamp_ms: Some(a.1) }, Checkpoint { height: b.0, hash: "ef".repeat(32), set_hash: None, timestamp_ms: Some(b.1) }], ..Default::default() };
        let err = Trust::new(&g, &pair((tip, days_ago(2)), (tip + day, days_ago(2) + 86_400_000 - bt)), true).unwrap_err().to_string();
        assert!(err.contains(&format!("checkpoint #{} records", tip + day)) && err.contains(&format!("after #{tip}'s time")), "{err}");
        Trust::new(&g, &pair((tip, days_ago(2)), (tip + day, days_ago(1))), true).unwrap();
        let mut both = at(tip, Some(days_ago(1)));
        both.checkpoints.push(Checkpoint { timestamp_ms: Some(days_ago(1) + 1), ..both.checkpoints[0].clone() });
        assert!(Trust::new(&g, &both, true).unwrap_err().to_string().contains("conflicting checkpoints"));
        let filled = Trust::with_table(&g, &at(tip, None), true, &known(&g, &hash, vec![Checkpoint { height: tip, hash: "cd".repeat(32), set_hash: None, timestamp_ms: Some(days_ago(1)) }])).unwrap();
        assert_eq!(filled.checkpoints().next().unwrap().timestamp_ms, Some(days_ago(1)), "a time fills in a missing one");
        let zero = |t: u64| Pins { genesis_hash: Some(hash.clone()), checkpoints: vec![Checkpoint { timestamp_ms: Some(t), ..genesis_checkpoint(&g) }], ..Default::default() };
        Trust::new(&g, &zero(g.created_at_ms), true).unwrap();
        assert!(Trust::new(&g, &zero(g.created_at_ms + 1), true).unwrap_err().to_string().contains("created_at_ms"));
        // Blocks: at the checkpoint, its time (the hash covers it); after it, no earlier than it
        // allows; before it, unconstrained; a caller without the block checks only the hash.
        let t = Trust::new(&g, &at(tip, Some(days_ago(1))), true).unwrap();
        t.check_block(tip, &"cd".repeat(32), Some(days_ago(1))).unwrap();
        t.check_block(tip, &"cd".repeat(32), None).unwrap();
        let err = t.check_block(tip, &"cd".repeat(32), Some(days_ago(1) + 1)).unwrap_err().to_string();
        assert!(err.contains("the checkpoint's time is wrong"), "{err}");
        t.check_block(tip + 10, &"ab".repeat(32), Some(days_ago(1) + 10 * bt)).unwrap();
        let err = t.check_block(tip + 10, &"ab".repeat(32), Some(days_ago(1) + 10 * bt - 1)).unwrap_err().to_string();
        assert!(err.contains(&format!("earlier than the trusted checkpoint #{tip}")), "{err}");
        t.check_block(tip - 10, &"ab".repeat(32), Some(0)).unwrap();
        std::fs::remove_dir_all(root).ok();
    }

    /// gap-6-1: governance shortens the evidence window (what a passed `ParamChange` runs:
    /// `exec_set_param`) as far as it can, to two weeks (below that is refused), so a root is safe
    /// for a week. A node that executed that state enforces it, so a checkpoint 9 days old (fresh
    /// by the genesis' 10.5 days) is refused for an empty home and warned about otherwise; a state
    /// claiming a longer window doesn't lengthen it; a release that recorded the shorter period
    /// makes joiners enforce it too; and so does a peer's report, never below the floor.
    #[test]
    fn the_weak_subjectivity_period_follows_governance() {
        let (root, homes, g) = summoned("ws-gov", 1);
        let mut c = Chain::open(&homes[0]).unwrap();
        let day = 86_400_000 / g.block_time_ms;
        let mut j = crate::chain::Journal::default();
        let floor = evidence_floor_epochs(&g.params());
        assert_eq!(floor * g.epoch_blocks, 14 * day);
        let err = c.state.exec_set_param(&mut j, "evidence_max_age_epochs", day / g.epoch_blocks).unwrap_err().to_string();
        assert!(err.contains("weak-subjectivity floor"), "a day is below the floor: {err}");
        c.state.exec_set_param(&mut j, "evidence_max_age_epochs", floor).unwrap();
        assert!(c.state.exec_set_param(&mut j, "unbonding_blocks", 13 * day).is_err(), "unbonding can't go below the window");
        c.state.exec_set_param(&mut j, "unbonding_blocks", 14 * day).unwrap();
        assert_eq!(ws_period_blocks(&c.state.chain_params, g.epoch_blocks), 7 * day);
        let now = g.created_at_ms + 30 * 86_400_000;
        let hash = genesis_hash(&g);
        let nine_days_old = Pins { genesis_hash: Some(hash.clone()), checkpoints: vec![Checkpoint { height: 21 * day, hash: "cd".repeat(32), set_hash: None, timestamp_ms: None }], ..Default::default() };
        let by_genesis = Trust::new(&g, &nine_days_old, true).unwrap();
        assert_eq!((by_genesis.ws_blocks(), by_genesis.check_fresh((0, None), now).unwrap()), (21 * day / 2, None), "the genesis period: fresh");
        let mut live = by_genesis.clone();
        assert!(live.follow(&c.state.chain_params));
        assert_eq!(live.ws_blocks(), 7 * day);
        let err = live.check_fresh((0, None), now).unwrap_err().to_string();
        assert!(err.contains("weak-subjectivity period") && err.contains("~7.0 days, as the parameters in force in this node's own state set it"), "{err}");
        assert!(live.check_fresh((22 * day, None), now).unwrap().unwrap().contains("--checkpoint"), "a node with older state of its own: warned");
        assert_eq!(live.check_fresh((30 * day - 10, None), now).unwrap(), None, "its own head is recent");
        // Following a longer window changes nothing.
        let mut longer = c.state.chain_params.clone();
        longer.set("unbonding_blocks", 300 * day, &g.params()).unwrap();
        longer.set("evidence_max_age_epochs", 300 * day / g.epoch_blocks, &g.params()).unwrap();
        assert!(!live.follow(&longer) && live.ws_blocks() == 7 * day);
        assert!(!by_genesis.clone().follow(&longer), "never past the genesis period either");
        // A release that recorded the shorter period: its joiners enforce it before any state,
        // never below the floor (a typo can't ask for more than a week-old checkpoint).
        let mut table = known(&g, &hash, vec![]);
        for (recorded, enforced) in [(8 * day, 8 * day), (7 * day, 7 * day), (day / 2, 7 * day), (400 * day, 21 * day / 2)] {
            table[0].weak_subjectivity_blocks = Some(recorded);
            let released = Trust::with_table(&g, &nine_days_old, true, &table).unwrap();
            assert_eq!(released.ws_blocks(), enforced, "recorded {recorded}");
            assert_eq!(released.check_fresh((0, None), now).is_err(), enforced < 9 * day, "recorded {recorded}");
        }
        // A joiner on a release that predates the change: one peer's report is enough.
        let peer = |name: &str, w: u64| (name.to_string(), w);
        let mut joiner = by_genesis.clone();
        assert!(!joiner.heed(&[]) && !joiner.heed(&[peer("old", 21 * day / 2), peer("longer", 400 * day)]), "nothing shorter reported");
        assert!(joiner.heed(&[peer("http://stale", 21 * day / 2), peer("http://honest", 7 * day)]));
        assert_eq!(joiner.ws_blocks(), 7 * day);
        let err = joiner.check_fresh((0, None), now).unwrap_err().to_string();
        assert!(err.contains("~7.0 days, in force on the chain as http://honest reports it") && err.contains("--allow-stale-checkpoint"), "{err}");
        // A peer reporting less than any chain can have is held to the floor: a week.
        let mut lied_to = by_genesis.clone();
        assert!(lied_to.heed(&[peer("http://liar", 1)]));
        assert_eq!(lied_to.ws_blocks(), 7 * day);
        assert!(lied_to.check_fresh((0, None), now).unwrap_err().to_string().contains("the protocol's floor; it reports 1"));
        let six_days_old = Pins { checkpoints: vec![Checkpoint { height: 24 * day, hash: "cd".repeat(32), set_hash: None, timestamp_ms: None }], ..nine_days_old.clone() };
        let mut recent = Trust::new(&g, &six_days_old, true).unwrap();
        assert!(recent.heed(&[peer("http://liar", 1)]));
        assert_eq!(recent.check_fresh((0, None), now).unwrap(), None, "a week-old checkpoint survives any lie");
        // A chain that starts below the floor (dev chains) can't go below its own genesis window.
        let dev = Genesis { param_overrides: ChainParams::dev_overrides(g.block_time_ms, g.epoch_blocks), ..g.clone() };
        let dev_cp = dev.chain_params().unwrap();
        assert!(dev_cp.evidence_max_age_epochs < floor);
        let mut dev_trust = Trust::new(&dev, &Pins { genesis_hash: Some(genesis_hash(&dev)), ..Default::default() }, true).unwrap();
        assert_eq!(dev_trust.ws_blocks(), ws_floor_blocks(&dev_cp, &dev.params()));
        assert!(!dev_trust.heed(&[peer("http://liar", 1)]), "already at its floor");
        drop(c);
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
        let (got, cp) = fetch(&[url.clone(), url.clone()], None, Some(&genesis_hash(&g))).await.unwrap();
        assert_eq!(genesis_hash(&got), genesis_hash(&g));
        assert_eq!(cp.height, boundary);
        assert_eq!(Some(cp.hash.clone()), chain.lock().hash_at(boundary));
        assert_eq!(cp.set_hash, genesis_checkpoint(&g).set_hash, "the set didn't change");
        // With the block's time (review-r3-sync-1), in the form `join --checkpoint` takes.
        let time = |h: u64| chain.lock().block(h).unwrap().timestamp_ms;
        assert_eq!(cp.timestamp_ms, Some(time(boundary)));
        assert_eq!(Checkpoint::parse(&cp.to_flag()).unwrap(), cp);
        assert_eq!(cp.to_flag(), format!("{boundary}:{}:{}:{}", cp.hash, cp.set_hash.as_deref().unwrap(), time(boundary)));
        let three = fetch(std::slice::from_ref(&url), Some(3), None).await.unwrap().1;
        let hash3 = chain.lock().hash_at(3).unwrap();
        assert_eq!((three.hash.clone(), three.timestamp_ms), (hash3, Some(time(3))));
        assert_eq!(fetch(std::slice::from_ref(&url), Some(0), None).await.unwrap().1, genesis_checkpoint(&g));
        assert_eq!(genesis_checkpoint(&g).timestamp_ms, Some(g.created_at_ms));
        assert!(fetch(std::slice::from_ref(&url), Some(epoch_blocks + 100), None).await.unwrap_err().to_string().contains("isn't finalized"));
        // Pinned to another genesis: refused before anything else is fetched.
        let err = fetch(std::slice::from_ref(&url), None, Some(&"ab".repeat(32))).await.unwrap_err().to_string();
        assert!(err.contains(&format!("the peers serve genesis {}, not the pinned {}", genesis_hash(&g), "ab".repeat(32))), "{err}");
        // A peer that serves another block at that height, or another time for it, is called out.
        let liar = |block: serde_json::Value| {
            let real = url.clone();
            async move {
                let liar = Router::new().route("/api/block/{h}", get(move || async move { Json(block.clone()) })).fallback(move |req: axum::extract::Request| {
                    let real = real.clone();
                    async move { reqwest::get(format!("{real}{}", req.uri())).await.unwrap().text().await.unwrap() }
                });
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let liar_url = format!("http://{}", listener.local_addr().unwrap());
                tokio::spawn(async move { axum::serve(listener, liar).await.unwrap() });
                liar_url
            }
        };
        for lie in [serde_json::json!({ "hash": "ee".repeat(32), "timestamp_ms": time(3) }), serde_json::json!({ "hash": three.hash, "timestamp_ms": time(3) + 86_400_000 })] {
            let err = fetch(&[url.clone(), liar(lie).await], Some(3), None).await.unwrap_err().to_string();
            assert!(err.contains("disagree"), "{err}");
        }
        let err = fetch(&[liar(serde_json::json!({ "hash": three.hash })).await], Some(3), None).await.unwrap_err().to_string();
        assert!(err.contains("has no timestamp_ms"), "{err}");
        drop(chain);
        std::fs::remove_dir_all(root).ok();
    }
}
