//! The replay cache (docs/PROGRAMS-V0.md §6.6), node-local and never consensus.
//!
//! `consensus::replay` runs the parent's unexecuted ancestors through `State::run_block` on a copy
//! of the executed tip, for every proposal and every vote. A verifier that falls behind therefore
//! re-runs every block it hasn't executed at every vote, and program blocks (the only ones whose
//! execution is expensive) would make falling behind compound. The cache keeps the post-`run_block`
//! state of the ancestors a replay ran, keyed by block digest, so each block runs at most once as
//! an ancestor per validator, which is what the block fuel budget (§5.3) assumes:
//! - a digest commits to the block's whole ancestry, so the state after a block is the same
//!   whichever tip computed it, and a block on another fork has another digest: an entry is never
//!   used on a fork it doesn't belong to;
//! - at most [`MAX_REPLAY_CACHED`] entries (O(1) `imbl` clones that share structure), the lowest
//!   heights out first; entries at or below the executed height go each time a replay runs;
//! - a state whose `programs.fault` is set is never cached, so a fault that was transient is run
//!   again, and a state built on one isn't either;
//! - it's used only while META `programs` exists in the executed tip's state, so a v1 chain's
//!   proposers and verifiers run today's code exactly.

use crate::chain::State;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

/// The most post-states kept: two views' worth of the deepest ancestry a replay runs
/// (`consensus::MAX_REPLAY` is 64; a validator that far behind abstains).
pub const MAX_REPLAY_CACHED: usize = 16;

/// Replays that started from a cached state, since the start: the series
/// `blockula_replay_cache_hits_total`. Process-wide, like the fault counters.
static HITS: AtomicU64 = AtomicU64::new(0);

/// How often this process's replays used a cached state.
pub fn hits() -> u64 {
    HITS.load(Relaxed)
}

struct Entry {
    height: u64,
    state: State,
}

#[derive(Default)]
struct Inner {
    /// Block digest → the state after it ran.
    entries: BTreeMap<[u8; 32], Entry>,
    /// Hits on this cache (what tests read: [`hits`] is the process's).
    hits: u64,
    /// Test builds: the blocks a replay ran and cached here (one `put` each), which a test reads
    /// beside `hits` to see how often a block ran as an ancestor.
    #[cfg(test)]
    ran: u64,
}

/// What `App` holds beside the chain handle: cloned with it, so a proposal and a vote share one.
/// Locked only to look up or store an entry, never while a block runs.
#[derive(Clone, Default)]
pub struct ReplayCache(Arc<Mutex<Inner>>);

impl ReplayCache {
    /// The cache of the node that runs `chain`, which a test finds again with [`ReplayCache::of`].
    pub fn for_chain(chain: &super::Shared) -> ReplayCache {
        let cache = ReplayCache::default();
        #[cfg(test)]
        {
            let mut all = CACHES.lock();
            all.retain(|(c, _)| c.strong_count() > 0);
            all.push((Arc::downgrade(chain), cache.clone()));
        }
        #[cfg(not(test))]
        let _ = chain;
        cache
    }

    /// The cache `chain`'s node was started with ([`ReplayCache::for_chain`]): the simulator reads
    /// each validator's own, since [`hits`] is the process's.
    #[cfg(test)]
    pub fn of(chain: &super::Shared) -> Option<ReplayCache> {
        CACHES.lock().iter().find(|(c, _)| c.upgrade().is_some_and(|c| Arc::ptr_eq(&c, chain))).map(|(_, cache)| cache.clone())
    }

    /// The state after the block `digest`, if cached.
    pub fn get(&self, digest: &[u8; 32]) -> Option<State> {
        let mut inner = self.0.lock();
        let state = inner.entries.get(digest)?.state.clone();
        inner.hits += 1;
        HITS.fetch_add(1, Relaxed);
        Some(state)
    }

    /// Caches `state`, the state after the block `digest` at `height`; past [`MAX_REPLAY_CACHED`]
    /// entries the ones at the lowest heights go.
    pub fn put(&self, digest: [u8; 32], height: u64, state: State) {
        let mut inner = self.0.lock();
        #[cfg(test)]
        {
            inner.ran += 1;
        }
        inner.entries.insert(digest, Entry { height, state });
        while inner.entries.len() > MAX_REPLAY_CACHED {
            let Some(lowest) = inner.entries.iter().min_by_key(|(_, e)| e.height).map(|(d, _)| *d) else { break };
            inner.entries.remove(&lowest);
        }
    }

    /// Drops the entries at or below `executed`: the chain has executed those blocks, and a
    /// replay never starts below its executed tip.
    pub fn drop_through(&self, executed: u64) {
        self.0.lock().entries.retain(|_, e| e.height > executed);
    }

    /// The heights of the cached states, ascending.
    #[cfg(test)]
    pub fn heights(&self) -> Vec<u64> {
        let mut heights: Vec<u64> = self.0.lock().entries.values().map(|e| e.height).collect();
        heights.sort_unstable();
        heights
    }

    /// How many times this cache served a state.
    #[cfg(test)]
    pub fn served(&self) -> u64 {
        self.0.lock().hits
    }

    /// How many blocks replays ran and cached here.
    #[cfg(test)]
    pub fn ran(&self) -> u64 {
        self.0.lock().ran
    }
}

/// Test builds: every node's cache, by the chain it runs ([`ReplayCache::for_chain`]).
#[cfg(test)]
static CACHES: Mutex<Vec<(std::sync::Weak<Mutex<crate::chain::Chain>>, ReplayCache)>> = Mutex::new(Vec::new());

#[cfg(test)]
mod tests;
