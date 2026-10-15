//! Validator-set history with handoff proofs, so a node can learn who the validators are at any
//! epoch without replaying the chain (used by snapshot/fast sync).
//!
//! When an epoch boundary changes the active set, members of the outgoing set sign a handoff:
//! `(chain_id, epoch, hash(new set))` with their consensus key (FIPS 204 context
//! `HANDOFF_CONTEXT`). A quorum (2f+1) of the outgoing set makes the new set trusted. Epochs
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
use anyhow::{anyhow, bail, Result};
use commonware_codec::DecodeExt as _;
use commonware_cryptography::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const HANDOFF_CONTEXT: &[u8] = b"blockula/epoch-handoff/v1";

/// Byzantine quorum for `n` validators (n − f, f = ⌊(n−1)/3⌋).
pub fn quorum(n: usize) -> usize {
    n - n.saturating_sub(1) / 3
}

/// The hash a handoff signs and a checkpoint pins: of the sorted consensus keys (hex).
pub fn set_hash(keys: &[String]) -> [u8; 32] {
    crate::crypto::hash(format!("BLOCKULA_SET_V1:{}", keys.join(",")).as_bytes())
}

pub fn handoff_message(chain_id: &str, epoch: u64, keys: &[String]) -> Vec<u8> {
    let mut out = b"BLOCKULA_HANDOFF_V1".to_vec();
    out.extend_from_slice(&(chain_id.len() as u16).to_le_bytes());
    out.extend_from_slice(chain_id.as_bytes());
    out.extend_from_slice(&epoch.to_le_bytes());
    out.extend_from_slice(&set_hash(keys));
    out
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochRegistry {
    /// Sorted consensus keys (hex), by the first epoch each set is in effect for.
    pub sets: BTreeMap<u64, Vec<String>>,
    /// Highest epoch whose set this node knows.
    pub known_through: u64,
    /// Handoff signatures: epoch → signer index in the previous set → hex signature.
    pub proofs: BTreeMap<u64, BTreeMap<u32, String>>,
}

impl EpochRegistry {
    pub fn genesis(g: &Genesis) -> Self {
        let mut keys: Vec<String> = g.validators.iter().map(|v| v.public_key.clone()).collect();
        keys.sort();
        Self { sets: BTreeMap::from([(0, keys)]), known_through: 0, proofs: BTreeMap::new() }
    }

    /// The set in effect at `epoch`, if known.
    pub fn set(&self, epoch: u64) -> Option<&Vec<String>> {
        if epoch > self.known_through {
            return None;
        }
        self.sets.range(..=epoch).next_back().map(|(_, s)| s)
    }

    /// Records the set for `epoch` (called after executing the previous epoch's last block).
    /// Returns true if it differs from the previous epoch's.
    pub fn record(&mut self, epoch: u64, mut keys: Vec<String>) -> bool {
        keys.sort();
        let changed = self.set(epoch.saturating_sub(1)).is_none_or(|prev| *prev != keys) && epoch > 0;
        if changed {
            self.sets.insert(epoch, keys);
        }
        self.known_through = self.known_through.max(epoch);
        changed
    }

    /// Epochs (> 0) whose set changed.
    pub fn changes(&self) -> impl Iterator<Item = u64> + '_ {
        self.sets.keys().copied().filter(|e| *e > 0)
    }

    /// Adds a handoff signature after checking it. Returns whether it was new. Epoch 0's set is
    /// the genesis set: nothing hands off to it, so a "proof" for it is an error (FUZZ-2).
    pub fn add_sig(&mut self, chain_id: &str, epoch: u64, signer: u32, sig_hex: &str) -> Result<bool> {
        let before = epoch.checked_sub(1).ok_or_else(|| anyhow!("epoch 0 is the genesis set; it has no handoff"))?;
        let new_set = self.sets.get(&epoch).ok_or_else(|| anyhow!("no set change recorded at epoch {epoch}"))?.clone();
        let prev = self.set(before).ok_or_else(|| anyhow!("unknown set before epoch {epoch}"))?;
        let pk_hex = prev.get(signer as usize).ok_or_else(|| anyhow!("signer {signer} isn't in the previous set"))?;
        verify_sig(pk_hex, &handoff_message(chain_id, epoch, &new_set), sig_hex)?;
        Ok(self.proofs.entry(epoch).or_default().insert(signer, sig_hex.to_string()).is_none())
    }

    /// Signs every recorded set change where `key` was in the outgoing set and hasn't signed yet.
    pub fn sign_missing(&mut self, chain_id: &str, seed: [u8; 32]) -> Vec<(u64, u32, String)> {
        let key = PrivateKey::from_seed_bytes(seed);
        let me = hex::encode(key.public_key().as_ref());
        let mut out = vec![];
        for epoch in self.changes().collect::<Vec<_>>() {
            let Some(prev) = self.set(epoch - 1) else { continue };
            let Some(idx) = prev.iter().position(|k| *k == me) else { continue };
            if self.proofs.get(&epoch).is_some_and(|p| p.contains_key(&(idx as u32))) {
                continue;
            }
            let sig = hex::encode(key.sign(HANDOFF_CONTEXT, &handoff_message(chain_id, epoch, &self.sets[&epoch])).as_ref());
            self.proofs.entry(epoch).or_default().insert(idx as u32, sig.clone());
            out.push((epoch, idx as u32, sig));
        }
        out
    }

    /// Merges another node's registry: only sets we already know, only valid signatures. A peer's
    /// proofs for epoch 0 (the genesis set, which nothing hands off to) are ignored.
    pub fn merge_sigs(&mut self, chain_id: &str, other: &EpochRegistry) -> usize {
        let mut added = 0;
        for (epoch, sigs) in other.proofs.range(1..) {
            if other.sets.get(epoch) != self.sets.get(epoch) {
                continue;
            }
            for (signer, sig) in sigs {
                if let Ok(true) = self.add_sig(chain_id, *epoch, *signer, sig) {
                    added += 1;
                }
            }
        }
        added
    }

    /// Highest epoch through which every set change has a quorum handoff proof.
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
}

fn verify_sig(pk_hex: &str, msg: &[u8], sig_hex: &str) -> Result<()> {
    let pk = hex::decode(pk_hex).ok().and_then(|b| PublicKey::from_bytes(&b)).ok_or_else(|| anyhow!("bad validator key"))?;
    let sig = hex::decode(sig_hex).ok().and_then(|b| Signature::decode(b.as_slice()).ok()).ok_or_else(|| anyhow!("bad signature encoding"))?;
    if !pk.verify(HANDOFF_CONTEXT, msg, &sig) {
        bail!("handoff signature doesn't verify");
    }
    Ok(())
}

/// Verifies a peer's registry from the genesis set up to `epoch` and returns the set in effect
/// at `epoch` (for epoch 0, the genesis set). Every set change needs a quorum of distinct, valid
/// signatures from the set before it. Never panics on a peer's registry (FUZZ-1).
pub fn verify_chain(g: &Genesis, claimed: &EpochRegistry, epoch: u64) -> Result<Vec<String>> {
    let genesis = EpochRegistry::genesis(g);
    if claimed.sets.get(&0) != genesis.sets.get(&0) {
        bail!("epoch registry doesn't start from this genesis' validator set");
    }
    if claimed.known_through < epoch {
        bail!("peer only knows validator sets through epoch {}", claimed.known_through);
    }
    let mut prev = genesis.sets[&0].clone();
    // `range(1..=0)` would panic: walk the changes after genesis and stop past `epoch`.
    for (&e, set) in claimed.sets.range(1..).take_while(|(e, _)| **e <= epoch) {
        if set.is_empty() || set.windows(2).any(|w| w[0] >= w[1]) {
            bail!("set for epoch {e} isn't a sorted list of distinct keys");
        }
        let msg = handoff_message(&g.chain_id, e, set);
        let sigs = claimed.proofs.get(&e).cloned().unwrap_or_default();
        let mut ok = 0;
        for (signer, sig) in &sigs {
            if let Some(pk) = prev.get(*signer as usize) {
                if verify_sig(pk, &msg, sig).is_ok() {
                    ok += 1;
                }
            }
        }
        if ok < quorum(prev.len()) {
            bail!("set change at epoch {e} has {ok} valid handoff signatures, needs {}", quorum(prev.len()));
        }
        prev = set.clone();
    }
    Ok(prev)
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
        let mut regs: Vec<EpochRegistry> = (0..4).map(|_| reg.clone()).collect();
        for (r, s) in regs.iter_mut().zip(&seeds) {
            assert_eq!(r.sign_missing("ep", *s).len(), 1);
        }
        let mut partial = reg.clone();
        partial.merge_sigs("ep", &regs[0]);
        partial.merge_sigs("ep", &regs[1]);
        assert!(verify_chain(&g, &partial, 2).is_err());
        partial.merge_sigs("ep", &regs[2]);
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
        let evil_sig = hex::encode(PrivateKey::from_seed_bytes([66; 32]).sign(HANDOFF_CONTEXT, &handoff_message("ep", 2, &stuffed.sets[&2])).as_ref());
        for i in 0..3 {
            stuffed.proofs.entry(2).or_default().insert(i, evil_sig.clone());
        }
        assert!(verify_chain(&g, &stuffed, 2).is_err());
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
    /// panic (overflow checks are on in release) that ended the `exchange_epochs` task.
    #[test]
    fn handoff_proofs_for_epoch_0_are_ignored() {
        let (root, g) = one_validator_genesis("fuzz2");
        let mut reg = EpochRegistry::genesis(&g);
        let mut peer = reg.clone();
        peer.proofs.entry(0).or_default().insert(0, "00".into());
        assert_eq!(reg.merge_sigs("ep0", &peer), 0);
        assert!(reg.add_sig("ep0", 0, 0, "00").unwrap_err().to_string().contains("genesis"));
        assert_eq!(reg, EpochRegistry::genesis(&g), "nothing was recorded");
        std::fs::remove_dir_all(root).ok();
    }
}
