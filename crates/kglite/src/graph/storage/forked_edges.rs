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
//! | `out` / `inn` | per node, the `added` slots in allocation order (read reversed) |
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

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::stable_graph::StableDiGraph;
use petgraph::Direction;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::schema::{EdgeData, NodeData};

/// A set of small integers as a bitmap, grown on demand.
///
/// One bit test answers reads the overlay holds nothing for, which is nearly
/// all of them; a hash probe costs about 2.6x more when the held keys are
/// scattered (measured 2026-10-05, see `forked.rs` `OverlayNodes`).
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

/// An edge the overlay allocated.
#[derive(Clone)]
pub(crate) struct OverlayEdge {
    pub(crate) src: u32,
    pub(crate) dst: u32,
    pub(crate) weight: EdgeData,
}

/// The overlay's edge delta. Empty (`is_clean`) for a fork that only wrote
/// nodes, and then every read takes the base's own iterators.
#[derive(Clone, Default)]
pub(crate) struct EdgeLayer {
    pub(crate) added: FxHashMap<u32, OverlayEdge>,
    out: FxHashMap<u32, Vec<u32>>,
    inn: FxHashMap<u32, Vec<u32>>,
    pub(crate) removed: FxHashSet<u32>,
    pub(crate) weights: FxHashMap<u32, EdgeData>,
    pub(crate) touched: OverlayBits,
    /// One past the highest slot the overlay ever allocated; with the base's
    /// bound it is the slot an append takes when no slot has been vacated.
    alloc_bound: usize,
}

impl EdgeLayer {
    #[inline]
    pub(crate) fn is_clean(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.weights.is_empty()
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

    /// The overlay-added slots incident to `node` in `dir`, oldest first.
    #[inline]
    pub(crate) fn list(&self, node: NodeIndex, dir: Direction) -> &[u32] {
        let lists = match dir {
            Direction::Outgoing => &self.out,
            Direction::Incoming => &self.inn,
        };
        lists.get(&(node.index() as u32)).map_or(&[], Vec::as_slice)
    }

    /// Register an edge the overlay allocated at `slot`.
    pub(crate) fn insert_added(&mut self, slot: u32, src: u32, dst: u32, weight: EdgeData) {
        self.added.insert(slot, OverlayEdge { src, dst, weight });
        self.out.entry(src).or_default().push(slot);
        self.inn.entry(dst).or_default().push(slot);
        self.touched.set(slot);
        self.alloc_bound = self.alloc_bound.max(slot as usize + 1);
    }

    /// Drop an overlay-added edge, unlinking it from both lists. Searches from
    /// the newest end: rollback and a tail delete remove what was added last.
    pub(crate) fn take_added(&mut self, slot: u32) -> Option<OverlayEdge> {
        let edge = self.added.remove(&slot)?;
        for (lists, node) in [(&mut self.out, edge.src), (&mut self.inn, edge.dst)] {
            if let Some(list) = lists.get_mut(&node) {
                if let Some(pos) = list.iter().rposition(|&s| s == slot) {
                    list.remove(pos);
                }
                if list.is_empty() {
                    lists.remove(&node);
                }
            }
        }
        Some(edge)
    }

    /// Mark a live base edge as no longer live in this view.
    pub(crate) fn tombstone(&mut self, slot: u32) {
        self.removed.insert(slot);
        self.weights.remove(&slot);
        self.touched.set(slot);
    }

    /// The copy-on-write weight of a base edge, copied on first touch.
    pub(crate) fn weight_mut<'a>(
        &'a mut self,
        slot: u32,
        base_weight: &EdgeData,
    ) -> &'a mut EdgeData {
        self.touched.set(slot);
        self.weights
            .entry(slot)
            .or_insert_with(|| base_weight.clone())
    }

    /// The weight of `slot` as this view reads it.
    #[inline]
    pub(crate) fn weight<'a>(
        &'a self,
        base: &'a StableDiGraph<NodeData, EdgeData>,
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
        base: &StableDiGraph<NodeData, EdgeData>,
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
