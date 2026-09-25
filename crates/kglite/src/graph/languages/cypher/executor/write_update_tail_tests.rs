//! A query whose last clause is an updating clause returns no rows and no
//! columns, whatever a preceding `WITH` projected; its writes still apply.

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn seeded() -> DirGraph {
    let mut graph = DirGraph::new();
    let params = HashMap::new();
    execute_mut(
        &mut graph,
        "CREATE (:N {id: 1})-[:R]->(:N {id: 2})-[:R]->(:N {id: 3})",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    graph
}

fn count(graph: &DirGraph, query: &str) -> Value {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows[0][0]
        .clone()
}

#[test]
fn an_updating_last_clause_returns_nothing() {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    for (query, check, expected) in [
        (
            "MATCH ()-[r]->() WITH collect(r) AS xs DELETE xs",
            "MATCH ()-[r]->() RETURN count(r)",
            0,
        ),
        (
            "MATCH (n:N) WITH collect(n) AS ns UNWIND ns AS m SET m.y = 2",
            "MATCH (n:N) WHERE n.y = 2 RETURN count(n)",
            3,
        ),
        (
            "MATCH (n:N {id: 1}) WITH n, 5 AS k SET n.k = k",
            "MATCH (n:N) WHERE n.k = 5 RETURN count(n)",
            1,
        ),
        ("WITH 1 AS x CREATE (:W)", "MATCH (w:W) RETURN count(w)", 1),
        (
            "MATCH (n:N) WITH n.id AS i MERGE (:Q {id: i})",
            "MATCH (q:Q) RETURN count(q)",
            3,
        ),
        (
            "MATCH (n:N) WITH n, n.id AS i REMOVE n.y",
            "MATCH (n:N) RETURN count(n)",
            3,
        ),
        (
            "MATCH (n:N) WITH n, 1 AS one FOREACH (x IN [one] | SET n.f = x)",
            "MATCH (n:N) WHERE n.f = 1 RETURN count(n)",
            3,
        ),
    ] {
        let mut graph = seeded();
        let result = execute_mut(&mut graph, query, &opts).unwrap().result;
        assert!(result.rows.is_empty(), "{query} returned {:?}", result.rows);
        assert!(
            result.columns.is_empty(),
            "{query} returned columns {:?}",
            result.columns
        );
        assert_eq!(count(&graph, check), Value::Int64(expected), "{query}");
    }
}

#[test]
fn a_return_or_a_procedure_call_still_returns_rows() {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut graph = seeded();
    let result = execute_mut(
        &mut graph,
        "MATCH (n:N) WITH n SET n.z = 1 RETURN n.id AS id",
        &opts,
    )
    .unwrap()
    .result;
    assert_eq!(result.rows.len(), 3);
    let result = execute_mut(&mut graph, "CALL db.cdc.enable()", &opts)
        .unwrap()
        .result;
    assert_eq!(result.rows.len(), 1);
}
