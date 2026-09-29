//! MERGE rows reuse one scratch pattern for their evaluated properties: none
//! at all when every property is a literal, and each row's values — never the
//! previous row's — in the match and the create arm.

use super::merge_row_scratch;
use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::languages::cypher::ast::{Clause, CreatePattern};
use crate::graph::languages::cypher::parser::parse_cypher;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn merge_pattern(query: &str) -> CreatePattern {
    parse_cypher(query)
        .unwrap()
        .clauses
        .into_iter()
        .find_map(|clause| match clause {
            Clause::Merge(merge) => Some(merge.pattern),
            _ => None,
        })
        .expect("a MERGE clause")
}

fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows
}

#[test]
fn an_all_literal_merge_takes_no_scratch() {
    assert!(merge_row_scratch(&merge_pattern("MERGE (n:Item {id: 5, name: 'x'})")).is_none());
    assert!(merge_row_scratch(&merge_pattern("MERGE (n:Item)")).is_none());
    assert!(merge_row_scratch(&merge_pattern(
        "UNWIND [1] AS r MERGE (n:Item {id: 5, name: r})"
    ))
    .is_some());
    assert!(merge_row_scratch(&merge_pattern(
        "MATCH (a), (b) WITH a, b, 1 AS r MERGE (a)-[:T {k: r}]->(b)"
    ))
    .is_some());
}

#[test]
fn each_row_matches_and_creates_its_own_values() {
    let mut graph = DirGraph::new();
    let params = HashMap::new();
    execute_mut(
        &mut graph,
        "UNWIND [1, 2, 1, 3, 2] AS r MERGE (n:I {id: r, k: r * 10, tag: 'x'}) \
         ON MATCH SET n.hits = coalesce(n.hits, 0) + 1",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    assert_eq!(
        rows(&graph, "MATCH (n:I) RETURN n.id, n.k, n.hits ORDER BY n.id"),
        vec![
            vec![Value::Int64(1), Value::Int64(10), Value::Int64(1)],
            vec![Value::Int64(2), Value::Int64(20), Value::Int64(1)],
            vec![Value::Int64(3), Value::Int64(30), Value::Null],
        ]
    );
    execute_mut(
        &mut graph,
        "MATCH (a:I {id: 1}), (b:I {id: 2}) UNWIND [7, 8, 7] AS w \
         MERGE (a)-[:T {w: w}]->(b)",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    assert_eq!(
        rows(&graph, "MATCH (:I)-[t:T]->(:I) RETURN t.w ORDER BY t.w"),
        vec![vec![Value::Int64(7)], vec![Value::Int64(8)]]
    );
}
