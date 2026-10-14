//! Certified state snapshots and fast sync.
//!
//! Every `CHECKPOINT_INTERVAL` blocks is a checkpoint height C. Consensus requires the block at
//! C + `CHECKPOINT_DELAY` to carry `exec_height = C` and `exec_root = state_root(C)`, and every
//! verifier must have executed C and checked it before voting. So the finalization certificate
//! of that block (or of any descendant, via hash links) certifies `state_root(C)`.
//!
//! Each node writes the state at its latest checkpoints (canonical records → zstd → 1 MiB
//! chunks, each BLAKE3-hashed) and serves a manifest: chunk hashes, the blocks C+1..=F, F's
//! finalization certificate, and the validator-set history (handoff proofs) needed to check that
//! certificate. A joining node verifies everything, rebuilds the state and its tree from the
//! records, checks the root against the certified one, executes C+1..=F and carries on syncing.

use crate::chain::{Chain, Genesis, State};
use crate::consensus::{self, CBlock};
use crate::epochs::{self, EpochRegistry};
use crate::pq::MlDsaScheme;
use anyhow::{anyhow, bail, Context, Result};
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
/// Snapshots kept per node.
pub const KEEP_SNAPSHOTS: usize = 2;
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

/// Serializes and writes the state at `height` (call from a background thread with a clone).
pub fn write(home: &Path, height: u64, state_root: &str, state: &State) -> Result<SnapshotMeta> {
    let mut enc = zstd::Encoder::new(vec![], 3)?;
    write_state(state, &mut enc)?;
    let packed = enc.finish()?;
    let tmp = dir(home).join(format!("{height}.tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut chunks = vec![];
    for (i, c) in packed.chunks(CHUNK_SIZE).enumerate() {
        std::fs::write(tmp.join(format!("chunk-{i}")), c)?;
        chunks.push(ChunkInfo { hash: hex::encode(crate::crypto::hash(c)), size: c.len() });
    }
    let meta = SnapshotMeta { height, state_root: state_root.to_string(), chunks };
    std::fs::write(tmp.join("meta.json"), serde_json::to_vec_pretty(&meta)?)?;
    let final_dir = dir(home).join(height.to_string());
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&tmp, &final_dir)?;
    prune(home);
    Ok(meta)
}

/// Snapshot heights on disk, newest first.
pub fn list(home: &Path) -> Vec<u64> {
    let mut hs: Vec<u64> = std::fs::read_dir(dir(home)).map(|d| d.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect()).unwrap_or_default();
    hs.sort_unstable_by(|a, b| b.cmp(a));
    hs
}

fn prune(home: &Path) {
    for h in list(home).into_iter().skip(KEEP_SNAPSHOTS) {
        let _ = std::fs::remove_dir_all(dir(home).join(h.to_string()));
    }
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
    /// Validator-set history with handoff proofs.
    pub epochs: EpochRegistry,
}

/// Builds the manifest for the newest snapshot this node can prove.
pub fn build_manifest(c: &Chain) -> Result<Manifest> {
    for height in list(&c.home) {
        let Some(meta) = meta(&c.home, height) else { continue };
        let certifier = height + CHECKPOINT_DELAY;
        let mut blocks = vec![];
        let mut fin = None;
        for h in height + 1..=c.height().min(height + 2 * CHECKPOINT_INTERVAL) {
            let Some(b) = c.cblock(h) else { break };
            blocks.push(hex::encode(&b));
            if h >= certifier {
                if let Some(cert) = c.cert(h) {
                    fin = Some(hex::encode(cert));
                    break;
                }
            }
        }
        let Some(finalization) = fin else { continue };
        return Ok(Manifest { chain_id: c.genesis.chain_id.clone(), height, state_root: meta.state_root, chunks: meta.chunks, blocks, finalization, epochs: c.epochs.clone() });
    }
    bail!("no certified snapshot yet (one is taken every {CHECKPOINT_INTERVAL} blocks and certified {CHECKPOINT_DELAY} blocks later)")
}

/// A manifest whose certificate, block links and checkpoint commitment all check out.
pub struct Verified {
    pub height: u64,
    pub state_root: String,
    pub chunks: Vec<ChunkInfo>,
    pub blocks: Vec<CBlock>,
    pub finalization: Vec<u8>,
    pub epochs: EpochRegistry,
}

pub fn verify_manifest(g: &Genesis, m: &Manifest) -> Result<Verified> {
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
        expect += 1;
    }
    // The block at C + d commits to the checkpoint's state root.
    let certifier = blocks.iter().find(|b| b.height == m.height + CHECKPOINT_DELAY).ok_or_else(|| anyhow!("manifest doesn't include the certifying block"))?;
    if certifier.exec_height != m.height || hex::encode(certifier.exec_root) != m.state_root {
        bail!("certifying block doesn't commit to this snapshot's state root");
    }
    let last = blocks.last().unwrap();
    let params = g.params();
    let epoch = params.epoch_of(last.height);
    let set = epochs::verify_chain(g, &m.epochs, epoch).context("validator set history")?;
    let participants = consensus::keys_of(set.iter().map(|s| s.as_str())).ok_or_else(|| anyhow!("bad validator keys"))?;
    let verifier = MlDsaScheme::verifier(&consensus::namespace(&g.chain_id), participants);
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
    Ok(Verified { height: m.height, state_root: m.state_root.clone(), chunks: m.chunks.clone(), blocks, finalization: fin_bytes, epochs: m.epochs.clone() })
}

/// Checks one downloaded chunk against the manifest.
pub fn check_chunk(v: &Verified, i: usize, bytes: &[u8]) -> Result<()> {
    let info = v.chunks.get(i).ok_or_else(|| anyhow!("no chunk {i}"))?;
    if bytes.len() != info.size || hex::encode(crate::crypto::hash(bytes)) != info.hash {
        bail!("chunk {i} doesn't match the manifest");
    }
    Ok(())
}

/// Rebuilds the state (and its tree) from verified chunks and checks it against the certified
/// root. The state carries its own parameters, so `_g` isn't needed (the manifest was checked
/// against it already). Memory before the root check is bounded: the chunks (at most
/// `MAX_SNAPSHOT_CHUNKS`) are decompressed as a stream, never into one buffer, each record is
/// capped (`state::MAX_KEY_BYTES`, `state::MAX_VALUE_BYTES`) and so is the total
/// (`MAX_STATE_BYTES`); what remains is the state being rebuilt.
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

/// Decompresses (up to `MAX_STATE_BYTES`) and decodes a packed snapshot.
pub fn decode_state(packed: &[u8]) -> Result<State> {
    decode_state_from(packed)
}

/// [`decode_state`] from a stream of packed bytes.
fn decode_state_from(packed: impl Read) -> Result<State> {
    read_state(&mut std::io::BufReader::new(zstd::Decoder::new(packed)?.take(MAX_STATE_BYTES)))
}

/// Reads a list of chunks as one stream, without concatenating them.
struct Chunks<'a> {
    chunks: &'a [Vec<u8>],
    at: usize,
    off: usize,
}

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

/// Fetches, verifies and installs the newest certified snapshot from `peers` into an empty chain.
pub async fn fast_sync(chain: &crate::consensus::Shared, peers: &[String]) -> Result<u64> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build()?;
    let g = chain.lock().genesis.clone();
    let mut last_err = anyhow!("no peers");
    for peer in peers {
        match fast_sync_from(&client, &g, chain, peer).await {
            Ok(h) => return Ok(h),
            Err(e) => {
                tracing::debug!(peer = %peer, error = %e, "fast sync failed");
                last_err = e.context(format!("from {peer}"));
            }
        }
    }
    Err(last_err)
}

async fn fast_sync_from(client: &reqwest::Client, g: &Genesis, chain: &crate::consensus::Shared, peer: &str) -> Result<u64> {
    let body = crate::node::get_capped(client, &format!("{peer}/api/snapshot/latest"), 64 << 20).await?;
    let m: Manifest = serde_json::from_slice(&body)?;
    let v = verify_manifest(g, &m)?;
    println!("📦 snapshot #{} ({} chunks) certified by validators; downloading…", v.height, v.chunks.len());
    let mut chunks = vec![];
    for batch in (0..v.chunks.len()).collect::<Vec<_>>().chunks(4) {
        let futs = batch.iter().map(|i| {
            let url = format!("{peer}/api/snapshot/{}/chunk/{i}", v.height);
            let client = client.clone();
            async move { crate::node::get_capped(&client, &url, CHUNK_SIZE + 1024).await }
        });
        for (i, r) in batch.iter().zip(futures::future::join_all(futs).await) {
            let bytes = r?;
            check_chunk(&v, *i, &bytes)?;
            chunks.push(bytes);
        }
    }
    let state = restore(g, &v, &chunks)?;
    // Consensus anchors each epoch at the previous epoch's last block; if that's before the
    // snapshot, ask the peer for its hash (only used to seed this epoch's engine).
    let params = g.params();
    let epoch = params.epoch_of(v.blocks.last().map(|b| b.height).unwrap_or(v.height) + 1);
    let anchor = if epoch == 0 { None } else { Some(params.last_of(epoch - 1)) };
    let epoch_start = match anchor {
        Some(h) if h < v.height => {
            let b = crate::node::get_capped(client, &format!("{peer}/api/block/{h}"), 1 << 20).await.ok();
            b.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|v| v["hash"].as_str().map(|s| (h, s.to_string())))
        }
        _ => None,
    };
    let mut c = chain.lock();
    c.install_snapshot(&v, state, &chunks, epoch_start)?;
    Ok(c.height())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Account;

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
}
