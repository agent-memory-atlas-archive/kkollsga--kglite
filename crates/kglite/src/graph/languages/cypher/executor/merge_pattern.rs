//! MERGE's match arm: does the pattern already exist?
//!
//! Only the *lookup* lives here — a miss is reported to [`super::write`]'s
//! `execute_merge`, which then runs the pattern through CREATE.

use std::collections::HashMap;

use super::identity_fields::merge_expected_props;
use super::CypherExecutor;
use crate::datatypes::values::Value;
use crate::graph::languages::cypher::ast::{
    CreateEdgeDirection, CreateEdgePattern, CreateElement, CreateNodePattern, CreatePattern,
};
use crate::graph::languages::cypher::result::{EdgeBinding, ResultRow};
use crate::graph::schema::{DirGraph, EdgeData, InternedKey};
use crate::graph::storage::GraphRead;
use petgraph::graph::NodeIndex;

/// Returns the bound row when the pattern already exists, `None` when it does
/// not (the caller then CREATEs it).
pub(super) fn try_match_merge_pattern(
    graph: &DirGraph,
    pattern: &CreatePattern,
    row: &ResultRow,
    params: &HashMap<String, Value>,
) -> Result<Option<ResultRow>, String> {
    let executor = CypherExecutor::with_params(graph, params, None);

    match pattern.elements.len() {
        // Node-only MERGE: (var:Label {key: val, ...})
        1 => match &pattern.elements[0] {
            CreateElement::Node(node_pat) => match_node_pattern(graph, &executor, node_pat, row),
            _ => Err("MERGE pattern must start with a node".to_string()),
        },
        // Relationship MERGE: (a)-[r:TYPE]->(b)
        3 => match_relationship_pattern(graph, &executor, pattern, row),
        _ => Err("MERGE supports single-node or single-edge patterns only".to_string()),
    }
}

fn match_node_pattern(
    graph: &DirGraph,
    executor: &CypherExecutor<'_>,
    node_pat: &CreateNodePattern,
    row: &ResultRow,
) -> Result<Option<ResultRow>, String> {
    // If variable is already bound from prior MATCH, it's already matched
    if let Some(ref var) = node_pat.variable {
        if let Some(&existing_idx) = row.node_bindings.get(var) {
            if graph.graph.node_view(existing_idx).is_some() {
                let mut result_row = ResultRow::new();
                result_row.node_bindings.insert(var.clone(), existing_idx);
                return Ok(Some(result_row));
            }
        }
    }

    let label = node_pat.label.as_deref().unwrap_or("Node");

    // The id/property/composite indexes and `type_indices` are keyed by
    // PRIMARY type. If `label` also occurs as a secondary label on some node,
    // those structures miss the secondary-labelled candidates and would
    // falsely report "no match" → MERGE creates a duplicate. In that case skip
    // the index short-circuits and scan the full primary∪secondary candidate
    // set (`nodes_with_label`). The common case (label has no secondary
    // occurrences) keeps every index fast path.
    let label_has_secondary = graph.has_secondary_labels
        && graph
            .secondary_label_index
            .contains_key(&InternedKey::from_str(label));

    let expected_props = merge_expected_props(executor, node_pat, row, graph)?;

    if !label_has_secondary {
        match probe_node_indexes(graph, label, &expected_props) {
            IndexProbe::Matched(idx) => return Ok(Some(node_result_row(node_pat, idx))),
            IndexProbe::NoMatch => return Ok(None),
            IndexProbe::Unindexed => {}
        }
    }

    // Fall back to linear scan (no index covers the pattern, or `label` has
    // secondary occurrences). `nodes_with_label` unions primary + secondary
    // candidates (and is the identical `type_indices` clone when no secondary
    // labels exist).
    for idx in graph.nodes_with_label(label) {
        if node_matches_all(graph, idx, &expected_props) {
            return Ok(Some(node_result_row(node_pat, idx)));
        }
    }
    Ok(None)
}

/// What the index short-circuits settled. `NoMatch` is authoritative — the
/// index that answered covers the whole pattern, so no scan can add a match —
/// while `Unindexed` means no index applies and the caller must scan.
enum IndexProbe {
    Matched(NodeIndex),
    NoMatch,
    Unindexed,
}

fn probe_node_indexes(
    graph: &DirGraph,
    label: &str,
    expected_props: &[(&str, Value)],
) -> IndexProbe {
    // 1. If pattern contains "id" property, use O(1) id_index lookup
    if let Some((_, id_value)) = expected_props.iter().find(|(k, _)| *k == "id") {
        if let Some(idx) = graph.lookup_by_id_readonly(label, id_value) {
            if expected_props.len() == 1 || node_matches_all(graph, idx, expected_props) {
                return IndexProbe::Matched(idx);
            }
        }
        return IndexProbe::NoMatch;
    }

    // 2. Single non-id property: try property index.
    // Probed under the pattern's own spelling — `lookup_by_index` resolves a
    // type's registered title/id alias itself. The old `name`/`title` →
    // `title` remap was not that: on a type whose `name` is an ordinary stored
    // property distinct from the title, it asked the title index a question
    // about `name` and MERGE created a duplicate of the node it missed.
    if expected_props.len() == 1 {
        let (key, ref value) = expected_props[0];
        if let Some(candidates) = graph.lookup_by_index(label, key, value) {
            for &idx in &candidates {
                if node_matches_all(graph, idx, expected_props) {
                    return IndexProbe::Matched(idx);
                }
            }
            return IndexProbe::NoMatch;
        }
        // No index — fall through to linear scan
    }

    // 3. Multi-property: try composite index
    if expected_props.len() >= 2 {
        // Composite lookup excludes id/name/title: they use special storage,
        // not ordinary property columns.
        let mut indexable: Vec<(&str, &Value)> = expected_props
            .iter()
            .filter(|(k, _)| *k != "id" && *k != "name" && *k != "title")
            .map(|(k, v)| (*k, v))
            .collect();
        if indexable.len() >= 2 {
            indexable.sort_by(|a, b| a.0.cmp(b.0));
            let names: Vec<String> = indexable.iter().map(|(k, _)| k.to_string()).collect();
            let values: Vec<Value> = indexable.iter().map(|(_, v)| (*v).clone()).collect();
            if let Some(candidates) = graph.lookup_by_composite_predicate(label, &names, &values) {
                for &idx in &candidates {
                    if node_matches_all(graph, idx, expected_props) {
                        return IndexProbe::Matched(idx);
                    }
                }
                return IndexProbe::NoMatch;
            }
        }
    }

    IndexProbe::Unindexed
}

fn node_matches_all(graph: &DirGraph, idx: NodeIndex, props: &[(&str, Value)]) -> bool {
    let Some(node) = graph.graph.node_view(idx) else {
        return false;
    };
    let node_type = node.node_type_str(&graph.interner);
    props.iter().all(|(key, expected)| {
        let value = node.resolved_field(node_type, key, InternedKey::from_str(key));
        value.as_deref().is_some_and(|value| {
            if *key == "id" {
                // Identity matching keeps its normalization policy; ordinary
                // properties use Cypher predicate equality.
                value == expected
            } else {
                crate::graph::core::filtering::values_equal(value, expected)
            }
        })
    })
}

fn node_result_row(node_pat: &CreateNodePattern, idx: NodeIndex) -> ResultRow {
    let mut result_row = ResultRow::new();
    if let Some(ref var) = node_pat.variable {
        result_row.node_bindings.insert(var.clone(), idx);
    }
    result_row
}

fn match_relationship_pattern(
    graph: &DirGraph,
    executor: &CypherExecutor<'_>,
    pattern: &CreatePattern,
    row: &ResultRow,
) -> Result<Option<ResultRow>, String> {
    let source_var = create_node_variable(&pattern.elements[0]);
    let target_var = create_node_variable(&pattern.elements[2]);

    let source_idx = source_var
        .and_then(|v| row.node_bindings.get(v).copied())
        .ok_or("MERGE path: source node must be bound by prior MATCH")?;
    let target_idx = target_var
        .and_then(|v| row.node_bindings.get(v).copied())
        .ok_or("MERGE path: target node must be bound by prior MATCH")?;

    let CreateElement::Edge(edge_pat) = &pattern.elements[1] else {
        return Err("Expected edge in MERGE path pattern".to_string());
    };

    let (actual_src, actual_tgt) = match edge_pat.direction {
        CreateEdgeDirection::Outgoing => (source_idx, target_idx),
        CreateEdgeDirection::Incoming => (target_idx, source_idx),
    };

    let interned_ct = InternedKey::from_str(&edge_pat.connection_type);
    // The pattern's relationship properties are part of the match, exactly as
    // the node arm's are. Without them, parallel members between the same
    // endpoints are indistinguishable and `edges_directed(..).find(..)` binds
    // whichever one adjacency yields first — so `MERGE (a)-[r:T {k:0}]->(b)`
    // could bind the `{k:1}` member and run ON MATCH SET against it, and a
    // pattern matching no member never reached the CREATE arm.
    let expected_props = merge_expected_edge_props(executor, edge_pat, row, graph)?;
    let matching_edge = graph
        .graph
        .edges_directed(actual_src, petgraph::Direction::Outgoing)
        .find(|e| {
            e.target() == actual_tgt
                && e.weight().connection_type == interned_ct
                && edge_matches_all(e.weight(), &expected_props)
        });

    let Some(edge_ref) = matching_edge else {
        return Ok(None);
    };
    let mut result_row = ResultRow::new();
    if let Some(ref var) = edge_pat.variable {
        result_row.edge_bindings.insert(
            var.clone(),
            EdgeBinding {
                incarnation: None,
                source: actual_src,
                target: actual_tgt,
                edge_index: edge_ref.id(),
            },
        );
    }
    Ok(Some(result_row))
}

fn create_node_variable(element: &CreateElement) -> Option<&str> {
    match element {
        CreateElement::Node(np) => np.variable.as_deref(),
        _ => None,
    }
}

/// The property values a relationship MERGE pattern expects, in the pattern's
/// own spelling.
///
/// The node counterpart ([`merge_expected_props`]) canonicalises each key
/// through the type's identity aliases; relationships have no identity columns,
/// so a key here always names an ordinary stored property. Endpoint references
/// are snapshotted for the same reason the node path snapshots them: stored
/// properties hold resolved values, so an unresolved one would never compare
/// equal to what is on the edge.
fn merge_expected_edge_props<'p>(
    executor: &CypherExecutor<'_>,
    edge_pat: &'p CreateEdgePattern,
    row: &ResultRow,
    graph: &DirGraph,
) -> Result<Vec<(&'p str, Value)>, String> {
    edge_pat
        .properties
        .iter()
        .map(|(key, expr)| {
            let mut value = executor.evaluate_expression(expr, row)?;
            crate::graph::session::snapshot_property_values(
                &graph.graph,
                std::iter::once(&mut value),
            );
            Ok((key.as_str(), value))
        })
        .collect()
}

/// Whether a candidate edge carries every property the MERGE pattern spelled,
/// under the same Cypher predicate equality the node arm applies. A property
/// the edge does not carry never matches.
fn edge_matches_all(edge: &EdgeData, expected: &[(&str, Value)]) -> bool {
    expected.iter().all(|(key, value)| {
        edge.get_property(key)
            .is_some_and(|stored| crate::graph::core::filtering::values_equal(stored, value))
    })
}
