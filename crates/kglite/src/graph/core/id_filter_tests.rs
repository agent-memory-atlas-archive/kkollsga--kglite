//! `where({"id": x})` answers from the id index; it must agree with the scan
//! for every target kind and every input shape.

use std::collections::HashMap;

use petgraph::graph::NodeIndex;

use super::{filter_nodes_by_conditions, id_index_candidates, values_equal};
use crate::datatypes::values::FilterCondition;
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::GraphRead;

fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    let params: HashMap<String, Value> = HashMap::new();
    for query in [
        "UNWIND range(1, 40) AS i CREATE (:A {id: i, name: 'a'})",
        "UNWIND range(1, 40) AS i CREATE (:B {id: 'x' + toString(i), name: 'b'})",
        "CREATE (:B {id: '5', name: 'b'}), (:B {id: '7', name: 'b'})",
        "UNWIND range(1, 5) AS i CREATE (:C {id: toFloat(i), name: 'c'})",
    ] {
        execute_mut(&mut graph, query, &ExecuteOptions::eager(&params))
            .unwrap_or_else(|e| panic!("{query}: {e}"));
    }
    graph
}

fn scan(graph: &DirGraph, nodes: &[NodeIndex], target: &Value) -> Vec<NodeIndex> {
    nodes
        .iter()
        .copied()
        .filter(|&idx| {
            graph
                .node_view(idx)
                .and_then(|n| n.get_field_ref("id").map(|v| values_equal(&v, target)))
                .unwrap_or(false)
        })
        .collect()
}

fn targets() -> Vec<Value> {
    vec![
        Value::Int64(5),
        Value::Int64(7),
        Value::Int64(999),
        Value::Int64(-1),
        Value::UniqueId(5),
        Value::Float64(5.0),
        Value::Float64(5.5),
        Value::String("5".into()),
        Value::String("7".into()),
        Value::String("x3".into()),
        Value::String("nope".into()),
        Value::Boolean(true),
        Value::Null,
    ]
}

fn sorted(mut v: Vec<NodeIndex>) -> Vec<NodeIndex> {
    v.sort();
    v
}

fn filtered(graph: &DirGraph, nodes: Vec<NodeIndex>, target: &Value) -> Vec<NodeIndex> {
    let mut conditions = HashMap::new();
    conditions.insert("id".to_string(), FilterCondition::Equals(target.clone()));
    sorted(filter_nodes_by_conditions(graph, nodes, &conditions))
}

#[test]
fn id_equality_agrees_with_the_scan_on_every_input_shape() {
    let graph = graph();
    let all: Vec<NodeIndex> = graph.graph.node_indices().collect();
    let by_type = |t: &str| graph.nodes_with_label(t);
    let inputs: Vec<(&str, Vec<NodeIndex>)> = vec![
        ("every node", all.clone()),
        ("A in full", by_type("A")),
        ("B in full", by_type("B")),
        ("C in full", by_type("C")),
        ("A subset", by_type("A").into_iter().take(10).collect()),
        ("A and B subset", {
            let mut v: Vec<_> = by_type("A").into_iter().take(10).collect();
            v.extend(by_type("B").into_iter().take(10));
            v
        }),
    ];
    // Build every type's id index so subset inputs may use it as well.
    for t in ["A", "B", "C"] {
        graph.lookup_by_id_readonly(t, &Value::Int64(1));
    }
    for (label, nodes) in inputs {
        for target in targets() {
            assert_eq!(
                filtered(&graph, nodes.clone(), &target),
                sorted(scan(&graph, &nodes, &target)),
                "{label}, id = {target:?}"
            );
        }
    }
}

#[test]
fn id_equality_finds_the_one_node_through_the_index() {
    let graph = graph();
    let a = graph.nodes_with_label("A");
    let hit = filtered(&graph, a, &Value::Int64(5));
    assert_eq!(hit.len(), 1);
    assert_eq!(
        graph
            .node_view(hit[0])
            .and_then(|n| n.get_field_ref("id").map(|v| v.into_owned())),
        Some(Value::Int64(5))
    );
}

#[test]
fn a_type_with_repeated_ids_is_answered_by_the_scan() {
    let mut graph = DirGraph::new();
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(
        &mut graph,
        "CREATE (:D {id: 1, name: 'x'}), (:D {id: 1, name: 'y'}), (:D {id: 2, name: 'z'})",
        &ExecuteOptions::eager(&params),
    )
    .expect("seed");
    let nodes = graph.nodes_with_label("D");
    let hits = filtered(&graph, nodes.clone(), &Value::Int64(1));
    assert_eq!(hits.len(), 2, "both copies of the repeated id");
    assert_eq!(hits, sorted(scan(&graph, &nodes, &Value::Int64(1))));
}

#[test]
fn the_index_answers_when_ids_are_unique_and_declines_when_not() {
    let graph = graph();
    let a = graph.nodes_with_label("A");
    let types: std::collections::HashSet<&str> = ["A"].into_iter().collect();
    let hits = id_index_candidates(&graph, &a, &types, true, &Value::Int64(5));
    assert_eq!(hits.map(|h| h.len()), Some(1));
    assert!(id_index_candidates(&graph, &a, &types, true, &Value::Float64(5.0)).is_none());
}
