//! The authenticated state tree: a compact binary sparse Merkle tree over 256-bit key hashes.
//!
//! # Definition (a consensus rule; light clients reimplement exactly this)
//!
//! A record `(namespace, key) → value` is the leaf `k = BLAKE3(namespace ‖ key)`,
//! `v = BLAKE3(value)`. The hash of the subtree holding the leaves `S` whose `k` starts with a
//! given `d`-bit prefix is:
//! - `S` empty: 32 zero bytes ([`EMPTY`]);
//! - one leaf: `BLAKE3(0x00 ‖ k ‖ v)`, whatever `d` is (a lone leaf sits as high as it can);
//! - otherwise `BLAKE3(0x01 ‖ left ‖ right)`, the children being the subtrees whose prefixes
//!   extend this one with bit `d` = 0 and 1 (bit 0 is the most significant bit of byte 0).
//!
//! The root is the subtree at the empty prefix. It is a function of the set of records only, not
//! of the order they were written in (history independence), so a snapshot is checked by
//! rebuilding it, and two implementations agree without sharing any history. [`root_of`] is the
//! definition written as code; [`Smt`] maintains the same root incrementally.
//!
//! A proof lists the sibling hashes from the root down to where the key's path ends: at the
//! key's own leaf (inclusion), or at an empty subtree or another key's lone leaf (exclusion).
//! [`verify`] is pure and needs only BLAKE3, so it ports to the SDK as-is.
//!
//! # Why our own tree and not the `jmt` crate
//!
//! We evaluated `jmt` 0.12.0 (Penumbra's Jellyfish Merkle Tree, generic `SimpleHasher`, so it
//! could hash with BLAKE3). Its logical tree is the same kind of compact binary SMT, so the
//! cost model and proofs would be the same. We didn't take it because:
//! - **Versioned storage we don't want.** Node keys carry a version and every update leaves stale
//!   nodes behind, so the store needs a stale-node index and a pruner, and `TreeReader` must
//!   answer `get_value_option(max_version, key)` (a value history). We keep one version, written
//!   per block in one redb transaction; redb's MVCC read transactions already give readers a
//!   consistent past version for free.
//! - **Format and dependency surface.** Its hash format (domain strings, placeholder hash) and
//!   ~15 dependencies (`ics23`, `hashbrown` 0.13, `itertools` 0.10, `num-derive` 0.3,
//!   `mirai-annotations`, `sha2`, `borsh`, …) would become consensus obligations we can't change.
//!   This file is the whole format, with a reference implementation to test against.
//! - **Portability.** The SDK's light-client verifier is the 20-line [`verify`] below.
//!
//! The cost: we own the data-structure code. It is property-tested against [`root_of`] (random
//! inserts and deletes, every proof checked) and every block in the test suite cross-checks the
//! incremental root against a full rebuild.
//!
//! # Storage
//!
//! Only internal nodes (subtrees with two or more leaves) are stored, keyed by position (depth and
//! prefix, [`NodeKey`]); a leaf lives in its parent's child slot. An update reads and rewrites
//! only the nodes on the paths to the changed keys: O(k log n) for k keys. Reads go through the
//! synchronous [`NodeStore`] trait; writes land in an in-memory overlay (an `imbl` map, so cloning
//! the tree is O(1) and a clone never sees the original's later writes). With no backing store
//! the overlay is the whole tree. P-STATE-B persists it: each block it writes
//! [`Smt::pending_changes`] (with the records) in one redb transaction, then [`Smt::rebase`]s onto
//! the new database snapshot.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub type Hash = [u8; 32];

/// Hash of an empty subtree.
pub const EMPTY: Hash = [0; 32];

/// Tree key of a record.
pub fn key_hash(namespace: u8, key: &[u8]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[namespace]);
    h.update(key);
    *h.finalize().as_bytes()
}

/// Hash of a record's encoded value.
pub fn value_hash(value: &[u8]) -> Hash {
    *blake3::hash(value).as_bytes()
}

pub fn leaf_hash(key: &Hash, value: &Hash) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[0]);
    h.update(key);
    h.update(value);
    *h.finalize().as_bytes()
}

fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(&[1]);
    h.update(left);
    h.update(right);
    *h.finalize().as_bytes()
}

/// Bit `i` of a key hash, most significant first.
fn bit(h: &Hash, i: usize) -> usize {
    ((h[i / 8] >> (7 - i % 8)) & 1) as usize
}

fn with_bit(mut prefix: Hash, i: usize) -> Hash {
    prefix[i / 8] |= 1 << (7 - i % 8);
    prefix
}

/// Whether two key hashes agree on their first `bits` bits.
fn same_prefix(a: &Hash, b: &Hash, bits: usize) -> bool {
    let (whole, rest) = (bits / 8, bits % 8);
    a[..whole] == b[..whole] && (rest == 0 || (a[whole] ^ b[whole]) >> (8 - rest) == 0)
}

/// The root of a set of leaves `(key hash, value hash)`, sorted by key hash with no duplicates,
/// straight from the definition. Used to check [`Smt`] and to rebuild roots from scratch.
pub fn root_of(leaves: &[(Hash, Hash)]) -> Hash {
    fn sub(leaves: &[(Hash, Hash)], depth: usize) -> Hash {
        match leaves {
            [] => EMPTY,
            [(k, v)] => leaf_hash(k, v),
            _ => {
                let split = leaves.partition_point(|(k, _)| bit(k, depth) == 0);
                node_hash(&sub(&leaves[..split], depth + 1), &sub(&leaves[split..], depth + 1))
            }
        }
    }
    debug_assert!(leaves.windows(2).all(|w| w[0].0 < w[1].0), "leaves must be sorted and unique");
    sub(leaves, 0)
}

/// What a child slot of an internal node (or the root) holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Child {
    Empty,
    /// A lone leaf (the only key under this position).
    Leaf { key: Hash, value: Hash },
    /// An internal node, stored at the child's position; this is its hash.
    Node(Hash),
}

impl Child {
    pub fn hash(&self) -> Hash {
        match self {
            Child::Empty => EMPTY,
            Child::Leaf { key, value } => leaf_hash(key, value),
            Child::Node(h) => *h,
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Child::Empty => out.push(0),
            Child::Leaf { key, value } => {
                out.push(1);
                out.extend_from_slice(key);
                out.extend_from_slice(value);
            }
            Child::Node(h) => {
                out.push(2);
                out.extend_from_slice(h);
            }
        }
    }

    /// Strict inverse of the encoding; returns the child and the bytes it used.
    fn decode_from(b: &[u8]) -> Option<(Child, usize)> {
        let hash = |at: usize| -> Option<Hash> { b.get(at..at + 32)?.try_into().ok() };
        match b.first()? {
            0 => Some((Child::Empty, 1)),
            1 => Some((Child::Leaf { key: hash(1)?, value: hash(33)? }, 65)),
            2 => Some((Child::Node(hash(1)?), 33)),
            _ => None,
        }
    }

    /// Binary form (`0` | `1 ‖ key ‖ value` | `2 ‖ hash`), e.g. for persisting the root slot.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![];
        self.encode_into(&mut out);
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self> {
        match Child::decode_from(b) {
            Some((c, n)) if n == b.len() => Ok(c),
            _ => bail!("bad tree child encoding"),
        }
    }
}

/// An internal node: the two subtrees below it, holding at least two leaves between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node(pub [Child; 2]);

impl Node {
    pub fn hash(&self) -> Hash {
        node_hash(&self.0[0].hash(), &self.0[1].hash())
    }

    /// Binary form: both children's encodings, left first.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(130);
        self.0[0].encode_into(&mut out);
        self.0[1].encode_into(&mut out);
        out
    }

    pub fn decode(b: &[u8]) -> Result<Self> {
        let bad = || anyhow!("bad tree node encoding");
        let (l, n) = Child::decode_from(b).ok_or_else(bad)?;
        let (r, m) = Child::decode_from(&b[n..]).ok_or_else(bad)?;
        if n + m != b.len() {
            return Err(bad());
        }
        Ok(Node([l, r]))
    }
}

/// Position of an internal node: its depth and the key-hash prefix leading to it (bits from
/// `depth` on are zero). The root node is at depth 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey {
    pub depth: u8,
    pub prefix: Hash,
}

impl NodeKey {
    fn at(depth: usize, prefix: Hash) -> Self {
        // Two distinct keys differ by bit 255 at the latest, so internal nodes stop at depth 255.
        Self { depth: u8::try_from(depth).expect("internal nodes are at depth < 256"), prefix }
    }

    /// Binary form: `depth ‖ prefix[..⌈depth/8⌉]`. Byte order sorts parents before children.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut out = vec![self.depth];
        out.extend_from_slice(&self.prefix[..(self.depth as usize).div_ceil(8)]);
        out
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let (&depth, bytes) = b.split_first().ok_or_else(|| anyhow!("empty node key"))?;
        if bytes.len() != (depth as usize).div_ceil(8) {
            bail!("bad node key length");
        }
        let mut prefix = EMPTY;
        prefix[..bytes.len()].copy_from_slice(bytes);
        if depth % 8 != 0 && bytes[bytes.len() - 1] << (depth % 8) != 0 {
            bail!("node key has bits past its depth");
        }
        Ok(Self { depth, prefix })
    }
}

/// Read access to persisted tree nodes. Synchronous, like execution. P-STATE-B implements it
/// over a redb read transaction (a consistent snapshot as of the last committed block).
pub trait NodeStore: Send + Sync {
    /// The internal node at `key`, if the store has one.
    fn get(&self, key: &NodeKey) -> Result<Option<Node>>;
}

/// A store with no nodes: the base of a purely in-memory tree.
struct NoNodes;

impl NodeStore for NoNodes {
    fn get(&self, _: &NodeKey) -> Result<Option<Node>> {
        Ok(None)
    }
}

/// The tree. Cloning is O(1); clones evolve independently.
#[derive(Clone)]
pub struct Smt {
    root: Child,
    /// Nodes written since the base was last brought up to date (`None`: deleted from the base).
    /// Boxed so the map's chunks stay small: a write copies the chunk it shares with clones.
    overlay: imbl::HashMap<NodeKey, Option<Arc<Node>>>,
    base: Arc<dyn NodeStore>,
}

impl Default for Smt {
    fn default() -> Self {
        Self { root: Child::Empty, overlay: imbl::HashMap::new(), base: Arc::new(NoNodes) }
    }
}

impl Smt {
    /// A tree whose nodes are in `base`, with root slot `root` (as persisted).
    pub fn open(root: Child, base: Arc<dyn NodeStore>) -> Self {
        Self { root, overlay: imbl::HashMap::new(), base }
    }

    pub fn root(&self) -> Hash {
        self.root.hash()
    }

    /// The root slot, to persist alongside the nodes.
    pub fn root_child(&self) -> Child {
        self.root
    }

    /// Node changes not yet in the base store, in key order (`None`: delete).
    pub fn pending_changes(&self) -> Vec<(NodeKey, Option<Node>)> {
        let mut v: Vec<(NodeKey, Option<Node>)> = self.overlay.iter().map(|(k, n)| (*k, n.as_deref().copied())).collect();
        v.sort_unstable_by_key(|(k, _)| *k);
        v
    }

    /// Switches to a base store that now holds every pending change, and forgets them.
    pub fn rebase(&mut self, base: Arc<dyn NodeStore>) {
        self.base = base;
        self.overlay = imbl::HashMap::new();
    }

    fn node(&self, key: &NodeKey) -> Result<Node> {
        match self.overlay.get(key) {
            Some(Some(n)) => Ok(**n),
            Some(None) => bail!("state tree node {key:?} was deleted"),
            None => self.base.get(key)?.ok_or_else(|| anyhow!("state tree node {key:?} is missing")),
        }
    }

    fn put(&mut self, key: NodeKey, node: Node) -> Child {
        self.overlay.insert(key, Some(Arc::new(node)));
        Child::Node(node.hash())
    }

    fn delete(&mut self, key: NodeKey) -> Result<()> {
        if self.base.get(&key)?.is_some() {
            self.overlay.insert(key, None);
        } else {
            self.overlay.remove(&key);
        }
        Ok(())
    }

    /// Value hash stored for a key hash, if any.
    pub fn get(&self, key: &Hash) -> Result<Option<Hash>> {
        let (mut child, mut depth, mut prefix) = (self.root, 0, EMPTY);
        loop {
            match child {
                Child::Empty => return Ok(None),
                Child::Leaf { key: k, value } => return Ok((k == *key).then_some(value)),
                Child::Node(_) => {
                    let n = self.node(&NodeKey::at(depth, prefix))?;
                    let b = bit(key, depth);
                    if b == 1 {
                        prefix = with_bit(prefix, depth);
                    }
                    child = n.0[b];
                    depth += 1;
                }
            }
        }
    }

    /// Applies writes (`Some(value hash)`) and deletes (`None`), sorted by key hash with no
    /// duplicates, and returns the new root. Deleting an absent key is a no-op.
    pub fn update(&mut self, updates: &[(Hash, Option<Hash>)]) -> Result<Hash> {
        if !updates.windows(2).all(|w| w[0].0 < w[1].0) {
            bail!("tree updates must be sorted by key hash, without duplicates");
        }
        let root = self.root;
        self.root = self.apply(root, 0, EMPTY, updates)?;
        Ok(self.root())
    }

    /// The new content of the slot at (`depth`, `prefix`), currently `child`, after `ups` (all
    /// of whose keys have that prefix).
    fn apply(&mut self, child: Child, depth: usize, prefix: Hash, ups: &[(Hash, Option<Hash>)]) -> Result<Child> {
        if ups.is_empty() {
            return Ok(child);
        }
        match child {
            Child::Empty => {
                let leaves: Vec<(Hash, Hash)> = ups.iter().filter_map(|(k, v)| v.map(|v| (*k, v))).collect();
                Ok(self.build(depth, prefix, &leaves))
            }
            Child::Leaf { key, value } => {
                if let [(k, Some(v))] = ups
                    && (*k, *v) == (key, value)
                {
                    return Ok(child);
                }
                // A lone leaf has no stored nodes below it: merge and build the subtree afresh.
                let mut leaves: Vec<(Hash, Hash)> = ups.iter().filter_map(|(k, v)| v.map(|v| (*k, v))).collect();
                if ups.binary_search_by(|(k, _)| k.cmp(&key)).is_err() {
                    let at = leaves.partition_point(|(k, _)| *k < key);
                    leaves.insert(at, (key, value));
                }
                Ok(self.build(depth, prefix, &leaves))
            }
            Child::Node(_) => {
                let pos = NodeKey::at(depth, prefix);
                let node = self.node(&pos)?;
                let split = ups.partition_point(|(k, _)| bit(k, depth) == 0);
                let l = self.apply(node.0[0], depth + 1, prefix, &ups[..split])?;
                let r = self.apply(node.0[1], depth + 1, with_bit(prefix, depth), &ups[split..])?;
                if [l, r] == node.0 {
                    return Ok(child);
                }
                match (l, r) {
                    (Child::Empty, Child::Empty) => {
                        self.delete(pos)?;
                        Ok(Child::Empty)
                    }
                    // A lone leaf moves up to the highest slot with no sibling.
                    (leaf @ Child::Leaf { .. }, Child::Empty) | (Child::Empty, leaf @ Child::Leaf { .. }) => {
                        self.delete(pos)?;
                        Ok(leaf)
                    }
                    _ => Ok(self.put(pos, Node([l, r]))),
                }
            }
        }
    }

    /// Stores the subtree holding exactly `leaves` (sorted) at a slot that had nothing stored.
    fn build(&mut self, depth: usize, prefix: Hash, leaves: &[(Hash, Hash)]) -> Child {
        match leaves {
            [] => Child::Empty,
            [(key, value)] => Child::Leaf { key: *key, value: *value },
            _ => {
                let split = leaves.partition_point(|(k, _)| bit(k, depth) == 0);
                let l = self.build(depth + 1, prefix, &leaves[..split]);
                let r = self.build(depth + 1, with_bit(prefix, depth), &leaves[split..]);
                self.put(NodeKey::at(depth, prefix), Node([l, r]))
            }
        }
    }

    /// Proof of inclusion or exclusion for a key hash, against [`Smt::root`].
    pub fn prove(&self, key: &Hash) -> Result<Proof> {
        let (mut child, mut depth, mut prefix) = (self.root, 0, EMPTY);
        let mut siblings = vec![];
        loop {
            match child {
                Child::Empty => return Ok(Proof { siblings, leaf: None }),
                Child::Leaf { key, value } => return Ok(Proof { siblings, leaf: Some(ProofLeaf { key, value }) }),
                Child::Node(_) => {
                    let n = self.node(&NodeKey::at(depth, prefix))?;
                    let b = bit(key, depth);
                    siblings.push(n.0[1 - b].hash());
                    if b == 1 {
                        prefix = with_bit(prefix, depth);
                    }
                    child = n.0[b];
                    depth += 1;
                }
            }
        }
    }
}

/// The leaf a proof's path ends at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofLeaf {
    #[serde(with = "hex32")]
    pub key: Hash,
    #[serde(with = "hex32")]
    pub value: Hash,
}

/// Merkle proof for one key: sibling hashes from the root down, and where the path ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    #[serde(with = "hex32_list")]
    pub siblings: Vec<Hash>,
    /// `None`: an empty subtree (the key is absent). Otherwise a lone leaf: the key's own
    /// (inclusion) or another key with the same prefix (exclusion).
    pub leaf: Option<ProofLeaf>,
}

/// Checks that `(namespace, key)` holds `value` (`None`: is absent) under `root`.
pub fn verify(root: &Hash, namespace: u8, key: &[u8], value: Option<&[u8]>, proof: &Proof) -> bool {
    let k = key_hash(namespace, key);
    let depth = proof.siblings.len();
    if depth > 256 {
        return false;
    }
    let mut h = match (&proof.leaf, value) {
        (Some(l), Some(v)) if l.key == k && l.value == value_hash(v) => leaf_hash(&l.key, &l.value),
        (Some(l), None) if l.key != k && same_prefix(&l.key, &k, depth) => leaf_hash(&l.key, &l.value),
        (None, None) => EMPTY,
        _ => return false,
    };
    for i in (0..depth).rev() {
        h = if bit(&k, i) == 0 { node_hash(&h, &proof.siblings[i]) } else { node_hash(&proof.siblings[i], &h) };
    }
    h == *root
}

mod hex32 {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| D::Error::custom("expected 32 bytes of hex"))
    }
}

mod hex32_list {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[[u8; 32]], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter().map(hex::encode))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<[u8; 32]>, D::Error> {
        Vec::<String>::deserialize(d)?.iter().map(|s| hex::decode(s).ok().and_then(|b| b.try_into().ok()).ok_or_else(|| D::Error::custom("expected 32 bytes of hex"))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    fn kv(i: u32) -> (Hash, Hash) {
        (key_hash(1, &i.to_be_bytes()), value_hash(&i.to_le_bytes()))
    }

    fn model_root(m: &BTreeMap<Hash, Hash>) -> Hash {
        root_of(&m.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>())
    }

    /// Fixed vectors: these change only if the tree format does (a consensus change). The 5-leaf
    /// root was also computed by an independent JS implementation of the definition (noble BLAKE3).
    #[test]
    fn golden_roots() {
        assert_eq!(root_of(&[]), EMPTY);
        let one = kv(1);
        assert_eq!(root_of(&[one]), leaf_hash(&one.0, &one.1));
        let mut leaves: Vec<(Hash, Hash)> = (0..5).map(kv).collect();
        leaves.sort();
        assert_eq!(hex::encode(root_of(&leaves)), "2daa89cfecc65603ee75d4fbdbad2f57d267e26634a7d8d957f39812c786a6b8");
    }

    #[test]
    fn same_prefix_counts_bits() {
        let a = [0b1010_0000; 32];
        let mut b = a;
        b[1] = 0b1011_0000;
        assert!(same_prefix(&a, &b, 11));
        assert!(!same_prefix(&a, &b, 12));
        assert!(same_prefix(&a, &a, 256));
        assert!(same_prefix(&a, &EMPTY, 0));
    }

    #[test]
    fn encodings_round_trip() {
        let n = Node([Child::Leaf { key: [1; 32], value: [2; 32] }, Child::Node([3; 32])]);
        assert_eq!(Node::decode(&n.encode()).unwrap(), n);
        assert!(Node::decode(&[n.encode(), vec![0]].concat()).is_err());
        for c in [Child::Empty, n.0[0], n.0[1]] {
            assert_eq!(Child::decode(&c.encode()).unwrap(), c);
        }
        assert!(Child::decode(&[3]).is_err());
        assert!(Child::decode(&[2, 0]).is_err(), "truncated");
        let k = NodeKey { depth: 9, prefix: with_bit(with_bit(EMPTY, 0), 8) };
        assert_eq!(NodeKey::from_bytes(&k.to_bytes()).unwrap(), k);
        assert!(NodeKey::from_bytes(&[9, 0x80, 0xc0]).is_err(), "bit 9 is past depth 9");
        assert_eq!(NodeKey::from_bytes(&[0]).unwrap(), NodeKey::at(0, EMPTY));
    }

    #[test]
    fn proofs_include_exclude_and_reject_tampering() {
        let mut t = Smt::default();
        let mut leaves: Vec<(Hash, Option<Hash>)> = (0..200u32).map(|i| (key_hash(1, &i.to_be_bytes()), Some(value_hash(&i.to_le_bytes())))).collect();
        leaves.sort();
        let root = t.update(&leaves).unwrap();
        for i in [0u32, 7, 199] {
            let p = t.prove(&key_hash(1, &i.to_be_bytes())).unwrap();
            assert!(verify(&root, 1, &i.to_be_bytes(), Some(&i.to_le_bytes()), &p));
            assert!(!verify(&root, 1, &i.to_be_bytes(), Some(b"tampered"), &p), "wrong value");
            assert!(!verify(&root, 1, &i.to_be_bytes(), None, &p), "a present key isn't absent");
            assert!(!verify(&root, 2, &i.to_be_bytes(), Some(&i.to_le_bytes()), &p), "wrong namespace");
            assert!(!verify(&root, 1, &(i + 1).to_be_bytes(), Some(&i.to_le_bytes()), &p), "wrong key");
            let mut bad = p.clone();
            bad.siblings[0][0] ^= 1;
            assert!(!verify(&root, 1, &i.to_be_bytes(), Some(&i.to_le_bytes()), &bad), "tampered sibling");
            let mut short = p.clone();
            short.siblings.pop();
            assert!(!verify(&root, 1, &i.to_be_bytes(), Some(&i.to_le_bytes()), &short));
        }
        for i in [200u32, 5000] {
            let p = t.prove(&key_hash(1, &i.to_be_bytes())).unwrap();
            assert!(verify(&root, 1, &i.to_be_bytes(), None, &p), "absent key");
            assert!(!verify(&root, 1, &i.to_be_bytes(), Some(b"x"), &p));
            // An exclusion proof for one key doesn't exclude a present one.
            assert!(!verify(&root, 1, &3u32.to_be_bytes(), None, &p));
        }
        // JSON round trip (the API format).
        let p = t.prove(&key_hash(1, &3u32.to_be_bytes())).unwrap();
        let back: Proof = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
        // The empty tree proves everything absent.
        let e = Smt::default();
        assert!(verify(&EMPTY, 1, b"k", None, &e.prove(&key_hash(1, b"k")).unwrap()));
    }

    /// A store backed by a map, to exercise `pending_changes` / `rebase` (what redb will do).
    struct MapStore(BTreeMap<NodeKey, Node>);
    impl NodeStore for MapStore {
        fn get(&self, key: &NodeKey) -> Result<Option<Node>> {
            Ok(self.0.get(key).copied())
        }
    }

    #[test]
    fn flushing_to_a_base_store_keeps_the_tree() {
        let mut t = Smt::default();
        let mut model = BTreeMap::new();
        let mut persisted = BTreeMap::new();
        for round in 0..6u32 {
            let mut ups: Vec<(Hash, Option<Hash>)> = (0..40u32)
                .map(|i| {
                    let (k, v) = kv(i * 7 % 61);
                    if (i + round) % 3 == 0 { (k, None) } else { (k, Some(v)) }
                })
                .collect();
            ups.sort();
            ups.dedup_by(|a, b| a.0 == b.0);
            for (k, v) in &ups {
                match v {
                    Some(v) => model.insert(*k, *v),
                    None => model.remove(k),
                };
            }
            let root = t.update(&ups).unwrap();
            assert_eq!(root, model_root(&model));
            for (k, n) in t.pending_changes() {
                match n {
                    Some(n) => persisted.insert(k, n),
                    None => persisted.remove(&k),
                };
            }
            t.rebase(Arc::new(MapStore(persisted.clone())));
            let reopened = Smt::open(t.root_child(), Arc::new(MapStore(persisted.clone())));
            for (k, v) in &model {
                assert_eq!(reopened.get(k).unwrap(), Some(*v));
            }
        }
        // Exactly the internal nodes of the canonical tree are stored, nothing stale.
        let mut fresh = Smt::default();
        fresh.update(&model.iter().map(|(k, v)| (*k, Some(*v))).collect::<Vec<_>>()).unwrap();
        assert_eq!(fresh.pending_changes().into_iter().map(|(k, n)| (k, n.unwrap())).collect::<BTreeMap<_, _>>(), persisted);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Random batches of writes and deletes: the incremental root always equals the
        /// definition's, the stored nodes are exactly the canonical ones, and every key's proof
        /// (present or not) verifies.
        #[test]
        fn incremental_matches_definition(batches in prop::collection::vec(prop::collection::vec((0u16..300, prop::option::of(0u8..4)), 1..60), 1..8)) {
            let mut t = Smt::default();
            let mut model: BTreeMap<Hash, Hash> = BTreeMap::new();
            for batch in batches {
                let mut ups: BTreeMap<Hash, Option<Hash>> = BTreeMap::new();
                for (k, v) in batch {
                    ups.insert(key_hash(3, &k.to_be_bytes()), v.map(|v| value_hash(&[v])));
                }
                for (k, v) in &ups {
                    match v {
                        Some(v) => model.insert(*k, *v),
                        None => model.remove(k),
                    };
                }
                let ups: Vec<(Hash, Option<Hash>)> = ups.into_iter().collect();
                let clone = t.clone();
                let before = clone.root();
                let root = t.update(&ups).unwrap();
                prop_assert_eq!(root, model_root(&model));
                prop_assert_eq!(clone.root(), before, "clones don't see later writes");
            }
            let mut fresh = Smt::default();
            fresh.update(&model.iter().map(|(k, v)| (*k, Some(*v))).collect::<Vec<_>>()).unwrap();
            prop_assert_eq!(fresh.pending_changes(), t.pending_changes(), "same nodes as a fresh build");
            let root = t.root();
            for k in 0u16..300 {
                let key = k.to_be_bytes();
                let p = t.prove(&key_hash(3, &key)).unwrap();
                let value = model.get(&key_hash(3, &key));
                prop_assert_eq!(t.get(&key_hash(3, &key)).unwrap(), value.copied());
                match value {
                    Some(v) => {
                        let l = p.leaf.unwrap();
                        prop_assert_eq!(l.value, *v);
                        // verify() takes the value bytes; the model only has hashes, so check the
                        // path by hand through a proof for the value hash we know.
                        let mut h = leaf_hash(&l.key, &l.value);
                        for i in (0..p.siblings.len()).rev() {
                            h = if bit(&l.key, i) == 0 { node_hash(&h, &p.siblings[i]) } else { node_hash(&p.siblings[i], &h) };
                        }
                        prop_assert_eq!(h, root);
                        prop_assert!(!verify(&root, 3, &key, None, &p));
                    }
                    None => prop_assert!(verify(&root, 3, &key, None, &p)),
                }
            }
        }
    }
}
