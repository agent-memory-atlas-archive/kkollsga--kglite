//! Structurally shared containers for the writer overlay's delta.
//!
//! A transaction forks the overlay, so a clone has to cost the same however
//! many writes the overlay already holds. These containers keep their
//! entries behind two levels of `Arc`ed pages: a clone copies the short top
//! level and shares every page, and the first write to a page after a clone
//! privatises that page (and the mid-level node above it) and nothing else.
//!
//! - [`PagedMap`]: `u32` key to value, for node weights, added edges, chain
//!   heads, tombstones and re-weighted edges.
//! - [`PagedBits`]: a sparse set of `u32`, the bit-test gate in front of them.
//! - [`OpLog`]: the append-only operation log, in sealed shared chunks.
//!
//! The top level has one entry per `2^(LEAF_BITS + MID_BITS)` keys, so a
//! 12M-slot edge space is a handful of entries rather than thousands.

use std::sync::Arc;

use rustc_hash::FxHashMap;

/// Leaves a mid-level node points at: `2^MID_BITS`.
const MID_BITS: u32 = 9;
const MID_SLOTS: usize = 1 << MID_BITS;

/// One mid-level node: pointers to leaf pages.
struct Mid<T> {
    leaves: Box<[Option<Arc<T>>]>,
}

impl<T> Mid<T> {
    fn new() -> Self {
        Self {
            leaves: (0..MID_SLOTS).map(|_| None).collect(),
        }
    }
}

impl<T> Clone for Mid<T> {
    fn clone(&self) -> Self {
        Self {
            leaves: self.leaves.clone(),
        }
    }
}

/// Two levels of shared pages over `u32` keys; a leaf covers `2^LEAF_BITS` keys.
pub(crate) struct Tree2<T, const LEAF_BITS: u32> {
    top: Vec<Option<Arc<Mid<T>>>>,
}

impl<T, const L: u32> Clone for Tree2<T, L> {
    fn clone(&self) -> Self {
        Self {
            top: self.top.clone(),
        }
    }
}

impl<T, const L: u32> Default for Tree2<T, L> {
    fn default() -> Self {
        Self { top: Vec::new() }
    }
}

impl<T: Clone, const L: u32> Tree2<T, L> {
    #[inline]
    fn split(key: u32) -> (usize, usize) {
        let page = (key >> L) as usize;
        (page >> MID_BITS, page & (MID_SLOTS - 1))
    }

    #[inline]
    fn leaf(&self, key: u32) -> Option<&T> {
        let (top, mid) = Self::split(key);
        self.top
            .get(top)?
            .as_ref()?
            .leaves
            .get(mid)?
            .as_ref()
            .map(|leaf| &**leaf)
    }

    /// The leaf holding `key`, privatised along its path; `None` when no leaf
    /// exists there. Never allocates.
    fn leaf_mut_existing(&mut self, key: u32) -> Option<&mut T> {
        let (top, mid) = Self::split(key);
        let node = Arc::make_mut(self.top.get_mut(top)?.as_mut()?);
        Some(Arc::make_mut(node.leaves.get_mut(mid)?.as_mut()?))
    }

    /// The leaf holding `key`, created by `make` when absent.
    fn leaf_mut_or_insert(&mut self, key: u32, make: impl FnOnce() -> T) -> &mut T {
        let (top, mid) = Self::split(key);
        if top >= self.top.len() {
            self.top.resize_with(top + 1, || None);
        }
        let node = Arc::make_mut(self.top[top].get_or_insert_with(|| Arc::new(Mid::new())));
        Arc::make_mut(node.leaves[mid].get_or_insert_with(|| Arc::new(make())))
    }

    /// Every leaf, with the first key it covers.
    fn leaves(&self) -> impl Iterator<Item = (u32, &T)> {
        self.top.iter().enumerate().flat_map(|(t, mid)| {
            mid.iter().flat_map(move |mid| {
                mid.leaves.iter().enumerate().filter_map(move |(m, leaf)| {
                    let first = (((t << MID_BITS) | m) as u64) << L;
                    leaf.as_ref().map(|leaf| (first as u32, &**leaf))
                })
            })
        })
    }

    /// Every leaf, highest first.
    fn leaves_rev(&self) -> impl Iterator<Item = &T> {
        self.top.iter().rev().flat_map(|mid| {
            mid.iter()
                .flat_map(|mid| mid.leaves.iter().rev().filter_map(|leaf| leaf.as_deref()))
        })
    }

    /// Every leaf by value, sharing none: pages another holder still reads are cloned.
    fn into_leaves(self) -> impl Iterator<Item = (u32, T)> {
        self.top.into_iter().enumerate().flat_map(|(t, mid)| {
            let mid =
                mid.map(|mid| Arc::try_unwrap(mid).unwrap_or_else(|shared| (*shared).clone()));
            mid.into_iter().flat_map(move |mid| {
                mid.leaves
                    .into_vec()
                    .into_iter()
                    .enumerate()
                    .filter_map(move |(m, leaf)| {
                        let first = (((t << MID_BITS) | m) as u64) << L;
                        leaf.map(|leaf| {
                            (
                                first as u32,
                                Arc::try_unwrap(leaf).unwrap_or_else(|shared| (*shared).clone()),
                            )
                        })
                    })
            })
        })
    }
}

/// Keys per leaf of a [`PagedMap`]: `2^9`.
const MAP_LEAF_BITS: u32 = 9;

/// A `u32`-keyed map whose clone is O(top level), not O(entries).
pub(crate) struct PagedMap<V> {
    tree: Tree2<FxHashMap<u32, V>, MAP_LEAF_BITS>,
    len: usize,
}

impl<V> Clone for PagedMap<V> {
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
            len: self.len,
        }
    }
}

impl<V> Default for PagedMap<V> {
    fn default() -> Self {
        Self {
            tree: Tree2::default(),
            len: 0,
        }
    }
}

impl<V: Clone> PagedMap<V> {
    #[inline]
    pub(crate) fn get(&self, key: u32) -> Option<&V> {
        self.tree.leaf(key)?.get(&key)
    }

    #[inline]
    pub(crate) fn contains_key(&self, key: u32) -> bool {
        self.get(key).is_some()
    }

    /// Mutable access; privatises the pages on the key's path only when it is held.
    pub(crate) fn get_mut(&mut self, key: u32) -> Option<&mut V> {
        if !self.contains_key(key) {
            return None;
        }
        self.tree.leaf_mut_existing(key)?.get_mut(&key)
    }

    pub(crate) fn insert(&mut self, key: u32, value: V) -> Option<V> {
        let old = self
            .tree
            .leaf_mut_or_insert(key, FxHashMap::default)
            .insert(key, value);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    pub(crate) fn remove(&mut self, key: u32) -> Option<V> {
        if !self.contains_key(key) {
            return None;
        }
        let old = self.tree.leaf_mut_existing(key)?.remove(&key);
        self.len -= 1;
        old
    }

    /// The value at `key`, inserting `make()` first when absent.
    pub(crate) fn get_or_insert_with(&mut self, key: u32, make: impl FnOnce() -> V) -> &mut V {
        if !self.contains_key(key) {
            self.insert(key, make());
        }
        self.get_mut(key).expect("held after insert")
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &V)> {
        self.tree
            .leaves()
            .flat_map(|(_, leaf)| leaf.iter().map(|(&key, value)| (key, value)))
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.iter().map(|(key, _)| key)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, value)| value)
    }

    /// The highest key held, scanning from the top page down.
    pub(crate) fn max_key(&self) -> Option<u32> {
        self.tree
            .leaves_rev()
            .find_map(|leaf| leaf.keys().max().copied())
    }

    /// Take every entry out, leaving the map empty.
    pub(crate) fn drain(&mut self) -> Vec<(u32, V)> {
        self.len = 0;
        std::mem::take(&mut self.tree)
            .into_leaves()
            .flat_map(|(_, leaf)| leaf.into_iter())
            .collect()
    }
}

/// A `u32` set on the same pages; membership is the same cost whatever the size.
#[derive(Clone, Default)]
pub(crate) struct PagedSet {
    map: PagedMap<()>,
}

impl PagedSet {
    #[inline]
    pub(crate) fn contains(&self, key: u32) -> bool {
        self.map.contains_key(key)
    }

    pub(crate) fn insert(&mut self, key: u32) {
        self.map.insert(key, ());
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.map.keys()
    }
}

/// Words per leaf of [`PagedBits`]: 4096 bits, 512 bytes.
const BIT_WORDS: usize = 64;
const BIT_LEAF_BITS: u32 = 12;

/// A sparse set of integers as paged bitmaps, shared between clones until written.
///
/// One bit test answers reads the overlay holds nothing for, which is nearly
/// all of them; a hash probe costs about 2.6x more when the held keys are
/// scattered (measured 2026-10-05). A flat bitmap sized to the highest index
/// (12M edge slots: 1.5 MB) would be copied whole by every clone.
#[derive(Clone, Default)]
pub(crate) struct PagedBits {
    tree: Tree2<[u64; BIT_WORDS], BIT_LEAF_BITS>,
}

impl PagedBits {
    #[inline]
    pub(crate) fn holds(&self, idx: u32) -> bool {
        match self.tree.leaf(idx) {
            Some(words) => (words[((idx >> 6) as usize) % BIT_WORDS] >> (idx & 63)) & 1 == 1,
            None => false,
        }
    }

    pub(crate) fn set(&mut self, idx: u32) {
        let words = self.tree.leaf_mut_or_insert(idx, || [0; BIT_WORDS]);
        words[((idx >> 6) as usize) % BIT_WORDS] |= 1 << (idx & 63);
    }

    /// Pages allocated, for the tests that pin the sparse layout.
    #[cfg(test)]
    pub(crate) fn pages(&self) -> usize {
        self.tree.leaves().count()
    }
}

/// Operations per sealed chunk of an [`OpLog`].
const LOG_CHUNK: usize = 2048;

/// An append-only log whose clone shares every sealed chunk.
///
/// The open chunk is copied by a clone (at most `LOG_CHUNK` entries); a chunk
/// that fills is sealed behind an `Arc` and never written again.
#[derive(Clone)]
pub(crate) struct OpLog<T> {
    sealed: Vec<Arc<[T]>>,
    open: Vec<T>,
}

impl<T> Default for OpLog<T> {
    fn default() -> Self {
        Self {
            sealed: Vec::new(),
            open: Vec::new(),
        }
    }
}

impl<T: Copy> OpLog<T> {
    pub(crate) fn push(&mut self, op: T) {
        self.open.push(op);
        if self.open.len() == LOG_CHUNK {
            let full = std::mem::take(&mut self.open);
            self.sealed.push(Arc::from(full));
        }
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.sealed.len() * LOG_CHUNK + self.open.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.sealed
            .iter()
            .flat_map(|chunk| chunk.iter())
            .chain(self.open.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// Under random inserts, removals and clones, a map reads as the model
    /// map it mirrors, and a clone never sees a later write of its source.
    #[test]
    fn paged_map_matches_a_model_and_clones_are_isolated() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for round in 0..40 {
            let span: u64 = [300, 5_000, 2_000_000, 40_000_000][round % 4];
            let mut map: PagedMap<u64> = PagedMap::default();
            let mut model: std::collections::BTreeMap<u32, u64> = Default::default();
            let mut snapshots: Vec<(PagedMap<u64>, std::collections::BTreeMap<u32, u64>)> = vec![];
            for step in 0..600u64 {
                let key = (xorshift(&mut state) % span) as u32;
                match xorshift(&mut state) % 5 {
                    0 | 1 => assert_eq!(map.insert(key, step), model.insert(key, step)),
                    2 => assert_eq!(map.remove(key), model.remove(&key)),
                    3 => {
                        if let (Some(a), Some(b)) = (map.get_mut(key), model.get_mut(&key)) {
                            *a += 7;
                            *b += 7;
                        }
                    }
                    _ => {
                        if snapshots.len() < 6 {
                            snapshots.push((map.clone(), model.clone()));
                        }
                    }
                }
                assert_eq!(map.get(key), model.get(&key));
                assert_eq!(map.len(), model.len());
            }
            let mut held: Vec<(u32, u64)> = map.iter().map(|(k, v)| (k, *v)).collect();
            held.sort_unstable();
            assert_eq!(
                held,
                model.iter().map(|(&k, &v)| (k, v)).collect::<Vec<_>>()
            );
            for (snap, expected) in &snapshots {
                let mut seen: Vec<(u32, u64)> = snap.iter().map(|(k, v)| (k, *v)).collect();
                seen.sort_unstable();
                assert_eq!(
                    seen,
                    expected.iter().map(|(&k, &v)| (k, v)).collect::<Vec<_>>()
                );
            }
            let mut drained = map.drain();
            drained.sort_unstable();
            assert_eq!(drained, held);
            assert!(map.is_empty() && map.get(0).is_none());
        }
    }

    /// The paged set holds what was set across leaf and mid boundaries and
    /// allocates only where a bit is set; a clone keeps what it had.
    #[test]
    fn paged_bits_hold_exactly_what_was_set_across_pages() {
        let held = [
            0u32,
            1,
            63,
            64,
            4095,
            4096,
            4097,
            8191,
            8192,
            1 << 20,
            (1 << 21) + 5,
            12_000_000,
        ];
        let mut bits = PagedBits::default();
        assert!(
            !bits.holds(0) && !bits.holds(12_000_000),
            "empty holds nothing"
        );
        for &idx in &held {
            bits.set(idx);
        }
        for idx in (0..9000).chain([
            1 << 20,
            (1 << 20) + 1,
            (1 << 21) + 5,
            11_999_999,
            12_000_000,
            12_000_001,
        ]) {
            assert_eq!(bits.holds(idx), held.contains(&idx), "bit {idx}");
        }
        assert_eq!(bits.pages(), 6, "pages 0, 1, 2, 256, 512 and 2929 only");
        let copy = bits.clone();
        bits.set(12);
        assert!(
            !copy.holds(12) && bits.holds(12),
            "a clone does not see a later write"
        );
        assert!(copy.holds(4097), "and keeps what it held");
    }

    /// A log reads back in order across chunk boundaries, and a clone shares
    /// its sealed chunks yet reads only what it held.
    #[test]
    fn op_log_reads_in_order_and_clones_share_sealed_chunks() {
        let mut log: OpLog<u32> = OpLog::default();
        for i in 0..(LOG_CHUNK * 3 + 17) as u32 {
            log.push(i);
        }
        assert_eq!(log.len(), LOG_CHUNK * 3 + 17);
        let copy = log.clone();
        log.push(9);
        assert_eq!(copy.len() + 1, log.len());
        assert!(copy.iter().copied().eq(0..(LOG_CHUNK * 3 + 17) as u32));
        assert!(Arc::ptr_eq(&copy.sealed[0], &log.sealed[0]));
    }
}
