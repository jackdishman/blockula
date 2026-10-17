//! Multi-validator BFT: Commonware simplex orders blocks, marshal disseminates and backfills them,
//! and finalized blocks are executed in order by `Chain::execute` (deferred execution).
//!
//! Block flow:
//! * **propose** (leader): wait until `parent.timestamp + block_time`, pick mempool txs (highest
//!   tip per byte first, up to the height's `max_block_bytes`) that apply on top of our executed
//!   state plus any not-yet-executed ancestors, and attach the latest state root we've executed
//!   (`exec_height`, `exec_root`). A leader more than `econ::FEE_LAG` blocks behind can't price
//!   txs (the base fee isn't fixed in its state yet) and proposes an empty block.
//! * **verify** (everyone, before voting): view and parent view against the consensus context,
//!   the protocol version in force at the height (`upgrade.rs`; no vote at all past a halt or for
//!   a version this binary can't run), structure, size limits, timestamps, proposer == leader,
//!   stateless tx checks (canonical wire bytes, chain id and genesis hash, address prefix,
//!   validity window, ML-DSA signature, the lowest possible fee), and the attached state root
//!   (waiting until we've executed that height). A block with txs must cite a height within
//!   `econ::FEE_LAG` of its own, so once we've executed it we check the block's size and every
//!   tx's fee against the height's exact base fee. Only permanently invalid blocks get `false`;
//!   anything that can still change keeps the verdict pending.
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
        scheme::reporter::AttributableReporter,
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
/// A proposal's `exec_height` may trail the verifier's executed height by at most this much
/// (so a proposer can't dodge the state-root check by citing an ancient height).
pub const MAX_EXEC_LAG: u64 = 16;
/// The codec's bound on a block's tx bytes. The limit blocks are held to is the governance
/// parameter `max_block_bytes` (2 MiB at genesis), fixed per height like the base fee (econ.rs).
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

/// The consensus signing domain of one chain *instance*: its id and its genesis hash (hex), as
/// `Tx::signing_bytes` binds txs (audit #7). Every vote and certificate is signed under it, so a
/// relaunch that keeps the chain id (same validator keys, another genesis) shares no valid
/// signature with the old chain: its certificates can't be replayed here, or paired with this
/// chain's votes into equivocation evidence.
pub fn namespace(chain_id: &str, genesis_hash: &str) -> Vec<u8> {
    format!("blockula/{chain_id}/{genesis_hash}").into_bytes()
}

/// [`namespace`] of `g`'s chain (computes its genesis hash).
pub fn genesis_namespace(g: &crate::chain::Genesis) -> Vec<u8> {
    namespace(&g.chain_id, &crate::checkpoints::genesis_hash(g))
}

/// The p2p handshake's namespace: per chain id (`docs/PQ-TRANSPORT.md`), unchanged by the
/// consensus domain above.
pub fn p2p_namespace(chain_id: &str) -> Vec<u8> {
    format!("blockula/{chain_id}/p2p").into_bytes()
}

/// Most unexecuted ancestors `propose` and `verify` replay before building on a block or judging
/// it: beyond that a proposer proposes an empty block and a verifier abstains.
pub const MAX_REPLAY: usize = 64;

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
            // A tx is bounded by its kind (`Tx::from_bytes`); ghost txs are the largest.
            if len > crate::types::MAX_GHOST_TX_BYTES || total > MAX_BLOCK_BYTES {
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
/// is named by its id and its genesis hash (the instance, audit #7). A ghost tx's Action proof is
/// one of them (it needs no state); unsigned ghost txs have no signature, their proof authorizes
/// them (ghost.rs).
pub fn check_tx_stateless(tx: &Tx, chain_id: &str, genesis_hash: &str, prefix: &str) -> anyhow::Result<()> {
    check_tx_unproven(tx, chain_id, genesis_hash, prefix)?;
    crate::ghost::verify_proof(tx)
}

/// A tx's signature didn't verify (as opposed to a format error, which a peer on another release
/// could relay in good faith).
#[derive(Debug)]
pub struct BadSignature(anyhow::Error);

impl std::fmt::Display for BadSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for BadSignature {}

/// [`check_tx_stateless`] without the ghost proof: the cheap part, which gossip runs before it
/// spends a proof verification on the tx. A bad signature is a [`BadSignature`].
pub fn check_tx_unproven(tx: &Tx, chain_id: &str, genesis_hash: &str, prefix: &str) -> anyhow::Result<()> {
    tx.check_chain(chain_id, genesis_hash)?;
    tx.check_format()?;
    tx.check_prefix(prefix)?;
    crate::chain::check_fee(tx)?;
    if !tx.is_unsigned() {
        tx.verify_signature().map_err(|e| anyhow::Error::new(BadSignature(e)))?;
    }
    Ok(())
}

// --------------------------------------------------------------------------- mempool gossip

/// Ghost proofs one peer may have verified per second, and at once. Above the chain's own ghost
/// throughput (`ghost_max_block_bytes` holds about seven a block), so honest relays never reach it.
pub const GOSSIP_PROOFS_PER_SEC: u64 = 8;
pub const GOSSIP_PROOF_BURST: u64 = 32;
/// Ghost proof verifications in flight at once, over every peer.
pub const GOSSIP_PROOFS_IN_FLIGHT: usize = 16;
/// How long a peer's mempool gossip is dropped unread after it relayed a tx whose signature or
/// proof doesn't verify. Honest nodes verify every tx before they relay it (HTTP and their own
/// system txs), so that never happens by accident; votes and blocks from the peer are unaffected.
pub const GOSSIP_MUTE_MS: u64 = 5 * 60_000;
/// Gossiped txs (hashes of their wire bytes) remembered as invalid: the same bytes are never
/// verified twice.
const REMEMBERED_REJECTS: usize = 4_096;
/// Peers whose budgets are remembered (only tracked peers connect: far fewer).
const REMEMBERED_PEERS: usize = 1_024;

/// Audit F30: keeps gossiped txs from costing verification out of proportion. The ingest loop
/// drops duplicates (by id, against the mempool and finalized txs) and remembered rejects before
/// any signature or proof work, verifies ghost proofs within a per-peer token bucket (and a cap
/// in flight) off the loop, and mutes a peer that relays a bad signature or proof.
pub struct GossipGate<K> {
    peers: HashMap<K, PeerBudget>,
    rejected: std::collections::HashSet<[u8; 32]>,
    order: std::collections::VecDeque<[u8; 32]>,
    /// Ids of the txs whose proofs are being verified (at most [`GOSSIP_PROOFS_IN_FLIGHT`]): the
    /// same tx from another peer meanwhile is a duplicate too.
    verifying: std::collections::HashSet<String>,
}

#[derive(Clone, Copy, Debug)]
struct PeerBudget {
    /// Proof verifications available, in thousandths.
    milli: u64,
    at_ms: u64,
    muted_until_ms: u64,
}

impl<K> Default for GossipGate<K> {
    fn default() -> Self {
        GossipGate { peers: HashMap::new(), rejected: Default::default(), order: Default::default(), verifying: Default::default() }
    }
}

impl<K: std::hash::Hash + Eq + Clone> GossipGate<K> {
    /// Whether a message is dropped unread: its peer is muted, or these bytes failed before.
    pub fn drops(&self, peer: &K, fingerprint: &[u8; 32], now_ms: u64) -> bool {
        self.peers.get(peer).is_some_and(|b| now_ms < b.muted_until_ms) || self.rejected.contains(fingerprint)
    }

    fn budget(&mut self, peer: &K, now_ms: u64) -> &mut PeerBudget {
        if self.peers.len() >= REMEMBERED_PEERS && !self.peers.contains_key(peer) {
            self.peers.retain(|_, b| now_ms < b.muted_until_ms);
        }
        let b = self.peers.entry(peer.clone()).or_insert(PeerBudget { milli: GOSSIP_PROOF_BURST * 1_000, at_ms: now_ms, muted_until_ms: 0 });
        let refill = now_ms.saturating_sub(b.at_ms).saturating_mul(GOSSIP_PROOFS_PER_SEC);
        b.milli = b.milli.saturating_add(refill).min(GOSSIP_PROOF_BURST * 1_000);
        b.at_ms = b.at_ms.max(now_ms);
        b
    }

    /// Whether the tx `id` is having its proof verified.
    pub fn verifying(&self, id: &str) -> bool {
        self.verifying.contains(id)
    }

    /// Takes one proof verification of the tx `id` from `peer`'s budget (and a slot in flight),
    /// if it has one.
    pub fn take_proof(&mut self, peer: &K, id: &str, now_ms: u64) -> bool {
        if self.verifying.len() >= GOSSIP_PROOFS_IN_FLIGHT || self.verifying.contains(id) {
            return false;
        }
        let b = self.budget(peer, now_ms);
        if b.milli < 1_000 {
            return false;
        }
        b.milli -= 1_000;
        self.verifying.insert(id.to_string());
        true
    }

    /// The proof verification of `id` that [`GossipGate::take_proof`] allowed has finished.
    pub fn proof_done(&mut self, id: &str) {
        self.verifying.remove(id);
    }

    /// These bytes are invalid (remembered); if `mute`, their signature or proof didn't verify,
    /// and `peer` is muted for [`GOSSIP_MUTE_MS`].
    pub fn reject(&mut self, peer: &K, fingerprint: [u8; 32], mute: bool, now_ms: u64) {
        if self.rejected.insert(fingerprint) {
            self.order.push_back(fingerprint);
            while self.order.len() > REMEMBERED_REJECTS {
                if let Some(old) = self.order.pop_front() {
                    self.rejected.remove(&old);
                }
            }
        }
        if mute {
            let b = self.budget(peer, now_ms);
            b.muted_until_ms = now_ms.saturating_add(GOSSIP_MUTE_MS);
            b.milli = 0;
        }
    }
}

/// What the mempool ingest loop does with one gossiped message.
#[derive(Debug)]
pub enum Screened {
    /// Dropped. `verified` says whether it reached the signature check (a duplicate, a muted
    /// peer's message or remembered-invalid bytes never do).
    Dropped { why: &'static str, verified: bool },
    /// Verified: into the mempool.
    Admit(Tx),
    /// Its signature verified; its ghost proof is next (the gate's budget is taken, so
    /// [`GossipGate::proof_done`] must follow): `(tx, id, fingerprint)`.
    Prove(Tx, String, [u8; 32]),
}

/// Screens one gossiped message, cheapest checks first (audit F30): its size (before it's copied
/// or parsed), a muted peer or remembered-invalid bytes, then, once parsed, a tx already in the
/// mempool, finalized or being verified, the fee against the next block's base fee and Ghost Mode
/// being on; only then the ML-DSA signature, and a ghost proof only within the peer's budget.
pub fn screen_gossip<K: std::hash::Hash + Eq + Clone>(gate: &Mutex<GossipGate<K>>, chain: &Shared, from: &K, raw: &[u8], now: u64) -> Screened {
    let drop = |why| Screened::Dropped { why, verified: false };
    // The kind's own bound (`Tx::from_bytes`) applies next; ghost txs are the largest.
    if raw.len() > crate::types::MAX_GHOST_TX_BYTES {
        return drop("too large");
    }
    let fingerprint = crate::crypto::hash(raw);
    if gate.lock().drops(from, &fingerprint, now) {
        return drop("muted peer or known-invalid bytes");
    }
    let Ok(tx) = Tx::from_bytes(raw) else {
        return drop("unparseable");
    };
    let id = tx.id();
    let (chain_id, genesis_hash, prefix, priced, ghost_off, known) = {
        let c = chain.lock();
        let next = c.height() + 1;
        let known = c.in_mempool(&id) || c.has_tx(&id);
        (c.genesis.chain_id.clone(), c.genesis_hash.clone(), c.genesis.address_prefix.clone(), crate::econ::check_fee(&c.state, &tx, next), tx.action.is_ghost() && crate::ghost::check_enabled(&c.state, next).is_err(), known)
    };
    if known || gate.lock().verifying(&id) {
        return drop("already known");
    }
    if ghost_off {
        return drop("Ghost Mode is off");
    }
    if priced.is_err() {
        return drop("below the base fee");
    }
    // Format and the ML-DSA signature: failures are remembered, and a bad signature mutes the peer
    // (honest nodes verify every tx before they send it: HTTP submissions and their own txs, and
    // gossip isn't relayed).
    if let Err(e) = check_tx_unproven(&tx, &chain_id, &genesis_hash, &prefix) {
        let bad_signature = e.is::<BadSignature>();
        gate.lock().reject(from, fingerprint, bad_signature, now);
        return Screened::Dropped { why: if bad_signature { "bad signature" } else { "invalid" }, verified: true };
    }
    if tx.action.ghost_bundle().is_none() {
        return Screened::Admit(tx);
    }
    if !gate.lock().take_proof(from, &id, now) {
        return Screened::Dropped { why: "over the peer's proof budget", verified: true };
    }
    Screened::Prove(tx, id, fingerprint)
}

/// Verifies the ghost proof of a tx [`screen_gossip`] passed on, and admits it, or remembers its
/// bytes and mutes `from` (a proof that doesn't verify never comes from an honest node). Returns
/// whether the proof verified.
pub fn prove_gossiped<K: std::hash::Hash + Eq + Clone>(gate: &Mutex<GossipGate<K>>, chain: &Shared, from: &K, tx: Tx, id: &str, fingerprint: [u8; 32], now: impl FnOnce() -> u64) -> bool {
    let verdict = crate::ghost::verify_proof(&tx);
    let mut g = gate.lock();
    g.proof_done(id);
    match verdict {
        Ok(()) => {
            drop(g);
            admit_gossiped(chain, tx);
            true
        }
        Err(e) => {
            debug!(error = %e, "dropping gossiped ghost tx with a bad proof; muting the peer");
            g.reject(from, fingerprint, true, now());
            false
        }
    }
}

/// Admits a tx a peer gossiped (verified), without echoing it back out: its originator already
/// sent it to everyone.
fn admit_gossiped(chain: &Arc<Mutex<Chain>>, tx: Tx) {
    let mut c = chain.lock();
    let g = c.gossip.take();
    if let Err(e) = c.submit_tx(tx) {
        debug!(error = %e, "dropping gossiped tx");
    }
    c.gossip = g;
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
        // replay them exactly (as verifiers will), so we don't re-propose (or overdraw) their txs
        // and every tx we pick is admittable on the parent's state.
        let executed = self.chain.lock().height();
        let Some(pending) = unexecuted(parent.clone(), &mut ancestry, executed).await else {
            // Far behind (still catching up): propose an empty block rather than guess (but never
            // at a checkpoint-certifying height, which needs the real root).
            if crate::snapshot::certifies(height).is_some() {
                return None;
            }
            let c = self.chain.lock();
            return Some(CBlock::new(parent.digest(), height, view, parent_view, timestamp_ms, proposer, c.height(), hex_32(&c.state.root()), vec![]).with_versions(version, signal));
        };
        let tip = Tip::of(&self.chain.lock());
        let (exec_height, exec_root, chain_id) = (tip.height, hex_32(&tip.state.root()), tip.chain_id.clone());
        // This height's base fee and size limit are fixed once `height − FEE_LAG` has executed
        // (econ.rs). Further behind than that we can't price txs, so the block stays empty
        // (verifiers refuse txs in a block whose cited state can't price them).
        let slot = crate::econ::slot(&tip.state, height);
        let limit = slot.map_or(0, |s| s.max_block_bytes as usize);
        // Ghost txs' own size limits, fixed in the same slot.
        let mut ghost = crate::ghost::BlockBudget::new(slot.as_ref());
        let Some(mut state) = replay(tip, &parent, pending.clone()) else {
            warn!(height, "the parent doesn't extend our finalized chain; not proposing");
            return None;
        };
        let included: std::collections::HashSet<String> = pending.iter().flat_map(|b| b.parse_txs().into_iter().flatten().map(|t| t.id())).collect();
        let txs = {
            let c = self.chain.lock();
            let mut txs = vec![];
            let mut bytes = 0;
            // Senders whose earlier tx was left out: their later nonces can't apply.
            let mut stuck = std::collections::HashSet::new();
            // Validator duties first (the mempool's lane), then by tip per byte.
            for tx in c.proposal_txs() {
                if limit == 0 || txs.len() >= MAX_BLOCK_TXS {
                    break;
                }
                if stuck.contains(&tx.from) || included.contains(&tx.id()) {
                    continue;
                }
                let raw = tx.to_bytes();
                let mut budget = ghost;
                if raw.len() > tx.max_bytes() || bytes + raw.len() > limit || budget.take(tx, raw.len()).is_err() {
                    // Unsigned ghost txs have no sender or nonce: leaving one out holds nothing
                    // else back.
                    if !tx.is_unsigned() {
                        stuck.insert(tx.from.clone());
                    }
                    continue;
                }
                // Only propose txs that will succeed (judged as verifiers and execution will judge
                // them); a tx that would just burn its fee waits or expires.
                let mut tj = crate::chain::Journal::default();
                match state.apply_j(&mut tj, tx, &chain_id, &proposer, height, true) {
                    Ok(crate::chain::Outcome::Applied) => {}
                    Ok(crate::chain::Outcome::Failed(_)) | Err(_) => {
                        state.rollback(tj);
                        if !tx.is_unsigned() {
                            stuck.insert(tx.from.clone());
                        }
                        continue;
                    }
                }
                ghost = budget;
                bytes += raw.len();
                txs.push(raw);
            }
            #[cfg(test)]
            if c.junk_proposals {
                txs.push(junk_tx(&chain_id, &c.genesis_hash, &self.address_prefix, height).to_bytes());
            }
            txs
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
        #[cfg(test)]
        let exec_root = {
            let mut r = exec_root;
            if self.chain.lock().wrong_exec_root {
                r[0] ^= 1;
            }
            r
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
        let (chain_id, genesis_hash) = {
            let c = self.chain.lock();
            // Our executed height only grows, so this stays true once it is.
            if block.exec_height + MAX_EXEC_LAG < c.height().min(parent.height) {
                return reject("exec_height too far behind");
            }
            (c.genesis.chain_id.clone(), c.genesis_hash.clone())
        };
        // Txs need a cited state that fixed this height's base fee (econ.rs): the proposer could
        // price them, and we can check them once we've executed it.
        if !block.txs.is_empty() && block.exec_height + crate::econ::FEE_LAG < block.height {
            return reject("txs in a block whose cited state can't price them");
        }
        let mut seen = std::collections::HashSet::new();
        let mut txs = Vec::with_capacity(block.txs.len());
        for raw in &block.txs {
            let Ok(tx) = Tx::from_bytes(raw) else { return reject("unparseable or non-canonical tx") };
            if !seen.insert(tx.id()) {
                return reject("duplicate tx");
            }
            txs.push(tx);
        }
        // Ghost txs' size limits come from this height's fee slot, fixed once `height − FEE_LAG`
        // executed (the same for every verifier, whatever parameter change executes after it), and
        // are checked before any proof is.
        if !txs.is_empty() {
            wait_executed(&ctx, &self.chain, block.height.saturating_sub(crate::econ::FEE_LAG), None).await;
            let slot = {
                let c = self.chain.lock();
                (c.height() < block.height).then(|| crate::econ::slot(&c.state, block.height)).flatten()
            };
            // Executed past this height already: whether it's the finalized block decides below.
            if let Some(slot) = slot {
                let mut ghost = crate::ghost::BlockBudget::new(Some(&slot));
                for (tx, raw) in txs.iter().zip(&block.txs) {
                    if let Err(e) = ghost.take(tx, raw.len()) {
                        warn!(error = %e, "ghost txs over the block's limits");
                        return reject("ghost txs over the size limits");
                    }
                }
            }
        }
        // Tx checks (signatures and ghost proofs included) run without holding the chain lock. The
        // validity window's upper bound is a governance parameter, so the replay below checks it
        // on the parent's state; expiry doesn't depend on any state.
        for tx in &txs {
            if let Err(e) = check_tx_stateless(tx, &chain_id, &genesis_hash, &self.address_prefix).and_then(|_| crate::chain::check_validity(tx, block.height, u64::MAX)) {
                warn!(error = %e, "bad tx in block");
                return reject("invalid tx");
            }
        }
        // The cited state root, once we've executed that height ourselves (until then the
        // verdict can still change, so we wait).
        match wait_executed(&ctx, &self.chain, block.exec_height, None).await {
            Some(root) if root == block.exec_root => {}
            Some(_) => return reject(if checkpoint.is_some() { "checkpoint state root mismatch" } else { "state root mismatch" }),
            // Below our history (we fast-synced past it): we can't check it, so we abstain.
            None => return futures::future::pending().await,
        }
        let (seen_already, replays, executed, slot) = {
            let c = self.chain.lock();
            (finalized(&c, &block), seen.iter().any(|id| c.has_tx(id)), c.height(), crate::econ::slot(&c.state, block.height))
        };
        match seen_already {
            Seen::Not => {}
            Seen::This => return true,
            Seen::Other => return reject("conflicts with a finalized block"),
            Seen::Unknown => return futures::future::pending().await,
        }
        if replays {
            return reject("replays an already-finalized tx");
        }
        // This height's size limit and base fee, exactly: we've executed the cited state (at most
        // FEE_LAG back) and not this height yet, so the slot is known.
        if let Some(slot) = slot {
            if block.txs.iter().map(|t| t.len() as u64).sum::<u64>() > slot.max_block_bytes {
                return reject("block exceeds max_block_bytes");
            }
            if txs.iter().any(|t| slot.fee(t.size()).checked_add(t.tip).is_none_or(|c| c > t.fee)) {
                return reject("a tx's fee doesn't cover the block's base fee");
            }
        }
        // CV-5: replay the unexecuted ancestors exactly (txs, then end-of-block and end-of-epoch
        // steps: `State::run_block`), then every tx must be admittable on the parent's state.
        // Every honest verifier replays the same blocks from the same executed state, so they
        // agree; beyond `MAX_REPLAY` ancestors we abstain.
        let Some(pending) = unexecuted(parent.clone(), &mut ancestry, executed).await else { return futures::future::pending().await };
        let (seen_already, tip) = {
            let c = self.chain.lock();
            (finalized(&c, &block), Tip::of(&c))
        };
        match seen_already {
            Seen::Not => {}
            Seen::This => return true,
            Seen::Other => return reject("conflicts with a finalized block"),
            Seen::Unknown => return futures::future::pending().await,
        }
        let Some(mut state) = replay(tip, &parent, pending) else { return reject("builds on a block that conflicts with the finalized chain") };
        for tx in &txs {
            if let Err(e) = state.apply(tx, &chain_id, &block.proposer, block.height) {
                warn!(error = %e, tx = %tx.id(), "unadmittable tx in block");
                return reject("carries a tx that can't be admitted");
            }
        }
        true
    }
}

/// Whether we've executed a block at `block`'s height already (we're late to vote).
enum Seen {
    Not,
    /// This very block: valid.
    This,
    /// Another one: `block` can never be finalized.
    Other,
    /// We have, but no longer know its hash (pruned): abstain.
    Unknown,
}

fn finalized(c: &Chain, block: &CBlock) -> Seen {
    if c.height() < block.height {
        return Seen::Not;
    }
    match c.hash_at(block.height) {
        Some(h) if h == hex::encode(block.digest.0) => Seen::This,
        Some(_) => Seen::Other,
        None => Seen::Unknown,
    }
}

/// Our executed tip: height, hash and a copy of the state (O(1): its maps share structure).
struct Tip {
    height: u64,
    hash: String,
    state: crate::chain::State,
    chain_id: String,
}

impl Tip {
    fn of(c: &Chain) -> Self {
        Self { height: c.height(), hash: c.last_hash().to_string(), state: c.state.clone(), chain_id: c.genesis.chain_id.clone() }
    }
}

/// `parent` and its ancestors above `executed` (newest first): `None` if there are more than
/// [`MAX_REPLAY`] of them, or the stream ends first.
async fn unexecuted(parent: Arc<CBlock>, ancestry: &mut impl Ancestry<CBlock>, executed: u64) -> Option<Vec<Arc<CBlock>>> {
    let mut pending = vec![];
    let mut b = parent;
    while b.height > executed {
        if pending.len() >= MAX_REPLAY {
            return None;
        }
        let h = b.height;
        pending.push(b);
        if h == executed + 1 {
            break;
        }
        b = ancestry.next().await?;
    }
    Some(pending)
}

/// The state after `parent`: `tip`'s state with the unexecuted ancestors `pending` (newest first,
/// from [`unexecuted`]) run through exactly the execute path (txs, then end-of-block and
/// end-of-epoch steps: `State::run_block`), on a copy. `None` if they don't extend our executed
/// chain (a fork that can never be finalized on top of it).
fn replay(tip: Tip, parent: &CBlock, mut pending: Vec<Arc<CBlock>>) -> Option<crate::chain::State> {
    // Blocks we executed since the ancestry was collected.
    pending.retain(|b| b.height > tip.height);
    let (below, link) = match pending.last() {
        Some(oldest) => (oldest.height - 1, oldest.parent),
        None => (parent.height, parent.digest),
    };
    if below != tip.height || hex::encode(link.0) != tip.hash {
        return None;
    }
    let mut state = tip.state;
    for b in pending.iter().rev() {
        let txs: Vec<Tx> = b.parse_txs().into_iter().flatten().collect();
        state.run_block(&tip.chain_id, b.height, &hex::encode(b.digest.0), &b.proposer, b.view, b.parent_view, &txs, |_, _| {});
    }
    Some(state)
}

/// Tests: a junk tx for a Byzantine proposer to stuff its blocks with: well-formed, signed and
/// fee-paying on paper (stateless checks pass), from an account that holds nothing, so it can
/// never be admitted.
#[cfg(test)]
pub fn junk_tx(chain_id: &str, genesis_hash: &str, prefix: &str, height: u64) -> Tx {
    let k = crate::crypto::Keys::from_seed([0xee; 32]);
    let mut tx = Tx { chain_id: chain_id.into(), genesis_hash: genesis_hash.into(), from: k.address_with(prefix), nonce: 0, fee: 0, tip: 0, valid_until: height + 10, action: crate::types::Action::Transfer { to: k.address_with(prefix), amount: 1 }, memo: format!("junk {height}"), public_key: hex::encode(k.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
    tx.fee = tx.charge().unwrap();
    tx.sign(&k)
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

/// Whether an activity is fault evidence: two conflicting signed votes from one validator.
pub fn is_fault(a: &Activity<MlDsaScheme, Digest>) -> bool {
    matches!(a, Activity::ConflictingNotarize(_) | Activity::ConflictingFinalize(_) | Activity::NullifyFinalize(_))
}

/// Passes only fault evidence on (in front of `AttributableReporter`, which would otherwise
/// verify every vote a second time).
#[derive(Clone)]
pub struct Faults<R>(pub R);

impl<R: Reporter<Activity = Activity<MlDsaScheme, Digest>>> Reporter for Faults<R> {
    type Activity = Activity<MlDsaScheme, Digest>;
    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        if is_fault(&activity) { self.0.report(activity) } else { commonware_actor::Feedback::Ok }
    }
}

/// Where verified fault evidence goes: encoded `Activity` bytes, the `evidence` of a
/// `ReportEquivocation` tx, for the node's evidence task (`node::report_evidence`). Simplex
/// doesn't persist the votes it compared, so that task keeps them until the chain has acted.
#[derive(Clone)]
pub struct EvidenceSink {
    pub found: futures::channel::mpsc::UnboundedSender<Vec<u8>>,
}

impl Reporter for EvidenceSink {
    type Activity = Activity<MlDsaScheme, Digest>;
    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        if is_fault(&activity) {
            warn!(epoch = ?activity_epoch(&activity), "a validator signed conflicting votes; reporting it");
            let _ = self.found.unbounded_send(activity.encode().to_vec());
        }
        commonware_actor::Feedback::Ok
    }
}

fn activity_epoch(a: &Activity<MlDsaScheme, Digest>) -> Option<u64> {
    use commonware_consensus::Epochable as _;
    match a {
        Activity::ConflictingNotarize(c) => Some(c.epoch().get()),
        Activity::ConflictingFinalize(c) => Some(c.epoch().get()),
        Activity::NullifyFinalize(c) => Some(c.epoch().get()),
        _ => None,
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
    /// Blocks executed without the certificate their height keeps on disk (epoch ends,
    /// checkpoints): [`fill_late_certs`] looks for it in marshal.
    pub late_certs: Option<futures::channel::mpsc::UnboundedSender<(u64, Digest)>>,
}

/// Fills in certificates [`CertCapture`] missed (it only sees finalizations simplex reports
/// while the node runs: not ones marshal fetched from peers to catch up, nor any from before a
/// restart, and it forgets them if execution falls 4,096 behind): marshal keeps the
/// finalizations it stored, so the certificate an epoch's last block needs (the anchor of every
/// fast-sync manifest in the next epoch) is read from there. Blocks marshal only backfilled
/// through a later finalization have no certificate of their own; those stay without.
pub async fn fill_late_certs(marshal: MarshalMailbox, chain: Shared, mut heights: futures::channel::mpsc::UnboundedReceiver<(u64, Digest)>) {
    while let Some((height, digest)) = heights.next().await {
        let Some(f) = marshal.get_finalization(Height::new(height)).await else {
            debug!(height, "no finalization of its own in marshal (backfilled through a later one)");
            continue;
        };
        if f.proposal.payload != digest {
            warn!(height, "marshal's finalization at this height is for another block");
            continue;
        }
        match Chain::store_late_cert(&chain, height, &hex::encode(digest.0), f.encode().to_vec()) {
            Ok(true) => info!(height, "kept a finalization certificate from marshal that execution missed"),
            Ok(false) => {}
            Err(e) => warn!(height, error = %e, "couldn't keep a late certificate"),
        }
    }
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
                    let missed = cert.is_none();
                    // Holds the chain lock only for the in-memory parts, not the fsyncs.
                    let executed = Chain::execute_shared(&self.chain, finalized_from(&block, cert));
                    let c = self.chain.lock();
                    match executed {
                        Ok(()) => {
                            if missed && (c.state.params.is_boundary(block.height) || block.height.is_multiple_of(crate::chain::CERT_CHECKPOINT)) {
                                if let Some(late) = &self.late_certs {
                                    let _ = late.unbounded_send((block.height, block.digest));
                                }
                            }
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
        let namespace = {
            let c = chain.lock();
            namespace(&c.genesis.chain_id, &c.genesis_hash)
        };
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

    /// Secondary peers besides `epoch`'s set: the eligible candidates most likely to join next
    /// (bonded, so not strangers), at most `limit` of them, so they sync before they vote. The
    /// p2p layer panics on a peer set over its cap, so the list is capped here.
    pub fn candidates(&self, epoch: u64, limit: usize) -> Set<PublicKey> {
        let c = self.chain.lock();
        let keys: Vec<PublicKey> = crate::staking::likely_candidates(&c.state, epoch, limit).iter().filter_map(|a| c.state.validators.get(a)).filter_map(|v| PublicKey::from_bytes(&hex::decode(&v.consensus_key).ok()?)).collect();
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

/// An epoch's leader election (SF-1): simplex's round robin over the epoch's sorted set, its
/// order shuffled by `seed` (`beacon::elector_seed`). Without a seed it's the plain rotation,
/// `sorted_set[(epoch + view) mod n]`. The shuffle is Commonware's (sorting indices by
/// BLAKE3(seed ‖ index)), so it is part of the consensus rules at the pinned version.
pub fn elector(seed: Option<[u8; 32]>) -> RoundRobin<Blake3> {
    match seed {
        Some(seed) => RoundRobin::shuffled(&seed),
        None => RoundRobin::default(),
    }
}

/// The address of the leader of `view` in `epoch`, as consensus elects it: the epoch's set and
/// beacon from `state`. Downtime accounting (missed leader slots, MAINNET-DESIGN 3.4) must use
/// this (`staking::leader_index` elects with the same elector), so it charges the validators
/// simplex actually waited for. `None`: no set for `epoch`.
#[cfg_attr(not(test), allow(dead_code))] // tests and tools; staking uses `leader_index`
pub fn leader(state: &crate::chain::State, epoch: u64, view: u64) -> Option<String> {
    use commonware_consensus::simplex::elector::{Config, Elector};
    use commonware_consensus::types::{Round, View};
    let set = keys_of(state.active_set(epoch)?.iter().map(|v| v.consensus_key.as_str()))?;
    let e = <RoundRobin<Blake3> as Config<MlDsaScheme>>::build(elector(crate::beacon::elector_seed(state, epoch)), &set);
    let idx = Elector::<MlDsaScheme>::elect(&e, Round::new(Epoch::new(epoch), View::new(view)), None);
    Some(set.get(idx.get() as usize)?.address(state.params.prefix()))
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
        // Equivocation evidence the engines verify goes to the evidence task, which submits it.
        let (found_tx, found_rx) = futures::channel::mpsc::unbounded::<Vec<u8>>();
        let evidence = EvidenceSink { found: found_tx };
        context.child("evidence").spawn({
            let chain = chain.clone();
            move |ctx| crate::node::report_evidence(ctx, chain, found_rx)
        });
        let (epoch_tx, mut epoch_rx) = futures::channel::mpsc::unbounded::<u64>();
        let (late_tx, late_rx) = futures::channel::mpsc::unbounded::<(u64, Digest)>();
        context.child("late_certs").spawn({
            let (marshal, chain) = (marshal.clone(), chain.clone());
            move |_| fill_late_certs(marshal, chain, late_rx)
        });
        let executor = Executor { chain: chain.clone(), certs: certs.clone(), epochs: epoch_tx, marshal: Some(marshal.clone()), late_certs: Some(late_tx) };
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
        context.child("mempool_in").spawn(move |ctx| async move {
            // Audit F30: duplicates and known-bad bytes are dropped before any signature or proof
            // work, ghost proofs are verified within a per-peer budget off this loop, and a peer
            // that relays a tx whose signature or proof doesn't verify is muted for a while.
            let gate = Arc::new(Mutex::new(GossipGate::default()));
            loop {
                let Ok((from, msg)) = mp_receiver.recv().await else { break };
                match screen_gossip(&gate, &gossip_chain, &from, msg.as_ref(), ms(ctx.current())) {
                    Screened::Dropped { why, verified } => debug!(?from, why, verified, "dropping gossiped tx"),
                    Screened::Admit(tx) => admit_gossiped(&gossip_chain, tx),
                    // A ghost proof (~6 ms): on the blocking pool, so one peer's proofs can't hold
                    // up everyone else's gossip.
                    Screened::Prove(tx, id, fingerprint) => {
                        let (chain, gate) = (gossip_chain.clone(), gate.clone());
                        ctx.child("ghost_proof").shared(true).spawn(move |ctx| async move {
                            prove_gossiped(&gate, &chain, &from, tx, &id, fingerprint, || ms(ctx.current()));
                        });
                    }
                }
            }
        });

        // Epoch orchestrator: one simplex engine per epoch, consensus channels muxed by epoch.
        let (mux, mut votes, mut votes_backup) = Muxer::builder(context.child("vote_mux"), votes_net.0, votes_net.1, 1024).with_backup().build();
        mux.start();
        let (mux, mut certs_mux, mut certs_backup) = Muxer::builder(context.child("cert_mux"), certs_net.0, certs_net.1, 1024).with_backup().build();
        mux.start();
        let (mux, mut resolver_mux) = Muxer::new(context.child("resolver_mux"), resolver_net.0, resolver_net.1, 1024);
        mux.start();

        // Slashing protection's clock (its startup rule waits on the network for a while), the
        // newest epoch the network has finalized a block in (no votes in epochs it has left), and
        // the view of the newest block executed (no votes at or below it: audit F18).
        if let Some(p) = protection.clone() {
            let chain = chain.clone();
            context.child("slashing_protection").spawn(move |ctx| async move {
                loop {
                    let (finalized, executed) = {
                        let c = chain.lock();
                        (c.state.params.epoch_of(c.finalized_tip.max(1)), c.last_block().map(|b| (c.state.params.epoch_of(b.height), b.view)))
                    };
                    p.note_finalized_epoch(finalized);
                    if let Some((epoch, view)) = executed {
                        p.note_executed(epoch, view);
                    }
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
                let secondary = provider.candidates(epoch, MAX_PEERS_PER_SET.saturating_sub(primary.len()));
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
            // SF-1: the leader order is a permutation seeded from this epoch's beacon.
            let seed = crate::beacon::elector_seed(&chain.lock().state, epoch);
            let engine = simplex::Engine::new(
                context.child("simplex").with_attribute("epoch", epoch),
                simplex::config::Config {
                    scheme: scheme.as_ref().clone(),
                    elector: elector(seed),
                    blocker: blocker.clone(),
                    automaton: app.clone(),
                    relay: app.clone(),
                    // Marshal and the certificate capture get everything; slashing protection
                    // watches votes and certificates under our key (when on); faults go through
                    // `AttributableReporter` (both votes verified) to the evidence task, with a
                    // filter in front so it doesn't re-verify every vote.
                    reporter: Reporters::from((
                        marshal.clone(),
                        Reporters::from((
                            certs.clone(),
                            Reporters::<_, _, crate::protection::Observer>::from((
                                Faults(AttributableReporter::new(crate::staking::NoRng, scheme.as_ref().clone(), evidence.clone(), Sequential, true)),
                                protection.clone().map(crate::protection::Observer),
                            )),
                        )),
                    )),
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
                    // Keep full votes until their round is pruned, so a conflicting vote that
                    // arrives after the certificate still yields evidence (CV-3).
                    track_historical_votes: true,
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
                        let member = scheme.participants().position(&from).map(|i| i as u32);
                        hint_behind(&marshal, &chain, protection.as_deref(), epoch, their, from, member);
                    },
                    msg = certs_backup.recv() => {
                        let Some((their, (from, _))) = msg else { return };
                        let member = scheme.participants().position(&from).map(|i| i as u32);
                        hint_behind(&marshal, &chain, protection.as_deref(), epoch, their, from, member);
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
/// `member`: the peer's index in our epoch's set, if it's in it.
fn hint_behind(marshal: &MarshalMailbox, chain: &Shared, protection: Option<&crate::protection::Protection>, ours: u64, theirs: u64, from: PublicKey, member: Option<u32>) {
    if theirs <= ours {
        return;
    }
    // The epoch is the sender's word (the mux subchannel it picked), so slashing protection
    // counts it only from more of our epoch's validators than can be faulty (audit F18 and its
    // review). The boundary finalization marshal fetches is verified, and moves the node on.
    if let Some(p) = protection {
        p.note_later_epoch(ours, member, theirs);
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
/// v4 is the segmented block logs and the state database, see `store.rs`; v5 the integrated
/// protocol v1: txs signed for a genesis hash, state schema 4, block metadata with signals; v6
/// state schema 5; v7 consensus votes signed under the chain instance's namespace, chain id and
/// genesis hash, so older journals no longer verify, and state schema 6; v8 state schema 7; v9
/// state schema 8). Dev chains from an older layout reset themselves (`node::dev_is_stale`).
pub const STORAGE_VERSION: &str = "prunable-v9";

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
    use crate::types::DROPS_PER_FANG;
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
        // A tx up to the ghost bound decodes (its kind's own bound applies when it's parsed);
        // past it, the block doesn't.
        let ghost_sized = CBlock::new(Digest([1; 32]), 5, 9, 7, 1_000, String::new(), 4, [2; 32], vec![vec![0; crate::types::MAX_GHOST_TX_BYTES]]);
        assert!(CBlock::decode(ghost_sized.encode().as_ref()).is_ok());
        assert_eq!(ghost_sized.parse_txs(), vec![None]);
        let big = CBlock::new(Digest([1; 32]), 5, 9, 7, 1_000, String::new(), 4, [2; 32], vec![vec![0; crate::types::MAX_GHOST_TX_BYTES + 1]]);
        assert!(CBlock::decode(big.encode().as_ref()).is_err());
        // Txs that aren't canonical wire bytes don't parse.
        assert_eq!(b.parse_txs(), vec![None, None]);
    }

    /// Ghost txs in a proposed block: their proofs are checked before voting (a tx whose fee was
    /// changed after proving is permanently invalid), and they're held to `ghost_max_tx_bytes`
    /// each and `ghost_max_block_bytes` together (rejected at once, before any proof is checked).
    /// A tx that passes all of that still has to be admittable on the parent's state (CV-5): this
    /// one spends a note from a tree of its own on a chain with Ghost Mode off, so it isn't (the
    /// simulator's `ghost` scenario has blocks of admittable ghost txs voted through).
    #[test]
    fn verify_checks_ghost_proofs_and_size_limits() {
        let mut o = SummonOpts::new("gv", 1);
        o.params.insert("ghost_max_tx_bytes".into(), 150 << 10);
        o.params.insert("ghost_max_block_bytes".into(), 200 << 10);
        let (root_dir, c, seed) = home_at_zero_with("gv", o);
        let (instance, genesis_root, genesis_hash) = (c.state.ghost.meta.instance, hex_32(&c.root_at(0).unwrap()), c.genesis_hash.clone());
        let parent = genesis_block(&c.genesis);
        let chain: Shared = Arc::new(Mutex::new(c));
        let good = crate::ghost::tests::stateless_transfer("gv", &genesis_hash, instance, "a", 100);
        let other = crate::ghost::tests::stateless_transfer("gv", &genesis_hash, instance, "b", 100);
        let mut tampered = good.clone();
        tampered.fee += 1;
        check_tx_stateless(&good, "gv", &genesis_hash, "boo").unwrap();
        let mut budget = crate::ghost::BlockBudget::new(crate::econ::slot(&chain.lock().state, 1).as_ref());
        budget.take(&good, good.size() as usize).unwrap();
        assert!(budget.take(&other, other.size() as usize).is_err(), "two ~131 KiB ghost txs are over ghost_max_block_bytes");
        deterministic::Runner::timed(Duration::from_secs(60)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            let child = |txs: Vec<&Tx>| CBlock::new(parent.digest(), 1, 1, 0, 1_000, leader.address("boo"), 0, genesis_root, txs.into_iter().map(Tx::to_bytes).collect());
            let mut verify = async |block: CBlock| {
                let cctx = SimplexContext { round: Round::new(Epoch::zero(), View::new(1)), leader: leader.clone(), parent: (View::new(0), parent.digest()) };
                let ancestry = marshal::ancestry::from_iter([Arc::new(block), Arc::new(parent.clone())]);
                Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry).await
            };
            assert!(verify(child(vec![])).await, "the empty block is fine");
            assert!(!verify(child(vec![&tampered])).await, "its proof no longer verifies");
            assert!(!verify(child(vec![&good, &other])).await, "over ghost_max_block_bytes");
            assert!(!verify(child(vec![&good])).await, "not admittable here (Ghost Mode is off)");
        });
        std::fs::remove_dir_all(root_dir).ok();
    }

    /// A verifier that hasn't executed the height a block cites keeps the verdict pending (it
    /// used to vote no after ~3 block times at checkpoints), then checks the root once it has,
    /// and then (CV-5) replays the unexecuted ancestors to admit every tx. Mismatched views,
    /// versions and junk tx bytes are still rejected at once.
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
        let (hash100, hash101) = (Digest(hex_32(&a.hash_at(100).unwrap())), Digest(hex_32(&a.hash_at(101).unwrap())));
        drop(a);
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(ahead.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let genesis_hash = b.genesis_hash.clone();
        let chain: Shared = Arc::new(Mutex::new(b));
        deterministic::Runner::timed(Duration::from_secs(120)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            // Ancestors the verifier hasn't executed, linked to the blocks it will execute.
            let on = |parent: Digest, height: u64| CBlock::new(parent, height, 6, 5, 0, String::new(), height - 4, [0; 32], vec![]);
            let p101 = on(hash100, 101);
            let p102 = on(p101.digest(), 102);
            let p103 = on(p102.digest(), 103);
            let q102 = on(hash101, 102);
            let child = |parent: &CBlock, view: u64, exec_height: u64, root: [u8; 32], txs: Vec<Vec<u8>>| CBlock::new(parent.digest(), parent.height + 1, view, 6, 1_000, leader.address("boo"), exec_height, root, txs);
            let cctx = |parent: &CBlock| SimplexContext { round: Round::new(Epoch::zero(), View::new(7)), leader: leader.clone(), parent: (View::new(6), parent.digest()) };
            let mut verify = async |block: CBlock, ancestors: &[&CBlock]| {
                let cctx = cctx(ancestors[0]);
                let ancestry = marshal::ancestry::from_iter(std::iter::once(Arc::new(block)).chain(ancestors.iter().map(|b| Arc::new((*b).clone()))));
                Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry).await
            };
            let tx = |from: &crate::crypto::Keys, valid_until: u64| {
                let mut t = Tx { chain_id: "lag".into(), genesis_hash: genesis_hash.clone(), from: from.address(), nonce: 0, fee: 0, tip: 0, valid_until, action: crate::types::Action::Transfer { to: "xyz.boo".into(), amount: 1 }, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
                t.fee = t.charge().unwrap();
                t.sign(from)
            };
            let (user, funded) = (crate::crypto::Keys::from_seed([5; 32]), crate::crypto::Keys::from_seed(seed));
            let above = [&p103, &p102, &p101];
            // Permanent failures don't wait.
            assert!(!verify(child(&p103, 8, 100, root100, vec![]), &above).await, "view isn't the round's");
            assert!(!verify(CBlock { version: 2, ..child(&p103, 7, 100, root100, vec![]) }.sealed(), &above).await, "unknown version");
            assert!(!verify(child(&p103, 7, 100, root100, vec![b"junk".to_vec()]), &above).await, "junk tx bytes");
            assert!(!verify(child(&p103, 7, 100, root100, vec![tx(&funded, 50).to_bytes()]), &above).await, "expired tx");
            let mut padded = tx(&funded, 110).to_bytes();
            padded.push(0);
            assert!(!verify(child(&p103, 7, 100, root100, vec![padded]), &above).await, "non-canonical tx bytes");
            assert_eq!(context.current(), std::time::UNIX_EPOCH, "rejected without waiting");

            // The verifier catches up 10 s from now (well past 3 block times), then 10 s later.
            let catch_up = chain.clone();
            context.child("catch_up").spawn(move |ctx| async move {
                for _ in 0..2 {
                    ctx.sleep(Duration::from_secs(10)).await;
                    catch_up.lock().produce_block_with(vec![]).unwrap();
                }
            });
            // Block 104 must certify checkpoint 100, which we haven't executed: wait, then replay
            // 101–103 on top of it and accept.
            assert!(verify(child(&p103, 7, 100, root100, vec![]), &above).await);
            assert!(context.current() >= std::time::UNIX_EPOCH + Duration::from_secs(10), "waited for execution");
            // A block citing 101 (not executed yet) with the wrong root: rejected only once we
            // can check it (it used to be accepted unchecked).
            assert!(!verify(child(&q102, 7, 101, [9; 32], vec![]), &[&q102]).await);
            assert!(context.current() >= std::time::UNIX_EPOCH + Duration::from_secs(20));
            assert!(verify(child(&q102, 7, 101, root101, vec![tx(&funded, 110).to_bytes()]), &[&q102]).await);
            // A tx nobody can pay for isn't admittable: the block is junk (CV-5).
            assert!(!verify(child(&q102, 7, 101, root101, vec![tx(&user, 110).to_bytes()]), &[&q102]).await);
            // Ancestors that don't extend what we executed can never be finalized on top of it.
            assert!(!verify(child(&p102, 7, 101, root101, vec![]), &[&p102, &p101]).await, "a fork below our tip");
        });
        std::fs::remove_dir_all(root_dir).ok();
    }

    /// Empty unexecuted blocks at heights `below + 1 ..= to`, chained onto the executed block
    /// `below` (hash `hash`), newest first: the ancestry verification and proposals replay.
    fn line_on(below: u64, hash: Digest, to: u64) -> Vec<CBlock> {
        let mut blocks: Vec<CBlock> = vec![];
        for h in below + 1..=to {
            let parent = blocks.last().map_or(hash, |b| b.digest());
            blocks.push(CBlock::new(parent, h, h, h - 1, 0, String::new(), below, [0; 32], vec![]));
        }
        blocks.reverse();
        blocks
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
        let hash10 = Digest(hex_32(c.last_hash()));
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let chain: Shared = Arc::new(Mutex::new(c));
        deterministic::Runner::timed(Duration::from_secs(120)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            // Unexecuted ancestors up to `height`, on our executed block 10 (verification replays them).
            let above = |height: u64| line_on(10, hash10, height);
            let child = |parent: &CBlock, version: u32| CBlock::new(parent.digest(), parent.height + 1, 7, 6, 1_000, leader.address("boo"), 10, root10, vec![]).with_versions(version, 1);
            let cctx = |parent: &CBlock| SimplexContext { round: Round::new(Epoch::zero(), View::new(7)), leader: leader.clone(), parent: (View::new(6), parent.digest()) };
            // `Some(verdict)`, or `None` if it's still pending after 10 s.
            let mut verify = async |version: u32, ancestors: Vec<CBlock>| {
                let block = child(&ancestors[0], version);
                let cctx = cctx(&ancestors[0]);
                let ancestry = marshal::ancestry::from_iter(std::iter::once(block).chain(ancestors).map(Arc::new));
                let verdict = Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry);
                commonware_macros::select! {
                    v = verdict => Some(v),
                    _ = context.sleep(Duration::from_secs(10)) => None,
                }
            };
            assert_eq!(verify(1, above(20)).await, Some(true), "v1 before the activation height");
            assert_eq!(verify(2, above(20)).await, Some(false), "v2 too early");
            // This binary runs v1 only: from the activation height on, no vote either way.
            assert_eq!(verify(2, above(73)).await, None);
            assert_eq!(verify(1, above(73)).await, None);
            crate::upgrade::configure(&mut chain.lock(), v2, None);
            assert_eq!(verify(1, above(73)).await, Some(false), "v1 after the activation height");
            assert_eq!(verify(2, above(73)).await, Some(true));
            // A block 64+ heights past what we executed waits until the version there is certain.
            assert_eq!(verify(2, above(80)).await, None);
            // --halt-height 50: nothing past it gets a vote.
            crate::upgrade::configure(&mut chain.lock(), v2, Some(50));
            assert_eq!(verify(1, above(20)).await, Some(true));
            assert_eq!(verify(2, above(73)).await, None);
            let c = chain.lock();
            assert!(c.halt.allows(&c.state, 50) && !c.halt.allows(&c.state, 51));
        });
        std::fs::remove_dir_all(home).ok();
    }

    /// The fee market in consensus (P-ECON): a proposer fills blocks only up to the height's
    /// `max_block_bytes` and proposes no txs at a height whose base fee its state hasn't fixed
    /// (more than `econ::FEE_LAG` ahead); a verifier refuses a block with txs that cites a state
    /// that can't price them, and, once it has executed the cited state, blocks over the size
    /// limit and txs that don't pay the height's exact base fee (which the stateless check can't
    /// see). Here the limit is lowered to 64 KiB (~12 transfers).
    #[test]
    fn blocks_are_sized_and_priced_by_the_fee_market() {
        let home = std::env::temp_dir().join(format!("blockula-feemarket-{}-{}", crate::chain::now_ms(), std::process::id()));
        let params = [("max_block_bytes".to_string(), crate::types::MAX_TX_BYTES as u64)].into_iter().collect();
        Chain::summon(&home, &SummonOpts { params, ..SummonOpts::new("fees", 1) }).unwrap();
        let mut c = Chain::open(&home).unwrap();
        let users: Vec<crate::crypto::Keys> = (1..=14u8).map(|i| crate::crypto::Keys::from_seed([i; 32])).collect();
        for u in &users {
            c.faucet(&u.address(), 100 * crate::types::DROPS_PER_FANG).unwrap();
        }
        c.produce_block().unwrap();
        let e = c.height();
        let me = c.me().unwrap();
        // Fees for the block after next (the next one, e + 1, is the unexecuted parent), or one
        // drop short of its base fee.
        let transfer = |u: &crate::crypto::Keys, short: bool| {
            let mut t = Tx { chain_id: "fees".into(), genesis_hash: c.genesis_hash.clone(), from: u.address(), nonce: 0, fee: 0, tip: 0, valid_until: e + 50, action: crate::types::Action::Transfer { to: me.clone(), amount: 1 }, memo: String::new(), public_key: hex::encode(u.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            t.fee = if short { crate::econ::base_fee(&c.state, e + 2, t.size()) - 1 } else { crate::econ::max_fee(&c.state, t.size()) };
            t.sign(u)
        };
        let paid: Vec<Tx> = users.iter().map(|u| transfer(u, false)).collect();
        let underpaid = transfer(&users[0], true);
        for t in &paid {
            c.submit(t.clone()).unwrap();
        }
        let root_e = hex_32(&c.root_at(e).unwrap());
        let hash_e = Digest(hex_32(c.last_hash()));
        let genesis_hash = c.genesis_hash.clone();
        let seed: [u8; 32] = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        let chain: Shared = Arc::new(Mutex::new(c));
        deterministic::Runner::timed(Duration::from_secs(60)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            // Empty unexecuted blocks e + 1 ..= to on our head, newest first (they're replayed).
            let run = |to: u64| line_on(e, hash_e, to);
            let cctx = |parent: &CBlock| SimplexContext { round: Round::new(Epoch::zero(), View::new(parent.height + 1)), leader: leader.clone(), parent: (View::new(parent.view), parent.digest()) };
            let mut propose = async |ancestors: Vec<CBlock>| {
                let cctx = cctx(&ancestors[0]);
                let ancestry = marshal::ancestry::from_iter(ancestors.into_iter().map(Arc::new));
                Application::<deterministic::Context>::propose(&mut app, (context.child("propose"), cctx), ancestry, ()).await.unwrap()
            };
            // On one unexecuted block: as many txs as fit the limit, citing our state.
            let b = propose(run(e + 1)).await;
            let bytes: usize = b.txs.iter().map(Vec::len).sum();
            assert!(b.txs.len() >= 10 && b.txs.len() < paid.len() && bytes <= crate::types::MAX_TX_BYTES, "{} txs, {bytes} bytes", b.txs.len());
            assert_eq!(b.exec_height, e);
            // Seventeen blocks ahead of our state we can still price txs; eighteen we can't.
            assert!(!propose(run(e + 16)).await.txs.is_empty());
            let far = propose(run(e + 17)).await;
            assert!(far.txs.is_empty() && far.height == e + 18);

            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            let mut verify = async |block: CBlock, ancestors: Vec<CBlock>| {
                let cctx = cctx(&ancestors[0]);
                let ancestry = marshal::ancestry::from_iter(std::iter::once(block).chain(ancestors).map(Arc::new));
                Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry).await
            };
            let child = |parent: &CBlock, exec_height: u64, root: [u8; 32], txs: &[&Tx]| CBlock::new(parent.digest(), parent.height + 1, parent.height + 1, parent.view, 1_000, leader.address("boo"), exec_height, root, txs.iter().map(|t| t.to_bytes()).collect());
            let above = run(e + 1);
            assert!(verify(child(&above[0], e, root_e, &[&paid[0], &paid[1]]), above.clone()).await);
            // Underpaying by one drop passes every stateless check, not the exact one.
            check_tx_stateless(&underpaid, "fees", &genesis_hash, "boo").unwrap();
            assert!(!verify(child(&above[0], e, root_e, &[&underpaid]), above.clone()).await);
            // Over the height's limit (but well under the codec's bound).
            assert!(!verify(child(&above[0], e, root_e, &paid.iter().collect::<Vec<_>>()), above.clone()).await);
            // Txs in a block whose cited state is more than FEE_LAG back: refused at once.
            let ahead = run(e + 30);
            let t0 = context.current();
            assert!(!verify(child(&ahead[0], e + 10, [0; 32], &[&paid[0]]), ahead).await);
            assert_eq!(context.current(), t0, "without waiting");
        });
        std::fs::remove_dir_all(home).ok();
    }

    /// A one-validator home whose genesis starts at the simulated clock's zero.
    fn home_at_zero(tag: &str) -> (std::path::PathBuf, Chain, [u8; 32]) {
        home_at_zero_with(tag, SummonOpts::new(tag, 1))
    }

    fn home_at_zero_with(tag: &str, opts: SummonOpts) -> (std::path::PathBuf, Chain, [u8; 32]) {
        let home = std::env::temp_dir().join(format!("blockula-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        Chain::summon(&home, &opts).unwrap();
        let mut g = Genesis::load(&home.join("genesis.json")).unwrap();
        g.created_at_ms = 0;
        std::fs::write(home.join("genesis.json"), serde_json::to_string_pretty(&g).unwrap()).unwrap();
        let seed = hex::decode(std::fs::read_to_string(home.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap();
        (home.clone(), Chain::open(&home).unwrap(), seed)
    }

    /// CV-5: verification replays the unexecuted ancestors through the exact execute path (txs
    /// and end-of-block steps), so a tx is judged on the parent's real state: nonces an ancestor
    /// used, and balances its end-of-block steps moved (here, its proposer's charge for a junk tx),
    /// count. Beyond `MAX_REPLAY` ancestors the verifier abstains.
    #[test]
    fn verify_replays_unexecuted_ancestors_exactly() {
        // Tips up to 1,000× the base fee, so one tx can spend nearly all of P's balance.
        let params = [("tip_cap_multiple".to_string(), 1_000)].into_iter().collect();
        let (home, mut c, seed) = home_at_zero_with("replay", SummonOpts { params, ..SummonOpts::new("replay", 1) });
        let (chain_id, genesis_hash) = (c.genesis.chain_id.clone(), c.genesis_hash.clone());
        let junk_genesis = genesis_hash.clone();
        let signed = move |from: &crate::crypto::Keys, nonce: u64, tip: u64, to: String, amount: u64| {
            let mut t = Tx { chain_id: chain_id.clone(), genesis_hash: genesis_hash.clone(), from: from.address(), nonce, fee: 0, tip, valid_until: 50, action: crate::types::Action::Transfer { to, amount }, memo: String::new(), public_key: hex::encode(from.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            t.fee = t.charge().unwrap();
            t.sign(from)
        };
        let (me, p) = (crate::crypto::Keys::from_seed(seed), crate::crypto::Keys::from_seed([4; 32]));
        // P holds 0.01 FANG: enough for one tx with a tip that leaves it 1,000 drops.
        let funded = crate::types::MIN_NEW_ACCOUNT;
        c.produce_block_with(vec![signed(&me, 0, 0, p.address(), funded)]).unwrap();
        let (tip, root) = (Digest(hex_32(c.last_hash())), hex_32(&c.state.root()));
        assert_eq!(c.state.account(&p.address()).balance, funded);
        let chain_id = c.genesis.chain_id.clone();
        let chain: Shared = Arc::new(Mutex::new(c));
        deterministic::Runner::timed(Duration::from_secs(120)).start(|context| async move {
            let pricey = |nonce: u64| {
                let probe = signed(&p, nonce, 0, me.address(), 1);
                signed(&p, nonce, funded - probe.charge().unwrap() - 1_000, me.address(), 1)
            };
            assert_eq!(pricey(0).charge().unwrap(), funded - 1_000);
            let send = |nonce: u64| signed(&me, nonce, 0, p.address(), DROPS_PER_FANG);
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            // Ancestor 2 (unexecuted), proposed by P; candidate 3, by the leader.
            let a2 = |txs: Vec<Vec<u8>>| CBlock::new(tip, 2, 2, 1, 0, p.address(), 1, root, txs);
            let b3 = |parent: &CBlock, txs: Vec<Tx>| CBlock::new(parent.digest(), parent.height + 1, parent.height + 1, parent.height, 1_000, leader.address("boo"), 1, root, txs.iter().map(Tx::to_bytes).collect());
            let mut verify = async |block: CBlock, ancestors: Vec<CBlock>| {
                let cctx = SimplexContext { round: Round::new(Epoch::zero(), View::new(block.view)), leader: leader.clone(), parent: (View::new(block.parent_view), ancestors[0].digest()) };
                let ancestry = marshal::ancestry::from_iter(std::iter::once(block).chain(ancestors).map(Arc::new));
                Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx), ancestry).await
            };
            // P can pay for its pricey tx on top of an empty ancestor…
            let empty = a2(vec![]);
            assert!(verify(b3(&empty, vec![pricey(0)]), vec![empty.clone()]).await);
            // …but not once the ancestor's junk tx has cost P, its proposer (an end-of-block step).
            let junky = a2(vec![junk_tx(&chain_id, &junk_genesis, "boo", 2).to_bytes()]);
            assert!(!verify(b3(&junky, vec![pricey(0)]), vec![junky.clone()]).await);
            assert!(verify(b3(&junky, vec![signed(&p, 0, 0, me.address(), 1)]), vec![junky.clone()]).await);
            // An ancestor's tx used nonce 1: the next one must be 2.
            let used = a2(vec![send(1).to_bytes()]);
            assert!(!verify(b3(&used, vec![send(1)]), vec![used.clone()]).await);
            assert!(verify(b3(&used, vec![send(2)]), vec![used.clone()]).await);
            // More than MAX_REPLAY unexecuted ancestors: the verdict stays pending.
            let mut line = vec![a2(vec![])];
            while line.len() < MAX_REPLAY + 1 {
                let last = line.last().unwrap();
                line.push(CBlock::new(last.digest(), last.height + 1, last.height + 1, last.height, 0, p.address(), 1, root, vec![]));
            }
            line.reverse();
            let far = b3(&line[0], vec![]);
            let started = context.current();
            commonware_macros::select! {
                v = verify(far, line) => panic!("decided ({v}) without replaying"),
                _ = context.sleep(Duration::from_secs(30)) => {},
            }
            assert!(context.current() >= started + Duration::from_secs(30));
        });
        std::fs::remove_dir_all(home).ok();
    }

    /// A proposer replays the same way, and an honest one's blocks pass verification; a Byzantine
    /// one that stuffs a junk tx in gets its block rejected (and, if it were finalized anyway,
    /// charged for it: see `staking::tests`).
    #[test]
    fn honest_proposals_verify_and_junk_ones_dont() {
        let (home, c, seed) = home_at_zero("junk");
        let g = c.genesis.clone();
        let chain: Shared = Arc::new(Mutex::new(c));
        deterministic::Runner::timed(Duration::from_secs(120)).start(|context| async move {
            let leader = PrivateKey::from_seed_bytes(seed).public_key();
            let mut app = App { chain: chain.clone(), block_time_ms: 1_000, address_prefix: "boo".into() };
            let parent = genesis_block(&g);
            let cctx = SimplexContext { round: Round::new(Epoch::zero(), View::new(1)), leader: leader.clone(), parent: (View::new(0), parent.digest()) };
            let mut blocks = vec![];
            for junk in [false, true] {
                chain.lock().junk_proposals = junk;
                let ancestry = marshal::ancestry::from_iter([Arc::new(parent.clone())]);
                let block = Application::<deterministic::Context>::propose(&mut app, (context.child("propose"), cctx.clone()), ancestry, ()).await.expect("proposes");
                assert_eq!(block.txs.len(), junk as usize);
                let ancestry = marshal::ancestry::from_iter([Arc::new(block.clone()), Arc::new(parent.clone())]);
                assert_eq!(Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx.clone()), ancestry).await, !junk);
                blocks.push(block);
            }
            // A late voter that already executed the honest block: it's valid; the other one at
            // that height can never be finalized.
            chain.lock().execute(finalized_from(&blocks[0], None)).unwrap();
            for (block, valid) in [(&blocks[0], true), (&blocks[1], false)] {
                let ancestry = marshal::ancestry::from_iter([Arc::new(block.clone()), Arc::new(parent.clone())]);
                assert_eq!(Application::<deterministic::Context>::verify(&mut app, (context.child("verify"), cctx.clone()), ancestry).await, valid);
            }
        });
        std::fs::remove_dir_all(home).ok();
    }

    /// Audit F30: gossip is screened cheapest-first. A tx already in the mempool is dropped before
    /// its signature is looked at (this one's signature is junk: it was put in the mempool
    /// directly), the same invalid bytes are verified once whichever peer sends them, a peer that
    /// relays a bad signature is muted for a while (its good txs too; other peers aren't), and
    /// oversized messages are dropped before they're parsed.
    #[test]
    fn gossip_drops_duplicates_and_known_junk_before_verifying() {
        let (home, mut c, _) = home_at_zero("gossip");
        let users: Vec<crate::crypto::Keys> = (1..=4u8).map(|i| crate::crypto::Keys::from_seed([i; 32])).collect();
        for u in &users {
            c.faucet(&u.address(), 100 * DROPS_PER_FANG).unwrap();
        }
        c.produce_block().unwrap();
        let me = c.me().unwrap();
        let transfer = |c: &Chain, u: &crate::crypto::Keys, amount: u64| {
            let mut t = Tx { chain_id: c.genesis.chain_id.clone(), genesis_hash: c.genesis_hash.clone(), from: u.address(), nonce: 0, fee: 0, tip: 0, valid_until: c.height() + 50, action: crate::types::Action::Transfer { to: me.clone(), amount }, memo: String::new(), public_key: hex::encode(u.public_key()), signature: "00".repeat(crate::crypto::SIGNATURE_LEN) };
            t.fee = crate::econ::max_fee(&c.state, t.size());
            t.sign(u)
        };
        let forge = |t: &Tx| {
            let mut f = t.clone();
            f.signature = format!("{}{}", if f.signature.starts_with("00") { "01" } else { "00" }, &f.signature[2..]);
            f
        };
        // users[0]'s tx sits in the mempool with a signature that doesn't verify.
        let junk_in_pool = forge(&transfer(&c, &users[0], 1));
        assert!(junk_in_pool.verify_signature().is_err());
        c.submit(junk_in_pool.clone()).unwrap();
        let chain: Shared = Arc::new(Mutex::new(c));
        let gate = Mutex::new(GossipGate::<u8>::default());
        let (a, b, d) = (1u8, 2u8, 3u8);
        let dropped = |r: Screened| match r {
            Screened::Dropped { why, verified } => (why, verified),
            other => panic!("expected a drop, got {other:?}"),
        };

        // Known: dropped unverified, and not held against the peer.
        assert_eq!(dropped(screen_gossip(&gate, &chain, &a, &junk_in_pool.to_bytes(), 0)), ("already known", false));
        let good_a = transfer(&chain.lock(), &users[1], 1);
        let Screened::Admit(tx) = screen_gossip(&gate, &chain, &a, &good_a.to_bytes(), 0) else { panic!("a good tx from a is admitted") };
        admit_gossiped(&chain, tx);
        assert!(chain.lock().in_mempool(&good_a.id()));
        assert_eq!(dropped(screen_gossip(&gate, &chain, &d, &good_a.to_bytes(), 0)), ("already known", false));

        // A bad signature: verified once, remembered, and its relayer muted.
        let bad = forge(&transfer(&chain.lock(), &users[2], 2));
        assert_eq!(dropped(screen_gossip(&gate, &chain, &b, &bad.to_bytes(), 1_000)), ("bad signature", true));
        assert_eq!(dropped(screen_gossip(&gate, &chain, &d, &bad.to_bytes(), 1_000)), ("muted peer or known-invalid bytes", false), "the same bytes from anyone else");
        let good_b = transfer(&chain.lock(), &users[2], 3);
        assert_eq!(dropped(screen_gossip(&gate, &chain, &b, &good_b.to_bytes(), 1_000 + GOSSIP_MUTE_MS - 1)), ("muted peer or known-invalid bytes", false));
        assert!(matches!(screen_gossip(&gate, &chain, &d, &good_b.to_bytes(), 1_000), Screened::Admit(_)), "another peer's copy is fine");
        assert!(matches!(screen_gossip(&gate, &chain, &b, &good_b.to_bytes(), 1_000 + GOSSIP_MUTE_MS), Screened::Admit(_)), "the mute wears off");

        // Another instance of the chain (an error a peer on another network could send in good
        // faith): remembered, no mute.
        let mut other = transfer(&chain.lock(), &users[3], 4);
        other.genesis_hash = "00".repeat(32);
        let other = other.sign(&users[3]);
        assert_eq!(dropped(screen_gossip(&gate, &chain, &d, &other.to_bytes(), 2_000)), ("invalid", true));
        assert_eq!(dropped(screen_gossip(&gate, &chain, &d, &other.to_bytes(), 2_000)), ("muted peer or known-invalid bytes", false));
        let good_d = transfer(&chain.lock(), &users[3], 5);
        assert!(matches!(screen_gossip(&gate, &chain, &d, &good_d.to_bytes(), 2_000), Screened::Admit(_)));

        // Too large: dropped before anything is parsed.
        assert_eq!(dropped(screen_gossip(&gate, &chain, &d, &vec![0; crate::types::MAX_GHOST_TX_BYTES + 1], 2_000)), ("too large", false));
        std::fs::remove_dir_all(home).ok();
    }

    /// Audit F30: ghost proofs from gossip are verified within each peer's budget, once per tx
    /// however many peers relay it, and a proof that doesn't verify mutes its relayer.
    #[test]
    fn gossiped_ghost_proofs_are_budgeted_and_verified_once() {
        let (home, mut c, _) = home_at_zero("gossip-ghost");
        let mut j = crate::chain::Journal::default();
        let h = c.height();
        crate::upgrade::exec_activate_feature(&mut c.state, &mut j, crate::ghost::FEATURE, h).unwrap();
        let (chain_id, genesis_hash, instance) = (c.genesis.chain_id.clone(), c.genesis_hash.clone(), c.state.ghost.meta.instance);
        let chain: Shared = Arc::new(Mutex::new(c));
        let gate = Mutex::new(GossipGate::<u8>::default());
        let good = crate::ghost::tests::stateless_transfer(&chain_id, &genesis_hash, instance, "g1", 100);
        let mut tampered = crate::ghost::tests::stateless_transfer(&chain_id, &genesis_hash, instance, "g2", 100);
        tampered.tip += 1;
        tampered.fee += 1;

        // In flight: the same tx from another peer is a duplicate.
        let Screened::Prove(tx, id, fp) = screen_gossip(&gate, &chain, &1, &good.to_bytes(), 0) else { panic!("verified next") };
        assert!(matches!(screen_gossip(&gate, &chain, &2, &good.to_bytes(), 0), Screened::Dropped { why: "already known", verified: false }));
        // Its proof verifies (the mempool then refuses it: its anchor is a tree of its own).
        assert!(prove_gossiped(&gate, &chain, &1, tx, &id, fp, || 0));
        assert!(!gate.lock().verifying(&id) && !gate.lock().drops(&1, &fp, 0));

        // A proof that doesn't verify (the tx changed after proving): remembered, relayer muted.
        let Screened::Prove(tx, id, fp) = screen_gossip(&gate, &chain, &3, &tampered.to_bytes(), 0) else { panic!("verified next") };
        assert!(!prove_gossiped(&gate, &chain, &3, tx, &id, fp, || 0));
        assert!(matches!(screen_gossip(&gate, &chain, &4, &tampered.to_bytes(), 0), Screened::Dropped { verified: false, .. }), "never verified again");
        assert!(gate.lock().drops(&3, &[0; 32], GOSSIP_MUTE_MS - 1) && !gate.lock().drops(&3, &[0; 32], GOSSIP_MUTE_MS));

        // Budgets: GOSSIP_PROOFS_IN_FLIGHT at once over every peer; GOSSIP_PROOF_BURST per peer,
        // refilled at GOSSIP_PROOFS_PER_SEC.
        let mut g = GossipGate::<u8>::default();
        let ids: Vec<String> = (0..200).map(|i| format!("{i}")).collect();
        for id in &ids[..GOSSIP_PROOFS_IN_FLIGHT] {
            assert!(g.take_proof(&1, id, 0));
        }
        assert!(!g.take_proof(&2, &ids[GOSSIP_PROOFS_IN_FLIGHT], 0), "nothing more in flight");
        let mut taken = GOSSIP_PROOFS_IN_FLIGHT as u64;
        for id in &ids[..GOSSIP_PROOFS_IN_FLIGHT] {
            g.proof_done(id);
        }
        let mut next = GOSSIP_PROOFS_IN_FLIGHT;
        while g.take_proof(&1, &ids[next], 0) {
            g.proof_done(&ids[next]);
            next += 1;
            taken += 1;
        }
        assert_eq!(taken, GOSSIP_PROOF_BURST, "a peer's burst");
        assert!(g.take_proof(&2, &ids[next], 0), "another peer has its own budget");
        g.proof_done(&ids[next]);
        assert!(!g.take_proof(&1, &ids[next + 1], 1_000 / GOSSIP_PROOFS_PER_SEC - 1));
        assert!(g.take_proof(&1, &ids[next + 1], 1_000 / GOSSIP_PROOFS_PER_SEC), "refilled");
        std::fs::remove_dir_all(home).ok();
    }
}
