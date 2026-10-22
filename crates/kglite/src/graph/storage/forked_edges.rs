//! The adjacency half of the fork overlay: edges added, removed or re-weighted
//! behind a base that a reader still holds.
//!
//! ## Shape
//!
//! `StableDiGraph` threads adjacency through per-node linked lists with head
//! insertion, so a node's edges read newest-first and a removal only unlinks.
//! The folded graph's read order is therefore exactly "edges added since the
//! fork, newest first, then the base's own list minus the removed ones". That
//! sentence is the whole read contract; every iterator in
//! [`forked_edge_iters`](super::forked_edge_iters) implements it, and the
//! randomised model test pins it against a graph that never forks.
//!
//! | state | holds |
//! |---|---|
//! | `added` | edges the overlay allocated: slot -> endpoints + weight |
//! | `out_head` / `in_head` | per node, the newest `added` edge; older ones follow the chain through the edges themselves |
//! | `removed` | base slots that are not live in this view |
//! | `weights` | base edges whose weight was written (copy-on-write) |
//! | `touched` | a superset gate: one bit test answers "may the base be wrong about this slot?" |
//!
//! A slot can sit in `removed` and in `added` at once: the overlay freed a base
//! edge and then reused its slot. Base reads consult `removed`; overlay reads
//! consult `added`; neither sees the other's entry.
//!
//! Slot identity (the issue #195 class) is not decided here: the slot an edge
//! takes comes from the fork's `SlotMirror`, and the fold replays the operation
//! log against the base and checks every slot petgraph hands back
//! (`forked.rs`, "Slot identity").

use std::sync::Arc;

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::Direction;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::schema::EdgeData;
use crate::graph::storage::fork_base::ForkBase;

/// A set of small integers as a flat bitmap, grown on demand.
///
/// One bit test answers reads the overlay holds nothing for, which is nearly
/// all of them; a hash probe costs about 2.6x more when the held keys are
/// scattered (measured 2026-10-05, see `forked.rs` `OverlayNodes`). Flat rather
/// than paged because a node scan tests it once per node, where a second
/// dependent load measured +12 % on a scan under a held overlay.
#[derive(Clone, Default)]
pub(crate) struct OverlayBits {
    words: Vec<u64>,
}

impl OverlayBits {
    #[inline]
    pub(crate) fn holds(&self, idx: u32) -> bool {
        self.words
            .get((idx >> 6) as usize)
            .is_some_and(|word| (word >> (idx & 63)) & 1 == 1)
    }

    pub(crate) fn set(&mut self, idx: u32) {
        let word = (idx >> 6) as usize;
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        self.words[word] |= 1 << (idx & 63);
    }

    pub(crate) fn clear(&mut self, idx: u32) {
        if let Some(word) = self.words.get_mut((idx >> 6) as usize) {
            *word &= !(1 << (idx & 63));
        }
    }
}

/// Words per page of a [`PagedBits`]: 4096 bits, 512 bytes.
const PAGE_WORDS: usize = 64;
const PAGE_SHIFT: u32 = 12;

/// A sparse set of integers as a paged bitmap. See [`OverlayBits`] for the dense kind.
///
/// One bit test answers reads the overlay holds nothing for, which is nearly
/// all of them; a hash probe costs about 2.6x more when the held keys are
/// scattered (measured 2026-10-05, see `forked.rs` `OverlayNodes`). The pages
/// are allocated only where a bit is set and are shared between clones until
/// one of them writes: a transaction clones the overlay, and a flat bitmap
/// sized to the highest index (12M edge slots: 1.5 MB), or a page copied per
/// scattered entry, would be paid by every one however few bits it sets.
#[derive(Clone, Default)]
pub(crate) struct PagedBits {
    pages: Vec<Option<Arc<[u64; PAGE_WORDS]>>>,
}

impl PagedBits {
    #[inline]
    pub(crate) fn holds(&self, idx: u32) -> bool {
        match self.pages.get((idx >> PAGE_SHIFT) as usize) {
            Some(Some(page)) => (page[((idx >> 6) as usize) % PAGE_WORDS] >> (idx & 63)) & 1 == 1,
            _ => false,
        }
    }

    pub(crate) fn set(&mut self, idx: u32) {
        let page = (idx >> PAGE_SHIFT) as usize;
        if page >= self.pages.len() {
            self.pages.resize_with(page + 1, || None);
        }
        let words =
            Arc::make_mut(self.pages[page].get_or_insert_with(|| Arc::new([0; PAGE_WORDS])));
        words[((idx >> 6) as usize) % PAGE_WORDS] |= 1 << (idx & 63);
    }
}

/// End of an overlay adjacency chain.
const NIL: u32 = u32::MAX;

/// An edge the overlay allocated, linked into the chain of the edges added at
/// its source and at its target (each newest first).
#[derive(Clone)]
pub(crate) struct OverlayEdge {
    pub(crate) src: u32,
    pub(crate) dst: u32,
    pub(crate) weight: EdgeData,
    next_out: u32,
    next_in: u32,
}

/// One node's overlay-added edges in one direction, newest first.
pub(crate) struct AddedChain<'a> {
    added: &'a FxHashMap<u32, OverlayEdge>,
    next: u32,
    outgoing: bool,
}

impl<'a> Iterator for AddedChain<'a> {
    type Item = (u32, &'a OverlayEdge);

    #[inline]
    fn next(&mut self) -> Option<(u32, &'a OverlayEdge)> {
        if self.next == NIL {
            return None;
        }
        let slot = self.next;
        let edge = self
            .added
            .get(&slot)
            .expect("a chained overlay edge is always in the added map");
        self.next = if self.outgoing {
            edge.next_out
        } else {
            edge.next_in
        };
        Some((slot, edge))
    }
}

/// The overlay's edge delta. Empty (`is_clean`) for a fork that only wrote
/// nodes, and then every read takes the base's own iterators.
#[derive(Clone, Default)]
pub(crate) struct EdgeLayer {
    pub(crate) added: FxHashMap<u32, OverlayEdge>,
    /// Per node, the newest added edge at it; the rest follow the chain in
    /// [`OverlayEdge`]. Chains instead of a `Vec` per node, so cloning an
    /// overlay (which a transaction does) allocates nothing per node.
    out_head: FxHashMap<u32, u32>,
    in_head: FxHashMap<u32, u32>,
    pub(crate) removed: FxHashSet<u32>,
    pub(crate) weights: FxHashMap<u32, EdgeData>,
    pub(crate) touched: PagedBits,
    /// Nodes an overlay edge, a removed base edge or a re-weighted base edge is
    /// incident to. A node outside it reads exactly as the base reads it, so
    /// its edges come straight from the base's own iterator.
    dirty_nodes: PagedBits,
    /// One past the highest slot the overlay ever allocated; with the base's
    /// bound it is the slot an append takes when no slot has been vacated.
    alloc_bound: usize,
}

impl EdgeLayer {
    #[inline]
    pub(crate) fn is_clean(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.weights.is_empty()
    }

    /// Whether `node` has any edge the overlay changed or added. `false` is a
    /// proof that the base's edges at `node` are this view's edges.
    #[inline]
    pub(crate) fn touches(&self, node: NodeIndex) -> bool {
        self.dirty_nodes.holds(node.index() as u32)
    }

    /// Edges live in this view minus the base's: `added - removed`.
    #[inline]
    pub(crate) fn count_delta(&self) -> isize {
        self.added.len() as isize - self.removed.len() as isize
    }

    /// Where an `add_edge` appends when the free list is empty: past every slot
    /// the overlay ever took, and past the base's.
    #[inline]
    pub(crate) fn append_bound(&self, base_bound: usize) -> usize {
        base_bound.max(self.alloc_bound)
    }

    /// One past the highest overlay-added slot, or 0.
    pub(crate) fn added_bound(&self) -> usize {
        self.added.keys().max().map_or(0, |&slot| slot as usize + 1)
    }

    /// The overlay-added edges incident to `node` in `dir`, newest first.
    #[inline]
    pub(crate) fn chain(&self, node: NodeIndex, dir: Direction) -> AddedChain<'_> {
        let heads = match dir {
            Direction::Outgoing => &self.out_head,
            Direction::Incoming => &self.in_head,
        };
        AddedChain {
            added: &self.added,
            next: heads.get(&(node.index() as u32)).copied().unwrap_or(NIL),
            outgoing: dir == Direction::Outgoing,
        }
    }

    /// Register an edge the overlay allocated at `slot`.
    pub(crate) fn insert_added(&mut self, slot: u32, src: u32, dst: u32, weight: EdgeData) {
        let next_out = self.out_head.insert(src, slot).unwrap_or(NIL);
        let next_in = self.in_head.insert(dst, slot).unwrap_or(NIL);
        self.added.insert(
            slot,
            OverlayEdge {
                src,
                dst,
                weight,
                next_out,
                next_in,
            },
        );
        self.touched.set(slot);
        self.dirty_nodes.set(src);
        self.dirty_nodes.set(dst);
        self.alloc_bound = self.alloc_bound.max(slot as usize + 1);
    }

    /// Drop an overlay-added edge, unlinking it from both chains. Rollback and
    /// a tail delete remove what was added last, which heads its chains.
    pub(crate) fn take_added(&mut self, slot: u32) -> Option<OverlayEdge> {
        let edge = self.added.remove(&slot)?;
        self.unlink(slot, edge.src, edge.next_out, true);
        self.unlink(slot, edge.dst, edge.next_in, false);
        Some(edge)
    }

    /// Remove `slot` from `node`'s chain, whose entry after it is `after`.
    fn unlink(&mut self, slot: u32, node: u32, after: u32, outgoing: bool) {
        let heads = if outgoing {
            &mut self.out_head
        } else {
            &mut self.in_head
        };
        let Some(&head) = heads.get(&node) else {
            return;
        };
        if head == slot {
            if after == NIL {
                heads.remove(&node);
            } else {
                heads.insert(node, after);
            }
            return;
        }
        let mut at = head;
        while let Some(edge) = self.added.get_mut(&at) {
            let link = if outgoing {
                &mut edge.next_out
            } else {
                &mut edge.next_in
            };
            if *link == slot {
                *link = after;
                return;
            }
            at = *link;
        }
    }

    /// Mark a live base edge as no longer live in this view.
    pub(crate) fn tombstone(&mut self, slot: u32, src: u32, dst: u32) {
        self.removed.insert(slot);
        self.weights.remove(&slot);
        self.touched.set(slot);
        self.dirty_nodes.set(src);
        self.dirty_nodes.set(dst);
    }

    /// The copy-on-write weight of a base edge, copied on first touch.
    pub(crate) fn weight_mut<'a>(
        &'a mut self,
        slot: u32,
        (src, dst): (u32, u32),
        base_weight: &EdgeData,
    ) -> &'a mut EdgeData {
        self.touched.set(slot);
        self.dirty_nodes.set(src);
        self.dirty_nodes.set(dst);
        self.weights
            .entry(slot)
            .or_insert_with(|| base_weight.clone())
    }

    /// The weight of `slot` as this view reads it.
    #[inline]
    pub(crate) fn weight<'a>(
        &'a self,
        base: &'a ForkBase,
        slot: EdgeIndex,
    ) -> Option<&'a EdgeData> {
        let raw = slot.index() as u32;
        if !self.touched.holds(raw) {
            return base.edge_weight(slot);
        }
        if let Some(edge) = self.added.get(&raw) {
            return Some(&edge.weight);
        }
        if self.removed.contains(&raw) {
            return None;
        }
        self.weights.get(&raw).or_else(|| base.edge_weight(slot))
    }

    /// The endpoints of `slot` as this view reads them.
    #[inline]
    pub(crate) fn endpoints(
        &self,
        base: &ForkBase,
        slot: EdgeIndex,
    ) -> Option<(NodeIndex, NodeIndex)> {
        let raw = slot.index() as u32;
        if !self.touched.holds(raw) {
            return base.edge_endpoints(slot);
        }
        if let Some(edge) = self.added.get(&raw) {
            return Some((
                NodeIndex::new(edge.src as usize),
                NodeIndex::new(edge.dst as usize),
            ));
        }
        if self.removed.contains(&raw) {
            return None;
        }
        base.edge_endpoints(slot)
    }

    /// Overlay-added slots in ascending order, for the whole-graph scans.
    pub(crate) fn added_sorted(&self) -> Vec<u32> {
        let mut slots: Vec<u32> = self.added.keys().copied().collect();
        slots.sort_unstable();
        slots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::schema::InternedKey;

    /// The paged bitmap behaves as a set across page and word boundaries, and
    /// allocates only where a bit is set.
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
        for idx in (0..9000).chain([1 << 20, (1 << 20) + 1, 11_999_999, 12_000_000, 12_000_001]) {
            assert_eq!(bits.holds(idx), held.contains(&idx), "bit {idx}");
        }
        let pages = bits.pages.iter().flatten().count();
        assert_eq!(
            pages, 5,
            "pages 0, 1, 2, 256 and 2929 are the only ones allocated"
        );
        let copy = bits.clone();
        bits.set(12);
        assert!(
            !copy.holds(12) && bits.holds(12),
            "a clone does not see a later write"
        );
        assert!(copy.holds(4097), "and keeps what it held");
    }

    /// Overlay edges chain newest first at both ends, and an edge leaves the
    /// chain whether it is the head, the middle or the tail.
    #[test]
    fn added_edges_chain_newest_first_and_unlink_from_anywhere() {
        let weight = || EdgeData::new_interned(InternedKey::from_u64(1), Vec::new());
        let out_of = |layer: &EdgeLayer, node: usize| -> Vec<u32> {
            layer
                .chain(NodeIndex::new(node), Direction::Outgoing)
                .map(|(slot, _)| slot)
                .collect()
        };
        let into = |layer: &EdgeLayer, node: usize| -> Vec<u32> {
            layer
                .chain(NodeIndex::new(node), Direction::Incoming)
                .map(|(slot, _)| slot)
                .collect()
        };
        let mut layer = EdgeLayer::default();
        // Slots 10..15 leave node 1; 12 and 14 arrive at node 2, the rest at 3;
        // 15 is a self-loop at node 1.
        for (slot, src, dst) in [
            (10, 1, 3),
            (11, 1, 3),
            (12, 1, 2),
            (13, 1, 3),
            (14, 1, 2),
            (15, 1, 1),
        ] {
            layer.insert_added(slot, src, dst, weight());
        }
        assert_eq!(out_of(&layer, 1), [15, 14, 13, 12, 11, 10]);
        assert_eq!(into(&layer, 3), [13, 11, 10]);
        assert_eq!(
            into(&layer, 1),
            [15],
            "a self-loop is on both of its node's chains"
        );
        for taken in [13, 15, 10, 14, 11, 12] {
            let expected_out: Vec<u32> = out_of(&layer, 1)
                .into_iter()
                .filter(|&s| s != taken)
                .collect();
            let expected_in3: Vec<u32> = into(&layer, 3)
                .into_iter()
                .filter(|&s| s != taken)
                .collect();
            let expected_in2: Vec<u32> = into(&layer, 2)
                .into_iter()
                .filter(|&s| s != taken)
                .collect();
            assert!(layer.take_added(taken).is_some());
            assert!(layer.take_added(taken).is_none(), "an edge leaves once");
            assert_eq!(out_of(&layer, 1), expected_out, "after taking {taken}");
            assert_eq!(into(&layer, 3), expected_in3, "after taking {taken}");
            assert_eq!(into(&layer, 2), expected_in2, "after taking {taken}");
        }
        assert!(layer.added.is_empty() && out_of(&layer, 1).is_empty());
        assert!(
            layer.out_head.is_empty() && layer.in_head.is_empty(),
            "emptied chains leave no head"
        );
    }
}
