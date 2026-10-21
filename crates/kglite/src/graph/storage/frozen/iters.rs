//! Iterators over a [`FrozenGraph`]. Each is a few words (a graph reference and
//! positions into the CSR arrays) so that adding its arm to the `Graph*`
//! iterator enums leaves their size to the largest existing variant.

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::Direction;

use super::{FrozenGraph, VACANT};
use crate::graph::core::iterators::GraphEdgeRef;
use crate::graph::schema::EdgeData;
use crate::graph::storage::disk::csr::CsrEdge;

/// Live node slots in ascending order.
pub struct FrozenNodeIndices<'a> {
    graph: &'a FrozenGraph,
    pos: usize,
    len: usize,
}

impl<'a> FrozenNodeIndices<'a> {
    pub(crate) fn new(graph: &'a FrozenGraph) -> Self {
        FrozenNodeIndices {
            graph,
            pos: 0,
            len: graph.node_bound_len(),
        }
    }
}

impl Iterator for FrozenNodeIndices<'_> {
    type Item = NodeIndex;

    #[inline]
    fn next(&mut self) -> Option<NodeIndex> {
        while self.pos < self.len {
            let slot = self.pos;
            self.pos += 1;
            if self.graph.node_is_alive(slot) {
                return Some(NodeIndex::new(slot));
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len - self.pos;
        if self.graph.has_vacant_nodes() {
            (0, Some(remaining))
        } else {
            (remaining, Some(remaining))
        }
    }
}

/// One node's edges in one direction, optionally narrowed to a type run: the
/// run's slice of the adjacency and of the type ordinals, walked in step.
pub struct FrozenEdges<'a> {
    graph: &'a FrozenGraph,
    edges: std::slice::Iter<'a, CsrEdge>,
    types: std::slice::Iter<'a, u16>,
    node: u32,
    outgoing: bool,
}

impl<'a> FrozenEdges<'a> {
    pub(crate) fn new(graph: &'a FrozenGraph, node: NodeIndex, dir: Direction) -> Self {
        let (lo, hi) = graph.run(node.index(), dir);
        Self::over(graph, node, dir, lo, hi)
    }

    pub(crate) fn over(
        graph: &'a FrozenGraph,
        node: NodeIndex,
        dir: Direction,
        lo: usize,
        hi: usize,
    ) -> Self {
        let (edges, types) = graph.csr(dir);
        Self::over_slices(graph, node, dir, edges, types, lo, hi)
    }

    /// `over` with the direction's arrays already resolved.
    #[inline(always)]
    pub(crate) fn over_slices(
        graph: &'a FrozenGraph,
        node: NodeIndex,
        dir: Direction,
        edges: &'a [CsrEdge],
        types: &'a [u16],
        lo: usize,
        hi: usize,
    ) -> Self {
        FrozenEdges {
            graph,
            edges: edges.get(lo..hi).unwrap_or(&[]).iter(),
            types: types.get(lo..hi).unwrap_or(&[]).iter(),
            node: node.index() as u32,
            outgoing: dir == Direction::Outgoing,
        }
    }

    pub(crate) fn empty(graph: &'a FrozenGraph, node: NodeIndex, dir: Direction) -> Self {
        Self::over(graph, node, dir, 0, 0)
    }
}

impl<'a> Iterator for FrozenEdges<'a> {
    type Item = GraphEdgeRef<'a>;

    #[inline]
    fn next(&mut self) -> Option<GraphEdgeRef<'a>> {
        let entry = *self.edges.next()?;
        let ty = self.types.next().copied().unwrap_or(0);
        let this = NodeIndex::new(self.node as usize);
        let peer = NodeIndex::new(entry.peer as usize);
        let (source, target) = if self.outgoing {
            (this, peer)
        } else {
            (peer, this)
        };
        Some(GraphEdgeRef::new(
            source,
            target,
            EdgeIndex::new(entry.edge_idx as usize),
            self.graph.edge_data(entry.edge_idx, ty),
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.edges.size_hint()
    }
}

/// Edges from `a` to one target.
pub struct FrozenEdgesConnecting<'a> {
    inner: FrozenEdges<'a>,
    target: u32,
}

impl<'a> FrozenEdgesConnecting<'a> {
    pub(crate) fn new(graph: &'a FrozenGraph, a: NodeIndex, b: NodeIndex) -> Self {
        FrozenEdgesConnecting {
            inner: FrozenEdges::new(graph, a, Direction::Outgoing),
            target: b.index() as u32,
        }
    }
}

impl<'a> Iterator for FrozenEdgesConnecting<'a> {
    type Item = GraphEdgeRef<'a>;

    #[inline]
    fn next(&mut self) -> Option<GraphEdgeRef<'a>> {
        let target = self.target;
        self.inner
            .by_ref()
            .find(|edge| edge.target().index() as u32 == target)
    }
}

/// Every live edge slot in ascending order.
pub struct FrozenEdgeRefs<'a> {
    graph: &'a FrozenGraph,
    pos: usize,
}

impl<'a> FrozenEdgeRefs<'a> {
    pub(crate) fn new(graph: &'a FrozenGraph) -> Self {
        FrozenEdgeRefs { graph, pos: 0 }
    }
}

impl<'a> Iterator for FrozenEdgeRefs<'a> {
    type Item = GraphEdgeRef<'a>;

    #[inline]
    fn next(&mut self) -> Option<GraphEdgeRef<'a>> {
        let slots = self.graph.endpoint_slots();
        while self.pos < slots.len() {
            let slot = self.pos;
            self.pos += 1;
            let ep = slots[slot];
            if ep.src != VACANT {
                return Some(GraphEdgeRef::new(
                    NodeIndex::new(ep.src as usize),
                    NodeIndex::new(ep.dst as usize),
                    EdgeIndex::new(slot),
                    self.graph.edge_data(slot as u32, ep.ty),
                ));
            }
        }
        None
    }
}

/// Live edge slot numbers in ascending order.
pub struct FrozenEdgeIndices<'a> {
    graph: &'a FrozenGraph,
    pos: usize,
}

impl<'a> FrozenEdgeIndices<'a> {
    pub(crate) fn new(graph: &'a FrozenGraph) -> Self {
        FrozenEdgeIndices { graph, pos: 0 }
    }
}

impl Iterator for FrozenEdgeIndices<'_> {
    type Item = EdgeIndex;

    #[inline]
    fn next(&mut self) -> Option<EdgeIndex> {
        let slots = self.graph.endpoint_slots();
        while self.pos < slots.len() {
            let slot = self.pos;
            self.pos += 1;
            if slots[slot].src != VACANT {
                return Some(EdgeIndex::new(slot));
            }
        }
        None
    }
}

/// Neighbours of one node: the outgoing run's peers, then the incoming run's.
/// For an undirected walk the incoming pass skips the node itself, so a
/// self-loop is yielded once, as petgraph's `neighbors_undirected` does.
pub struct FrozenNeighbors<'a> {
    graph: &'a FrozenGraph,
    out_pos: u32,
    out_end: u32,
    in_pos: u32,
    in_end: u32,
    skip: u32,
}

impl<'a> FrozenNeighbors<'a> {
    pub(crate) fn directed(graph: &'a FrozenGraph, node: NodeIndex, dir: Direction) -> Self {
        let (lo, hi) = graph.run(node.index(), dir);
        match dir {
            Direction::Outgoing => Self::new(graph, (lo, hi), (0, 0), VACANT),
            Direction::Incoming => Self::new(graph, (0, 0), (lo, hi), VACANT),
        }
    }

    pub(crate) fn undirected(graph: &'a FrozenGraph, node: NodeIndex) -> Self {
        Self::new(
            graph,
            graph.run(node.index(), Direction::Outgoing),
            graph.run(node.index(), Direction::Incoming),
            node.index() as u32,
        )
    }

    fn new(graph: &'a FrozenGraph, out: (usize, usize), inn: (usize, usize), skip: u32) -> Self {
        FrozenNeighbors {
            graph,
            out_pos: out.0 as u32,
            out_end: out.1 as u32,
            in_pos: inn.0 as u32,
            in_end: inn.1 as u32,
            skip,
        }
    }
}

impl Iterator for FrozenNeighbors<'_> {
    type Item = NodeIndex;

    #[inline]
    fn next(&mut self) -> Option<NodeIndex> {
        if self.out_pos < self.out_end {
            let slot = self.out_pos as usize;
            self.out_pos += 1;
            let (edges, _) = self.graph.csr(Direction::Outgoing);
            return edges.get(slot).map(|e| NodeIndex::new(e.peer as usize));
        }
        while self.in_pos < self.in_end {
            let slot = self.in_pos as usize;
            self.in_pos += 1;
            let (edges, _) = self.graph.csr(Direction::Incoming);
            let peer = edges.get(slot)?.peer;
            if peer != self.skip {
                return Some(NodeIndex::new(peer as usize));
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining =
            self.out_end.saturating_sub(self.out_pos) + self.in_end.saturating_sub(self.in_pos);
        (0, Some(remaining as usize))
    }
}

/// Iterate edge weights of the live slots.
pub(crate) fn weights(graph: &FrozenGraph) -> impl Iterator<Item = &EdgeData> + '_ {
    graph
        .endpoint_slots()
        .iter()
        .enumerate()
        .filter(|(_, ep)| ep.src != VACANT)
        .map(|(slot, ep)| graph.edge_data(slot as u32, ep.ty))
}

#[cfg(test)]
mod size_tests {
    use super::*;
    use crate::graph::core::iterators::{
        DiskEdgeReferences, DiskEdges, DiskEdgesConnecting, DiskNeighbors,
    };

    /// The frozen arms ride inside enums that already hold a disk iterator;
    /// they must not be what sets those enums' size.
    #[test]
    fn frozen_iterators_fit_inside_the_disk_variants() {
        assert!(std::mem::size_of::<FrozenEdges>() <= std::mem::size_of::<DiskEdges>());
        assert!(
            std::mem::size_of::<FrozenEdgesConnecting>()
                <= std::mem::size_of::<DiskEdgesConnecting>()
        );
        assert!(std::mem::size_of::<FrozenEdgeRefs>() <= std::mem::size_of::<DiskEdgeReferences>());
        assert!(std::mem::size_of::<FrozenNeighbors>() <= std::mem::size_of::<DiskNeighbors>());
    }
}
