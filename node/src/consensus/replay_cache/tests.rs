//! The replay cache (docs/PROGRAMS-V0.md §6.6; WBS S4c's acceptance), through `consensus::replay`
//! on a dev chain with programs on and over unexecuted chains of its own making.

use super::*;
use crate::consensus::{finalized_from, replay, CBlock, Tip};
use crate::programs::probe::*;
use crate::types::{Action, Tx};

/// An unexecuted chain of `n` blocks on `d`'s tip, each carrying Alice's next call of `ret` on
/// `p`; `stamp` tells forks apart. Newest first.
fn calls_chain(d: &Dev, p: &str, n: u64, stamp: u64) -> Vec<Arc<CBlock>> {
    let alice = keys(ALICE);
    let first = d.c.state.account(&alice.address_with(&d.prefix())).nonce;
    let blocks: Vec<Vec<Tx>> = (0..n).map(|i| d.txs_at(&alice, Some(first + i), 0, vec![calls(vec![d.call(p, "ret", vec![], 0)], vec![])])).collect();
    unexecuted_chain(d, &blocks, stamp)
}

/// `replay` as `propose` and `verify` call it: on `d`'s executed tip, to `chain[from]` (newest
/// first) and the ancestors under it.
fn replay_to(cache: &ReplayCache, d: &Dev, chain: &[Arc<CBlock>], from: usize) -> crate::chain::State {
    replay(cache, Tip::of(&d.c), &chain[from], chain[from..].to_vec()).expect("they extend the executed chain")
}

/// A replay that starts from a cached state gives the state a replay that runs everything gives,
/// which is the state executing the blocks gives: the same records, so the same root.
#[test]
fn cached_and_uncached_replays_give_equal_roots_and_the_executed_chains() {
    let mut d = Dev::new("cache-equal");
    let p = d.deploy(1).remove(0);
    let chain = calls_chain(&d, &p, 4, 0);
    let e = d.c.height();
    // Nothing cached: all four blocks run, and each is cached after it ran.
    let cold = ReplayCache::default();
    let uncached = replay_to(&cold, &d, &chain, 0);
    assert_eq!((cold.served(), cold.heights()), (0, vec![e + 1, e + 2, e + 3, e + 4]));
    // The first two cached: the third and fourth run on the second's state.
    let warm = ReplayCache::default();
    replay_to(&warm, &d, &chain, 2);
    assert_eq!((warm.served(), warm.heights()), (0, vec![e + 1, e + 2]));
    let cached = replay_to(&warm, &d, &chain, 0);
    assert_eq!(warm.served(), 1, "it started from a cached state");
    assert_eq!(warm.heights(), vec![e + 1, e + 2, e + 3, e + 4]);
    assert_eq!(cached.full_root(), uncached.full_root());
    assert_ne!(cached.full_root(), d.c.state.full_root(), "the four calls changed something");
    // All four cached: nothing runs.
    let again = replay_to(&warm, &d, &chain, 0);
    assert_eq!(warm.served(), 2);
    assert_eq!(again.full_root(), uncached.full_root());
    // And it is what executing them reaches.
    for b in chain.iter().rev() {
        d.c.execute(finalized_from(b, None)).unwrap();
    }
    assert_eq!(d.c.state.full_root(), cached.full_root());
}

/// A cached state belongs to its digest, which commits to the block's whole ancestry: an entry for
/// one fork is never used on another, even a fork of the same transactions.
#[test]
fn a_forks_entry_is_never_used_on_another_fork() {
    let mut d = Dev::new("cache-fork");
    let p = d.deploy(1).remove(0);
    let (a, same_txs) = (calls_chain(&d, &p, 3, 0), calls_chain(&d, &p, 3, 1));
    let alice_pays = d.tx(&keys(ALICE), Action::Transfer { to: d.address(BOB), amount: FANG });
    let other = unexecuted_chain(&d, &[vec![alice_pays.clone()], vec![], vec![]], 2);
    let cache = ReplayCache::default();
    let on_a = replay_to(&cache, &d, &a, 0);
    assert_eq!(cache.heights().len(), 3);
    // The same transactions in other blocks: other digests, none served, three more entries.
    let on_same = replay_to(&cache, &d, &same_txs, 0);
    assert_eq!((cache.served(), cache.heights().len()), (0, 6));
    assert_eq!(on_same.full_root(), on_a.full_root(), "the same transactions reach the same records");
    // Other transactions: another state, from nothing cached.
    let on_other = replay_to(&cache, &d, &other, 0);
    assert_eq!(cache.served(), 0);
    assert_ne!(on_other.full_root(), on_a.full_root());
    // The first fork still has its entries.
    assert_eq!(replay_to(&cache, &d, &a, 0).full_root(), on_a.full_root());
    assert_eq!(cache.served(), 1);
}

/// A replay never starts below the executed tip, so entries at or below the executed height go
/// each time it runs.
#[test]
fn entries_at_or_below_the_executed_height_go() {
    let mut d = Dev::new("cache-drop");
    let p = d.deploy(1).remove(0);
    let chain = calls_chain(&d, &p, 3, 0);
    let (e, cache) = (d.c.height(), ReplayCache::default());
    replay_to(&cache, &d, &chain, 0);
    assert_eq!(cache.heights(), vec![e + 1, e + 2, e + 3]);
    for b in chain[1..].iter().rev() {
        d.c.execute(finalized_from(b, None)).unwrap();
    }
    assert_eq!(d.c.height(), e + 2);
    // What's left to replay is the third block, whose entry is cached: it's served, and the
    // executed blocks' entries go.
    let state = replay(&cache, Tip::of(&d.c), &chain[0], vec![chain[0].clone()]).unwrap();
    assert_eq!((cache.heights(), cache.served()), (vec![e + 3], 1));
    d.c.execute(finalized_from(&chain[0], None)).unwrap();
    assert_eq!(d.c.state.full_root(), state.full_root());
}

/// At most [`MAX_REPLAY_CACHED`] entries: the lowest heights go first.
#[test]
fn the_cache_holds_at_most_sixteen_states() {
    assert_eq!(MAX_REPLAY_CACHED, 16);
    let cache = ReplayCache::default();
    for h in (1..=20u64).rev() {
        cache.put([h as u8; 32], h, State::default());
    }
    assert_eq!(cache.heights(), (5..=20).collect::<Vec<u64>>());
    assert!(cache.get(&[4; 32]).is_none() && cache.get(&[5; 32]).is_some());
}

/// A state that hit a program fault is never cached, nor is one built on it, so a fault that was
/// transient is run again; the clean blocks before it are cached.
#[test]
fn a_faulted_replay_is_not_cached() {
    let mut d = Dev::new("cache-fault");
    let p = d.deploy(1).remove(0);
    let chain = calls_chain(&d, &p, 3, 0);
    let (e, cache) = (d.c.height(), ReplayCache::default());
    // The second block's call faults (the oldest block is last in `chain`).
    let faulting = chain[1].parse_txs().remove(0).unwrap().id();
    inject::arm(&faulting, Injected::Panic);
    let faulted = replay_to(&cache, &d, &chain, 0);
    inject::disarm(&faulting);
    assert!(faulted.programs.fault.is_some(), "the replay faulted");
    assert_eq!(cache.heights(), vec![e + 1], "only the block before the fault is cached");
    // Nothing faults now: the replay starts from the cached first block, runs the rest clean, and
    // caches them.
    let clean = replay_to(&cache, &d, &chain, 0);
    assert!(clean.programs.fault.is_none());
    assert_eq!((cache.served(), cache.heights()), (1, vec![e + 1, e + 2, e + 3]));
    assert_eq!(clean.full_root(), replay_to(&ReplayCache::default(), &d, &chain, 0).full_root());
}

/// A chain without programs never consults or fills the cache (§6.6: a v1 chain's proposers and
/// verifiers run today's code exactly): its replay is the same states, with nothing cached.
#[test]
fn the_cache_is_never_used_on_a_chain_without_programs() {
    let mut d = Dev::dormant("cache-v1");
    let alice = keys(ALICE);
    let first = d.c.state.account(&alice.address_with(&d.prefix())).nonce;
    let blocks: Vec<Vec<Tx>> = (0..3).map(|i| d.txs_at(&alice, Some(first + i), 0, vec![Action::Transfer { to: d.address(BOB), amount: FANG }])).collect();
    let chain = unexecuted_chain(&d, &blocks, 0);
    let cache = ReplayCache::default();
    let (once, twice) = (replay_to(&cache, &d, &chain, 0), replay_to(&cache, &d, &chain, 0));
    assert_eq!((cache.served(), cache.heights()), (0, vec![]));
    assert_eq!(once.full_root(), twice.full_root());
    for b in chain.iter().rev() {
        d.c.execute(finalized_from(b, None)).unwrap();
    }
    assert_eq!(d.c.state.full_root(), once.full_root());
}

/// A proposal and a vote share one cache through `App`'s clones: the proposal's replay of the
/// unexecuted parent is what the vote's starts from.
#[test]
fn a_vote_starts_from_what_the_proposal_replayed() {
    let mut d = Dev::new("cache-app");
    let p = d.deploy(1).remove(0);
    let call = d.tx(&keys(ALICE), calls(vec![d.call(&p, "ret", vec![], 0)], vec![]));
    let r = Round2::new(&mut d, std::slice::from_ref(&call));
    assert!(r.cache.heights().is_empty());
    assert!(r.propose().is_some());
    assert_eq!((r.cache.heights(), r.cache.served()), (vec![r.e + 1], 0), "the parent ran once, and was cached");
    assert_eq!(r.verify(r.block(&[])), Some(true));
    assert_eq!(r.cache.served(), 1, "the vote didn't run it again");
    r.give_back(&mut d);
}
