//! Certified state snapshots and fast sync.
//!
//! Every `CHECKPOINT_INTERVAL` blocks is a checkpoint height C. Consensus requires the block at
//! C + `CHECKPOINT_DELAY` to carry `exec_height = C` and `exec_root = state_root(C)`, and every
//! verifier must have executed C and checked it before voting. So the finalization certificate
//! of that block (or of any descendant, via hash links) certifies `state_root(C)`.
//!
//! Each node writes the state at its latest checkpoints (canonical records → zstd → 1 MiB
//! chunks, each BLAKE3-hashed) and serves a manifest: chunk hashes, the blocks C+1..=F, F's
//! finalization certificate and the epoch anchor (below). It keeps its two newest snapshots and
//! two long-lived ones (one per [`long_interval`], ~15 minutes of blocks), and serves a big state
//! from the newest long-lived one, so a joiner always has at least [`LONG_SNAPSHOT_MS`] to fetch
//! the chunks before they're deleted (F63). A joiner spreads the chunk requests over every peer
//! that holds the same snapshot, gives up on a peer that stalls or fails one, and gives up on the
//! whole download past a deadline set by its size (F62). The validator-set history (handoff
//! proofs) needed to check that certificate comes separately, in pages of `/api/epochs` from the
//! joiner's newest trusted root (`fetch_history`), so neither grows with the chain. A joining node
//! verifies everything, checks it against the checkpoints it trusts (`checkpoints.rs`), rebuilds
//! the state and its tree from the records, checks the root against the certified one, executes
//! C+1..=F and carries on syncing.
//!
//! **Epoch anchor.** Consensus starts each epoch's engine from the hash of the previous epoch's
//! last block. When that block is before C, the manifest carries it with its finalization
//! certificate, signed by the set of the epoch it ends (epoch-end certificates are kept forever,
//! see `store.rs`), so nothing in a manifest is taken from the peer on trust. A pruning node's
//! window always reaches back to it (`chain::min_keep_blocks` covers an epoch plus every kept
//! snapshot), and a certificate execution didn't get from consensus is filled in from marshal
//! afterwards (`consensus::fill_late_certs`).
//!
//! Both directions stream: the writer encodes records straight into the compressor and cuts
//! chunk files as they fill; a joiner checks each chunk as it arrives, appends it to
//! `base.state.zst.part`, and decodes that file as a stream. Neither ever holds the encoded state
//! in memory, only the state itself.

use crate::chain::{Genesis, State};
use crate::consensus::{self, CBlock};
use crate::epochs::{self, EpochRegistry};
use crate::pq::MlDsaScheme;
use anyhow::{anyhow, bail, ensure, Context, Result};
use commonware_codec::{Decode, DecodeExt};
use commonware_consensus::simplex::types::Finalization;
use commonware_cryptography::{Digestible, blake3::Digest, certificate::Verifier as _};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const CHECKPOINT_INTERVAL: u64 = 100;
/// Blocks after a checkpoint until the block that must certify it (execution lags ordering).
pub const CHECKPOINT_DELAY: u64 = 4;
pub const CHUNK_SIZE: usize = 1 << 20;
/// Snapshots kept per node: the newest ones, and as many long-lived ones (at multiples of
/// [`long_interval`]).
pub const KEEP_SNAPSHOTS: usize = 2;
/// A served long-lived snapshot stays on disk at least this long (between one and two
/// [`long_interval`]s), so a joiner has that long to download it (F63).
pub const LONG_SNAPSHOT_MS: u64 = 15 * 60_000;
/// A snapshot (compressed) this small downloads well within the 100 blocks the newest one lives
/// at least (0.3 MiB/s at 1 s blocks), so the newest is served; a bigger state is served from the
/// newest long-lived snapshot instead.
pub const SMALL_SNAPSHOT_BYTES: u64 = 32 << 20;
/// A chunk request that takes longer than this (a 1 MiB chunk below ~35 KiB/s) fails, and the
/// chunk is asked of another peer (F62).
pub const CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// A peer that takes longer than this for a chunk (below ~100 KiB/s) is asked only once every
/// faster one has failed (F62).
pub const CHUNK_SLOW: std::time::Duration = std::time::Duration::from_secs(10);
/// A snapshot download must finish within this per chunk (~0.5 MiB/s overall) plus
/// [`DOWNLOAD_SLACK`], or the joiner gives up on that manifest and asks the next peer (F62).
pub const DOWNLOAD_PER_CHUNK: std::time::Duration = std::time::Duration::from_secs(2);
pub const DOWNLOAD_SLACK: std::time::Duration = std::time::Duration::from_secs(120);
/// A peer's validator-set history (its `/api/epochs` pages, from the joiner's root through the
/// manifest's epoch) must arrive within this per epoch it spans (each page that continues holds
/// a set change, so there are at most that many pages; 2 s is ~0.4 MiB/s for a change of a
/// hundred keys and their quorum) plus [`HISTORY_SLACK`], and never takes longer than
/// [`HISTORY_MAX`] (review-fix-sync-1).
pub const HISTORY_PER_EPOCH: std::time::Duration = std::time::Duration::from_secs(2);
pub const HISTORY_SLACK: std::time::Duration = std::time::Duration::from_secs(120);
pub const HISTORY_MAX: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// A manifest is refused before anything else is fetched for it when it certifies a block past
/// the height the chain can have reached by now plus this much clock difference (blocks are at
/// least `block_time_ms` apart from the genesis on; validators refuse timestamps more than
/// `consensus::MAX_CLOCK_DRIFT_MS` ahead).
pub const FUTURE_SLACK_MS: u64 = 3_600_000;

/// Blocks between long-lived snapshots at this chain's block time: the fewest whole checkpoint
/// intervals that last [`LONG_SNAPSHOT_MS`] (900 blocks at 1 s blocks; one interval on a chain
/// whose checkpoints are that far apart already, where every snapshot is long-lived).
pub fn long_interval(params: &crate::chain::Params) -> u64 {
    let per_interval_ms = CHECKPOINT_INTERVAL.saturating_mul(params.block_time_ms.max(1));
    CHECKPOINT_INTERVAL.saturating_mul(LONG_SNAPSHOT_MS.div_ceil(per_interval_ms).max(1))
}
/// Refuse to decompress a snapshot bigger than this.
pub const MAX_STATE_BYTES: u64 = 4 << 30;
/// Most chunks a manifest may list: `MAX_STATE_BYTES` of state, plus slack for a compressed
/// stream that ends up a little larger than its input. Checked before anything is downloaded.
pub const MAX_SNAPSHOT_CHUNKS: usize = (MAX_STATE_BYTES / CHUNK_SIZE as u64) as usize + 16;

pub fn is_checkpoint(height: u64) -> bool {
    height > 0 && height % CHECKPOINT_INTERVAL == 0
}

/// If `height` must certify a checkpoint, which one.
pub fn certifies(height: u64) -> Option<u64> {
    (height > CHECKPOINT_INTERVAL && height % CHECKPOINT_INTERVAL == CHECKPOINT_DELAY).then(|| height - CHECKPOINT_DELAY)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkInfo {
    pub hash: String,
    pub size: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub height: u64,
    pub state_root: String,
    pub chunks: Vec<ChunkInfo>,
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("snapshots")
}

/// First bytes of an encoded state.
const STATE_MAGIC: &[u8; 8] = b"BOOSTATE";

/// Writes the state as canonical records: `"BOOSTATE" ‖ schema u32 ‖ count u64`, then per record
/// `namespace u8 ‖ key_len u32 ‖ key ‖ value_len u32 ‖ value`, little-endian, in canonical order.
pub fn write_state(state: &State, w: &mut impl Write) -> Result<()> {
    w.write_all(STATE_MAGIC)?;
    w.write_all(&crate::state::codec::SCHEMA_VERSION.to_le_bytes())?;
    w.write_all(&(state.record_count() as u64).to_le_bytes())?;
    let mut err = Ok(());
    state.for_each_record(|n, k, v| {
        if err.is_ok() {
            err = (|| {
                w.write_all(&[n])?;
                w.write_all(&(k.len() as u32).to_le_bytes())?;
                w.write_all(k)?;
                w.write_all(&(v.len() as u32).to_le_bytes())?;
                w.write_all(v)
            })();
        }
    });
    Ok(err?)
}

/// The inverse of [`write_state`]: rebuilds the state and its tree from the records (strict:
/// see `State::from_records`). The caller checks the root.
pub fn read_state(r: &mut impl Read) -> Result<State> {
    fn u32_at(r: &mut impl Read) -> Result<u32> {
        let mut b = [0u8; 4];
        r.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn bytes(r: &mut impl Read, max: usize) -> Result<Vec<u8>> {
        let len = u32_at(r)? as usize;
        if len > max {
            bail!("record too large ({len} bytes)");
        }
        // Grows with the bytes actually there, so a length prefix alone can't allocate `max`.
        let mut b = Vec::new();
        r.take(len as u64).read_to_end(&mut b)?;
        if b.len() != len {
            bail!("state snapshot truncated inside a record");
        }
        Ok(b)
    }
    let mut head = [0u8; 20];
    r.read_exact(&mut head).context("state snapshot too short")?;
    if &head[..8] != STATE_MAGIC {
        bail!("not a Blockula state snapshot");
    }
    let schema = u32::from_le_bytes(head[8..12].try_into().unwrap());
    if schema != crate::state::codec::SCHEMA_VERSION {
        bail!("state snapshot schema {schema}, this binary reads {}", crate::state::codec::SCHEMA_VERSION);
    }
    let count = u64::from_le_bytes(head[12..20].try_into().unwrap());
    let records = (0..count).map(|_| {
        let mut n = [0u8; 1];
        r.read_exact(&mut n)?;
        Ok((n[0], bytes(r, crate::state::MAX_KEY_BYTES)?, bytes(r, crate::state::MAX_VALUE_BYTES)?))
    });
    let state = State::from_records(records)?;
    if r.read(&mut [0u8; 1])? != 0 {
        bail!("trailing bytes after the state records");
    }
    Ok(state)
}

/// Cuts a byte stream into `CHUNK_SIZE` chunk files (`chunk-{i}`), fsynced, hashing each.
struct ChunkWriter {
    dir: PathBuf,
    buf: Vec<u8>,
    chunks: Vec<ChunkInfo>,
}

impl ChunkWriter {
    fn cut(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut f = std::fs::File::create(self.dir.join(format!("chunk-{}", self.chunks.len())))?;
        f.write_all(&self.buf)?;
        f.sync_all()?;
        self.chunks.push(ChunkInfo { hash: hex::encode(crate::crypto::hash(&self.buf)), size: self.buf.len() });
        self.buf.clear();
        Ok(())
    }
}

impl Write for ChunkWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let n = data.len().min(CHUNK_SIZE - self.buf.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == CHUNK_SIZE {
            self.cut()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Writes the state at `height` as chunk files (call from a background thread with a clone).
/// Streams: records → zstd → chunks, so memory stays at a chunk plus the compressor's window.
/// The snapshot appears atomically (a `.tmp` directory renamed into place, then fsynced).
pub fn write(home: &Path, height: u64, state_root: &str, state: &State) -> Result<SnapshotMeta> {
    let tmp = dir(home).join(format!("{height}.tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut chunks = ChunkWriter { dir: tmp.clone(), buf: Vec::with_capacity(CHUNK_SIZE), chunks: vec![] };
    {
        let mut enc = std::io::BufWriter::with_capacity(1 << 16, zstd::Encoder::new(&mut chunks, 3)?);
        write_state(state, &mut enc)?;
        enc.into_inner().map_err(|e| e.into_error())?.finish()?;
    }
    chunks.cut()?;
    let meta = SnapshotMeta { height, state_root: state_root.to_string(), chunks: chunks.chunks };
    crate::store::write_durably(&tmp.join("meta.json"), &serde_json::to_vec_pretty(&meta)?)?;
    let final_dir = dir(home).join(height.to_string());
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&tmp, &final_dir)?;
    crate::store::sync_dir(&dir(home))?;
    prune(home, long_interval(&state.params));
    Ok(meta)
}

/// Removes snapshots a crash left half-written (`{height}.tmp`).
pub fn remove_partial(home: &Path) {
    let Ok(entries) = std::fs::read_dir(dir(home)) else { return };
    for e in entries.flatten() {
        if e.file_name().to_str().is_some_and(|n| n.ends_with(".tmp")) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Snapshot heights on disk, newest first.
pub fn list(home: &Path) -> Vec<u64> {
    let mut hs: Vec<u64> = std::fs::read_dir(dir(home)).map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect()).unwrap_or_default();
    hs.sort_unstable_by(|a, b| b.cmp(a));
    hs
}

/// The long-lived snapshots of `heights` (newest first, as [`list`] gives them): in each stretch
/// of `long` heights ([`long_interval`]), the first one this node wrote. That's the stretch's
/// first checkpoint, unless the writer was still busy with the one before, so peers usually hold
/// the same ones. Newest first.
fn long_lived(heights: &[u64], long: u64) -> Vec<u64> {
    let mut out: Vec<u64> = vec![];
    for &h in heights {
        match out.last_mut() {
            Some(l) if *l / long == h / long => *l = h,
            _ => out.push(h),
        }
    }
    out
}

/// The snapshot heights to keep, of `heights` (newest first): the newest [`KEEP_SNAPSHOTS`], and
/// the newest [`KEEP_SNAPSHOTS`] long-lived ones ([`long_lived`]). Whatever [`serving_order`]
/// puts first is among them, and stays until two newer ones of its kind are written: at least
/// `CHECKPOINT_INTERVAL` blocks for the newest, `long` for a long-lived one.
fn kept(heights: &[u64], long: u64) -> Vec<u64> {
    let mut keep: Vec<u64> = heights.iter().copied().take(KEEP_SNAPSHOTS).chain(long_lived(heights, long).into_iter().take(KEEP_SNAPSHOTS)).collect();
    keep.sort_unstable_by(|a, b| b.cmp(a));
    keep.dedup();
    keep
}

fn prune(home: &Path, long: u64) {
    let all = list(home);
    let keep = kept(&all, long);
    for h in all.into_iter().filter(|h| !keep.contains(h)) {
        let _ = std::fs::remove_dir_all(dir(home).join(h.to_string()));
    }
}

/// The order a manifest tries snapshots in (`heights` newest first): newest first, but when the
/// newest is bigger than `small` bytes, the long-lived ones first, so a joiner downloading a big
/// state has at least [`LONG_SNAPSHOT_MS`] before the snapshot is deleted (F63).
fn serving_order(heights: Vec<u64>, newest_bytes: u64, small: u64, long: u64) -> Vec<u64> {
    if newest_bytes <= small {
        return heights;
    }
    let mut first = long_lived(&heights, long);
    first.extend(heights.into_iter().filter(|h| !first.contains(h)).collect::<Vec<_>>());
    first
}

pub fn meta(home: &Path, height: u64) -> Option<SnapshotMeta> {
    serde_json::from_slice(&std::fs::read(dir(home).join(height.to_string()).join("meta.json")).ok()?).ok()
}

pub fn chunk(home: &Path, height: u64, i: usize) -> Option<Vec<u8>> {
    std::fs::read(dir(home).join(height.to_string()).join(format!("chunk-{i}"))).ok()
}

/// What a joining node needs to trust a snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub chain_id: String,
    pub height: u64,
    pub state_root: String,
    pub chunks: Vec<ChunkInfo>,
    /// Consensus blocks height+1 ..= height+len (hex), hash-linked; the last is certified.
    pub blocks: Vec<String>,
    /// Finalization certificate (hex) of the last block.
    pub finalization: String,
    /// The epoch anchor, when it's before `height` (and only then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

/// The last block of the epoch before the one a joiner enters, certified by that epoch's set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// The consensus block (hex): its height is what makes it the anchor.
    pub block: String,
    /// Its finalization certificate (hex).
    pub finalization: String,
}

/// The epoch anchor a manifest ending at `last` must certify: the previous epoch's last block,
/// unless it's the snapshot's own block or one of the manifest's (hash links cover those).
pub fn anchor_height(params: &crate::chain::Params, snapshot: u64, last: u64) -> Option<u64> {
    match params.epoch_of(last.saturating_add(1)) {
        0 => None,
        e => Some(params.last_of(e - 1)).filter(|a| *a < snapshot),
    }
}

/// The epoch anchor a fast-synced home started from, as its manifest proved it (written before
/// `base.json`, so it's there whenever the home is fast-synced).
fn anchor_file(home: &Path) -> PathBuf {
    home.join("anchor.json")
}

/// Saves (or, with `None`, clears) the anchor proof [`build_manifest`] serves on.
pub fn save_anchor(home: &Path, anchor: Option<&Anchor>) -> Result<()> {
    match anchor {
        Some(a) => crate::store::write_durably(&anchor_file(home), &serde_json::to_vec(a)?),
        None => match std::fs::remove_file(anchor_file(home)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        },
    }
}

/// The anchor at height `a` as this node can prove it: the block and certificate it executed, or
/// the anchor its own fast sync verified (`anchor.json`), which is all a fast-synced node has.
fn anchor_proof(home: &Path, a: u64, cblock: impl Fn(u64) -> Option<Vec<u8>>, cert: impl Fn(u64) -> Option<Vec<u8>>) -> Option<Anchor> {
    if let (Some(b), Some(f)) = (cblock(a), cert(a)) {
        return Some(Anchor { block: hex::encode(b), finalization: hex::encode(f) });
    }
    let saved: Anchor = serde_json::from_slice(&std::fs::read(anchor_file(home)).ok()?).ok()?;
    let block = CBlock::decode(hex::decode(&saved.block).ok()?.as_slice()).ok()?;
    (block.height == a).then_some(saved)
}

/// Builds the manifest for the newest snapshot this node can prove, from what it has published
/// (no consensus lock). The validator-set history isn't in it: `/api/epochs` pages serve that.
#[cfg_attr(not(test), allow(dead_code))] // tests and the simulator; `/api/snapshot/latest` passes the joiner's `min`
pub fn build_manifest(live: &crate::view::Live) -> Result<Manifest> {
    build_manifest_reaching(live, 0)
}

/// [`build_manifest`], for a snapshot whose manifest reaches at least `min_last` (a joiner's
/// newest checkpoint: an older one would be no use to it). A big state is served from a
/// long-lived snapshot when one reaches that far ([`serving_order`]). When none does, the one
/// reaching furthest: the joiner sees it's behind its checkpoint ([`SnapshotBehind`]) and waits
/// for the next snapshot, rather than taking "nothing here" for "sync from the genesis"
/// (review-fix-sync-3).
pub fn build_manifest_reaching(live: &crate::view::Live, min_last: u64) -> Result<Manifest> {
    build_manifest_with(live, min_last, SMALL_SNAPSHOT_BYTES)
}

/// [`build_manifest_reaching`], with a state counting as big past `small` bytes (tests).
pub(crate) fn build_manifest_with(live: &crate::view::Live, min_last: u64, small: u64) -> Result<Manifest> {
    let v = live.view();
    let home = &v.disk.home;
    let params = &v.state.params;
    let heights = list(home);
    let newest_bytes = heights.first().and_then(|h| meta(home, *h)).map_or(0, |m| m.chunks.iter().map(|c| c.size as u64).sum());
    // The servable manifest reaching furthest, when none reaches `min_last`.
    let mut behind: Option<(u64, Manifest)> = None;
    for height in serving_order(heights, newest_bytes, small, long_interval(params)) {
        let Some(meta) = meta(home, height) else { continue };
        let certifier = height + CHECKPOINT_DELAY;
        let mut blocks = vec![];
        let mut fin = None;
        for h in height + 1..=v.height.min(height + 2 * CHECKPOINT_INTERVAL) {
            let Some(b) = v.cblock(h) else { break };
            blocks.push(hex::encode(&b));
            if h >= certifier {
                if let Some(cert) = v.cert(h) {
                    fin = Some((h, hex::encode(cert)));
                    break;
                }
            }
        }
        let Some((last, finalization)) = fin else { continue };
        let anchor = match anchor_height(params, height, last) {
            None => None,
            // Without the anchor's proof this snapshot can't be served; an older one may be.
            Some(a) => match anchor_proof(home, a, |h| v.cblock(h), |h| v.cert(h)) {
                Some(p) => Some(p),
                None => continue,
            },
        };
        let m = Manifest { chain_id: v.genesis.chain_id.clone(), height, state_root: meta.state_root, chunks: meta.chunks, blocks, finalization, anchor };
        if last < min_last {
            if behind.as_ref().is_none_or(|(l, _)| last > *l) {
                behind = Some((last, m));
            }
            continue;
        }
        return Ok(m);
    }
    if let Some((_, m)) = behind {
        return Ok(m);
    }
    bail!("no certified snapshot yet (one is taken every {CHECKPOINT_INTERVAL} blocks and certified {CHECKPOINT_DELAY} blocks later, and the epoch anchor before it must be certified too)")
}

/// A manifest whose certificate, block links, checkpoint commitment and epoch anchor all check out.
pub struct Verified {
    pub height: u64,
    pub state_root: String,
    pub chunks: Vec<ChunkInfo>,
    pub blocks: Vec<CBlock>,
    pub finalization: Vec<u8>,
    /// The validator-set history verified for it: from the trusted root it was checked from
    /// through the last block's epoch, and nothing after (F08).
    pub epochs: EpochRegistry,
    /// The epoch anchor's height and hash, when it's before the snapshot (certified) ...
    pub epoch_start: Option<(u64, String)>,
    /// ... and its proof, which this node serves on in its own manifests.
    pub anchor: Option<Anchor>,
}

impl Verified {
    /// Hash of the block at `height`, if the manifest proves it: the snapshot's block, the epoch
    /// anchor, or one of the manifest's blocks.
    pub fn hash_at(&self, height: u64) -> Option<String> {
        if height == self.height {
            return self.blocks.first().map(|b| hex::encode(b.parent_bytes()));
        }
        if let Some((_, hash)) = self.epoch_start.as_ref().filter(|(a, _)| *a == height) {
            return Some(hash.clone());
        }
        let i = height.checked_sub(self.height.saturating_add(1))?;
        self.blocks.get(usize::try_from(i).ok()?).map(|b| hex::encode(b.digest_bytes()))
    }

    /// Checks the snapshot against the checkpoints this node trusts (`checkpoints.rs`). The
    /// manifest proves blocks from the snapshot on (and the anchor), and the validator sets up to
    /// its last block's epoch: a checkpoint is checked against whichever of those it names. One
    /// that's newer than all of it means the peer is behind ([`SnapshotBehind`]). One that's older
    /// and pins no set can't be checked, so it's refused rather than ignored, unless a newer pinned
    /// set checked out (history before that set no longer matters).
    pub fn check_trust(&self, trust: &crate::checkpoints::Trust) -> Result<()> {
        let last = self.blocks.last().map(|b| b.height).unwrap_or(self.height);
        let last_epoch = trust.epoch_of(last);
        let (mut unchecked, mut newest_pin) = (vec![], None);
        for cp in trust.checkpoints().filter(|c| c.height > 0) {
            if cp.height > last {
                return Err(anyhow::Error::new(SnapshotBehind { certified: last, checkpoint: cp.height }));
            }
            let mut checked = false;
            if let Some(hash) = self.hash_at(cp.height) {
                trust.check_block(cp.height, &hash)?;
                checked = true;
            }
            let pinned = trust.pinned_epoch(cp);
            if cp.set_hash.is_some() && pinned <= last_epoch {
                if pinned < self.epochs.base {
                    // Before the root the history was verified from: a newer pinned set (that
                    // root) supersedes it.
                    checked = true;
                } else {
                    // `verify_manifest` verified every set change from its root through `last_epoch`.
                    let set = self.epochs.set(pinned).ok_or_else(|| anyhow!("the manifest's history doesn't reach epoch {pinned}"))?;
                    trust.check_set(pinned, set)?;
                    (checked, newest_pin) = (true, Some(cp.height));
                }
            }
            if !checked {
                unchecked.push(cp.height);
            }
        }
        if let Some(h) = unchecked.into_iter().find(|h| newest_pin.is_none_or(|p| *h > p)) {
            bail!(
                "trusted checkpoint #{h} is older than this snapshot (#{}) and pins no validator set, so the snapshot can't be checked against it: pass its set hash too (HEIGHT:HASH:SET_HASH, `blockula checkpoint` prints it), or sync from genesis with --full",
                self.height
            );
        }
        Ok(())
    }
}

/// A trusted checkpoint is newer than everything a peer's manifest certifies: the peer is behind
/// (or its snapshots are older than the checkpoint), so another peer or a later snapshot may do.
#[derive(Debug)]
pub struct SnapshotBehind {
    pub certified: u64,
    pub checkpoint: u64,
}

impl std::fmt::Display for SnapshotBehind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the snapshot is certified through #{}, before trusted checkpoint #{}", self.certified, self.checkpoint)
    }
}

impl std::error::Error for SnapshotBehind {}

/// The verifier for a validator set (hex consensus keys) on the chain instance whose consensus
/// namespace is `ns` (`consensus::genesis_namespace`).
fn verifier_for(ns: &[u8], set: &[String]) -> Result<MlDsaScheme> {
    let participants = consensus::keys_of(set.iter().map(|s| s.as_str())).ok_or_else(|| anyhow!("bad validator keys"))?;
    Ok(MlDsaScheme::verifier(ns, participants))
}

/// Checks the epoch anchor at height `a`: the block's height, and a valid finalization of it by
/// the set of its epoch (`set`). Returns its hash.
fn verify_anchor(g: &Genesis, ns: &[u8], set: &[String], a: u64, p: &Anchor) -> Result<String> {
    let block = CBlock::decode(hex::decode(&p.block)?.as_slice())?;
    if block.height != a {
        bail!("the anchor block is #{}, expected #{a}", block.height);
    }
    let epoch = g.params().epoch_of(a);
    let verifier = verifier_for(ns, set)?;
    let fin = Finalization::<MlDsaScheme, Digest>::decode_cfg(hex::decode(&p.finalization)?.as_slice(), &verifier.certificate_codec_config())?;
    if fin.proposal.payload != block.digest() {
        bail!("the anchor certificate is for another block");
    }
    if fin.proposal.round.epoch().get() != epoch {
        bail!("the anchor certificate is from epoch {}, expected {epoch}", fin.proposal.round.epoch().get());
    }
    if !fin.verify(&mut commonware_utils::sys_rng(), &verifier, &commonware_parallel::Sequential) {
        bail!("invalid anchor certificate");
    }
    Ok(hex::encode(block.digest_bytes()))
}

/// The epoch of a manifest's last block (what its certificate and history must reach), without
/// decoding it: the blocks run from `height + 1`.
pub fn last_epoch(g: &Genesis, m: &Manifest) -> u64 {
    g.params().epoch_of(m.height.saturating_add(m.blocks.len() as u64))
}

/// The root a joiner verifies a manifest's history from: its newest trusted one for the epoch of
/// the manifest's epoch anchor (whose set certifies it), or of its last block when it needs none.
pub fn trusted_root(g: &Genesis, m: &Manifest, trust: &crate::checkpoints::Trust) -> epochs::Root {
    let params = g.params();
    let last = m.height.saturating_add(m.blocks.len() as u64);
    trust.root(anchor_height(&params, m.height, last).map_or(params.epoch_of(last), |a| params.epoch_of(a)))
}

/// [`verify_manifest_from`] with the history verified from the genesis set.
#[cfg_attr(not(test), allow(dead_code))] // tests and the fuzz targets; joiners verify from their trusted root
pub fn verify_manifest(g: &Genesis, m: &Manifest, history: &EpochRegistry) -> Result<Verified> {
    verify_manifest_from(g, m, history, &epochs::Root::genesis(g))
}

/// Verifies a manifest and the validator-set history it's checked against (`history`, from the
/// peer's `/api/epochs`), the history from `root`. What's verified of the history (`root`
/// through the last block's epoch) is all [`Verified`] keeps.
pub fn verify_manifest_from(g: &Genesis, m: &Manifest, history: &EpochRegistry, root: &epochs::Root) -> Result<Verified> {
    if m.chain_id != g.chain_id {
        bail!("manifest is for chain {}", m.chain_id);
    }
    if !is_checkpoint(m.height) {
        bail!("{} isn't a checkpoint height", m.height);
    }
    // Bounds what a joiner downloads and holds before the root check.
    if m.chunks.len() > MAX_SNAPSHOT_CHUNKS {
        bail!("manifest lists {} chunks, at most {MAX_SNAPSHOT_CHUNKS}", m.chunks.len());
    }
    if m.chunks.iter().any(|c| c.size == 0 || c.size > CHUNK_SIZE) {
        bail!("manifest chunk sizes must be 1..={CHUNK_SIZE} bytes");
    }
    let blocks: Vec<CBlock> = m.blocks.iter().map(|h| Ok(CBlock::decode(hex::decode(h)?.as_slice())?)).collect::<Result<_>>()?;
    let mut expect = m.height + 1;
    for w in blocks.windows(2) {
        if w[1].parent != w[0].digest() {
            bail!("manifest blocks aren't hash-linked at height {}", w[1].height);
        }
    }
    for b in &blocks {
        if b.height != expect {
            bail!("manifest blocks aren't contiguous at {}", b.height);
        }
        // A peer's heights near u64::MAX must not overflow (overflow checks panic in release).
        expect = expect.checked_add(1).ok_or_else(|| anyhow!("manifest block heights overflow"))?;
    }
    // The block at C + d commits to the checkpoint's state root.
    let certifier = blocks.iter().find(|b| b.height == m.height + CHECKPOINT_DELAY).ok_or_else(|| anyhow!("manifest doesn't include the certifying block"))?;
    if certifier.exec_height != m.height || hex::encode(certifier.exec_root) != m.state_root {
        bail!("certifying block doesn't commit to this snapshot's state root");
    }
    let last = blocks.last().unwrap();
    let params = g.params();
    let epoch = params.epoch_of(last.height);
    // Verifies every set change from the root through `epoch`; only that part is kept, so
    // `epochs.set(e)` is trusted for root ≤ e ≤ epoch, nothing past `epoch` is adopted, and of
    // each change's signatures only a quorum that verified (review-fix-sync-2, -5).
    let (set, verified) = epochs::verify_history(g, history, root, epoch).context("validator set history")?;
    // Certificates of this chain instance only (another genesis under the same id never verifies).
    let ns = consensus::genesis_namespace(g);
    let verifier = verifier_for(&ns, &set)?;
    let fin_bytes = hex::decode(&m.finalization)?;
    let fin = Finalization::<MlDsaScheme, Digest>::decode_cfg(fin_bytes.as_slice(), &verifier.certificate_codec_config())?;
    if fin.proposal.payload != last.digest() {
        bail!("certificate isn't for the last manifest block");
    }
    if fin.proposal.round.epoch().get() != epoch {
        bail!("certificate is from epoch {}, expected {epoch}", fin.proposal.round.epoch().get());
    }
    if !fin.verify(&mut commonware_utils::sys_rng(), &verifier, &commonware_parallel::Sequential) {
        bail!("invalid finalization certificate");
    }
    let epoch_start = match anchor_height(&params, m.height, last.height) {
        None if m.anchor.is_some() => bail!("manifest carries an epoch anchor it doesn't need"),
        None => None,
        Some(a) => {
            let p = m.anchor.as_ref().ok_or_else(|| anyhow!("manifest doesn't certify the epoch anchor #{a}"))?;
            // a < m.height < last, so its epoch's set was verified above (when it's at or after
            // the root; before it, the root's pin is what's trusted).
            let set = verified.set(params.epoch_of(a)).ok_or_else(|| anyhow!("no verified validator set for the anchor's epoch {} (the trusted root is at epoch {})", params.epoch_of(a), root.epoch))?;
            Some((a, verify_anchor(g, &ns, set, a, p).context("epoch anchor")?))
        }
    };
    Ok(Verified { height: m.height, state_root: m.state_root.clone(), chunks: m.chunks.clone(), blocks, finalization: fin_bytes, epochs: verified, anchor: m.anchor.clone().filter(|_| epoch_start.is_some()), epoch_start })
}

/// [`verify_manifest_from`] the newest root `trust` has for the manifest's epoch, then
/// [`Verified::check_trust`]: what a joiner runs on a peer's manifest.
pub fn verify_manifest_trusted(g: &Genesis, m: &Manifest, history: &EpochRegistry, trust: &crate::checkpoints::Trust) -> Result<Verified> {
    let v = verify_manifest_from(g, m, history, &trusted_root(g, m, trust))?;
    v.check_trust(trust)?;
    Ok(v)
}

/// A peer's validator-set history from `from` through at least `through`, assembled from its
/// `/api/epochs` pages (each capped, and the whole at `epochs::MAX_HISTORY_BYTES`), within
/// [`DownloadLimits::history`]. A page that says there's more must hold a set change and end
/// where the next one starts, as [`EpochRegistry::slice`] pages do, so there are at most as many
/// as the epochs spanned: a peer can't hold a joiner with an endless stream of pages
/// (review-fix-sync-1).
pub async fn fetch_history(client: &reqwest::Client, peer: &str, from: u64, through: u64, limits: &DownloadLimits) -> Result<EpochRegistry> {
    let deadline = limits.history(from, through);
    tokio::time::timeout(deadline, fetch_pages(client, peer, from, through))
        .await
        .map_err(|_| anyhow!("{peer}'s validator-set history from epoch {from} through {through} didn't arrive within {}s", deadline.as_secs()))?
}

async fn fetch_pages(client: &reqwest::Client, peer: &str, from: u64, through: u64) -> Result<EpochRegistry> {
    #[derive(Deserialize)]
    struct Next {
        #[serde(default)]
        next: Option<u64>,
    }
    let mut total = 0usize;
    let mut out: Option<EpochRegistry> = None;
    let mut at = from;
    loop {
        let body = crate::node::get_capped(client, &format!("{peer}/api/epochs?from={at}"), epochs::MAX_PAGE_BYTES).await.with_context(|| format!("{peer}/api/epochs?from={at}"))?;
        total += body.len();
        ensure!(total <= epochs::MAX_HISTORY_BYTES, "{peer}'s validator-set history from epoch {from} passes {} MiB", epochs::MAX_HISTORY_BYTES >> 20);
        let page: EpochRegistry = serde_json::from_slice(&body).with_context(|| format!("{peer}/api/epochs?from={at}"))?;
        let next = serde_json::from_slice::<Next>(&body).map(|n| n.next).unwrap_or_default();
        if let Some(n) = next {
            ensure!(
                n > at && n == page.known_through && page.changes().any(|e| e <= page.known_through),
                "{peer}/api/epochs?from={at}: a page that continues must hold a set change and end where the next one starts"
            );
        }
        match &mut out {
            None => out = Some(page),
            Some(o) => o.append_page(&page)?,
        }
        let o = out.as_ref().expect("set above");
        match next {
            Some(n) if o.known_through < through && n > at => at = n,
            _ => return Ok(out.expect("set above")),
        }
    }
}

/// Checks one downloaded chunk against the manifest.
pub fn check_chunk(v: &Verified, i: usize, bytes: &[u8]) -> Result<()> {
    let info = v.chunks.get(i).ok_or_else(|| anyhow!("no chunk {i}"))?;
    if bytes.len() != info.size || hex::encode(crate::crypto::hash(bytes)) != info.hash {
        bail!("chunk {i} doesn't match the manifest");
    }
    Ok(())
}

/// Rebuilds the state (and its tree) from verified chunks held in memory (tests; a joiner streams
/// them through a file, [`restore_file`]) and checks it against the certified
/// root. The state carries its own parameters, so `_g` isn't needed (the manifest was checked
/// against it already). Memory before the root check is bounded: the chunks (at most
/// `MAX_SNAPSHOT_CHUNKS`) are decompressed as a stream, never into one buffer, each record is
/// capped (`state::MAX_KEY_BYTES`, `state::MAX_VALUE_BYTES`) and so is the total
/// (`MAX_STATE_BYTES`); what remains is the state being rebuilt.
#[cfg(test)]
pub fn restore(_g: &Genesis, v: &Verified, chunks: &[Vec<u8>]) -> Result<State> {
    if chunks.len() != v.chunks.len() {
        bail!("expected {} chunks", v.chunks.len());
    }
    for (i, c) in chunks.iter().enumerate() {
        check_chunk(v, i, c)?;
    }
    let state = decode_state_from(Chunks { chunks, at: 0, off: 0 })?;
    if state.root() != v.state_root {
        bail!("rebuilt state root doesn't match the certified root");
    }
    Ok(state)
}

/// Rebuilds the state from a packed file whose chunks were checked already (a joiner's
/// download), streaming, and checks it against the certified root.
pub fn restore_file(v: &Verified, packed: &Path) -> Result<State> {
    let state = decode_state_file(packed)?;
    if state.root() != v.state_root {
        bail!("rebuilt state root doesn't match the certified root");
    }
    Ok(state)
}

/// Decompresses (up to `MAX_STATE_BYTES`) and decodes a packed snapshot.
#[cfg(test)]
pub fn decode_state(packed: &[u8]) -> Result<State> {
    decode_state_from(packed)
}

/// [`decode_state`] from a file, as a stream (`base.state.zst`).
pub fn decode_state_file(path: &Path) -> Result<State> {
    decode_state_from(std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?))
}

/// One of this node's own snapshots (a rebuild after pruning): every chunk checked against its
/// `meta.json`, then decoded as a stream and checked against the root it records.
pub fn read_local(home: &Path, height: u64) -> Result<State> {
    let meta = meta(home, height).ok_or_else(|| anyhow!("snapshot {height} has no meta.json"))?;
    let snap = dir(home).join(height.to_string());
    for (i, c) in meta.chunks.iter().enumerate() {
        let bytes = std::fs::read(snap.join(format!("chunk-{i}")))?;
        if bytes.len() != c.size || hex::encode(crate::crypto::hash(&bytes)) != c.hash {
            bail!("chunk {i} of snapshot {height} doesn't match its meta.json");
        }
    }
    let files = (0..meta.chunks.len()).map(|i| snap.join(format!("chunk-{i}"))).collect();
    let state = decode_state_from(ChunkFiles { files, cur: None })?;
    if state.root() != meta.state_root {
        bail!("snapshot {height} doesn't decode to its state root");
    }
    Ok(state)
}

/// Chunk files read in order as one stream.
struct ChunkFiles {
    files: std::collections::VecDeque<PathBuf>,
    cur: Option<std::fs::File>,
}

impl Read for ChunkFiles {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.cur.is_none() {
                let Some(p) = self.files.pop_front() else { return Ok(0) };
                self.cur = Some(std::fs::File::open(p)?);
            }
            match self.cur.as_mut().unwrap().read(buf)? {
                0 => self.cur = None,
                n => return Ok(n),
            }
        }
    }
}

/// [`decode_state`] from a stream of packed bytes.
fn decode_state_from(packed: impl Read) -> Result<State> {
    read_state(&mut std::io::BufReader::new(zstd::Decoder::new(packed)?.take(MAX_STATE_BYTES)))
}

/// Reads a list of chunks as one stream, without concatenating them.
#[cfg(test)]
struct Chunks<'a> {
    chunks: &'a [Vec<u8>],
    at: usize,
    off: usize,
}

#[cfg(test)]
impl Read for Chunks<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while let Some(c) = self.chunks.get(self.at) {
            if self.off < c.len() {
                let n = buf.len().min(c.len() - self.off);
                buf[..n].copy_from_slice(&c[self.off..self.off + n]);
                self.off += n;
                return Ok(n);
            }
            (self.at, self.off) = (self.at + 1, 0);
        }
        Ok(0)
    }
}

/// Fetches, verifies and installs a certified snapshot (what a peer serves: its newest, or a big
/// state's newest long-lived one) from `peers` into an empty chain, refusing any that contradicts
/// `trust`. Peers are tried in a random order; each one's manifest is downloaded from all of them
/// ([`download_chunks`]).
pub async fn fast_sync(chain: &crate::consensus::Shared, peers: &[String], trust: &crate::checkpoints::Trust) -> Result<u64> {
    fast_sync_with(chain, peers, trust, &DownloadLimits::default()).await
}

/// [`fast_sync`] within `limits`.
pub async fn fast_sync_with(chain: &crate::consensus::Shared, peers: &[String], trust: &crate::checkpoints::Trust, limits: &DownloadLimits) -> Result<u64> {
    let client = reqwest::Client::builder().timeout(MANIFEST_TIMEOUT).build()?;
    let g = chain.lock().genesis.clone();
    let mut last_err = anyhow!("no peers");
    // A peer that's only behind a checkpoint says a later snapshot will do: report that one.
    let mut behind = None;
    // In a new order each run, so no listed endpoint is always the one asked first (F62).
    let peers = shuffled(peers);
    for peer in &peers {
        match fast_sync_from(&client, &g, chain, peer, &peers, trust, limits).await {
            Ok(h) => return Ok(h),
            Err(e) => {
                tracing::debug!(peer = %peer, error = %e, "fast sync failed");
                let e = e.context(format!("from {peer}"));
                if e.chain().any(|c| c.is::<SnapshotBehind>()) {
                    behind = Some(e);
                } else {
                    last_err = e;
                }
            }
        }
    }
    Err(behind.unwrap_or(last_err))
}

/// `peers` in a random order.
fn shuffled(peers: &[String]) -> Vec<String> {
    let mut out = peers.to_vec();
    for i in (1..out.len()).rev() {
        let mut r = [0u8; 8];
        let j = match getrandom::fill(&mut r) {
            Ok(()) => (u64::from_le_bytes(r) % (i as u64 + 1)) as usize,
            Err(_) => i,
        };
        out.swap(i, j);
    }
    out
}

/// How long a fast-sync attempt may take: the snapshot download ([`CHUNK_TIMEOUT`],
/// [`CHUNK_SLOW`], [`DOWNLOAD_PER_CHUNK`], [`DOWNLOAD_SLACK`]) and the history before it
/// ([`HISTORY_PER_EPOCH`], [`HISTORY_SLACK`], [`HISTORY_MAX`]); tests shorten them.
#[derive(Clone, Debug)]
pub struct DownloadLimits {
    pub chunk_timeout: std::time::Duration,
    pub slow: std::time::Duration,
    pub per_chunk: std::time::Duration,
    pub slack: std::time::Duration,
    pub history_per_epoch: std::time::Duration,
    pub history_slack: std::time::Duration,
    pub history_max: std::time::Duration,
}

impl Default for DownloadLimits {
    fn default() -> Self {
        Self { chunk_timeout: CHUNK_TIMEOUT, slow: CHUNK_SLOW, per_chunk: DOWNLOAD_PER_CHUNK, slack: DOWNLOAD_SLACK, history_per_epoch: HISTORY_PER_EPOCH, history_slack: HISTORY_SLACK, history_max: HISTORY_MAX }
    }
}

impl DownloadLimits {
    /// The most a download of `chunks` chunks may take.
    pub fn download(&self, chunks: usize) -> std::time::Duration {
        self.slack.saturating_add(self.per_chunk.saturating_mul(u32::try_from(chunks).unwrap_or(u32::MAX)))
    }

    /// The most a history from epoch `from` through `through` may take.
    pub fn history(&self, from: u64, through: u64) -> std::time::Duration {
        let epochs = u32::try_from(through.saturating_sub(from).saturating_add(1)).unwrap_or(u32::MAX);
        self.history_slack.saturating_add(self.history_per_epoch.saturating_mul(epochs)).min(self.history_max)
    }
}

/// Downloads a verified snapshot's chunks into `out`, four at a time, each checked against the
/// manifest as it arrives. Chunks are identified by their hashes, so any peer holding the same
/// snapshot can serve them, and they're spread over `sources` (chunk `i` asks source `i mod n`
/// first, then the next). A source that fails a chunk (an error, a 404, a wrong chunk, or no
/// answer within `chunk_timeout`) isn't asked again; one slower than `limits.slow` is asked only
/// once every faster one has failed. The whole download must finish within the deadline the
/// limits give for this many chunks, so no peer holds a joiner longer than that (F62).
pub async fn download_chunks(client: &reqwest::Client, sources: &[String], v: &Verified, out: &mut impl Write, limits: &DownloadLimits) -> Result<()> {
    use std::sync::atomic::{AtomicU8, Ordering};
    const OK: u8 = 0;
    const SLOW: u8 = 1;
    const FAILED: u8 = 2;
    ensure!(!sources.is_empty(), "no peer to download from");
    let deadline = limits.download(v.chunks.len());
    let health: Vec<AtomicU8> = sources.iter().map(|_| AtomicU8::new(OK)).collect();
    let fetch = |i: usize| {
        let health = &health;
        async move {
            let mut last = anyhow!("no peer left that serves snapshot #{} (chunk {i})", v.height);
            for pass in [OK, SLOW] {
                for k in 0..sources.len() {
                    let s = (i + k) % sources.len();
                    if health[s].load(Ordering::Relaxed) != pass {
                        continue;
                    }
                    let src = &sources[s];
                    let url = format!("{src}/api/snapshot/{}/chunk/{i}", v.height);
                    let started = std::time::Instant::now();
                    let got = match tokio::time::timeout(limits.chunk_timeout, crate::node::get_capped(client, &url, CHUNK_SIZE + 1024)).await {
                        Ok(Ok(bytes)) => check_chunk(v, i, &bytes).map(|()| bytes),
                        Ok(Err(e)) => Err(e),
                        Err(_) => Err(anyhow!("no chunk within {}s", limits.chunk_timeout.as_secs_f64())),
                    };
                    match got {
                        Ok(bytes) => {
                            if started.elapsed() > limits.slow {
                                tracing::debug!(peer = %src, chunk = i, took = ?started.elapsed(), "slow peer; asking the others first");
                                let _ = health[s].compare_exchange(OK, SLOW, Ordering::Relaxed, Ordering::Relaxed);
                            }
                            return Ok(bytes);
                        }
                        Err(e) => {
                            tracing::debug!(peer = %src, chunk = i, error = %format!("{e:#}"), "chunk failed; asking another peer");
                            health[s].store(FAILED, Ordering::Relaxed);
                            last = e.context(format!("chunk {i} from {src}"));
                        }
                    }
                }
            }
            Err(last)
        }
    };
    let all = async {
        for batch in (0..v.chunks.len()).collect::<Vec<_>>().chunks(4) {
            for r in futures::future::join_all(batch.iter().map(|i| fetch(*i))).await {
                out.write_all(&r?)?;
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::time::timeout(deadline, all).await.map_err(|_| anyhow!("the snapshot's {} chunks didn't arrive within {}s", v.chunks.len(), deadline.as_secs()))?
}

/// How long the request for a manifest (and any other single request of a fast sync) may take.
pub const MANIFEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

async fn fast_sync_from(client: &reqwest::Client, g: &Genesis, chain: &crate::consensus::Shared, peer: &str, peers: &[String], trust: &crate::checkpoints::Trust, limits: &DownloadLimits) -> Result<u64> {
    // A snapshot older than our newest checkpoint is no use: ask for one that reaches it.
    let body = crate::node::get_capped(client, &format!("{peer}/api/snapshot/latest?min={}", trust.newest()), 64 << 20).await?;
    let m: Manifest = serde_json::from_slice(&body)?;
    ensure!(m.blocks.len() as u64 <= 4 * CHECKPOINT_INTERVAL, "the manifest lists {} blocks", m.blocks.len());
    ensure!(m.chunks.len() <= MAX_SNAPSHOT_CHUNKS, "manifest lists {} chunks, at most {MAX_SNAPSHOT_CHUNKS}", m.chunks.len());
    let last = m.height.saturating_add(m.blocks.len() as u64);
    // Nothing is fetched for a manifest of blocks the chain can't have yet: the history it would
    // need is the peer's to make up, as long as it likes (review-fix-sync-1).
    let expected = trust.expected_height(crate::chain::now_ms().saturating_add(FUTURE_SLACK_MS));
    ensure!(last <= expected, "the manifest certifies #{last}, past any height the chain can have reached by now (#{expected})");
    // Ends before our newest checkpoint (an old-binary peer serves its newest anyway): a later
    // snapshot will do, so say so without fetching anything for it (review-fix-sync-3).
    if trust.newest() > last {
        return Err(anyhow::Error::new(SnapshotBehind { certified: last, checkpoint: trust.newest() }));
    }
    // One deadline over the rest of the attempt: the history, then the chunks (F62).
    let (from, through) = (trusted_root(g, &m, trust).epoch, last_epoch(g, &m));
    let deadline = tokio::time::Instant::now() + limits.history(from, through).saturating_add(limits.download(m.chunks.len()));
    let late = || anyhow!("fast sync from {peer} didn't finish within its deadline");
    // The history from our newest trusted root through the manifest's epoch, nothing more.
    let history = tokio::time::timeout_at(deadline, fetch_history(client, peer, from, through, limits)).await.map_err(|_| late())??;
    let v = verify_manifest_trusted(g, &m, &history, trust)?;
    println!("📦 snapshot #{} ({} chunks) certified by validators; downloading…", v.height, v.chunks.len());
    // Each chunk is checked as it arrives and appended to a file: memory holds a few chunks at
    // most, never the whole snapshot.
    let part = chain.lock().home.join("base.state.zst.part");
    let mut file = std::io::BufWriter::new(std::fs::File::create(&part)?);
    // The manifest's peer first, then the others (any of them may hold the same snapshot).
    let sources: Vec<String> = std::iter::once(peer.to_string()).chain(peers.iter().filter(|p| *p != peer).cloned()).collect();
    tokio::time::timeout_at(deadline, download_chunks(client, &sources, &v, &mut file, limits)).await.map_err(|_| late())??;
    file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    let state = restore_file(&v, &part)?;
    // The epoch anchor (if it's before the snapshot) came certified in the manifest.
    let mut c = chain.lock();
    c.install_snapshot(&v, state, &part)?;
    Ok(c.height())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Account;
    use crate::checkpoints::{Checkpoint, Pins, Trust};
    use crate::pq::PrivateKey;
    use commonware_codec::Encode as _;
    use commonware_consensus::simplex::types::{Finalize, Proposal};
    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_cryptography::Signer as _;

    /// A 4-validator chain with 10-block epochs (so a snapshot at 100 sits in epoch 10, and its
    /// anchor, #99, ends epoch 9), and its validators' seeds.
    fn four_validators(tag: &str) -> (PathBuf, Genesis, Vec<[u8; 32]>) {
        let root = std::env::temp_dir().join(format!("blockula-anchor-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = crate::chain::Chain::summon(&root, &crate::chain::SummonOpts { epoch_blocks: 10, ..crate::chain::SummonOpts::new(tag, 4) }).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        let seeds = homes.iter().map(|h| hex::decode(std::fs::read_to_string(h.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap()).collect();
        (root, g, seeds)
    }

    fn keys(seeds: &[[u8; 32]]) -> Vec<String> {
        let mut k: Vec<String> = seeds.iter().map(|s| hex::encode(PrivateKey::from_seed_bytes(*s).public_key().as_ref())).collect();
        k.sort();
        k
    }

    /// A finalization of `block` in `epoch` by 3 of the 4 `seeds` (a quorum), hex.
    fn finalization(g: &Genesis, seeds: &[[u8; 32]], block: &CBlock, epoch: u64) -> String {
        let participants = consensus::keys_of(keys(seeds).iter().map(String::as_str)).unwrap();
        let ns = consensus::genesis_namespace(g);
        let p = Proposal::new(Round::new(Epoch::new(epoch), View::new(block.height)), View::new(block.height - 1), block.digest());
        let votes: Vec<_> = seeds.iter().take(3).map(|s| Finalize::sign(&MlDsaScheme::signer(&ns, participants.clone(), PrivateKey::from_seed_bytes(*s)).unwrap(), p.clone()).unwrap()).collect();
        let fin = Finalization::from_finalizes(&MlDsaScheme::verifier(&ns, participants), commonware_utils::iter::NonEmpty::try_new(votes.iter()).unwrap(), &commonware_parallel::Sequential).unwrap();
        hex::encode(fin.encode())
    }

    fn anchor_block(height: u64) -> CBlock {
        CBlock::new(Digest(crate::crypto::hash(b"before the anchor")), height, height, height - 1, 1000 * height, String::new(), 0, [0; 32], vec![])
    }

    /// A manifest for a snapshot at 100 with blocks 101..=`last` (104 certifies the state root),
    /// signed by `seeds` (the set of every epoch its history has), with the epoch anchor if it
    /// needs one.
    fn manifest(g: &Genesis, seeds: &[[u8; 32]], last: u64) -> Manifest {
        let state_root = crate::crypto::hash(b"the state at 100");
        let mut blocks = vec![];
        let mut parent = Digest(crate::crypto::hash(b"block 100"));
        for h in 101..=last {
            let (exec_height, exec_root) = if h == 104 { (100, state_root) } else { (99, [7; 32]) };
            let b = CBlock::new(parent, h, h, h - 1, 1000 * h, String::new(), exec_height, exec_root, vec![]);
            parent = b.digest();
            blocks.push(b);
        }
        let params = g.params();
        let anchor = anchor_height(&params, 100, last).map(|a| {
            let b = anchor_block(a);
            Anchor { block: hex::encode(b.encode()), finalization: finalization(g, seeds, &b, params.epoch_of(a)) }
        });
        Manifest {
            chain_id: g.chain_id.clone(),
            height: 100,
            state_root: hex::encode(state_root),
            chunks: vec![ChunkInfo { hash: hex::encode(crate::crypto::hash(b"c")), size: 1 }],
            blocks: blocks.iter().map(|b| hex::encode(b.encode())).collect(),
            finalization: finalization(g, seeds, blocks.last().unwrap(), params.epoch_of(last)),
            anchor,
        }
    }

    /// The genesis set through `through`, unchanged.
    fn steady(g: &Genesis, through: u64) -> EpochRegistry {
        let mut reg = EpochRegistry::genesis(g);
        let set = reg.sets[&0].clone();
        for e in 1..=through {
            reg.record(e, set.clone());
        }
        reg
    }

    fn err(r: Result<Verified>) -> String {
        format!("{:#}", r.err().expect("refused"))
    }

    /// CRY-2: the epoch anchor before a snapshot comes with a finalization by its epoch's set, and
    /// the joiner checks the block's height, that the certificate is for it, from that epoch, and
    /// valid. A manifest that doesn't need an anchor can't carry one.
    #[test]
    fn epoch_anchor_is_certified_by_the_previous_epochs_set() {
        let (root, g, seeds) = four_validators("anchor");
        let m = manifest(&g, &seeds, 104);
        let h = steady(&g, 10);
        let v = verify_manifest(&g, &m, &h).unwrap();
        let anchor_hash = hex::encode(anchor_block(99).digest_bytes());
        assert_eq!(v.epoch_start, Some((99, anchor_hash.clone())));
        assert_eq!(v.anchor, m.anchor);
        assert_eq!((v.hash_at(99), v.hash_at(100)), (Some(anchor_hash), Some(hex::encode(v.blocks[0].parent_bytes()))));
        assert_eq!((v.hash_at(103), v.hash_at(98), v.hash_at(105)), (Some(hex::encode(v.blocks[2].digest_bytes())), None, None));
        let mut missing = m.clone();
        missing.anchor = None;
        assert!(err(verify_manifest(&g, &missing, &h)).contains("doesn't certify the epoch anchor #99"));
        let mut tampered = m.clone();
        let mut f = hex::decode(&tampered.anchor.as_ref().unwrap().finalization).unwrap();
        let n = f.len();
        f[n - 100] ^= 0xff;
        tampered.anchor.as_mut().unwrap().finalization = hex::encode(f);
        assert!(err(verify_manifest(&g, &tampered, &h)).contains("epoch anchor"));
        let with = |block: &CBlock, epoch: u64| {
            let mut m = m.clone();
            m.anchor = Some(Anchor { block: hex::encode(block.encode()), finalization: finalization(&g, &seeds, block, epoch) });
            m
        };
        assert!(err(verify_manifest(&g, &with(&anchor_block(99), 10), &h)).contains("from epoch 10, expected 9"));
        assert!(err(verify_manifest(&g, &with(&anchor_block(98), 9), &h)).contains("is #98, expected #99"));
        let mut swapped = with(&anchor_block(99), 9);
        swapped.anchor.as_mut().unwrap().finalization = finalization(&g, &seeds, &CBlock::new(Digest([1; 32]), 99, 99, 98, 0, String::new(), 0, [0; 32], vec![]), 9);
        assert!(err(verify_manifest(&g, &swapped, &h)).contains("for another block"));
        // Signed by a set that isn't epoch 9's.
        let strangers: Vec<[u8; 32]> = (0..4u8).map(|i| [200 + i; 32]).collect();
        let mut forged = m.clone();
        forged.anchor.as_mut().unwrap().finalization = finalization(&g, &strangers, &anchor_block(99), 9);
        assert!(verify_manifest(&g, &forged, &h).is_err());
        // Blocks through 109 end epoch 10: the anchor is the manifest's own last block.
        let through = manifest(&g, &seeds, 109);
        assert!(through.anchor.is_none());
        assert_eq!(verify_manifest(&g, &through, &h).unwrap().epoch_start, None);
        let mut extra = through.clone();
        extra.anchor = m.anchor.clone();
        assert!(err(verify_manifest(&g, &extra, &h)).contains("doesn't need"));
        std::fs::remove_dir_all(root).ok();
    }

    /// A joiner checks the manifest against what it trusts: block hashes where the manifest proves
    /// them, pinned sets where its history reaches, and it never ignores a checkpoint it can't
    /// check. A checkpoint past the manifest means the peer is behind.
    #[test]
    fn manifests_are_checked_against_trusted_checkpoints() {
        let (root, g, seeds) = four_validators("trusted");
        let v = verify_manifest(&g, &manifest(&g, &seeds, 104), &steady(&g, 10)).unwrap();
        let real_set = crate::checkpoints::genesis_checkpoint(&g).set_hash;
        let trust = |cps: Vec<Checkpoint>| Trust::new(&g, &Pins { checkpoints: cps, ..Default::default() }, false).unwrap();
        let cp = |height: u64, hash: &str, set_hash: Option<String>| Checkpoint { height, hash: hash.into(), set_hash };
        let (other, at) = ("ab".repeat(32), |h: u64| v.hash_at(h).unwrap());
        for h in [99, 100, 103] {
            v.check_trust(&trust(vec![cp(h, &at(h), None)])).unwrap();
            assert!(v.check_trust(&trust(vec![cp(h, &other, None)])).unwrap_err().to_string().contains("another chain"), "#{h}");
        }
        let behind = v.check_trust(&trust(vec![cp(150, &other, None)])).unwrap_err();
        assert!(behind.is::<SnapshotBehind>(), "{behind}");
        // Older than the snapshot: only a pinned set can be checked.
        assert!(v.check_trust(&trust(vec![cp(50, &other, None)])).unwrap_err().to_string().contains("pins no validator set"));
        v.check_trust(&trust(vec![cp(50, &other, real_set.clone())])).unwrap();
        assert!(v.check_trust(&trust(vec![cp(50, &other, Some("cd".repeat(32)))])).unwrap_err().to_string().contains("forged"));
        // An unpinned one before a pinned set that checked out no longer matters; after it, it does.
        v.check_trust(&trust(vec![cp(40, &other, None), cp(50, &other, real_set.clone())])).unwrap();
        assert!(v.check_trust(&trust(vec![cp(50, &other, real_set.clone()), cp(60, &other, None)])).unwrap_err().to_string().contains("#60"));
        std::fs::remove_dir_all(root).ok();
    }

    /// Weak subjectivity, concretely: the genesis keys (long unbonded, say) sign a retroactive
    /// handoff to a set they made up at epoch 6, which then certifies a whole fake snapshot. Its
    /// history checks out from the genesis, so a joiner trusting only the genesis would take it; a
    /// checkpoint that pins the real set of epoch 7 refuses it.
    #[test]
    fn a_retroactive_handoff_forgery_needs_only_old_keys_and_a_checkpoint_stops_it() {
        let (root, g, seeds) = four_validators("forgery");
        let evil: Vec<[u8; 32]> = (0..4u8).map(|i| [200 + i; 32]).collect();
        let mut reg = steady(&g, 5);
        assert!(reg.record(6, keys(&evil)));
        // The old keys sign whatever they're told to.
        let told = reg.clone();
        for s in &seeds[..3] {
            reg.sign_missing(&epochs::Instance::of(&g), *s, |e| told.set(e).cloned());
        }
        for e in 7..=10 {
            reg.record(e, keys(&evil));
        }
        let forged = manifest(&g, &evil, 104);
        let v = verify_manifest(&g, &forged, &reg).expect("the history verifies from the genesis: that's the attack");
        let pinned = |set_hash: Option<String>| Trust::new(&g, &Pins { checkpoints: vec![Checkpoint { height: 75, hash: "ab".repeat(32), set_hash }], ..Default::default() }, false).unwrap();
        let err = v.check_trust(&pinned(crate::checkpoints::genesis_checkpoint(&g).set_hash)).unwrap_err().to_string();
        assert!(err.contains("epoch 7") && err.contains("forged"), "{err}");
        std::fs::remove_dir_all(root).ok();
    }

    /// F08: a peer's history may run past the manifest's certified epoch with changes nobody
    /// signed (here, at epoch 50, to keys the peer holds). The joiner keeps only what it verified,
    /// from its root through the last block's epoch, so nothing of the planted change reaches its
    /// registry, and a validator's `sign_missing` (which also needs its own state to show both
    /// sides) has nothing of it to sign.
    #[test]
    fn only_the_verified_history_is_adopted_from_a_manifest() {
        let (root, g, seeds) = four_validators("planted");
        let evil: Vec<[u8; 32]> = (0..4u8).map(|i| [200 + i; 32]).collect();
        let mut history = steady(&g, 10);
        assert!(history.record(50, keys(&evil)));
        let v = verify_manifest(&g, &manifest(&g, &seeds, 104), &history).unwrap();
        assert_eq!((v.epochs.base, v.epochs.known_through), (0, 10));
        assert_eq!(v.epochs.sets.keys().copied().collect::<Vec<_>>(), vec![0], "only the genesis set: no change through epoch 10");
        let mut adopted = v.epochs.clone();
        let executed = steady(&g, 10);
        assert!(adopted.sign_missing(&epochs::Instance::of(&g), seeds[0], |e| executed.set(e).cloned()).is_empty());
        // Executing on, the joiner records the epochs itself; the planted set never comes back.
        for e in 11..=50 {
            adopted.record(e, keys(&seeds));
        }
        assert_eq!(adopted.set(50), Some(&keys(&seeds)));
        assert!(adopted.sign_missing(&epochs::Instance::of(&g), seeds[0], |e| executed.set(e).or(executed.set(10)).cloned()).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    /// F02 (review-integrate-compose-1): instance B relaunches A under the same chain id with the
    /// same genesis validators (only `created_at_ms` differs, so the genesis hash does). On A, a
    /// quorum of the genesis keys handed off to a set the attacker holds (it joined A's set, or
    /// A's keys leaked once A was abandoned). A forged B snapshot certified by those keys under
    /// B's own namespace, served with A's history, used to verify on B with only B's genesis
    /// trusted. Handoffs bind the genesis hash now, so A's don't verify on B.
    #[test]
    fn handoffs_do_not_replay_across_a_relaunch() {
        let (root, a, seeds) = four_validators("relaunch");
        let mut b = a.clone();
        b.created_at_ms += 1;
        assert_ne!(crate::checkpoints::genesis_hash(&a), crate::checkpoints::genesis_hash(&b));
        let evil: Vec<[u8; 32]> = (0..4u8).map(|i| [200 + i; 32]).collect();
        let history = |g: &Genesis| {
            let mut reg = steady(g, 5);
            assert!(reg.record(6, keys(&evil)));
            let told = reg.clone();
            for s in &seeds[..3] {
                reg.sign_missing(&epochs::Instance::of(g), *s, |e| told.set(e).cloned());
            }
            for e in 7..=10 {
                reg.record(e, keys(&evil));
            }
            reg
        };
        let on_a = history(&a);
        // On A itself the history verifies (that's A's own weak-subjectivity story).
        assert!(verify_manifest(&a, &manifest(&a, &evil, 104), &on_a).is_ok());
        // On B, A's handoffs are no proof of anything.
        let forged = manifest(&b, &evil, 104);
        let e = err(verify_manifest(&b, &forged, &on_a));
        assert!(e.contains("epoch 6") && e.contains("0 valid handoff signatures"), "{e}");
        let genesis_only = Trust::new(&b, &Pins::default(), false).unwrap();
        assert!(verify_manifest_trusted(&b, &forged, &on_a, &genesis_only).is_err());
        // B's own validators signing B's change is what B needs.
        assert_eq!(verify_manifest(&b, &forged, &history(&b)).unwrap().epochs.set(10), Some(&keys(&evil)));
        std::fs::remove_dir_all(root).ok();
    }

    /// review-fix-sync-2, -5: a joiner adopts only what verified. A peer's history with junk at
    /// a lower signer index than a quorum of valid signatures (epoch 1), and with a set and junk
    /// proofs past the certified epoch while its `known_through` is that epoch: the manifest
    /// verifies, but the joiner's registry holds neither, so it serves pages that verify, counts
    /// only real signatures toward a quorum, and still collects (and signs) epoch 11's handoff.
    #[test]
    fn junk_and_entries_past_the_certified_epoch_are_not_adopted() {
        let (root, g, seeds) = four_validators("junk");
        let at = epochs::Instance::of(&g);
        let newcomer = PrivateKey::from_seed_bytes([9; 32]).public_key();
        let next = {
            let mut k = keys(&seeds[1..]);
            k.push(hex::encode(newcomer.as_ref()));
            k.sort();
            k
        };
        let mut history = steady(&g, 0);
        assert!(history.record(1, next.clone()));
        for e in 2..=10 {
            history.record(e, next.clone());
        }
        let told = history.clone();
        for s in &seeds {
            history.sign_missing(&at, *s, |e| told.set(e).cloned());
        }
        assert_eq!(history.proofs[&1].len(), 4, "all four outgoing validators signed (quorum 3)");
        let honest = history.clone();
        let lowest = *history.proofs[&1].keys().next().unwrap();
        history.proofs.get_mut(&1).unwrap().insert(lowest, "00".repeat(3309));
        // Past the certified epoch: the set the joiner will execute at 11 (the attacker can know
        // it), with junk for its handoff.
        history.sets.insert(11, keys(&seeds));
        history.proofs.insert(11, (0..4).map(|i| (i, "00".repeat(3309))).collect());
        let v = verify_manifest(&g, &manifest(&g, &[seeds[1], seeds[2], seeds[3], [9; 32]], 104), &history).unwrap();
        assert_eq!(v.epochs.known_through, 10);
        assert!(v.epochs.sets.keys().chain(v.epochs.proofs.keys()).all(|e| *e <= 10), "nothing past the certified epoch");
        assert!(!v.epochs.proofs[&1].contains_key(&lowest) && v.epochs.proofs[&1].len() == 3, "the junk is left out");
        let (page, _) = v.epochs.slice(0, epochs::PAGE_BYTES);
        assert!(epochs::verify_chain(&g, &page, 10).is_ok(), "what it serves verifies");
        // Executing on, epoch 11's real handoff is still missing here, so it's collected.
        let mut adopted = v.epochs.clone();
        adopted.record(11, keys(&seeds));
        let mut real = honest.clone();
        real.record(11, keys(&seeds));
        let told = real.clone();
        for s in &seeds[1..] {
            real.sign_missing(&at, *s, |e| told.set(e).cloned());
        }
        assert_eq!(adopted.proven_through(), 10);
        assert_eq!(adopted.merge_sigs(&at, &real), 3);
        assert_eq!(adopted.proven_through(), 11);
        std::fs::remove_dir_all(root).ok();
    }

    /// F10: a joiner verifies the history from its newest trusted root, so a peer that keeps only
    /// a window of it (from epoch 5 here) serves enough when a checkpoint pins a set at or after
    /// the window's start, and is refused, pointing at `--checkpoint`, when only the genesis is.
    #[test]
    fn a_windowed_history_verifies_from_a_pinned_root() {
        let (root, g, seeds) = four_validators("window");
        let mut kept = steady(&g, 10);
        kept.rebase(5);
        let m = manifest(&g, &seeds, 104);
        let genesis_only = Trust::new(&g, &Pins::default(), false).unwrap();
        assert!(format!("{:#}", verify_manifest_trusted(&g, &m, &kept, &genesis_only).err().unwrap()).contains("--checkpoint"));
        // A checkpoint at #49 pins epoch 5's set (the set certifying #50).
        let set_hash = Some(hex::encode(epochs::set_hash(&keys(&seeds))));
        let pinned = Trust::new(&g, &Pins { checkpoints: vec![Checkpoint { height: 49, hash: "ab".repeat(32), set_hash }], ..Default::default() }, false).unwrap();
        assert_eq!(trusted_root(&g, &m, &pinned).epoch, 5);
        let v = verify_manifest_trusted(&g, &m, &kept, &pinned).unwrap();
        assert_eq!((v.epochs.base, v.epochs.known_through), (5, 10));
        // A pin the window contradicts is a forgery.
        let wrong = Trust::new(&g, &Pins { checkpoints: vec![Checkpoint { height: 49, hash: "ab".repeat(32), set_hash: Some("cd".repeat(32)) }], ..Default::default() }, false).unwrap();
        assert!(format!("{:#}", verify_manifest_trusted(&g, &m, &kept, &wrong).err().unwrap()).contains("forged"));
        std::fs::remove_dir_all(root).ok();
    }

    /// A node serves the anchor it executed (block and certificate), or, having fast-synced past
    /// it, the one its own manifest proved (`anchor.json`).
    #[test]
    fn anchors_come_from_the_store_or_the_fast_sync() {
        let home = std::env::temp_dir().join(format!("blockula-anchorfile-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let proof = Anchor { block: hex::encode(anchor_block(99).encode()), finalization: "f1".into() };
        let none = |_: u64| None;
        assert_eq!(anchor_proof(&home, 99, none, none), None);
        save_anchor(&home, Some(&proof)).unwrap();
        assert_eq!(anchor_proof(&home, 99, none, none), Some(proof));
        assert_eq!(anchor_proof(&home, 89, none, none), None, "the saved anchor is for another epoch");
        let stored = anchor_proof(&home, 89, |h| Some(vec![h as u8]), |_| Some(vec![0xce]));
        assert_eq!(stored, Some(Anchor { block: "59".into(), finalization: "ce".into() }));
        assert_eq!(anchor_proof(&home, 89, |h| Some(vec![h as u8]), none), None, "no certificate, no anchor");
        save_anchor(&home, None).unwrap();
        save_anchor(&home, None).unwrap();
        assert_eq!(anchor_proof(&home, 99, none, none), None);
        std::fs::remove_dir_all(home).ok();
    }

    /// Restore decompresses the chunks as one stream, whatever the split, and a record's length
    /// prefix can't promise more than the stream holds.
    #[test]
    fn chunked_restore_streams_and_records_are_bounded() {
        let mut st = State::default();
        for i in 0..50u64 {
            st.accounts.insert(format!("boo1account{i}"), Account { balance: i, ..Default::default() });
        }
        st.supply = (0..50).sum();
        st.commit_block().unwrap();
        let mut enc = zstd::Encoder::new(vec![], 3).unwrap();
        write_state(&st, &mut enc).unwrap();
        let packed = enc.finish().unwrap();
        let chunks: Vec<Vec<u8>> = packed.chunks(7).map(<[u8]>::to_vec).collect();
        assert!(chunks.len() > 10);
        assert_eq!(decode_state_from(Chunks { chunks: &chunks, at: 0, off: 0 }).unwrap().root(), st.root());
        assert_eq!(decode_state(&packed).unwrap().root(), st.root());
        // Raw records: cut inside the last record, then a key length over the cap.
        let mut raw = vec![];
        write_state(&st, &mut raw).unwrap();
        assert!(read_state(&mut &raw[..raw.len() - 3]).err().unwrap().to_string().contains("truncated"));
        let mut huge = raw[..20].to_vec();
        huge.push(crate::state::ns::META);
        huge.extend_from_slice(&(crate::state::MAX_KEY_BYTES as u32 + 1).to_le_bytes());
        assert!(read_state(&mut huge.as_slice()).err().unwrap().to_string().contains("too large"));
    }

    /// The writer cuts chunk files as the compressed stream fills them (several here), and this
    /// node's own snapshot reads back, checked chunk by chunk and against its root.
    #[test]
    fn snapshots_stream_into_chunk_files_and_read_back() {
        let home = std::env::temp_dir().join(format!("blockula-snapwrite-{}-{}", crate::chain::now_ms(), std::process::id()));
        let mut st = State::default();
        // Incompressible keys (hashes), enough for a few MiB of records.
        for i in 0..60_000u64 {
            st.accounts.insert(crate::crypto::address_from_bytes(&crate::crypto::hash(&i.to_le_bytes())), Account { balance: i, ..Default::default() });
        }
        st.supply = (0..60_000).sum();
        st.commit_block().unwrap();
        let meta = write(&home, 100, &st.root(), &st).unwrap();
        assert!(meta.chunks.len() > 1, "{} chunks", meta.chunks.len());
        assert!(meta.chunks[..meta.chunks.len() - 1].iter().all(|c| c.size == CHUNK_SIZE));
        assert_eq!(list(&home), vec![100]);
        assert_eq!(read_local(&home, 100).unwrap().root(), st.root());
        // A damaged chunk is caught before decoding.
        let p = dir(&home).join("100").join("chunk-1");
        let mut b = std::fs::read(&p).unwrap();
        b[0] ^= 1;
        std::fs::write(&p, b).unwrap();
        assert!(read_local(&home, 100).err().unwrap().to_string().contains("chunk 1"));
        std::fs::remove_dir_all(home).ok();
    }

    /// F63: whatever a node serves stays on disk for a download's worth of blocks after a joiner
    /// takes its manifest, however many newer snapshots replace the newest meanwhile: 100 blocks
    /// for a small state's newest snapshot, a long interval (at least `LONG_SNAPSHOT_MS`) for a
    /// big state's long-lived one, also when the writer skipped a checkpoint.
    #[test]
    fn served_snapshots_outlive_a_download() {
        let params = |block_time_ms: u64| crate::chain::Params { block_time_ms, ..Default::default() };
        let intervals: Vec<u64> = [500, 1_000, 4_500, 60_000, 3_600_000].iter().map(|t| long_interval(&params(*t))).collect();
        assert_eq!(intervals, vec![1_800, 900, 200, 100, 100]);
        for (bt, long) in [(500u64, 1_800u64), (1_000, 900), (4_500, 200), (60_000, 100)] {
            assert!(long * bt >= LONG_SNAPSHOT_MS || long == CHECKPOINT_INTERVAL, "{bt} ms blocks");
            for skipped in [None, Some(long), Some(2 * long + CHECKPOINT_INTERVAL)] {
                // Every checkpoint is written (but `skipped`, the writer being busy) and pruned.
                let mut disk: Vec<u64> = vec![];
                let mut served: Vec<(u64, u64, u64)> = vec![]; // (snapshot, newest then, how long it must stay)
                for n in (1..=12 * long / CHECKPOINT_INTERVAL).map(|i| i * CHECKPOINT_INTERVAL) {
                    if Some(n) != skipped {
                        disk.insert(0, n);
                        disk = kept(&disk, long);
                        assert!(disk.len() <= 2 * KEEP_SNAPSHOTS);
                    }
                    for (s, at, stay) in &served {
                        if n <= at + stay {
                            assert!(disk.contains(s), "{bt} ms blocks, skipping {skipped:?}: #{s}, served at #{at}, deleted by #{n}");
                        }
                    }
                    if disk.is_empty() {
                        continue;
                    }
                    let small = serving_order(disk.clone(), 1, 1, long)[0];
                    assert_eq!(small, disk[0], "a small state is served from the newest");
                    served.push((small, disk[0], CHECKPOINT_INTERVAL));
                    // The newest long-lived one: the first written in the newest snapshot's
                    // stretch, so it goes only once two later stretches have begun.
                    let big = serving_order(disk.clone(), 2, 1, long)[0];
                    assert!(big.is_multiple_of(long) || big == CHECKPOINT_INTERVAL || skipped == Some(big - CHECKPOINT_INTERVAL), "#{big}");
                    served.push((big, disk[0], long));
                }
            }
        }
        // On disk: `write` keeps the newest two and the first of each of the last two stretches.
        let home = std::env::temp_dir().join(format!("blockula-snapkeep-{}-{}", crate::chain::now_ms(), std::process::id()));
        let mut st = State::default();
        st.params.block_time_ms = 1_000;
        st.commit_block().unwrap();
        for h in (1..=20).map(|i| i * CHECKPOINT_INTERVAL) {
            write(&home, h, &st.root(), &st).unwrap();
        }
        assert_eq!(list(&home), vec![2_000, 1_900, 1_800, 900]);
        assert!(chunk(&home, 900, 0).is_some());
        assert_eq!(serving_order(list(&home), 2, 1, 900), vec![1_800, 900, 2_000, 1_900]);
        std::fs::remove_dir_all(home).ok();
    }

    /// Chunk servers for [`download_chunks`] tests: each path prefix behaves as named, and every
    /// request is counted.
    async fn chunk_servers(chunks: Vec<Vec<u8>>) -> (String, std::sync::Arc<parking_lot::Mutex<std::collections::BTreeMap<String, usize>>>) {
        use axum::extract::{Path as UrlPath, State as Shared};
        use axum::response::IntoResponse;
        type Hits = std::sync::Arc<parking_lot::Mutex<std::collections::BTreeMap<String, usize>>>;
        async fn serve(Shared((chunks, hits)): Shared<(std::sync::Arc<Vec<Vec<u8>>>, Hits)>, UrlPath((who, _h, i)): UrlPath<(String, u64, usize)>) -> axum::response::Response {
            *hits.lock().entry(who.clone()).or_default() += 1;
            let mut bytes = chunks[i].clone();
            match who.as_str() {
                "stall" => tokio::time::sleep(std::time::Duration::from_secs(3_600)).await,
                "slow" => tokio::time::sleep(std::time::Duration::from_millis(400)).await,
                "wrong" => bytes[0] ^= 1,
                "missing" => return axum::http::StatusCode::NOT_FOUND.into_response(),
                _ => {}
            }
            bytes.into_response()
        }
        let hits: Hits = Default::default();
        let app = axum::Router::new().route("/{who}/api/snapshot/{h}/chunk/{i}", axum::routing::get(serve)).with_state((std::sync::Arc::new(chunks), hits.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), hits)
    }

    /// F62: a peer that stalls, sends a wrong chunk or doesn't have the snapshot is dropped and
    /// its chunks come from the others; a slow one is asked only when nothing faster is left; and
    /// no peer, however it answers, holds the download past its deadline.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stalling_chunk_server_is_abandoned() {
        let chunks: Vec<Vec<u8>> = (0..12u8).map(|i| vec![i; 1_000 + i as usize]).collect();
        let v = Verified {
            height: 100,
            state_root: String::new(),
            chunks: chunks.iter().map(|c| ChunkInfo { hash: hex::encode(crate::crypto::hash(c)), size: c.len() }).collect(),
            blocks: vec![],
            finalization: vec![],
            epochs: EpochRegistry::default(),
            epoch_start: None,
            anchor: None,
        };
        let (base, hits) = chunk_servers(chunks.clone()).await;
        let client = reqwest::Client::new();
        let limits = DownloadLimits { chunk_timeout: std::time::Duration::from_millis(500), slow: std::time::Duration::from_millis(250), per_chunk: std::time::Duration::from_millis(100), slack: std::time::Duration::from_secs(5), ..DownloadLimits::default() };
        let get = |who: &[&str], limits: DownloadLimits| {
            let (client, v, hits) = (client.clone(), &v, &hits);
            let sources: Vec<String> = who.iter().map(|w| format!("{base}/{w}")).collect();
            async move {
                hits.lock().clear();
                let mut out = vec![];
                let started = std::time::Instant::now();
                let r = download_chunks(&client, &sources, v, &mut out, &limits).await;
                (r.map(|()| out), started.elapsed(), hits.lock().clone())
            }
        };
        let whole: Vec<u8> = chunks.concat();
        // The stalling peer is listed first: it gets the first batch's share, times out once per
        // chunk it was asked for, and is never asked again.
        let (out, took, n) = get(&["stall", "honest"], limits.clone()).await;
        assert_eq!(out.unwrap(), whole);
        assert!(n["stall"] <= 2 && n["honest"] == 12, "{n:?}");
        assert!(took < std::time::Duration::from_secs(3), "{took:?}");
        // Wrong bytes and a 404 cost a request or a few each, only in the first batch: its four
        // chunks ask at once, so a source can be asked by every chunk that reaches it before its
        // failure is recorded. "wrong" is first for chunks 0 and 3; "missing" is first for chunk 1
        // and second for chunks 0 and 3, so it gets a third request whenever "wrong" fails before
        // "missing" answers its 404 (a race, not a retry).
        let (out, _, n) = get(&["wrong", "missing", "honest"], limits.clone()).await;
        assert_eq!(out.unwrap(), whole);
        assert!(n["wrong"] <= 2 && n["missing"] <= 3 && n["honest"] == 12, "{n:?}");
        // A slow peer (under the timeout) gets its first batch's share, then only what nobody
        // faster can serve; alone, it's still used.
        let (out, _, n) = get(&["slow", "honest"], limits.clone()).await;
        assert_eq!(out.unwrap(), whole);
        assert!(n["slow"] <= 2 && n["honest"] >= 10, "{n:?}");
        let (out, _, n) = get(&["slow"], limits.clone()).await;
        assert_eq!((out.unwrap(), n["slow"]), (whole.clone(), 12));
        // A stalling peer alone fails at the chunk timeout, not at its leisure.
        let (out, took, _) = get(&["stall"], limits.clone()).await;
        assert!(format!("{:#}", out.unwrap_err()).contains("no chunk within"));
        assert!(took < std::time::Duration::from_secs(2), "{took:?}");
        // A peer that answers every chunk just in time still can't outlast the deadline (here
        // 12 chunks in 3 rounds of 400 ms against 300 ms).
        let tight = DownloadLimits { per_chunk: std::time::Duration::ZERO, slack: std::time::Duration::from_millis(300), ..limits.clone() };
        let (out, took, _) = get(&["slow"], tight).await;
        assert!(format!("{:#}", out.unwrap_err()).contains("didn't arrive within"));
        assert!(took < std::time::Duration::from_secs(1), "{took:?}");
        // Nobody to ask.
        assert!(download_chunks(&client, &[], &v, &mut vec![], &limits).await.is_err());
    }

    /// Fast-sync peers for [`fast_sync_with`] tests: `/{who}/api/snapshot/latest` serves `manifests[who]`
    /// (503 when there's none) and `/{who}/api/epochs?from=k` answers as `who` says. "trickle":
    /// a page per epoch, each with a set change, 300 ms apiece (a peer that can make up as many
    /// epochs as the manifest spans). "flat": the 96-byte page that changes nothing and says
    /// there's more. Every request is counted, by `who` and path.
    async fn fast_sync_peers(g: &Genesis, manifests: std::collections::BTreeMap<String, Manifest>) -> (String, std::sync::Arc<parking_lot::Mutex<std::collections::BTreeMap<String, usize>>>) {
        use axum::extract::{Path as UrlPath, Query, State as Shared};
        use axum::response::IntoResponse;
        type Hits = std::sync::Arc<parking_lot::Mutex<std::collections::BTreeMap<String, usize>>>;
        type Ctx = (std::sync::Arc<std::collections::BTreeMap<String, Manifest>>, [Vec<String>; 2], Hits);
        async fn latest(Shared((manifests, _, hits)): Shared<Ctx>, UrlPath(who): UrlPath<String>) -> axum::response::Response {
            *hits.lock().entry(format!("{who}/latest")).or_default() += 1;
            match manifests.get(&who) {
                Some(m) => axum::Json(m.clone()).into_response(),
                None => (axum::http::StatusCode::SERVICE_UNAVAILABLE, "no certified snapshot yet").into_response(),
            }
        }
        async fn epochs(Shared((_, sets, hits)): Shared<Ctx>, UrlPath(who): UrlPath<String>, Query(q): Query<std::collections::HashMap<String, u64>>) -> axum::response::Response {
            *hits.lock().entry(format!("{who}/epochs")).or_default() += 1;
            let k = q.get("from").copied().unwrap_or(0);
            let page = match who.as_str() {
                "trickle" => {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let set = |e: u64| sets[(e % 2) as usize].clone();
                    serde_json::json!({ "sets": { k.to_string(): set(k), (k + 1).to_string(): set(k + 1) }, "base": k, "known_through": k + 1, "proofs": {}, "format": epochs::HANDOFF_FORMAT, "next": k + 1 })
                }
                _ => serde_json::json!({ "sets": { k.to_string(): [] }, "base": k, "known_through": k + 1, "proofs": {}, "format": epochs::HANDOFF_FORMAT, "next": k + 1 }),
            };
            axum::Json(page).into_response()
        }
        let hits: Hits = Default::default();
        let genesis_set = EpochRegistry::genesis(g).sets[&0].clone();
        let other = keys(&[[200; 32]]);
        let app = axum::Router::new()
            .route("/{who}/api/snapshot/latest", axum::routing::get(latest))
            .route("/{who}/api/epochs", axum::routing::get(epochs))
            .with_state((std::sync::Arc::new(manifests), [genesis_set, other], hits.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), hits)
    }

    /// An empty joiner's chain for `g` (what `blockula join` leaves).
    fn joiner(g: &Genesis, tag: &str) -> (PathBuf, crate::consensus::Shared) {
        let home = std::env::temp_dir().join(format!("blockula-joiner-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("genesis.json"), serde_json::to_string(g).unwrap()).unwrap();
        let chain = crate::chain::Chain::open(&home).unwrap();
        (home, std::sync::Arc::new(parking_lot::Mutex::new(chain)))
    }

    /// F62, review-fix-sync-1: one deadline covers a fast-sync attempt's history and chunks, so
    /// a peer that answers history pages slowly (each a real-looking set change, one epoch at a
    /// time) loses the attempt and the next peer is asked; a page that changes nothing yet says
    /// there's more is refused at once; and a manifest of blocks the chain can't have reached
    /// yet (its height is the peer's word until verified) is refused before any history is
    /// asked for. review-fix-sync-3: a peer whose manifest ends before the joiner's checkpoint
    /// says so (`SnapshotBehind`, so the joiner waits for a newer snapshot), also without asking
    /// for history.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_paging_history_forever_loses_the_attempt() {
        let (root, g, seeds) = four_validators("endless");
        let m = manifest(&g, &seeds, 104);
        let mut future = m.clone();
        future.height = 1_000_000_000_000;
        let manifests = std::collections::BTreeMap::from([("trickle".to_string(), m.clone()), ("flat".to_string(), m.clone()), ("future".to_string(), future), ("behind".to_string(), m.clone())]);
        let (base, hits) = fast_sync_peers(&g, manifests).await;
        let url = |who: &str| format!("{base}/{who}");
        let limits = DownloadLimits {
            history_per_epoch: std::time::Duration::from_millis(10),
            history_slack: std::time::Duration::from_millis(200),
            history_max: std::time::Duration::from_secs(1),
            per_chunk: std::time::Duration::from_millis(10),
            slack: std::time::Duration::from_millis(100),
            ..DownloadLimits::default()
        };
        assert_eq!(limits.history(0, 10), std::time::Duration::from_millis(310));
        assert_eq!(limits.history(0, u64::MAX), std::time::Duration::from_secs(1), "capped");
        let client = reqwest::Client::new();
        // The page that changes nothing: refused on the first one (it used to page on until the
        // manifest's epoch, as far off as the peer liked).
        let e = format!("{:#}", fetch_history(&client, &url("flat"), 0, u64::MAX / 4, &limits).await.unwrap_err());
        assert!(e.contains("must hold a set change"), "{e}");
        let n = hits.lock().clone();
        assert_eq!(n["flat/epochs"], 1, "{n:?}");
        // The trickler alone: out of time on the history, long before its pages run out.
        let genesis_only = Trust::new(&g, &Pins::default(), false).unwrap();
        let (home, chain) = joiner(&g, "endless");
        let started = std::time::Instant::now();
        let e = format!("{:#}", fast_sync_with(&chain, &[url("trickle")], &genesis_only, &limits).await.unwrap_err());
        assert!(e.contains("within"), "{e}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
        let n = hits.lock().clone();
        assert!(n["trickle/epochs"] <= 3, "{n:?}");
        // With another peer listed, that one is asked too, whichever comes first.
        hits.lock().clear();
        let started = std::time::Instant::now();
        assert!(fast_sync_with(&chain, &[url("trickle"), url("none")], &genesis_only, &limits).await.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
        let n = hits.lock().clone();
        assert_eq!((n["trickle/latest"], n["none/latest"]), (1, 1), "{n:?}");
        // A manifest past the chain's expected height: nothing more is fetched for it.
        hits.lock().clear();
        let e = format!("{:#}", fast_sync_with(&chain, &[url("future")], &genesis_only, &limits).await.unwrap_err());
        assert!(e.contains("past any height the chain can have reached"), "{e}");
        assert!(!hits.lock().clone().contains_key("future/epochs"));
        // Behind the joiner's checkpoint (#200 > #104): wait for a later snapshot.
        let ahead = Trust::new(&g, &Pins { checkpoints: vec![Checkpoint { height: 200, hash: "ab".repeat(32), set_hash: None }], ..Default::default() }, false).unwrap();
        let e = fast_sync_with(&chain, &[url("behind")], &ahead, &limits).await.unwrap_err();
        assert!(e.chain().any(|c| c.is::<SnapshotBehind>()), "{e:#}");
        assert!(!hits.lock().clone().contains_key("behind/epochs"));
        assert_eq!(chain.lock().height(), 0);
        std::fs::remove_dir_all(home).ok();
        std::fs::remove_dir_all(root).ok();
    }

    /// F62: fast sync asks the peers in a new order each run, so no listed endpoint is always first.
    #[test]
    fn peers_are_asked_in_a_new_order_each_run() {
        let peers: Vec<String> = (0..4).map(|i| format!("http://peer{i}")).collect();
        let mut firsts = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let mut s = shuffled(&peers);
            firsts.insert(s[0].clone());
            s.sort();
            assert_eq!(s, peers);
        }
        assert!(firsts.len() > 1, "{firsts:?}");
        assert!(shuffled(&[]).is_empty());
    }
}
