//! Persistence: the state database and the block logs.
//!
//! # A node's home (on-disk layout 4, `consensus::STORAGE_VERSION`)
//!
//! ```text
//! genesis.json, validator.key, node.json, join.json   configuration and keys
//! layout.json        { "segment_heights": N }, fixed when the home is created
//! blocks/{first}.jsonl   executed blocks (one JSON line each, with the state root after it)
//! cblocks/{first}.bin    the exact consensus block bytes per height (served by /api/sync)
//! state.redb         records, tree nodes, the commit point, indexes and stored certificates
//! epochs.json        validator-set history with handoff proofs
//! base.json, base.state.zst   the certified snapshot a fast-synced home started from
//! snapshots/{height}/         this node's certified snapshots (chunks + meta.json)
//! consensus/, consensus.version   simplex and marshal journals
//! ```
//!
//! The logs are split into segment files of `segment_heights` heights each, named by the first
//! height a segment can hold (zero-padded, so names sort by height). Append-only files can't be
//! cut at the front; whole segments can be deleted, which is how pruning works.
//!
//! # Commit protocol (per executed block)
//!
//! 1. The consensus block and the executed block are appended to their segments and fsynced.
//! 2. One redb write transaction ([`Store::commit`], durable) writes the records the block
//!    changed, the tree nodes it changed ([`crate::state::tree::Smt::pending_changes`]), the
//!    block's index rows (tx ids, per-account history, hash, log positions, a certificate if it's
//!    kept on disk) and the commit point ([`CommitPoint`]: height, root, block hash, schema).
//! 3. Only then does execution return, and consensus get its acknowledgement.
//!
//! A crash between 1 and 2 leaves blocks in the log past the commit point: `Chain::open` executes
//! just those again (they were never acknowledged, but they're finalized, so the result is the
//! same). A crash after 2 leaves nothing to do. Restart time depends on the size of the state
//! (records are loaded into memory), never on the length of the history.
//!
//! # Tables
//!
//! | table | key | value |
//! |---|---|---|
//! | `records` | namespace ‖ key | canonical record bytes |
//! | `nodes` | `NodeKey::to_bytes` | `Node::encode` |
//! | `meta` | `"commit"` | [`CommitPoint`] |
//! | `blocks` | height | [`BlockMeta`] (header, log positions, tx ids, history rows) |
//! | `block_hashes` | block hash | height |
//! | `txs` | tx id | height ‖ index in the block (big-endian u64, u32) |
//! | `history` | account ‖ 0 ‖ height ‖ index | () |
//! | `index` | index tag ‖ … (see [`derived_keys`]) | () |
//! | `certs` | height | finalization certificate (checkpoints and epoch ends) |
//! | `epoch_heads` | height of an epoch's last block | hash ‖ state root (never pruned) |
//!
//! `index` holds lookups the API would otherwise answer by scanning state: accounts by key,
//! names by owner, Hunts by creator and winner, claims by claimant, vouches by voucher. They are
//! derived from the records a block changed (old value from the table, new from the block), in
//! the same transaction, so they always agree with the committed state.

use crate::state::codec::decode;
use crate::state::tree::{Child, Hash, Node, NodeKey, NodeStore};
use crate::state::{ns, RecordChange};
use anyhow::{anyhow, bail, Context, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use parking_lot::Mutex;
use redb::{Database, Durability, ReadOnlyTable, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Version of the tables and their encodings. A store of another format (or state schema) is
/// rebuilt from the logs on open.
pub const STORE_FORMAT: u32 = 1;
pub const DB_FILE: &str = "state.redb";
/// redb's page cache.
pub const CACHE_BYTES: usize = 256 << 20;

const RECORDS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("records");
const NODES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("nodes");
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const BLOCKS: TableDefinition<u64, &[u8]> = TableDefinition::new("blocks");
const HASHES: TableDefinition<&[u8], u64> = TableDefinition::new("block_hashes");
const TXS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("txs");
const HISTORY: TableDefinition<&[u8], ()> = TableDefinition::new("history");
const INDEX: TableDefinition<&[u8], ()> = TableDefinition::new("index");
const CERTS: TableDefinition<u64, &[u8]> = TableDefinition::new("certs");
const EPOCH_HEADS: TableDefinition<u64, &[u8]> = TableDefinition::new("epoch_heads");

/// Tags of the derived indexes (first byte of an `index` key).
pub mod ix {
    /// key id (32 bytes) ‖ account
    pub const KEY: u8 = 1;
    /// owner ‖ 0 ‖ registered_at (u64 BE) ‖ name
    pub const NAME: u8 = 2;
    /// creator ‖ 0 ‖ hunt id
    pub const HUNT_CREATOR: u8 = 3;
    /// winner ‖ 0 ‖ hunt id
    pub const HUNT_WINNER: u8 = 4;
    /// claimant ‖ 0 ‖ claim id
    pub const CLAIMANT: u8 = 5;
    /// voucher ‖ 0 ‖ vouchee
    pub const VOUCHER: u8 = 6;
}

/// What the store holds: the state after `height`, and how the home got there.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CommitPoint {
    pub format: u32,
    pub schema: u32,
    pub genesis_hash: String,
    pub height: u64,
    pub block_hash: String,
    pub root: Hash,
    /// The tree's root slot (`Child::encode`), to reopen the tree without reading anything else.
    pub root_child: Vec<u8>,
    /// Fast-sync base height (0: the history starts at genesis).
    pub base: u64,
    /// Heights below this one have been pruned: no block, tx or history rows, no log segments.
    pub pruned_below: u64,
    /// Number of records (a cheap integrity check on open).
    pub records: u64,
}

/// Where an entry sits in a segmented log: the segment (its first height), the byte offset of
/// the entry and its length (framing included).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Pos {
    pub seg: u64,
    pub off: u64,
    pub len: u32,
}

impl Pos {
    pub fn end(&self) -> u64 {
        self.off + self.len as u64
    }
}

/// Per-height index row: the header the API lists, where the block is, and what to delete when
/// the height is pruned.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BlockMeta {
    pub hash: Hash,
    pub prev_hash: Hash,
    pub state_root: Hash,
    pub timestamp_ms: u64,
    pub proposer: String,
    pub tx_count: u32,
    pub failed_count: u32,
    /// The executed block (JSON line) and the consensus block bytes.
    pub block: Pos,
    pub cblock: Option<Pos>,
    /// Tx ids, in block order.
    pub txs: Vec<Hash>,
    /// The history rows written for this block: (tx index, account).
    pub history: Vec<(u32, String)>,
}

/// Everything one block (or a full-state load) writes, in one transaction.
pub struct Commit<'a> {
    pub point: &'a mut CommitPoint,
    pub records: &'a [RecordChange],
    pub nodes: &'a [(NodeKey, Option<Node>)],
    /// The executed block's index row (none for a genesis or snapshot load).
    pub block: Option<&'a BlockMeta>,
    /// Keep this height's hash and root forever (an epoch's last block).
    pub epoch_head: bool,
    /// A certificate to keep on disk: (height, bytes).
    pub cert: Option<(u64, &'a [u8])>,
    /// Prune index rows below this height (at most `MAX_PRUNE_PER_COMMIT` heights at a time).
    pub prune_below: u64,
    /// Fsync before returning. Rebuilds commit most blocks without it and sync the last.
    pub durable: bool,
}

/// Heights whose rows one commit deletes at most, so pruning never makes a block's commit slow.
pub const MAX_PRUNE_PER_COMMIT: u64 = 8;

pub fn hash32(hex_str: &str) -> Result<Hash> {
    hex::decode(hex_str).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| anyhow!("expected 32 bytes of hex, got {hex_str:?}"))
}

fn record_key(namespace: u8, key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + key.len());
    k.push(namespace);
    k.extend_from_slice(key);
    k
}

pub fn history_key(account: &str, height: u64, idx: u32) -> Vec<u8> {
    let mut k = Vec::with_capacity(account.len() + 13);
    k.extend_from_slice(account.as_bytes());
    k.push(0);
    k.extend_from_slice(&height.to_be_bytes());
    k.extend_from_slice(&idx.to_be_bytes());
    k
}

/// `tag ‖ a ‖ 0 ‖ b` (accounts never contain a zero byte, so prefixes can't collide).
fn ix_key(tag: u8, a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(2 + a.len() + b.len());
    k.push(tag);
    k.extend_from_slice(a);
    k.push(0);
    k.extend_from_slice(b);
    k
}

/// Prefix of every index key under `(tag, a)`.
pub fn ix_prefix(tag: u8, a: &[u8]) -> Vec<u8> {
    ix_key(tag, a, &[])
}

/// The derived index keys a record contributes (see the module docs). Values that don't decode
/// contribute nothing: indexes are for display, never for validity.
pub fn derived_keys(namespace: u8, key: &[u8], value: &[u8], out: &mut Vec<Vec<u8>>) {
    match namespace {
        ns::ACCOUNTS => {
            if let Ok(a) = decode::<crate::chain::Account>(value) {
                for k in a.keys.keys() {
                    let id = hex::decode(k).unwrap_or_else(|_| k.as_bytes().to_vec());
                    out.push(ix_key(ix::KEY, &id, key));
                }
            }
        }
        ns::NAMES => {
            if let Ok(n) = decode::<crate::chain::NameRecord>(value) {
                out.push(ix_key(ix::NAME, n.owner.as_bytes(), &[&n.registered_at.to_be_bytes()[..], key].concat()));
            }
        }
        ns::HUNTS => {
            if let Ok(h) = decode::<crate::chain::Hunt>(value) {
                out.push(ix_key(ix::HUNT_CREATOR, h.creator.as_bytes(), key));
                if let Some(w) = &h.awarded_to {
                    out.push(ix_key(ix::HUNT_WINNER, w.as_bytes(), key));
                }
            }
        }
        ns::CLAIMS => {
            if let Ok(c) = decode::<crate::poc::Claim>(value) {
                out.push(ix_key(ix::CLAIMANT, c.claimant.as_bytes(), key));
            }
        }
        ns::VOUCHES => {
            if let Ok(v) = decode::<crate::poc::Vouch>(value) {
                out.push(ix_key(ix::VOUCHER, v.voucher.as_bytes(), key));
            }
        }
        _ => {}
    }
}

fn storage(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("state database: {e}")
}

/// The redb database of a home.
pub struct Store {
    db: Database,
    pub path: PathBuf,
}

impl Store {
    /// Opens (or creates) the database at `path`, with every table present.
    pub fn open(path: &Path) -> Result<Store> {
        let db = Database::builder().set_cache_size(CACHE_BYTES).create(path).with_context(|| format!("opening {}", path.display()))?;
        let mut tx = db.begin_write().map_err(storage)?;
        tx.set_quick_repair(true);
        {
            tx.open_table(RECORDS).map_err(storage)?;
            tx.open_table(NODES).map_err(storage)?;
            tx.open_table(META).map_err(storage)?;
            tx.open_table(BLOCKS).map_err(storage)?;
            tx.open_table(HASHES).map_err(storage)?;
            tx.open_table(TXS).map_err(storage)?;
            tx.open_table(HISTORY).map_err(storage)?;
            tx.open_table(INDEX).map_err(storage)?;
            tx.open_table(CERTS).map_err(storage)?;
            tx.open_table(EPOCH_HEADS).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(Store { db, path: path.to_path_buf() })
    }

    /// Deletes the database file (the caller rebuilds it).
    pub fn remove(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// The commit point, if anything was ever committed.
    pub fn commit_point(&self) -> Result<Option<CommitPoint>> {
        let tx = self.db.begin_read().map_err(storage)?;
        let t = tx.open_table(META).map_err(storage)?;
        let Some(v) = t.get("commit").map_err(storage)? else { return Ok(None) };
        Ok(Some(decode(v.value())?))
    }

    /// A consistent read-only view as of the last commit.
    pub fn snapshot(&self) -> Result<Snap> {
        let txn = self.db.begin_read().map_err(storage)?;
        let nodes = txn.open_table(NODES).map_err(storage)?;
        Ok(Snap { nodes, txn })
    }

    /// Calls `f(namespace, key, value)` for every record, in canonical order.
    pub fn for_each_record(&self, mut f: impl FnMut(u8, &[u8], &[u8]) -> Result<()>) -> Result<u64> {
        let tx = self.db.begin_read().map_err(storage)?;
        let t = tx.open_table(RECORDS).map_err(storage)?;
        let mut n = 0;
        for r in t.iter().map_err(storage)? {
            let (k, v) = r.map_err(storage)?;
            let k = k.value();
            let (&namespace, key) = k.split_first().ok_or_else(|| anyhow!("empty record key in the state database"))?;
            f(namespace, key, v.value())?;
            n += 1;
        }
        Ok(n)
    }

    /// Writes one commit in one transaction (see the module docs).
    pub fn commit(&self, c: Commit) -> Result<()> {
        let mut tx = self.db.begin_write().map_err(storage)?;
        tx.set_quick_repair(true);
        tx.set_durability(if c.durable { Durability::Immediate } else { Durability::None }).map_err(storage)?;
        {
            let mut records = tx.open_table(RECORDS).map_err(storage)?;
            let mut index = tx.open_table(INDEX).map_err(storage)?;
            let (mut old_ix, mut new_ix) = (vec![], vec![]);
            for r in c.records {
                let k = record_key(r.namespace, &r.key);
                old_ix.clear();
                new_ix.clear();
                let old = match &r.value {
                    Some(v) => {
                        derived_keys(r.namespace, &r.key, v, &mut new_ix);
                        records.insert(k.as_slice(), v.as_slice()).map_err(storage)?.map(|g| g.value().to_vec())
                    }
                    None => records.remove(k.as_slice()).map_err(storage)?.map(|g| g.value().to_vec()),
                };
                match (&old, &r.value) {
                    (None, Some(_)) => c.point.records += 1,
                    (Some(_), None) => c.point.records = c.point.records.saturating_sub(1),
                    _ => {}
                }
                if let Some(old) = &old {
                    derived_keys(r.namespace, &r.key, old, &mut old_ix);
                }
                for k in old_ix.iter().filter(|k| !new_ix.contains(k)) {
                    index.remove(k.as_slice()).map_err(storage)?;
                }
                for k in new_ix.iter().filter(|k| !old_ix.contains(k)) {
                    index.insert(k.as_slice(), ()).map_err(storage)?;
                }
            }
            let mut nodes = tx.open_table(NODES).map_err(storage)?;
            for (k, n) in c.nodes {
                let key = k.to_bytes();
                match n {
                    Some(n) => {
                        nodes.insert(key.as_slice(), n.encode().as_slice()).map_err(storage)?;
                    }
                    None => {
                        nodes.remove(key.as_slice()).map_err(storage)?;
                    }
                }
            }
            let mut blocks = tx.open_table(BLOCKS).map_err(storage)?;
            let mut hashes = tx.open_table(HASHES).map_err(storage)?;
            let mut txs = tx.open_table(TXS).map_err(storage)?;
            let mut history = tx.open_table(HISTORY).map_err(storage)?;
            let mut certs = tx.open_table(CERTS).map_err(storage)?;
            let mut heads = tx.open_table(EPOCH_HEADS).map_err(storage)?;
            let height = c.point.height;
            if let Some(m) = c.block {
                blocks.insert(height, borsh::to_vec(m)?.as_slice()).map_err(storage)?;
                hashes.insert(&m.hash[..], height).map_err(storage)?;
                for (i, id) in m.txs.iter().enumerate() {
                    let mut at = height.to_be_bytes().to_vec();
                    at.extend_from_slice(&(i as u32).to_be_bytes());
                    txs.insert(&id[..], at.as_slice()).map_err(storage)?;
                }
                for (i, account) in &m.history {
                    history.insert(history_key(account, height, *i).as_slice(), ()).map_err(storage)?;
                }
                if c.epoch_head {
                    heads.insert(height, [&m.hash[..], &m.state_root[..]].concat().as_slice()).map_err(storage)?;
                }
            }
            if let Some((h, cert)) = c.cert {
                certs.insert(h, cert).map_err(storage)?;
            }
            // Pruning: a few heights' rows per commit.
            let target = c.prune_below.min(c.point.pruned_below.saturating_add(MAX_PRUNE_PER_COMMIT)).min(height);
            for h in c.point.pruned_below..target {
                let meta = blocks.remove(h).map_err(storage)?.map(|g| decode::<BlockMeta>(g.value())).transpose()?;
                if let Some(m) = meta {
                    hashes.remove(&m.hash[..]).map_err(storage)?;
                    for id in &m.txs {
                        txs.remove(&id[..]).map_err(storage)?;
                    }
                    for (i, account) in &m.history {
                        history.remove(history_key(account, h, *i).as_slice()).map_err(storage)?;
                    }
                }
                if heads.get(h).map_err(storage)?.is_none() {
                    certs.remove(h).map_err(storage)?;
                }
            }
            c.point.pruned_below = c.point.pruned_below.max(target);
            let mut meta = tx.open_table(META).map_err(storage)?;
            meta.insert("commit", borsh::to_vec(&*c.point)?.as_slice()).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(())
    }

    /// Stores a certificate outside a block commit (a certificate that arrived late).
    pub fn put_cert(&self, height: u64, cert: &[u8]) -> Result<()> {
        let mut tx = self.db.begin_write().map_err(storage)?;
        tx.set_quick_repair(true);
        tx.open_table(CERTS).map_err(storage)?.insert(height, cert).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
}

/// A read-only snapshot of the store (one redb read transaction). It is the state tree's node
/// store, and what API reads use, so both see exactly the committed block.
pub struct Snap {
    nodes: ReadOnlyTable<&'static [u8], &'static [u8]>,
    txn: ReadTransaction,
}

impl NodeStore for Snap {
    fn get(&self, key: &NodeKey) -> Result<Option<Node>> {
        match self.nodes.get(key.to_bytes().as_slice()).map_err(storage)? {
            Some(v) => Ok(Some(Node::decode(v.value())?)),
            None => Ok(None),
        }
    }
}

impl Snap {
    pub fn block_meta(&self, height: u64) -> Result<Option<BlockMeta>> {
        let t = self.txn.open_table(BLOCKS).map_err(storage)?;
        t.get(height).map_err(storage)?.map(|v| decode(v.value())).transpose()
    }

    pub fn height_of(&self, hash: &Hash) -> Result<Option<u64>> {
        let t = self.txn.open_table(HASHES).map_err(storage)?;
        Ok(t.get(&hash[..]).map_err(storage)?.map(|v| v.value()))
    }

    /// Where a finalized tx is: (height, index in the block).
    pub fn tx(&self, id: &Hash) -> Result<Option<(u64, usize)>> {
        let t = self.txn.open_table(TXS).map_err(storage)?;
        let Some(v) = t.get(&id[..]).map_err(storage)? else { return Ok(None) };
        let v = v.value();
        if v.len() != 12 {
            bail!("bad tx index row");
        }
        Ok(Some((u64::from_be_bytes(v[..8].try_into().unwrap()), u32::from_be_bytes(v[8..].try_into().unwrap()) as usize)))
    }

    /// An account's newest history entries (height, tx index), newest first.
    pub fn history(&self, account: &str, limit: usize) -> Result<Vec<(u64, usize)>> {
        let t = self.txn.open_table(HISTORY).map_err(storage)?;
        let mut lo = account.as_bytes().to_vec();
        lo.push(0);
        let mut hi = account.as_bytes().to_vec();
        hi.push(1);
        let mut out = vec![];
        for r in t.range(lo.as_slice()..hi.as_slice()).map_err(storage)?.rev().take(limit) {
            let (k, _) = r.map_err(storage)?;
            let k = k.value();
            let tail = &k[k.len() - 12..];
            out.push((u64::from_be_bytes(tail[..8].try_into().unwrap()), u32::from_be_bytes(tail[8..].try_into().unwrap()) as usize));
        }
        Ok(out)
    }

    /// Suffixes of the index keys under `(tag, a)`, in key order (`newest_first`: reversed), at
    /// most `limit`.
    pub fn index(&self, tag: u8, a: &[u8], newest_first: bool, limit: usize) -> Result<Vec<Vec<u8>>> {
        let t = self.txn.open_table(INDEX).map_err(storage)?;
        let lo = ix_prefix(tag, a);
        let mut hi = lo.clone();
        *hi.last_mut().unwrap() = 1;
        let range = t.range(lo.as_slice()..hi.as_slice()).map_err(storage)?;
        let it: Box<dyn Iterator<Item = _>> = if newest_first { Box::new(range.rev()) } else { Box::new(range) };
        let mut out = vec![];
        for r in it.take(limit) {
            let (k, _) = r.map_err(storage)?;
            out.push(k.value()[lo.len()..].to_vec());
        }
        Ok(out)
    }

    pub fn cert(&self, height: u64) -> Result<Option<Vec<u8>>> {
        let t = self.txn.open_table(CERTS).map_err(storage)?;
        Ok(t.get(height).map_err(storage)?.map(|v| v.value().to_vec()))
    }

    /// (hash, state root) of an epoch's last block, kept through pruning.
    pub fn epoch_head(&self, height: u64) -> Result<Option<(Hash, Hash)>> {
        let t = self.txn.open_table(EPOCH_HEADS).map_err(storage)?;
        let Some(v) = t.get(height).map_err(storage)? else { return Ok(None) };
        let v = v.value();
        Ok(Some((v[..32].try_into()?, v[32..64].try_into()?)))
    }

    pub fn record_count(&self) -> Result<u64> {
        let t = self.txn.open_table(RECORDS).map_err(storage)?;
        t.len().map_err(storage)
    }
}

// ------------------------------------------------------------------------------ segment logs

/// Heights per log segment for new homes (a few hours of blocks at 1 s).
pub const SEGMENT_HEIGHTS: u64 = 10_000;

/// `layout.json`: fixed when a home is created.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Layout {
    pub segment_heights: u64,
}

impl Layout {
    /// Reads `layout.json`, or writes it with `segment_heights` for a new home.
    pub fn load_or_create(home: &Path, segment_heights: u64) -> Result<Layout> {
        let path = home.join("layout.json");
        match fs::read(&path) {
            Ok(b) => {
                let l: Layout = serde_json::from_slice(&b).with_context(|| format!("{}", path.display()))?;
                if l.segment_heights == 0 {
                    bail!("{}: segment_heights must be positive", path.display());
                }
                Ok(l)
            }
            Err(_) => {
                let l = Layout { segment_heights: segment_heights.max(1) };
                write_durably(&path, &serde_json::to_vec_pretty(&l)?)?;
                Ok(l)
            }
        }
    }
}

/// Writes a file so that after a crash it holds either the old contents or the new ones: write a
/// temporary file, fsync it, rename it over the target, fsync the directory.
pub fn write_durably(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    {
        let mut f = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// Fsyncs a directory, so files created or renamed in it survive a crash.
pub fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir).and_then(|d| d.sync_all()).with_context(|| format!("syncing {}", dir.display()))
}

/// How a log frames its entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// One JSON line per entry (`blocks/`).
    Lines,
    /// `height u64 LE ‖ len u32 LE ‖ bytes` (`cblocks/`).
    Records,
}

const RECORD_HEADER: usize = 12;

/// Read side of a segmented log; shared with the API (no consensus lock).
pub struct SegReader {
    dir: PathBuf,
    ext: &'static str,
    pub framing: Framing,
    pub seg_heights: u64,
    files: Mutex<Vec<(u64, Arc<File>)>>,
}

impl SegReader {
    /// First height of the segment holding `height`.
    pub fn seg_of(&self, height: u64) -> u64 {
        (height.max(1) - 1) / self.seg_heights * self.seg_heights + 1
    }

    pub fn path(&self, seg: u64) -> PathBuf {
        self.dir.join(format!("{seg:012}.{}", self.ext))
    }

    /// Segment first heights on disk, ascending.
    pub fn segments(&self) -> Vec<u64> {
        let suffix = format!(".{}", self.ext);
        let mut v: Vec<u64> = fs::read_dir(&self.dir).map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(&suffix)?.parse().ok()).collect()).unwrap_or_default();
        v.sort_unstable();
        v
    }

    fn file(&self, seg: u64) -> Result<Arc<File>> {
        let mut files = self.files.lock();
        if let Some((_, f)) = files.iter().find(|(s, _)| *s == seg) {
            return Ok(f.clone());
        }
        let f = Arc::new(File::open(self.path(seg))?);
        if files.len() >= 8 {
            files.remove(0);
        }
        files.push((seg, f.clone()));
        Ok(f)
    }

    /// Drops cached handles to a deleted segment (so its disk space is freed).
    fn forget(&self, seg: u64) {
        self.files.lock().retain(|(s, _)| *s != seg);
    }

    /// The payload at `pos` (without its framing).
    pub fn read(&self, pos: Pos) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; pos.len as usize];
        self.file(pos.seg)?.read_exact_at(&mut buf, pos.off)?;
        match self.framing {
            Framing::Lines => {
                if buf.pop() != Some(b'\n') {
                    bail!("log entry at {pos:?} isn't a line");
                }
            }
            Framing::Records => {
                if buf.len() < RECORD_HEADER {
                    bail!("log entry at {pos:?} is truncated");
                }
                buf.drain(..RECORD_HEADER);
            }
        }
        Ok(buf)
    }

    /// Visits the entries from `from` (a segment and offset) to the end of the log, in order:
    /// `f(pos, payload, height)` (height: the record header's, or 0 for lines). Stops early when
    /// `f` returns false. A torn last entry in the last segment is truncated away (it was never
    /// acknowledged); a malformed entry anywhere else is an error.
    pub fn scan(&self, from: (u64, u64), mut f: impl FnMut(Pos, &[u8], u64) -> Result<bool>) -> Result<()> {
        let segs: Vec<u64> = self.segments().into_iter().filter(|s| *s >= from.0).collect();
        for (i, &seg) in segs.iter().enumerate() {
            let last = i + 1 == segs.len();
            let start = if seg == from.0 { from.1 } else { 0 };
            let path = self.path(seg);
            let mut file = File::open(&path)?;
            let size = file.metadata()?.len();
            if start > size {
                bail!("{} is shorter than the store expects ({size} < {start} bytes)", path.display());
            }
            file.seek(SeekFrom::Start(start))?;
            let mut r = BufReader::with_capacity(1 << 20, file);
            let mut off = start;
            let mut torn = None;
            loop {
                match self.framing {
                    Framing::Lines => {
                        let mut line = vec![];
                        let n = r.read_until(b'\n', &mut line)?;
                        if n == 0 {
                            break;
                        }
                        if line.last() != Some(&b'\n') {
                            torn = Some(off);
                            break;
                        }
                        let pos = Pos { seg, off, len: n as u32 };
                        off += n as u64;
                        if line.len() == 1 {
                            continue; // blank line
                        }
                        if !f(pos, &line[..n - 1], 0)? {
                            return Ok(());
                        }
                    }
                    Framing::Records => {
                        let mut head = [0u8; RECORD_HEADER];
                        let got = read_full(&mut r, &mut head)?;
                        if got == 0 {
                            break;
                        }
                        if got < RECORD_HEADER {
                            torn = Some(off);
                            break;
                        }
                        let height = u64::from_le_bytes(head[..8].try_into().unwrap());
                        let len = u32::from_le_bytes(head[8..].try_into().unwrap()) as usize;
                        let mut body = vec![0u8; len.min(64 << 20)];
                        if len > body.len() || read_full(&mut r, &mut body)? < len {
                            torn = Some(off);
                            break;
                        }
                        let pos = Pos { seg, off, len: (RECORD_HEADER + len) as u32 };
                        off += pos.len as u64;
                        if !f(pos, &body, height)? {
                            return Ok(());
                        }
                    }
                }
            }
            if let Some(at) = torn {
                if !last {
                    bail!("{} has a torn entry at byte {at} but isn't the last segment", path.display());
                }
                tracing::warn!(file = %path.display(), at, "dropping a torn entry at the end of a log (crash during a write)");
                OpenOptions::new().write(true).open(&path)?.set_len(at)?;
            }
        }
        Ok(())
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// A segmented append-only log (write side; reads go through [`SegLog::reader`]).
pub struct SegLog {
    reader: Arc<SegReader>,
    cur: Option<(u64, File, u64)>,
    unsynced: bool,
}

impl SegLog {
    pub fn open(dir: &Path, ext: &'static str, framing: Framing, seg_heights: u64) -> Result<SegLog> {
        fs::create_dir_all(dir)?;
        let reader = Arc::new(SegReader { dir: dir.to_path_buf(), ext, framing, seg_heights, files: Mutex::new(vec![]) });
        Ok(SegLog { reader, cur: None, unsynced: false })
    }

    pub fn reader(&self) -> Arc<SegReader> {
        self.reader.clone()
    }

    fn segment(&mut self, seg: u64) -> Result<&mut (u64, File, u64)> {
        if self.cur.as_ref().is_none_or(|c| c.0 != seg) {
            self.sync()?;
            let path = self.reader.path(seg);
            let created = !path.exists();
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            if created {
                sync_dir(&self.reader.dir)?;
            }
            let len = file.metadata()?.len();
            self.cur = Some((seg, file, len));
        }
        Ok(self.cur.as_mut().unwrap())
    }

    /// Appends the entry for `height` (to the segment that holds it). Not durable until
    /// [`SegLog::sync`].
    pub fn append(&mut self, height: u64, payload: &[u8]) -> Result<Pos> {
        let framing = self.reader.framing;
        let seg = self.reader.seg_of(height);
        let mut entry = Vec::with_capacity(payload.len() + RECORD_HEADER);
        match framing {
            Framing::Lines => {
                entry.extend_from_slice(payload);
                entry.push(b'\n');
            }
            Framing::Records => {
                entry.extend_from_slice(&height.to_le_bytes());
                entry.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                entry.extend_from_slice(payload);
            }
        }
        let cur = self.segment(seg)?;
        cur.1.write_all(&entry)?;
        let pos = Pos { seg, off: cur.2, len: entry.len() as u32 };
        cur.2 += entry.len() as u64;
        self.unsynced = true;
        Ok(pos)
    }

    pub fn sync(&mut self) -> Result<()> {
        if self.unsynced {
            if let Some((_, f, _)) = &self.cur {
                f.sync_data()?;
            }
            self.unsynced = false;
        }
        Ok(())
    }

    /// Cuts the log right after `end` (segment, offset): later bytes and segments are deleted.
    pub fn truncate(&mut self, end: (u64, u64)) -> Result<()> {
        self.cur = None;
        for seg in self.reader.segments() {
            if seg > end.0 {
                self.reader.forget(seg);
                fs::remove_file(self.reader.path(seg))?;
            } else if seg == end.0 {
                let f = OpenOptions::new().write(true).open(self.reader.path(seg))?;
                if f.metadata()?.len() > end.1 {
                    f.set_len(end.1)?;
                    f.sync_all()?;
                }
            }
        }
        sync_dir(&self.reader.dir)
    }

    /// Deletes every segment whose heights are all below `height`. Returns how many.
    pub fn prune_below(&mut self, height: u64) -> Result<usize> {
        let mut n = 0;
        for seg in self.reader.segments() {
            if seg.saturating_add(self.reader.seg_heights) <= height && self.cur.as_ref().is_none_or(|c| c.0 != seg) {
                self.reader.forget(seg);
                fs::remove_file(self.reader.path(seg))?;
                n += 1;
            }
        }
        if n > 0 {
            sync_dir(&self.reader.dir)?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("blockula-store-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn segmented_logs_append_read_scan_truncate_and_prune() {
        let d = dir("seg");
        for framing in [Framing::Lines, Framing::Records] {
            let mut log = SegLog::open(&d.join(format!("{framing:?}")), "log", framing, 3).unwrap();
            let r = log.reader();
            let mut at = vec![];
            for h in 1..=10u64 {
                at.push(log.append(h, format!("entry {h}").as_bytes()).unwrap());
            }
            log.sync().unwrap();
            assert_eq!(r.segments(), vec![1, 4, 7, 10]);
            for (i, p) in at.iter().enumerate() {
                assert_eq!(r.read(*p).unwrap(), format!("entry {}", i + 1).as_bytes());
            }
            // Scan from the middle of a segment.
            let mut seen = vec![];
            r.scan((at[4].seg, at[4].off), |p, b, h| {
                seen.push((p, String::from_utf8(b.to_vec()).unwrap(), h));
                Ok(true)
            })
            .unwrap();
            assert_eq!(seen.len(), 6);
            assert_eq!(seen[0].0, at[4]);
            if framing == Framing::Records {
                assert_eq!(seen.iter().map(|s| s.2).collect::<Vec<_>>(), (5..=10).collect::<Vec<_>>());
            }
            // A torn tail is cut off; the next append lands where it was.
            drop(log);
            let mut f = OpenOptions::new().append(true).open(r.path(10)).unwrap();
            f.write_all(b"tor").unwrap();
            let mut n = 0;
            r.scan((10, 0), |_, _, _| {
                n += 1;
                Ok(true)
            })
            .unwrap();
            assert_eq!(n, 1);
            assert_eq!(fs::metadata(r.path(10)).unwrap().len(), at[9].end());
            // Truncate after height 5, prune below 7.
            let mut log = SegLog::open(&d.join(format!("{framing:?}")), "log", framing, 3).unwrap();
            log.truncate((at[4].seg, at[4].end())).unwrap();
            assert_eq!(r.segments(), vec![1, 4]);
            assert_eq!(fs::metadata(r.path(4)).unwrap().len(), at[4].end());
            let p6 = log.append(6, b"again 6").unwrap();
            assert_eq!(p6.off, at[5].off);
            log.append(7, b"7").unwrap();
            assert_eq!(log.prune_below(7).unwrap(), 2);
            assert_eq!(r.segments(), vec![7]);
        }
        fs::remove_dir_all(d).ok();
    }

    #[test]
    fn durable_writes_replace_atomically() {
        let d = dir("durable");
        let p = d.join("x.json");
        write_durably(&p, b"one").unwrap();
        write_durably(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert!(!d.join("x.json.tmp").exists());
        fs::remove_dir_all(d).ok();
    }
}
