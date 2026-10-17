//! Validator-set history with handoff proofs, so a node can learn who the validators are at any
//! epoch without replaying the chain (used by snapshot/fast sync).
//!
//! When an epoch boundary changes the active set, members of the outgoing set sign a handoff:
//! `(genesis hash, chain_id, epoch, hash(old set), hash(new set))` with their consensus key
//! (FIPS 204 context `HANDOFF_CONTEXT`). The genesis hash binds it to one chain instance: a
//! relaunch that keeps the chain id and the validators' keys shares no handoff with the old chain
//! (F02, like votes and certificates). A quorum (2f+1) of the outgoing set makes the new set trusted. Epochs
//! where the set doesn't change need no proof: hiding a change can't make a joiner accept a fake
//! state, only fail to verify (it then tries another peer).
//!
//! Handoffs can be signed at any time, also retroactively, and nothing slashes a handoff. So keys
//! of a set that has since unbonded can hand off to a made-up set, and a joiner that trusts only
//! the genesis would follow it. That's weak subjectivity: joiners also pin a recent validator set
//! (a checkpoint's `set_hash`, `checkpoints.rs`), which every later set must descend from, and
//! refuse roots older than the weak-subjectivity period.

use crate::chain::Genesis;
use crate::pq::{PrivateKey, PublicKey, Signature};
use anyhow::{anyhow, bail, ensure, Result};
use commonware_codec::DecodeExt as _;
use commonware_cryptography::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const HANDOFF_CONTEXT: &[u8] = b"blockula/epoch-handoff/v1";
/// The handoff message format a registry's proofs are signed under: 2 binds the outgoing set
/// too, so a signature is a quorum share for exactly one transition (F08); 3 binds the chain
/// instance (its genesis hash), so no other genesis under the same chain id accepts it (F02). A
/// registry saved by an older binary has its proofs dropped on load. Validators sign again only
/// the changes their state still holds both sides of (`epoch_sets_kept`), so `Chain::open` then
/// starts the registry at the oldest set the state holds: a joiner trusting an older root needs a
/// newer checkpoint (review-fix-sync-4; relaunched chains start at this format and never upgrade).
pub const HANDOFF_FORMAT: u32 = 3;
/// What one `/api/epochs` page aims for (it always holds at least one set change). Pages, not
/// the whole history, are what nodes exchange and joiners fetch (F10, F19, F26).
pub const PAGE_BYTES: usize = 2 << 20;
/// The largest page a node reads (a page holds one change past `PAGE_BYTES` at most: a set of a
/// few hundred keys and a quorum of its signatures).
pub const MAX_PAGE_BYTES: usize = 16 << 20;
/// The most a joiner assembles from pages before it gives up on a peer.
pub const MAX_HISTORY_BYTES: usize = 512 << 20;
/// A node trims history older than its window (`chain::registry_keep_epochs`) only once the
/// registry passes this size, so a small history stays whole (a joiner trusting an older root,
/// e.g. the genesis on a young chain, still finds it) and a big one stays bounded.
pub const KEEP_BYTES: usize = 8 << 20;

/// Byzantine quorum for `n` validators (n − f, f = ⌊(n−1)/3⌋).
pub fn quorum(n: usize) -> usize {
    n - n.saturating_sub(1) / 3
}

/// The hash a handoff signs and a checkpoint pins: of the sorted consensus keys (hex).
pub fn set_hash(keys: &[String]) -> [u8; 32] {
    crate::crypto::hash(format!("BLOCKULA_SET_V1:{}", keys.join(",")).as_bytes())
}

/// The chain instance a handoff is signed for: its id and its genesis hash (the digest of its
/// genesis block, `checkpoints::genesis_hash`). A relaunch under the same id is another
/// instance: none of its handoffs verify on the old one, nor the old one's on it (F02).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub chain_id: String,
    pub genesis_hash: [u8; 32],
}

impl Instance {
    pub fn new(chain_id: &str, genesis_hash: [u8; 32]) -> Self {
        Self { chain_id: chain_id.to_string(), genesis_hash }
    }

    /// `g`'s instance (computes its genesis hash).
    pub fn of(g: &Genesis) -> Self {
        Self::new(&g.chain_id, crate::consensus::genesis_block(g).digest_bytes())
    }
}

/// What the outgoing set (`prev`) signs when the set becomes `keys` at `epoch` on chain instance
/// `at`: `BLOCKULA_HANDOFF_V3 ‖ genesis hash ‖ len(chain id) (u16 LE) ‖ chain id ‖ epoch (u64 LE)
/// ‖ set_hash(prev) ‖ set_hash(keys)`.
pub fn handoff_message(at: &Instance, epoch: u64, prev: &[String], keys: &[String]) -> Vec<u8> {
    let chain_id = &at.chain_id;
    let mut out = b"BLOCKULA_HANDOFF_V3".to_vec();
    out.extend_from_slice(&at.genesis_hash);
    out.extend_from_slice(&(chain_id.len() as u16).to_le_bytes());
    out.extend_from_slice(chain_id.as_bytes());
    out.extend_from_slice(&epoch.to_le_bytes());
    out.extend_from_slice(&set_hash(prev));
    out.extend_from_slice(&set_hash(keys));
    out
}

/// Validator-set history: the set at `base` and every change after it, with handoff proofs.
///
/// A node keeps the changes of a window (`chain::registry_keep_epochs`: the weak-subjectivity
/// period with a margin, since no joiner may start from an older root) and serves it in pages
/// from any epoch (`slice`). Its own entries come from executing epoch boundaries; a fast-synced
/// node starts from what its manifest's history verified, from its trusted root through the
/// certified epoch, and nothing past it (F08).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochRegistry {
    /// Sorted consensus keys (hex): the set in effect at `base`, then each change by the first
    /// epoch it's in effect for.
    pub sets: BTreeMap<u64, Vec<String>>,
    /// Highest epoch whose set this node knows.
    pub known_through: u64,
    /// Handoff signatures: epoch (a change after `base`) → signer index in the previous set → hex
    /// signature. At most a quorum is kept from peers (plus this node's own).
    pub proofs: BTreeMap<u64, BTreeMap<u32, String>>,
    /// The first epoch held: `sets[base]` is the set in effect then, unproven here (the genesis
    /// set at 0, or a trusted root's).
    #[serde(default)]
    pub base: u64,
    /// `HANDOFF_FORMAT` the proofs are signed under.
    #[serde(default)]
    pub format: u32,
}

/// A handoff signature from a peer, checked against our own sets before it's verified.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub epoch: u64,
    pub signer: u32,
    sig: String,
    key: String,
    msg: Vec<u8>,
}

impl Candidate {
    pub fn verify(&self) -> bool {
        verify_sig(&self.key, &self.msg, &self.sig).is_ok()
    }
}

/// Keeps the candidates whose signatures verify (call without the chain lock: one ML-DSA
/// verification each).
pub fn verify_candidates(c: Vec<Candidate>) -> Vec<Candidate> {
    c.into_iter().filter(Candidate::verify).collect()
}

impl EpochRegistry {
    pub fn genesis(g: &Genesis) -> Self {
        let mut keys: Vec<String> = g.validators.iter().map(|v| v.public_key.clone()).collect();
        keys.sort();
        Self { sets: BTreeMap::from([(0, keys)]), known_through: 0, proofs: BTreeMap::new(), base: 0, format: HANDOFF_FORMAT }
    }

    /// The set in effect at `epoch`, if known (from `base` through `known_through`).
    pub fn set(&self, epoch: u64) -> Option<&Vec<String>> {
        if epoch > self.known_through || epoch < self.base {
            return None;
        }
        self.sets.range(self.base..=epoch).next_back().map(|(_, s)| s)
    }

    /// Records the set for `epoch` (called after executing the previous epoch's last block).
    /// Returns true if it differs from the previous epoch's.
    pub fn record(&mut self, epoch: u64, mut keys: Vec<String>) -> bool {
        keys.sort();
        if epoch < self.base {
            return false; // before what this registry holds (a replay below a fast-sync root)
        }
        let changed = epoch > self.base && self.set(epoch - 1).is_none_or(|prev| *prev != keys);
        if changed && self.sets.get(&epoch) != Some(&keys) {
            // A different set than recorded before: its proofs were for that one.
            self.proofs.remove(&epoch);
            self.sets.insert(epoch, keys);
        } else if !changed && epoch > self.base && self.sets.remove(&epoch).is_some() {
            // Executed, the set didn't change here after all: drop what was recorded instead.
            self.proofs.remove(&epoch);
        }
        self.known_through = self.known_through.max(epoch);
        changed
    }

    /// Epochs after `base` whose set changed.
    pub fn changes(&self) -> impl Iterator<Item = u64> + '_ {
        let base = self.base;
        self.sets.keys().copied().filter(move |e| *e > base)
    }

    /// Drops the proofs of an older handoff format (they no longer verify). Returns whether it did.
    pub fn upgrade_format(&mut self) -> bool {
        if self.format == HANDOFF_FORMAT {
            return false;
        }
        self.proofs.clear();
        self.format = HANDOFF_FORMAT;
        true
    }

    /// Signs every recorded set change where `key` was in the outgoing set and hasn't signed yet,
    /// but only those this node's own executed state shows: `verified(e)` is the set (sorted
    /// keys) its state holds for epoch `e`, and both sides of the change must match it. Entries a
    /// peer supplied and this node never executed are never signed (F08).
    pub fn sign_missing(&mut self, at: &Instance, seed: [u8; 32], verified: impl Fn(u64) -> Option<Vec<String>>) -> Vec<(u64, u32, String)> {
        let key = PrivateKey::from_seed_bytes(seed);
        let me = hex::encode(key.public_key().as_ref());
        let mut out = vec![];
        for epoch in self.changes().collect::<Vec<_>>() {
            let Some(prev) = self.set(epoch - 1) else { continue };
            let Some(idx) = prev.iter().position(|k| *k == me) else { continue };
            if self.proofs.get(&epoch).is_some_and(|p| p.contains_key(&(idx as u32))) {
                continue;
            }
            if verified(epoch - 1).as_ref() != Some(prev) || verified(epoch).as_ref() != self.sets.get(&epoch) {
                continue;
            }
            let msg = handoff_message(at, epoch, prev, &self.sets[&epoch]);
            let sig = hex::encode(key.sign(HANDOFF_CONTEXT, &msg).as_ref());
            self.proofs.entry(epoch).or_default().insert(idx as u32, sig.clone());
            out.push((epoch, idx as u32, sig));
        }
        self.format = HANDOFF_FORMAT;
        out
    }

    /// The signatures in `other` worth verifying: for set changes we recorded ourselves (the
    /// same sets on both sides of it), from signers we don't hold yet, and only while the change
    /// lacks a quorum (up to what it lacks). At most `max` in all. Cheap: nothing is verified.
    pub fn candidates(&self, at: &Instance, other: &EpochRegistry, max: usize) -> Vec<Candidate> {
        let mut out = vec![];
        if other.format != HANDOFF_FORMAT {
            return out;
        }
        for (epoch, sigs) in other.proofs.range(self.base.saturating_add(1).max(1)..) {
            let (Some(new), Some(prev)) = (self.sets.get(epoch), self.set(epoch - 1)) else { continue };
            if other.sets.get(epoch) != Some(new) {
                continue;
            }
            let have = self.proofs.get(epoch);
            let mut need = quorum(prev.len()).saturating_sub(have.map_or(0, |h| h.len()));
            if need == 0 {
                continue;
            }
            let msg = handoff_message(at, *epoch, prev, new);
            for (signer, sig) in sigs {
                if out.len() >= max {
                    return out;
                }
                if need == 0 {
                    break;
                }
                if have.is_some_and(|h| h.contains_key(signer)) {
                    continue;
                }
                let Some(key) = prev.get(*signer as usize) else { continue };
                out.push(Candidate { epoch: *epoch, signer: *signer, sig: sig.clone(), key: key.clone(), msg: msg.clone() });
                need -= 1;
            }
        }
        out
    }

    /// Adds verified candidates whose change is still the one they were checked against.
    /// Returns how many were new.
    pub fn add_verified(&mut self, at: &Instance, verified: Vec<Candidate>) -> usize {
        let mut added = 0;
        // The message of each change, computed once (it hashes both sets).
        let mut msgs: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        for c in verified {
            // Nothing hands off to the base (or the genesis) set.
            let Some(before) = c.epoch.checked_sub(1).filter(|_| c.epoch > self.base) else { continue };
            let (Some(new), Some(prev)) = (self.sets.get(&c.epoch), self.set(before)) else { continue };
            if prev.get(c.signer as usize) != Some(&c.key) {
                continue;
            }
            if *msgs.entry(c.epoch).or_insert_with(|| handoff_message(at, c.epoch, prev, new)) != c.msg {
                continue;
            }
            if self.proofs.entry(c.epoch).or_default().insert(c.signer, c.sig).is_none() {
                added += 1;
            }
        }
        added
    }

    /// Merges another node's registry: only sets we already know, only valid signatures, each
    /// verified once (never one we hold, nor past a quorum). A peer's proofs for epoch 0 (the
    /// genesis set, which nothing hands off to) are ignored.
    pub fn merge_sigs(&mut self, at: &Instance, other: &EpochRegistry) -> usize {
        let c = verify_candidates(self.candidates(at, other, usize::MAX));
        self.add_verified(at, c)
    }

    /// Highest epoch through which every set change (after `base`) has a quorum handoff proof.
    pub fn proven_through(&self) -> u64 {
        for epoch in self.changes() {
            let prev_len = self.set(epoch - 1).map(|s| s.len()).unwrap_or(0);
            let have = self.proofs.get(&epoch).map(|p| p.len()).unwrap_or(0);
            if have < quorum(prev_len) {
                return epoch - 1;
            }
        }
        self.known_through
    }

    /// About what the registry takes as JSON (keys and signatures as hex, plus a little framing).
    pub fn approx_bytes(&self) -> usize {
        let sets: usize = self.sets.values().map(|s| s.iter().map(|k| k.len() + 3).sum::<usize>() + 16).sum();
        let proofs: usize = self.proofs.values().map(|p| p.values().map(|s| s.len() + 16).sum::<usize>() + 16).sum();
        sets + proofs
    }

    /// Drops history before `keep_from` (see [`EpochRegistry::rebase`]), but only once the
    /// registry is bigger than `budget`. Returns whether it did.
    pub fn trim(&mut self, keep_from: u64, budget: usize) -> bool {
        if keep_from <= self.base || self.approx_bytes() <= budget {
            return false;
        }
        let base = self.base;
        self.rebase(keep_from);
        self.base != base
    }

    /// Starts the registry at `at` (the set in effect then becomes the base), dropping what's
    /// before it. A no-op unless `base < at <= known_through`.
    pub fn rebase(&mut self, at: u64) {
        if at <= self.base || at > self.known_through {
            return;
        }
        let Some(set) = self.set(at).cloned() else { return };
        let keep = at.saturating_add(1);
        self.sets = if at == u64::MAX { BTreeMap::new() } else { self.sets.split_off(&keep) };
        self.sets.insert(at, set);
        self.proofs = if at == u64::MAX { BTreeMap::new() } else { self.proofs.split_off(&keep) };
        self.base = at;
    }

    /// Forgets everything after `through`, and anything held past `known_through` (which no
    /// set lookup reaches: a peer's registry may list it anyway, review-fix-sync-5).
    pub fn truncate(&mut self, through: u64) {
        let keep = through.min(self.known_through).max(self.base);
        if let Some(cut) = keep.checked_add(1) {
            self.sets.split_off(&cut);
            self.proofs.split_off(&cut);
        }
        self.known_through = self.known_through.min(keep);
    }

    /// What `/api/epochs?from=` serves: the set in effect at `from` (clamped to what's held) and
    /// the changes after it, each with a quorum of its proofs, up to about `budget` bytes (at
    /// least one change). When it stops early, `known_through` ends before the first change left
    /// out, and the second value is where the next page starts.
    pub fn slice(&self, from: u64, budget: usize) -> (EpochRegistry, Option<u64>) {
        let base = from.clamp(self.base, self.known_through.max(self.base));
        let first = self.set(base).cloned().unwrap_or_default();
        let mut bytes = first.iter().map(|k| k.len() + 3).sum::<usize>();
        let mut out = EpochRegistry { sets: BTreeMap::from([(base, first)]), known_through: self.known_through.max(base), proofs: BTreeMap::new(), base, format: self.format };
        let known = self.known_through;
        for (&e, set) in self.sets.range(base.saturating_add(1)..).filter(|(e, _)| **e > base).take_while(|(e, _)| **e <= known) {
            let q = self.set(e - 1).map_or(0, |p| quorum(p.len()));
            let proofs: BTreeMap<u32, String> = self.proofs.get(&e).map(|p| p.iter().take(q).map(|(i, s)| (*i, s.clone())).collect()).unwrap_or_default();
            let size = set.iter().map(|k| k.len() + 3).sum::<usize>() + proofs.values().map(|s| s.len() + 16).sum::<usize>() + 32;
            if out.sets.len() > 1 && bytes + size > budget {
                out.known_through = e - 1;
                return (out, Some(e - 1));
            }
            bytes += size;
            out.sets.insert(e, set.clone());
            if !proofs.is_empty() {
                out.proofs.insert(e, proofs);
            }
        }
        (out, None)
    }

    /// Appends the next page of a peer's history (`slice` from where this one ends).
    pub fn append_page(&mut self, page: &EpochRegistry) -> Result<()> {
        ensure!(page.base == self.known_through, "the page starts at epoch {}, not {}", page.base, self.known_through);
        ensure!(page.sets.get(&page.base).is_some() && page.set(page.base) == self.set(self.known_through), "the page's starting set isn't where the last one ended");
        ensure!(page.known_through >= page.base, "the page ends before it starts");
        // Only what the page covers: changes after its start, through its end.
        let within = |e: &u64| *e > page.base && *e <= page.known_through;
        for (e, s) in page.sets.range(page.base.saturating_add(1)..).filter(|(e, _)| within(e)) {
            self.sets.insert(*e, s.clone());
        }
        for (e, p) in page.proofs.range(page.base.saturating_add(1)..).filter(|(e, _)| within(e)) {
            self.proofs.insert(*e, p.clone());
        }
        self.known_through = page.known_through;
        self.format = page.format;
        Ok(())
    }
}


fn verify_sig(pk_hex: &str, msg: &[u8], sig_hex: &str) -> Result<()> {
    let pk = hex::decode(pk_hex).ok().and_then(|b| PublicKey::from_bytes(&b)).ok_or_else(|| anyhow!("bad validator key"))?;
    let sig = hex::decode(sig_hex).ok().and_then(|b| Signature::decode(b.as_slice()).ok()).ok_or_else(|| anyhow!("bad signature encoding"))?;
    if !pk.verify(HANDOFF_CONTEXT, msg, &sig) {
        bail!("handoff signature doesn't verify");
    }
    Ok(())
}

/// A validator set a node trusts without a handoff: the genesis set, or a checkpoint's pinned one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub epoch: u64,
    pub set_hash: [u8; 32],
}

impl Root {
    pub fn genesis(g: &Genesis) -> Self {
        Root { epoch: 0, set_hash: set_hash(&EpochRegistry::genesis(g).sets[&0]) }
    }
}

/// Verifies a peer's registry from the genesis set up to `epoch` and returns the set in effect
/// at `epoch` (for epoch 0, the genesis set). Never panics on a peer's registry (FUZZ-1).
#[cfg_attr(not(test), allow(dead_code))] // tests and the fuzz targets; nodes verify from a `Root`
pub fn verify_chain(g: &Genesis, claimed: &EpochRegistry, epoch: u64) -> Result<Vec<String>> {
    verify_from(g, claimed, &Root::genesis(g), epoch)
}

/// Verifies a peer's registry from a trusted `root` up to `epoch` and returns the set in effect
/// at `epoch`. The registry's set at the root's epoch must hash to the root's; every change after
/// it, through `epoch`, needs a quorum of distinct, valid signatures from the set before it,
/// signed for `g`'s chain instance (its genesis hash too, F02). History before the root doesn't
/// matter (and the registry may not hold it).
pub fn verify_from(g: &Genesis, claimed: &EpochRegistry, root: &Root, epoch: u64) -> Result<Vec<String>> {
    verify_history(g, claimed, root, epoch).map(|(set, _)| set)
}

/// [`verify_from`], also returning what of `claimed` it verified, which is all a node may adopt
/// from a peer: the registry from the root's epoch (its base) through `epoch` (nothing after it,
/// F08 and review-fix-sync-5) holding, for each set change, only signatures that verified, a
/// quorum of them. Junk a peer listed beside them never reaches the node's registry, so it never
/// serves it in place of a valid signature or counts it toward a quorum (review-fix-sync-2).
pub fn verify_history(g: &Genesis, claimed: &EpochRegistry, root: &Root, epoch: u64) -> Result<(Vec<String>, EpochRegistry)> {
    if claimed.base == 0 && claimed.sets.get(&0) != EpochRegistry::genesis(g).sets.get(&0) {
        bail!("epoch registry doesn't start from this genesis' validator set");
    }
    ensure!(claimed.sets.contains_key(&claimed.base) && claimed.sets.range(..claimed.base).next().is_none(), "malformed epoch registry: no set at its base epoch {}", claimed.base);
    ensure!(root.epoch <= epoch, "the trusted root (epoch {}) is after epoch {epoch}", root.epoch);
    if claimed.base > root.epoch {
        bail!("the peer's validator-set history starts at epoch {}, after the trusted root at epoch {} (it keeps about the weak-subjectivity period): pass a newer --checkpoint HEIGHT:HASH:SET_HASH", claimed.base, root.epoch);
    }
    if claimed.known_through < epoch {
        bail!("peer only knows validator sets through epoch {}", claimed.known_through);
    }
    let mut prev = claimed.set(root.epoch).ok_or_else(|| anyhow!("no validator set at the trusted root's epoch {}", root.epoch))?.clone();
    if set_hash(&prev) != root.set_hash {
        bail!("the validator set at epoch {} hashes to {}, but the trusted root pins {}: a forged validator-set history", root.epoch, hex::encode(set_hash(&prev)), hex::encode(root.set_hash));
    }
    let at = Instance::of(g);
    let mut kept = EpochRegistry { sets: BTreeMap::from([(root.epoch, prev.clone())]), known_through: epoch, proofs: BTreeMap::new(), base: root.epoch, format: HANDOFF_FORMAT };
    // `range(1..=0)` would panic: walk the changes after the root and stop past `epoch`.
    for (&e, set) in claimed.sets.range(root.epoch.saturating_add(1)..).filter(|(e, _)| **e > root.epoch).take_while(|(e, _)| **e <= epoch) {
        if set.is_empty() || set.windows(2).any(|w| w[0] >= w[1]) {
            bail!("set for epoch {e} isn't a sorted list of distinct keys");
        }
        let msg = handoff_message(&at, e, &prev, set);
        let need = quorum(prev.len());
        let mut valid = BTreeMap::new();
        for (signer, sig) in claimed.proofs.get(&e).into_iter().flatten() {
            if valid.len() >= need {
                break;
            }
            if let Some(pk) = prev.get(*signer as usize) {
                if verify_sig(pk, &msg, sig).is_ok() {
                    valid.insert(*signer, sig.clone());
                }
            }
        }
        if valid.len() < need {
            bail!("set change at epoch {e} has {} valid handoff signatures, needs {need}", valid.len());
        }
        kept.sets.insert(e, set.clone());
        kept.proofs.insert(e, valid);
        prev = set.clone();
    }
    Ok((prev, kept))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{Chain, SummonOpts};

    #[test]
    fn handoff_chain_verifies_and_rejects_forgeries() {
        let root = std::env::temp_dir().join(format!("blockula-epochs-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &SummonOpts::new("ep", 4)).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        let seeds: Vec<[u8; 32]> = homes.iter().map(|h| hex::decode(std::fs::read_to_string(h.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap()).collect();
        let newcomer = hex::encode(PrivateKey::from_seed_bytes([9; 32]).public_key().as_ref());
        let mut reg = EpochRegistry::genesis(&g);
        reg.record(1, reg.set(0).unwrap().clone());
        let mut next = reg.set(0).unwrap().clone();
        next.push(newcomer.clone());
        assert!(reg.record(2, next.clone()));
        assert_eq!(reg.proven_through(), 1);
        // Two of four signatures aren't a quorum.
        let truth = reg.clone();
        let mut regs: Vec<EpochRegistry> = (0..4).map(|_| reg.clone()).collect();
        for (r, s) in regs.iter_mut().zip(&seeds) {
            assert_eq!(r.sign_missing(&Instance::of(&g), *s, |e| truth.set(e).cloned()).len(), 1);
        }
        let mut partial = reg.clone();
        partial.merge_sigs(&Instance::of(&g), &regs[0]);
        partial.merge_sigs(&Instance::of(&g), &regs[1]);
        assert!(verify_chain(&g, &partial, 2).is_err());
        partial.merge_sigs(&Instance::of(&g), &regs[2]);
        assert_eq!(partial.proven_through(), 2);
        let mut sorted = next.clone();
        sorted.sort();
        assert_eq!(verify_chain(&g, &partial, 2).unwrap(), sorted);
        // A forged set (attacker key swapped in) fails: signatures cover the real set's hash.
        let mut forged = partial.clone();
        let evil = hex::encode(PrivateKey::from_seed_bytes([66; 32]).public_key().as_ref());
        let mut fake = forged.sets[&2].clone();
        *fake.iter_mut().find(|k| **k == newcomer).unwrap() = evil;
        fake.sort();
        forged.sets.insert(2, fake);
        assert!(verify_chain(&g, &forged, 2).is_err());
        // Signatures from outside the previous set (or duplicated) don't count.
        let mut stuffed = reg.clone();
        let evil_sig = hex::encode(PrivateKey::from_seed_bytes([66; 32]).sign(HANDOFF_CONTEXT, &handoff_message(&Instance::of(&g), 2, &stuffed.sets[&0], &stuffed.sets[&2])).as_ref());
        for i in 0..3 {
            stuffed.proofs.entry(2).or_default().insert(i, evil_sig.clone());
        }
        assert!(verify_chain(&g, &stuffed, 2).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// A 4-validator genesis and its seeds.
    fn four(tag: &str) -> (std::path::PathBuf, Genesis, Vec<[u8; 32]>) {
        let root = std::env::temp_dir().join(format!("blockula-epochs-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &SummonOpts::new("ep", 4)).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        let seeds = homes.iter().map(|h| hex::decode(std::fs::read_to_string(h.join("validator.key")).unwrap().trim()).unwrap().try_into().unwrap()).collect();
        (root, g, seeds)
    }

    /// `changes` set changes, one per epoch from epoch 1: a newcomer joins, then leaves, and so on.
    /// Every change is signed by all four genesis validators (in both sets, and a quorum of each).
    fn churn(g: &Genesis, seeds: &[[u8; 32]], changes: u64) -> EpochRegistry {
        let mut reg = EpochRegistry::genesis(g);
        let genesis = reg.sets[&0].clone();
        let mut with = genesis.clone();
        with.push(hex::encode(PrivateKey::from_seed_bytes([9; 32]).public_key().as_ref()));
        with.sort();
        for e in 1..=changes {
            assert!(reg.record(e, if e % 2 == 1 { with.clone() } else { genesis.clone() }));
        }
        let truth = reg.clone();
        for s in seeds {
            reg.sign_missing(&Instance::of(g), *s, |e| truth.set(e).cloned());
        }
        assert_eq!(reg.proven_through(), changes);
        reg
    }

    /// F19, F26: merging verifies a signature only if it's one we lack for a change still short
    /// of a quorum, so an identical (or larger) registry costs nothing the second time.
    #[test]
    fn merging_verifies_each_signature_once_and_stops_at_a_quorum() {
        let (root, g, seeds) = four("merge");
        let full = churn(&g, &seeds, 40);
        assert!(full.candidates(&Instance::of(&g), &full, usize::MAX).is_empty(), "an identical registry: nothing to verify");
        let mut fresh = full.clone();
        fresh.proofs.clear();
        let wanted = fresh.candidates(&Instance::of(&g), &full, usize::MAX);
        // Exactly a quorum per change: 3 of 4 into a 5-key set, 4 of 5 back to the genesis set.
        assert_eq!(wanted.len(), (1..=40).map(|e| if e % 2 == 1 { 3 } else { 4 }).sum::<usize>());
        assert_eq!(fresh.candidates(&Instance::of(&g), &full, 7).len(), 7, "capped per round");
        assert_eq!(fresh.add_verified(&Instance::of(&g), verify_candidates(wanted)), 140);
        assert_eq!(fresh.proven_through(), 40);
        assert!(fresh.candidates(&Instance::of(&g), &full, usize::MAX).is_empty(), "every change has a quorum now");
        assert_eq!(fresh.merge_sigs(&Instance::of(&g), &full), 0);
        // Garbage in a peer's registry is verified (and refused) only where we lack signatures.
        let mut junk = full.clone();
        for p in junk.proofs.values_mut() {
            for sig in p.values_mut() {
                *sig = "00".repeat(3309);
            }
        }
        let mut lacking = full.clone();
        lacking.proofs.get_mut(&7).unwrap().clear();
        assert_eq!(lacking.candidates(&Instance::of(&g), &junk, usize::MAX).len(), 3);
        assert_eq!(lacking.merge_sigs(&Instance::of(&g), &junk), 0);
        // Proofs signed under an older handoff format aren't even considered.
        assert!(lacking.candidates(&Instance::of(&g), &EpochRegistry { format: 1, ..full.clone() }, usize::MAX).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    /// F10: the history goes out in pages of about `PAGE_BYTES` that reassemble into what the node
    /// holds (a quorum of proofs per change), and verifies from the genesis or from any pinned set
    /// the pages reach back to; a node keeps a window (`rebase`), so an older root can't be served.
    #[test]
    fn history_is_paged_within_a_budget_and_verifies_from_a_pinned_root() {
        let (root, g, seeds) = four("pages");
        let full = churn(&g, &seeds, 300);
        let bytes = serde_json::to_vec(&full).unwrap().len();
        assert!(bytes > 4 * PAGE_BYTES, "{bytes} bytes of history");
        let (mut assembled, mut next) = full.slice(0, PAGE_BYTES);
        let mut pages = 1;
        while let Some(from) = next {
            let (page, more) = full.slice(from, PAGE_BYTES);
            let size = serde_json::to_vec(&page).unwrap().len();
            assert!(size <= PAGE_BYTES + 64 * 1024, "page {pages}: {size} bytes");
            assembled.append_page(&page).unwrap();
            (next, pages) = (more, pages + 1);
        }
        assert!(pages > 4);
        assert_eq!(assembled.known_through, 300);
        assert_eq!(verify_chain(&g, &assembled, 300).unwrap(), full.sets[&300]);
        // A page that doesn't continue where the last one ended is refused.
        let (later, _) = full.slice(200, PAGE_BYTES);
        let (mut early, _) = full.slice(0, 1);
        assert!(early.append_page(&later).is_err());
        // From a pinned root: only what's after it is verified; before it needn't be held.
        let pin = |e: u64| Root { epoch: e, set_hash: set_hash(full.set(e).unwrap()) };
        let (from_150, _) = full.slice(150, usize::MAX);
        assert_eq!(from_150.base, 150);
        assert_eq!(verify_from(&g, &from_150, &pin(150), 300).unwrap(), full.sets[&300]);
        assert!(verify_chain(&g, &from_150, 300).unwrap_err().to_string().contains("starts at epoch 150"));
        let mut kept = full.clone();
        kept.rebase(160);
        assert_eq!((kept.base, kept.set(159), kept.set(160)), (160, None, full.set(160)));
        assert!(verify_from(&g, &kept, &pin(150), 300).unwrap_err().to_string().contains("--checkpoint"));
        assert!(verify_from(&g, &kept, &pin(170), 300).is_ok());
        // A node trims to its window only once the history is big: a small one stays whole.
        let mut small = full.clone();
        assert!(!small.trim(160, small.approx_bytes()) && small.base == 0);
        assert!(small.trim(160, small.approx_bytes() - 1) && small == kept);
        assert!(serde_json::to_vec(&full).unwrap().len().abs_diff(full.approx_bytes()) < full.approx_bytes() / 10, "approx_bytes is close to the JSON size");
        // A root the history contradicts is a forgery.
        let wrong = Root { epoch: 170, set_hash: set_hash(full.set(171).unwrap()) };
        assert!(verify_from(&g, &kept, &wrong, 300).unwrap_err().to_string().contains("forged"));
        // Truncated past an epoch, nothing after it is left.
        let mut cut = full.clone();
        cut.truncate(100);
        assert_eq!((cut.known_through, cut.sets.keys().next_back(), cut.proofs.keys().next_back()), (100, Some(&100), Some(&100)));
        std::fs::remove_dir_all(root).ok();
    }

    /// F08: a validator signs only set changes its own executed state shows (both sides of the
    /// change), never one a peer's registry planted; and a handoff signature covers the outgoing
    /// set too, so it isn't a quorum share for the same new set after another previous one.
    #[test]
    fn only_executed_transitions_are_signed_and_signatures_bind_the_outgoing_set() {
        let (root, g, seeds) = four("planted");
        let mut reg = churn(&g, &seeds, 4);
        reg.proofs.clear();
        let executed = reg.clone();
        // A peer's registry adds a change at epoch 5 to keys it controls.
        let planted: Vec<String> = {
            let mut k: Vec<String> = (0..4u8).map(|i| hex::encode(PrivateKey::from_seed_bytes([200 + i; 32]).public_key().as_ref())).collect();
            k.sort();
            k
        };
        reg.record(5, planted.clone());
        let signed = reg.sign_missing(&Instance::of(&g), seeds[0], |e| executed.set(e).cloned());
        assert_eq!(signed.iter().map(|(e, ..)| *e).collect::<Vec<_>>(), vec![1, 2, 3, 4], "not the planted change");
        assert!(!reg.proofs.contains_key(&5));
        // Nor one whose new set the state shows but not the old one (or the other way round).
        let mut half = reg.clone();
        half.proofs.clear();
        assert!(half.sign_missing(&Instance::of(&g), seeds[0], |e| (e != 2).then(|| executed.set(e).cloned()).flatten()).iter().all(|(e, ..)| *e != 2 && *e != 3));
        // The outgoing set is in the message.
        let (a, b, new) = (&executed.sets[&0], &executed.sets[&1], &planted);
        let key = PrivateKey::from_seed_bytes(seeds[0]);
        let sig = hex::encode(key.sign(HANDOFF_CONTEXT, &handoff_message(&Instance::of(&g), 5, a, new)).as_ref());
        let pk = hex::encode(key.public_key().as_ref());
        assert!(verify_sig(&pk, &handoff_message(&Instance::of(&g), 5, a, new), &sig).is_ok());
        assert!(verify_sig(&pk, &handoff_message(&Instance::of(&g), 5, b, new), &sig).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// F02 (review-integrate-compose-1): a relaunch that keeps the chain id and the validators'
    /// keys (only `created_at_ms` differs here, so the genesis hash does) shares no handoff with
    /// the old instance: what the old one's validators signed doesn't verify, isn't merged and
    /// isn't even a candidate on the new one, and the other way round.
    #[test]
    fn a_handoff_for_another_genesis_of_the_chain_id_does_not_verify() {
        let (root, a, seeds) = four("relaunch");
        let mut b = a.clone();
        b.created_at_ms += 1;
        assert_eq!((&a.chain_id, EpochRegistry::genesis(&a)), (&b.chain_id, EpochRegistry::genesis(&b)), "same id, same genesis set");
        assert_ne!(Instance::of(&a), Instance::of(&b));
        let on_a = churn(&a, &seeds, 3);
        assert_eq!(verify_chain(&a, &on_a, 3).unwrap(), on_a.sets[&3]);
        let err = verify_chain(&b, &on_a, 3).unwrap_err().to_string();
        assert!(err.contains("epoch 1") && err.contains("0 valid handoff signatures"), "{err}");
        let mut on_b = on_a.clone();
        on_b.proofs.clear();
        assert!(on_b.candidates(&Instance::of(&b), &on_a, usize::MAX).iter().all(|c| !c.verify()), "A's signatures are no candidates' worth on B");
        assert_eq!(on_b.merge_sigs(&Instance::of(&b), &on_a), 0);
        // Signed for B, they verify on B and not on A.
        let truth = on_b.clone();
        for s in &seeds {
            on_b.sign_missing(&Instance::of(&b), *s, |e| truth.set(e).cloned());
        }
        assert_eq!(verify_chain(&b, &on_b, 3).unwrap(), on_b.sets[&3]);
        assert!(verify_chain(&a, &on_b, 3).is_err());
        // The message names the genesis hash, so a signature over one is no signature over the other.
        let (prev, new) = (&on_a.sets[&0], &on_a.sets[&1]);
        assert_ne!(handoff_message(&Instance::of(&a), 1, prev, new), handoff_message(&Instance::of(&b), 1, prev, new));
        std::fs::remove_dir_all(root).ok();
    }

    /// review-fix-sync-2, -5: what a node adopts from a peer's history is only what verified: a
    /// quorum of valid signatures per change (junk at a lower index than valid ones is left out,
    /// so the node never serves it in their place), from the root through the epoch asked for,
    /// and nothing listed past it, also when the peer's `known_through` is that epoch.
    #[test]
    fn only_verified_signatures_and_epochs_are_adopted() {
        let (root, g, seeds) = four("adopt");
        let full = churn(&g, &seeds, 4);
        // All four outgoing validators signed epoch 1's change (quorum 3); junk replaces the
        // lowest-index signature.
        assert_eq!(full.proofs[&1].len(), 4);
        let mut junk = full.clone();
        junk.proofs.get_mut(&1).unwrap().insert(0, "00".repeat(3309));
        let (set, kept) = verify_history(&g, &junk, &Root::genesis(&g), 4).unwrap();
        assert_eq!(set, full.sets[&4]);
        assert_eq!(kept.proofs[&1].keys().copied().collect::<Vec<_>>(), vec![1, 2, 3], "the junk is left out");
        assert!(kept.proofs.values().all(|p| p.len() == 3 || p.len() == 4) && kept.proven_through() == 4);
        // What it serves verifies (the first quorum by signer index was the junk before).
        let (page, next) = kept.slice(0, PAGE_BYTES);
        assert_eq!((verify_chain(&g, &page, 4).unwrap(), next), (full.sets[&4].clone(), None));
        let (page, _) = { let mut all = junk.clone(); all.truncate(4); all }.slice(0, PAGE_BYTES);
        assert!(verify_chain(&g, &page, 4).is_err(), "served as listed, the junk would displace a valid signature");
        // A registry listing sets and proofs past its known_through: none of it is adopted, and
        // truncating at known_through drops it too.
        let mut beyond = full.clone();
        beyond.truncate(3);
        beyond.sets.insert(4, full.sets[&4].clone());
        beyond.proofs.insert(4, BTreeMap::from([(0, "00".repeat(3309))]));
        assert_eq!(beyond.known_through, 3);
        let (_, kept) = verify_history(&g, &beyond, &Root::genesis(&g), 3).unwrap();
        assert!(kept.sets.keys().chain(kept.proofs.keys()).all(|e| *e <= 3) && kept.known_through == 3);
        beyond.truncate(3);
        assert!(beyond.sets.keys().chain(beyond.proofs.keys()).all(|e| *e <= 3), "truncating at known_through still cuts what's listed past it");
        // From a pinned root, the adopted registry starts there.
        let pin = Root { epoch: 2, set_hash: set_hash(full.set(2).unwrap()) };
        let (_, from_2) = verify_history(&g, &junk, &pin, 4).unwrap();
        assert_eq!((from_2.base, from_2.sets.keys().copied().collect::<Vec<_>>(), from_2.proofs.keys().copied().collect::<Vec<_>>()), (2, vec![2, 3, 4], vec![3, 4]));
        std::fs::remove_dir_all(root).ok();
    }

    fn one_validator_genesis(tag: &str) -> (std::path::PathBuf, Genesis) {
        let root = std::env::temp_dir().join(format!("blockula-epochs-{tag}-{}-{}", crate::chain::now_ms(), std::process::id()));
        let homes = Chain::summon(&root, &SummonOpts::new("ep0", 1)).unwrap();
        let g = Genesis::load(&homes[0].join("genesis.json")).unwrap();
        (root, g)
    }

    /// FUZZ-1: a fast-sync manifest from epoch 0 (a young chain, or any peer making one up) asked
    /// for `verify_chain(.., 0)`, which ran `sets.range(1..=0)` and panicked the joining node.
    #[test]
    fn verify_chain_at_epoch_0_is_the_genesis_set() {
        let (root, g) = one_validator_genesis("fuzz1");
        let reg = EpochRegistry::genesis(&g);
        assert_eq!(verify_chain(&g, &reg, 0).unwrap(), reg.sets[&0]);
        // Later set changes (even bogus, unsigned ones) don't matter at epoch 0.
        let mut later = reg.clone();
        later.sets.insert(5, vec!["zz".into()]);
        later.known_through = 5;
        assert_eq!(verify_chain(&g, &later, 0).unwrap(), reg.sets[&0]);
        assert!(verify_chain(&g, &later, 5).is_err(), "the unsigned change still fails where it applies");
        // A registry that doesn't start from this genesis is refused at epoch 0 too.
        let mut other = reg.clone();
        other.sets.insert(0, vec!["ab".into()]);
        assert!(verify_chain(&g, &other, 0).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// FUZZ-2: a peer's "handoff proof" for epoch 0 made `add_sig` compute `0 - 1`, an overflow
    /// panic (overflow checks are on in release) that ended the `exchange_epochs` task. Merging
    /// skips it (nothing hands off to the genesis set), and so does adding a verified one.
    #[test]
    fn handoff_proofs_for_epoch_0_are_ignored() {
        let (root, g) = one_validator_genesis("fuzz2");
        let mut reg = EpochRegistry::genesis(&g);
        let mut peer = reg.clone();
        peer.proofs.entry(0).or_default().insert(0, "00".into());
        assert_eq!(reg.merge_sigs(&Instance::of(&g), &peer), 0);
        assert!(reg.candidates(&Instance::of(&g), &peer, usize::MAX).is_empty());
        assert!(reg.add_verified(&Instance::of(&g), vec![Candidate { epoch: 0, signer: 0, sig: "00".into(), key: reg.sets[&0][0].clone(), msg: vec![] }]) == 0);
        assert_eq!(reg, EpochRegistry::genesis(&g), "nothing was recorded");
        std::fs::remove_dir_all(root).ok();
    }
}
