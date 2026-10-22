//! Turning a frozen topology into the heap graph a write needs.

use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::stable_graph::StableDiGraph;

use super::FrozenGraph;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::interner::InternedKey;
use crate::graph::storage::property_storage::{ColumnarRow, PropertyStorage};
use crate::graph::storage::MappedGraph;

pub(super) fn placeholder_node() -> NodeData {
    NodeData {
        node_type: InternedKey::from_u64(0),
        ident: None,
        properties: PropertyStorage::Columnar(ColumnarRow::new(0)),
    }
}

impl FrozenGraph {
    /// The same graph as a heap petgraph, with every node and edge on the slot
    /// it holds here.
    ///
    /// Vacant slots are filled with placeholders and then removed in ascending
    /// order, which leaves each free list with its highest slot first: the
    /// state petgraph's deserializer builds from a vacancy pattern, and so the
    /// state a heap load of the same file starts from.
    pub(crate) fn to_stable_graph(&self) -> StableDiGraph<NodeData, EdgeData> {
        let node_bound = self.node_bound_len();
        let edge_bound = self.edge_bound_len();
        let mut graph = StableDiGraph::with_capacity(node_bound, edge_bound);
        let mut vacant_nodes = Vec::new();
        for slot in 0..node_bound {
            if self.node_is_alive(slot) {
                graph.add_node(self.node_slice()[slot].clone());
            } else {
                graph.add_node(placeholder_node());
                vacant_nodes.push(slot);
            }
        }
        let mut vacant_edges = Vec::new();
        for slot in 0..edge_bound {
            let live = self.endpoint_slots()[slot];
            let ends = self
                .edge_is_live(slot)
                .then_some((live.src as usize, live.dst as usize));
            let added = match ends {
                Some((src, dst)) => graph.add_edge(
                    NodeIndex::new(src),
                    NodeIndex::new(dst),
                    self.edge_data(slot as u32, live.ty).clone(),
                ),
                None => {
                    vacant_edges.push(slot);
                    graph.add_edge(
                        NodeIndex::new(0),
                        NodeIndex::new(0),
                        EdgeData::new_interned(InternedKey::from_u64(0), Vec::new()),
                    )
                }
            };
            debug_assert_eq!(added, EdgeIndex::new(slot));
        }
        for slot in vacant_edges {
            graph.remove_edge(EdgeIndex::new(slot));
        }
        for slot in vacant_nodes {
            graph.remove_node(NodeIndex::new(slot));
        }
        graph
    }

    /// The heap backend a write continues on: the petgraph above, the column
    /// stores this graph owns, and the peer counts the file recorded (valid
    /// until the first edge write, which `MappedGraph` already handles).
    ///
    /// Reads every mapped section's CRC32 first: after the copy nothing would
    /// check the bytes again, and a corrupt section would become heap state.
    pub(crate) fn to_mapped(&self) -> MappedGraph {
        if let Err(error) = self.verify_sections() {
            panic!("cannot promote a corrupt memory-mapped topology to the heap: {error}");
        }
        let mut mapped = MappedGraph::from_deserialized(self.to_stable_graph());
        mapped.column_stores = self.column_stores.clone();
        mapped.peer_base = self.peer_base().cloned();
        mapped
    }
}
