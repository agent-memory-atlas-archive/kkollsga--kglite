//! A node MERGE whose pattern covers a declared unique tuple stops at the one
//! node holding it. Each test breaks the invariant on purpose, planting a
//! second node on an occupied tuple behind the constraint's back; a MERGE that
//! returns one row stopped at the unique index's occupant, and one that returns
//! two scanned the label.

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn write(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params)).unwrap();
}

fn count(graph: &mut DirGraph, query: &str) -> Value {
    let params = HashMap::new();
    let mut rows = execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows;
    rows.remove(0).remove(0)
}

fn labelled_count(graph: &DirGraph, label: &str) -> usize {
    let params = HashMap::new();
    let query = format!("MATCH (n:{label}) RETURN count(n)");
    let rows = execute_read(graph, &query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows;
    match rows[0][0] {
        Value::Int64(n) => n as usize,
        ref other => panic!("count was {other:?}"),
    }
}

/// A `U` graph under `declare`, holding `base`, with `hidden` then created
/// while the unique index is emptied, so the constraint never saw it.
fn graph_with_hidden_duplicate(declare: &str, base: &str, hidden: &str) -> DirGraph {
    let mut graph = DirGraph::new();
    write(&mut graph, declare);
    write(&mut graph, base);
    let saved = graph.unique_indices.clone();
    for index in graph.unique_indices.values_mut() {
        index.clear();
    }
    write(&mut graph, hidden);
    graph.unique_indices = saved;
    graph
}

const UNIQUE_K: &str = "CREATE CONSTRAINT FOR (n:U) REQUIRE n.k IS UNIQUE";
const BASE: &str = "CREATE (:U {k: 'a', v: 1, w: 1}), (:U {k: 'b', v: 1, w: 2})";
const HIDDEN: &str = "CREATE (:U {k: 'a', v: 1, w: 3})";

#[test]
fn a_unique_key_stops_at_its_occupant() {
    let mut graph = graph_with_hidden_duplicate(UNIQUE_K, BASE, HIDDEN);
    assert_eq!(
        count(&mut graph, "MERGE (n:U {k: 'a'}) RETURN count(n)"),
        Value::Int64(1)
    );
    assert_eq!(labelled_count(&graph, "U"), 3);
}

#[test]
fn an_integer_unique_key_stops_at_its_occupant() {
    let mut graph = DirGraph::new();
    write(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:U) REQUIRE n.k IS UNIQUE",
    );
    write(&mut graph, "CREATE (:U {k: 7})");
    let saved = graph.unique_indices.clone();
    for index in graph.unique_indices.values_mut() {
        index.clear();
    }
    write(&mut graph, "CREATE (:U {k: 7})");
    graph.unique_indices = saved;
    assert_eq!(
        count(&mut graph, "MERGE (n:U {k: 7}) RETURN count(n)"),
        Value::Int64(1)
    );
}

#[test]
fn a_composite_unique_key_needs_every_property_in_the_pattern() {
    let mut graph = graph_with_hidden_duplicate(
        "CREATE CONSTRAINT FOR (n:U) REQUIRE (n.k, n.v) IS UNIQUE",
        BASE,
        HIDDEN,
    );
    // The whole tuple is named, and `(a, 1)` has a single occupant.
    assert_eq!(
        count(&mut graph, "MERGE (n:U {k: 'a', v: 1}) RETURN count(n)"),
        Value::Int64(1)
    );
    // Half the tuple does not identify a node; the scan finds both.
    assert_eq!(
        count(&mut graph, "MERGE (n:U {k: 'a'}) RETURN count(n)"),
        Value::Int64(2)
    );
}

#[test]
fn a_key_that_is_not_the_unique_one_scans_every_node() {
    let mut graph = graph_with_hidden_duplicate(
        "CREATE CONSTRAINT FOR (n:U) REQUIRE n.w IS UNIQUE",
        BASE,
        HIDDEN,
    );
    assert_eq!(
        count(&mut graph, "MERGE (n:U {k: 'a'}) RETURN count(n)"),
        Value::Int64(2)
    );
}

#[test]
fn no_constraint_returns_every_match() {
    let mut graph = DirGraph::new();
    write(&mut graph, "CREATE (:U {k: 'a'}), (:U {k: 'a'})");
    assert_eq!(
        count(&mut graph, "MERGE (n:U {k: 'a'}) RETURN count(n)"),
        Value::Int64(2)
    );
}

#[test]
fn an_unoccupied_unique_key_creates_one_node() {
    let mut graph = graph_with_hidden_duplicate(UNIQUE_K, BASE, HIDDEN);
    assert_eq!(
        count(&mut graph, "MERGE (n:U {k: 'c'}) RETURN count(n)"),
        Value::Int64(1)
    );
    assert_eq!(labelled_count(&graph, "U"), 4);
}
