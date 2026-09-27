//! The shortest-path searches under a resolved valid-time filter (a Cypher
//! `shortestPath` / `allShortestPaths` under `FOR VALID_TIME AS OF`).
//!
//! The plain searches expand over `neighbors_*`, which yield nodes, not
//! relationships, so a relationship's own interval cannot be tested there.
//! These expanders walk the relationships instead and follow one only when
//! the filter admits it (keyed on its own source) and the node it reaches.
//! The caller picks them once per search; the endpoints come from the guarded
//! pattern matcher.

use petgraph::graph::NodeIndex;
use petgraph::Direction;
use std::collections::HashSet;

use super::bidirectional::bidirectional_bfs;
use super::graph_algorithms::{node_passes_via_filter, EdgeDir};
use super::Interrupt;
use crate::graph::core::graph_filter::ElementFilter;
use crate::graph::schema::{DirGraph, InternedKey};
use crate::graph::storage::GraphRead;

fn directions(direction: EdgeDir) -> &'static [Direction] {
    match direction {
        EdgeDir::Any => &[Direction::Outgoing, Direction::Incoming],
        EdgeDir::Outgoing => &[Direction::Outgoing],
        EdgeDir::Incoming => &[Direction::Incoming],
    }
}

/// Every node one admitted relationship of an accepted type leads to from
/// `node`, itself admitted, into `sink`. A node reached over two
/// relationships repeats; the BFS callers' seen-checks drop it.
pub(super) fn expand_admitted_into(
    graph: &DirGraph,
    node: NodeIndex,
    direction: EdgeDir,
    connection_types: Option<&[InternedKey]>,
    filter: &ElementFilter,
    sink: &mut dyn FnMut(u32),
) {
    let hint = match connection_types {
        Some([one]) => Some(*one),
        _ => None,
    };
    for &dir in directions(direction) {
        for edge in graph.graph.edges_directed_filtered(node, dir, hint) {
            let conn = edge.connection_type();
            if connection_types.is_some_and(|types| !types.contains(&conn)) {
                continue;
            }
            let far = match dir {
                Direction::Outgoing => edge.target(),
                Direction::Incoming => edge.source(),
            };
            if filter.admits_hop(graph, edge.id(), conn, edge.source(), far) {
                sink(far.index() as u32);
            }
        }
    }
}

/// [`expand_admitted_into`] collected and deduplicated, for the
/// all-shortest-paths predecessor DAG (a repeat there is a duplicate path).
pub(super) fn admitted_neighbors(
    graph: &DirGraph,
    node: NodeIndex,
    directed: bool,
    connection_types: Option<&[InternedKey]>,
    filter: &ElementFilter,
) -> Vec<NodeIndex> {
    let direction = if directed {
        EdgeDir::Outgoing
    } else {
        EdgeDir::Any
    };
    let mut out: Vec<NodeIndex> = Vec::new();
    expand_admitted_into(graph, node, direction, connection_types, filter, &mut |n| {
        out.push(NodeIndex::new(n as usize))
    });
    if out.len() > 1 {
        out.sort_unstable();
        out.dedup();
    }
    out
}

/// The meet-in-the-middle search of `bidirectional_path` over admitted
/// relationships and nodes.
pub(super) fn bidirectional_path_guarded(
    graph: &DirGraph,
    (source, target): (NodeIndex, NodeIndex),
    connection_types: Option<&[InternedKey]>,
    via_types: &Option<HashSet<&str>>,
    direction: EdgeDir,
    deadline: Interrupt,
    filter: &ElementFilter,
) -> Option<Vec<NodeIndex>> {
    let source_id = u32::try_from(source.index()).ok()?;
    let target_id = u32::try_from(target.index()).ok()?;
    let backward = direction.reversed();
    let path = bidirectional_bfs(
        source_id,
        target_id,
        |u, sink| {
            let node = NodeIndex::new(u as usize);
            expand_admitted_into(graph, node, direction, connection_types, filter, sink)
        },
        |u, sink| {
            let node = NodeIndex::new(u as usize);
            expand_admitted_into(graph, node, backward, connection_types, filter, sink)
        },
        |w| node_passes_via_filter(graph, NodeIndex::new(w as usize), via_types),
        deadline,
    )?;
    Some(
        path.into_iter()
            .map(|idx| NodeIndex::new(idx as usize))
            .collect(),
    )
}
