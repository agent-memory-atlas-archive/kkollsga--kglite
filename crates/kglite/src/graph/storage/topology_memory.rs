//! Where a backend's topology lives, for `graph_info()`.

use super::backend::GraphBackend;
use super::GraphRead;

/// Size of one petgraph node entry: the 32-byte `NodeData` and two adjacency heads.
const HEAP_NODE_BYTES: usize = 40;
/// Size of one petgraph edge entry: 16-byte `EdgeData` plus endpoints and links.
const HEAP_EDGE_BYTES: usize = 32;

/// Topology memory of one backend; see the `topology_*` fields of `GraphInfo`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TopologyMemory {
    pub(crate) mapped_bytes: usize,
    pub(crate) heap_bytes: usize,
    pub(crate) overlay_entries: usize,
}

impl GraphBackend {
    /// Mapped and heap bytes of the topology, and the entries of the write
    /// overlay over a frozen base. The disk backend reports zeros: its CSR is
    /// reported by `edge_storage_info`.
    pub(crate) fn topology_memory(&self) -> TopologyMemory {
        if GraphRead::is_disk(self) {
            return TopologyMemory::default();
        }
        match self.frozen_base() {
            Some(base) => TopologyMemory {
                mapped_bytes: base.mapped_bytes(),
                heap_bytes: base.heap_bytes(),
                overlay_entries: self.overlay_entries().unwrap_or(0),
            },
            None => TopologyMemory {
                mapped_bytes: 0,
                heap_bytes: self.node_bound() * HEAP_NODE_BYTES
                    + self.edge_bound() * HEAP_EDGE_BYTES,
                overlay_entries: 0,
            },
        }
    }
}
