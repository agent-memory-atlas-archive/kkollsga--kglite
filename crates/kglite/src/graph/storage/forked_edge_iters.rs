//! The chained iterators of a fork with edge changes: the overlay's edges, then
//! the base's, in the order the folded graph would yield them (`forked_edges`).
//!
//! Each is held behind a `Box` inside the shared `Graph*` enums, so the enums
//! keep their size and the in-memory arms keep their layout. They are built
//! only for a [`ForkedGraph`](super::forked::ForkedGraph) whose edge layer is
//! not clean; a fork that wrote only nodes hands out the base's own iterators.

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::stable_graph::{EdgeReferences, Edges};
use petgraph::visit::EdgeRef;
use petgraph::{Directed, Direction};

use crate::graph::core::iterators::GraphEdgeRef;
use crate::graph::schema::EdgeData;
use crate::graph::storage::forked_edges::{EdgeLayer, OverlayEdge};

#[inline]
fn overlay_ref(slot: u32, edge: &OverlayEdge) -> GraphEdgeRef<'_> {
    GraphEdgeRef::new(
        NodeIndex::new(edge.src as usize),
        NodeIndex::new(edge.dst as usize),
        EdgeIndex::new(slot as usize),
        &edge.weight,
    )
}

/// A base edge as this view reads it, or `None` when the overlay removed it.
#[inline]
fn base_ref<'a>(
    layer: &'a EdgeLayer,
    id: EdgeIndex,
    source: NodeIndex,
    target: NodeIndex,
    weight: &'a EdgeData,
) -> Option<GraphEdgeRef<'a>> {
    let raw = id.index() as u32;
    if layer.touched.holds(raw) {
        if layer.removed.contains(&raw) {
            return None;
        }
        if let Some(own) = layer.weights.get(&raw) {
            return Some(GraphEdgeRef::new(source, target, id, own));
        }
    }
    Some(GraphEdgeRef::new(source, target, id, weight))
}

/// One node's edges in one direction: overlay edges newest first, then the
/// base's list.
pub struct ForkedEdges<'a> {
    layer: &'a EdgeLayer,
    added: std::iter::Rev<std::slice::Iter<'a, u32>>,
    base: Edges<'a, EdgeData, Directed, u32>,
}

impl<'a> ForkedEdges<'a> {
    pub(crate) fn new(
        layer: &'a EdgeLayer,
        node: NodeIndex,
        dir: Direction,
        base: Edges<'a, EdgeData, Directed, u32>,
    ) -> Self {
        Self {
            layer,
            added: layer.list(node, dir).iter().rev(),
            base,
        }
    }
}

impl<'a> Iterator for ForkedEdges<'a> {
    type Item = GraphEdgeRef<'a>;

    fn next(&mut self) -> Option<GraphEdgeRef<'a>> {
        if let Some(&slot) = self.added.next() {
            let edge = self
                .layer
                .added
                .get(&slot)
                .expect("a listed overlay edge is always in the added map");
            return Some(overlay_ref(slot, edge));
        }
        for er in self.base.by_ref() {
            if let Some(found) =
                base_ref(self.layer, er.id(), er.source(), er.target(), er.weight())
            {
                return Some(found);
            }
        }
        None
    }
}

/// A node's neighbours, in petgraph's order: every outgoing target, then the
/// incoming sources (an undirected walk skips a self-loop's second visit).
pub struct ForkedNeighbors<'a> {
    out: Option<ForkedEdges<'a>>,
    inn: Option<ForkedEdges<'a>>,
    skip_self: Option<NodeIndex>,
}

impl<'a> ForkedNeighbors<'a> {
    pub(crate) fn new(
        out: Option<ForkedEdges<'a>>,
        inn: Option<ForkedEdges<'a>>,
        skip_self: Option<NodeIndex>,
    ) -> Self {
        Self {
            out,
            inn,
            skip_self,
        }
    }
}

impl Iterator for ForkedNeighbors<'_> {
    type Item = NodeIndex;

    fn next(&mut self) -> Option<NodeIndex> {
        if let Some(out) = self.out.as_mut() {
            if let Some(edge) = out.next() {
                return Some(edge.target());
            }
        }
        let inn = self.inn.as_mut()?;
        for edge in inn.by_ref() {
            if self.skip_self != Some(edge.source()) {
                return Some(edge.source());
            }
        }
        None
    }
}

/// The edges from one node to another.
pub struct ForkedEdgesConnecting<'a> {
    edges: ForkedEdges<'a>,
    target: NodeIndex,
}

impl<'a> ForkedEdgesConnecting<'a> {
    pub(crate) fn new(edges: ForkedEdges<'a>, target: NodeIndex) -> Self {
        Self { edges, target }
    }
}

impl<'a> Iterator for ForkedEdgesConnecting<'a> {
    type Item = GraphEdgeRef<'a>;

    fn next(&mut self) -> Option<GraphEdgeRef<'a>> {
        let target = self.target;
        self.edges.by_ref().find(|edge| edge.target() == target)
    }
}

/// Every live edge in ascending slot order: the base's, minus the removed,
/// merged with the overlay's sorted by slot. The two never share a slot,
/// because a base slot the overlay reused is in the base's `removed` set.
pub struct ForkedEdgeRefs<'a> {
    layer: &'a EdgeLayer,
    base: EdgeReferences<'a, EdgeData, u32>,
    pending: Option<GraphEdgeRef<'a>>,
    added: Vec<u32>,
    next_added: usize,
}

impl<'a> ForkedEdgeRefs<'a> {
    pub(crate) fn new(layer: &'a EdgeLayer, base: EdgeReferences<'a, EdgeData, u32>) -> Self {
        Self {
            layer,
            base,
            pending: None,
            added: layer.added_sorted(),
            next_added: 0,
        }
    }

    fn next_base(&mut self) -> Option<GraphEdgeRef<'a>> {
        if let Some(read_ahead) = self.pending.take() {
            return Some(read_ahead);
        }
        for er in self.base.by_ref() {
            if let Some(found) =
                base_ref(self.layer, er.id(), er.source(), er.target(), er.weight())
            {
                return Some(found);
            }
        }
        None
    }
}

impl<'a> Iterator for ForkedEdgeRefs<'a> {
    type Item = GraphEdgeRef<'a>;

    fn next(&mut self) -> Option<GraphEdgeRef<'a>> {
        let base = self.next_base();
        let added = self.added.get(self.next_added).copied();
        match (base, added) {
            (Some(base), Some(slot)) if (slot as usize) < base.id().index() => {
                self.pending = Some(base);
                self.next_added += 1;
                Some(overlay_ref(slot, &self.layer.added[&slot]))
            }
            (Some(base), _) => Some(base),
            (None, Some(slot)) => {
                self.next_added += 1;
                Some(overlay_ref(slot, &self.layer.added[&slot]))
            }
            (None, None) => None,
        }
    }
}
