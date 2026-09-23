//! `DELETE` and `DETACH DELETE` execution.
//!
//! The clause runs in four ordered stages — collect every target and authorize
//! it, verify a plain DELETE leaves no dangling edge, retire the relationship
//! slots, then commit — because openCypher deletes are statement-atomic: a
//! refusal must happen before the first storage mutation, and every row's
//! deletions must be visible to the checks the other rows run.

use std::collections::HashSet;

use super::relationship_identity::StatementRelationshipIdentities;
use super::write::check_interrupt_periodic;
use super::write_scope::{enforce_bound_edge_write_scope, enforce_node_write_scope};
use crate::datatypes::values::{RelValue, Value};
use crate::graph::algorithms::Interrupt;
use crate::graph::languages::cypher::ast::{DeleteClause, Expression};
use crate::graph::languages::cypher::result::{EdgeBinding, MutationStats, ResultRow, ResultSet};
use crate::graph::schema::DirGraph;
use crate::graph::storage::GraphRead;
use petgraph::graph::{EdgeIndex, NodeIndex};

/// Everything one DELETE statement removes, collected before any mutation.
#[derive(Default)]
struct DeleteTargets {
    nodes: HashSet<NodeIndex>,
    edges: HashSet<EdgeIndex>,
}

pub(super) fn execute_delete(
    graph: &mut DirGraph,
    delete: &DeleteClause,
    result_set: &ResultSet,
    stats: &mut MutationStats,
    interrupt: &Interrupt,
    relationship_identities: &mut StatementRelationshipIdentities,
) -> Result<(), String> {
    let targets = collect_delete_targets(
        graph,
        delete,
        result_set,
        interrupt,
        relationship_identities,
    )?;

    if !delete.detach {
        verify_nodes_keep_no_edges(graph, &targets, interrupt)?;
    }

    // Retire every relationship slot that this DELETE is about to remove.
    // DETACH DELETE owns incident edges that do not appear as named bindings,
    // so collect them while the nodes and their adjacency are still live.
    // The sparse statement-local generations then distinguish a later CREATE
    // that reuses one of these physical slots from a retained stale binding.
    invalidate_deleted_relationships(
        graph,
        &targets.nodes,
        &targets.edges,
        delete.detach,
        relationship_identities,
    )?;

    commit_deletions(graph, &targets, stats);
    Ok(())
}

/// Stage 1: collect all nodes and edges to delete across all rows, and
/// authorize each against the role-scoped write whitelist as it is first
/// seen. Authorization belongs *here*, not at the commit: a refusal must
/// return before any storage mutation (the refusal-before-mutation norm
/// stated at `set_row.rs`'s `enforce_write_scope` call), so a statement whose
/// 500th row is out of scope does not delete the first 499. The check is per
/// distinct node/edge, not per row — a `HashSet::insert` that returns false
/// has already been judged.
fn collect_delete_targets(
    graph: &DirGraph,
    delete: &DeleteClause,
    result_set: &ResultSet,
    interrupt: &Interrupt,
    relationship_identities: &StatementRelationshipIdentities,
) -> Result<DeleteTargets, String> {
    let mut targets = DeleteTargets::default();
    for (row_idx, row) in result_set.rows.iter().enumerate() {
        check_interrupt_periodic(interrupt, row_idx)?;
        for expr in &delete.expressions {
            let var_name = match expr {
                Expression::Variable(name) => name,
                other => return Err(format!("DELETE expects variable names, got {:?}", other)),
            };
            collect_row_target(graph, row, var_name, &mut targets, relationship_identities)?;
        }
    }
    Ok(targets)
}

fn collect_row_target(
    graph: &DirGraph,
    row: &ResultRow,
    var_name: &str,
    targets: &mut DeleteTargets,
    identities: &StatementRelationshipIdentities,
) -> Result<(), String> {
    if let Some(&node_idx) = row.node_bindings.get(var_name) {
        if targets.nodes.insert(node_idx) {
            enforce_node_write_scope(graph, node_idx)?;
        }
        return Ok(());
    }
    if let Some(edge_binding) = row.edge_bindings.get(var_name) {
        return collect_bound_relationship(graph, var_name, edge_binding, targets, identities);
    }
    // Not bound to a node/edge. A node or relationship VALUE (projected by
    // WITH / collect) is still deletable; anything else is NULL — e.g. an
    // unmatched OPTIONAL MATCH variable — and openCypher ignores NULL in
    // DELETE (so the idiomatic single-statement cascade `MATCH (root)
    // OPTIONAL MATCH (root)-->(child) DETACH DELETE root, child` works even
    // when a branch is empty). Skip it.
    collect_projected_target(graph, row, var_name, targets, identities)
}

fn collect_bound_relationship(
    graph: &DirGraph,
    var_name: &str,
    edge_binding: &EdgeBinding,
    targets: &mut DeleteTargets,
    identities: &StatementRelationshipIdentities,
) -> Result<(), String> {
    if !targets.edges.insert(edge_binding.edge_index) {
        return Ok(());
    }
    let token = edge_binding
        .incarnation
        .ok_or_else(|| format!("Relationship '{var_name}' has no statement-local identity"))?;
    if !identities.accepts(edge_binding.edge_index, token) {
        return Err(format!(
            "Relationship '{var_name}' is stale after its storage slot was reused"
        ));
    }
    enforce_bound_edge_write_scope(graph, edge_binding)
}

fn collect_projected_target(
    graph: &DirGraph,
    row: &ResultRow,
    var_name: &str,
    targets: &mut DeleteTargets,
    identities: &StatementRelationshipIdentities,
) -> Result<(), String> {
    match row.projected.get(var_name) {
        Some(Value::NodeRef(i)) => {
            let node_idx = NodeIndex::new(*i as usize);
            if targets.nodes.insert(node_idx) {
                enforce_node_write_scope(graph, node_idx)?;
            }
        }
        // A materialised node value (`collect(n)` / `RETURN n`) is deletable
        // too — this is the load-bearing case for `FOREACH (e IN collect(n) |
        // DETACH DELETE e)`, where the loop variable is bound in `projected`
        // as a `Value::Node`, not a `NodeRef`. Both `NodeValue` constructors
        // (`materialize_node_value` + the Variable-resolution path) set `id`
        // to the petgraph index, so it resolves the same way as `NodeRef`.
        // (Without this arm, DELETE inside FOREACH over a collected list was a
        // silent no-op.)
        Some(Value::Node(nv)) => {
            let node_idx = NodeIndex::new(nv.id as usize);
            if targets.nodes.insert(node_idx) {
                enforce_node_write_scope(graph, node_idx)?;
            }
        }
        // A materialised relationship value (`collect(r)` then UNWIND, or a
        // `FOREACH` loop variable) is deletable on the same grounds as the
        // node arms above. Without it the value fell through here and DELETE
        // was a silent no-op: no edge removed, no error. It carries its own
        // identity checks rather than the binding path's, because a value can
        // outlive the slot it names.
        Some(Value::Relationship(rel)) => {
            collect_projected_relationship(graph, var_name, rel, targets, identities)?;
        }
        _ => {}
    }
    Ok(())
}

fn collect_projected_relationship(
    graph: &DirGraph,
    var_name: &str,
    rel: &RelValue,
    targets: &mut DeleteTargets,
    identities: &StatementRelationshipIdentities,
) -> Result<(), String> {
    let edge_index = EdgeIndex::new(rel.id as usize);
    if !targets.edges.insert(edge_index) {
        return Ok(());
    }
    let token = rel.incarnation.ok_or_else(|| {
        format!("Relationship value '{var_name}' was not bound by this statement")
    })?;
    if !identities.accepts(edge_index, token) {
        return Err(format!(
            "Relationship '{var_name}' is stale after its storage slot was reused"
        ));
    }
    let binding = EdgeBinding {
        incarnation: Some(token),
        source: NodeIndex::new(rel.start_id as usize),
        target: NodeIndex::new(rel.end_id as usize),
        edge_index,
    };
    // The token is statement-local; a value from another graph or a rebuilt
    // slot can still name a live edge whose endpoints are not the ones the
    // value records. Deleting that edge would remove a relationship the caller
    // never selected.
    if graph.graph.edge_endpoints(edge_index) != Some((binding.source, binding.target)) {
        return Err(format!(
            "Relationship '{var_name}' no longer occupies the storage slot it names"
        ));
    }
    enforce_bound_edge_write_scope(graph, &binding)
}

/// Stage 2, plain DELETE only: verify no node keeps edges. Relationships
/// deleted by THIS statement don't count — openCypher deletes are
/// statement-atomic, so `MATCH (a)-[r]->(b) DELETE r, a` succeeds when `r`
/// covers every relationship attached to `a`.
fn verify_nodes_keep_no_edges(
    graph: &DirGraph,
    targets: &DeleteTargets,
    interrupt: &Interrupt,
) -> Result<(), String> {
    // Arena guard: node_weight (and the disk backend's edge iteration)
    // materialize into the query arena (protocol in disk/graph.rs); scoped so
    // the borrow ends before the caller's &mut commits.
    let _arena_guard = graph.graph.begin_query();
    for (node_count, &node_idx) in targets.nodes.iter().enumerate() {
        check_interrupt_periodic(interrupt, node_count)?;
        let has_edges = graph
            .graph
            .edges_directed(node_idx, petgraph::Direction::Outgoing)
            .any(|e| !targets.edges.contains(&e.id()))
            || graph
                .graph
                .edges_directed(node_idx, petgraph::Direction::Incoming)
                .any(|e| !targets.edges.contains(&e.id()));
        if has_edges {
            return Err(format!(
                "Cannot delete node '{}' because it still has relationships. Use DETACH DELETE to delete the node and all its relationships.",
                node_display_name(graph, node_idx)
            ));
        }
    }
    Ok(())
}

fn node_display_name(graph: &DirGraph, node_idx: NodeIndex) -> String {
    graph
        .graph
        .node_view(node_idx)
        .map(|n| {
            n.get_field_ref("name")
                .or_else(|| n.get_field_ref("title"))
                .map(|v| match v.as_ref() {
                    // Bare string, not the quoted Display form (and never the
                    // old Debug `String("…")`).
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| format!("index {}", node_idx.index()))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

/// Stage 4: infallible commit of the preflighted target set. Deliberately
/// non-interruptible: once deletion begins, completing it preserves atomic
/// statement semantics without an O(graph) rollback checkpoint.
fn commit_deletions(graph: &mut DirGraph, targets: &DeleteTargets, stats: &mut MutationStats) {
    for edge_index in targets.edges.iter().copied() {
        crate::graph::edge_embeddings::remove_edge_with_embeddings(graph, edge_index);
        stats.relationships_deleted += 1;
    }

    // The explicit edge-variable deletes above still need cache invalidation
    // (`detach_delete_nodes` only covers its own edges).
    if stats.relationships_deleted > 0 {
        graph.invalidate_edge_type_counts_cache();
        graph.connection_types.clear();
    }

    // DETACH-delete the nodes — incident edges, the nodes, and index cleanup.
    // For a plain DELETE, `verify_nodes_keep_no_edges` has established that the
    // nodes carry no edges, so none are removed here. Shared with
    // `purge_provisional_nodes` via `maintain::detach_delete_nodes`.
    //
    // Write scope: the incident edges removed here are **collateral of an
    // already-authorized node delete** and are deliberately not re-checked per
    // far endpoint. Re-checking would be both wrong and expensive — wrong
    // because a node the role may delete cannot be left behind as a dangling
    // half-edge just because it points at a type the role may not write, and
    // expensive because it is an O(degree) type resolution per deleted node.
    // Stage 1 authorized every node in `targets.nodes`; that authorization
    // covers everything attached to them.
    let (nodes_deleted, edges_removed) =
        crate::graph::mutation::maintain::detach_delete_nodes(graph, &targets.nodes);
    stats.nodes_deleted += nodes_deleted;
    stats.relationships_deleted += edges_removed;
}

pub(super) fn invalidate_deleted_relationships(
    graph: &DirGraph,
    nodes: &HashSet<NodeIndex>,
    explicitly_deleted: &HashSet<EdgeIndex>,
    detach: bool,
    identities: &mut StatementRelationshipIdentities,
) -> Result<(), String> {
    let mut edges = explicitly_deleted.clone();
    if detach {
        let _arena_guard = graph.graph.begin_query();
        for &node in nodes {
            edges.extend(
                graph
                    .graph
                    .edges_directed(node, petgraph::Direction::Outgoing)
                    .map(|edge| edge.id()),
            );
            edges.extend(
                graph
                    .graph
                    .edges_directed(node, petgraph::Direction::Incoming)
                    .map(|edge| edge.id()),
            );
        }
    }
    for edge in edges {
        identities.invalidate(edge)?;
    }
    Ok(())
}
