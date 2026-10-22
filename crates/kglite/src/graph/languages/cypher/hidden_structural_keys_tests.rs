//! `ExecuteOptions::hide_structural_keys`: `keys(n)`, `properties(n)`,
//! `properties(r)` and `n {.*}` drop the virtual `title` and `type`.
//!
//! Absolute expected values throughout: the default shape is pinned elsewhere
//! (`node_record_golden_tests`), and this file pins what the opt-in changes
//! and what it must leave alone.

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn run(graph: &mut DirGraph, query: &str, hide: bool) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params).with_hidden_structural_keys(hide);
    let outcome = if query.contains("CREATE") || query.contains("SET") {
        execute_mut(graph, query, &opts)
    } else {
        execute_read(graph, query, &opts)
    };
    outcome
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
}

fn scalar(graph: &mut DirGraph, query: &str, hide: bool) -> Value {
    let rows = run(graph, query, hide);
    assert_eq!(rows.len(), 1, "{query}");
    rows[0][0].clone()
}

fn strings(items: &[&str]) -> Value {
    Value::List(items.iter().map(|s| Value::String(s.to_string())).collect())
}

fn keys_of(graph: &mut DirGraph, query: &str, hide: bool) -> Value {
    scalar(graph, query, hide)
}

fn graph() -> DirGraph {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (:J {id: 'database', v: 1}), \
                (:T {id: 1, title: 'Hello', type: 'custom'}), \
                (:U {id: 2, type: 'U'}), \
                (:K {k: 3})",
        false,
    );
    run(
        &mut g,
        "MATCH (a:J), (b:T) CREATE (a)-[:R {rid: 5}]->(b), (a)-[:S {type: 'mine'}]->(b)",
        false,
    );
    g
}

#[test]
fn the_default_maps_keep_every_structural_key() {
    let mut g = graph();
    assert_eq!(
        keys_of(&mut g, "MATCH (n:J) RETURN keys(n)", false),
        strings(&["id", "title", "type", "v"])
    );
    assert_eq!(
        keys_of(&mut g, "MATCH ()-[r:R]->() RETURN keys(r)", false),
        strings(&["rid", "type"])
    );
}

/// GraphBridge's migration-journal record: `id` is the user's key, `v` its
/// data, and the minted `title` / the type name are not stored properties.
#[test]
fn a_journal_record_is_its_stored_properties_only() {
    let mut g = graph();
    assert_eq!(
        keys_of(&mut g, "MATCH (n:J) RETURN keys(n)", true),
        strings(&["id", "v"])
    );
    let Value::Map(map) = scalar(&mut g, "MATCH (n:J) RETURN properties(n)", true) else {
        panic!("properties(n) is a map");
    };
    let pairs: Vec<(&str, &Value)> = map.iter().collect();
    assert_eq!(
        pairs,
        vec![
            ("id", &Value::String("database".into())),
            ("v", &Value::Int64(1))
        ]
    );
}

#[test]
fn user_data_named_title_or_type_survives() {
    let mut g = graph();
    // A title the user wrote and a stored `type` stay.
    assert_eq!(
        keys_of(&mut g, "MATCH (n:T) RETURN keys(n)", true),
        strings(&["id", "title", "type"])
    );
    assert_eq!(
        scalar(&mut g, "MATCH (n:T) RETURN properties(n).type", true),
        Value::String("custom".into())
    );
    assert_eq!(
        scalar(&mut g, "MATCH (n:T) RETURN properties(n).title", true),
        Value::String("Hello".into())
    );
    // A stored `type` equal to the label is still stored data; the minted
    // title is not.
    assert_eq!(
        keys_of(&mut g, "MATCH (n:U) RETURN keys(n)", true),
        strings(&["id", "type"])
    );
    // A node whose id was minted: `id` is still reported, because the engine
    // does not record whether it was supplied.
    assert_eq!(
        keys_of(&mut g, "MATCH (n:K) RETURN keys(n)", true),
        strings(&["id", "k"])
    );
}

#[test]
fn relationship_maps_hide_only_the_virtual_type() {
    let mut g = graph();
    assert_eq!(
        keys_of(&mut g, "MATCH ()-[r:R]->() RETURN keys(r)", true),
        strings(&["rid"])
    );
    assert_eq!(
        keys_of(&mut g, "MATCH ()-[r:S]->() RETURN keys(r)", true),
        strings(&["type"])
    );
    assert_eq!(
        scalar(&mut g, "MATCH ()-[r:S]->() RETURN properties(r).type", true),
        Value::String("mine".into())
    );
}

#[test]
fn keys_stay_in_lockstep_with_properties_and_projection() {
    let mut g = graph();
    for label in ["J", "T", "U", "K"] {
        let a = keys_of(&mut g, &format!("MATCH (n:{label}) RETURN keys(n)"), true);
        let b = keys_of(
            &mut g,
            &format!("MATCH (n:{label}) RETURN keys(properties(n))"),
            true,
        );
        let c = keys_of(
            &mut g,
            &format!("MATCH (n:{label}) RETURN keys(n {{.*}})"),
            true,
        );
        assert_eq!(a, b, "{label}: keys(n) vs keys(properties(n))");
        assert_eq!(a, c, "{label}: keys(n) vs keys(n {{.*}})");
    }
}

#[test]
fn a_collected_node_value_follows_the_same_rule() {
    let mut g = graph();
    assert_eq!(
        keys_of(
            &mut g,
            "MATCH (n:J) WITH collect(n) AS ns RETURN keys(ns[0])",
            true
        ),
        strings(&["id", "v"])
    );
    assert_eq!(
        keys_of(
            &mut g,
            "MATCH (n:T) WITH collect(n) AS ns RETURN keys(ns[0])",
            true
        ),
        strings(&["id", "title", "type"])
    );
}

/// Only the three property-map surfaces change: a whole node still carries
/// `title` and `type`, so reads on a collected node keep working.
#[test]
fn return_n_and_property_reads_are_untouched() {
    let mut g = graph();
    let rows = run(&mut g, "MATCH (n:J) RETURN n", true);
    let Value::Node(node) = &rows[0][0] else {
        panic!("RETURN n is a node");
    };
    let keys: Vec<&str> = node.properties.keys().collect();
    assert_eq!(keys, vec!["id", "title", "type", "v"]);
    assert_eq!(
        scalar(
            &mut g,
            "MATCH (n:J) WITH collect(n) AS ns RETURN ns[0].title",
            true
        ),
        Value::String("J_database".into())
    );
    assert_eq!(
        scalar(&mut g, "MATCH (n:J) RETURN n.type", true),
        Value::String("J".into())
    );
}

#[test]
fn a_mutation_that_returns_the_map_hides_too() {
    let mut g = graph();
    assert_eq!(
        scalar(&mut g, "MATCH (n:J) SET n.v = 2 RETURN keys(n)", true),
        strings(&["id", "v"])
    );
    assert_eq!(
        scalar(&mut g, "CREATE (n:Z {id: 9, w: 1}) RETURN keys(n)", true),
        strings(&["id", "w"])
    );
    // And the same statement in default mode still reports all three.
    assert_eq!(
        scalar(&mut g, "MATCH (n:J) SET n.v = 3 RETURN keys(n)", false),
        strings(&["id", "title", "type", "v"])
    );
}
