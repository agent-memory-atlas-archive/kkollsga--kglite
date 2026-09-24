//! Relationship text indexes: build, query, freshness at every hooked write,
//! rollback, stale-binding safety and vacuum. The oracle throughout is a
//! wholesale rebuild of the same graph, as in `text_indexes_freshness_tests`.

use super::*;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn params(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect()
}

fn run(graph: &mut DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("query failed: {query}: {e}"))
        .result
        .rows
}

fn run_err(graph: &mut DirGraph, query: &str) -> String {
    let params = HashMap::new();
    match execute_mut(graph, query, &ExecuteOptions::eager(&params)) {
        Ok(outcome) => panic!("expected {query} to fail, got {:?}", outcome.result.rows),
        Err(error) => error.to_string(),
    }
}

/// Three documents on `CLAIMS` (k = 0, 1, 2 between the same two nodes — a
/// parallel group) plus one `TAG` relationship that is never indexed.
fn claims_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), \
         (a)-[:CLAIMS {k: 0, text: 'the quick brown fox'}]->(b), \
         (a)-[:CLAIMS {k: 1, text: 'a quick brown marmoset appears'}]->(b), \
         (a)-[:CLAIMS {k: 2, text: 'slow green turtles'}]->(b), \
         (a)-[:TAG {text: 'quick'}]->(b)",
    );
    run(
        &mut graph,
        "CALL db.edge_text_index.build({type: 'CLAIMS', property: 'text'}) YIELD indexed RETURN indexed",
    );
    graph
}

fn store(graph: &DirGraph) -> &TextIndexStore {
    edge_text_index_store(graph, "CLAIMS", "text").expect("index built")
}

/// `(k, text_bm25)` for every live `CLAIMS` relationship, through Cypher —
/// which also runs the query-entry refresh.
fn scores(graph: &DirGraph, query: &str) -> Vec<(Value, Value)> {
    let params = params(&[("q", Value::String(query.into()))]);
    execute_read(
        graph,
        "MATCH ()-[r:CLAIMS]->() RETURN r.k AS k, text_bm25(r, 'text', $q) AS s ORDER BY k",
        &ExecuteOptions::eager(&params),
    )
    .expect("score query")
    .result
    .rows
    .into_iter()
    .map(|row| (row[0].clone(), row[1].clone()))
    .collect()
}

fn rebuilt_scores(graph: &DirGraph, query: &str) -> Vec<(Value, Value)> {
    let mut rebuilt = graph.clone();
    build_edge_text_index(&mut rebuilt, "CLAIMS", "text", None).expect("rebuild");
    scores(&rebuilt, query)
}

fn assert_matches_rebuild(graph: &DirGraph, query: &str) {
    assert_eq!(
        scores(graph, query),
        rebuilt_scores(graph, query),
        "a refreshed relationship index must equal a rebuilt one ({query})"
    );
    assert!(store(graph).validate().is_ok());
}

fn score_of(rows: &[(Value, Value)], k: i64) -> Value {
    rows.iter()
        .find(|(key, _)| *key == Value::Int64(k))
        .map(|(_, score)| score.clone())
        .unwrap_or_else(|| panic!("no row for k={k}: {rows:?}"))
}

#[test]
fn build_reports_and_scores_like_the_node_lane() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), \
         (a)-[:CLAIMS {k: 0, text: 'quick fox'}]->(b), \
         (a)-[:CLAIMS {k: 1, text: ['quick', null, 'marmoset']}]->(b), \
         (a)-[:CLAIMS {k: 2, text: 7}]->(b)",
    );
    let rows = run(
        &mut graph,
        "CALL db.edge_text_index.build({type: 'CLAIMS', property: 'text'}) \
         YIELD indexed, skipped, terms RETURN indexed, skipped, terms",
    );
    assert_eq!(
        rows,
        vec![vec![Value::Int64(2), Value::Int64(1), Value::Int64(3)]]
    );
    let rows = scores(&graph, "marmoset");
    assert_eq!(score_of(&rows, 0), Value::Float64(0.0));
    assert!(matches!(score_of(&rows, 1), Value::Float64(s) if s > 0.0));
    assert_eq!(score_of(&rows, 2), Value::Null, "no document scores null");
}

#[test]
fn build_refuses_unknown_type_and_all_absent_property() {
    let mut graph = claims_graph();
    let error = run_err(
        &mut graph,
        "CALL db.edge_text_index.build({type: 'NOPE', property: 'text'}) YIELD indexed RETURN indexed",
    );
    assert!(
        error.contains("Unknown relationship type 'NOPE'"),
        "{error}"
    );
    let error = run_err(
        &mut graph,
        "CALL db.edge_text_index.build({type: 'CLAIMS', property: 'missing'}) YIELD indexed RETURN indexed",
    );
    assert!(
        error.contains("No 'CLAIMS' relationship carries text"),
        "{error}"
    );
    let error = run_err(
        &mut graph,
        "CALL db.edge_text_index.build({type: 'CLAIMS', property: 'text', bogus: 1}) YIELD indexed RETURN indexed",
    );
    assert!(error.contains("Accepted:"), "{error}");
}

#[test]
fn text_bm25_without_an_index_names_the_build_procedure() {
    let mut graph = claims_graph();
    let error = run_err(
        &mut graph,
        "MATCH ()-[r:TAG]->() RETURN text_bm25(r, 'text', 'quick') AS s",
    );
    assert!(error.contains("db.edge_text_index.build"), "{error}");
}

#[test]
fn set_and_remove_are_folded_in_at_query_entry() {
    let mut graph = claims_graph();
    run(
        &mut graph,
        "MATCH ()-[r:CLAIMS {k: 2}]->() SET r.text = 'zebra quick'",
    );
    assert!(store(&graph).edge_is_stale(&graph));
    assert_matches_rebuild(&graph, "zebra");
    assert!(
        !store(&graph).edge_is_stale(&graph),
        "the read refreshed it"
    );

    run(&mut graph, "MATCH ()-[r:CLAIMS {k: 0}]->() REMOVE r.text");
    assert_eq!(score_of(&scores(&graph, "quick"), 0), Value::Null);
    assert_matches_rebuild(&graph, "quick");
}

#[test]
fn a_write_to_another_property_leaves_the_index_current() {
    let mut graph = claims_graph();
    run(
        &mut graph,
        "MATCH ()-[r:CLAIMS {k: 1}]->() SET r.weight = 3",
    );
    assert!(!store(&graph).edge_is_stale(&graph));
}

#[test]
fn delete_prunes_and_a_parallel_member_in_the_reused_slot_is_indexed() {
    let mut graph = claims_graph();
    let before = store(&graph).documents();
    run(&mut graph, "MATCH ()-[r:CLAIMS {k: 1}]->() DELETE r");
    assert_eq!(store(&graph).documents(), before - 1);
    assert!(
        !store(&graph).edge_is_stale(&graph),
        "deletion is not staleness"
    );
    // The next relationship takes the freed slot: below the watermark, so only
    // the creation hook can make it visible.
    run(
        &mut graph,
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) \
         CREATE (a)-[:CLAIMS {k: 9, text: 'zebra zebra'}]->(b)",
    );
    assert!(store(&graph).edge_is_stale(&graph));
    assert!(matches!(score_of(&scores(&graph, "zebra"), 9), Value::Float64(s) if s > 0.0));
    assert_matches_rebuild(&graph, "zebra");
}

#[test]
fn merge_creation_is_indexed() {
    let mut graph = claims_graph();
    run(
        &mut graph,
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) \
         MERGE (a)-[:CLAIMS {k: 5, text: 'merged zebra'}]->(b)",
    );
    assert!(matches!(score_of(&scores(&graph, "zebra"), 5), Value::Float64(s) if s > 0.0));
    assert_matches_rebuild(&graph, "zebra");
}

#[test]
fn a_retired_binding_does_not_score_the_slot_s_new_owner() {
    let mut graph = claims_graph();
    let rows = run(
        &mut graph,
        "MATCH (a:Doc {id: 1})-[r:CLAIMS {k: 0}]->(b:Doc {id: 2}) DELETE r \
         CREATE (a)-[fresh:CLAIMS {k: 7, text: 'the quick brown fox'}]->(b) \
         RETURN text_bm25(r, 'text', 'fox') AS stale, text_bm25(fresh, 'text', 'fox') AS live, \
         id(r) = id(fresh) AS reused",
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0][2],
        Value::Boolean(true),
        "precondition: slot reused"
    );
    assert_eq!(rows[0][0], Value::Null);
    assert!(
        matches!(rows[0][1], Value::Float64(s) if s > 0.0),
        "{rows:?}"
    );
}

#[test]
fn relationship_values_score_like_bindings() {
    let mut graph = claims_graph();
    let binding = run(
        &mut graph,
        "MATCH ()-[r:CLAIMS {k: 1}]->() RETURN text_bm25(r, 'text', 'marmoset') AS s",
    );
    for producer in [
        "MATCH ()-[r:CLAIMS {k: 1}]->() WITH collect(r)[0] AS rel",
        "MATCH ()-[r:CLAIMS {k: 1}]->() WITH collect(r) AS rs UNWIND rs AS rel",
        "CALL { MATCH ()-[r:CLAIMS {k: 1}]->() RETURN collect(r)[0] AS rel } WITH rel",
    ] {
        let rows = run(
            &mut graph,
            &format!("{producer} RETURN text_bm25(rel, 'text', 'marmoset') AS s"),
        );
        assert_eq!(rows, binding, "{producer}");
    }
}

#[test]
fn a_failed_statement_leaves_pre_statement_scores() {
    let mut graph = claims_graph();
    let before = scores(&graph, "zebra");
    run_err(
        &mut graph,
        "MATCH ()-[r:CLAIMS {k: 0}]->() SET r.text = 'zebra stripes' \
         WITH r, text_bm25(r, 'text', 'zebra') AS s RETURN s, 1 / 0 AS boom",
    );
    assert!(
        store(&graph).edge_is_stale(&graph),
        "the refresh the failed statement did must be re-done"
    );
    assert_eq!(scores(&graph, "zebra"), before);
    assert_matches_rebuild(&graph, "zebra");
}

#[test]
fn a_rolled_back_delete_restores_the_document() {
    let mut graph = claims_graph();
    let before = scores(&graph, "marmoset");
    run_err(
        &mut graph,
        "MATCH ()-[r:CLAIMS {k: 1}]->() DELETE r WITH 1 AS x RETURN 1 / 0 AS boom",
    );
    assert!(store(&graph).edge_is_stale(&graph));
    assert_eq!(scores(&graph, "marmoset"), before);
}

#[test]
fn build_and_drop_inside_a_failed_statement_are_undone() {
    let mut graph = claims_graph();
    run_err(
        &mut graph,
        "CALL db.edge_text_index.drop({type: 'CLAIMS', property: 'text'}) YIELD dropped \
         RETURN dropped, 1 / 0 AS boom",
    );
    assert!(edge_text_index_store(&graph, "CLAIMS", "text").is_some());
    run_err(
        &mut graph,
        "CALL db.edge_text_index.build({type: 'TAG', property: 'text'}) YIELD indexed \
         RETURN indexed, 1 / 0 AS boom",
    );
    assert!(edge_text_index_store(&graph, "TAG", "text").is_none());
}

#[test]
fn drop_and_list() {
    let mut graph = claims_graph();
    let rows = run(
        &mut graph,
        "CALL db.edge_text_index.list() YIELD entity, type, property, documents, index_state \
         RETURN entity, type, property, documents, index_state",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::String("relationship".into()),
            Value::String("CLAIMS".into()),
            Value::String("text".into()),
            Value::Int64(3),
            Value::String("online".into()),
        ]]
    );
    let dropped = run(
        &mut graph,
        "CALL db.edge_text_index.drop({type: 'CLAIMS', property: 'text'}) YIELD dropped RETURN dropped",
    );
    assert_eq!(dropped, vec![vec![Value::Boolean(true)]]);
    let again = run(
        &mut graph,
        "CALL db.edge_text_index.drop({type: 'CLAIMS', property: 'text'}) YIELD dropped RETURN dropped",
    );
    assert_eq!(again, vec![vec![Value::Boolean(false)]]);
}

#[test]
fn show_and_drop_index_address_it_as_relationship_type_property() {
    let mut graph = claims_graph();
    let rows = run(
        &mut graph,
        "CALL db.indexes() YIELD name, type, entityType RETURN name, type, entityType",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::String("relationship:CLAIMS.text".into()),
            Value::String("FULLTEXT".into()),
            Value::String("RELATIONSHIP".into()),
        ]]
    );
    run(&mut graph, "DROP INDEX relationship:CLAIMS.text");
    assert!(edge_text_index_store(&graph, "CLAIMS", "text").is_none());
}

#[test]
fn vacuum_drops_the_index() {
    let mut graph = claims_graph();
    run(&mut graph, "MATCH ()-[r:CLAIMS {k: 0}]->() DELETE r");
    graph.vacuum();
    assert!(graph.edge_text_indexes.is_empty());
}

#[test]
fn an_unindexed_graph_does_no_relationship_hook_work() {
    let mut graph = DirGraph::new();
    let before = crate::graph::index_freshness::write_hooks::work_past_gate();
    run(
        &mut graph,
        "CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), (a)-[r:CLAIMS {text: 'x'}]->(b) \
         SET r.text = 'y'",
    );
    assert_eq!(
        crate::graph::index_freshness::write_hooks::work_past_gate(),
        before
    );
}
