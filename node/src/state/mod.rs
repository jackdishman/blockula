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

// `tree::verify` (what light clients run), `full_root` and `check_invariants` are exercised by
// tests and the fuzz harness rather than by the node itself.
#![cfg_attr(not(test), allow(dead_code))]

pub mod codec;
pub mod tmap;
pub mod tree;

pub use tmap::{Aggregate, JMap, TMap};
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
    /// Withdrawn vouches that still count (`poc::DroppedVouch`), by `vouchee/voucher`.
    pub const DROPPED_VOUCHES: u8 = 26;
    /// GitHub handles governance holds for an account (`poc::HandleHold`), by lowercase handle.
    pub const HANDLE_HOLDS: u8 = 27;

    // 32–47: the protocol v1 modules, each in its own range so they're built in parallel without
    // colliding. Names say the intended use; a module may repurpose a number in its own range
    // before launch, never after. A module that needs more takes the next free number from 64 up
    // (48–63 are Ghost Mode's).
    /// staking.rs (P-STAKE): 32–36.
    pub const STAKE_POOLS: u8 = 32;
    pub const STAKE_DELEGATIONS: u8 = 33;
    pub const STAKE_UNBONDING: u8 = 34;
    pub const STAKE_EVIDENCE: u8 = 35;
    pub const STAKE_LIVENESS: u8 = 36;
    /// governance.rs (P-GOV): 37–42.
    pub const GOV_PROPOSALS: u8 = 37;
    pub const GOV_VOTES: u8 = 38;
    pub const GOV_MULTISIGS: u8 = 39;
    pub const GOV_MULTISIG_PROPOSALS: u8 = 40;
    pub const GOV_DISTRIBUTIONS: u8 = 41;
    pub const GOV_DISTRIBUTION_CLAIMS: u8 = 42;
    /// econ.rs (P-ECON): 43, the genesis reserved-name list.
    pub const RESERVED_NAMES: u8 = 43;
    /// beacon.rs (P-BEACON): 44–46. Commitments and reveals share a record per epoch (44); 45
    /// holds validators' revert attestations on contribution claims.
    pub const BEACON_COMMITS: u8 = 44;
    pub const BEACON_REVERTS: u8 = 45;
    pub const BEACON_VALUES: u8 = 46;
    /// upgrade.rs (P-UPGRADE): 47, the active feature flags.
    pub const FEATURES: u8 = 47;
    /// ghost.rs (Ghost Mode): 48–52 in use, 53–55 reserved for it, 56–63 kept for a pool
    /// migration (a new versioned pool next to this one).
    pub const GHOST_COMMITMENTS: u8 = 48;
    pub const GHOST_ROOTS: u8 = 49;
    pub const GHOST_RETIRED: u8 = 50;
    pub const GHOST_NULLIFIERS: u8 = 51;
    pub const GHOST_ADDRESSES: u8 = 52;

    // 64 on: modules that outgrew their range.
    /// staking.rs: its unbonding and redelegation entries by the validator they left (so a slash
    /// reads only that validator's), and the highest view executed per epoch (evidence bounds).
    pub const STAKE_ENTRIES_BY_VALIDATOR: u8 = 64;
    pub const STAKE_VIEWS: u8 = 65;
}

/// Keys of the scalar records (namespace [`ns::META`]). They're emitted sorted by key.
pub mod meta {
    pub const BURNED: &[u8] = b"burned";
    /// The governance-adjustable parameters (`params::ChainParams`).
    pub const CHAIN_PARAMS: &[u8] = b"chain_params";
    pub const NEXT_HUNT_ID: &[u8] = b"next_hunt_id";
    /// Genesis constants (`chain::Params`).
    pub const PARAMS: &[u8] = b"params";
    /// `(next_claim_id, next_seance_id, next_grant_id, open_seance, next_seance_at)`.
    pub const POC: &[u8] = b"poc";
    pub const SCHEMA: &[u8] = b"schema";
    pub const SUPPLY: &[u8] = b"supply";
    // One key per protocol v1 module, for its scalars (if it has any) as one record.
    pub const STAKING: &[u8] = b"staking";
    pub const GOVERNANCE: &[u8] = b"governance";
    pub const ECON: &[u8] = b"econ";
    pub const BEACON: &[u8] = b"beacon";
    /// The upgrade schedule and its withdrawals (`upgrade::Upgrades::schedule`, `withdrawals`).
    pub const UPGRADE: &[u8] = b"upgrade";
    /// Ghost Mode's scalars (`ghost::GhostMeta`).
    pub const GHOST: &[u8] = b"ghost";
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

/// A protocol v1 module's slice of the state (staking.rs, governance.rs, econ.rs, beacon.rs,
/// upgrade.rs) as records: its maps, each under a namespace from the module's range in [`ns`],
/// and its scalars as META records under the module's key in [`meta`]. `State` lists the modules
/// once, so a module adds a map or a scalar by editing only its own file.
pub trait SubState {
    fn tables(&self) -> Vec<(u8, &dyn Table)>;
    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)>;
    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        vec![]
    }
    /// Loads one of the module's scalar records: `Ok(false)` if `key` isn't one of them.
    fn load_meta(&mut self, _key: &[u8], _value: &[u8]) -> Result<bool> {
        Ok(false)
    }
}

impl<K, V, A> Table for TMap<K, V, A>
where
    K: Ord + Clone + RecordKey,
    V: Clone + BorshSerialize + BorshDeserialize,
    A: Aggregate<V>,
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
    /// The protocol v1 modules, whose maps and scalars are records too.
    fn modules(&self) -> [&dyn SubState; 6] {
        [&self.staking, &self.governance, &self.econ, &self.beacon, &self.upgrade, &self.ghost]
    }

    /// Every map, by namespace, in namespace order. The destructuring is exhaustive on purpose:
    /// a new `State` or `Poc` field doesn't compile until it is listed here as a map (with a new
    /// namespace), as a module (whose own `tables` destructure exhaustively), or skipped as a
    /// scalar that `meta_records` covers, so nothing escapes the root.
    fn tables(&self) -> Vec<(u8, &dyn Table)> {
        let State { accounts, names, supply: _, burned: _, validators, active_sets, params: _, chain_params: _, hunts, next_hunt_id: _, github, github_by_account, familiars, poc, staking, governance, econ, beacon, upgrade, ghost, tree: _ } = self;
        let Poc { repos, souls, vouches, dropped_vouches, sybils, claims, claim_by_pr, handles, handle_holds, jurors, seances, grants, next_claim_id: _, next_seance_id: _, next_grant_id: _, open_seance: _, next_seance_at: _ } = poc;
        let mut t: Vec<(u8, &dyn Table)> = vec![
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
            (ns::DROPPED_VOUCHES, dropped_vouches),
            (ns::HANDLE_HOLDS, handle_holds),
        ];
        let modules: [&dyn SubState; 6] = [staking, governance, econ, beacon, upgrade, ghost];
        for m in modules {
            t.extend(m.tables());
        }
        t.sort_by_key(|(n, _)| *n);
        t
    }

    /// [`State::tables`], mutably (same list, same order).
    fn tables_mut(&mut self) -> Vec<(u8, &mut dyn Table)> {
        let State { accounts, names, supply: _, burned: _, validators, active_sets, params: _, chain_params: _, hunts, next_hunt_id: _, github, github_by_account, familiars, poc, staking, governance, econ, beacon, upgrade, ghost, tree: _ } = self;
        let Poc { repos, souls, vouches, dropped_vouches, sybils, claims, claim_by_pr, handles, handle_holds, jurors, seances, grants, next_claim_id: _, next_seance_id: _, next_grant_id: _, open_seance: _, next_seance_at: _ } = poc;
        let mut t: Vec<(u8, &mut dyn Table)> = vec![
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
            (ns::DROPPED_VOUCHES, dropped_vouches),
            (ns::HANDLE_HOLDS, handle_holds),
        ];
        let modules: [&mut dyn SubState; 6] = [staking, governance, econ, beacon, upgrade, ghost];
        for m in modules {
            t.extend(m.tables_mut());
        }
        t.sort_by_key(|(n, _)| *n);
        t
    }

    /// The scalar records, sorted by key. Cheap, so every commit re-checks them all. Exhaustive
    /// like [`State::tables`].
    fn meta_records(&self) -> Vec<(&'static [u8], Vec<u8>)> {
        let State { supply, burned, params, chain_params, next_hunt_id, poc, accounts: _, names: _, validators: _, active_sets: _, hunts: _, github: _, github_by_account: _, familiars: _, staking: _, governance: _, econ: _, beacon: _, upgrade: _, ghost: _, tree: _ } = self;
        let Poc { next_claim_id, next_seance_id, next_grant_id, open_seance, next_seance_at, repos: _, souls: _, vouches: _, dropped_vouches: _, sybils: _, claims: _, claim_by_pr: _, handles: _, handle_holds: _, jurors: _, seances: _, grants: _ } = poc;
        let mut m = vec![
            (meta::BURNED, encode(burned)),
            (meta::CHAIN_PARAMS, encode(chain_params)),
            (meta::NEXT_HUNT_ID, encode(next_hunt_id)),
            (meta::PARAMS, encode(params)),
            (meta::POC, encode(&(next_claim_id, next_seance_id, next_grant_id, open_seance, next_seance_at))),
            (meta::SCHEMA, encode(&SCHEMA_VERSION)),
            (meta::SUPPLY, encode(supply)),
        ];
        for module in self.modules() {
            m.extend(module.meta_records());
        }
        m.sort_by(|a, b| a.0.cmp(b.0));
        m
    }

    fn load_meta(&mut self, key: &[u8], v: &[u8]) -> Result<()> {
        match key {
            meta::BURNED => self.burned = decode(v)?,
            meta::CHAIN_PARAMS => self.chain_params = decode(v)?,
            meta::NEXT_HUNT_ID => self.next_hunt_id = decode(v)?,
            meta::PARAMS => self.params = decode(v)?,
            meta::POC => {
                let p = &mut self.poc;
                (p.next_claim_id, p.next_seance_id, p.next_grant_id, p.open_seance, p.next_seance_at) = decode(v)?;
            }
            meta::SCHEMA => {
                let schema: u32 = decode(v)?;
                ensure!(schema == SCHEMA_VERSION, "state schema {schema}, this binary reads {SCHEMA_VERSION}");
            }
            meta::SUPPLY => self.supply = decode(v)?,
            _ => {
                let modules: [&mut dyn SubState; 6] = [&mut self.staking, &mut self.governance, &mut self.econ, &mut self.beacon, &mut self.upgrade, &mut self.ghost];
                for m in modules {
                    if m.load_meta(key, v)? {
                        return Ok(());
                    }
                }
                bail!("unknown scalar record {}", String::from_utf8_lossy(key))
            }
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
        self.meta_records().len() + self.tables().iter().map(|(_, t)| t.record_count()).sum::<usize>()
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
        // The state machine's caps keep every record readable from a snapshot (`check_bounds`).
        debug_assert!(records.iter().all(|r| r.key.len() <= MAX_KEY_BYTES && r.value.as_ref().is_none_or(|v| v.len() <= MAX_VALUE_BYTES)), "a record outgrew the snapshot limits");
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
        let mut in_order = |n: u8, key: &[u8], value: &[u8]| -> Result<()> {
            if let Some(prev) = &last {
                ensure!((prev.0, prev.1.as_slice()) < (n, key), "records out of order at namespace {n}");
            }
            leaves.push((tree::key_hash(n, key), tree::value_hash(value)));
            last = Some((n, key.to_vec()));
            Ok(())
        };
        // The scalars come first (namespace 0), then the maps' records.
        let mut records = records.into_iter();
        let mut first_map_record = None;
        for r in records.by_ref() {
            let (n, key, value) = r?;
            if n != ns::META {
                first_map_record = Some((n, key, value));
                break;
            }
            in_order(n, &key, &value)?;
            state.load_meta(&key, &value)?;
            scalars.push((key, value));
        }
        let mut tables = state.tables_mut();
        for r in first_map_record.map(Ok).into_iter().chain(records) {
            let (n, key, value) = r?;
            in_order(n, &key, &value)?;
            let Some((_, t)) = tables.iter_mut().find(|(m, _)| *m == n) else { bail!("unknown record namespace {n}") };
            t.load(&key, &value)?;
        }
        drop(tables);
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
    /// (open hunts, stake pools and unbonding entries, vouches (withdrawn ones too, while held), jurors, claim bonds, unwithdrawn
    /// grants, open Séance budgets), and records that refer to each other agree. `Err` names the
    /// first problem.
    pub fn check_invariants(&self) -> Result<()> {
        let p = &self.poc;
        let mut held: u128 = self.accounts.values().map(|a| a.balance as u128).sum();
        let open_hunts: Vec<&crate::chain::Hunt> = self.hunts.values().filter(|h| h.status == HuntStatus::Open).collect();
        held += open_hunts.iter().map(|h| h.amount as u128).sum::<u128>();
        held += p.vouches.values().map(|v| v.bond as u128).sum::<u128>();
        held += p.dropped_vouches.values().map(|d| d.bond as u128).sum::<u128>();
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
        // The protocol v1 modules' escrows: stake and unbonding, proposal deposits, open
        // distributions, the shielded pool.
        held += crate::staking::escrow(self) + crate::governance::escrow(self) + crate::ghost::escrow(self);
        ensure!(held == self.supply as u128, "supply is {} drops but balances and escrows hold {held}", self.supply);
        // Running totals and indexes the maps keep (`tmap::Aggregate`) match a recomputation.
        let full = self.accounts.full_aggregate();
        ensure!((self.accounts.total(), self.accounts.indexed()) == full, "the accounts' running totals (Σ balance, vesting accounts) drifted from the state");
        ensure!(self.staking.pools.total() == self.staking.pools.full_aggregate().0, "the running total of pool stake drifted from the pools");
        ensure!(self.staking.entries.total() == self.staking.entries.full_aggregate().0, "the running total of unbonding drifted from the entries");
        ensure!(self.poc.claims.indexed() == self.poc.claims.full_aggregate().1, "the open-claims index drifted from the claims");
        crate::staking::check_invariants(self)?;
        crate::governance::check_invariants(self)?;
        crate::econ::check_invariants(self)?;
        crate::beacon::check_invariants(self)?;
        crate::upgrade::check_invariants(self)?;
        crate::ghost::check_invariants(self)?;

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
        for (handle, h) in p.handle_holds.iter() {
            ensure!(*handle == handle.to_ascii_lowercase() && !h.account.is_empty(), "handle hold {handle} is misfiled");
        }
        for (key, d) in p.dropped_vouches.iter() {
            ensure!(key.split_once('/').is_some_and(|(to, by)| !to.is_empty() && !by.contains('/') && to != by), "dropped vouch {key} is misfiled");
            ensure!(d.at <= d.dropped_at, "dropped vouch {key} was withdrawn before it was made");
        }
        self.check_bounds()
    }

    /// Every nested collection stays within the cap the state machine enforces for it, which
    /// keeps every record within what a snapshot may carry (audit #11: a record past
    /// [`MAX_VALUE_BYTES`] would make every later snapshot unreadable). [`State::check_record_sizes`]
    /// checks the records themselves.
    fn check_bounds(&self) -> Result<()> {
        use crate::chain::{MAX_HUNT_FUNDERS, MAX_KEYS, MAX_LOCKS};
        use crate::poc::{JURY_SIZE, MAX_CLAIM_ATTESTATIONS, MAX_GRANTS, MAX_SEANCE_CLAIMS, PAIRS_PER_JUROR};
        let p = &self.poc;
        for (a, acct) in self.accounts.iter() {
            ensure!(acct.keys.len() <= MAX_KEYS && acct.locks.len() <= MAX_LOCKS, "{a} holds {} keys and {} locks (max {MAX_KEYS} and {MAX_LOCKS})", acct.keys.len(), acct.locks.len());
        }
        for (id, h) in self.hunts.iter() {
            ensure!(h.funders.len() <= MAX_HUNT_FUNDERS, "hunt #{id} has {} funders (max {MAX_HUNT_FUNDERS})", h.funders.len());
        }
        for (a, gs) in p.grants.iter() {
            ensure!(gs.len() <= MAX_GRANTS, "{a} holds {} grants (max {MAX_GRANTS})", gs.len());
        }
        for (id, c) in p.claims.iter() {
            ensure!(c.attestations.len() <= MAX_CLAIM_ATTESTATIONS, "claim #{id} has {} attestations (max {MAX_CLAIM_ATTESTATIONS})", c.attestations.len());
        }
        for (id, s) in p.seances.iter() {
            ensure!(s.claims.len() <= MAX_SEANCE_CLAIMS && s.jurors.len() <= JURY_SIZE, "Séance #{id} has {} claims and {} jurors", s.claims.len(), s.jurors.len());
            ensure!(s.jurors.values().all(|seat| seat.pairs.len() <= PAIRS_PER_JUROR && seat.spam.len() <= MAX_SEANCE_CLAIMS), "Séance #{id} has an oversized jury seat");
        }
        Ok(())
    }

    /// Every record fits what a snapshot may carry ([`MAX_KEY_BYTES`], [`MAX_VALUE_BYTES`]).
    /// O(state): it encodes every record.
    pub fn check_record_sizes(&self) -> Result<()> {
        let mut too_big = None;
        self.for_each_record(|n, k, v| {
            if too_big.is_none() && (k.len() > MAX_KEY_BYTES || v.len() > MAX_VALUE_BYTES) {
                too_big = Some(format!("record ({n}, {}) is {} + {} bytes", hex::encode(&k[..k.len().min(16)]), k.len(), v.len()));
            }
        });
        match too_big {
            Some(e) => bail!("{e}, past the snapshot limits ({MAX_KEY_BYTES} + {MAX_VALUE_BYTES})"),
            None => Ok(()),
        }
    }

    /// Test builds run this after every block: the incremental root must equal a rebuild from
    /// scratch and every record must fit a snapshot (both skipped past 50k records, which only
    /// the benchmarks build), and the invariants must hold.
    #[cfg(test)]
    pub fn debug_check(&self, height: u64) {
        if self.record_count() <= 50_000 {
            assert_eq!(hex::encode(self.tree.root()), hex::encode(self.full_root()), "incremental state root diverged from a full rebuild at height {height}");
            if let Err(e) = self.check_record_sizes() {
                panic!("record too large at height {height}: {e}");
            }
        }
        if let Err(e) = self.check_invariants() {
            panic!("state invariant broken at height {height}: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
// The SDK's light-client vectors (P-CLIENTS).
#[cfg(test)]
mod proof_vectors;
