//! The read path (SEC-4): what the HTTP API serves from, never under the consensus lock.
//!
//! After every executed block the chain publishes a [`ReadView`]: an O(1) clone of the state, the
//! store snapshot committed with it (tree nodes, tx index, history, derived indexes, block
//! positions) and the block's header. Mempool changes publish a [`MempoolView`] (persistent maps,
//! so publishing is O(1) too). Handlers load the current `Arc`s from [`Live`] and never touch the
//! `Mutex<Chain>` that propose, verify and execute use; only writes (submitting a tx, the faucet)
//! take it.
//!
//! Listing endpoints that would scan state (Hunts by bounty, the Soul leaderboard, claims by
//! status) are computed at most once per view, on first use ([`ReadView::hunts`] etc.), so the
//! work per block is bounded however many requests arrive.

use crate::chain::{Base, Genesis, HuntStatus, Role, State};
use crate::epochs::EpochRegistry;
use crate::store::{hash32, ix, BlockMeta, SegReader, Snap};
use crate::types::{Block, Tx, NAME_SUFFIX};
use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Parsed blocks kept in memory for reads (by JSON size).
pub const BLOCK_CACHE_BYTES: usize = 64 << 20;
/// Certificates kept in memory for this many recent heights (older ones only at checkpoints and
/// epoch ends, in the store).
pub const RECENT_CERTS: usize = 1_024;

/// The on-disk side of reads: the store and the block logs, plus bounded caches.
pub struct Disk {
    pub home: PathBuf,
    pub blocks: Arc<SegReader>,
    pub cblocks: Arc<SegReader>,
    certs: RwLock<BTreeMap<u64, Arc<Vec<u8>>>>,
    cache: Mutex<BlockCache>,
}

#[derive(Default)]
struct BlockCache {
    map: HashMap<u64, Arc<Block>>,
    order: VecDeque<(u64, usize)>,
    bytes: usize,
}

impl Disk {
    pub fn new(home: PathBuf, blocks: Arc<SegReader>, cblocks: Arc<SegReader>) -> Self {
        Self { home, blocks, cblocks, certs: Default::default(), cache: Default::default() }
    }

    pub fn cached_block(&self, height: u64) -> Option<Arc<Block>> {
        self.cache.lock().map.get(&height).cloned()
    }

    /// Remembers a block (`bytes`: its JSON size), evicting the oldest past the budget.
    pub fn cache_block(&self, block: Arc<Block>, bytes: usize) {
        let mut c = self.cache.lock();
        if c.map.insert(block.height, block.clone()).is_none() {
            c.order.push_back((block.height, bytes));
            c.bytes += bytes;
        }
        while c.bytes > BLOCK_CACHE_BYTES && c.order.len() > 1 {
            let (h, b) = c.order.pop_front().unwrap();
            c.map.remove(&h);
            c.bytes -= b;
        }
    }

    /// The executed block at `meta`'s position, from the cache or the log.
    pub fn load_block(&self, height: u64, meta: &BlockMeta) -> Option<Arc<Block>> {
        if let Some(b) = self.cached_block(height) {
            return Some(b);
        }
        let line = self.blocks.read(meta.block).ok()?;
        let b: Arc<Block> = Arc::new(serde_json::from_slice(&line).ok()?);
        (b.height == height).then(|| {
            self.cache_block(b.clone(), line.len());
            b
        })
    }

    pub fn recent_cert(&self, height: u64) -> Option<Arc<Vec<u8>>> {
        self.certs.read().get(&height).cloned()
    }

    /// The lowest height in `range` whose certificate is in memory.
    pub fn next_recent_cert(&self, range: std::ops::RangeInclusive<u64>) -> Option<u64> {
        self.certs.read().range(range).next().map(|(h, _)| *h)
    }

    pub fn remember_cert(&self, height: u64, cert: Vec<u8>) {
        let mut certs = self.certs.write();
        certs.insert(height, Arc::new(cert));
        while certs.len() > RECENT_CERTS {
            certs.pop_first();
        }
    }
}

/// A pending tx, its id and the height it arrived for.
#[derive(Clone)]
pub struct Pending {
    pub id: String,
    pub tx: Arc<Tx>,
    pub added_at: u64,
    /// The name it asks to register, directly or through a multisig action, as admission found
    /// it (`econ::name_asked_for`).
    pub name: Option<String>,
}

/// Pending txs and recent failures, as of the last mempool change. Persistent maps: cloning is
/// O(1), so every change publishes a new one (and the chain checks the mempool against a block
/// from a copy, without its lock).
#[derive(Clone, Default)]
pub struct MempoolView {
    txs: imbl::OrdMap<u64, Pending>,
    ids: imbl::HashMap<String, u64>,
    senders: imbl::HashMap<String, usize>,
    /// Pending name registrations, direct or wrapped in a multisig action: (normalized name,
    /// arrival seq), so a name's waiting registrations are a range read, not a mempool scan
    /// ([`MempoolView::registrations`]).
    names: imbl::OrdSet<(String, u64)>,
    failed: imbl::HashMap<String, (String, u64)>,
    /// The remembered failures that ran in a block (their fee was charged); the rest were dropped
    /// from the mempool unrun and may still execute elsewhere until their `valid_until`.
    ran: imbl::HashSet<String>,
    next: u64,
}

impl MempoolView {
    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&Arc<Tx>> {
        self.txs.get(self.ids.get(id)?).map(|p| &p.tx)
    }

    /// Pending txs, newest first.
    pub fn newest(&self) -> impl Iterator<Item = &Arc<Tx>> {
        self.txs.values().rev().map(|p| &p.tx)
    }

    /// Pending txs in arrival order.
    pub fn in_order(&self) -> impl Iterator<Item = &Pending> {
        self.txs.values()
    }

    pub fn pending_count(&self, sender: &str) -> usize {
        self.senders.get(sender).copied().unwrap_or(0)
    }

    /// The pending txs asking to register `name` (normalized), newest first.
    pub fn registrations<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a Pending> + 'a {
        let range = (name.to_string(), 0)..=(name.to_string(), u64::MAX);
        self.names.range(range).rev().filter_map(|(_, seq)| self.txs.get(seq))
    }

    /// Why a tx failed or was dropped, if it did recently.
    pub fn failure(&self, id: &str) -> Option<&(String, u64)> {
        self.failed.get(id)
    }

    /// Whether a remembered failure ran in a block (and was charged), not just dropped unrun.
    pub fn failure_ran(&self, id: &str) -> bool {
        self.ran.contains(id)
    }

    /// Files a pending tx; `name`: the name it asks to register (normalized), if any.
    pub(crate) fn add(&mut self, id: String, tx: &Tx, added_at: u64, name: Option<String>) {
        if self.ids.contains_key(&id) {
            return;
        }
        let seq = self.next;
        self.next += 1;
        *self.senders.entry(tx.from.clone()).or_default() += 1;
        if let Some(name) = &name {
            self.names.insert((name.clone(), seq));
        }
        self.txs.insert(seq, Pending { id: id.clone(), tx: Arc::new(tx.clone()), added_at, name });
        self.ids.insert(id, seq);
    }

    pub(crate) fn remove(&mut self, id: &str) {
        let Some(seq) = self.ids.remove(id) else { return };
        let Some(Pending { tx, name, .. }) = self.txs.remove(&seq) else { return };
        if let Some(name) = name {
            self.names.remove(&(name, seq));
        }
        if let Some(n) = self.senders.get_mut(&tx.from) {
            *n -= 1;
            if *n == 0 {
                self.senders.remove(&tx.from);
            }
        }
    }

    pub(crate) fn fail(&mut self, id: String, reason: String, height: u64, ran: bool) {
        if ran {
            self.ran.insert(id.clone());
        } else {
            self.ran.remove(&id);
        }
        self.failed.insert(id, (reason, height));
    }

    pub(crate) fn unfail(&mut self, id: &str) {
        self.failed.remove(id);
        self.ran.remove(id);
    }

    pub(crate) fn has_failure(&self, id: &str) -> bool {
        self.failed.contains_key(id)
    }
}

/// The state and history as of one executed block.
pub struct ReadView {
    pub state: State,
    pub height: u64,
    pub last_hash: String,
    pub genesis: Arc<Genesis>,
    pub genesis_hash: String,
    pub genesis_root: String,
    pub role: Role,
    pub base: Option<Base>,
    /// Heights below this were pruned from this node.
    pub pruned_below: u64,
    /// Blocks this node keeps (`None`: an archive node).
    pub keep_blocks: Option<u64>,
    pub head: Option<Arc<Block>>,
    pub snap: Arc<Snap>,
    pub disk: Arc<Disk>,
    memo: Memo,
}

#[derive(Default)]
struct Memo {
    hunts: OnceLock<HuntsMemo>,
    souls: OnceLock<Vec<(u64, String)>>,
    claims: OnceLock<ClaimsMemo>,
}

/// Hunts by bounty (biggest first, then by id), overall and per displayed status.
pub struct HuntsMemo {
    pub all: Vec<u64>,
    pub by_status: HashMap<&'static str, Vec<u64>>,
    pub open_total: u64,
}

/// Claim ids newest first per status, the counts, and the jury pool.
pub struct ClaimsMemo {
    pub by_status: HashMap<String, Vec<u64>>,
    pub counts: BTreeMap<String, usize>,
    pub jurors: usize,
    pub active_jurors: usize,
}

/// A Hunt's status as the API shows it (`expired`: open past its deadline).
pub fn hunt_status(h: &crate::chain::Hunt, height: u64) -> &'static str {
    match h.status {
        HuntStatus::Open if height > h.expires_at => "expired",
        HuntStatus::Open => "open",
        HuntStatus::Awarded => "awarded",
        HuntStatus::Cancelled => "cancelled",
    }
}

fn status_name(s: crate::poc::ClaimStatus) -> String {
    serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

impl ReadView {
    #[allow(clippy::too_many_arguments)]
    pub fn new(state: State, height: u64, last_hash: String, genesis: Arc<Genesis>, genesis_hash: String, genesis_root: String, role: Role, base: Option<Base>, pruned_below: u64, keep_blocks: Option<u64>, head: Option<Arc<Block>>, snap: Arc<Snap>, disk: Arc<Disk>) -> Self {
        Self { state, height, last_hash, genesis, genesis_hash, genesis_root, role, base, pruned_below, keep_blocks, head, snap, disk, memo: Memo::default() }
    }

    pub fn base_height(&self) -> u64 {
        self.base.as_ref().map(|b| b.height).unwrap_or(0)
    }

    /// First height whose block this node holds.
    pub fn history_from(&self) -> u64 {
        (self.base_height() + 1).max(self.pruned_below)
    }

    /// Whether `height` was executed here but is no longer stored (pruned or before fast sync).
    pub fn is_pruned(&self, height: u64) -> bool {
        height >= 1 && height <= self.height && height < self.history_from()
    }

    pub fn block_meta(&self, height: u64) -> Option<BlockMeta> {
        if height < self.history_from() || height > self.height {
            return None;
        }
        self.snap.block_meta(height).ok()?
    }

    /// Headers of the newest blocks below `before`, newest first (from the index: no block is
    /// read from the log).
    pub fn headers(&self, before: u64, limit: usize) -> Vec<(u64, BlockMeta)> {
        let top = before.saturating_sub(1).min(self.height);
        (self.history_from()..=top).rev().take(limit).filter_map(|h| Some((h, self.block_meta(h)?))).collect()
    }

    /// The base fee block `height` charged: from memory if the block is there, else from its
    /// index row (never by parsing the block, F66).
    pub fn base_fee(&self, height: u64) -> Option<crate::econ::FeeSlot> {
        if let Some(b) = self.head.as_ref().filter(|b| b.height == height).cloned().or_else(|| self.disk.cached_block(height)) {
            return b.base_fee;
        }
        self.block_meta(height)?.base_fee
    }

    /// Executed block at `height`.
    pub fn block(&self, height: u64) -> Option<Arc<Block>> {
        if height == 0 || height > self.height || height < self.history_from() {
            return None;
        }
        if let Some(h) = self.head.as_ref().filter(|b| b.height == height) {
            return Some(h.clone());
        }
        if let Some(b) = self.disk.cached_block(height) {
            return Some(b);
        }
        self.disk.load_block(height, &self.block_meta(height)?)
    }

    pub fn block_by_hash(&self, hash: &str) -> Option<Arc<Block>> {
        let h = self.snap.height_of(&hash32(hash).ok()?).ok()??;
        self.block(h).filter(|b| b.hash == hash)
    }

    /// State root after executing `height` (0 = genesis).
    pub fn root_at(&self, height: u64) -> Option<String> {
        if height == 0 {
            return Some(self.genesis_root.clone());
        }
        if height > self.height {
            return None;
        }
        if height == self.height {
            return Some(hex::encode(self.state.root_hash()));
        }
        if let Some(b) = self.base.as_ref().filter(|b| b.height == height) {
            return Some(b.state_root.clone());
        }
        if let Some(m) = self.block_meta(height) {
            return Some(hex::encode(m.state_root));
        }
        self.snap.epoch_head(height).ok()?.map(|(_, r)| hex::encode(r))
    }

    /// Hash of the executed block at `height`, including the fast-sync base and the anchor of
    /// the epoch it synced into, and epochs' last blocks (kept through pruning).
    pub fn hash_at(&self, height: u64) -> Option<String> {
        if height == 0 {
            return Some(self.genesis_hash.clone());
        }
        if height > self.height {
            return None;
        }
        if height == self.height {
            return Some(self.last_hash.clone());
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
        if let Some(m) = self.block_meta(height) {
            return Some(hex::encode(m.hash));
        }
        self.snap.epoch_head(height).ok()?.map(|(h, _)| hex::encode(h))
    }

    /// The latest executed block's timestamp.
    pub fn head_time_ms(&self) -> Option<u64> {
        self.head.as_ref().map(|b| b.timestamp_ms)
    }

    /// Exact consensus block bytes at `height`, if held.
    pub fn cblock(&self, height: u64) -> Option<Vec<u8>> {
        let pos = self.block_meta(height)?.cblock?;
        self.disk.cblocks.read(pos).ok()
    }

    /// Finalization certificate for `height`, if held (recent ones in memory, checkpoints and
    /// epoch ends on disk).
    pub fn cert(&self, height: u64) -> Option<Vec<u8>> {
        if let Some(c) = self.disk.recent_cert(height) {
            return Some((*c).clone());
        }
        self.snap.cert(height).ok()?
    }

    /// The lowest height in `range` with a certificate this node holds (memory or disk).
    pub fn next_cert(&self, range: std::ops::RangeInclusive<u64>) -> Option<u64> {
        let disk = self.snap.next_cert(range.clone()).ok().flatten();
        match (self.disk.next_recent_cert(range), disk) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub fn cert_len(&self, height: u64) -> usize {
        if let Some(c) = self.disk.recent_cert(height) {
            return c.len();
        }
        self.snap.cert(height).ok().flatten().map(|c| c.len()).unwrap_or(0)
    }

    /// Where a finalized tx is: (height, index).
    pub fn tx_location(&self, id: &str) -> Option<(u64, usize)> {
        self.snap.tx(&hash32(id).ok()?).ok()?
    }

    /// A finalized tx and its failure reason, if any.
    /// Reads just that tx from the log (its span is indexed) unless the block is in memory.
    pub fn finalized_tx(&self, height: u64, idx: usize) -> Option<(Tx, Option<String>)> {
        let in_memory = self.head.as_ref().filter(|b| b.height == height).cloned().or_else(|| self.disk.cached_block(height));
        if let Some(b) = in_memory {
            return Some((b.txs.get(idx)?.clone(), b.failed.get(&idx).cloned()));
        }
        let m = self.block_meta(height)?;
        if let Some(&(off, len)) = m.tx_spans.get(idx) {
            let bytes = self.disk.blocks.read_within(m.block, off, len).ok()?;
            let tx: Tx = serde_json::from_slice(&bytes).ok()?;
            return Some((tx, m.failed.iter().find(|(i, _)| *i as usize == idx).map(|(_, r)| r.clone())));
        }
        let b = self.disk.load_block(height, &m)?;
        Some((b.txs.get(idx)?.clone(), b.failed.get(&idx).cloned()))
    }

    /// An account's latest finalized txs (height, index), newest first.
    pub fn history(&self, account: &str, limit: usize) -> Vec<(u64, usize)> {
        self.snap.history(account, limit).unwrap_or_default().into_iter().filter(|(h, _)| *h >= self.history_from()).collect()
    }

    /// Names an account owns, oldest registration first.
    pub fn names_of(&self, owner: &str, limit: usize) -> Vec<String> {
        let rows = self.snap.index(ix::NAME, owner.as_bytes(), false, limit).unwrap_or_default();
        rows.into_iter().filter_map(|r| String::from_utf8(r.get(8..)?.to_vec()).ok()).collect()
    }

    /// The first name an account registered that still resolves (shown instead of its address;
    /// expired names, DEC-11, are skipped).
    pub fn primary_name(&self, owner: &str) -> Option<String> {
        let active = |n: &String| self.state.names.get(n).is_some_and(|r| crate::econ::name_active(&self.state, r));
        self.names_of(owner, 8).into_iter().find(active).map(|n| format!("{n}{NAME_SUFFIX}"))
    }

    /// Accounts that hold the key `key_id` (hex), from the key index.
    /// The accounts after `after` (a page's last) in address order, or from the first.
    pub fn accounts_with_key_after(&self, key_id: &str, after: Option<&str>, limit: usize) -> Vec<String> {
        let Ok(id) = hex::decode(key_id) else { return vec![] };
        let rows = match after {
            Some(a) => self.snap.index_after(ix::KEY, &id, a.as_bytes(), limit),
            None => self.snap.index(ix::KEY, &id, false, limit),
        };
        rows.unwrap_or_default().into_iter().filter_map(|r| String::from_utf8(r).ok()).collect()
    }

    /// Ids (u64 keys) under an index, e.g. the Hunts an account created.
    pub fn ids(&self, tag: u8, account: &str, newest_first: bool, limit: usize) -> Vec<u64> {
        self.snap.index(tag, account.as_bytes(), newest_first, limit).unwrap_or_default().into_iter().filter_map(|r| Some(u64::from_be_bytes(r.try_into().ok()?))).collect()
    }

    /// Accounts `voucher` vouches for.
    pub fn vouching_for(&self, voucher: &str, limit: usize) -> Vec<String> {
        self.snap.index(ix::VOUCHER, voucher.as_bytes(), false, limit).unwrap_or_default().into_iter().filter_map(|r| String::from_utf8(r).ok()).collect()
    }

    /// The tx's plain-English summary (names from the index, not a scan).
    pub fn describe(&self, tx: &Tx, height: Option<u64>) -> String {
        self.state.describe_with(tx, height, &|a| self.primary_name(a))
    }

    pub fn hunts(&self) -> &HuntsMemo {
        self.memo.hunts.get_or_init(|| {
            let mut all: Vec<(u64, u64)> = self.state.hunts.values().map(|h| (h.amount, h.id)).collect();
            all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            let mut by_status: HashMap<&'static str, Vec<u64>> = HashMap::new();
            let mut open_total = 0u64;
            for (_, id) in &all {
                let h = &self.state.hunts[id];
                let s = hunt_status(h, self.height);
                if s == "open" {
                    open_total = open_total.saturating_add(h.amount);
                }
                by_status.entry(s).or_default().push(*id);
            }
            HuntsMemo { all: all.into_iter().map(|(_, id)| id).collect(), by_status, open_total }
        })
    }

    /// Accounts with Soul, highest effective Soul first (ties by address).
    pub fn souls(&self) -> &[(u64, String)] {
        self.memo.souls.get_or_init(|| {
            let mut list: Vec<(u64, String)> = self.state.poc.souls.keys().map(|a| (self.state.soul_of(a, self.height), a.clone())).filter(|(s, _)| *s > 0).collect();
            list.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            list
        })
    }

    pub fn claims(&self) -> &ClaimsMemo {
        self.memo.claims.get_or_init(|| {
            let mut by_status: HashMap<String, Vec<u64>> = HashMap::new();
            let mut counts = BTreeMap::new();
            for c in self.state.poc.claims.values().rev() {
                let s = status_name(c.status);
                *counts.entry(s.clone()).or_default() += 1;
                by_status.entry(s).or_default().push(c.id);
            }
            let jurors = &self.state.poc.jurors;
            ClaimsMemo { by_status, counts, jurors: jurors.len(), active_jurors: jurors.values().filter(|r| r.active).count() }
        })
    }
}

/// What the chain publishes for readers. Shared by the chain (writer) and the API.
pub struct Live {
    view: ArcSwap<ReadView>,
    mempool: ArcSwap<MempoolView>,
    epochs: ArcSwap<EpochRegistry>,
    /// `/api/epochs` pages as served (JSON), by `from`, and the registry they were cut from.
    epoch_pages: Mutex<(Arc<EpochRegistry>, HashMap<u64, Arc<Vec<u8>>>)>,
    /// Highest finalized height proven by a certificate (consensus updates it between blocks).
    pub finalized_tip: AtomicU64,
    /// Median of the tips HTTP peers claim (display only).
    pub claimed: AtomicU64,
    pub disk: Arc<Disk>,
}

impl Live {
    pub fn new(view: ReadView, epochs: EpochRegistry, disk: Arc<Disk>) -> Self {
        let epochs = Arc::new(epochs);
        Self { view: ArcSwap::from_pointee(view), mempool: ArcSwap::from_pointee(MempoolView::default()), epochs: ArcSwap::new(epochs.clone()), epoch_pages: Mutex::new((epochs, HashMap::new())), finalized_tip: AtomicU64::new(0), claimed: AtomicU64::new(0), disk }
    }

    pub fn view(&self) -> Arc<ReadView> {
        self.view.load_full()
    }

    pub fn mempool(&self) -> Arc<MempoolView> {
        self.mempool.load_full()
    }

    pub fn epochs(&self) -> Arc<EpochRegistry> {
        self.epochs.load_full()
    }

    pub fn publish(&self, view: ReadView) {
        self.view.store(Arc::new(view));
    }

    pub fn publish_mempool(&self, m: MempoolView) {
        self.mempool.store(Arc::new(m));
    }

    pub fn publish_epochs(&self, e: Arc<EpochRegistry>) {
        self.epochs.store(e);
    }

    /// The `/api/epochs` page from `from` (JSON: the registry's `slice` plus `proven_through` and
    /// `next`), serialized once per published registry (a few pages are cached), without holding
    /// anything the chain waits on.
    pub fn epoch_page(&self, from: u64) -> Arc<Vec<u8>> {
        let e = self.epochs();
        {
            let pages = self.epoch_pages.lock();
            if let Some(p) = pages.1.get(&from).filter(|_| Arc::ptr_eq(&pages.0, &e)) {
                return p.clone();
            }
        }
        let (page, next) = e.slice(from, crate::epochs::PAGE_BYTES);
        let mut v = serde_json::to_value(&page).unwrap_or_default();
        v["proven_through"] = serde_json::json!(page.proven_through());
        v["next"] = serde_json::json!(next);
        let bytes = Arc::new(serde_json::to_vec(&v).unwrap_or_default());
        let mut pages = self.epoch_pages.lock();
        if !Arc::ptr_eq(&pages.0, &e) {
            *pages = (e, HashMap::new());
        }
        if pages.1.len() >= 16 {
            pages.1.clear();
        }
        pages.1.insert(from, bytes.clone());
        bytes
    }

    /// Highest height known final: certified, or executed here.
    pub fn finalized(&self, height: u64) -> u64 {
        self.finalized_tip.load(Ordering::Relaxed).max(height)
    }

    /// The network height peers claim (median), never below what we know is final.
    pub fn network_height_claimed(&self, height: u64) -> u64 {
        self.claimed.load(Ordering::Relaxed).max(self.finalized(height))
    }

    /// Behind what peers claim (or what we verified) by more than a few blocks: for display
    /// (`/api/status`, `/api/health`). Validator duties use `Chain::syncing`, verified heights only.
    pub fn syncing(&self, height: u64) -> bool {
        height + crate::chain::SYNC_SLACK < self.network_height_claimed(height)
    }
}
