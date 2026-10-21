//! The CSR-backed arms of the `Graph*` iterator enums: a disk graph's iterator and a frozen
//! topology's, folded behind one variant each so the heap backends' arms keep the shape they had.

use petgraph::graph::{EdgeIndex, NodeIndex};

use super::iterators::{
    DiskEdgeIndices, DiskEdgeReferences, DiskEdges, DiskEdgesConnecting, DiskNeighbors,
    DiskNodeIndices, GraphEdgeRef,
};
use crate::graph::storage::frozen::{
    FrozenEdgeIndices, FrozenEdgeRefs, FrozenEdges, FrozenEdgesConnecting, FrozenNeighbors,
    FrozenNodeIndices,
};

macro_rules! csr_iterator {
    ($name:ident, $disk:ty, $frozen:ty, $item:ty, $lt:lifetime, $inline:meta) => {
        pub enum $name<$lt> {
            Disk($disk),
            Frozen($frozen),
        }

        impl<$lt> Iterator for $name<$lt> {
            type Item = $item;

            // The `Graph*` enums whose in-memory `next` is split off (`GraphEdges`,
            // `GraphNeighbors`) reach this from their out-of-line `next_other`, so it
            // inlines there; the others inline their arms into the heap backends' hottest
            // loops and keep this body out of line.
            #[$inline]
            fn next(&mut self) -> Option<$item> {
                match self {
                    Self::Disk(iter) => iter.next(),
                    Self::Frozen(iter) => iter.next(),
                }
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                match self {
                    Self::Disk(iter) => iter.size_hint(),
                    Self::Frozen(iter) => iter.size_hint(),
                }
            }
        }
    };
}

csr_iterator!(CsrNodeIndices, DiskNodeIndices<'a>, FrozenNodeIndices<'a>, NodeIndex, 'a, inline(never));
csr_iterator!(CsrEdges, DiskEdges<'a>, FrozenEdges<'a>, GraphEdgeRef<'a>, 'a, inline);
csr_iterator!(CsrEdgeReferences, DiskEdgeReferences<'a>, FrozenEdgeRefs<'a>, GraphEdgeRef<'a>, 'a, inline(never));
csr_iterator!(CsrEdgeIndices, DiskEdgeIndices<'a>, FrozenEdgeIndices<'a>, EdgeIndex, 'a, inline(never));
csr_iterator!(CsrEdgesConnecting, DiskEdgesConnecting<'a>, FrozenEdgesConnecting<'a>, GraphEdgeRef<'a>, 'a, inline(never));

/// `DiskNeighbors` owns its peers, so it carries no lifetime.
pub enum CsrNeighbors<'a> {
    Disk(DiskNeighbors),
    Frozen(FrozenNeighbors<'a>),
}

impl Iterator for CsrNeighbors<'_> {
    type Item = NodeIndex;

    #[inline]
    fn next(&mut self) -> Option<NodeIndex> {
        match self {
            Self::Disk(iter) => iter.next(),
            Self::Frozen(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Disk(iter) => iter.size_hint(),
            Self::Frozen(iter) => iter.size_hint(),
        }
    }
}
