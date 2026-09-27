//! The duplicate-id map: per node type and graph version, the nodes the id
//! index shadows. The id index keeps one node per `(type, id)` — the last one
//! in the type's node order — so when version nodes share an id, a seek under
//! a valid-time filter may be handed a version that is not visible while
//! another is. This map names the others, grouped under the node the index
//! returns, so the seek admit-tests only nodes carrying the sought id.
//!
//! Built lazily by one walk over the type (an id read and an id-index probe
//! per node) and cached beside the endpoint indexes, under the same version
//! stamp and byte cap. It holds only shadowed nodes, so it is empty on a
//! graph whose ids are unique; its heap is at most [`GROUP_BYTES`] per id
//! that occurs more than once plus [`MEMBER_BYTES`] per shadowed node, in
//! every storage mode, and a map that would pass the cap is not built
//! (seeks then walk the type, comparing ids before reading any bound).

use petgraph::graph::NodeIndex;
use rustc_hash::FxHashMap;

use crate::graph::dir_graph::DirGraph;
use crate::graph::storage::GraphRead;

/// The most one group costs beside its members: the hash-map slot (key,
/// boxed-slice header, control byte) rounded up for the table's load factor.
pub(crate) const GROUP_BYTES: usize = 48;
/// One shadowed node, twice over for a growing member list.
pub(crate) const MEMBER_BYTES: usize = 2 * size_of::<NodeIndex>();

/// Per node the id index returns for an id that occurs more than once, the
/// other nodes with that id in the type's node order.
#[derive(Debug, Default)]
pub(crate) struct DuplicateIds {
    groups: FxHashMap<NodeIndex, Box<[NodeIndex]>>,
    members: usize,
}

impl DuplicateIds {
    /// Walk `node_type`; `None` once the map would hold more than `budget`
    /// bytes.
    pub(crate) fn build(graph: &DirGraph, node_type: &str, budget: usize) -> Option<Self> {
        let mut groups: FxHashMap<NodeIndex, Vec<NodeIndex>> = FxHashMap::default();
        let mut members = 0usize;
        if let Some(nodes) = graph.type_indices.get(node_type) {
            for idx in nodes.iter() {
                let Some(hit) = shadowed_by(graph, node_type, idx) else {
                    continue;
                };
                groups.entry(hit).or_default().push(idx);
                members += 1;
                if Self::bytes_for(groups.len(), members) > budget {
                    return None;
                }
            }
        }
        let groups = groups
            .into_iter()
            .map(|(hit, others)| (hit, others.into_boxed_slice()))
            .collect();
        Some(DuplicateIds { groups, members })
    }

    /// The nodes sharing `hit`'s id, latest in the type's order first.
    pub(crate) fn others(&self, hit: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        self.groups
            .get(&hit)
            .into_iter()
            .flat_map(|others| others.iter().rev().copied())
    }

    pub(crate) fn bytes(&self) -> usize {
        Self::bytes_for(self.groups.len(), self.members)
    }

    fn bytes_for(groups: usize, members: usize) -> usize {
        groups
            .saturating_mul(GROUP_BYTES)
            .saturating_add(members.saturating_mul(MEMBER_BYTES))
    }
}

/// The node the id index returns for `idx`'s id, when that is another node.
pub(crate) fn shadowed_by(graph: &DirGraph, node_type: &str, idx: NodeIndex) -> Option<NodeIndex> {
    let id = graph.graph.get_node_id(idx)?;
    graph
        .lookup_by_id_readonly(node_type, &id)
        .filter(|&hit| hit != idx)
}
