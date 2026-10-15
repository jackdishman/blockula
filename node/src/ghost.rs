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
//!   tip and `ghost_verify_fee`: the tip goes to the proposer, the rest is burned.
//! - **Anchors.** At the end of a block that changed the tree, its root becomes the anchor; the
//!   root it replaces stays usable for `ghost_anchor_window` blocks.
//! - **Pause.** `GhostPause` from f + 1 distinct validators of the current epoch's active set
//!   pauses the pool from the next block (f + 1 can halt the chain anyway, so this adds no power,
//!   and it can only stop the pool, never move funds). A pause belongs to the activation it was
//!   voted under, so governance resumes the pool by switching the feature off and on again.
//! - **Limits.** Ghost txs (50–52) up to `ghost_max_tx_bytes`, at most `ghost_max_block_bytes` of
//!   them per block ([`BlockBudget`], consensus.rs; both as the block's fee slot fixed them
//!   `econ::FEE_LAG` blocks ahead), at most [`MAX_MEMPOOL_GHOST`] queued, and a
//!   queued spend of a note is replaced only by a higher tip per byte ([`mempool_admit`]).
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
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::LazyLock;

/// The feature flag (upgrade.rs `KNOWN_FEATURES`).
pub const FEATURE: &str = "ghost";
/// Most ghost txs the mempool holds (each is ~135 KiB).
pub const MAX_MEMPOOL_GHOST: usize = 256;
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
struct Verdicts {
    by_key: HashMap<[u8; 32], Result<(), VerifyError>>,
    order: VecDeque<[u8; 32]>,
}

const REMEMBERED_VERDICTS: usize = 4_096;

static VERDICTS: LazyLock<parking_lot::Mutex<Verdicts>> = LazyLock::new(|| parking_lot::Mutex::new(Verdicts { by_key: HashMap::new(), order: VecDeque::new() }));

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
    let known = VERDICTS.lock().by_key.get(&key).copied();
    let verdict = match known {
        Some(v) => v,
        None => {
            let v = blockula_ghost::verify(&public, proof);
            let mut cache = VERDICTS.lock();
            if cache.by_key.insert(key, v).is_none() {
                cache.order.push_back(key);
            }
            while cache.order.len() > REMEMBERED_VERDICTS {
                if let Some(old) = cache.order.pop_front() {
                    cache.by_key.remove(&old);
                }
            }
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

/// The base fee, tip and verify fee an unsigned tx's `fee` must cover (`None` on overflow).
fn unsigned_floor(state: &State, tx: &Tx, height: u64) -> Option<(u64, u64)> {
    let base = crate::econ::base_fee(state, height, tx.size());
    let floor = base.checked_add(tx.tip)?.checked_add(state.chain_params.ghost_verify_fee)?;
    Some((base, floor))
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
    let (base, floor) = unsigned_floor(state, tx, height).ok_or_else(|| anyhow!("fee overflows"))?;
    if tx.fee < floor {
        bail!("fee too low: an unsigned ghost tx pays exactly its fee (it's in the proof's value balance), which must cover the base fee {base}, the tip {} and ghost_verify_fee {} ({floor} drops), but it's {}", tx.tip, state.chain_params.ghost_verify_fee, tx.fee);
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
    })
}

/// Mempool admission of a ghost tx (`Chain::submit_tx`) against the queued txs: at most
/// [`MAX_MEMPOOL_GHOST`] ghost txs, and one queued spend per note: a tx spending a nullifier a
/// queued tx spends replaces it only with a higher tip per byte. Returns the ids it replaces.
pub fn mempool_admit(queued: &[Tx], ids: &[String], tx: &Tx) -> Result<Vec<String>> {
    let Some(b) = tx.action.ghost_bundle() else { return Ok(vec![]) };
    let mut ghosts = 0;
    let mut replaces = vec![];
    for (q, id) in queued.iter().zip(ids) {
        let Some(qb) = q.action.ghost_bundle() else { continue };
        ghosts += 1;
        if qb.nullifiers.iter().any(|n| b.nullifiers.contains(n)) {
            if TipRate::of(tx) <= TipRate::of(q) {
                bail!("a queued transaction already spends one of these notes; to replace it, pay a higher tip per byte than its {} drops for {} bytes", q.tip, q.size());
            }
            replaces.push(id.clone());
        }
    }
    if replaces.is_empty() && ghosts >= MAX_MEMPOOL_GHOST {
        bail!("the mempool holds its maximum of {MAX_MEMPOOL_GHOST} ghost transactions; try again shortly");
    }
    Ok(replaces)
}

#[cfg(test)]
pub(crate) mod tests;
