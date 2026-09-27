//! The duplicate-id map: per node type and graph version, every node that
//! shares its id with another node of the type, grouped by id. The id index
//! keeps one node per (type, id) — the last in the type's node order, under
//! one spelling of the id — so when version nodes share an id, a seek under a
//! valid-time filter may be handed a version that is not visible while
//! another is. Ids are grouped as the index compares them
//! ([`canonical_id`]): a loaded `UniqueId(1)` and a Cypher-created
//! `Int64(1)` are one id. A group is named by the node the index answers for
//! the id, so the seek admit-tests only nodes carrying the sought id, latest
//! in the type's node order first.
//!
//! Built lazily — one walk over the type with an id read and an id-index
//! probe per node, then a walk without id reads to place each group's own
//! node — and cached beside the endpoint indexes, under the same version
//! stamp and byte cap. It holds only nodes whose id repeats, so it is empty
//! on a graph whose ids are unique. Its heap is one flat array,
//! [`ENTRY_BYTES`] per slot of its capacity ([`DuplicateIds::bytes`]); a
//! growth is refused before the old and the new buffer together would pass
//! the budget, so the build never holds more than the budget (allocator size
//! classes aside), in every storage mode. A map that would pass it is not
//! built: seeks then walk the type, comparing ids before reading any bound.

use petgraph::graph::NodeIndex;

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::schema::canonical_id;
use crate::graph::storage::GraphRead;

/// One member: its group, its position in the type's node order, itself.
#[derive(Clone, Copy, Debug)]
struct Entry {
    group: NodeIndex,
    position: u32,
    node: NodeIndex,
}

/// The heap one slot of the map's array costs.
pub(crate) const ENTRY_BYTES: usize = size_of::<Entry>();
/// The first capacity a build reserves.
const FIRST_CAPACITY: usize = 16;

/// The nodes of a type whose id occurs more than once, sorted by group, then
/// by node order.
#[derive(Debug, Default)]
pub(crate) struct DuplicateIds {
    entries: Vec<Entry>,
}

impl DuplicateIds {
    /// Walk `node_type`; `None` once the map would hold more than `budget`
    /// bytes.
    pub(crate) fn build(graph: &DirGraph, node_type: &str, budget: usize) -> Option<Self> {
        let mut entries = Vec::new();
        let Some(nodes) = graph.type_indices.get(node_type) else {
            return Some(Self { entries });
        };
        let positions = u32::try_from(nodes.len()).ok()?;
        for position in 0..positions {
            let Some(node) = nodes.get(position as usize) else {
                continue;
            };
            let Some(group) = group_of(graph, node_type, node).filter(|&g| g != node) else {
                continue;
            };
            push(&mut entries, budget, group, position, node)?;
        }
        if entries.is_empty() {
            return Some(Self { entries });
        }
        entries.sort_unstable_by_key(|e| (e.group, e.position));
        let shadowed = entries.len();
        for position in 0..positions {
            let Some(node) = nodes.get(position as usize) else {
                continue;
            };
            if entries[..shadowed]
                .binary_search_by_key(&node, |e| e.group)
                .is_ok()
            {
                push(&mut entries, budget, node, position, node)?;
            }
        }
        entries.sort_unstable_by_key(|e| (e.group, e.position));
        Some(Self { entries })
    }

    /// Every node of `group` (the node the id index answers for the id's
    /// canonical spelling), latest in the type's node order first; empty
    /// when the id does not repeat.
    pub(crate) fn latest_first(&self, group: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        let start = self.entries.partition_point(|e| e.group < group);
        let end = self.entries.partition_point(|e| e.group <= group);
        self.entries[start..end].iter().rev().map(|e| e.node)
    }

    pub(crate) fn bytes(&self) -> usize {
        self.entries.capacity() * ENTRY_BYTES
    }
}

/// Append one entry, doubling the capacity when full; `None` when the old and
/// the grown buffer together would pass `budget`.
fn push(
    entries: &mut Vec<Entry>,
    budget: usize,
    group: NodeIndex,
    position: u32,
    node: NodeIndex,
) -> Option<()> {
    if entries.len() == entries.capacity() {
        let grown = (entries.capacity() * 2).max(FIRST_CAPACITY);
        if (entries.capacity() + grown).saturating_mul(ENTRY_BYTES) > budget {
            return None;
        }
        entries.reserve_exact(grown - entries.len());
    }
    entries.push(Entry {
        group,
        position,
        node,
    });
    Some(())
}

/// The node the id index answers for `id`'s canonical spelling: the name of
/// its group.
pub(crate) fn group_for(graph: &DirGraph, node_type: &str, id: &Value) -> Option<NodeIndex> {
    graph.lookup_by_id_readonly(node_type, &canonical_id(id))
}

/// The group of `node`'s id; `None` for a node without one (a NULL id is
/// never sought).
fn group_of(graph: &DirGraph, node_type: &str, node: NodeIndex) -> Option<NodeIndex> {
    let id = graph.graph.get_node_id(node)?;
    if matches!(id, Value::Null) {
        return None;
    }
    group_for(graph, node_type, &id)
}

/// Whether `a` and `b` are one id to the id index.
pub(crate) fn same_id(a: &Value, b: &Value) -> bool {
    canonical_id(a) == canonical_id(b)
}

#[cfg(test)]
#[path = "duplicate_ids_tests.rs"]
mod tests;
