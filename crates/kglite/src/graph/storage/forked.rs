//! `ForkedGraph` — the writer-side copy-on-write overlay over a shared base.
//!
//! ## What this removes
//!
//! Holding any second `Arc<DirGraph>` — a lazy `ResultView`, a `freeze()`, a
//! `Session`, an open `Transaction` — made the next write deep-copy the entire
//! graph. Measured 2026-08-10 at 1M nodes: **36.3 ms** against a 3.0 µs
//! control, of which the backend row was **37.8 ms of a 41.6 ms**
//! `DirGraph::clone` on a plain graph. This module makes that row O(changes).
//!
//! ## The structural fact that forces this shape
//!
//! The copy-on-write has to be **writer-side**. A reader holds
//! `Arc<DirGraph>` and reads it as `&DirGraph` while the writer wants
//! `&mut DirGraph` to the same allocation — that is aliasing UB, and the only
//! escapes are a lock on the read path (over budget in the MATCH loop) or a
//! read guard that makes writes block on a held Python `ResultView` (a
//! deadlock hazard traded for a latency cliff). So the *reader's* graph is
//! left byte-for-byte untouched and the *writer* builds the delta.
//!
//! ## What the overlay covers, and what it deliberately does not
//!
//! | write | forked behaviour |
//! |---|---|
//! | node weight (`SET`, `REMOVE`, labels, title, id) | copied into `nodes` on first touch, O(1) |
//! | `add_node` (`CREATE`, `MERGE` insert) | appended to `nodes` at a predicted index, O(1) |
//! | column-store writes | the overlay owns its own map, O(types) `Arc` bumps at fork |
//! | **edge weight (`SET r.p`), `add_edge`, `remove_node`, `remove_edge`** | **materialise, then proceed** |
//!
//! The last row is a deliberate scope boundary, not an oversight, and its two
//! halves are out of scope for different reasons.
//!
//! `StableDiGraph` threads adjacency through per-node linked lists, so
//! `add_edge` / `remove_node` / `remove_edge` each rewrite *existing* nodes'
//! adjacency — which an overlay cannot express without reimplementing
//! `edges_directed`, `edges_directed_filtered`, `edges_connecting`,
//! `neighbors_*` and `edge_references` as base⊕overlay chains behind their GAT
//! iterator types.
//!
//! An **edge weight** would need that same rewrite for a different reason:
//! those iterators hand out `&EdgeData` borrowed out of the base, so a weight
//! parked in a delta would be served by `edge_weight`'s point lookup and missed
//! by every iterating read — `WHERE r.p = x` silently filtering on the
//! pre-write value, which is what it did until 2026-08-23
//! (`held_reader::an_edge_property_write_under_a_held_reader_reaches_traversal_reads`).
//! Node weights have no such split: `node_indices` is the only node-level
//! iterator and it yields indices, which every caller resolves through
//! `node_weight`.
//!
//! **Because no edit reaches base adjacency or a base edge weight, the overlay
//! needs no edge chaining at all** — traversal and edge iteration delegate
//! straight to the base, only `node_indices` gains a variant, and the read path
//! is unchanged apart from the node-weight probe.
//!
//! `materialise` is exactly today's cost — a base deep clone plus the overlay
//! replayed — so a topology write while a reader is held is no worse than
//! before this module existed, and everything else is O(changes).
//!
//! ## Slot identity, and why forking is *conditional*
//!
//! Rollback guarantees a node or edge comes back on the exact
//! `NodeIndex`/`EdgeIndex` it vacated (`dir_graph/rollback.rs`), and
//! `NodeIndex` is the key of every index structure on `DirGraph`. So the
//! indices the overlay hands out must be the indices the base will produce when
//! the overlay is folded back in. `StableGraph::add_node` reuses free-list
//! slots (LIFO) and offers no index-controlled insertion, so the overlay
//! allocates exactly as `add_node` would: each index is the
//! [`SlotMirror`](super::slot_mirror) prediction — the free-list head while
//! the base has vacated slots, then contiguous slots past every occupied one.
//! Replaying the appends in allocation order then reproduces every index
//! (issue #195 was an overlay that appended past `node_bound()` while slots
//! were still listed).
//!
//! That needs a mirror whose free-list order is known, which is what
//! [`can_fork`] checks. Edges need nothing: the overlay never allocates one.
//! [`ForkedGraph::check_fold`] replays the appends against the fold target's
//! mirror before anything changes, and the mirror's `debug_assert` in
//! `note_node_added` re-checks each prediction against petgraph as the fold
//! runs. A base that cannot be forked falls back to the deep clone, which is
//! slower and never wrong — the same fail-safe direction
//! `rollback::journal_covers` takes.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::stable_graph::StableDiGraph;
use petgraph::visit::NodeIndexable;
use rustc_hash::FxHashMap;

use crate::datatypes::Value;
use crate::graph::core::iterators::{ForkedReusedIndices, GraphNodeIndices};
use crate::graph::schema::{EdgeData, InternedKey, NodeData};
use crate::graph::storage::column_store::ColumnStore;
use crate::graph::storage::undo::{ColumnarPreImages, ColumnarWrite, UndoJournal};
use crate::graph::storage::{GraphRead, GraphWrite, MemoryGraph};

/// A writer's copy-on-write view over a base another holder is still reading.
pub struct ForkedGraph {
    /// The reader's graph. **Never mutated while this exists** — that is the
    /// one unforgivable failure mode in this design, because a reader observing
    /// a writer's edit is silent and unrecoverable. The over-specified
    /// fingerprint in `dir_graph::rollback_tests::held_reader` is the
    /// golden-snapshot guard for it.
    base: Arc<MemoryGraph>,
    /// Node weights that diverge from the base: copies taken on first write,
    /// plus every node appended since the fork. Keyed by raw node index.
    nodes: OverlayNodes,
    /// Which slots the appended nodes took, in the order the fold-back must
    /// replay them.
    appended: Appended,
    /// The overlay's own column-store map, seeded with one `Arc` bump per type
    /// at fork time. Complete, so reads never chain into the base for it.
    column_stores: FxHashMap<InternedKey, Arc<ColumnStore>>,
    /// Statement-scoped inverse-op buffer. Lives *here*, so every undo entry
    /// reverses through this backend's `GraphWrite` and therefore lands in the
    /// overlay — never in the shared base.
    undo: Option<Box<UndoJournal>>,
    /// Continues the base's mirror. Predictions must stay in step across the
    /// fork or the fold-back would allocate different slots.
    slot_mirror: super::slot_mirror::SlotMirror,
}

/// The overlay's node weights, keyed by raw index, with a bitmap in front.
///
/// Every read of a node the overlay does not hold — nearly all of them in a
/// scan — would otherwise pay a hash miss, and `FxHash` misses cost about 2.6×
/// more when the held keys are scattered (reused free-list slots) than when
/// they are one contiguous run (measured 2026-10-05: 2.09 vs 0.80 ms per 1M
/// lookups at 100 keys). One bit test answers those reads instead. The bitmap
/// is allocated on the first insert and grows to the highest index held.
#[derive(Clone, Default)]
struct OverlayNodes {
    map: FxHashMap<u32, NodeData>,
    present: Vec<u64>,
}

impl OverlayNodes {
    #[inline]
    fn holds(&self, idx: u32) -> bool {
        self.present
            .get((idx >> 6) as usize)
            .is_some_and(|word| (word >> (idx & 63)) & 1 == 1)
    }

    #[inline]
    fn get(&self, idx: u32) -> Option<&NodeData> {
        if self.holds(idx) {
            self.map.get(&idx)
        } else {
            None
        }
    }

    #[inline]
    fn get_mut(&mut self, idx: u32) -> Option<&mut NodeData> {
        self.map.get_mut(&idx)
    }

    fn insert(&mut self, idx: u32, data: NodeData) {
        let word = (idx >> 6) as usize;
        if word >= self.present.len() {
            self.present.resize(word + 1, 0);
        }
        self.present[word] |= 1 << (idx & 63);
        self.map.insert(idx, data);
    }

    fn remove(&mut self, idx: u32) -> Option<NodeData> {
        if let Some(word) = self.present.get_mut((idx >> 6) as usize) {
            *word &= !(1 << (idx & 63));
        }
        self.map.remove(&idx)
    }

    fn drain(&mut self) -> impl Iterator<Item = (u32, NodeData)> + '_ {
        self.present.clear();
        self.map.drain()
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

/// The slots an overlay's appended nodes took.
///
/// Allocation has two phases, because `StableGraph::add_node` does: it pops
/// the free list (LIFO) until it is empty, and only then appends past every
/// slot. Once the overlay's mirror runs out of free slots it never gains one
/// (the overlay frees nothing), so every fresh slot follows every reused one.
#[derive(Clone, Default)]
struct Appended {
    /// Slots reused from the base's free list, in allocation order.
    reused_order: Vec<u32>,
    /// The same slots, sorted, for merging into `node_indices`.
    reused: BTreeSet<u32>,
    /// Slots past every base and reused slot, contiguous by construction.
    fresh: Range<u32>,
}

impl Appended {
    fn len(&self) -> usize {
        self.reused_order.len() + self.fresh.len()
    }

    /// One past the highest slot taken, or 0 when none was.
    fn bound(&self) -> usize {
        let reused = self.reused.last().map_or(0, |&idx| idx as usize + 1);
        reused.max(self.fresh.end as usize)
    }

    /// Every slot in allocation order: the order the fold-back replays.
    fn in_order(&self) -> impl Iterator<Item = u32> + '_ {
        self.reused_order.iter().copied().chain(self.fresh.clone())
    }

    fn push(&mut self, idx: u32, reused: bool) {
        if reused {
            self.reused_order.push(idx);
            self.reused.insert(idx);
        } else if self.fresh.is_empty() {
            self.fresh = idx..idx + 1;
        } else {
            debug_assert_eq!(idx, self.fresh.end, "fresh slots must be contiguous");
            self.fresh.end += 1;
        }
    }
}

/// Whether `base` can be shared behind an overlay rather than deep-copied.
///
/// The overlay allocates by the base's slot-mirror prediction (module doc),
/// so the mirror must know petgraph's free-list order. It does unless the
/// graph was adopted with holes — loaded or rebuilt from a `StableDiGraph`
/// whose free-list order is not observable (`SlotMirror::for_adopted_graph`).
pub(crate) fn can_fork(base: &MemoryGraph) -> bool {
    base.slot_mirror.is_synced()
}

impl ForkedGraph {
    pub(super) fn count_incoming_nonself_edges_filtered(
        &self,
        node: NodeIndex,
        conn_type: Option<InternedKey>,
        other_node_type: Option<InternedKey>,
        deadline: Option<std::time::Instant>,
    ) -> Result<usize, String> {
        self.base.count_edges_filtered_impl(
            node,
            petgraph::Direction::Incoming,
            conn_type,
            other_node_type,
            deadline,
            true,
        )
    }

    /// Fork `base` — O(types), no node or edge is copied.
    pub(crate) fn new(base: Arc<MemoryGraph>) -> Self {
        let column_stores = base.column_stores.clone();
        let slot_mirror = base.slot_mirror.clone();
        Self {
            base,
            nodes: OverlayNodes::default(),
            appended: Appended::default(),
            column_stores,
            undo: None,
            slot_mirror,
        }
    }

    /// How many node weights this overlay holds — the only nodes a clone of it
    /// duplicates, and what the `BACKEND_CLONE_NODES` oracle counts for a fork
    /// of a fork.
    #[cfg(test)]
    #[inline]
    pub(crate) fn overlay_node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Check that folding into `target` reproduces every appended index,
    /// without mutating anything.
    ///
    /// Replays the appends in allocation order against a copy of `target`'s
    /// slot mirror: each prediction must be the index the overlay handed out,
    /// and that index must carry its weight. A refusal here leaves both the overlay and
    /// `target` untouched, so a caller can keep serving the overlay instead of
    /// tearing down a half-folded backend (issue #195).
    fn check_fold(&self, target: &MemoryGraph) -> Result<(), String> {
        let mut mirror = target.slot_mirror.clone();
        // What `node_bound()` would read before each replayed `add_node`.
        let mut bound = target.inner().node_bound();
        for idx in self.appended.in_order() {
            if !self.nodes.holds(idx) {
                return Err(format!("appended node {idx} has no weight in the overlay"));
            }
            let idx = idx as usize;
            match mirror.predict_next_node(bound) {
                Some(predicted) if predicted.index() == idx => {
                    mirror.note_node_added(bound, predicted);
                    bound = bound.max(idx + 1);
                }
                other => {
                    return Err(format!(
                        "the overlay handed out node {idx}, but the fold target \
                         would allocate {other:?}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Replay the overlay into `target`, which must be a graph in the base's
    /// exact pre-fork state.
    ///
    /// **This is the fold-back path, and the slot-identity proof lives here.**
    /// [`check_fold`](Self::check_fold) runs first, so a fold that would
    /// allocate different slots is refused before either side changes.
    /// Appended nodes then go back through `GraphWrite::add_node`, whose
    /// `SlotMirror::note_node_added` debug-asserts that petgraph allocated the
    /// index the mirror predicted. The `assert_eq!` below is the release half
    /// of that check: past `check_fold` it can only fire if the mirror itself
    /// disagrees with petgraph, and a different index would silently mis-key
    /// every `DirGraph` index that recorded the overlay's number.
    fn apply_overlay(&mut self, target: &mut MemoryGraph) -> Result<(), String> {
        self.check_fold(target)?;
        // Appended nodes first, in allocation order, so each `add_node` pops
        // the slot the overlay took.
        let appended = std::mem::take(&mut self.appended);
        for idx in appended.in_order() {
            let data = self
                .nodes
                .remove(idx)
                .expect("check_fold proved every appended index carries a weight");
            let actual = GraphWrite::add_node(target, data);
            assert_eq!(
                actual.index() as u32,
                idx,
                "fold-back allocated node {} where the overlay handed out {idx}; \
                 the slot mirror disagrees with petgraph (see storage/slot_mirror.rs)",
                actual.index()
            );
        }
        // Then the copy-on-write weights, which are pure overwrites.
        for (idx, data) in self.nodes.drain() {
            if let Some(slot) = target
                .inner_mut()
                .node_weight_mut(NodeIndex::new(idx as usize))
            {
                *slot = data;
            }
        }
        target.column_stores = std::mem::take(&mut self.column_stores);
        // The base's stores went with the assignment above, so on compaction
        // each overlay store's columns are its own again and folding its heap
        // tail copies the tail only (`column_store/tail.rs`, "Heap tails").
        for store in target.column_stores.values_mut() {
            if store.has_heap_tail() {
                Arc::make_mut(store).fold_heap_tail();
            }
        }
        target.undo = self.undo.take();
        Ok(())
    }

    /// Fold into the base when this writer is the only holder left, and return
    /// the collapsed backend.
    ///
    /// `Err(self)` when a reader is still outstanding, or when the fold would
    /// not reproduce the overlay's indices. Either way the overlay comes back
    /// intact and keeps serving reads and writes.
    ///
    /// The reader dropping is exactly what makes `Arc::get_mut` succeed, so the
    /// common "hold a view, write, drop the view, write again" pattern
    /// self-heals on the next write with no timer and no bookkeeping.
    pub(crate) fn try_compact(mut self: Box<Self>) -> Result<MemoryGraph, Box<Self>> {
        if Arc::get_mut(&mut self.base).is_none() {
            return Err(self);
        }
        if self.check_fold(&self.base).is_err() {
            return Err(self);
        }
        // Sole owner: take the base out and fold into it in place — no node or
        // edge is copied, which is what makes compaction O(changes), not O(V+E).
        let mut owned = Arc::try_unwrap(std::mem::replace(
            &mut self.base,
            Arc::new(MemoryGraph::new()),
        ))
        .unwrap_or_else(|_| unreachable!("get_mut proved unique ownership"));
        self.apply_overlay(&mut owned)
            .unwrap_or_else(|reason| unreachable!("check_fold passed on this base: {reason}"));
        Ok(owned)
    }

    /// Deep-copy the base and fold into the copy. Used when a write cannot be
    /// expressed in the overlay while a reader is still holding the base — the
    /// whole-graph copy the overlay exists to avoid, paid only on that write.
    ///
    /// `Err` when the fold would not reproduce the overlay's indices; the
    /// overlay is then untouched and the copy is dropped.
    pub(crate) fn materialise(&mut self) -> Result<MemoryGraph, String> {
        #[cfg(test)]
        super::backend::note_nodes_copied(self.base.inner().node_count());
        let mut owned = self.base.deep_clone();
        self.apply_overlay(&mut owned)?;
        Ok(owned)
    }

    /// A standalone graph equal to what this overlay reads as, without
    /// disturbing it. For the `Serialize` arm in `backend.rs`, which needs one
    /// concrete `StableDiGraph`.
    pub(crate) fn to_memory_graph(&self) -> Result<MemoryGraph, String> {
        let mut clone = ForkedGraph {
            base: Arc::clone(&self.base),
            nodes: self.nodes.clone(),
            appended: self.appended.clone(),
            column_stores: self.column_stores.clone(),
            undo: None,
            slot_mirror: self.slot_mirror.clone(),
        };
        let mut owned = self.base.deep_clone();
        clone.apply_overlay(&mut owned)?;
        Ok(owned)
    }

    #[inline]
    pub(crate) fn begin_undo(&mut self) {
        self.undo = Some(Box::new(UndoJournal::new()));
    }

    #[inline]
    pub(crate) fn take_undo(&mut self) -> Option<Box<UndoJournal>> {
        self.undo.take()
    }

    #[inline]
    pub(crate) fn undo_journal_mut(&mut self) -> Option<&mut UndoJournal> {
        self.undo.as_deref_mut()
    }

    /// The overlay's copy of a base node, taken on first write.
    ///
    /// This is the single point where a base node stops being shared, and the
    /// reason the base is never mutated: every `&mut NodeData` this backend
    /// hands out points into `self.nodes`.
    #[inline]
    fn cow_node(&mut self, idx: NodeIndex) -> Option<&mut NodeData> {
        let raw = idx.index() as u32;
        if !self.nodes.holds(raw) {
            let base = self.base.inner().node_weight(idx)?.clone();
            self.nodes.insert(raw, base);
        }
        self.nodes.get_mut(raw)
    }

    /// Clone `idx`'s current weight into the journal as its pre-statement
    /// state. Reads through [`GraphRead::node_weight`], i.e. overlay-then-base,
    /// so the pre-image is what *this writer* would have read — not what the
    /// base holds, which may already differ if an earlier statement wrote it.
    #[cold]
    fn capture_node_weight(&mut self, idx: NodeIndex) {
        let current = GraphRead::node_weight(self, idx).cloned();
        if let Some(journal) = self.undo.as_deref_mut() {
            journal.note_node_weight(idx, || current);
        }
    }

    /// Journal the pre-image a property write is about to overwrite — the
    /// overlay's counterpart of `impl_heap_pre_image_capture!`. Same two
    /// shapes, same reasoning (see that macro): a row-storage node journals a
    /// `NodeData` clone, a columnar one journals the prior value of each cell
    /// the write names.
    ///
    /// The store read is `self.column_stores`, the overlay's own map, which is
    /// what the write will mutate — so the pre-image is what *this writer*
    /// would read back, not what the shared base holds.
    #[inline]
    fn capture_property_pre_image(&mut self, idx: NodeIndex, write: ColumnarWrite<'_>) {
        if self.undo.is_none() {
            return;
        }
        let Some(nd) = GraphRead::node_weight(self, idx) else {
            return;
        };
        let columnar = nd
            .properties
            .columnar_row_id()
            .map(|row_id| (nd.node_type, row_id));
        match columnar {
            None => self.capture_node_weight(idx),
            Some((type_key, row_id)) => {
                let captured = self
                    .column_stores
                    .get(&type_key)
                    .map(|store| ColumnarPreImages::capture(store, row_id, write));
                if let (Some(captured), Some(journal)) = (captured, self.undo.as_deref_mut()) {
                    captured.record(journal, type_key, row_id);
                }
            }
        }
    }

    /// The row id of a columnar node, or `None` for row storage. Read before
    /// any copy-on-write so a columnar property write — which changes the
    /// store, never the node — does not needlessly copy a `NodeData`.
    #[inline]
    fn columnar_row_of(&self, idx: NodeIndex) -> Option<(InternedKey, u32)> {
        let nd = GraphRead::node_weight(self, idx)?;
        nd.properties
            .columnar_row_id()
            .map(|row_id| (nd.node_type, row_id))
    }

    #[inline]
    pub(crate) fn base_stable_digraph(&self) -> &StableDiGraph<NodeData, EdgeData> {
        self.base.inner()
    }
}

impl Clone for ForkedGraph {
    /// Forking a fork keeps the same base and copies only the delta.
    fn clone(&self) -> Self {
        Self {
            base: Arc::clone(&self.base),
            nodes: self.nodes.clone(),
            appended: self.appended.clone(),
            column_stores: self.column_stores.clone(),
            undo: None,
            slot_mirror: self.slot_mirror.clone(),
        }
    }
}

impl std::fmt::Debug for ForkedGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ForkedGraph {{ base: {} nodes / {} edges, overlay: {} node weights, \
             {} appended }}",
            self.base.inner().node_count(),
            self.base.inner().edge_count(),
            self.nodes.len(),
            self.appended.len()
        )
    }
}

// Node weights are overlay-then-base; adjacency and edge weights are all the
// base's, because no write this backend accepts touches either (module doc).
impl GraphRead for ForkedGraph {
    type NodeIndicesIter<'a> = GraphNodeIndices<'a>;
    type EdgeIndicesIter<'a> = <MemoryGraph as GraphRead>::EdgeIndicesIter<'a>;
    type EdgesIter<'a> = <MemoryGraph as GraphRead>::EdgesIter<'a>;
    type EdgeReferencesIter<'a> = <MemoryGraph as GraphRead>::EdgeReferencesIter<'a>;
    type EdgesConnectingIter<'a> = <MemoryGraph as GraphRead>::EdgesConnectingIter<'a>;
    type NeighborsIter<'a> = <MemoryGraph as GraphRead>::NeighborsIter<'a>;

    #[inline]
    fn node_count(&self) -> usize {
        self.base.inner().node_count() + self.appended.len()
    }

    #[inline]
    fn edge_count(&self) -> usize {
        self.base.inner().edge_count()
    }

    /// Highest occupied slot + 1, as petgraph's own `node_bound()` would read
    /// after the same appends.
    #[inline]
    fn node_bound(&self) -> usize {
        self.base.inner().node_bound().max(self.appended.bound())
    }

    /// The base's, unmodified — for the same reason `edge_count` is: no edit
    /// this overlay expresses creates or frees an edge slot (module doc).
    #[inline]
    fn edge_bound(&self) -> usize {
        GraphRead::edge_bound(&*self.base)
    }

    #[inline]
    fn is_memory(&self) -> bool {
        true
    }

    #[inline]
    fn node_weight(&self, idx: NodeIndex) -> Option<&NodeData> {
        match self.nodes.get(idx.index() as u32) {
            Some(data) => Some(data),
            None => self.base.inner().node_weight(idx),
        }
    }

    #[inline]
    fn node_type_of(&self, idx: NodeIndex) -> Option<InternedKey> {
        self.node_weight(idx).map(|n| n.node_type)
    }

    #[inline]
    fn get_node_property(&self, idx: NodeIndex, key: InternedKey) -> Option<Value> {
        self.node_view(idx)?.get_value(key)
    }

    #[inline]
    fn get_node_id(&self, idx: NodeIndex) -> Option<Value> {
        Some(self.node_view(idx)?.id().into_owned())
    }

    #[inline]
    fn get_node_title(&self, idx: NodeIndex) -> Option<Value> {
        Some(self.node_view(idx)?.title().into_owned())
    }

    #[inline]
    fn str_prop_eq(&self, idx: NodeIndex, key: InternedKey, target: &str) -> Option<bool> {
        self.node_view(idx)?.str_prop_eq(key, target)
    }

    // Edge delegation from here down, structure and weights alike: no write
    // this backend accepts reaches base adjacency *or* a base `EdgeData`
    // (module doc), so the base's answer is the whole answer and the iterating
    // reads below agree with `edge_weight`'s point lookup by construction.

    #[inline]
    fn edges_directed_filtered(
        &self,
        idx: NodeIndex,
        dir: petgraph::Direction,
        conn_type_filter: Option<InternedKey>,
    ) -> Self::EdgesIter<'_> {
        GraphRead::edges_directed_filtered(&*self.base, idx, dir, conn_type_filter)
    }

    fn edge_endpoint_keys<'a>(
        &'a self,
    ) -> Box<dyn Iterator<Item = (NodeIndex, NodeIndex, InternedKey)> + 'a> {
        GraphRead::edge_endpoint_keys(&*self.base)
    }

    fn count_edges_grouped_by_peer(
        &self,
        conn_type: InternedKey,
        dir: petgraph::Direction,
        deadline: Option<std::time::Instant>,
    ) -> Result<HashMap<u32, i64>, String> {
        GraphRead::count_edges_grouped_by_peer(&*self.base, conn_type, dir, deadline)
    }

    fn count_edges_filtered(
        &self,
        node: NodeIndex,
        dir: petgraph::Direction,
        conn_type: Option<InternedKey>,
        other_node_type: Option<InternedKey>,
        deadline: Option<std::time::Instant>,
    ) -> Result<usize, String> {
        GraphRead::count_edges_filtered(
            &*self.base,
            node,
            dir,
            conn_type,
            other_node_type,
            deadline,
        )
    }

    #[inline]
    fn column_store(&self, type_key: InternedKey) -> Option<&Arc<ColumnStore>> {
        self.column_stores.get(&type_key)
    }

    fn column_stores_iter(
        &self,
    ) -> Box<dyn Iterator<Item = (InternedKey, &Arc<ColumnStore>)> + '_> {
        Box::new(self.column_stores.iter().map(|(k, v)| (*k, v)))
    }

    /// Every live index in ascending order — the unforked graph's scan order,
    /// which `type_indices` bucket order and the rollback fidelity tests both
    /// pin. Fresh slots lie above every base slot, so without reused ones a
    /// chain suffices; reused slots sit in base gaps and are merged in.
    #[inline]
    fn node_indices(&self) -> Self::NodeIndicesIter<'_> {
        let base = self.base.inner().node_indices();
        let fresh = self.appended.fresh.start as usize..self.appended.fresh.end as usize;
        if self.appended.reused.is_empty() {
            GraphNodeIndices::Forked {
                base: Box::new(base),
                appended: fresh,
            }
        } else {
            GraphNodeIndices::ForkedReused(Box::new(ForkedReusedIndices::new(
                base,
                self.appended.reused.iter(),
                fresh,
            )))
        }
    }

    #[inline]
    fn edge_indices(&self) -> Self::EdgeIndicesIter<'_> {
        GraphRead::edge_indices(&*self.base)
    }

    #[inline]
    fn edge_references(&self) -> Self::EdgeReferencesIter<'_> {
        GraphRead::edge_references(&*self.base)
    }

    fn edge_weights<'a>(&'a self) -> Box<dyn Iterator<Item = &'a EdgeData> + 'a> {
        GraphRead::edge_weights(&*self.base)
    }

    #[inline]
    fn edges_directed(&self, idx: NodeIndex, dir: petgraph::Direction) -> Self::EdgesIter<'_> {
        GraphRead::edges_directed(&*self.base, idx, dir)
    }

    #[inline]
    fn edges(&self, idx: NodeIndex) -> Self::EdgesIter<'_> {
        GraphRead::edges(&*self.base, idx)
    }

    #[inline]
    fn edges_connecting(&self, a: NodeIndex, b: NodeIndex) -> Self::EdgesConnectingIter<'_> {
        GraphRead::edges_connecting(&*self.base, a, b)
    }

    #[inline]
    fn edge_weight(&self, idx: EdgeIndex) -> Option<&EdgeData> {
        self.base.inner().edge_weight(idx)
    }

    #[inline]
    fn find_edge(&self, a: NodeIndex, b: NodeIndex) -> Option<EdgeIndex> {
        GraphRead::find_edge(&*self.base, a, b)
    }

    #[inline]
    fn edge_endpoints(&self, idx: EdgeIndex) -> Option<(NodeIndex, NodeIndex)> {
        GraphRead::edge_endpoints(&*self.base, idx)
    }

    #[inline]
    fn neighbors_directed(
        &self,
        idx: NodeIndex,
        dir: petgraph::Direction,
    ) -> Self::NeighborsIter<'_> {
        GraphRead::neighbors_directed(&*self.base, idx, dir)
    }

    #[inline]
    fn neighbors_undirected(&self, idx: NodeIndex) -> Self::NeighborsIter<'_> {
        GraphRead::neighbors_undirected(&*self.base, idx)
    }
}

// Every mutation lands in the overlay. The three that cannot be expressed here
// are intercepted one level up, in `GraphBackend` — the only place that can
// replace a `Forked` with a `Memory`.
impl GraphWrite for ForkedGraph {
    #[inline]
    fn node_weight_mut(&mut self, idx: NodeIndex) -> Option<&mut NodeData> {
        if self.undo.is_some() {
            self.capture_node_weight(idx);
        }
        self.cow_node(idx)
    }

    #[inline]
    fn node_weight_mut_silent(&mut self, idx: NodeIndex) -> Option<&mut NodeData> {
        self.cow_node(idx)
    }

    /// Unreachable for the same reason as the three below, though not for the
    /// same cause: `GraphBackend` materialises before dispatching an edge-weight
    /// write, because an overlay copy of one would be invisible to every
    /// iterating read (see the module doc).
    fn edge_weight_mut(&mut self, _idx: EdgeIndex) -> Option<&mut EdgeData> {
        unreachable!("forked backend must be materialised before edge_weight_mut")
    }

    #[inline]
    fn install_column_store(&mut self, type_key: InternedKey, store: Arc<ColumnStore>) {
        self.column_stores.insert(type_key, store);
    }

    #[inline]
    fn column_store_mut(&mut self, type_key: InternedKey) -> Option<&mut Arc<ColumnStore>> {
        self.column_stores.get_mut(&type_key)
    }

    #[inline]
    fn take_column_store(&mut self, type_key: InternedKey) -> Option<Arc<ColumnStore>> {
        self.column_stores.remove(&type_key)
    }

    #[inline]
    fn clear_column_stores(&mut self) {
        self.column_stores.clear();
    }

    fn set_node_property(&mut self, idx: NodeIndex, key: InternedKey, value: Value) {
        self.capture_property_pre_image(idx, ColumnarWrite::Cell(key));
        // Columnar: the value lives in the store, the node only holds a row id,
        // so nothing about the node diverges and no `NodeData` is copied.
        //
        // `Arc::make_mut` here sees the base's handle as well as the overlay's,
        // so the *first* write per type copies the store — which is exactly
        // right: the reader's base must keep the store it was forked with. It
        // is once per fork, not once per statement: the copy the overlay now
        // owns is uniquely held, so every later write mutates it in place. This
        // is the one place the master legitimately forks, and it is why the
        // write-site assertion in `columnar_write.rs` exempts a forked backend.
        if let Some((type_key, row_id)) = self.columnar_row_of(idx) {
            if let Some(store) = self.column_stores.get_mut(&type_key) {
                Arc::make_mut(store).set(row_id, key, &value, None);
            }
            return;
        }
        if let Some(nd) = self.cow_node(idx) {
            nd.properties.insert(key, value);
        }
    }

    fn set_node_property_if_absent(&mut self, idx: NodeIndex, key: InternedKey, value: Value) {
        if GraphRead::node_has_property(self, idx, key) {
            return;
        }
        GraphWrite::set_node_property(self, idx, key, value);
    }

    /// Titles follow properties: on a columnar node they live in the store's
    /// reserved column, so the overlay writes them there — copying the store
    /// once per type on the first write, exactly as `set_node_property` does,
    /// so the base a reader is holding keeps its own titles.
    fn set_node_title(&mut self, idx: NodeIndex, value: Value) {
        if let Some((type_key, row_id)) = self.columnar_row_of(idx) {
            if self.undo.is_some() {
                let prior = self
                    .column_stores
                    .get(&type_key)
                    .and_then(|store| store.get_title(row_id));
                if let Some(journal) = self.undo.as_deref_mut() {
                    journal.note_columnar_title(type_key, row_id, prior);
                }
            }
            if let Some(store) = self.column_stores.get_mut(&type_key) {
                if Arc::make_mut(store).set_title(row_id, &value) {
                    return;
                }
            }
        }
        if let Some(nd) = self.cow_node(idx) {
            nd.title = value;
        }
    }

    fn remove_node_property(&mut self, idx: NodeIndex, key: InternedKey) -> Option<Value> {
        let previous = GraphRead::get_node_property(self, idx, key);
        self.capture_property_pre_image(idx, ColumnarWrite::Cell(key));
        if let Some((type_key, row_id)) = self.columnar_row_of(idx) {
            if previous.is_some() {
                if let Some(store) = self.column_stores.get_mut(&type_key) {
                    Arc::make_mut(store).set(row_id, key, &Value::Null, None);
                }
            }
            return previous;
        }
        self.cow_node(idx)?.properties.remove(key)
    }

    fn clear_node_property(&mut self, idx: NodeIndex, key: InternedKey) -> Option<Value> {
        GraphWrite::remove_node_property(self, idx, key)
    }

    /// Replace-**all**: every cell present on the row is nulled, then `pairs`
    /// are written — arm for arm what `impl_heap_column_writes!` does, so a
    /// held view cannot turn a caller's replace into an update.
    fn replace_node_properties(&mut self, idx: NodeIndex, pairs: Vec<(InternedKey, Value)>) {
        // Whole-row shape: the pre-image has to span the nulled cells as well
        // as the incoming keys, which is exactly `ReplaceRow`.
        let written: Vec<InternedKey> = pairs.iter().map(|(k, _)| *k).collect();
        self.capture_property_pre_image(idx, ColumnarWrite::ReplaceRow(&written));
        if let Some((type_key, row_id)) = self.columnar_row_of(idx) {
            let Some(store) = self.column_stores.get_mut(&type_key) else {
                return;
            };
            let store = Arc::make_mut(store);
            let existing: Vec<_> = store
                .row_properties(row_id)
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            for key in existing {
                store.set(row_id, key, &Value::Null, None);
            }
            for (key, value) in pairs {
                store.set(row_id, key, &value, None);
            }
            return;
        }
        if let Some(nd) = self.cow_node(idx) {
            nd.properties.replace_all(pairs);
        }
    }

    #[inline]
    fn add_node(&mut self, data: NodeData) -> NodeIndex {
        let node_type = data.node_type;
        let bound_before = self.node_bound();
        // The slot petgraph would hand out (module doc): `can_fork` admits only
        // a synced mirror, so there is always a prediction.
        let reused = self.slot_mirror.has_free_nodes();
        let idx = self
            .slot_mirror
            .predict_next_node(bound_before)
            .expect("can_fork admits only a base whose slot mirror is synced");
        self.nodes.insert(idx.index() as u32, data);
        self.appended.push(idx.index() as u32, reused);
        self.slot_mirror.note_node_added(bound_before, idx);
        if let Some(journal) = self.undo.as_deref_mut() {
            journal.note_node_added(idx, node_type);
        }
        idx
    }

    /// Unreachable: `GraphBackend` materialises before dispatching any of the
    /// three adjacency-mutating writes here (see the module doc). The panic is
    /// the assertion that the interception is complete, not a stub — reaching
    /// it would mean a base adjacency edit was about to happen under a live
    /// reader.
    fn remove_node(&mut self, _idx: NodeIndex) -> Option<NodeData> {
        unreachable!("forked backend must be materialised before remove_node")
    }

    fn add_edge(&mut self, _a: NodeIndex, _b: NodeIndex, _data: EdgeData) -> EdgeIndex {
        unreachable!("forked backend must be materialised before add_edge")
    }

    fn remove_edge(&mut self, _idx: EdgeIndex) -> Option<EdgeData> {
        unreachable!("forked backend must be materialised before remove_edge")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::storage::interner::StringInterner;

    fn node(i: i64, interner: &mut StringInterner) -> NodeData {
        NodeData::new(
            Value::Int64(i),
            Value::String(format!("n{i}")),
            "Item".to_string(),
            HashMap::new(),
            interner,
        )
    }

    fn graph_of(n: i64, interner: &mut StringInterner) -> MemoryGraph {
        let mut graph = MemoryGraph::new();
        for i in 0..n {
            GraphWrite::add_node(&mut graph, node(i, interner));
        }
        graph
    }

    /// A fold whose slots would not line up is refused before either side
    /// changes. Without the up-front check the replay allocated the listed
    /// slot, asserted, and left the target half-folded (issue #195).
    #[test]
    fn a_fold_that_would_misplace_a_node_changes_nothing() {
        let mut interner = StringInterner::new();
        let mut forked = ForkedGraph::new(Arc::new(graph_of(3, &mut interner)));
        let appended = GraphWrite::add_node(&mut forked, node(3, &mut interner));
        assert_eq!(appended, NodeIndex::new(3));

        // Same bound (3), but slot 1 heads the free list, so `add_node` would
        // reuse it instead of appending at 3.
        let mut target = graph_of(4, &mut interner);
        GraphWrite::remove_node(&mut target, NodeIndex::new(3));
        GraphWrite::remove_node(&mut target, NodeIndex::new(1));
        assert_eq!(target.inner().node_bound(), 3);

        let error = forked
            .apply_overlay(&mut target)
            .expect_err("the fold must refuse a target that reuses a listed slot");
        assert!(error.contains("node 3"), "{error}");
        assert_eq!(
            target.inner().node_count(),
            2,
            "the target must be untouched"
        );
        assert_eq!(forked.appended.len(), 1, "the overlay must keep its append");
        assert!(
            GraphRead::node_weight(&forked, appended).is_some(),
            "the overlay must still serve the appended node"
        );
    }
}
