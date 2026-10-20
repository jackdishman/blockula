//! `blockula replay`: runs recorded history through this binary again and checks that it
//! reproduces it exactly, block by block. It's the v1 replay gate (docs/PROGRAMS-PLAN.md P2,
//! docs/REPLAY.md): protocol v2's rules land on main behind version gates, and below the height
//! that activates them a v2 binary must be a v1 binary.
//!
//! An archive is what `scripts/archive-chain.mjs` writes: `meta.json`, `genesis.json`, and the
//! chain's finalized consensus blocks as `/api/sync` served them, batch by batch, each with the
//! certificate of its last block. A recorded archive (`node/testdata/replay/`) also holds the
//! block log of the node that recorded it (`meta.json` `blocks`): what each block did there.
//! The replay runs the paths a node runs, each in a fresh home:
//! 1. **Sync**, as a joining node, on `node::apply_batch_trusted`'s own steps: each batch checked
//!    (`node::verify_batch_trusted`: certificate, hash links, and the checkpoints and validator-set
//!    pins this binary embeds for the chain), then executed block by block (`node::execute_batch`,
//!    `Chain::execute_shared`). Before it executes a block the replay runs `consensus::verify`'s
//!    checks that need neither a consensus round nor the clock, with `verify`'s own functions
//!    (`consensus::check_header` and the rest): the protocol version in force, the parent link and
//!    spacing, the proposer being its view's leader, the cited height and state root, and the txs
//!    (canonical, distinct, passing the stateless checks, within the fee slot's size limits and
//!    paying its base fee).
//! 2. **The recording, block by block**, where the archive has one: each executed block's txs
//!    (which applied, which failed, which were skipped), its state root and its base fee must be
//!    the recorded ones, and then so must its whole record. Failure reasons aren't compared:
//!    releases reword them, and they aren't state (audit F31, as `Chain::replay`).
//! 3. **Startup**, as a node rebuilding its state database: the recorded block log, opened by
//!    `Chain::open`, which replays it from genesis (`Chain::replay`: every tx's outcome and every
//!    block's state root).
//!
//! The first difference stops it, naming the height and what differs.

use crate::chain::{Chain, Genesis};
use crate::checkpoints::{Pins, Trust};
use crate::consensus::{self, CBlock, Shared};
use crate::node::SyncBatch;
use crate::types::{Block, Tx};
use anyhow::{anyhow, bail, ensure, Context, Result};
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often a long replay says where it is.
const PROGRESS_EVERY: Duration = Duration::from_secs(30);

/// `meta.json`. Fields it doesn't know are ignored: the archive script may record more.
#[derive(Deserialize)]
struct Meta {
    chain_id: String,
    genesis_hash: String,
    #[serde(default)]
    segments: Vec<Segment>,
    open: Option<Open>,
    /// The recording node's block log (a recorded archive only), in files like `segments`.
    #[serde(default)]
    blocks: Vec<Segment>,
}

/// A closed file: the heights it holds and the SHA-256 of its bytes (gzipped when its name ends
/// in `.gz`).
#[derive(Deserialize)]
struct Segment {
    file: String,
    from: u64,
    to: u64,
    sha256: String,
}

/// The file the archive still appends to.
#[derive(Deserialize)]
struct Open {
    file: String,
    from: u64,
}

/// An archive directory, its files checked against `meta.json` as they're read.
pub struct Archive {
    dir: PathBuf,
    meta: Meta,
}

impl Archive {
    pub fn open(dir: &Path) -> Result<Archive> {
        let path = dir.join("meta.json");
        let meta: Meta = serde_json::from_slice(&std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?).with_context(|| path.display().to_string())?;
        let genesis = Genesis::load(&dir.join("genesis.json"))?;
        let hash = crate::checkpoints::genesis_hash(&genesis);
        ensure!(genesis.chain_id == meta.chain_id && hash == meta.genesis_hash, "{}: genesis.json is {} (genesis {hash}), but meta.json says {} (genesis {})", dir.display(), genesis.chain_id, meta.chain_id, meta.genesis_hash);
        let mut sync_next = 1;
        for (what, files) in [("segments", &meta.segments), ("blocks", &meta.blocks)] {
            let mut next = 1u64;
            for s in files {
                ensure!(s.from == next && s.to >= s.from, "meta.json: {what} skip or repeat heights at {} (expected {next}..)", s.file);
                next = s.to.checked_add(1).ok_or_else(|| anyhow!("meta.json: {what}: {} ends at an impossible height", s.file))?;
            }
            if what == "segments" {
                sync_next = next;
            }
        }
        if let Some(o) = &meta.open {
            ensure!(o.from == sync_next, "meta.json: the open file {} starts at {}, after closed files ending at {}", o.file, o.from, sync_next - 1);
        }
        Ok(Archive { dir: dir.to_path_buf(), meta })
    }

    pub fn chain_id(&self) -> &str {
        &self.meta.chain_id
    }

    /// The batches in height order: the closed files (each checked against its hash), then the
    /// open one.
    fn batches(&self) -> impl Iterator<Item = Result<SyncBatch>> + '_ {
        let closed = self.meta.segments.iter().map(|s| (s.file.as_str(), Some(s.sha256.as_str())));
        let files = closed.chain(self.meta.open.iter().map(|o| (o.file.as_str(), None))).collect();
        self.file_lines(files).map(|l| l.and_then(|(file, l)| serde_json::from_slice(&l).with_context(|| format!("{file}: a line isn't a batch"))))
    }

    /// The recorded block log's lines in height order (none without a recording).
    fn recorded(&self) -> impl Iterator<Item = Result<Vec<u8>>> + '_ {
        self.file_lines(self.meta.blocks.iter().map(|s| (s.file.as_str(), Some(s.sha256.as_str()))).collect()).map(|l| l.map(|(_, l)| l))
    }

    pub fn has_recording(&self) -> bool {
        !self.meta.blocks.is_empty()
    }

    /// Every line of `files` (each a name and, closed, its SHA-256) with its file's name, one
    /// file read at a time.
    fn file_lines<'a>(&'a self, files: Vec<(&'a str, Option<&'a str>)>) -> impl Iterator<Item = Result<(&'a str, Vec<u8>)>> + 'a {
        files.into_iter().flat_map(move |(file, sha)| -> Box<dyn Iterator<Item = Result<(&'a str, Vec<u8>)>> + 'a> {
            match self.lines(file, sha) {
                Ok(lines) => Box::new(lines.into_iter().map(move |l| Ok((file, l)))),
                Err(e) => Box::new(std::iter::once(Err(e))),
            }
        })
    }

    /// A file's lines. A closed file must hash to `sha256` first. The open one is read up to its
    /// last complete line: the archive script may be appending to it.
    fn lines(&self, file: &str, sha256: Option<&str>) -> Result<Vec<Vec<u8>>> {
        ensure!(Path::new(file).file_name().and_then(|n| n.to_str()) == Some(file), "meta.json names {file:?}, not a file in the archive");
        let path = self.dir.join(file);
        let open = if sha256.is_none() { " (the open file: if the archive script closed it meanwhile, replay again)" } else { "" };
        let raw = std::fs::read(&path).with_context(|| format!("reading {}{open}", path.display()))?;
        if let Some(want) = sha256 {
            let got = crate::release_cli::sha256_hex(&raw);
            ensure!(got == want, "{} hashes to {got}, but meta.json says {want}", path.display());
        }
        let mut text = vec![];
        if file.ends_with(".gz") {
            flate2::read::MultiGzDecoder::new(&raw[..]).read_to_end(&mut text).with_context(|| format!("decompressing {}", path.display()))?;
        } else {
            text = raw;
        }
        let complete = text.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        if sha256.is_some() {
            ensure!(complete == text.len(), "{} ends in a partial line", path.display());
        }
        Ok(text[..complete].split(|&b| b == b'\n').filter(|l| !l.is_empty()).map(<[u8]>::to_vec).collect())
    }
}

pub struct Opts {
    pub archive: PathBuf,
    /// Where the replay's homes go (`sync/`, `startup/`); a temporary directory by default.
    pub home: Option<PathBuf>,
    /// The last height to replay (default: the archive's last).
    pub to: Option<u64>,
    /// Keep only this many blocks on disk while syncing (long archives); skips the startup replay.
    pub prune_blocks: Option<u64>,
    /// Checkpoints to trust besides those this binary embeds for the chain (`--checkpoint`).
    pub checkpoints: Vec<crate::checkpoints::Checkpoint>,
}

/// What a replay that reproduced everything went through.
#[derive(Debug, Default)]
pub struct Report {
    pub height: u64,
    pub state_root: String,
    /// Blocks compared with the recording, and failure reasons worded otherwise than recorded.
    pub recorded: u64,
    pub reworded: u64,
    /// Blocks `Chain::open` replayed from the recorded block log.
    pub startup: Option<u64>,
    /// The consensus blocks' txs by tag: applied, failed, skipped.
    pub tags: BTreeMap<u8, [u64; 3]>,
    pub secs: f64,
}

impl Report {
    /// All the txs: applied, failed, skipped.
    pub fn txs(&self) -> [u64; 3] {
        self.tags.values().fold([0; 3], |[a, f, s], [a2, f2, s2]| [a + a2, f + f2, s + s2])
    }
}

/// Replays `o.archive` (see the module docs). Its homes are removed on success, unless they were
/// asked for (`o.home`).
pub fn run(o: &Opts) -> Result<Report> {
    ensure!(o.to != Some(0), "--to must be a height (1 or more)");
    let archive = Archive::open(&o.archive)?;
    let (root, temporary) = match &o.home {
        Some(h) => {
            ensure!(!h.exists() || std::fs::read_dir(h)?.next().is_none(), "{} isn't empty", h.display());
            (h.clone(), false)
        }
        None => {
            // Named for the chain, as far as its id (the archive's word) is safe in a path.
            let id: String = archive.chain_id().chars().take(64).map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
            (std::env::temp_dir().join(format!("blockula-replay-{id}-{}-{}", std::process::id(), crate::chain::now_ms())), true)
        }
    };
    let report = replay(&archive, o, &root);
    match &report {
        Ok(_) if temporary => {
            if let Err(e) = std::fs::remove_dir_all(&root) {
                eprintln!("couldn't remove the replay's homes in {}: {e}", root.display());
            }
        }
        Ok(_) => {}
        Err(_) => eprintln!("the replay's homes are kept in {}", root.display()),
    }
    report
}

fn replay(archive: &Archive, o: &Opts, root: &Path) -> Result<Report> {
    let started = Instant::now();
    let mut r = sync(archive, o, &root.join("sync"), started)?;
    if archive.has_recording() && o.prune_blocks.is_none() {
        r.startup = Some(startup(archive, &root.join("startup"), r.height, &r.state_root)?);
    }
    r.secs = started.elapsed().as_secs_f64();
    Ok(r)
}

/// Steps 1 and 2 (the module docs), in `home`. The sync chain is gone when it returns.
fn sync(archive: &Archive, o: &Opts, home: &Path, started: Instant) -> Result<Report> {
    std::fs::create_dir_all(home)?;
    std::fs::copy(archive.dir.join("genesis.json"), home.join("genesis.json"))?;
    let mut chain = Chain::open(home)?;
    chain.set_scratch();
    if o.prune_blocks.is_some() {
        chain.set_pruning(o.prune_blocks);
    }
    // What a node joining the chain trusts: its genesis (here the archive's), the checkpoints this
    // binary embeds for it, and any pinned.
    let pins = Pins { genesis_hash: Some(archive.meta.genesis_hash.clone()), checkpoints: o.checkpoints.clone(), ..Pins::default() };
    let trust = Trust::new(&chain.genesis, &pins, false)?;
    let parent = consensus::genesis_block(&chain.genesis);
    let shared: Shared = Arc::new(Mutex::new(chain));
    let provider = consensus::EpochProvider::new(shared.clone(), None);
    let mut w = Watch { shared: shared.clone(), archive, to: o.to, recorded: archive.recorded(), parent, leaders: None, txs: vec![], r: Report::default(), started, progress: Instant::now() };
    for batch in archive.batches() {
        let batch = batch?;
        let (from, epoch) = {
            let c = shared.lock();
            (c.height() + 1, c.state.params.epoch_of(c.height() + 1))
        };
        let verifier = provider.epoch_scheme(epoch).ok_or_else(|| anyhow!("no validator set for epoch {epoch} (height {from})"))?;
        let (blocks, cert) = crate::node::verify_batch_trusted(&shared, &verifier, epoch, &batch, Some(&trust)).with_context(|| format!("batch {from}..={}", batch.to.unwrap_or_default()))?;
        crate::node::execute_batch(&shared, &blocks, &cert, Some(&mut w))?;
        // Blocks past `--to` aren't executed, and no batch after the one holding it is read (that
        // one was checked whole): what follows may be missing or damaged.
        if o.to.is_some_and(|to| w.r.height >= to) {
            break;
        }
    }
    let Watch { mut recorded, r, .. } = w;
    ensure!(r.height > 0, "{}: the archive holds no blocks", archive.dir.display());
    if let Some(to) = o.to {
        ensure!(r.height == to, "the archive ends at block {}, before {to}", r.height);
    } else if let Some(line) = recorded.next() {
        let extra: Block = serde_json::from_slice(&line?)?;
        bail!("the recorded block log goes on (block {}) past the archive's last block, {}", extra.height, r.height);
    }
    Ok(r)
}

/// What the replay checks of each block its sync executes (`node::execute_batch`): before, on its
/// parent's state, [`check_block`]; after, what it did against the recording, if there's one.
struct Watch<'a, I> {
    shared: Shared,
    archive: &'a Archive,
    to: Option<u64>,
    /// The recorded block log's lines not compared yet.
    recorded: I,
    /// The last block executed (first, the genesis block) and its epoch's leader election.
    parent: CBlock,
    leaders: Option<consensus::EpochLeaders>,
    /// The txs of the block executing, as [`check_block`] decoded them.
    txs: Vec<Tx>,
    r: Report,
    started: Instant,
    progress: Instant,
}

impl<I: Iterator<Item = Result<Vec<u8>>>> crate::node::BatchWatch for Watch<'_, I> {
    fn before(&mut self, b: &CBlock) -> Result<bool> {
        if self.to.is_some_and(|to| b.height > to) {
            return Ok(false);
        }
        self.txs = check_block(&self.shared, &self.parent, b, &mut self.leaders)?;
        Ok(true)
    }

    fn after(&mut self, b: &CBlock, executed: Result<()>) -> Result<()> {
        executed.with_context(|| format!("executing block {}", b.height))?;
        let c = self.shared.lock();
        let got = c.last_block().filter(|x| x.height == b.height).ok_or_else(|| anyhow!("block {} executed, but the chain's head isn't it", b.height))?;
        let now = fates(got, &self.txs)?;
        for (tx, fate) in self.txs.iter().zip(&now) {
            self.r.tags.entry(tx.action.to_bytes()[0]).or_default()[fate.slot()] += 1;
        }
        if let Some(line) = self.recorded.next() {
            self.r.reworded += compare(got, &line?, &self.txs, &now)?;
            self.r.recorded += 1;
        } else if self.archive.has_recording() {
            bail!("the recorded block log ends before block {}", b.height);
        }
        (self.r.height, self.r.state_root) = (b.height, got.state_root.clone());
        drop(c);
        self.parent = b.clone();
        if self.progress.elapsed() >= PROGRESS_EVERY {
            eprintln!("… block {} ({:.0} blocks/s)", self.r.height, self.r.height as f64 / self.started.elapsed().as_secs_f64());
            self.progress = Instant::now();
        }
        Ok(())
    }
}

/// `consensus::verify`'s checks of block `b` (on `parent`) that need neither its consensus round
/// nor the clock, made with `verify`'s own functions on the state `b` runs on (the cited root,
/// which `verify` waits for, compared here). Its certificate, parent link and the checkpoints are
/// `verify_batch_trusted`'s. What `verify` checks that this doesn't: the round (the view and
/// parent view against it, the leader's signature), the clock, how far behind the cited height may
/// be (it depends on how far each verifier had executed), and CV-5, every tx admittable on the
/// parent's state as `verify` rebuilds it (execution admits each tx again where it runs, and skips
/// one that isn't: the recording shows which). `leaders` keeps the epoch's election (set and
/// beacon order: fixed before the epoch starts). Returns the decoded txs.
fn check_block(chain: &Shared, parent: &CBlock, b: &CBlock, leaders: &mut Option<consensus::EpochLeaders>) -> Result<Vec<Tx>> {
    let h = b.height;
    let (txs, ids) = consensus::decode_txs(b).map_err(|why| anyhow!("block {h}: {why}"))?;
    let c = chain.lock();
    let version = crate::upgrade::version_at(&c.state, h);
    let epoch = c.state.params.epoch_of(h);
    if leaders.as_ref().is_none_or(|l| l.epoch != epoch) {
        *leaders = consensus::EpochLeaders::new(&c.state, epoch);
    }
    let leader = leaders.as_ref().and_then(|l| l.leader(b.view)).ok_or_else(|| anyhow!("block {h}: no validator set for its epoch {epoch}"))?;
    consensus::check_header(b, parent, version, c.genesis.block_time_ms, &leader).map_err(|why| {
        anyhow!(
            "block {h}: {why} (it says v{}, time {}, proposer {}, view {}, citing height {}; this binary has v{version} in force, the parent's time {} and the view's leader {leader})",
            b.version,
            b.timestamp_ms,
            b.proposer,
            b.view,
            b.exec_height,
            parent.timestamp_ms
        )
    })?;
    let cited = c.root_at(b.exec_height).ok_or_else(|| anyhow!("block {h} cites height {}, whose state root this replay no longer holds", b.exec_height))?;
    ensure!(cited == hex::encode(b.exec_root), "block {h} cites state root {} at height {}, where this binary has {cited}", hex::encode(b.exec_root), b.exec_height);
    // The fee slot of `h`: fixed once `h − FEE_LAG` executed, so `verify` knows it too.
    if let Some(slot) = crate::econ::slot(&c.state, h) {
        consensus::check_ghost_budget(&slot, b, &txs).with_context(|| format!("block {h}"))?;
        consensus::check_slot(&slot, b, &txs).map_err(|why| anyhow!("block {h}: {why}"))?;
    }
    ensure!(!consensus::replays_finalized(&c, &ids), "block {h} replays an already-finalized tx");
    consensus::check_txs(&txs, &c.genesis.chain_id, &c.genesis_hash, c.state.params.prefix(), h).with_context(|| format!("block {h} fails the stateless checks"))?;
    Ok(txs)
}

/// What became of one of a block's txs.
#[derive(Debug, PartialEq)]
enum Fate<'a> {
    Applied,
    Failed(&'a str),
    Skipped,
}

impl Fate<'_> {
    /// Its column in [`Report::tags`].
    fn slot(&self) -> usize {
        match self {
            Fate::Applied => 0,
            Fate::Failed(_) => 1,
            Fate::Skipped => 2,
        }
    }
}

/// The fate of each of `all` (the consensus block's txs, in order) in `b`, the executed block.
fn fates<'a>(b: &'a Block, all: &[Tx]) -> Result<Vec<Fate<'a>>> {
    let mut included = b.txs.iter().map(Tx::id).enumerate().peekable();
    let fates = all
        .iter()
        .map(|tx| {
            let id = tx.id();
            match included.next_if(|(_, t)| *t == id) {
                Some((i, _)) => b.failed.get(&i).map_or(Fate::Applied, |why| Fate::Failed(why)),
                None => Fate::Skipped,
            }
        })
        .collect();
    ensure!(included.next().is_none(), "block {} includes txs its consensus block doesn't carry", b.height);
    Ok(fates)
}

/// Checks the block this binary executed (`got`, its txs' fates `now`) against the recorded one
/// (`line` of the recorded block log): each tx's fate (failure reasons aside), the sizes of those
/// skipped, the state root and the base fee, then the whole record. Returns how many failure
/// reasons are worded otherwise.
fn compare(got: &Block, line: &[u8], all: &[Tx], now: &[Fate]) -> Result<u64> {
    let want: Block = serde_json::from_slice(line).context("the recorded block log has a line that isn't a block")?;
    let h = want.height;
    ensure!(got.height == h && got.hash == want.hash, "the recorded block log has block {h} ({}) where the archive has block {} ({})", want.hash, got.height, got.hash);
    let then = fates(&want, all).with_context(|| format!("the recorded block {h}"))?;
    let mut reworded = 0;
    for (i, ((now, then), tx)) in now.iter().zip(&then).zip(all).enumerate() {
        match (now, then) {
            (Fate::Failed(a), Fate::Failed(b)) => reworded += u64::from(a != b),
            (a, b) if a == b => {}
            (now, then) => bail!("block {h}: tx {i} ({}, {}) {}, but the recording says it {}", tx.action.kind(), tx.id(), told(now, "is"), told(then, "was")),
        }
    }
    ensure!(got.skipped == want.skipped, "block {h}: skipped txs of {:?} bytes, but the recording skipped {:?}", got.skipped, want.skipped);
    ensure!(got.state_root == want.state_root, "block {h}: state root {}, but the recording has {}", got.state_root, want.state_root);
    ensure!(got.base_fee == want.base_fee, "block {h}: base fee {:?}, but the recording has {:?}", got.base_fee, want.base_fee);
    // Then the whole record, failure reasons aside: below v2 a block's record is v1's
    // (PROGRAMS-PLAN.md), so a field added, dropped or derived otherwise fails here too.
    let record = |mut v: serde_json::Value| {
        if let Some(reasons) = v.get_mut("failed").and_then(serde_json::Value::as_object_mut) {
            reasons.values_mut().for_each(|why| *why = serde_json::Value::Null);
        }
        v
    };
    let (ours, theirs) = (record(serde_json::to_value(got)?), record(serde_json::from_slice(line)?));
    if ours != theirs {
        let fields = |v: &serde_json::Value| v.as_object().map(|o| o.keys().cloned().collect::<BTreeSet<_>>()).unwrap_or_default();
        let differ: Vec<String> = fields(&ours).union(&fields(&theirs)).filter(|k| ours.get(k.as_str()) != theirs.get(k.as_str())).cloned().collect();
        bail!("block {h}: its record differs from the recording's in {}", differ.join(", "));
    }
    Ok(reworded)
}

fn told(f: &Fate, verb: &str) -> String {
    match f {
        Fate::Applied => "applied".into(),
        Fate::Failed(why) => format!("failed ({why})"),
        Fate::Skipped => format!("{verb} skipped (not admitted)"),
    }
}

/// The startup replay: the recorded block log through block `to` as a node's own (`blocks/`) in
/// a fresh home, which `Chain::open` rebuilds a state database from, to state root `root`.
/// Returns how many blocks it replayed.
fn startup(archive: &Archive, home: &Path, to: u64, root: &str) -> Result<u64> {
    std::fs::create_dir_all(home)?;
    std::fs::copy(archive.dir.join("genesis.json"), home.join("genesis.json"))?;
    let (mut blocks, _) = crate::store::open_logs(home, None)?;
    // The recorded files hold heights 1.. in order (`Archive::open`), a line each.
    for (height, line) in (1..=to).zip(archive.recorded()) {
        blocks.append(height, &line?)?;
    }
    blocks.sync()?;
    drop(blocks);
    let c = Chain::open(home).context("Chain::open, replaying the recorded block log")?;
    ensure!(c.height() == to && c.state.root() == root, "Chain::open replayed the recorded block log to block {} (state root {}), not {to} ({root})", c.height(), c.state.root());
    Ok(c.replayed)
}

/// Ways a test binary can diverge from v1, each a mistake the gate exists to catch: the negative
/// tests switch one on (on their own thread) and check the replay fails where it starts.
#[cfg(test)]
pub mod diverge {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Divergence {
        /// Admission refuses txs that carry a memo: a v2 admission rule that missed its version
        /// gate (`upgrade::test_gate` without the version check). Included txs become skipped.
        Admission,
        /// A transfer that opens an account fails: an action handler changed without a gate.
        Handler,
        /// Each epoch's end moves a drop from the community account to the treasury: a change no
        /// tx's outcome shows, only the state root.
        EndOfEpoch,
    }

    thread_local! {
        static ON: Cell<Option<Divergence>> = const { Cell::new(None) };
    }

    pub fn set(d: Option<Divergence>) {
        ON.with(|c| c.set(d));
    }

    pub fn on(d: Divergence) -> bool {
        ON.with(|c| c.get() == Some(d))
    }
}

#[cfg(test)]
mod corpus;
#[cfg(test)]
mod tests;
