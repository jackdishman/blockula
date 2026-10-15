//! The note commitment tree: Poseidon2, depth 32, append-only.
//!
//! The node keeps only a [`Frontier`] (33 digests and a counter, O(32) work per append) and a
//! [`RootHistory`] of recent roots that spends may use as their anchor. Wallets and tests use
//! [`FullTree`], which keeps every node so it can produce authentication paths.

use std::collections::VecDeque;
use std::sync::OnceLock;

use crate::field::{DIGEST_BYTES, Digest, ZERO_DIGEST, digest_from_bytes, digest_to_bytes};
use crate::hash::{compress, uncommitted_leaf};

/// Tree depth: room for 2^32 notes.
pub const DEPTH: usize = 32;
/// Number of leaves the tree can hold.
pub const CAPACITY: u64 = 1 << DEPTH;

/// Roots of empty subtrees: `EMPTY[0]` is the uncommitted leaf, `EMPTY[h + 1] =
/// compress(EMPTY[h], EMPTY[h])`.
pub fn empty_roots() -> &'static [Digest; DEPTH + 1] {
    static EMPTY: OnceLock<[Digest; DEPTH + 1]> = OnceLock::new();
    EMPTY.get_or_init(|| {
        let mut out = [uncommitted_leaf(); DEPTH + 1];
        for h in 0..DEPTH {
            out[h + 1] = compress(&out[h], &out[h]);
        }
        out
    })
}

/// The tree is full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeFull;

/// An authentication path: the leaf's position and its 32 siblings, bottom first. Bit `h` of the
/// position says whether the node at height `h` is a right child. Witness data: the position
/// links a spend to its note, so `Debug` prints `MerklePath(..)`.
#[derive(Clone, PartialEq, Eq)]
pub struct MerklePath {
    /// Leaf index.
    pub position: u32,
    /// Sibling at each height, leaf level first.
    pub siblings: [Digest; DEPTH],
}

impl MerklePath {
    /// The root this path gives for `leaf`.
    pub fn root(&self, leaf: &Digest) -> Digest {
        let mut node = *leaf;
        for (h, sibling) in self.siblings.iter().enumerate() {
            node = if (self.position >> h) & 1 == 0 {
                compress(&node, sibling)
            } else {
                compress(sibling, &node)
            };
        }
        node
    }
}

redacted_debug!(MerklePath);

/// The append-only frontier: what the node stores. `branch[h]` holds the pending left node at
/// height `h` when bit `h` of `size` is set, and zero otherwise, so the encoding is canonical.
/// Heights run to `DEPTH`: bit `DEPTH` of `size` is set only when the tree is full
/// (`size == CAPACITY`), and `branch[DEPTH]` then holds the root of the full tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frontier {
    size: u64,
    branch: [Digest; DEPTH + 1],
}

impl Default for Frontier {
    fn default() -> Self {
        Self::new()
    }
}

impl Frontier {
    /// Encoded length: size (u64) ‖ 33 digests (heights 0..=32).
    pub const BYTES: usize = 8 + (DEPTH + 1) * DIGEST_BYTES;

    /// An empty tree.
    pub fn new() -> Self {
        Self { size: 0, branch: [ZERO_DIGEST; DEPTH + 1] }
    }

    /// Number of leaves appended.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Appends a note commitment and returns its position.
    pub fn append(&mut self, leaf: Digest) -> Result<u32, TreeFull> {
        // Positions are u32: the tree is full exactly when `size` no longer fits one.
        let position = u32::try_from(self.size).map_err(|_| TreeFull)?;
        // Fold the new leaf into the complete left subtrees it finishes, one per trailing 1 bit
        // of its position, and park the result one level up. The last leaf (position 2^32 − 1)
        // finishes all 32 and parks the root of the full tree at height DEPTH.
        let carries = position.trailing_ones() as usize;
        let mut node = leaf;
        for pending in &mut self.branch[..carries] {
            node = compress(pending, &node);
            *pending = ZERO_DIGEST;
        }
        self.branch[carries] = node;
        self.size = u64::from(position) + 1;
        Ok(position)
    }

    /// The current root (empty leaves to the right).
    pub fn root(&self) -> Digest {
        if self.size == CAPACITY {
            return self.branch[DEPTH];
        }
        let empty = empty_roots();
        let mut node = empty[0];
        for (h, (branch, empty)) in self.branch[..DEPTH].iter().zip(&empty[..DEPTH]).enumerate() {
            node = if (self.size >> h) & 1 == 1 { compress(branch, &node) } else { compress(&node, empty) };
        }
        node
    }

    /// Canonical encoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::BYTES);
        out.extend_from_slice(&self.size.to_le_bytes());
        for d in &self.branch {
            out.extend_from_slice(&digest_to_bytes(d));
        }
        out
    }

    /// Strict inverse of [`Self::to_bytes`]: `size ≤ CAPACITY`, canonical digests, and every
    /// slot whose bit of `size` is clear is zero. So below capacity the root slot (height
    /// DEPTH) is zero, and at capacity every slot but the root is.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::BYTES {
            return None;
        }
        let size = u64::from_le_bytes(bytes[..8].try_into().ok()?);
        if size > CAPACITY {
            return None;
        }
        let mut branch = [ZERO_DIGEST; DEPTH + 1];
        for (h, (slot, chunk)) in branch.iter_mut().zip(bytes[8..].as_chunks::<DIGEST_BYTES>().0).enumerate() {
            *slot = digest_from_bytes(chunk)?;
            if (size >> h) & 1 == 0 && *slot != ZERO_DIGEST {
                return None;
            }
        }
        Some(Self { size, branch })
    }
}

/// The last `window` roots, newest last. A spend's anchor must be one of them.
#[derive(Clone, Debug)]
pub struct RootHistory {
    window: usize,
    roots: VecDeque<Digest>,
}

impl RootHistory {
    /// Keeps up to `window` roots (`ghost_anchor_window`, e.g. one per block for 1,024 blocks).
    pub fn new(window: usize) -> Self {
        Self { window: window.max(1), roots: VecDeque::new() }
    }

    /// Records a root, dropping the oldest beyond the window.
    pub fn push(&mut self, root: Digest) {
        self.roots.push_back(root);
        while self.roots.len() > self.window {
            self.roots.pop_front();
        }
    }

    /// Whether `root` is a recent root.
    pub fn contains(&self, root: &Digest) -> bool {
        self.roots.contains(root)
    }

    /// The roots, oldest first.
    pub fn roots(&self) -> impl Iterator<Item = &Digest> {
        self.roots.iter()
    }
}

/// A tree that keeps every node, for authentication paths. Memory is ~2 digests per leaf.
#[derive(Clone, Debug, Default)]
pub struct FullTree {
    /// `levels[h][i]` is node `i` at height `h`, with empty leaves to the right.
    levels: Vec<Vec<Digest>>,
}

impl FullTree {
    /// An empty tree.
    pub fn new() -> Self {
        Self { levels: vec![Vec::new(); DEPTH + 1] }
    }

    /// Number of leaves.
    pub fn size(&self) -> u64 {
        self.levels.first().map_or(0, |l| l.len() as u64)
    }

    /// Appends a leaf and returns its position.
    pub fn append(&mut self, leaf: Digest) -> Result<u32, TreeFull> {
        if self.levels.is_empty() {
            *self = Self::new();
        }
        let position = u32::try_from(self.size()).map_err(|_| TreeFull)?;
        self.levels[0].push(leaf);
        let empty = empty_roots();
        let mut idx = position as usize;
        for (h, empty) in empty.iter().enumerate().take(DEPTH) {
            let left = idx & !1;
            let l = self.levels[h][left];
            let r = self.levels[h].get(left + 1).copied().unwrap_or(*empty);
            let parent = compress(&l, &r);
            idx >>= 1;
            if idx < self.levels[h + 1].len() {
                self.levels[h + 1][idx] = parent;
            } else {
                self.levels[h + 1].push(parent);
            }
        }
        Ok(position)
    }

    /// The current root.
    pub fn root(&self) -> Digest {
        self.levels.get(DEPTH).and_then(|l| l.first()).copied().unwrap_or(empty_roots()[DEPTH])
    }

    /// The authentication path of a leaf, or `None` if it isn't in the tree.
    pub fn path(&self, position: u32) -> Option<MerklePath> {
        if u64::from(position) >= self.size() {
            return None;
        }
        let empty = empty_roots();
        let mut siblings = [ZERO_DIGEST; DEPTH];
        let mut idx = position as usize;
        for (h, s) in siblings.iter_mut().enumerate() {
            *s = self.levels[h].get(idx ^ 1).copied().unwrap_or(empty[h]);
            idx >>= 1;
        }
        Some(MerklePath { position, siblings })
    }

    /// The leaf at `position`.
    pub fn leaf(&self, position: u32) -> Option<Digest> {
        self.levels.first()?.get(position as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::xof_to_digest;

    fn leaf(i: u32) -> Digest {
        xof_to_digest("blockula ghost v1 test leaf", &i.to_le_bytes())
    }

    #[test]
    fn frontier_matches_full_tree_and_paths() {
        let mut frontier = Frontier::new();
        let mut full = FullTree::new();
        assert_eq!(frontier.root(), empty_roots()[DEPTH]);
        assert_eq!(full.root(), empty_roots()[DEPTH]);
        for i in 0..37u32 {
            assert_eq!(frontier.append(leaf(i)), Ok(i));
            assert_eq!(full.append(leaf(i)), Ok(i));
            assert_eq!(frontier.root(), full.root(), "after {} leaves", i + 1);
            let decoded = Frontier::from_bytes(&frontier.to_bytes()).unwrap();
            assert_eq!(decoded, frontier);
        }
        for i in 0..37u32 {
            let path = full.path(i).unwrap();
            assert_eq!(path.root(&leaf(i)), full.root());
            assert_ne!(path.root(&leaf(i + 100)), full.root());
        }
        assert!(full.path(37).is_none());
    }

    #[test]
    fn frontier_matches_full_tree_across_carries() {
        // Every size up to 2^10 + 1: long carry chains at 2^k − 1 → 2^k, and the slots they
        // clear.
        let mut frontier = Frontier::new();
        let mut full = FullTree::new();
        for i in 0..(1u32 << 10) + 1 {
            frontier.append(leaf(i)).unwrap();
            full.append(leaf(i)).unwrap();
            assert_eq!(frontier.root(), full.root(), "after {} leaves", i + 1);
        }
        assert_eq!(Frontier::from_bytes(&frontier.to_bytes()), Some(frontier));
    }

    /// The encoding of a frontier with `size` leaves and these slots (no canonicality checks).
    fn encode(size: u64, slots: &[Digest; DEPTH + 1]) -> Vec<u8> {
        let mut out = size.to_le_bytes().to_vec();
        for d in slots {
            out.extend_from_slice(&digest_to_bytes(d));
        }
        out
    }

    /// `x_h`: the root of a height-`h` subtree whose leaves are all `x`.
    fn uniform_roots(x: Digest) -> [Digest; DEPTH + 1] {
        let mut out = [x; DEPTH + 1];
        for h in 0..DEPTH {
            out[h + 1] = compress(&out[h], &out[h]);
        }
        out
    }

    #[test]
    fn frontier_root_is_right_at_the_last_leaf_and_at_capacity() {
        // A tree of 2^32 − 1 leaves, from arbitrary pending left nodes: its root folds them over
        // an empty leaf, and the last append folds them over the new leaf. (Audit finding: the
        // last append used to drop that root, and `root()` then gave the empty tree's.)
        let mut slots = [ZERO_DIGEST; DEPTH + 1];
        for (h, s) in slots[..DEPTH].iter_mut().enumerate() {
            *s = leaf(1000 + h as u32);
        }
        let mut frontier = Frontier::from_bytes(&encode(CAPACITY - 1, &slots)).unwrap();
        let fold = |start: Digest| slots[..DEPTH].iter().fold(start, |node, left| compress(left, &node));
        assert_eq!(frontier.root(), fold(empty_roots()[0]));

        let last = leaf(7);
        assert_eq!(frontier.append(last), Ok(u32::MAX));
        assert_eq!(frontier.size(), CAPACITY);
        assert_eq!(frontier.root(), fold(last));
        assert_ne!(frontier.root(), empty_roots()[DEPTH]);
        // Full: nothing more goes in, and nothing changes.
        let full = frontier.clone();
        assert_eq!(frontier.append(leaf(8)), Err(TreeFull));
        assert_eq!(frontier, full);
        // The full state round-trips with its root.
        let decoded = Frontier::from_bytes(&frontier.to_bytes()).unwrap();
        assert_eq!(decoded.root(), fold(last));
        assert_eq!(decoded, frontier);
    }

    #[test]
    fn frontier_at_capacity_agrees_with_paths() {
        // A tree whose leaves are all `x`, independently: at 2^32 leaves its root is `x_32`, and
        // the authentication paths (the code wallets and the circuit use) agree with the
        // frontier at 2^32 − 1 and 2^32 leaves.
        let x = leaf(42);
        let xs = uniform_roots(x);
        let mut slots = xs;
        slots[DEPTH] = ZERO_DIGEST;
        let mut frontier = Frontier::from_bytes(&encode(CAPACITY - 1, &slots)).unwrap();

        // 2^32 − 1 leaves: the last real leaf is at 2^32 − 2, a left child of an empty leaf
        // and a right child above that.
        let mut siblings = [ZERO_DIGEST; DEPTH];
        siblings.copy_from_slice(&xs[..DEPTH]);
        siblings[0] = empty_roots()[0];
        assert_eq!(frontier.root(), MerklePath { position: u32::MAX - 1, siblings }.root(&x));

        // 2^32 leaves.
        frontier.append(x).unwrap();
        assert_eq!(frontier.root(), xs[DEPTH]);
        siblings[0] = x;
        assert_eq!(frontier.root(), MerklePath { position: u32::MAX, siblings }.root(&x));
        let mut expected = [ZERO_DIGEST; DEPTH + 1];
        expected[DEPTH] = xs[DEPTH];
        assert_eq!(frontier.to_bytes(), encode(CAPACITY, &expected));
    }

    #[test]
    fn frontier_rejects_noncanonical() {
        let mut frontier = Frontier::new();
        frontier.append(leaf(0)).unwrap();
        frontier.append(leaf(1)).unwrap();
        let mut bytes = frontier.to_bytes();
        // size = 2: slot 0 must be zero.
        bytes[8] = 1;
        assert!(Frontier::from_bytes(&bytes).is_none());
        let mut too_big = frontier.to_bytes();
        too_big[..8].copy_from_slice(&(CAPACITY + 1).to_le_bytes());
        assert!(Frontier::from_bytes(&too_big).is_none());

        let d = leaf(9);
        // Below capacity the root slot is zero ...
        let mut slots = [ZERO_DIGEST; DEPTH + 1];
        slots[1] = d;
        assert!(Frontier::from_bytes(&encode(2, &slots)).is_some());
        slots[DEPTH] = d;
        assert!(Frontier::from_bytes(&encode(2, &slots)).is_none());
        let all = [d; DEPTH + 1];
        assert!(Frontier::from_bytes(&encode(CAPACITY - 1, &all)).is_none());
        // ... and at capacity every other slot is.
        let mut root_only = [ZERO_DIGEST; DEPTH + 1];
        root_only[DEPTH] = d;
        assert_eq!(Frontier::from_bytes(&encode(CAPACITY, &root_only)).map(|f| f.root()), Some(d));
        for h in 0..DEPTH {
            let mut extra = root_only;
            extra[h] = d;
            assert!(Frontier::from_bytes(&encode(CAPACITY, &extra)).is_none(), "slot {h}");
        }
        // The old 32-slot encoding, which couldn't hold a full tree's root, is refused.
        assert!(Frontier::from_bytes(&encode(CAPACITY, &root_only)[..8 + DEPTH * DIGEST_BYTES]).is_none());
        assert!(Frontier::from_bytes(&encode(0, &[ZERO_DIGEST; DEPTH + 1])[..8 + DEPTH * DIGEST_BYTES]).is_none());
    }

    #[test]
    fn root_history_window() {
        let mut h = RootHistory::new(2);
        h.push(leaf(1));
        h.push(leaf(2));
        h.push(leaf(3));
        assert!(!h.contains(&leaf(1)));
        assert!(h.contains(&leaf(2)) && h.contains(&leaf(3)));
    }
}
