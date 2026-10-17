//! Tracked state maps and their undo journal.
//!
//! [`TMap`] is an `imbl::OrdMap` (clones share structure, so cloning `State` is O(1)) that also
//! remembers which keys changed since the last commit: the state root is then updated from just
//! those keys. Every mutating method marks the key, so no mutation path can forget to. Reads go
//! through `Deref` to the inner map; there is deliberately no `DerefMut`.
//!
//! [`JMap`] is the journal side: the old value of every touched key (first touch wins) plus
//! whether the key was already dirty, so a rollback restores both the value and the dirty mark.
//! That keeps mempool dry runs and proposals (apply, then roll back) from leaving keys dirty.
//!
//! A map may also keep an [`Aggregate`] of its values up to date (a running sum and an index of
//! keys), so end-of-block code reads totals in O(1) instead of walking the map. Like the dirty
//! marks, it's maintained by the mutating methods themselves, so no mutation path can skip it.

use imbl::{OrdMap, OrdSet};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Deref;

type Entry<'a, K, V> = imbl::ordmap::Entry<'a, K, V, imbl::shared_ptr::DefaultSharedPtr>;

/// What a [`TMap`] keeps up to date about its values: the sum of a weight and the set of keys a
/// predicate accepts. Derived data, outside the records and the root: rebuilt as records load,
/// and `State::check_invariants` compares it with a full recomputation. The default, `()`, keeps
/// nothing and costs nothing.
pub trait Aggregate<V> {
    /// Whether the map aggregates at all.
    const ON: bool = true;
    fn weight(_v: &V) -> u128 {
        0
    }
    fn indexed(_v: &V) -> bool {
        false
    }
}

impl<V> Aggregate<V> for () {
    const ON: bool = false;
}

pub struct TMap<K, V, A = ()> {
    map: OrdMap<K, V>,
    dirty: OrdSet<K>,
    /// The aggregate over every key not in `pending`: Σ weight, and the keys `A::indexed` accepts.
    total: u128,
    index: OrdSet<K>,
    /// Keys handed out mutably (`get_mut`, `entry`) since the last [`TMap::settle`]: their old
    /// contribution came out then, the new one goes in when they settle (readers count them).
    pending: OrdSet<K>,
    _aggregate: PhantomData<fn() -> A>,
}

impl<K, V, A> Default for TMap<K, V, A> {
    fn default() -> Self {
        Self { map: OrdMap::new(), dirty: OrdSet::new(), total: 0, index: OrdSet::new(), pending: OrdSet::new(), _aggregate: PhantomData }
    }
}

impl<K: Clone, V: Clone, A> Clone for TMap<K, V, A> {
    fn clone(&self) -> Self {
        Self { map: self.map.clone(), dirty: self.dirty.clone(), total: self.total, index: self.index.clone(), pending: self.pending.clone(), _aggregate: PhantomData }
    }
}

impl<K: Ord + std::fmt::Debug, V: std::fmt::Debug, A> std::fmt::Debug for TMap<K, V, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.map.fmt(f)
    }
}

/// Same JSON as the `BTreeMap` it replaced (API shapes don't change).
impl<K: Ord + serde::Serialize, V: serde::Serialize, A> serde::Serialize for TMap<K, V, A> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.map.serialize(s)
    }
}

impl<K, V, A> Deref for TMap<K, V, A> {
    type Target = OrdMap<K, V>;
    fn deref(&self) -> &OrdMap<K, V> {
        &self.map
    }
}

#[cfg(not(test))]
impl<'a, K: Ord, V, A> IntoIterator for &'a TMap<K, V, A> {
    type Item = (&'a K, &'a V);
    type IntoIter = imbl::ordmap::Iter<'a, K, V, imbl::shared_ptr::DefaultSharedPtr>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

// Tests count the records that reads walk through (`visits`): what tells a range read from a
// scan of the whole map (audit F57: slashing a validator must read only its own entries). In
// test builds a map's `iter`, `range`, `keys` and `values` are these counting wrappers; in the
// node they're the inner map's own, through `Deref`.
#[cfg(test)]
thread_local! {
    static VISITS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Records every map read on this thread has walked through so far (tests).
#[cfg(test)]
pub fn visits() -> u64 {
    VISITS.with(|v| v.get())
}

#[cfg(test)]
fn count<T>(_: &T) {
    VISITS.with(|v| v.set(v.get() + 1));
}

#[cfg(test)]
type Counted<I> = std::iter::Inspect<I, fn(&<I as Iterator>::Item)>;

#[cfg(test)]
type Ptr = imbl::shared_ptr::DefaultSharedPtr;

#[cfg(test)]
impl<'a, K: Ord, V, A> IntoIterator for &'a TMap<K, V, A> {
    type Item = (&'a K, &'a V);
    type IntoIter = Counted<imbl::ordmap::Iter<'a, K, V, Ptr>>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter().inspect(count)
    }
}

#[cfg(test)]
impl<K: Ord, V, A> TMap<K, V, A> {
    pub fn iter(&self) -> Counted<imbl::ordmap::Iter<'_, K, V, Ptr>> {
        self.map.iter().inspect(count)
    }

    pub fn range<R, Q>(&self, range: R) -> Counted<imbl::ordmap::RangedIter<'_, K, V, Ptr>>
    where
        R: std::ops::RangeBounds<Q>,
        Q: Ord + ?Sized,
        K: Borrow<Q>,
    {
        self.map.range(range).inspect(count)
    }

    pub fn keys(&self) -> Counted<imbl::ordmap::Keys<'_, K, V, Ptr>> {
        self.map.keys().inspect(count)
    }

    pub fn values(&self) -> Counted<imbl::ordmap::Values<'_, K, V, Ptr>> {
        self.map.values().inspect(count)
    }
}

impl<K: Ord + Clone, V: Clone, A: Aggregate<V>> FromIterator<(K, V)> for TMap<K, V, A> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut m = Self::default();
        for (k, v) in iter {
            m.insert(k, v);
        }
        m
    }
}

impl<K: Ord + Clone, V: Clone, A: Aggregate<V>> TMap<K, V, A> {
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.dirty.insert(key.clone());
        self.detach(&key);
        self.attach(&key, &value);
        self.map.insert(key, value)
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        if !self.map.contains_key(key) {
            return None;
        }
        self.detach(key);
        let (k, v) = self.map.remove_with_key(key)?;
        self.dirty.insert(k);
        Some(v)
    }

    /// Mutable access; marks the key dirty whether or not the caller changes it.
    pub fn get_mut<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        let k = self.map.get_key_value(key)?.0.clone();
        self.dirty.insert(k.clone());
        self.detach(key);
        self.defer(k);
        self.map.get_mut(key)
    }

    pub fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        self.dirty.insert(key.clone());
        self.detach(&key);
        self.defer(key.clone());
        self.map.entry(key)
    }

    /// Inserts a value the state tree already commits to (loading a snapshot): not dirty.
    pub fn insert_committed(&mut self, key: K, value: V) -> Option<V> {
        self.detach(&key);
        self.attach(&key, &value);
        self.map.insert(key, value)
    }

    /// Σ `A::weight` over every value. O(keys handed out mutably since the last settle).
    pub fn total(&self) -> u128 {
        self.total + self.pending.iter().filter_map(|k| self.map.get(k)).map(A::weight).sum::<u128>()
    }

    /// The keys whose values `A::indexed` accepts, in order. O(index + pending).
    pub fn indexed(&self) -> Vec<K> {
        let mut keys: Vec<K> = self.index.iter().cloned().collect();
        let before = keys.len();
        keys.extend(self.pending.iter().filter(|k| self.map.get(*k).is_some_and(A::indexed)).cloned());
        if keys.len() > before {
            keys.sort();
        }
        keys
    }

    /// Folds the pending keys' current values into the aggregate (commits do it).
    pub fn settle(&mut self) {
        let TMap { map, total, index, pending, .. } = self;
        for k in std::mem::take(pending) {
            if let Some(v) = map.get(&k) {
                *total += A::weight(v);
                if A::indexed(v) {
                    index.insert(k);
                }
            }
        }
    }

    /// The aggregate recomputed from every value (for checks).
    pub fn full_aggregate(&self) -> (u128, Vec<K>) {
        (self.map.values().map(A::weight).sum(), self.map.iter().filter(|(_, v)| A::indexed(v)).map(|(k, _)| k.clone()).collect())
    }

    /// Takes `key`'s current contribution out of the aggregate (a no-op for pending keys, whose
    /// contribution is out already).
    fn detach<Q: Ord + ?Sized>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
    {
        if !A::ON || self.pending.remove(key).is_some() {
            return;
        }
        if let Some(v) = self.map.get(key) {
            self.total -= A::weight(v);
            self.index.remove(key);
        }
    }

    fn attach(&mut self, key: &K, value: &V) {
        if A::ON {
            self.total += A::weight(value);
            if A::indexed(value) {
                self.index.insert(key.clone());
            }
        }
    }

    fn defer(&mut self, key: K) {
        if A::ON {
            self.pending.insert(key);
        }
    }

    pub fn is_dirty<Q: Ord + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.dirty.contains(key)
    }

    /// Keys changed since the last commit, in order.
    pub fn dirty(&self) -> &OrdSet<K> {
        &self.dirty
    }

    /// Takes the changed keys (the caller commits them), settling the aggregate.
    pub fn take_dirty(&mut self) -> OrdSet<K> {
        self.settle();
        std::mem::take(&mut self.dirty)
    }

    /// Unmarks a key whose value is back to what was last committed (rollback only).
    fn clean(&mut self, key: &K) {
        self.dirty.remove(key);
    }
}

/// Old values of touched entries, for rollback (first touch wins), with their dirty marks.
pub struct JMap<K, V>(HashMap<K, (Option<V>, bool)>);

impl<K, V> Default for JMap<K, V> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: Ord + std::hash::Hash + Clone, V: Clone> JMap<K, V> {
    /// Records `key`'s current value (and dirty mark) unless it was already recorded. Call it
    /// before changing the entry.
    pub fn touch<Q: Ord + std::hash::Hash + Eq + ?Sized + ToOwned<Owned = K>, A: Aggregate<V>>(&mut self, map: &TMap<K, V, A>, key: &Q)
    where
        K: Borrow<Q>,
    {
        if !self.0.contains_key(key) {
            self.0.insert(key.to_owned(), (map.get(key).cloned(), map.is_dirty(key)));
        }
    }

    /// Every touched key with the value it had before (`None`: absent). Unordered: sort before
    /// anything order-dependent.
    pub fn touched(&self) -> impl Iterator<Item = (&K, Option<&V>)> {
        self.0.iter().map(|(k, (old, _))| (k, old.as_ref()))
    }

    /// Folds a nested journal in, keeping the oldest values.
    pub fn absorb(&mut self, other: Self) {
        for (k, v) in other.0 {
            self.0.entry(k).or_insert(v);
        }
    }

    /// Puts every recorded entry back as it was, dirty mark included.
    pub fn restore<A: Aggregate<V>>(self, map: &mut TMap<K, V, A>) {
        for (k, (old, was_dirty)) in self.0 {
            match old {
                Some(v) => {
                    map.insert(k.clone(), v);
                }
                None => {
                    map.remove(&k);
                }
            }
            if !was_dirty {
                map.clean(&k);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutations_mark_keys_and_rollback_unmarks_them() {
        let mut m: TMap<String, u64> = TMap::default();
        m.insert("a".into(), 1);
        m.insert("b".into(), 2);
        assert_eq!(m.take_dirty().len(), 2);
        // Reads don't mark; get_mut, entry and remove do (remove only if present).
        assert_eq!(m.get("a"), Some(&1));
        assert!(m.remove("zzz").is_none());
        assert!(m.dirty().is_empty());
        *m.get_mut("a").unwrap() += 1;
        *m.entry("c".into()).or_default() += 5;
        m.remove("b");
        assert_eq!(m.dirty().iter().cloned().collect::<Vec<_>>(), vec!["a", "b", "c"]);
        m.take_dirty();
        // A journaled change rolled back leaves the key clean again…
        let mut j: JMap<String, u64> = JMap::default();
        j.touch(&m, "a");
        m.insert("a".into(), 99);
        j.touch(&m, "a");
        j.touch(&m, "new");
        m.insert("new".into(), 1);
        j.restore(&mut m);
        assert_eq!((m.get("a"), m.get("new")), (Some(&2), None));
        assert!(m.dirty().is_empty(), "values are back to the committed ones");
        // …but a key that was already dirty stays dirty.
        m.insert("c".into(), 6);
        let mut j: JMap<String, u64> = JMap::default();
        j.touch(&m, "c");
        m.remove("c");
        j.restore(&mut m);
        assert_eq!(m.get("c"), Some(&6));
        assert!(m.is_dirty("c"));
    }

    /// Sums values; indexes the even ones.
    struct Evens;
    impl Aggregate<u64> for Evens {
        fn weight(v: &u64) -> u128 {
            *v as u128
        }
        fn indexed(v: &u64) -> bool {
            v % 2 == 0
        }
    }

    #[test]
    fn the_aggregate_follows_every_mutation_and_rollback() {
        let mut m: TMap<u64, u64, Evens> = TMap::default();
        let check = |m: &TMap<u64, u64, Evens>| assert_eq!((m.total(), m.indexed()), m.full_aggregate());
        // A deterministic mix of inserts, in-place edits, entries, removes and rolled-back
        // journals, checked after every step and after settling.
        let mut x = 7u64;
        for step in 0..2_000u64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let (k, v) = ((x >> 33) % 50, (x >> 13) % 1_000);
            let mut j: JMap<u64, u64> = JMap::default();
            let journaled = step % 7 == 0;
            if journaled {
                j.touch(&m, &k);
            }
            match step % 5 {
                0 => {
                    m.insert(k, v);
                }
                1 => {
                    if let Some(old) = m.get_mut(&k) {
                        *old = v;
                    }
                }
                2 => *m.entry(k).or_default() += v,
                3 => {
                    m.remove(&k);
                }
                _ => {
                    // Handed out mutably but left alone.
                    let _ = m.entry(k);
                }
            }
            check(&m);
            if journaled {
                j.restore(&mut m);
                check(&m);
            }
            if step % 11 == 0 {
                m.settle();
                assert!(m.pending.is_empty());
                check(&m);
            }
            if step % 13 == 0 {
                // A clone (what readers get) carries the aggregate with it.
                let c = m.clone();
                check(&c);
            }
        }
        // Loading committed values builds it too.
        let mut loaded: TMap<u64, u64, Evens> = TMap::default();
        for (k, v) in m.iter() {
            loaded.insert_committed(*k, *v);
        }
        assert_eq!((loaded.total(), loaded.indexed()), (m.total(), m.indexed()));
        // Maps without an aggregate keep nothing.
        let mut plain: TMap<u64, u64> = TMap::default();
        *plain.entry(1).or_default() += 5;
        assert_eq!((plain.total(), plain.indexed(), plain.pending.len()), (0, vec![], 0));
    }

    #[test]
    fn json_matches_a_btreemap() {
        let m: TMap<u64, String> = [(2, "b".to_string()), (1, "a".to_string())].into_iter().collect();
        let b: std::collections::BTreeMap<u64, String> = m.iter().map(|(k, v)| (*k, v.clone())).collect();
        assert_eq!(serde_json::to_string(&m).unwrap(), serde_json::to_string(&b).unwrap());
    }

    #[test]
    fn clones_are_independent() {
        let mut a: TMap<u64, u64> = (0..1000).map(|i| (i, i)).collect();
        let b = a.clone();
        a.insert(5, 0);
        assert_eq!((a[&5], b[&5]), (0, 5));
    }
}
