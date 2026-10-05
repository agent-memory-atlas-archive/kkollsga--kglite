//! A node has one title. `CREATE` and `MERGE`'s create arm refuse a declared
//! title field and `title` that disagree; on a type with no declared title
//! field, `title` is the title and `name` stays a readable property.

use crate::datatypes::values::{DataFrame, Value};
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::maintain::add_nodes;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn write(graph: &mut DirGraph, query: &str) -> Result<(), String> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows
}

fn s(value: &str) -> Value {
    Value::String(value.into())
}

#[test]
fn a_declared_title_field_and_a_different_title_are_refused() {
    let mut graph = DirGraph::new();
    let df = DataFrame::from_cypher_rows(
        vec!["id".into(), "label".into()],
        vec![vec![Value::Int64(1), s("L1")]],
    )
    .unwrap();
    add_nodes(
        &mut graph,
        df,
        "T".into(),
        "id".into(),
        Some("label".into()),
        None,
    )
    .unwrap();
    for query in [
        "CREATE (:T {id: 2, label: 'L2', title: 'Ann'})",
        "MERGE (:T {id: 2, label: 'L2', title: 'Ann'})",
    ] {
        let error = write(&mut graph, query).unwrap_err();
        assert!(error.contains("two different titles"), "{query}: {error}");
    }
    write(&mut graph, "CREATE (:T {id: 3, label: 'L3', title: 'L3'})").unwrap();
    assert_eq!(
        rows(&graph, "MATCH (n:T) RETURN n.id, n.title ORDER BY n.id"),
        vec![
            vec![Value::Int64(1), s("L1")],
            vec![Value::Int64(3), s("L3")]
        ]
    );
}

#[test]
fn title_is_the_title_and_name_stays_a_property() {
    let mut graph = DirGraph::new();
    write(&mut graph, "CREATE (:Q {id: 1, name: 'Nan', title: 'Ann'})").unwrap();
    write(&mut graph, "MERGE (:Q {id: 2, name: 'Nim', title: 'Bea'})").unwrap();
    write(&mut graph, "CREATE (:Q {id: 3, name: null, title: 'Cid'})").unwrap();
    write(&mut graph, "CREATE (:Q {id: 4, name: 'Dan'})").unwrap();
    assert_eq!(
        rows(
            &graph,
            "MATCH (n:Q) RETURN n.id, n.title, n.name ORDER BY n.id"
        ),
        vec![
            vec![Value::Int64(1), s("Ann"), s("Nan")],
            vec![Value::Int64(2), s("Bea"), s("Nim")],
            vec![Value::Int64(3), s("Cid"), s("Cid")],
            vec![Value::Int64(4), s("Dan"), s("Dan")],
        ]
    );
}
