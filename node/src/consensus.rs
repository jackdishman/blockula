//! Multi-validator BFT: Commonware simplex orders blocks, marshal disseminates and backfills them,
//! and finalized blocks are executed in order by `Chain::execute` (deferred execution).
//!
//! Block flow:
//! * **propose** (leader): wait until `parent.timestamp + block_time`, pick mempool txs (highest
//!   tip per byte first) that apply on top of our executed state plus any not-yet-executed
//!   ancestors, and attach the latest state root we've executed (`exec_height`, `exec_root`).
//! * **verify** (everyone, before voting): view and parent view against the consensus context,
//!   the protocol version in force at the height (`upgrade.rs`; no vote at all past a halt or for
//!   a version this binary can't run), structure, size limits, timestamps, proposer == leader,
//!   stateless tx checks
//!   (canonical wire bytes, chain id, address prefix, validity window, ML-DSA signature, fee), and
//!   the attached state root (waiting until we've executed that height). Only permanently invalid
//!   blocks get `false`; anything that can still change keeps the verdict pending.
//! * **finalize**: marshal delivers finalized blocks in order to [`Executor`], which executes them
//!   (invalid txs are skipped) and acknowledges only once the block logs are fsynced and the
//!   state database has committed the block (`store.rs`).

use crate::chain::{Chain, Finalized, Genesis};
use crate::pq::{MlDsaScheme, PrivateKey, PublicKey};
use crate::types::Tx;
use bytes::{Buf, BufMut};
use commonware_broadcast::buffered;
use commonware_codec::{Decode as _, EncodeSize, Error as CodecError, Read, Write, Encode};
use commonware_consensus::{
    Application, Block, Heightable, Reporter, Reporters,
    marshal::{self, Update, ancestry::Ancestry, core::Actor as MarshalActor, resolver::p2p as marshal_resolver, standard::Inline},
    simplex::{
        self,
        config::{ForwardPolicy, SkipBudget, SkipPolicy},
        elector::RoundRobin,
        types::{Activity, Context as SimplexContext, Finalization},
    },
    types::{Epoch, FixedEpocher, Height, ViewDelta},
};
use commonware_cryptography::{Blake3, Digestible, Hasher, Signer as _, blake3::Digest, certificate::{self, Scoped, Scheme as _}};
use commonware_p2p::{Blocker, Manager, Provider, Receiver, Recipients, Sender, TrackedPeers, utils::mux::{Builder as _, Muxer}};
use commonware_parallel::Sequential;
use commonware_runtime::{BufferPooler, Clock, Handle, Metrics, Spawner, Storage, buffer::paged::CacheRef};
use commonware_storage::{archive::prunable, translator::TwoCap};
use commonware_utils::{NZU64, NZUsize, ordered::Set};
use futures::StreamExt;
use parking_lot::Mutex;
use rand_core::CryptoRng;
use std::{collections::HashMap, num::{NonZeroU16, NonZeroU32, NonZeroU64, NonZeroUsize}, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use tracing::{debug, error, info, warn};

pub type Shared = Arc<Mutex<Chain>>;
pub type MarshalMailbox = marshal::core::Mailbox<MlDsaScheme, marshal::standard::Standard<CBlock>>;

pub const MAX_BLOCK_TXS: usize = 512;
pub use crate::types::MAX_TX_BYTES;
/// A proposal's `exec_height` may trail the verifier's executed height by at most this much
/// (so a proposer can't dodge the state-root check by citing an ancient height).
pub const MAX_EXEC_LAG: u64 = 16;
pub const MAX_BLOCK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MESSAGE_SIZE: u32 = 8 * 1024 * 1024;
/// How far in the future (by our clock) a block timestamp may be.
pub const MAX_CLOCK_DRIFT_MS: u64 = 1_000;

/// p2p channels.
pub const CH_VOTES: u64 = 0;
pub const CH_CERTS: u64 = 1;
pub const CH_RESOLVER: u64 = 2;
pub const CH_BACKFILL: u64 = 3;
pub const CH_BLOCKS: u64 = 4;
pub const CH_MEMPOOL: u64 = 5;
pub const CHANNELS: [u64; 6] = [CH_VOTES, CH_CERTS, CH_RESOLVER, CH_BACKFILL, CH_BLOCKS, CH_MEMPOOL];

pub fn namespace(chain_id: &str) -> Vec<u8> {
    format!("blockula/{chain_id}").into_bytes()
}

// ------------------------------------------------------------------------------------ block

/// Protocol version this binary executes (MAINNET-DESIGN 3.9: the version in force at a height).
pub const PROTOCOL_VERSION: u32 = 1;
/// Newest protocol version this binary supports, signalled in every block it proposes.
pub const SUPPORTED_PROTOCOL_VERSION: u32 = 1;

/// The block consensus agrees on (CBlock v2). Txs are carried in their wire form
/// (`Tx::to_bytes`), so the digest is over canonical bytes and every node parses the same thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CBlock {
    /// Protocol version in force at `height`.
    pub version: u32,
    pub parent: Digest,
    pub height: u64,
    /// The simplex view this block was proposed in, and its parent's view, both from the
    /// consensus context (verifiers check them). Views in between were nullified.
    pub view: u64,
    pub parent_view: u64,
    pub timestamp_ms: u64,
    /// Address of the proposer (must be the round's leader). Tips go here.
    pub proposer: String,
    /// Latest height the proposer had executed, and its state root (checked by verifiers that
    /// have executed it too).
    pub exec_height: u64,
    pub exec_root: [u8; 32],
    /// Newest protocol version the proposer's binary supports (upgrade readiness, display only).
    pub signal: u32,
    pub txs: Vec<Vec<u8>>,
    digest: Digest,
}

impl CBlock {
    /// A block at `height` on `parent` under this binary's protocol version and signal.
    #[allow(clippy::too_many_arguments)]
    pub fn new(parent: Digest, height: u64, view: u64, parent_view: u64, timestamp_ms: u64, proposer: String, exec_height: u64, exec_root: [u8; 32], txs: Vec<Vec<u8>>) -> Self {
        Self { version: PROTOCOL_VERSION, parent, height, view, parent_view, timestamp_ms, proposer, exec_height, exec_root, signal: SUPPORTED_PROTOCOL_VERSION, txs, digest: Digest([0; 32]) }.sealed()
    }

    /// This block under protocol `version`, signalling `signal` (re-sealed if either changes).
    pub fn with_versions(mut self, version: u32, signal: u32) -> Self {
        if (version, signal) == (self.version, self.signal) {
            return self;
        }
        (self.version, self.signal) = (version, signal);
        self.sealed()
    }

    /// Computes the digest over everything else.
    fn sealed(mut self) -> Self {
        self.digest = Blake3::hash(&[b"BLOCKULA_CBLOCK_V2", &self.encode_body()]);
        self
    }

    fn encode_body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.body_size());
        self.write_body(&mut out);
        out
    }

    fn body_size(&self) -> usize {
        4 + 32 + 8 * 4 + 4 + self.proposer.len() + 8 + 32 + 4 + 4 + self.txs.iter().map(|t| 4 + t.len()).sum::<usize>()
    }

    fn write_body(&self, buf: &mut impl BufMut) {
        buf.put_u32(self.version);
        buf.put_slice(&self.parent.0);
        buf.put_u64(self.height);
        buf.put_u64(self.view);
        buf.put_u64(self.parent_view);
        buf.put_u64(self.timestamp_ms);
        buf.put_u32(self.proposer.len() as u32);
        buf.put_slice(self.proposer.as_bytes());
        buf.put_u64(self.exec_height);
        buf.put_slice(&self.exec_root);
        buf.put_u32(self.signal);
        buf.put_u32(self.txs.len() as u32);
        for t in &self.txs {
            buf.put_u32(t.len() as u32);
            buf.put_slice(t);
        }
    }

    pub fn digest_bytes(&self) -> [u8; 32] {
        self.digest.0
    }

    pub fn parent_bytes(&self) -> [u8; 32] {
        self.parent.0
    }

    /// The block's txs, `None` where the bytes aren't a canonical wire-form tx.
    pub fn parse_txs(&self) -> Vec<Option<Tx>> {
        self.txs.iter().map(|t| Tx::from_bytes(t).ok()).collect()
    }
}

impl Write for CBlock {
    fn write(&self, buf: &mut impl BufMut) {
        self.write_body(buf);
    }
}
impl EncodeSize for CBlock {
    fn encode_size(&self) -> usize {
        self.body_size()
    }
}
impl Read for CBlock {
    type Cfg = ();
    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, CodecError> {
        fn need(buf: &impl Buf, n: usize) -> Result<(), CodecError> {
            if buf.remaining() < n { Err(CodecError::EndOfBuffer) } else { Ok(()) }
        }
        need(buf, 4 + 32 + 8 * 4 + 4)?;
        let version = buf.get_u32();
        let mut parent = [0u8; 32];
        buf.copy_to_slice(&mut parent);
        let height = buf.get_u64();
        let view = buf.get_u64();
        let parent_view = buf.get_u64();
        let timestamp_ms = buf.get_u64();
        let plen = buf.get_u32() as usize;
        if plen > 128 {
            return Err(CodecError::Invalid("CBlock", "proposer too long"));
        }
        need(buf, plen + 8 + 32 + 4 + 4)?;
        let mut p = vec![0u8; plen];
        buf.copy_to_slice(&mut p);
        let proposer = String::from_utf8(p).map_err(|_| CodecError::Invalid("CBlock", "proposer utf8"))?;
        let exec_height = buf.get_u64();
        let mut exec_root = [0u8; 32];
        buf.copy_to_slice(&mut exec_root);
        let signal = buf.get_u32();
        let n = buf.get_u32() as usize;
        if n > MAX_BLOCK_TXS {
            return Err(CodecError::Invalid("CBlock", "too many txs"));
        }
        let mut txs = Vec::with_capacity(n);
        let mut total = 0;
        for _ in 0..n {
            need(buf, 4)?;
            let len = buf.get_u32() as usize;
            total += len;
            if len > MAX_TX_BYTES || total > MAX_BLOCK_BYTES {
                return Err(CodecError::Invalid("CBlock", "tx too large"));
            }
            need(buf, len)?;
            let mut t = vec![0u8; len];
            buf.copy_to_slice(&mut t);
            txs.push(t);
        }
        Ok(CBlock { version, parent: Digest(parent), height, view, parent_view, timestamp_ms, proposer, exec_height, exec_root, signal, txs, digest: Digest([0; 32]) }.sealed())
    }
}
impl Digestible for CBlock {
    type Digest = Digest;
    fn digest(&self) -> Digest {
        self.digest
    }
}
impl Heightable for CBlock {
    fn height(&self) -> Height {
        Height::new(self.height)
    }
}
impl Block for CBlock {
    fn parent(&self) -> Digest {
        self.parent
    }
}

/// Height-0 block every node derives from genesis.json.
pub fn genesis_block(g: &Genesis) -> CBlock {
    let (id, prefix) = (g.chain_id.as_bytes(), g.address_prefix.as_bytes());
    let seed = Blake3::hash(&[b"BLOCKULA_GENESIS_V2", &(id.len() as u32).to_be_bytes(), id, &(prefix.len() as u32).to_be_bytes(), prefix, &g.created_at_ms.to_be_bytes(), serde_json::to_string(&g.validators).unwrap().as_bytes()]);
    let root = g.initial_state().root();
    CBlock::new(seed, 0, 0, 0, g.created_at_ms, String::new(), 0, hex_32(&root), vec![])
}

pub fn hex_32(s: &str) -> [u8; 32] {
    hex::decode(s).ok().and_then(|v| v.try_into().ok()).unwrap_or([0; 32])
}

#[allow(dead_code)]
pub fn participants(g: &Genesis) -> anyhow::Result<Set<PublicKey>> {
    let keys = g
        .validators
        .iter()
        .map(|v| hex::decode(&v.public_key).ok().and_then(|b| PublicKey::from_bytes(&b)).ok_or_else(|| anyhow::anyhow!("bad validator public key for {}", v.address)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Set::try_from(keys).map_err(|_| anyhow::anyhow!("duplicate validator keys in genesis"))
}

fn ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ------------------------------------------------------------------------------ application

#[derive(Clone)]
pub struct App {
    pub chain: Shared,
    pub block_time_ms: u64,
    /// The chain's address prefix (proposer addresses use it).
    pub address_prefix: String,
}

/// Clock skew per validator for the simulator (test builds only): `(chain, ms)`, keyed by the
/// address of the validator's `Shared` chain. `App::now_ms` shifts what that validator reads as
/// now, as a skewed system clock would (sim.rs clock-skew scenarios).
#[cfg(test)]
pub(crate) static CLOCK_SKEW_MS: parking_lot::Mutex<Vec<(usize, i64)>> = parking_lot::Mutex::new(Vec::new());

impl App {
    /// The wall clock this validator reads (ms since the Unix epoch): the runtime's, skewed in
    /// simulations.
    fn now_ms(&self, ctx: &impl Clock) -> u64 {
        let now = ms(ctx.current());
        #[cfg(test)]
        if let Some((_, skew)) = CLOCK_SKEW_MS.lock().iter().find(|(k, _)| *k == Arc::as_ptr(&self.chain) as usize) {
            return now.saturating_add_signed(*skew);
        }
        now
    }
}

/// Stateless checks every tx must pass: in a proposed block, over HTTP and from gossip. The chain
/// is named by its id and its genesis hash (the instance, audit #7).
pub fn check_tx_stateless(tx: &Tx, chain_id: &str, genesis_hash: &str, prefix: &str) -> anyhow::Result<()> {
    tx.check_chain(chain_id, genesis_hash)?;
    tx.check_format()?;
    tx.check_prefix(prefix)?;
    crate::chain::check_fee(tx)?;
    tx.verify_signature()
}

impl<E> Application<E> for App
where
    E: rand_core::Rng + Spawner + Metrics + Clock,
{
    type SigningScheme = MlDsaScheme;
    type Context = SimplexContext<Digest, PublicKey>;
    type Block = CBlock;
    type Input = ();

    async fn propose(&mut self, (ctx, cctx): (E, Self::Context), mut ancestry: impl Ancestry<CBlock>, _: ()) -> Option<CBlock> {
        let parent = ancestry.next().await?;
        let target = parent.timestamp_ms + self.block_time_ms;
        let now = self.now_ms(&ctx);
        if target > now {
            ctx.sleep(Duration::from_millis(target - now)).await;
        }
        let timestamp_ms = self.now_ms(&ctx).max(target);
        let height = parent.height + 1;
        let (view, parent_view) = (cctx.round.view().get(), cctx.parent.0.get());
        let proposer = cctx.leader.address(&self.address_prefix);
        // Nothing past --halt-height, a halt or a version this binary can't run (3.9). Blocks
        // carry the version in force at their height and signal what this binary supports.
        let (version, signal) = {
            let c = self.chain.lock();
            if !c.halt.allows(&c.state, height) {
                return None;
            }
            (crate::upgrade::version_at(&c.state, height), c.halt.supported)
        };

        // Blocks between what we've executed and the parent are ordered but not yet executed:
        // collect them so we don't re-propose (or overdraw) their txs.
        let exec_height = self.chain.lock().height();
        let mut pending = vec![parent.clone()];
        if parent.height > exec_height {
            while pending.last().unwrap().height > exec_height + 1 {
                if pending.len() > 64 {
                    // Far behind (still catching up): propose an empty block rather than guess
                    // (but never at a checkpoint-certifying height, which needs the real root).
                    if crate::snapshot::certifies(height).is_some() {
                        return None;
                    }
                    let c = self.chain.lock();
                    return Some(CBlock::new(parent.digest(), height, view, parent_view, timestamp_ms, proposer, c.height(), hex_32(&c.state.root()), vec![]).with_versions(version, signal));
                }
                pending.push(ancestry.next().await?);
            }
        } else {
            pending.clear();
        }
        // Build on a journal over the live state (rolled back afterwards) — no state clone.
        let (exec_height, exec_root, txs) = {
        let mut c = self.chain.lock();
        if c.height() != exec_height {
            // We executed more blocks meanwhile; drop the ones now executed.
            let h = c.height();
            pending.retain(|b| b.height > h);
        }
        let (chain_id, exec_height, exec_root) = (c.genesis.chain_id.clone(), c.height(), hex_32(&c.state.root()));
        let mut j = crate::chain::Journal::default();
        let mut included = std::collections::HashSet::new();
        for b in pending.iter().rev() {
            for tx in b.parse_txs().into_iter().flatten() {
                included.insert(tx.id());
                let _ = c.state.apply_j(&mut j, &tx, &chain_id, &b.proposer, b.height, false);
            }
        }
        let mut txs = vec![];
        let mut bytes = 0;
        // Senders whose earlier tx was left out: their later nonces can't apply.
        let mut stuck = std::collections::HashSet::new();
        let c = &mut *c; // borrow the mempool and the state separately
        for tx in crate::chain::proposal_order(&c.mempool) {
            if txs.len() >= MAX_BLOCK_TXS {
                break;
            }
            if stuck.contains(&tx.from) || included.contains(&tx.id()) {
                continue;
            }
            let raw = tx.to_bytes();
            if raw.len() > MAX_TX_BYTES || bytes + raw.len() > MAX_BLOCK_BYTES {
                stuck.insert(tx.from.clone());
                continue;
            }
            // Only propose txs that will succeed; a tx that would just burn its fee waits/expires.
            let mut tj = crate::chain::Journal::default();
            match c.state.apply_j(&mut tj, tx, &chain_id, &proposer, height, false) {
                Ok(crate::chain::Outcome::Applied) => j.absorb_pub(tj),
                Ok(crate::chain::Outcome::Failed(_)) | Err(_) => {
                    c.state.rollback(tj);
                    stuck.insert(tx.from.clone());
                    continue;
                }
            }
            bytes += raw.len();
            txs.push(raw);
        }
        c.state.rollback(j);
        (exec_height, exec_root, txs)
        };
        // Checkpoint rule: the block at C + CHECKPOINT_DELAY must commit to state_root(C).
        let (exec_height, exec_root) = match crate::snapshot::certifies(height) {
            Some(cp) => match wait_executed(&ctx, &self.chain, cp, Some(Duration::from_millis(3 * self.block_time_ms.max(200)))).await {
                Some(root) => (cp, root),
                None => {
                    warn!(height, checkpoint = cp, "haven't executed the checkpoint yet; skipping this proposal");
                    return None;
                }
            },
            None => (exec_height, exec_root),
        };
        debug!(height, view, txs = txs.len(), "proposing");
        Some(CBlock::new(parent.digest(), height, view, parent_view, timestamp_ms, proposer, exec_height, exec_root, txs).with_versions(version, signal))
    }

    /// Returns `false` only for permanently invalid blocks (the `Application::verify` contract):
    /// a lagging verifier keeps the verdict pending until it has executed what the block cites,
    /// and simplex's timeouts cancel the future if that takes too long.
    async fn verify(&mut self, (ctx, cctx): (E, Self::Context), mut ancestry: impl Ancestry<CBlock>) -> bool {
        let (Some(block), Some(parent)) = (ancestry.next().await, ancestry.next().await) else { return false };
        let reject = |why: &str| {
            warn!(height = block.height, why, "rejecting block");
            false
        };
        if block.view != cctx.round.view().get() || block.parent_view != cctx.parent.0.get() {
            return reject("view or parent_view doesn't match the consensus round");
        }
        // The protocol version in force at this height (3.9). An upgrade activates at least
        // MIN_UPGRADE_LEAD blocks after the block that schedules it, so once we've executed
        // `height - MIN_UPGRADE_LEAD` the version is certain (until then we wait).
        wait_executed(&ctx, &self.chain, block.height.saturating_sub(crate::upgrade::MIN_UPGRADE_LEAD), None).await;
        let expected = {
            let c = self.chain.lock();
            // Halted, past --halt-height, or a version this binary can't run: no vote either way.
            (c.halt.allows(&c.state, block.height)).then(|| crate::upgrade::version_at(&c.state, block.height))
        };
        let Some(expected) = expected else { return futures::future::pending().await };
        if block.version != expected {
            return reject("wrong protocol version for this height");
        }
        if block.parent != parent.digest() || block.height != parent.height + 1 {
            return reject("bad parent link");
        }
        if block.timestamp_ms < parent.timestamp_ms + self.block_time_ms {
            return reject("block too early");
        }
        if block.timestamp_ms > self.now_ms(&ctx) + MAX_CLOCK_DRIFT_MS {
            return reject("timestamp too far in the future");
        }
        if block.proposer != cctx.leader.address(&self.address_prefix) {
            return reject("proposer is not the leader");
        }
        if block.exec_height >= block.height {
            return reject("exec_height not below block height");
        }
        // Checkpoint rule: this block must commit to state_root(C).
        let checkpoint = crate::snapshot::certifies(block.height);
        if checkpoint.is_some_and(|cp| block.exec_height != cp) {
            return reject("checkpoint-certifying block must carry the checkpoint's exec_height");
        }
        let (chain_id, genesis_hash, max_validity) = {
            let c = self.chain.lock();
            // Our executed height only grows, so this stays true once it is.
            if block.exec_height + MAX_EXEC_LAG < c.height().min(parent.height) {
                return reject("exec_height too far behind");
            }
            (c.genesis.chain_id.clone(), c.genesis_hash.clone(), c.state.chain_params.max_tx_validity_blocks)
        };
        // Tx checks (signatures included) run without holding the chain lock.
        let mut seen = std::collections::HashSet::new();
        let mut ids = vec![];
        for raw in &block.txs {
            let Ok(tx) = Tx::from_bytes(raw) else { return reject("unparseable or non-canonical tx") };
            let id = tx.id();
            if !seen.insert(id.clone()) {
                return reject("duplicate tx");
            }
            if let Err(e) = check_tx_stateless(&tx, &chain_id, &genesis_hash, &self.address_prefix).and_then(|_| crate::chain::check_validity(&tx, block.height, max_validity)) {
                warn!(error = %e, "bad tx in block");
                return reject("invalid tx");
            }
            ids.push(id);
        }
        // The cited state root, once we've executed that height ourselves (until then the
        // verdict can still change, so we wait).
        match wait_executed(&ctx, &self.chain, block.exec_height, None).await {
            Some(root) if root == block.exec_root => {}
            Some(_) => return reject(if checkpoint.is_some() { "checkpoint state root mismatch" } else { "state root mismatch" }),
            // Below our history (we fast-synced past it): we can't check it, so we abstain.
            None => return futures::future::pending().await,
        }
        let c = self.chain.lock();
        if ids.iter().any(|id| c.has_tx(id)) {
            return reject("replays an already-finalized tx");
        }
        true
    }
}

/// Waits until we've executed `height` and returns its state root: `None` if we gave up after
/// `deadline`, or can't know it (it's below the snapshot we fast-synced from).
async fn wait_executed<E: Clock>(ctx: &E, chain: &Shared, height: u64, deadline: Option<Duration>) -> Option<[u8; 32]> {
    let step = Duration::from_millis(50);
    let mut waited = Duration::ZERO;
    loop {
        {
            let c = chain.lock();
            if c.height() >= height {
                return c.root_at(height).map(|r| hex_32(&r));
            }
        }
        if deadline.is_some_and(|d| waited >= d) {
            return None;
        }
        ctx.sleep(step).await;
        waited += step;
    }
}

// ------------------------------------------------------------------------------- reporters

/// Remembers finalization certificates by block digest so executed blocks can carry their proof.
#[derive(Clone, Default)]
pub struct CertCapture {
    certs: Arc<Mutex<HashMap<Digest, Vec<u8>>>>,
}

impl Reporter for CertCapture {
    type Activity = Activity<MlDsaScheme, Digest>;
    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        if let Activity::Finalization(f) = activity {
            let mut certs = self.certs.lock();
            if certs.len() > 4096 {
                certs.clear();
            }
            certs.insert(f.proposal.payload, f.encode().to_vec());
        }
        commonware_actor::Feedback::Ok
    }
}

/// Receives finalized blocks from marshal, in order, and executes them. After the last block of
/// an epoch it tells the orchestrator which epoch to enter next.
#[derive(Clone)]
pub struct Executor {
    pub chain: Shared,
    pub certs: CertCapture,
    pub epochs: futures::channel::mpsc::UnboundedSender<u64>,
    /// For pruning old finalized data out of marshal's archives.
    pub marshal: Option<MarshalMailbox>,
}

pub fn finalized_from(b: &CBlock, certificate: Option<Vec<u8>>) -> Finalized {
    Finalized {
        height: b.height,
        hash: hex::encode(b.digest.0),
        parent: hex::encode(b.parent.0),
        timestamp_ms: b.timestamp_ms,
        view: b.view,
        parent_view: b.parent_view,
        version: b.version,
        signal: b.signal,
        proposer: b.proposer.clone(),
        txs: b.parse_txs().into_iter().flatten().collect(),
        cblock: b.encode().to_vec(),
        certificate,
    }
}

impl Reporter for Executor {
    type Activity = Update<CBlock>;
    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        use commonware_utils::Acknowledgement as _;
        match activity {
            Update::Tip(_, height, _) => {
                self.chain.lock().set_finalized_tip(height.get());
            }
            Update::Block(block, ack) => {
                if block.height <= self.chain.lock().height() {
                    ack.acknowledge(); // redelivery after restart: already executed
                } else {
                    let cert = self.certs.certs.lock().remove(&block.digest);
                    // Holds the chain lock only for the in-memory parts, not the fsyncs.
                    let executed = Chain::execute_shared(&self.chain, finalized_from(&block, cert));
                    let c = self.chain.lock();
                    match executed {
                        Ok(()) => {
                            debug!(height = block.height, txs = c.last_block().map(|b| b.txs.len()).unwrap_or(0), "executed");
                            if block.height % 100 == 0 && block.height > MARSHAL_RETAIN {
                                if let Some(m) = &self.marshal {
                                    m.prune(Height::new(block.height - MARSHAL_RETAIN));
                                }
                            }
                            if c.state.params.is_boundary(block.height) {
                                let next = c.state.params.epoch_of(block.height) + 1;
                                info!(epoch = next, validators = c.state.active_sets.get(&next).map(|s| s.len()).unwrap_or(0), "epoch boundary");
                                let _ = self.epochs.unbounded_send(next);
                            }
                            ack.acknowledge();
                        }
                        // Not acknowledging makes marshal stop: better than diverging silently. A
                        // halt (upgrade.rs) stops the same way, on purpose.
                        Err(e) => match c.halt.signal.get() {
                            Some(halt) => warn!(height = block.height, "{halt}"),
                            None => error!(error = %e, height = block.height, "failed to execute finalized block"),
                        },
                    }
                }
            }
        }
        commonware_actor::Feedback::Ok
    }
}

// --------------------------------------------------------------------------------- epochs

/// Validator set (and signing scheme) per epoch, read from executed state.
#[derive(Clone)]
pub struct EpochProvider {
    chain: Shared,
    key: Option<PrivateKey>,
    namespace: Vec<u8>,
    cache: Arc<Mutex<HashMap<u64, Arc<MlDsaScheme>>>>,
    /// Every signature our schemes make passes it first.
    protection: Option<Arc<crate::protection::Protection>>,
}

impl EpochProvider {
    pub fn new(chain: Shared, key: Option<PrivateKey>) -> Self {
        let namespace = namespace(&chain.lock().genesis.chain_id);
        Self { chain, key, namespace, cache: Default::default(), protection: None }
    }

    /// Signs only what `protection` allows (validators).
    pub fn with_protection(mut self, protection: Option<Arc<crate::protection::Protection>>) -> Self {
        self.protection = protection;
        self
    }

    pub fn participants(&self, epoch: u64) -> Option<Set<PublicKey>> {
        let c = self.chain.lock();
        keys_of(c.state.active_set(epoch)?.iter().map(|v| v.consensus_key.as_str()))
    }

    /// Approved (and active) validators: allowed to connect as secondary peers, so an approved
    /// validator that didn't fit under the cap can still sync. Pending registrations can't
    /// connect (anyone can register, so they'd let strangers in).
    pub fn candidates(&self) -> Set<PublicKey> {
        use crate::chain::ValidatorStatus::{Active, Approved};
        let c = self.chain.lock();
        let keys: Vec<PublicKey> = c.state.validators.values().filter(|v| matches!(v.status, Approved | Active)).filter_map(|v| PublicKey::from_bytes(&hex::decode(&v.consensus_key).ok()?)).collect();
        Set::from_iter_dedup(keys)
    }

    pub fn epoch_scheme(&self, epoch: u64) -> Option<Arc<MlDsaScheme>> {
        if let Some(s) = self.cache.lock().get(&epoch) {
            return Some(s.clone());
        }
        let parts = self.participants(epoch)?;
        let scheme = match &self.key {
            Some(k) if parts.position(&k.public_key()).is_some() => MlDsaScheme::signer(&self.namespace, parts, k.clone())?.with_protection(self.protection.clone()),
            _ => MlDsaScheme::verifier(&self.namespace, parts),
        };
        let scheme = Arc::new(scheme);
        let mut cache = self.cache.lock();
        cache.insert(epoch, scheme.clone());
        if cache.len() > 16 {
            let oldest = *cache.keys().min().unwrap();
            cache.remove(&oldest);
        }
        Some(scheme)
    }
}

pub fn keys_of<'a>(hex_keys: impl Iterator<Item = &'a str>) -> Option<Set<PublicKey>> {
    let keys: Option<Vec<PublicKey>> = hex_keys.map(|k| PublicKey::from_bytes(&hex::decode(k).ok()?)).collect();
    Set::try_from(keys?).ok()
}

impl certificate::Provider for EpochProvider {
    type Scope = Epoch;
    type Scheme = MlDsaScheme;
    fn scoped(&self, epoch: Epoch) -> Option<Scoped<MlDsaScheme>> {
        let s = self.epoch_scheme(epoch.get())?;
        Some(if s.me().is_some() { Scoped::scheme(s) } else { Scoped::verifier(s) })
    }
}

// ----------------------------------------------------------------------------------- wiring

pub struct ValidatorConfig {
    pub chain: Shared,
    pub key: PrivateKey,
    pub genesis: CBlock,
    pub block_time_ms: u64,
    pub epoch_blocks: u64,
    /// Prefix for all on-disk partitions (lets several validators share one runtime in tests).
    pub partition: String,
    /// Slashing protection for this key (`protection.rs`); every vote passes it.
    pub protection: Option<Arc<crate::protection::Protection>>,
}

pub type Channel<S, R> = (S, R);

/// Starts marshal, the epoch orchestrator (one simplex engine per epoch) and mempool gossip for
/// one node over already-registered p2p channels (`CHANNELS` order). The node votes in epochs
/// whose set contains its key and observes the others. Generic over runtime and network so the
/// deterministic simulator and the real tokio + authenticated-discovery stack share this code.
pub async fn start_validator<E, Pm, Bl, S, R>(context: E, cfg: ValidatorConfig, peers: Pm, blocker: Bl, mut channels: Vec<Channel<S, R>>) -> (Handle<()>, MarshalMailbox)
where
    E: BufferPooler + Clock + CryptoRng + Spawner + Storage + Metrics,
    Pm: Provider<PublicKey = PublicKey> + Manager<PublicKey = PublicKey> + Clone,
    Bl: Blocker<PublicKey = PublicKey> + Clone,
    S: Sender<PublicKey = PublicKey>,
    R: Receiver<PublicKey = PublicKey>,
{
    assert_eq!(channels.len(), CHANNELS.len());
    let mempool_net = channels.pop().unwrap();
    let blocks_net = channels.pop().unwrap();
    let backfill_net = channels.pop().unwrap();
    let resolver_net = channels.pop().unwrap();
    let certs_net = channels.pop().unwrap();
    let votes_net = channels.pop().unwrap();

    let me = cfg.key.public_key();
    let protection = cfg.protection.clone();
    let provider = EpochProvider::new(cfg.chain.clone(), Some(cfg.key.clone())).with_protection(protection.clone());
    let page_cache = CacheRef::from_pooler(&context, NonZeroU16::new(PAGE_SIZE).unwrap(), NonZeroUsize::new(PAGE_CACHE_PAGES).unwrap());
    let prefix = cfg.partition.clone();
    let chain = cfg.chain.clone();
    let block_time_ms = cfg.block_time_ms;
    let genesis = cfg.genesis.clone();
    let epoch_len = NonZeroU64::new(cfg.epoch_blocks.max(2)).unwrap();

    let (marshal_tx, marshal_rx) = futures::channel::oneshot::channel();
    let handle = context.child("validator").spawn(move |context| async move {
        let resolver = marshal_resolver::init(
            context.child("marshal_resolver"),
            marshal_resolver::Config {
                public_key: me.clone(),
                peer_provider: peers.clone(),
                blocker: blocker.clone(),
                mailbox_size: NZUsize!(1024),
                timeout: Duration::from_secs(2),
                fetch_retry_timeout: Duration::from_millis(200),
                priority_requests: false,
                priority_responses: false,
            },
            backfill_net,
        );
        let (broadcast_engine, buffer) = buffered::Engine::new(
            context.child("broadcast"),
            buffered::Config { public_key: me.clone(), mailbox_size: NZUsize!(1024), deque_size: 16, priority: false, codec_config: (), peer_provider: peers.clone() },
        );
        broadcast_engine.start(blocks_net);

        let finalizations = prunable::Archive::init(context.child("finalizations"), archive_config(&prefix, "finalizations", page_cache.clone(), 1024usize)).await.expect("finalizations archive");
        let blocks = prunable::Archive::init(context.child("blocks"), archive_config(&prefix, "blocks", page_cache.clone(), ())).await.expect("blocks archive");
        let (marshal_actor, marshal, floor) = MarshalActor::init(
            context.child("marshal"),
            finalizations,
            blocks,
            marshal::Config {
                provider: provider.clone(),
                epocher: FixedEpocher::new(epoch_len),
                start: marshal::Start::Genesis(genesis.clone()),
                partition_prefix: format!("{prefix}-marshal"),
                mailbox_size: NZUsize!(1024),
                view_retention: ViewDelta::new(10),
                prunable_items_per_section: NZU64!(64),
                page_cache: page_cache.clone(),
                replay_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
                key_write_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
                value_write_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
                block_codec_config: (),
                max_repair: NZUsize!(64),
                max_pending_acks: NZUsize!(1),
                strategy: Sequential,
            },
        )
        .await;
        let processed = floor.height().map(|h| h.get()).unwrap_or(0);
        let ours = chain.lock().height();
        if processed > ours {
            error!(marshal = processed, ours, "consensus storage is ahead of the block logs; delete the `consensus` dir to resync");
        } else if ours > processed {
            // We executed past marshal (HTTP catch-up): anchor marshal at our latest certified
            // block so it doesn't try to backfill what peers may have pruned.
            let c = chain.lock();
            let anchor = (ours.saturating_sub(MARSHAL_RETAIN)..=ours).rev().find_map(|h| c.cert(h).map(|b| (h, b)));
            drop(c);
            match anchor.and_then(|(h, b)| Finalization::<MlDsaScheme, Digest>::decode_cfg(b.as_slice(), &1024usize).ok().map(|f| (h, f))) {
                Some((h, f)) => {
                    info!(height = h, "anchoring marshal at our certified tip");
                    marshal.set_floor(f);
                }
                None => warn!(ours, processed, "no certificate near our tip; marshal will backfill from peers"),
            }
        }
        let _ = marshal_tx.send(marshal.clone());

        let certs = CertCapture::default();
        let (epoch_tx, mut epoch_rx) = futures::channel::mpsc::unbounded::<u64>();
        let executor = Executor { chain: chain.clone(), certs: certs.clone(), epochs: epoch_tx, marshal: Some(marshal.clone()) };
        marshal_actor.start(executor, buffer, resolver);

        // Mempool gossip (wire-form txs): push every locally accepted tx to all peers; accept theirs.
        let (gossip_tx, mut gossip_rx) = futures::channel::mpsc::unbounded::<Tx>();
        chain.lock().gossip = Some(gossip_tx);
        let (mut mp_sender, mut mp_receiver) = mempool_net;
        context.child("mempool_out").spawn(move |_| async move {
            while let Some(tx) = gossip_rx.next().await {
                let _ = mp_sender.send(Recipients::All, tx.to_bytes(), false);
            }
        });
        let gossip_chain = chain.clone();
        context.child("mempool_in").spawn(move |_| async move {
            loop {
                let Ok((from, msg)) = mp_receiver.recv().await else { break };
                let raw: Vec<u8> = msg.as_ref().to_vec();
                if raw.len() > MAX_TX_BYTES {
                    continue;
                }
                let Ok(tx) = Tx::from_bytes(&raw) else {
                    debug!(?from, "unparseable gossiped tx");
                    continue;
                };
                // Verify format + ML-DSA signature before taking the chain lock.
                let (chain_id, genesis_hash, prefix) = {
                    let c = gossip_chain.lock();
                    (c.genesis.chain_id.clone(), c.genesis_hash.clone(), c.genesis.address_prefix.clone())
                };
                if let Err(e) = check_tx_stateless(&tx, &chain_id, &genesis_hash, &prefix) {
                    debug!(?from, error = %e, "dropping invalid gossiped tx");
                    continue;
                }
                let mut c = gossip_chain.lock();
                // Don't echo gossip back out; the originator already sent it to everyone.
                let g = c.gossip.take();
                if let Err(e) = c.submit_tx(tx) {
                    debug!(error = %e, "dropping gossiped tx");
                }
                c.gossip = g;
            }
        });

        // Epoch orchestrator: one simplex engine per epoch, consensus channels muxed by epoch.
        let (mux, mut votes, mut votes_backup) = Muxer::builder(context.child("vote_mux"), votes_net.0, votes_net.1, 1024).with_backup().build();
        mux.start();
        let (mux, mut certs_mux, mut certs_backup) = Muxer::builder(context.child("cert_mux"), certs_net.0, certs_net.1, 1024).with_backup().build();
        mux.start();
        let (mux, mut resolver_mux) = Muxer::new(context.child("resolver_mux"), resolver_net.0, resolver_net.1, 1024);
        mux.start();

        // Slashing protection's clock (its startup rule waits on the network for a while), and the
        // newest epoch the network has finalized a block in (no votes in epochs it has left).
        if let Some(p) = protection.clone() {
            let chain = chain.clone();
            context.child("slashing_protection").spawn(move |ctx| async move {
                loop {
                    let finalized = {
                        let c = chain.lock();
                        c.state.params.epoch_of(c.finalized_tip.max(1))
                    };
                    p.note_finalized_epoch(finalized);
                    p.tick(ms(ctx.current()));
                    ctx.sleep(Duration::from_millis(250)).await;
                }
            });
        }

        let address_prefix = chain.lock().genesis.address_prefix.clone();
        let app = Inline::new(context.child("inline"), App { chain: chain.clone(), block_time_ms, address_prefix }, marshal.clone(), FixedEpocher::new(epoch_len));
        let bt = Duration::from_millis(block_time_ms);
        let (mut epoch, height) = {
            let c = chain.lock();
            (c.state.params.epoch_of(c.height() + 1), c.height())
        };
        let _ = height;
        let mut tracked = None;
        let mut active: Option<Handle<()>> = None;
        // Simplex journals below this epoch are gone (a few more are swept after a restart).
        let mut journals_from = epoch.saturating_sub(JOURNAL_EPOCHS_KEPT + 16);
        loop {
            // (Re)enter `epoch`.
            if let Some(h) = active.take() {
                h.abort();
            }
            let Some(scheme) = provider.epoch_scheme(epoch) else {
                error!(epoch, "no validator set known for epoch");
                return;
            };
            if tracked.is_none_or(|t| t < epoch) {
                let primary = scheme.participants().clone();
                let secondary = provider.candidates();
                let mut m = peers.clone();
                let _ = m.track(epoch, TrackedPeers::new(primary, secondary));
                tracked = Some(epoch);
            }
            let floor_digest = if epoch == 0 {
                genesis.digest()
            } else {
                let c = chain.lock();
                let h = c.state.params.last_of(epoch - 1);
                Digest(hex_32(&c.hash_at(h).unwrap_or_default()))
            };
            let voting = scheme.me().is_some();
            if let Some(p) = &protection {
                p.enter_epoch(epoch, scheme.me().map(|i| usize::from(i) as u32), scheme.participants().len(), ms(context.current()));
            }
            let engine = simplex::Engine::new(
                context.child("simplex").with_attribute("epoch", epoch),
                simplex::config::Config {
                    scheme: scheme.as_ref().clone(),
                    elector: RoundRobin::<Blake3>::default(),
                    blocker: blocker.clone(),
                    automaton: app.clone(),
                    relay: app.clone(),
                    reporter: Reporters::from((marshal.clone(), Reporters::<_, _, crate::protection::Observer>::from((certs.clone(), protection.clone().map(crate::protection::Observer))))),
                    strategy: Sequential,
                    partition: simplex_partition(&prefix, epoch),
                    mailbox_size: NZUsize!(1024),
                    epoch: Epoch::new(epoch),
                    floor: simplex::config::Floor::Genesis(floor_digest),
                    replay_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
                    write_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
                    page_cache: page_cache.clone(),
                    leader_timeout: bt + Duration::from_secs(2),
                    certification_timeout: bt + Duration::from_secs(3),
                    timeout_retry: Duration::from_secs(5),
                    fetch_timeout: Duration::from_secs(2),
                    view_retention: ViewDelta::new(10),
                    skip: SkipPolicy::Enabled { timeout: bt * 10 + Duration::from_secs(10), budget: SkipBudget::Participants },
                    forward: ForwardPolicy::Disabled,
                    track_historical_votes: false,
                },
            );
            let (Ok(v), Ok(c), Ok(r)) = (votes.register(epoch).await, certs_mux.register(epoch).await, resolver_mux.register(epoch).await) else {
                error!(epoch, "consensus mux closed");
                return;
            };
            active = Some(engine.start(v, c, r));
            info!(epoch, voting, validators = scheme.participants().len(), "entered epoch");
            // Older epochs' simplex journals are never read again (OPS-6).
            let keep_from = epoch.saturating_sub(JOURNAL_EPOCHS_KEPT);
            for old in journals_from..keep_from {
                let _ = context.remove(&simplex_partition(&prefix, old), None).await;
            }
            journals_from = journals_from.max(keep_from);

            // Wait for the boundary of this epoch (or hints that we're behind).
            loop {
                commonware_macros::select! {
                    next = epoch_rx.next() => {
                        let Some(mut next) = next else { return };
                        while let Ok(Some(n)) = epoch_rx.try_recv().map(Some) {
                            next = next.max(n);
                        }
                        if next > epoch {
                            epoch = next;
                            break;
                        }
                    },
                    msg = votes_backup.recv() => {
                        let Some((their, (from, _))) = msg else { return };
                        hint_behind(&marshal, &chain, epoch, their, from);
                    },
                    msg = certs_backup.recv() => {
                        let Some((their, (from, _))) = msg else { return };
                        hint_behind(&marshal, &chain, epoch, their, from);
                    },
                }
            }
        }
    });
    let mailbox = marshal_rx.await.expect("marshal started");
    (handle, mailbox)
}

/// A peer is voting in a later epoch: we're behind. Ask marshal for the finalization of our
/// epoch's last block (from that peer); backfill + execution then drive the transition.
fn hint_behind(marshal: &MarshalMailbox, chain: &Shared, ours: u64, theirs: u64, from: PublicKey) {
    if theirs <= ours {
        return;
    }
    let last = chain.lock().state.params.last_of(ours);
    debug!(ours, theirs, last, "peer is in a later epoch; fetching boundary");
    marshal.hint_finalized(Height::new(last), commonware_utils::vec::NonEmptyVec::new(from));
}

/// Simplex journals kept: the current epoch's and the two before it (OPS-6). Older ones are
/// deleted when an epoch starts, and at startup (`node::prune_epoch_journals`).
pub const JOURNAL_EPOCHS_KEPT: u64 = 2;

/// The storage partition of `epoch`'s simplex journal.
pub fn simplex_partition(prefix: &str, epoch: u64) -> String {
    format!("{prefix}-simplex-{epoch}")
}

/// Finalized blocks/certificates marshal keeps; older ones are pruned (our own `cblocks/` log and
/// checkpoint certificates keep serving history over HTTP).
pub const MARSHAL_RETAIN: u64 = 2_000;
/// Bumped when the on-disk storage layout changes (consensus journals, or the home's own files:
/// v4 is the segmented block logs and the state database, see `store.rs`).
pub const STORAGE_VERSION: &str = "prunable-v4";

fn archive_config<C>(prefix: &str, name: &str, page_cache: CacheRef, codec_config: C) -> prunable::Config<TwoCap, C> {
    prunable::Config {
        translator: TwoCap,
        metadata_partition: format!("{prefix}-{name}-metadata"),
        key_partition: format!("{prefix}-{name}-key"),
        key_page_cache: page_cache,
        value_partition: format!("{prefix}-{name}-value"),
        compression: Some(3),
        codec_config,
        items_per_section: NZU64!(256),
        key_write_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
        value_write_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
        replay_buffer: NonZeroUsize::new(IO_BUFFER).unwrap(),
    }
}

/// Per-peer message rate per channel. Commonware preallocates each channel's inbound mailbox
/// as (retained peers × burst) slots, so these (with `MAX_PEERS_PER_SET` and
/// `TRACKED_PEER_SETS`) bound p2p memory. 10,000/s × 1,027 peers was a ~470 MiB allocation per
/// channel — enough to abort a 512 MB VM at startup.
pub fn quota(channel: u64) -> commonware_runtime::Quota {
    commonware_runtime::Quota::per_second(NonZeroU32::new(rate(channel)).unwrap())
}

fn rate(channel: u64) -> u32 {
    match channel {
        CH_VOTES | CH_CERTS | CH_RESOLVER => 50,
        CH_BACKFILL => 100,
        CH_BLOCKS => 20,
        _ => 200, // mempool gossip
    }
}

/// Peers per tracked set (active + approved validators, with headroom over the 32 cap).
pub const MAX_PEERS_PER_SET: usize = 64;
/// Peer sets the p2p layer keeps (current and previous epoch).
pub const TRACKED_PEER_SETS: usize = 2;
/// Bytes per preallocated inbound mailbox slot (observed: 48 on 64-bit targets).
pub const MAILBOX_SLOT_BYTES: usize = 48;

/// Upper bound on preallocated p2p inbound mailbox memory for `bootstrappers` extra peers.
pub fn p2p_mailbox_bytes(bootstrappers: usize) -> usize {
    let retained = MAX_PEERS_PER_SET * TRACKED_PEER_SETS + bootstrappers;
    CHANNELS.iter().map(|c| retained * rate(*c) as usize * MAILBOX_SLOT_BYTES).sum()
}

/// Logs what the node is about to allocate up front.
pub fn log_memory_budget(bootstrappers: usize) {
    info!(
        p2p_mailboxes_mib = p2p_mailbox_bytes(bootstrappers) / (1 << 20),
        page_cache_mib = (PAGE_SIZE as usize * PAGE_CACHE_PAGES) / (1 << 20),
        journal_buffers_mib = (IO_BUFFER * 12) / (1 << 20),
        max_message_mib = MAX_MESSAGE_SIZE as usize / (1 << 20),
        "memory budget"
    );
}

pub const PAGE_SIZE: u16 = 4096;
pub const PAGE_CACHE_PAGES: usize = 1024;
/// Replay/write buffer per journal (marshal + simplex archives).
pub const IO_BUFFER: usize = 256 * 1024;

#[cfg(test)]
mod memory_tests {
    use super::*;

    #[test]
    fn p2p_preallocation_fits_a_small_vm() {
        // 32 validators bootstrapping from each other.
        let bytes = p2p_mailbox_bytes(64);
        assert!(bytes < 32 << 20, "p2p mailboxes would preallocate {} MiB", bytes >> 20);
        // The old settings (10,000/s, 256 peers × 4 sets) were ~470 MiB per channel.
        assert!(1027 * 10_000 * MAILBOX_SLOT_BYTES > 400 << 20);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::SummonOpts;
    use commonware_codec::DecodeExt as _;
    use commonware_consensus::types::{Round, View};
    use commonware_runtime::{Runner as _, Supervisor as _, deterministic};

    #[test]
    fn cblock_v2_encodes_decodes_and_commits_to_every_field() {
        let b = CBlock::new(Digest([1; 32]), 5, 9, 7, 1_000, "boo1proposer".into(), 4, [2; 32], vec![vec![1, 2, 3], vec![]]);
        let bytes = b.encode();
        assert_eq!(bytes.len(), b.encode_size());
        let back = CBlock::decode(bytes.as_ref()).unwrap();
        assert_eq!(back, b);
        assert_eq!((back.version, back.view, back.parent_view, back.signal), (PROTOCOL_VERSION, 9, 7, SUPPORTED_PROTOCOL_VERSION));
        assert_eq!(b.digest(), Blake3::hash(&[b"BLOCKULA_CBLOCK_V2", bytes.as_ref()]));
        assert_eq!(hex::encode(b.digest_bytes()), "ca6c7aad4e59ed281b8cca54e99c172eb72ebdf3f5ce4e51c5c83cd3eecb23e3");
        // Every header field is under the digest.
        let variants = [
            CBlock { version: 2, ..b.clone() },
            CBlock { view: 10, ..b.clone() },
            CBlock { parent_view: 8, ..b.clone() },
            CBlock { signal: 2, ..b.clone() },
            CBlock { exec_height: 3, ..b.clone() },
        ];
        for v in variants {
            assert_ne!(v.sealed().digest(), b.digest());
        }
        // Decoding bounds and exactness.
        let mut trailing = bytes.to_vec();
        trailing.push(0);
        assert!(CBlock::decode(trailing.as_slice()).is_err());
        assert!(CBlock::decode(&bytes[..bytes.len() - 1]).is_err());
        let many = CBlock::new(Digest([1; 32]), 5, 9, 7, 1_000, String::new(), 4, [2; 32], vec![vec![]; MAX_BLOCK_TXS + 1]);
        assert!(CBlock::decode(many.encode().as_ref()).is_err());
        let big = CBlock::new(Digest([1; 32]), 5, 9, 7, 1_000, String::new(), 4, [2; 32], vec![vec![0; MAX_TX_BYTES + 1]]);
        assert!(CBlock::decode(big.encode().as_ref()).is_err());
        // Txs that aren't canonical wire bytes don't parse.
        assert_eq!(b.parse_txs(), vec![None, None]);
    }

    /// A verifier that hasn't executed the height a block cites keeps the verdict pending (it
    /// used to vote no after ~3 block times at checkpoints), then checks the root once it has.
    /// Mismatched views, versions and junk txs are still rejected at once.
    #[test]
    fn lagging_verifier_waits_instead_of_rejecting() {
        let root_dir = std::env::temp_dir().join(format!("blockula-lag-{}-{}", crate::chain::now_ms(), std::process::id()));
        let ahead = root_dir.join("ahead");
        Chain::summon(&ahead, &SummonOpts::new("lag", 1)).unwrap();
        let behind = root_dir.join("behind");
        std::fs::create_dir_all(&behind).unwrap();
        for f in ["genesis.json", "validator.key"] {
            std::fs::copy(ahead.join(f), behind.join(f)).unwrap();
        }
        // The proposer's view of the chain: executed through 103. The verifier: through 99.
        let mut a = Chain::open(&ahead).unwrap();
        for _ in 0..103 {
            a.produce_block_with(vec![]).unwrap();
        }
        let mut b = Chain::open(&behind).unwrap();
        for _ in 0..99 {
            b.produce_block_with(vec![]).unwrap();
        }
        let (root100, root101) = (hex_32(&a.root_at(100).unwrap()), hex_32(&a.root_at(101).unwrap()));
        drop(a);
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(ahead.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let genesis_hash = b.genesis_hash.clone();
        let chain: Shared = Arc::new(Mutex::new(b));
        deterministic::Runner::timed(Duration::from_secs(120)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            let parent_at = |height: u64| CBlock::new(Digest([height as u8; 32]), height, 6, 5, 0, String::new(), height - 4, [0; 32], vec![]);
            let child = |parent: &CBlock, view: u64, exec_height: u64, root: [u8; 32], txs: Vec<Vec<u8>>| CBlock::new(parent.digest(), parent.height + 1, view, 6, 1_000, leader.address("boo"), exec_height, root, txs);
            let cctx = |parent: &CBlock| SimplexContext { round: Round::new(Epoch::zero(), View::new(7)), leader: leader.clone(), parent: (View::new(6), parent.digest()) };
            let mut verify = async |block: CBlock, parent: &CBlock| {
                let cctx = cctx(parent);
                let ancestry = marshal::ancestry::from_iter([Arc::new(block), Arc::new(parent.clone())]);
                Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry).await
            };
            let p103 = parent_at(103);
            // Permanent failures don't wait.
            assert!(!verify(child(&p103, 8, 100, root100, vec![]), &p103).await, "view isn't the round's");
            assert!(!verify(CBlock { version: 2, ..child(&p103, 7, 100, root100, vec![]) }.sealed(), &p103).await, "unknown version");
            assert!(!verify(child(&p103, 7, 100, root100, vec![b"junk".to_vec()]), &p103).await, "junk tx bytes");
            let user = crate::crypto::Keys::from_seed([5; 32]);
            let tx = |valid_until: u64| {
                let mut t = Tx { chain_id: "lag".into(), genesis_hash: genesis_hash.clone(), from: user.address(), nonce: 0, fee: 0, tip: 0, valid_until, action: crate::types::Action::Transfer { to: "xyz.boo".into(), amount: 1 }, memo: String::new(), public_key: hex::encode(user.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
                t.fee = t.charge().unwrap();
                t.sign(&user)
            };
            assert!(!verify(child(&p103, 7, 100, root100, vec![tx(50).to_bytes()]), &p103).await, "expired tx");
            let mut padded = tx(110).to_bytes();
            padded.push(0);
            assert!(!verify(child(&p103, 7, 100, root100, vec![padded]), &p103).await, "non-canonical tx bytes");
            assert_eq!(context.current(), std::time::UNIX_EPOCH, "rejected without waiting");

            // The verifier catches up 10 s from now (well past 3 block times), then 10 s later.
            let catch_up = chain.clone();
            context.child("catch_up").spawn(move |ctx| async move {
                for _ in 0..2 {
                    ctx.sleep(Duration::from_secs(10)).await;
                    catch_up.lock().produce_block_with(vec![]).unwrap();
                }
            });
            // Block 104 must certify checkpoint 100, which we haven't executed: wait, then accept.
            assert!(verify(child(&p103, 7, 100, root100, vec![]), &p103).await);
            assert!(context.current() >= std::time::UNIX_EPOCH + Duration::from_secs(10), "waited for execution");
            // A block citing 101 (not executed yet) with the wrong root: rejected only once we
            // can check it (it used to be accepted unchecked).
            let p102 = parent_at(102);
            assert!(!verify(child(&p102, 7, 101, [9; 32], vec![]), &p102).await);
            assert!(context.current() >= std::time::UNIX_EPOCH + Duration::from_secs(20));
            assert!(verify(child(&p102, 7, 101, root101, vec![tx(110).to_bytes()]), &p102).await);
        });
        std::fs::remove_dir_all(root_dir).ok();
    }

    /// Blocks must carry the protocol version in force at their height (3.9). A node votes on
    /// nothing it couldn't execute: a version its binary doesn't run, or a height past
    /// `--halt-height`. It proposes nothing there either, and signals its own supported version.
    #[test]
    fn verify_checks_the_version_in_force_and_abstains_past_a_halt() {
        let home = std::env::temp_dir().join(format!("blockula-versions-{}-{}", crate::chain::now_ms(), std::process::id()));
        Chain::summon(&home, &SummonOpts::new("versions", 1)).unwrap();
        let mut c = Chain::open(&home).unwrap();
        let v2 = SUPPORTED_PROTOCOL_VERSION + 1;
        c.state.upgrade.schedule = vec![crate::upgrade::ScheduledUpgrade { version: v2, activation_height: 70 }];
        for _ in 0..10 {
            c.produce_block_with(vec![]).unwrap();
        }
        let root10 = hex_32(&c.root_at(10).unwrap());
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let chain: Shared = Arc::new(Mutex::new(c));
        deterministic::Runner::timed(Duration::from_secs(120)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            let parent_at = |height: u64| CBlock::new(Digest([height as u8; 32]), height, 6, 5, 0, String::new(), 9, [0; 32], vec![]);
            let child = |parent: &CBlock, version: u32| CBlock::new(parent.digest(), parent.height + 1, 7, 6, 1_000, leader.address("boo"), 10, root10, vec![]).with_versions(version, 1);
            let cctx = |parent: &CBlock| SimplexContext { round: Round::new(Epoch::zero(), View::new(7)), leader: leader.clone(), parent: (View::new(6), parent.digest()) };
            // `Some(verdict)`, or `None` if it's still pending after 10 s.
            let mut verify = async |block: CBlock, parent: &CBlock| {
                let ancestry = marshal::ancestry::from_iter([Arc::new(block), Arc::new(parent.clone())]);
                let verdict = Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx(parent)), ancestry);
                commonware_macros::select! {
                    v = verdict => Some(v),
                    _ = context.sleep(Duration::from_secs(10)) => None,
                }
            };
            let (p20, p73) = (parent_at(20), parent_at(73));
            assert_eq!(verify(child(&p20, 1), &p20).await, Some(true), "v1 before the activation height");
            assert_eq!(verify(child(&p20, 2), &p20).await, Some(false), "v2 too early");
            // This binary runs v1 only: from the activation height on, no vote either way.
            assert_eq!(verify(child(&p73, 2), &p73).await, None);
            assert_eq!(verify(child(&p73, 1), &p73).await, None);
            crate::upgrade::configure(&mut chain.lock(), v2, None);
            assert_eq!(verify(child(&p73, 1), &p73).await, Some(false), "v1 after the activation height");
            assert_eq!(verify(child(&p73, 2), &p73).await, Some(true));
            // A block 64+ heights past what we executed waits until the version there is certain.
            let p80 = parent_at(80);
            assert_eq!(verify(child(&p80, 2), &p80).await, None);
            // --halt-height 50: nothing past it gets a vote.
            crate::upgrade::configure(&mut chain.lock(), v2, Some(50));
            assert_eq!(verify(child(&p20, 1), &p20).await, Some(true));
            assert_eq!(verify(child(&p73, 2), &p73).await, None);
            let c = chain.lock();
            assert!(c.halt.allows(&c.state, 50) && !c.halt.allows(&c.state, 51));
        });
        std::fs::remove_dir_all(home).ok();
    }
}
