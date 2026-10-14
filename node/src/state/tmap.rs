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

use imbl::{OrdMap, OrdSet};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::ops::Deref;

type Entry<'a, K, V> = imbl::ordmap::Entry<'a, K, V, imbl::shared_ptr::DefaultSharedPtr>;

pub struct TMap<K, V> {
    map: OrdMap<K, V>,
    dirty: OrdSet<K>,
}

impl<K, V> Default for TMap<K, V> {
    fn default() -> Self {
        Self { map: OrdMap::new(), dirty: OrdSet::new() }
    }
}

impl<K: Clone, V: Clone> Clone for TMap<K, V> {
    fn clone(&self) -> Self {
        Self { map: self.map.clone(), dirty: self.dirty.clone() }
    }
}

impl<K: Ord + std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for TMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.map.fmt(f)
    }
}

/// Same JSON as the `BTreeMap` it replaced (API shapes don't change).
impl<K: Ord + serde::Serialize, V: serde::Serialize> serde::Serialize for TMap<K, V> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.map.serialize(s)
    }
}

impl<K, V> Deref for TMap<K, V> {
    type Target = OrdMap<K, V>;
    fn deref(&self) -> &OrdMap<K, V> {
        &self.map
    }
}

impl<'a, K: Ord, V> IntoIterator for &'a TMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = imbl::ordmap::Iter<'a, K, V, imbl::shared_ptr::DefaultSharedPtr>;
    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}

impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for TMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut m = Self::default();
        for (k, v) in iter {
            m.insert(k, v);
        }
        m
    }
}

impl<K: Ord + Clone, V: Clone> TMap<K, V> {
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.dirty.insert(key.clone());
        self.map.insert(key, value)
    }

    pub fn remove<Q: Ord + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
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
        self.dirty.insert(k);
        self.map.get_mut(key)
    }

    pub fn entry(&mut self, key: K) -> Entry<'_, K, V> {
        self.dirty.insert(key.clone());
        self.map.entry(key)
    }

    /// Inserts a value the state tree already commits to (loading a snapshot): not dirty.
    pub fn insert_committed(&mut self, key: K, value: V) -> Option<V> {
        self.map.insert(key, value)
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

    /// Takes the changed keys (the caller commits them).
    pub fn take_dirty(&mut self) -> OrdSet<K> {
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
    pub fn touch<Q: Ord + std::hash::Hash + Eq + ?Sized + ToOwned<Owned = K>>(&mut self, map: &TMap<K, V>, key: &Q)
    where
        K: Borrow<Q>,
    {
        if !self.0.contains_key(key) {
            self.0.insert(key.to_owned(), (map.get(key).cloned(), map.is_dirty(key)));
        }
    }

    /// Folds a nested journal in, keeping the oldest values.
    pub fn absorb(&mut self, other: Self) {
        for (k, v) in other.0 {
            self.0.entry(k).or_insert(v);
        }
    }

    /// Puts every recorded entry back as it was, dirty mark included.
    pub fn restore(self, map: &mut TMap<K, V>) {
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
