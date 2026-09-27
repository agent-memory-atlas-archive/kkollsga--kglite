// src/graph/subgraph.rs
//! Subgraph extraction and selection expansion operations

use crate::datatypes::values::Value;
use crate::graph::core::fluent_filter::FluentFilter;
use crate::graph::schema::{CurrentSelection, DirGraph, EdgeData, SchemaInstall};
use crate::graph::storage::{GraphRead, GraphWrite};
use petgraph::graph::{EdgeIndex, NodeIndex};
use petgraph::Direction;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Expand the current selection by N hops using BFS.
///
/// This function takes all currently selected nodes and expands the selection
/// to include all nodes within `hops` distance from any selected node.
/// The expansion considers edges in both directions (undirected). Under
/// `valid_time` a hop is followed only when the relationship and the node it
/// reaches are visible.
pub fn expand_selection(
    graph: &DirGraph,
    selection: &mut CurrentSelection,
    hops: usize,
    valid_time: Option<&FluentFilter>,
) -> Result<(), String> {
    let valid_time = valid_time.filter(|filter| !filter.is_empty());
    // Arena guard: the filtered walk reads relationships, which disk mode
    // materialises into the query arena (protocol in disk/graph.rs).
    let _arena_guard = graph.graph.begin_query();
    let level_idx = selection.get_level_count().saturating_sub(1);
    let level = selection
        .get_level(level_idx)
        .ok_or_else(|| "No active selection level".to_string())?;

    // Start with current selection
    let mut frontier: HashSet<NodeIndex> = level.iter_node_indices().collect();
    let mut visited = frontier.clone();

    // BFS expansion for N hops
    let g = &graph.graph;
    for _ in 0..hops {
        let mut next_frontier = HashSet::new();

        for &node in &frontier {
            let Some(filter) = valid_time else {
                for neighbor in g.neighbors_undirected(node) {
                    if visited.insert(neighbor) {
                        next_frontier.insert(neighbor);
                    }
                }
                continue;
            };
            let outgoing = g
                .edges_directed(node, Direction::Outgoing)
                .map(|e| (e.target(), e));
            let incoming = g
                .edges_directed(node, Direction::Incoming)
                .map(|e| (e.source(), e));
            for (far, edge) in outgoing.chain(incoming) {
                let conn = edge.weight().connection_type;
                if !visited.contains(&far)
                    && filter.admits_hop(graph, edge.id(), conn, edge.source(), far)?
                {
                    visited.insert(far);
                    next_frontier.insert(far);
                }
            }
        }

        // If no new nodes were found, stop early
        if next_frontier.is_empty() {
            break;
        }

        frontier = next_frontier;
    }

    // Update selection with expanded nodes
    let level_mut = selection
        .get_level_mut(level_idx)
        .ok_or_else(|| "Failed to get mutable selection level".to_string())?;

    level_mut.selections.clear();
    level_mut.add_selection(None, visited.into_iter().collect());

    valid_time.map_or(Ok(()), FluentFilter::finish)
}

/// Extract a subgraph containing only the selected nodes and edges between them.
///
/// This creates an independent copy of the graph containing only the nodes
/// in the current selection and all edges that connect those nodes — under
/// `valid_time`, those whose own interval is visible (the nodes are the
/// selection, which the chain's steps already filtered).
pub fn extract_subgraph(
    source: &DirGraph,
    selection: &CurrentSelection,
    valid_time: Option<&FluentFilter>,
) -> Result<DirGraph, String> {
    let level_idx = selection.get_level_count().saturating_sub(1);
    let level = selection
        .get_level(level_idx)
        .ok_or_else(|| "No active selection level".to_string())?;
    let nodes = level.get_all_nodes();
    let Some(filter) = valid_time.filter(|filter| !filter.is_empty()) else {
        return copy_induced_subgraph(source, &nodes, |_| true).map(|(graph, _)| graph);
    };
    let visible = |edge: EdgeIndex| {
        let (Some((from, _)), Some(weight)) = (
            source.graph.edge_endpoints(edge),
            source.graph.edge_weight(edge),
        ) else {
            return false;
        };
        filter
            .admits_edge(source, edge, weight.connection_type, from)
            .unwrap_or(false)
    };
    let (graph, _) = copy_induced_subgraph(source, &nodes, visible)?;
    filter.finish()?;
    Ok(graph)
}

/// A fresh graph holding `nodes` (copied in the order given) and every
/// relationship between two of them that `keep_edge` accepts, with the
/// source's secondary labels, ontology, schema and type metadata
/// ([`clone_subset_metadata`]). The map takes each copied
/// source node to its new index. Temporal declarations, embeddings, text and
/// vector indexes, spatial config and id indexes are not copied. The copy's
/// column stores hold only the copied nodes' rows.
pub(crate) fn copy_induced_subgraph(
    source: &DirGraph,
    nodes: &[NodeIndex],
    mut keep_edge: impl FnMut(EdgeIndex) -> bool,
) -> Result<(DirGraph, HashMap<NodeIndex, NodeIndex>), String> {
    // Arena guard: disk-backed node/edge reads materialize into the query
    // arena (protocol in disk/graph.rs); no-op on memory/mapped.
    let _arena_guard = source.graph.begin_query();
    let node_set: HashSet<NodeIndex> = nodes.iter().copied().collect();

    let mut new_graph = DirGraph::new();
    // Before the inserts: they encode rows against the interner and type
    // schemas, and only add to the type metadata.
    clone_subset_metadata(&mut new_graph, source);

    // Map from old node indices to new node indices
    let mut index_map: HashMap<NodeIndex, NodeIndex> = HashMap::with_capacity(nodes.len());

    // Each node is inserted into the copy's own column stores, read through
    // `GraphRead`: the copy never shares the source's store `Arc`s, so its
    // heap is its own rows — a mapped or disk store is file-backed, and the
    // first write into a shared one would clone every row of the type.
    for &old_idx in nodes {
        let Some(node) = source.graph.node_view(old_idx) else {
            continue;
        };
        let node_type = node.node_type_str(&source.interner).to_string();
        let mut id = node.id().into_owned();
        let mut title = node.title().into_owned();
        let mut properties = node.property_pairs();
        crate::graph::session::snapshot_property_values(
            &source.graph,
            [&mut id, &mut title]
                .into_iter()
                .chain(properties.iter_mut().map(|(_, value)| value)),
        );
        let properties: HashMap<String, Value> = properties
            .into_iter()
            .map(|(key, value)| (source.interner.resolve(key).to_string(), value))
            .collect();
        let new_idx = new_graph.insert_node_routed(id, title, &node_type, properties);
        index_map.insert(old_idx, new_idx);
        new_graph
            .type_indices
            .entry_or_default(node_type)
            .push(new_idx);
    }

    // Copy edges between selected nodes
    let mut rel_types = HashSet::new();
    for &old_source_idx in nodes {
        for edge in source.graph.edges(old_source_idx) {
            let old_target_idx = edge.target();

            // Only copy edge if target is also in selection
            if node_set.contains(&old_target_idx) && keep_edge(edge.id()) {
                if let (Some(&new_source), Some(&new_target)) = (
                    index_map.get(&old_source_idx),
                    index_map.get(&old_target_idx),
                ) {
                    // Clone edge data (properties are already interned)
                    let mut properties = edge.weight().properties.clone();
                    crate::graph::session::snapshot_property_values(
                        &source.graph,
                        properties.iter_mut().map(|(_, value)| value),
                    );
                    rel_types.insert(edge.weight().connection_type);
                    let edge_data =
                        EdgeData::new_interned(edge.weight().connection_type, properties);
                    GraphWrite::add_edge(&mut new_graph.graph, new_source, new_target, edge_data);
                }
            }
        }
    }

    retain_subset_types(&mut new_graph, &rel_types);

    // Carry secondary labels: buckets are keyed above the storage backend
    // (labels.rs), so neither the node copy nor the store share moved them —
    // pre-2026-08-26 this silently dropped every label from save_subset /
    // extract_subgraph. Copied through index_map (never re-derived: a manual
    // label must survive even where an ontology could not explain it), with
    // the sorted-bucket invariant restored by construction since index_map
    // values are assigned in ascending old-index iteration order per bucket
    // — sorted per bucket only if the old bucket was sorted AND add_node
    // assigned ascending, which holds; assert it anyway.
    for (label, bucket) in &source.secondary_label_index {
        let mut copied: Vec<NodeIndex> = bucket
            .iter()
            .filter_map(|old_idx| index_map.get(old_idx).copied())
            .collect();
        if copied.is_empty() {
            continue;
        }
        copied.sort_unstable();
        new_graph.secondary_label_index.insert(*label, copied);
        new_graph.has_secondary_labels = true;
    }
    // The declared ontology travels with the copy (type-level metadata,
    // like the schema install below).
    new_graph.ontology = Arc::clone(&source.ontology);

    // Copy schema definition if present. The subgraph's nodes are a subset of a
    // graph that already satisfied these constraints, so installing them cannot
    // find a duplicate the source did not have — but surface the error rather
    // than discard it, so a genuine inconsistency in the source is not laundered
    // into a silently unconstrained copy.
    if let Some(schema) = source.get_schema() {
        new_graph
            // The target is a fresh graph, so merge and replace coincide; merge
            // states the intent (install this schema) without also asserting
            // that everything unnamed should be withdrawn.
            .set_schema(schema.clone(), SchemaInstall::Merge)
            .map_err(|violation| format!("subgraph schema install failed: {violation}"))?;
    }

    Ok((new_graph, index_map))
}

/// Copy the graph-level metadata a subset needs to be self-contained on
/// reload: the interner and type schemas the rows are encoded against, the
/// source's node and relationship type metadata (every property the type has,
/// not only those the kept rows carry — the schema check reads it), the
/// alias/tier maps `describe()` and property resolution read, and the caller's
/// user-schema version. Both subset copies — [`copy_induced_subgraph`] and the
/// streaming disk writer — take it from here and narrow it with
/// [`retain_subset_types`] once the copy is built, so both save the same
/// metadata.
///
/// The version carries because a subset of a graph at user-schema version N
/// is still at version N — only which rows came along changed — so a
/// migration runner pointed at the subset must not re-run migrations `1..=N`.
pub(crate) fn clone_subset_metadata(dest: &mut DirGraph, source: &DirGraph) {
    dest.interner = source.interner.clone();
    dest.type_schemas = source.type_schemas.clone();
    dest.node_type_metadata = source.node_type_metadata.clone();
    dest.connection_type_metadata = source.connection_type_metadata.clone();
    dest.id_field_aliases = source.id_field_aliases.clone();
    dest.title_field_aliases = source.title_field_aliases.clone();
    dest.parent_types = source.parent_types.clone();
    dest.user_schema_version = source.user_schema_version;
}

/// Narrow the metadata [`clone_subset_metadata`] copied to the types the
/// subset holds: the node types with a copied node and `rel_types`, the
/// relationship types of the copied edges. A type the subset has none of is
/// not listed by it.
pub(crate) fn retain_subset_types(
    dest: &mut DirGraph,
    rel_types: &HashSet<crate::graph::schema::InternedKey>,
) {
    let present = |node_type: &str| dest.type_indices.contains_key(node_type);
    let node_type_metadata = dest
        .node_type_metadata
        .iter()
        .filter(|(node_type, _)| present(node_type))
        .map(|(node_type, props)| (node_type.clone(), props.clone()))
        .collect();
    let id_field_aliases = dest
        .id_field_aliases
        .iter()
        .filter(|(node_type, _)| present(node_type))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let title_field_aliases = dest
        .title_field_aliases
        .iter()
        .filter(|(node_type, _)| present(node_type))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let parent_types = dest
        .parent_types
        .iter()
        .filter(|(node_type, _)| present(node_type))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let rel_names: HashSet<&str> = rel_types
        .iter()
        .map(|key| dest.interner.resolve(*key))
        .collect();
    let connection_type_metadata = dest
        .connection_type_metadata
        .iter()
        .filter(|(rel_type, _)| rel_names.contains(rel_type.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    dest.node_type_metadata = Arc::new(node_type_metadata);
    dest.id_field_aliases = Arc::new(id_field_aliases);
    dest.title_field_aliases = Arc::new(title_field_aliases);
    dest.parent_types = Arc::new(parent_types);
    dest.connection_type_metadata = Arc::new(connection_type_metadata);
}

/// Get summary statistics about the subgraph that would be extracted.
///
/// Returns the number of nodes and edges that would be included.
pub fn get_subgraph_stats(
    source: &DirGraph,
    selection: &CurrentSelection,
) -> Result<SubgraphStats, String> {
    // Arena guard: disk-backed node/edge reads materialize into the query
    // arena (protocol in disk/graph.rs); no-op on memory/mapped.
    let _arena_guard = source.graph.begin_query();
    let level_idx = selection.get_level_count().saturating_sub(1);
    let level = selection
        .get_level(level_idx)
        .ok_or_else(|| "No active selection level".to_string())?;

    let nodes = level.get_all_nodes();
    let node_set: HashSet<NodeIndex> = nodes.iter().copied().collect();

    // Count edges between selected nodes
    let mut edge_count = 0;
    let mut connection_types: HashMap<String, usize> = HashMap::new();
    let mut node_types: HashMap<String, usize> = HashMap::new();

    // Count node types
    for &node_idx in &nodes {
        if let Some(node) = source.graph.node_view(node_idx) {
            *node_types
                .entry(node.node_type_str(&source.interner).to_string())
                .or_insert(0) += 1;
        }
    }

    // Count edges and connection types
    for &source_idx in &nodes {
        for edge in source.graph.edges(source_idx) {
            if node_set.contains(&edge.target()) {
                edge_count += 1;
                let conn_type = edge.weight().connection_type_str(&source.interner);
                *connection_types.entry(conn_type.to_string()).or_insert(0) += 1;
            }
        }
    }

    Ok(SubgraphStats {
        node_count: nodes.len(),
        edge_count,
        node_types,
        connection_types,
    })
}

/// Statistics about a potential subgraph extraction
#[derive(Debug, Clone)]
pub struct SubgraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub node_types: HashMap<String, usize>,
    pub connection_types: HashMap<String, usize>,
}
