//! 👻 Ghost Mode in the node (MAINNET-DESIGN §6, DEC-2; docs/GHOST-MODE.md section 8): the
//! shielded pool's state, its actions (tags 50–54), proof verification and the turnstile. The
//! cryptography (keys, notes, the commitment tree, the Action STARK, note encryption) is the
//! `blockula-ghost` crate in `node/ghost`.
//!
//! **Disabled at genesis, and not audited.** Every ghost action is refused ("Ghost Mode is off")
//! until governance runs `ActivateFeature { ghost }` (upgrade.rs), which it should only do after
//! the gates in GHOST-MODE.md section 9. A mainnet genesis can't switch it on (`Genesis::check`),
//! and `ghost_pool_cap` starts at 0, so even an activated pool takes no deposits until governance
//! also raises the cap.
//!
//! How it fits the node:
//!
//! - **Transactions.** `Shield` (50), `SetGhostAddress` (53) and `GhostPause` (54) are ordinary
//!   signed txs. `PrivateTransfer` (51) and `Unshield` (52) are unsigned: no sender, nonce, key or
//!   signature, so nothing links them to an account. They're the same `Tx` type with those fields
//!   empty, in a wire form of their own (`BLOCKULA_GHOST_V1 ‖ chain_id ‖ fee ‖ tip ‖ valid_until ‖
//!   action`, see `Tx::signing_bytes`), which keeps blocks, gossip, the mempool and the API on one
//!   type and leaves every signed-tx code path as it was: `Tx::from_bytes` dispatches on the
//!   domain, `State::apply_j` hands unsigned txs to [`apply_unsigned`] before it looks for a
//!   sender, and the proof authorizes them instead of a signature. The nullifier set stops
//!   replays; `valid_until` is in the sighash.
//! - **Proofs.** Each bundle's proof is bound to its tx by the sighash (`Tx::ghost_sighash`: every
//!   field but the proof, including the chain instance, the fee and the tip), and its value
//!   balance comes from the tag and the tx's fields ([`value_balance`]), never from the sender.
//!   [`verify_proof`] needs no state, so it runs where signatures are checked, before the chain
//!   lock (HTTP submission on a blocking thread, gossip, consensus `verify`), and again at
//!   execution through a cache of verdicts keyed by the verifier's entire input: a hit is exactly
//!   a repeat of the same pure verification, so the cache changes how long execution takes,
//!   never what it does.
//! - **Value.** The pool is an escrow (`pool`, counted by `State::check_invariants`): a shield
//!   moves its amount from the sender into it; a private transfer pays its fee, and an unshield
//!   its amount and fee, out of it. The turnstile: nothing leaves beyond what's in the pool, so a
//!   soundness bug can at worst take shielded deposits, never mint; `ghost_pool_cap` bounds the
//!   deposits.
//! - **Fees.** A shield pays the usual byte fee and tip, plus `ghost_verify_fee` (burned). An
//!   unsigned tx's `fee` is exact (it's in the value balance) and must cover the base fee, its
//!   tip, `ghost_verify_fee` and, for an unshield that opens the recipient's account,
//!   `account_creation_fee` (DEC-6): the tip goes to the proposer, the rest is burned. Its fee
//!   and tip are whole multiples of [`FEE_GRID`] ([`canonical_fee`]), so they don't fingerprint
//!   wallets.
//! - **Anchors.** At the end of a block that changed the tree, its root becomes the anchor; the
//!   root it replaces stays usable for `ghost_anchor_window` blocks.
//! - **Pause.** `GhostPause` from f + 1 distinct validators of the current epoch's active set
//!   pauses the pool from the next block (f + 1 can halt the chain anyway, so this adds no power,
//!   and it can only stop the pool, never move funds). A pause belongs to the activation it was
//!   voted under, so governance resumes the pool by switching the feature off and on again.
//! - **Limits.** Ghost txs (50–52) up to `ghost_max_tx_bytes`, at most `ghost_max_block_bytes` of
//!   them per block ([`BlockBudget`], consensus.rs; both as the block's fee slot fixed them
//!   `econ::FEE_LAG` blocks ahead), at most [`MAX_MEMPOOL_GHOST`] queued; a queued spend of a
//!   note is replaced, and a full ghost share gives up its cheapest tx, only for a tip higher by
//!   a tenth and the relay of the newcomer's bytes ([`mempool_admit`]). A queued shield is its
//!   sender's last queued tx, which only its sender's own shield can replace ([`behind_shield`]).
//!
//! State (namespaces `state::ns` 48–52, 53–55 reserved; the scalars are the META record `ghost`):
//!
//! | Namespace | Key → value | |
//! |---|---|---|
//! | 48 `GHOST_COMMITMENTS` | position → note commitment | append-only, for wallet sync |
//! | 49 `GHOST_ROOTS` | retired root → height it was replaced | usable anchors besides the current one |
//! | 50 `GHOST_RETIRED` | height → root replaced then | the same, by age (for expiry) |
//! | 51 `GHOST_NULLIFIERS` | nullifier → height spent | grows forever; spends prepay it in fees |
//! | 52 `GHOST_ADDRESSES` | account → its 1,216-byte address | `SetGhostAddress` |
//! | META `ghost` | [`GhostMeta`] | instance, frontier, anchor, pool, pause |

use crate::chain::{check_validity, short, BlockInfo, Genesis, Journal, KeyRecord, Outcome, State, TipRate};
use crate::state::codec::{decode, encode};
use crate::state::{meta, ns, JMap, SubState, TMap, Table};
use crate::types::*;
use anyhow::{anyhow, bail, ensure, Result};
use blockula_ghost::field::{digest_from_bytes, digest_to_bytes};
use blockula_ghost::tree::CAPACITY;
use blockula_ghost::{Frontier, PublicInputs, ValueBalance, VerifyError};
use borsh::{BorshDeserialize, BorshSerialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::LazyLock;

/// The feature flag (upgrade.rs `KNOWN_FEATURES`).
pub const FEATURE: &str = "ghost";
/// Most ghost txs the mempool holds (each is ~135 KiB).
pub const MAX_MEMPOOL_GHOST: usize = 256;
/// An unsigned ghost tx's `fee` and `tip` are whole multiples of this (0.001 FANG). Both are
/// public, so a fee priced to the drop would tell wallets (and the moment they priced it) apart;
/// on a grid, txs priced the canonical way ([`canonical_fee`]) share a handful of values (audit
/// F38). Execution refuses anything else.
pub const FEE_GRID: u64 = DROPS_PER_FANG / 1_000;
/// A queued ghost tx is displaced (replaced by a spend of the same note or, a shield, by its
/// sender's shield at its nonce, or evicted from a full ghost share) only by a tip higher than
/// its own by at least 1/`BUMP_DIVISOR` of it and by at least the base fee of the newcomer's
/// bytes: every displacement pays to relay itself (audit F39, BIP-125's rules 3 and 4).
const BUMP_DIVISOR: u64 = 10;
/// `/api/ghost` lists at most this many retired anchors, newest first.
const LISTED_ROOTS: usize = 64;

/// A pause in force: f + 1 validators signed `GhostPause` under activation `activation` (the
/// feature's `activated_at`), the last of them at block `at`.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Pause {
    pub activation: u64,
    pub at: u64,
    pub voters: Vec<String>,
}

/// `GhostPause` signatures so far, by validator: they count only within one epoch and one
/// activation.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PauseVotes {
    pub epoch: u64,
    pub activation: u64,
    pub voters: BTreeSet<String>,
}

/// Ghost Mode's scalars, one META record.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct GhostMeta {
    /// This chain's identity ([`chain_instance`]), set at genesis: every proof's sighash binds it.
    pub instance: [u8; 32],
    /// The commitment tree's frontier (`blockula_ghost::Frontier`'s canonical encoding).
    pub frontier: Vec<u8>,
    /// The current anchor: the tree's root as of the end of the last block that changed it.
    pub root: [u8; 32],
    /// Drops in the shielded pool (the turnstile).
    pub pool: u64,
    pub pause: Option<Pause>,
    pub votes: Option<PauseVotes>,
}

impl Default for GhostMeta {
    fn default() -> Self {
        let f = Frontier::new();
        Self { instance: [0; 32], frontier: f.to_bytes(), root: digest_to_bytes(&f.root()), pool: 0, pause: None, votes: None }
    }
}

/// Ghost Mode's slice of the state (see the module docs for the namespaces).
#[derive(Clone, Debug, Default)]
pub struct Ghost {
    pub commitments: TMap<u64, [u8; 32]>,
    pub roots: TMap<[u8; 32], u64>,
    pub retired: TMap<u64, [u8; 32]>,
    pub nullifiers: TMap<[u8; 32], u64>,
    pub addresses: TMap<String, Vec<u8>>,
    pub meta: GhostMeta,
}

impl SubState for Ghost {
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let Ghost { commitments, roots, retired, nullifiers, addresses, meta: _ } = self;
        vec![(ns::GHOST_COMMITMENTS, commitments), (ns::GHOST_ROOTS, roots), (ns::GHOST_RETIRED, retired), (ns::GHOST_NULLIFIERS, nullifiers), (ns::GHOST_ADDRESSES, addresses)]
    }

    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let Ghost { commitments, roots, retired, nullifiers, addresses, meta: _ } = self;
        vec![(ns::GHOST_COMMITMENTS, commitments), (ns::GHOST_ROOTS, roots), (ns::GHOST_RETIRED, retired), (ns::GHOST_NULLIFIERS, nullifiers), (ns::GHOST_ADDRESSES, addresses)]
    }

    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        vec![(meta::GHOST, encode(&self.meta))]
    }

    fn load_meta(&mut self, key: &[u8], value: &[u8]) -> Result<bool> {
        if key != meta::GHOST {
            return Ok(false);
        }
        let m: GhostMeta = decode(value)?;
        ensure!(Frontier::from_bytes(&m.frontier).is_some(), "the ghost frontier isn't a canonical frontier");
        self.meta = m;
        Ok(true)
    }
}

/// Undo log for [`Ghost`] (part of `chain::Journal`). Retired anchors change only at the end of
/// a block, which isn't rolled back.
#[derive(Default)]
pub struct GhostJournal {
    pub commitments: JMap<u64, [u8; 32]>,
    pub nullifiers: JMap<[u8; 32], u64>,
    pub addresses: JMap<String, Vec<u8>>,
    pub meta: Option<GhostMeta>,
}

impl GhostJournal {
    pub fn absorb(&mut self, other: GhostJournal) {
        let GhostJournal { commitments, nullifiers, addresses, meta } = other;
        self.commitments.absorb(commitments);
        self.nullifiers.absorb(nullifiers);
        self.addresses.absorb(addresses);
        if self.meta.is_none() {
            self.meta = meta;
        }
    }

    pub fn rollback(self, g: &mut Ghost) {
        let GhostJournal { commitments, nullifiers, addresses, meta } = self;
        commitments.restore(&mut g.commitments);
        nullifiers.restore(&mut g.nullifiers);
        addresses.restore(&mut g.addresses);
        if let Some(m) = meta {
            g.meta = m;
        }
    }
}

fn meta_mut<'a>(state: &'a mut State, j: &mut Journal) -> &'a mut GhostMeta {
    if j.ghost.meta.is_none() {
        j.ghost.meta = Some(state.ghost.meta.clone());
    }
    &mut state.ghost.meta
}

/// This chain's identity, as every proof's sighash binds it: BLAKE3-KDF over what makes a
/// genesis unique (the consensus genesis seed's inputs: chain id, address prefix, creation time
/// and validators). Chains that share a chain id (a testnet started again, a fork) get different
/// instances, so a proof made for one is useless on another.
pub fn chain_instance(g: &Genesis) -> [u8; 32] {
    let mut out = vec![];
    let mut put = |b: &[u8]| {
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    };
    put(g.chain_id.as_bytes());
    put(g.address_prefix.as_bytes());
    put(&g.created_at_ms.to_le_bytes());
    for v in &g.validators {
        put(v.address.as_bytes());
        put(v.public_key.as_bytes());
    }
    blake3::derive_key("blockula ghost v1 chain instance", &out)
}

/// When the feature was last switched on (`None`: it's off).
fn activation(state: &State) -> Option<u64> {
    state.upgrade.features.get(FEATURE).map(|f| f.activated_at)
}

/// The pause in force for the current activation, if any (it may start only next block).
fn pause_of(state: &State) -> Option<&Pause> {
    let a = activation(state)?;
    state.ghost.meta.pause.as_ref().filter(|p| p.activation == a)
}

/// Whether the validators have paused Ghost Mode for blocks at `height`.
pub fn paused(state: &State, height: u64) -> bool {
    pause_of(state).is_some_and(|p| height > p.at)
}

/// f + 1 of `validators`: how many must sign `GhostPause`.
pub fn pause_threshold(validators: usize) -> usize {
    validators.saturating_sub(1) / 3 + 1
}

/// Ghost actions can run at `height`, or why not.
pub fn check_enabled(state: &State, height: u64) -> Result<()> {
    if !crate::upgrade::is_active(state, FEATURE) {
        bail!("Ghost Mode is off: it ships disabled, and only governance can switch it on (ActivateFeature {{ ghost }}), after its audit");
    }
    if let Some(p) = pause_of(state).filter(|_| paused(state, height)) {
        bail!("Ghost Mode is paused: {} validators signed GhostPause (from block {}); governance resumes it by switching the feature off and on again", p.voters.len(), p.at + 1);
    }
    Ok(())
}

/// What leaves the shielded pool (negative: enters it), as the proof must show it: −amount for a
/// shield, the fee for a private transfer, amount + fee for an unshield. Computed in `i128`; the
/// circuit carries at most ±(2^64 − 1).
pub fn value_balance(tx: &Tx) -> Result<ValueBalance> {
    let v: i128 = match &tx.action {
        Action::Shield { amount, .. } => -i128::from(*amount),
        Action::PrivateTransfer { .. } => i128::from(tx.fee),
        Action::Unshield { amount, .. } => i128::from(*amount) + i128::from(tx.fee),
        _ => bail!("not a ghost transfer"),
    };
    ValueBalance::new(v).ok_or_else(|| anyhow!("amount + fee is more than a value balance can carry (2^64 − 1 drops)"))
}

/// The statement a ghost tx's proof must prove (tags 50–52; `None` for other txs), from the tx
/// alone, and the proof.
pub fn statement(tx: &Tx) -> Result<Option<(PublicInputs, &[u8])>> {
    let Some(b) = tx.action.ghost_bundle() else { return Ok(None) };
    let digest = |h: &HexBytes| h.array32().and_then(|a| digest_from_bytes(&a)).ok_or_else(|| anyhow!("a ghost bundle's anchor, nullifiers and commitments are canonical 32-byte digests"));
    let public = PublicInputs {
        anchor: digest(&b.anchor)?,
        nullifiers: [digest(&b.nullifiers[0])?, digest(&b.nullifiers[1])?],
        commitments: [digest(&b.commitments[0])?, digest(&b.commitments[1])?],
        value_balance: value_balance(tx)?,
        sighash: tx.ghost_sighash().ok_or_else(|| anyhow!("no sighash"))?,
    };
    Ok(Some((public, &b.proof.0)))
}

/// Remembered verdicts (see the module docs: the key covers the verifier's whole input).
/// Valid proofs are kept least-recently-used first: the mempool check re-runs every queued ghost
/// tx each block, so their verdicts stay the freshest, and flushing them would take thousands of
/// new valid proofs (~6 ms of verification each) within one block. Failures, which junk makes
/// cheaply (a 1-byte proof fails at decoding in microseconds), are kept apart in a small FIFO,
/// so they never evict a valid tx's verdict (audit F41).
struct Verdicts {
    /// Verified statement → when it was last used (a tick of `clock`).
    ok: HashMap<[u8; 32], u64>,
    /// The same, by last use (oldest first).
    by_use: BTreeMap<u64, [u8; 32]>,
    clock: u64,
    failed: HashMap<[u8; 32], VerifyError>,
    failed_order: VecDeque<[u8; 32]>,
}

const REMEMBERED_VERDICTS: usize = 4_096;
const REMEMBERED_FAILURES: usize = 1_024;

impl Verdicts {
    fn get(&mut self, key: &[u8; 32]) -> Option<Result<(), VerifyError>> {
        if let Some(e) = self.failed.get(key) {
            return Some(Err(*e));
        }
        let used = self.ok.get_mut(key)?;
        self.by_use.remove(used);
        self.clock += 1;
        *used = self.clock;
        self.by_use.insert(self.clock, *key);
        Some(Ok(()))
    }

    fn put(&mut self, key: [u8; 32], verdict: Result<(), VerifyError>) {
        match verdict {
            Ok(()) => {
                if self.ok.contains_key(&key) {
                    return;
                }
                self.clock += 1;
                self.ok.insert(key, self.clock);
                self.by_use.insert(self.clock, key);
                while self.ok.len() > REMEMBERED_VERDICTS {
                    let Some((_, old)) = self.by_use.pop_first() else { break };
                    self.ok.remove(&old);
                }
            }
            Err(e) => {
                if self.failed.insert(key, e).is_none() {
                    self.failed_order.push_back(key);
                }
                while self.failed_order.len() > REMEMBERED_FAILURES {
                    if let Some(old) = self.failed_order.pop_front() {
                        self.failed.remove(&old);
                    }
                }
            }
        }
    }
}

static VERDICTS: LazyLock<parking_lot::Mutex<Verdicts>> = LazyLock::new(|| parking_lot::Mutex::new(Verdicts { ok: HashMap::new(), by_use: BTreeMap::new(), clock: 0, failed: HashMap::new(), failed_order: VecDeque::new() }));

/// Hash of everything `blockula_ghost::verify` reads: the public values (canonical encodings),
/// the circuit it checks against, and the proof bytes.
fn statement_key(public: &PublicInputs, proof: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_derive_key("blockula ghost v1 verified statement");
    for d in std::iter::once(&public.anchor).chain(&public.nullifiers).chain(&public.commitments) {
        h.update(&digest_to_bytes(d));
    }
    h.update(&[public.value_balance.is_negative() as u8]);
    h.update(&public.value_balance.magnitude().to_le_bytes());
    h.update(&public.sighash);
    h.update(&blockula_ghost::CIRCUIT_ID.to_le_bytes());
    h.update(&(proof.len() as u64).to_le_bytes());
    h.update(proof);
    *h.finalize().as_bytes()
}

/// Checks a ghost tx's Action proof against the statement derived from the tx (other txs pass).
/// Needs no state, so callers run it before taking the chain lock; execution runs it again and
/// gets the remembered verdict. ~6 ms when it isn't remembered.
pub fn verify_proof(tx: &Tx) -> Result<()> {
    let Some((public, proof)) = statement(tx)? else { return Ok(()) };
    let key = statement_key(&public, proof);
    let known = VERDICTS.lock().get(&key);
    let verdict = match known {
        Some(v) => v,
        None => {
            let v = blockula_ghost::verify(&public, proof);
            VERDICTS.lock().put(key, v);
            v
        }
    };
    verdict.map_err(|e| anyhow!("the Action proof doesn't verify ({e:?})"))
}

fn check_size(state: &State, tx: &Tx) -> Result<()> {
    let (size, max) = (tx.size(), state.chain_params.ghost_max_tx_bytes);
    if size > max {
        bail!("ghost transaction too large ({size} bytes, max {max}: ghost_max_tx_bytes)");
    }
    Ok(())
}

fn check_instance(state: &State, b: &GhostBundle) -> Result<()> {
    if b.instance.0 != state.ghost.meta.instance {
        bail!("this ghost transaction was made for another chain (instance {}…; this chain's is {}…, see /api/ghost)", hex::encode(&b.instance.0[..b.instance.0.len().min(6)]), hex::encode(&state.ghost.meta.instance[..6]));
    }
    Ok(())
}

/// The ghost share of one block (consensus `propose` and `verify`): ghost txs (tags 50–52) up to
/// `ghost_max_tx_bytes` each and `ghost_max_block_bytes` together, as the block's fee slot fixed
/// them `econ::FEE_LAG` blocks ahead (so proposers within the lag and every verifier agree on
/// them, whenever a `ParamChange` executes). That bounds the proofs a block makes every node
/// verify (~7 per MiB, ~45 ms of one core).
#[derive(Clone, Copy, Debug)]
pub struct BlockBudget {
    max_tx: u64,
    left: u64,
}

impl BlockBudget {
    /// The budget of the block whose fee slot is `slot` (none: no ghost txs).
    pub fn new(slot: Option<&crate::econ::FeeSlot>) -> Self {
        slot.map_or(Self { max_tx: 0, left: 0 }, |s| Self { max_tx: s.ghost_max_tx_bytes, left: s.ghost_max_block_bytes })
    }

    /// Counts a tx of `bytes` wire bytes if it's a ghost tx (others don't use this budget). On
    /// `Err` the budget is unchanged.
    pub fn take(&mut self, tx: &Tx, bytes: usize) -> Result<()> {
        if tx.action.ghost_bundle().is_none() {
            return Ok(());
        }
        let b = bytes as u64;
        if b > self.max_tx {
            bail!("ghost tx of {b} bytes is over ghost_max_tx_bytes ({})", self.max_tx);
        }
        if b > self.left {
            bail!("the block's ghost txs would exceed ghost_max_block_bytes");
        }
        self.left -= b;
        Ok(())
    }
}

/// The admission checks of the signed ghost actions (`State::admit` calls this last): Ghost Mode
/// on and unpaused, and for a shield its size, chain instance and proof. A tx failing them can't
/// be included at all (nobody is charged), like one with a bad nonce.
pub fn admit(state: &State, tx: &Tx, height: u64) -> Result<()> {
    if !tx.action.is_ghost() {
        return Ok(());
    }
    check_enabled(state, height)?;
    if let Some(b) = tx.action.ghost_bundle() {
        check_size(state, tx)?;
        check_instance(state, b)?;
        verify_proof(tx)?;
    }
    Ok(())
}

fn frontier(state: &State) -> Result<Frontier> {
    Frontier::from_bytes(&state.ghost.meta.frontier).ok_or_else(|| anyhow!("the ghost frontier record is corrupt"))
}

fn bytes32(b: &HexBytes) -> Result<[u8; 32]> {
    b.array32().ok_or_else(|| anyhow!("a ghost bundle's digests are 32 bytes"))
}

/// Whether `anchor` may anchor spends at `height`: the current root, or a root replaced at most
/// `ghost_anchor_window` blocks ago.
fn anchor_usable(state: &State, anchor: &[u8; 32], height: u64) -> bool {
    *anchor == state.ghost.meta.root || state.ghost.roots.get(anchor).is_some_and(|&replaced| height <= replaced.saturating_add(state.chain_params.ghost_anchor_window))
}

/// The stateful spend checks: a recent anchor, unspent nullifiers, room for two leaves.
fn check_spends(state: &State, b: &GhostBundle, height: u64) -> Result<()> {
    let anchor = bytes32(&b.anchor)?;
    if !anchor_usable(state, &anchor, height) {
        bail!("anchor {}… isn't a recent root of the commitment tree (a root stays usable for {} blocks after a newer one replaces it): prove against a current root", &hex::encode(anchor)[..12], state.chain_params.ghost_anchor_window);
    }
    for nf in &b.nullifiers {
        if let Some(at) = state.ghost.nullifiers.get(&bytes32(nf)?) {
            bail!("note already spent (nullifier {}… at block {at})", &hex::encode(&nf.0)[..12]);
        }
    }
    if frontier(state)?.size() + 2 > CAPACITY {
        bail!("the commitment tree is full: the pool is frozen until it migrates");
    }
    Ok(())
}

/// Spends the bundle's nullifiers and appends its two output commitments (dummies too).
fn record(state: &mut State, j: &mut Journal, b: &GhostBundle, height: u64) -> Result<()> {
    for nf in &b.nullifiers {
        let nf = bytes32(nf)?;
        j.ghost.nullifiers.touch(&state.ghost.nullifiers, &nf);
        state.ghost.nullifiers.insert(nf, height);
    }
    let mut f = frontier(state)?;
    for cm in &b.commitments {
        let cm = bytes32(cm)?;
        let digest = digest_from_bytes(&cm).ok_or_else(|| anyhow!("a note commitment isn't canonical"))?;
        let position = u64::from(f.append(digest).map_err(|_| anyhow!("the commitment tree is full"))?);
        j.ghost.commitments.touch(&state.ghost.commitments, &position);
        state.ghost.commitments.insert(position, cm);
    }
    meta_mut(state, j).frontier = f.to_bytes();
    Ok(())
}

/// The signed ghost actions (tags 50, 53, 54), after [`admit`]: their action phase (a failure is
/// charged the fee, as for any signed tx).
pub(crate) fn apply(state: &mut State, j: &mut Journal, tx: &Tx, sender: &str, signer: &str, key: &KeyRecord, height: u64) -> Result<()> {
    // Session keys never get here (they only send transfers and claims), and they mustn't:
    // shielded funds have no spend limit.
    if key.role != KeyRole::Owner {
        bail!("only owner keys can use Ghost Mode");
    }
    match &tx.action {
        Action::Shield { amount, bundle } => {
            let (cap, verify_fee) = (state.chain_params.ghost_pool_cap, state.chain_params.ghost_verify_fee);
            let pool = state.ghost.meta.pool.checked_add(*amount).filter(|p| *p <= cap).ok_or_else(|| anyhow!("the shielded pool holds {} and may hold at most {} (ghost_pool_cap)", fmt_fang(state.ghost.meta.pool), fmt_fang(cap)))?;
            check_spends(state, bundle, height)?;
            let cost = amount.checked_add(verify_fee).ok_or_else(|| anyhow!("amount overflows"))?;
            state.j_debit(j, sender, cost)?;
            state.j_burn(j, verify_fee);
            record(state, j, bundle, height)?;
            meta_mut(state, j).pool = pool;
        }
        Action::SetGhostAddress { address } => {
            j.ghost.addresses.touch(&state.ghost.addresses, sender);
            state.ghost.addresses.insert(sender.to_string(), address.0.clone());
        }
        Action::GhostPause {} => pause_vote(state, j, sender, signer, height)?,
        _ => bail!("not a signed ghost action"),
    }
    Ok(())
}

/// `GhostPause`: one validator's signature; at f + 1 of the epoch's active set the pool pauses
/// from the next block.
fn pause_vote(state: &mut State, j: &mut Journal, sender: &str, signer: &str, height: u64) -> Result<()> {
    let epoch = state.params.epoch_of(height);
    let validator = crate::staking::validator_for_signer(state, sender, signer, epoch).ok_or_else(|| anyhow!("only a validator in epoch {epoch}'s active set (its consensus key or its owner) can pause Ghost Mode"))?;
    let activation = activation(state).ok_or_else(|| anyhow!("Ghost Mode is off"))?;
    if let Some(p) = pause_of(state) {
        bail!("Ghost Mode is already paused (from block {})", p.at + 1);
    }
    let needed = pause_threshold(state.active_sets.get(&epoch).map_or(0, |s| s.len()));
    let m = meta_mut(state, j);
    let mut votes = m.votes.take().filter(|v| v.epoch == epoch && v.activation == activation).unwrap_or(PauseVotes { epoch, activation, voters: BTreeSet::new() });
    if !votes.voters.insert(validator.clone()) {
        bail!("validator {} already signed this epoch's pause", short(&validator));
    }
    if votes.voters.len() >= needed {
        m.pause = Some(Pause { activation, at: height, voters: votes.voters.into_iter().collect() });
    } else {
        m.votes = Some(votes);
    }
    Ok(())
}

/// What creating an unshield's recipient account burns (`account_creation_fee`, DEC-6), or 0.
fn opening_fee(state: &State, tx: &Tx) -> u64 {
    match &tx.action {
        Action::Unshield { to, .. } => state.creation_fee_for(to),
        _ => 0,
    }
}

/// What an unsigned tx's `fee` must cover: the base fee, its tip, `ghost_verify_fee` and, for an
/// unshield to an address with no account yet, `account_creation_fee` (audit F13: the recipient
/// account is opened like any other, and the creation fee is burned with the rest of the fee).
/// `(base, opening, floor)`, `None` on overflow.
fn unsigned_floor(state: &State, tx: &Tx, height: u64) -> Option<(u64, u64, u64)> {
    let base = crate::econ::base_fee(state, height, tx.size());
    let opening = opening_fee(state, tx);
    let floor = base.checked_add(tx.tip)?.checked_add(state.chain_params.ghost_verify_fee)?.checked_add(opening)?;
    Some((base, opening, floor))
}

/// The canonical `fee` for an unsigned ghost tx with no tip (GHOST-MODE.md section 8, "Fees";
/// `/api/ghost` serves it): the suggested base fee (`econ::max_fee`, which covers any block in
/// the next `econ::FEE_LAG`) for the largest ghost tx (`ghost_max_tx_bytes`: the fee is in the
/// sighash, so it's fixed before the proof, and with it the size, is known), plus
/// `ghost_verify_fee`, plus `account_creation_fee` when it unshields to a new account, rounded up
/// to [`FEE_GRID`]. Every wallet that prices this way at the same time pays the same.
pub fn canonical_fee(state: &State, opens_account: bool) -> u64 {
    let opening = if opens_account { state.chain_params.account_creation_fee } else { 0 };
    let fee = crate::econ::max_fee(state, state.chain_params.ghost_max_tx_bytes).saturating_add(state.chain_params.ghost_verify_fee).saturating_add(opening);
    fee.div_ceil(FEE_GRID).saturating_mul(FEE_GRID)
}

/// `State::apply_j` for an unsigned ghost tx (`PrivateTransfer`, `Unshield`): every check first,
/// then the effects. There's no account to charge, so it either applies in full or can't be
/// included (`Err`, state untouched); it never fails with a fee charged.
pub(crate) fn apply_unsigned(state: &mut State, j: &mut Journal, tx: &Tx, chain_id: &str, proposer: &str, height: u64, full_checks: bool) -> Result<Outcome> {
    if tx.chain_id != chain_id {
        bail!("wrong chain_id: expected {chain_id}");
    }
    if full_checks {
        tx.check_format()?;
    }
    tx.check_prefix(state.params.prefix())?;
    check_validity(tx, height, state.chain_params.max_tx_validity_blocks)?;
    check_enabled(state, height)?;
    let b = tx.action.ghost_bundle().ok_or_else(|| anyhow!("not an unsigned ghost transaction"))?;
    check_size(state, tx)?;
    check_instance(state, b)?;
    if !tx.fee.is_multiple_of(FEE_GRID) || !tx.tip.is_multiple_of(FEE_GRID) {
        bail!("an unsigned ghost tx's fee and tip are whole multiples of {FEE_GRID} drops (0.001 FANG), so they don't set wallets apart: price it with ghost::canonical_fee (fee {}, tip {})", tx.fee, tx.tip);
    }
    let (base, opening, floor) = unsigned_floor(state, tx, height).ok_or_else(|| anyhow!("fee overflows"))?;
    // The same tip cap as signed txs (`tip_cap_multiple` × the base fee).
    crate::econ::check_tip(state, tx.tip, base, height)?;
    if tx.fee < floor {
        let opens = if opening > 0 { format!(", account_creation_fee {opening} (it opens the recipient's account)") } else { String::new() };
        bail!("fee too low: an unsigned ghost tx pays exactly its fee (it's in the proof's value balance), which must cover the base fee {base}, the tip {}, ghost_verify_fee {}{opens} ({floor} drops), but it's {}", tx.tip, state.chain_params.ghost_verify_fee, tx.fee);
    }
    let leaving = value_balance(tx)?;
    if leaving.is_negative() {
        bail!("an unsigned ghost tx can't add value to the pool");
    }
    let pool = state.ghost.meta.pool.checked_sub(leaving.magnitude()).ok_or_else(|| anyhow!("turnstile: {} would leave the shielded pool, which holds {}", fmt_fang(leaving.magnitude()), fmt_fang(state.ghost.meta.pool)))?;
    let payee = match &tx.action {
        Action::Unshield { to, amount, .. } => {
            let to = state.resolve(to)?;
            if !state.accounts.contains_key(&to) && *amount < MIN_NEW_ACCOUNT {
                bail!("unshielding to a new account needs at least {}", fmt_fang(MIN_NEW_ACCOUNT));
            }
            Some((to, *amount))
        }
        _ => None,
    };
    check_spends(state, b, height)?;
    verify_proof(tx)?;
    let effects = |state: &mut State, j: &mut Journal| -> Result<()> {
        meta_mut(state, j).pool = pool;
        if let Some((to, amount)) = &payee {
            state.j_credit(j, to, *amount);
        }
        state.j_credit(j, proposer, tx.tip);
        // The rest of the fee is burned: the base fee, the verify fee, a new account's creation
        // fee, and anything above them.
        state.j_burn(j, tx.fee - tx.tip);
        record(state, j, b, height)
    };
    let mut gj = Journal::default();
    match effects(state, &mut gj) {
        Ok(()) => {
            j.absorb_pub(gj);
            Ok(Outcome::Applied)
        }
        Err(e) => {
            state.rollback(gj);
            Err(e)
        }
    }
}

/// End of every block, after governance's step: the tree's new root (if this block's ghost txs
/// changed it) becomes the anchor, the root it replaces stays usable for `ghost_anchor_window`
/// blocks, and older ones are dropped.
pub fn end_block(state: &mut State, b: &BlockInfo) {
    let h = b.height;
    if let Some(f) = Frontier::from_bytes(&state.ghost.meta.frontier) {
        let root = digest_to_bytes(&f.root());
        if root != state.ghost.meta.root {
            let old = std::mem::replace(&mut state.ghost.meta.root, root);
            state.ghost.roots.insert(old, h);
            state.ghost.retired.insert(h, old);
        }
    }
    // A root replaced at `at` is usable through `at + window`, so not from `h + 1` on once
    // `at + window ≤ h`.
    let window = state.chain_params.ghost_anchor_window;
    while let Some((at, root)) = state.ghost.retired.iter().next().map(|(a, r)| (*a, *r)) {
        if at.saturating_add(window) > h {
            break;
        }
        state.ghost.retired.remove(&at);
        state.ghost.roots.remove(&root);
    }
}

/// The shielded pool (an escrow: its FANG is in the supply but in no balance).
pub fn escrow(state: &State) -> u128 {
    u128::from(state.ghost.meta.pool)
}

/// Ghost Mode's consistency rules (part of `State::check_invariants`, which runs at the end of
/// a block): the commitments are exactly the frontier's leaves, the anchor is the frontier's
/// root, retired anchors are filed both ways, addresses have the right size.
pub fn check_invariants(state: &State) -> Result<()> {
    let g = &state.ghost;
    let f = frontier(state)?;
    let last = g.commitments.keys().next_back().map_or(0, |p| p + 1);
    ensure!(g.commitments.len() as u64 == f.size() && last == f.size(), "the ghost commitments don't match the frontier's {} leaves", f.size());
    ensure!(digest_to_bytes(&f.root()) == g.meta.root, "the ghost anchor isn't the frontier's root");
    ensure!(g.roots.len() == g.retired.len() && g.retired.iter().all(|(h, r)| g.roots.get(r) == Some(h)), "retired ghost anchors are misfiled");
    ensure!(!g.roots.contains_key(&g.meta.root), "the current ghost anchor is filed as retired");
    for (account, a) in g.addresses.iter() {
        ensure!(a.len() == GHOST_ADDRESS_BYTES && crate::crypto::is_address(account), "{account}'s ghost address is malformed");
    }
    ensure!(g.meta.votes.as_ref().is_none_or(|v| !v.voters.is_empty()) && g.meta.pause.as_ref().is_none_or(|p| !p.voters.is_empty()), "an empty ghost pause record");
    Ok(())
}

/// A Ghost Mode address's fingerprint (`xxxx-xxxx-xxxx-xxxx-xxxx`), what people compare.
pub fn fingerprint(address: &[u8]) -> Option<String> {
    blockula_ghost::Address::from_bytes(address).map(|a| a.fingerprint())
}

/// Plain-English summary of a ghost tx (see `State::describe_with`). Unsigned ones name no one:
/// "👻 a ghost transaction happened".
pub fn describe(state: &State, tx: &Tx, from: &str, primary_name: &dyn Fn(&str) -> Option<String>) -> String {
    match &tx.action {
        Action::Shield { amount, .. } => format!("{from} shielded {} into Ghost Mode 👻", fmt_fang(*amount)),
        Action::PrivateTransfer { .. } => format!("👻 a ghost transaction happened (fee {})", fmt_fang(tx.fee)),
        Action::Unshield { to, amount, .. } => {
            let to = match state.resolve(to) {
                Ok(addr) if crate::crypto::is_address(to) => primary_name(&addr).unwrap_or_else(|| short(&addr)),
                _ => format!("{}{NAME_SUFFIX}", normalize_name(to).unwrap_or(to.clone())),
            };
            format!("👻 {} left Ghost Mode for {to} (fee {})", fmt_fang(*amount), fmt_fang(tx.fee))
        }
        Action::SetGhostAddress { address } => format!("{from} published a Ghost Mode address (fingerprint {})", fingerprint(&address.0).unwrap_or_else(|| "invalid".into())),
        Action::GhostPause {} => format!("{from} signed a pause of Ghost Mode (it pauses once f + 1 of the epoch's validators have)"),
        _ => String::new(),
    }
}

/// A ghost transfer as the API shows it: the bundle without its ~130 KiB proof (its size and
/// BLAKE3 hash instead), so blocks of ghost txs aren't megabytes of hex. Wallets scan the
/// commitments and ciphertexts, which stay. `None` for other actions.
pub fn action_json(action: &Action) -> Option<Value> {
    let b = action.ghost_bundle()?;
    let bundle = json!({
        "instance": b.instance,
        "anchor": b.anchor,
        "nullifiers": b.nullifiers,
        "commitments": b.commitments,
        "enc": b.enc,
        "out": b.out,
        "proof_bytes": b.proof.0.len(),
        "proof_hash": blake3::hash(&b.proof.0).to_hex().to_string(),
    });
    Some(match action {
        Action::Shield { amount, .. } => json!({ "type": "shield", "amount": amount.to_string(), "bundle": bundle }),
        Action::Unshield { to, amount, .. } => json!({ "type": "unshield", "to": to, "amount": amount.to_string(), "bundle": bundle }),
        _ => json!({ "type": "private_transfer", "bundle": bundle }),
    })
}

/// `/api/ghost`: whether the pool is on or paused, its balance and cap, the tree, recent anchors,
/// the limits and the chain instance wallets bind proofs to. `height`: the executed height.
pub fn status_json(state: &State, height: u64) -> Value {
    let (g, p) = (&state.ghost, &state.chain_params);
    let epoch = state.params.epoch_of(height + 1);
    let pause = pause_of(state);
    let votes = g.meta.votes.as_ref().filter(|v| Some(v.activation) == activation(state) && v.epoch == epoch);
    let roots: Vec<Value> = g.retired.iter().rev().take(LISTED_ROOTS).map(|(at, r)| json!({ "root": hex::encode(r), "replaced_at": at, "usable_until": at.saturating_add(p.ghost_anchor_window) })).collect();
    json!({
        "active": activation(state).is_some(),
        "activated_at": activation(state),
        "paused": paused(state, height + 1),
        "paused_from": pause.map(|p| p.at + 1),
        "paused_by": pause.map(|p| p.voters.clone()),
        "pause_votes": votes.map_or(0, |v| v.voters.len()),
        "pause_threshold": pause_threshold(state.active_sets.get(&epoch).map_or(0, |s| s.len())),
        "audited": false,
        "pool": g.meta.pool.to_string(),
        "pool_display": fmt_fang(g.meta.pool),
        "pool_cap": p.ghost_pool_cap.to_string(),
        "pool_cap_display": fmt_fang(p.ghost_pool_cap),
        "frontier_size": frontier(state).map(|f| f.size()).unwrap_or(0),
        "root": hex::encode(g.meta.root),
        "recent_roots": roots,
        "retired_roots": g.retired.len(),
        "anchor_window": p.ghost_anchor_window,
        "nullifiers": g.nullifiers.len(),
        "instance": hex::encode(g.meta.instance),
        "circuit_id": blockula_ghost::CIRCUIT_ID,
        "max_tx_bytes": p.ghost_max_tx_bytes,
        "max_block_bytes": p.ghost_max_block_bytes,
        "verify_fee": p.ghost_verify_fee.to_string(),
        "fee_grid": FEE_GRID.to_string(),
        "canonical_fee": canonical_fee(state, false).to_string(),
        "canonical_fee_new_account": canonical_fee(state, true).to_string(),
    })
}

/// The least tip that displaces queued txs tipping `tip` together (see [`BUMP_DIVISOR`]): `tip`
/// plus the larger of a tenth of it and `relay` (the base fee of the newcomer's bytes), on the
/// fee grid.
fn displacing_tip(tip: u64, relay: u64) -> u64 {
    let bump = (tip / BUMP_DIVISOR).max(relay).max(1);
    tip.saturating_add(bump).div_ceil(FEE_GRID).saturating_mul(FEE_GRID)
}

/// Whether `tx` displaces what tips `cost` together (one queued tx, or a shield with what its
/// sender queued behind it): a tip that clears [`displacing_tip`], and a higher tip per byte.
fn displaces(tx: &Tx, cost: TipRate, relay: u64) -> bool {
    tx.tip >= displacing_tip(cost.tip, relay) && TipRate::of(tx) > cost
}

/// Review of F36, round 3: a queued `Shield` is always its sender's last queued tx, so a full
/// ghost share can always give it up for a higher tip (no tx of its sender waits behind it on
/// the nonce it would free) and nobody can pin the share with shields covered by a tx of their
/// own. `Chain::submit_tx` calls this for every tx from a sender with `shield` queued (at
/// `tx.nonce` or below; `last`: whether it's the sender's last queued tx, which the node keeps
/// so): a tx at a later nonce is refused (wait until the shield is included, or replace it),
/// and one at its nonce replaces it if it's a shield (from the same account, so nothing else
/// of anyone's goes) that displaces it. `relay`: the base fee of `tx`'s bytes; `hint`: that
/// of the shield's, the replacement's size a refusal names a tip for.
pub fn behind_shield(shield: &Tx, last: bool, tx: &Tx, relay: u64, hint: u64) -> Result<()> {
    let n = shield.nonce;
    if tx.nonce > n || !last {
        bail!("your shield (nonce {n}) is waiting in the mempool, and nothing queues behind a shield (so a full ghost share can always give it up for a higher tip): wait until it's included, or replace it with a shield at nonce {n} tipping at least {} drops", displacing_tip(shield.tip, hint));
    }
    if !matches!(tx.action, Action::Shield { .. }) {
        bail!("only a shield replaces your queued shield (nonce {n}): wait until it's included, then send this at nonce {}", n + 1);
    }
    if !displaces(tx, TipRate::of(shield), relay) {
        bail!("to replace your queued shield (nonce {n}), tip at least {} drops (and the fee to cover it)", displacing_tip(shield.tip, relay));
    }
    Ok(())
}

/// Mempool admission of a ghost tx (`Chain::submit_tx`) against the queued txs: one queued spend
/// per note, and at most [`MAX_MEMPOOL_GHOST`] ghost txs. A tx spending a nullifier a queued tx
/// spends replaces it, and a full ghost share gives up its lowest tip per byte (the newest
/// among equals, like `Chain::eviction_victim`; only when `evict`), only for a tip that clears
/// [`displacing_tip`]: a higher tip always gets in (audit F36), and every displacement pays the
/// relay of its own bytes (F39). Any queued ghost tx but the newcomer's sender's own can go (its
/// own would leave the newcomer behind a nonce gap, and its nonce is taken: review of F36).
/// A signed one (a `Shield`) goes with its sender's later txs, if any (they'd wait on a nonce
/// gap: `Chain::displace` drops them), and the newcomer outbids them together. The node never
/// queues a tx behind a shield ([`behind_shield`]), so that's the shield alone; round 2 kept
/// such shields instead, and 256 of them, each covered by a cheap tx of its sender, locked the
/// share against any tip (review of F36, round 3). `own`: the index of the sender's queued
/// shield `tx` replaces ([`behind_shield`]), left out (of the count, the notes and what can go).
/// `relay`: the base fee of `tx`'s bytes; `cap`: the most it may tip (`econ::max_tip`).
/// Returns the ids it displaces, with why.
pub fn mempool_admit(queued: &[Tx], ids: &[String], tx: &Tx, own: Option<usize>, relay: u64, cap: u64, evict: bool) -> Result<Vec<(String, &'static str)>> {
    let Some(b) = tx.action.ghost_bundle() else { return Ok(vec![]) };
    let kept = |i: usize| Some(i) != own;
    // Each sender's last queued tx (arrival order is each sender's nonce order).
    let mut last: HashMap<&str, usize> = HashMap::new();
    for (i, q) in queued.iter().enumerate().filter(|(i, q)| kept(*i) && !q.is_unsigned()) {
        last.insert(q.from.as_str(), i);
    }
    // What giving queued tx `i` up takes out: it, and if it's signed and not its sender's last,
    // the sender's later txs too.
    let cost = |i: usize, q: &Tx| -> TipRate {
        if q.is_unsigned() || last.get(q.from.as_str()) == Some(&i) {
            return TipRate::of(q);
        }
        let then = queued.iter().enumerate().skip(i).filter(|(j, t)| kept(*j) && !t.is_unsigned() && t.from == q.from);
        then.fold(TipRate { tip: 0, size: 0 }, |c, (_, t)| TipRate { tip: c.tip.saturating_add(t.tip), size: c.size.saturating_add(t.size()) })
    };
    let mut ghosts = 0;
    let mut replaces = vec![];
    // The lowest tip per byte queued that may go (newest among equals).
    let mut lowest: Option<(TipRate, &String)> = None;
    for (i, (q, id)) in queued.iter().zip(ids).enumerate().filter(|(i, _)| kept(*i)) {
        let Some(qb) = q.action.ghost_bundle() else { continue };
        ghosts += 1;
        let c = cost(i, q);
        if (q.is_unsigned() || q.from != tx.from) && lowest.as_ref().is_none_or(|(l, _)| c <= *l) {
            lowest = Some((c, id));
        }
        if qb.nullifiers.iter().any(|n| b.nullifiers.contains(n)) {
            if !q.is_unsigned() && q.from == tx.from {
                bail!("your queued transaction with nonce {} already spends one of these notes", q.nonce);
            }
            if !displaces(tx, c, relay) {
                bail!("a queued transaction already spends one of these notes; to replace it, pay a tip of at least {} drops (its {} plus the relay of yours) and the fee to cover it", displacing_tip(c.tip, relay), c.tip);
            }
            replaces.push((id.clone(), "replaced by a transaction spending the same note with a higher tip"));
        }
    }
    if replaces.is_empty() && ghosts >= MAX_MEMPOOL_GHOST {
        // An unsigned tx tips a whole multiple of the grid (review of fix-fees-2).
        let cap = if tx.is_unsigned() { cap / FEE_GRID * FEE_GRID } else { cap };
        return match lowest {
            Some((c, id)) if evict && displaces(tx, c, relay) => Ok(vec![(id.clone(), "evicted from the mempool's full ghost share by a transaction with a higher tip")]),
            // Review of fix-fees: never ask for a tip past the cap.
            Some((c, _)) if displacing_tip(c.tip, relay) > cap => bail!("the mempool holds its maximum of {MAX_MEMPOOL_GHOST} ghost transactions, and they tip more than this one may (at most {cap} drops); try again shortly"),
            Some((c, _)) => bail!("the mempool holds its maximum of {MAX_MEMPOOL_GHOST} ghost transactions; resubmit with a tip of at least {} drops (and the fee to cover it), or try again shortly", displacing_tip(c.tip, relay)),
            None => bail!("the mempool holds its maximum of {MAX_MEMPOOL_GHOST} ghost transactions; try again shortly"),
        };
    }
    Ok(replaces)
}

#[cfg(test)]
pub(crate) mod tests;
