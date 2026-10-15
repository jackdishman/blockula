//! The state layer: `State` (chain.rs) as a set of records, and the authenticated root over them.
//!
//! Every piece of state is a record `(namespace, key) → value`: one per entry of every state
//! map (namespaces in [`ns`]), plus the scalars in [`ns::META`] (schema version, parameters,
//! supply, burned, id counters, the PoC scalars). Values have a canonical binary encoding
//! ([`codec`]). The state root is the root of a sparse Merkle tree over all records ([`tree`]),
//! with inclusion and exclusion proofs.
//!
//! The maps are [`TMap`]s, which mark every key they change. After each block,
//! [`State::commit_block`] re-encodes just those records (and the scalars), updates the tree along
//! their paths and returns the new root: O(k log n) for k touched records, not O(state). Journal
//! rollbacks (mempool dry runs, proposals) unmark what they undo, so they cost the commit nothing.
//!
//! Mainnet is a new chain: there are no compatibility roots for v0.4 testnet chains, whose
//! `blocks.jsonl` won't replay under this binary.

// Proofs, record lookups and the persistence hooks are exercised by tests now; `/api/proof` and
// the redb store (P-STATE-B) are their callers in the node.
#![cfg_attr(not(test), allow(dead_code))]

pub mod codec;
pub mod tmap;
pub mod tree;

pub use tmap::{JMap, TMap};
pub use tree::{Hash, Proof};

use crate::chain::{HuntStatus, State};
use crate::poc::{ClaimStatus, Poc};
use anyhow::{bail, ensure, Result};
use borsh::{BorshDeserialize, BorshSerialize};
use codec::{decode, encode, RecordKey, SCHEMA_VERSION};

/// Record namespaces: the first byte of every record's tree key. Never renumber or reuse one.
pub mod ns {
    /// Scalars, keyed by name (see [`super::meta`]).
    pub const META: u8 = 0;
    pub const ACCOUNTS: u8 = 1;
    pub const NAMES: u8 = 2;
    pub const VALIDATORS: u8 = 3;
    pub const ACTIVE_SETS: u8 = 4;
    pub const HUNTS: u8 = 5;
    pub const GITHUB: u8 = 6;
    pub const GITHUB_BY_ACCOUNT: u8 = 7;
    pub const FAMILIARS: u8 = 8;
    pub const REPOS: u8 = 16;
    pub const SOULS: u8 = 17;
    pub const VOUCHES: u8 = 18;
    pub const SYBILS: u8 = 19;
    pub const CLAIMS: u8 = 20;
    pub const CLAIM_BY_PR: u8 = 21;
    pub const HANDLES: u8 = 22;
    pub const JURORS: u8 = 23;
    pub const SEANCES: u8 = 24;
    pub const GRANTS: u8 = 25;
}

/// Keys of the scalar records (namespace [`ns::META`]), in canonical (byte) order.
pub mod meta {
    pub const BURNED: &[u8] = b"burned";
    pub const NEXT_HUNT_ID: &[u8] = b"next_hunt_id";
    pub const PARAMS: &[u8] = b"params";
    /// `(next_claim_id, next_seance_id, next_grant_id, seed, open_seance, next_seance_at)`.
    pub const POC: &[u8] = b"poc";
    pub const SCHEMA: &[u8] = b"schema";
    pub const SUPPLY: &[u8] = b"supply";
}

/// Largest record key and value a snapshot may contain (bounds allocations while decoding).
pub const MAX_KEY_BYTES: usize = 4 << 10;
pub const MAX_VALUE_BYTES: usize = 16 << 20;

/// One record written by a commit; `value: None` deletes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordChange {
    pub namespace: u8,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
}

/// What [`State::commit_block`] did: the new root and every record it changed, in canonical
/// order. P-STATE-B persists `records` (and the tree's pending nodes) with the block.
#[derive(Clone, Debug)]
pub struct Commit {
    pub root: Hash,
    pub records: Vec<RecordChange>,
}

/// A state map seen as records (object-safe, so `State` can list its maps).
pub trait Table {
    fn record_count(&self) -> usize;
    /// Calls `f(key, value)` for every record, in key order.
    fn for_each_record(&self, f: &mut dyn FnMut(&[u8], &[u8]));
    fn encoded(&self, key: &[u8]) -> Option<Vec<u8>>;
    /// Moves the changed keys out as records (in key order), current values encoded.
    fn take_changes(&mut self, namespace: u8, out: &mut Vec<RecordChange>);
    /// Adds a record the tree already has (snapshot restore). Rejects non-canonical bytes.
    fn load(&mut self, key: &[u8], value: &[u8]) -> Result<()>;
}

impl<K, V> Table for TMap<K, V>
where
    K: Ord + Clone + RecordKey,
    V: Clone + BorshSerialize + BorshDeserialize,
{
    fn record_count(&self) -> usize {
        self.len()
    }

    fn for_each_record(&self, f: &mut dyn FnMut(&[u8], &[u8])) {
        let mut buf = vec![];
        for (k, v) in self.iter() {
            buf.clear();
            v.serialize(&mut buf).expect("encoding to memory can't fail");
            f(&k.key_bytes(), &buf);
        }
    }

    fn encoded(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.get(&K::from_key_bytes(key).ok()?).map(encode)
    }

    fn take_changes(&mut self, namespace: u8, out: &mut Vec<RecordChange>) {
        for k in self.take_dirty() {
            out.push(RecordChange { namespace, key: k.key_bytes().into_owned(), value: self.get(&k).map(encode) });
        }
    }

    fn load(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let (k, v) = (K::from_key_bytes(key)?, decode::<V>(value)?);
        // Strict decoding makes these hold already; checked because the tree hashes these bytes.
        ensure!(k.key_bytes() == key && encode(&v) == value, "non-canonical record encoding");
        ensure!(self.insert_committed(k, v).is_none(), "duplicate record");
        Ok(())
    }
}

impl State {
    /// Every map, by namespace, in namespace order. The destructuring is exhaustive on purpose:
    /// a new `State` or `Poc` field doesn't compile until it is listed here as a map (with a new
    /// namespace) or skipped as a scalar that `meta_records` covers, so nothing escapes the root.
    fn tables(&self) -> [(u8, &dyn Table); 18] {
        let State { accounts, names, supply: _, burned: _, validators, active_sets, params: _, hunts, next_hunt_id: _, github, github_by_account, familiars, poc, tree: _ } = self;
        let Poc { repos, souls, vouches, sybils, claims, claim_by_pr, handles, jurors, seances, grants, next_claim_id: _, next_seance_id: _, next_grant_id: _, seed: _, open_seance: _, next_seance_at: _ } = poc;
        [
            (ns::ACCOUNTS, accounts),
            (ns::NAMES, names),
            (ns::VALIDATORS, validators),
            (ns::ACTIVE_SETS, active_sets),
            (ns::HUNTS, hunts),
            (ns::GITHUB, github),
            (ns::GITHUB_BY_ACCOUNT, github_by_account),
            (ns::FAMILIARS, familiars),
            (ns::REPOS, repos),
            (ns::SOULS, souls),
            (ns::VOUCHES, vouches),
            (ns::SYBILS, sybils),
            (ns::CLAIMS, claims),
            (ns::CLAIM_BY_PR, claim_by_pr),
            (ns::HANDLES, handles),
            (ns::JURORS, jurors),
            (ns::SEANCES, seances),
            (ns::GRANTS, grants),
        ]
    }

    /// [`State::tables`], mutably (same list, same order).
    fn tables_mut(&mut self) -> [(u8, &mut dyn Table); 18] {
        let State { accounts, names, supply: _, burned: _, validators, active_sets, params: _, hunts, next_hunt_id: _, github, github_by_account, familiars, poc, tree: _ } = self;
        let Poc { repos, souls, vouches, sybils, claims, claim_by_pr, handles, jurors, seances, grants, next_claim_id: _, next_seance_id: _, next_grant_id: _, seed: _, open_seance: _, next_seance_at: _ } = poc;
        [
            (ns::ACCOUNTS, accounts),
            (ns::NAMES, names),
            (ns::VALIDATORS, validators),
            (ns::ACTIVE_SETS, active_sets),
            (ns::HUNTS, hunts),
            (ns::GITHUB, github),
            (ns::GITHUB_BY_ACCOUNT, github_by_account),
            (ns::FAMILIARS, familiars),
            (ns::REPOS, repos),
            (ns::SOULS, souls),
            (ns::VOUCHES, vouches),
            (ns::SYBILS, sybils),
            (ns::CLAIMS, claims),
            (ns::CLAIM_BY_PR, claim_by_pr),
            (ns::HANDLES, handles),
            (ns::JURORS, jurors),
            (ns::SEANCES, seances),
            (ns::GRANTS, grants),
        ]
    }

    /// The scalar records, in key order. Cheap, so every commit re-checks them all. Exhaustive
    /// like [`State::tables`].
    fn meta_records(&self) -> [(&'static [u8], Vec<u8>); 6] {
        let State { supply, burned, params, next_hunt_id, poc, accounts: _, names: _, validators: _, active_sets: _, hunts: _, github: _, github_by_account: _, familiars: _, tree: _ } = self;
        let Poc { next_claim_id, next_seance_id, next_grant_id, seed, open_seance, next_seance_at, repos: _, souls: _, vouches: _, sybils: _, claims: _, claim_by_pr: _, handles: _, jurors: _, seances: _, grants: _ } = poc;
        [
            (meta::BURNED, encode(burned)),
            (meta::NEXT_HUNT_ID, encode(next_hunt_id)),
            (meta::PARAMS, encode(params)),
            (meta::POC, encode(&(next_claim_id, next_seance_id, next_grant_id, seed, open_seance, next_seance_at))),
            (meta::SCHEMA, encode(&SCHEMA_VERSION)),
            (meta::SUPPLY, encode(supply)),
        ]
    }

    fn load_meta(&mut self, key: &[u8], v: &[u8]) -> Result<()> {
        match key {
            meta::BURNED => self.burned = decode(v)?,
            meta::NEXT_HUNT_ID => self.next_hunt_id = decode(v)?,
            meta::PARAMS => self.params = decode(v)?,
            meta::POC => {
                let p = &mut self.poc;
                (p.next_claim_id, p.next_seance_id, p.next_grant_id, p.seed, p.open_seance, p.next_seance_at) = decode(v)?;
            }
            meta::SCHEMA => {
                let schema: u32 = decode(v)?;
                ensure!(schema == SCHEMA_VERSION, "state schema {schema}, this binary reads {SCHEMA_VERSION}");
            }
            meta::SUPPLY => self.supply = decode(v)?,
            _ => bail!("unknown scalar record {}", String::from_utf8_lossy(key)),
        }
        Ok(())
    }

    /// Calls `f(namespace, key, value)` for every record, in canonical order: by namespace, then
    /// key bytes. The full root, snapshots and persistence all enumerate state through this.
    pub fn for_each_record(&self, mut f: impl FnMut(u8, &[u8], &[u8])) {
        for (k, v) in self.meta_records() {
            f(ns::META, k, &v);
        }
        for (n, t) in self.tables() {
            t.for_each_record(&mut |k, v| f(n, k, v));
        }
    }

    pub fn record_count(&self) -> usize {
        6 + self.tables().iter().map(|(_, t)| t.record_count()).sum::<usize>()
    }

    /// The current encoded value of a record.
    pub fn record(&self, namespace: u8, key: &[u8]) -> Option<Vec<u8>> {
        if namespace == ns::META {
            return self.meta_records().into_iter().find(|(k, _)| *k == key).map(|(_, v)| v);
        }
        self.tables().into_iter().find(|(n, _)| *n == namespace)?.1.encoded(key)
    }

    /// Updates the tree with everything changed since the last commit (the records a block
    /// touched, and any scalar that moved) and returns the new root with the changed records.
    /// Execution calls it once per block, after the end-of-block steps.
    pub fn commit_block(&mut self) -> Result<Commit> {
        let mut records = vec![];
        for (k, v) in self.meta_records() {
            if self.tree.get(&tree::key_hash(ns::META, k))? != Some(tree::value_hash(&v)) {
                records.push(RecordChange { namespace: ns::META, key: k.to_vec(), value: Some(v) });
            }
        }
        for (n, t) in self.tables_mut() {
            t.take_changes(n, &mut records);
        }
        let mut updates: Vec<(Hash, Option<Hash>)> = records.iter().map(|r| (tree::key_hash(r.namespace, &r.key), r.value.as_deref().map(tree::value_hash))).collect();
        updates.sort_unstable_by_key(|(k, _)| *k);
        let root = self.tree.update(&updates)?;
        Ok(Commit { root, records })
    }

    /// The root computed from scratch over every record: O(state). For checks; execution uses
    /// the incremental [`State::root`].
    pub fn full_root(&self) -> Hash {
        let mut leaves = Vec::with_capacity(self.record_count());
        self.for_each_record(|n, k, v| leaves.push((tree::key_hash(n, k), tree::value_hash(v))));
        leaves.sort_unstable_by_key(|(k, _)| *k);
        tree::root_of(&leaves)
    }

    /// Rebuilds a state from records in canonical order (as [`State::for_each_record`] emits
    /// them), building its tree from scratch. Rejects unknown, duplicate, out-of-order or
    /// non-canonical records and missing scalars; the caller compares the root with a trusted one.
    pub fn from_records(records: impl IntoIterator<Item = Result<(u8, Vec<u8>, Vec<u8>)>>) -> Result<State> {
        let (mut state, leaves) = Self::load_records(records)?;
        state.tree.update(&leaves.into_iter().map(|(k, v)| (k, Some(v))).collect::<Vec<_>>())?;
        Ok(state)
    }

    /// [`State::from_records`] for records whose tree is persisted already (the redb store): loads
    /// them into the maps and takes `tree` as it is instead of building one. Returns the state and
    /// the root of the loaded records computed from scratch, which the caller compares with the
    /// tree's.
    pub fn from_stored_records(records: impl IntoIterator<Item = Result<(u8, Vec<u8>, Vec<u8>)>>, tree: tree::Smt) -> Result<(State, Hash)> {
        let (mut state, leaves) = Self::load_records(records)?;
        state.tree = tree;
        Ok((state, tree::root_of(&leaves)))
    }

    /// Loads canonical records into a state (no tree yet) and returns it with its leaves, sorted.
    fn load_records(records: impl IntoIterator<Item = Result<(u8, Vec<u8>, Vec<u8>)>>) -> Result<(State, Vec<(Hash, Hash)>)> {
        let mut state = State::default();
        let mut last: Option<(u8, Vec<u8>)> = None;
        let mut scalars = vec![];
        let mut leaves = vec![];
        for r in records {
            let (n, key, value) = r?;
            if let Some(prev) = &last {
                ensure!((prev.0, prev.1.as_slice()) < (n, key.as_slice()), "records out of order at namespace {n}");
            }
            if n == ns::META {
                state.load_meta(&key, &value)?;
                scalars.push((key.clone(), value.clone()));
            } else {
                let Some((_, t)) = state.tables_mut().into_iter().find(|(m, _)| *m == n) else { bail!("unknown record namespace {n}") };
                t.load(&key, &value)?;
            }
            leaves.push((tree::key_hash(n, &key), tree::value_hash(&value)));
            last = Some((n, key));
        }
        let expected: Vec<(Vec<u8>, Vec<u8>)> = state.meta_records().into_iter().map(|(k, v)| (k.to_vec(), v)).collect();
        ensure!(scalars == expected, "missing or non-canonical scalar records");
        leaves.sort_unstable_by_key(|(k, _)| *k);
        Ok((state, leaves))
    }

    pub fn root_hash(&self) -> Hash {
        self.tree.root()
    }

    /// Merkle proof that a record has its committed value (or is absent), against
    /// [`State::root`]. Check it with [`tree::verify`] and the value from [`State::record`].
    pub fn prove(&self, namespace: u8, key: &[u8]) -> Result<Proof> {
        self.tree.prove(&tree::key_hash(namespace, key))
    }

    /// Accounting and consistency: every drop of the supply sits in a balance or an escrow
    /// (open hunts, validator bonds, vouches, jurors, claim bonds, unwithdrawn grants, open
    /// Séance budgets), and records that refer to each other agree. `Err` names the first problem.
    pub fn check_invariants(&self) -> Result<()> {
        let p = &self.poc;
        let mut held: u128 = self.accounts.values().map(|a| a.balance as u128).sum();
        let open_hunts: Vec<&crate::chain::Hunt> = self.hunts.values().filter(|h| h.status == HuntStatus::Open).collect();
        held += open_hunts.iter().map(|h| h.amount as u128).sum::<u128>();
        held += self.validators.values().map(|v| v.bond as u128).sum::<u128>();
        held += p.vouches.values().map(|v| v.bond as u128).sum::<u128>();
        held += p.jurors.values().map(|r| r.bond as u128).sum::<u128>();
        held += p.claims.values().map(|c| c.bond as u128).sum::<u128>();
        for (who, gs) in p.grants.iter() {
            ensure!(!gs.is_empty(), "empty grant list for {who}");
            for g in gs.values() {
                ensure!(g.withdrawn <= g.amount, "grant #{} of {who} withdrew more than it holds", g.id);
                ensure!(g.id < p.next_grant_id, "grant #{} is past next_grant_id", g.id);
                held += (g.amount - g.withdrawn) as u128;
            }
        }
        held += p.seances.values().filter(|s| !s.closed).map(|s| s.budget as u128).sum::<u128>();
        ensure!(held == self.supply as u128, "supply is {} drops but balances and escrows hold {held}", self.supply);

        for h in open_hunts {
            let funded = h.funders.values().map(|v| v.parse::<u128>().unwrap_or(u128::MAX)).sum::<u128>();
            ensure!(funded == h.amount as u128, "hunt #{} escrows {} but its funders put in {funded}", h.id, h.amount);
        }
        for (id, h) in self.hunts.iter() {
            ensure!(*id == h.id && *id < self.next_hunt_id, "hunt #{id} is misfiled");
        }
        for (n, r) in self.names.iter() {
            ensure!(*n == r.name, "name {n} is filed under the wrong key");
        }
        for (a, v) in self.validators.iter() {
            ensure!(*a == v.address, "validator {a} is filed under the wrong key");
        }
        for (e, set) in self.active_sets.iter() {
            ensure!(set.iter().all(|a| self.validators.contains_key(a)), "epoch {e}'s set has an unknown validator");
        }
        for (handle, l) in self.github.iter() {
            ensure!(self.github_by_account.get(&l.account) == Some(handle), "GitHub link @{handle} has no reverse entry");
        }
        for (account, handle) in self.github_by_account.iter() {
            ensure!(self.github.get(handle).is_some_and(|l| l.account == *account), "{account}'s GitHub link is dangling");
        }
        for (account, f) in self.familiars.iter() {
            let keys = self.keys_of(account);
            ensure!(!f.is_empty() && f.keys().all(|k| keys.contains_key(k)), "{account} has a Familiar that isn't one of its keys");
        }
        for (id, c) in p.claims.iter() {
            ensure!(*id == c.id && *id < p.next_claim_id, "claim #{id} is misfiled");
            ensure!(c.status.is_open() || c.bond == 0, "closed claim #{id} still holds a bond");
            if c.status == ClaimStatus::InSeance {
                ensure!(c.seance.is_some_and(|s| p.seances.get(&s).is_some_and(|s| !s.closed)), "claim #{id} is in a Séance that isn't open");
            }
        }
        for (pr, id) in p.claim_by_pr.iter() {
            ensure!(p.claims.get(id).is_some_and(|c| format!("{}#{}", c.repo, c.pr) == *pr), "{pr} points at the wrong claim");
        }
        for (id, s) in p.seances.iter() {
            ensure!(*id == s.id && *id < p.next_seance_id, "Séance #{id} is misfiled");
            ensure!(s.closed != (p.open_seance == Some(*id)), "Séance #{id} is open but not the open Séance (or vice versa)");
        }
        if let Some(id) = p.open_seance {
            ensure!(p.seances.contains_key(&id), "the open Séance #{id} doesn't exist");
        }
        for (to, v) in p.vouches.iter() {
            ensure!(v.voucher != *to, "{to} vouches for itself");
        }
        Ok(())
    }

    /// Test builds run this after every block: the incremental root must equal a rebuild from
    /// scratch (skipped past 50k records, which only the benchmarks build) and the invariants
    /// must hold.
    #[cfg(test)]
    pub fn debug_check(&self, height: u64) {
        if self.record_count() <= 50_000 {
            assert_eq!(hex::encode(self.tree.root()), hex::encode(self.full_root()), "incremental state root diverged from a full rebuild at height {height}");
        }
        if let Err(e) = self.check_invariants() {
            panic!("state invariant broken at height {height}: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
