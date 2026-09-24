//! `DROP INDEX` against the names `SHOW INDEXES` prints, for both entity kinds.
//!
//! The relationship-vector rows carry a `relationship:` prefix that collides
//! with nothing a node label can spell, and those names have to survive the
//! round trip: printed by one statement, pasted into the next. A name that
//! parses but resolves to the wrong structure is worse than a parse error,
//! because `IF EXISTS` then reports success over an index still installed.

use super::*;
use crate::graph::edge_embeddings::upsert_edge_embeddings;
use crate::graph::edge_embeddings::vector_index::{
    build_edge_vector_index, list_edge_vector_indexes, EdgeVectorIndexOptions,
};
use petgraph::graph::{EdgeIndex, NodeIndex};
use std::collections::HashSet;

/// Three `Doc` nodes, two `CLAIMS` edges between them, and a built HNSW index
/// over the `CLAIMS.text` relationship vector store. `Doc` also carries a
/// `text` property so the node-store collision case has something to embed.
fn edge_indexed_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for id in 0..3 {
        let node = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(id),
                Value::String(format!("n{id}")),
                "Doc".into(),
                HashMap::from([("text".into(), Value::String(format!("body {id}")))]),
                &mut graph.interner,
            ),
        );
        graph.type_indices.entry_or_default("Doc".into()).push(node);
    }
    for target in [1, 2] {
        GraphWrite::add_edge(
            &mut graph.graph,
            NodeIndex::new(0),
            NodeIndex::new(target),
            EdgeData::new("CLAIMS".into(), HashMap::new(), &mut graph.interner),
        );
    }
    graph.upsert_connection_type_metadata("CLAIMS", "Doc", "Doc", HashMap::new());
    upsert_edge_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![
            (EdgeIndex::new(0), vec![1.0, 0.0]),
            (EdgeIndex::new(1), vec![0.8, 0.6]),
        ],
        Some("cosine"),
    )
    .unwrap();
    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions::default(),
    )
    .unwrap();
    graph
}

/// A built node HNSW index over `Doc.text`, whose canonical name is the same
/// `Type.property` pair the relationship store uses.
fn add_node_vector_index(graph: &mut DirGraph) {
    crate::graph::embeddings::set_embeddings(
        graph,
        "Doc",
        "text",
        Some("cosine"),
        vec![
            (Value::Int64(0), vec![1.0f32, 0.0]),
            (Value::Int64(1), vec![0.0f32, 1.0]),
            (Value::Int64(2), vec![0.6f32, 0.8]),
        ],
    )
    .unwrap();
    crate::graph::embeddings::build_vector_index(
        graph, "Doc", "text", None, None, None, None, None,
    )
    .unwrap();
}

fn run(graph: &mut DirGraph, query: &str) -> Result<MutationStats, String> {
    let parsed = parser::parse_cypher(query).map_err(|e| e.to_string())?;
    let result = execute_mutable(
        &mut *graph,
        &parsed,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::default(),
    )?;
    Ok(result.stats.unwrap_or_default())
}

fn run_err(graph: &mut DirGraph, query: &str) -> String {
    run(graph, query).expect_err(&format!("`{query}` unexpectedly succeeded"))
}

fn read(graph: &DirGraph, query: &str) -> CypherResult {
    CypherExecutor::with_params(graph, &HashMap::new(), None)
        .execute(&parser::parse_cypher(query).unwrap())
        .unwrap()
}

/// The `name` column of every `SHOW INDEXES` row, in listing order.
fn index_names(graph: &DirGraph) -> Vec<String> {
    let result = read(graph, "SHOW INDEXES");
    let column = result.columns.iter().position(|c| c == "name").unwrap();
    result
        .rows
        .iter()
        .map(|row| match &row[column] {
            Value::String(name) => name.clone(),
            other => panic!("non-string index name: {other:?}"),
        })
        .collect()
}

/// `index_state` as `db.relationship_embeddings.list` reports it for the one store.
fn edge_index_state(graph: &DirGraph) -> String {
    let result = read(
        graph,
        "CALL db.relationship_embeddings.list({type:'CLAIMS'}) YIELD index_state RETURN index_state",
    );
    assert_eq!(result.rows.len(), 1, "{result:?}");
    match &result.rows[0][0] {
        Value::String(state) => state.clone(),
        other => panic!("non-string index_state: {other:?}"),
    }
}

/// The round trip the whole prefix exists for: the printed name, unquoted,
/// is what `DROP INDEX` takes.
#[test]
fn relationship_index_drops_under_the_name_show_indexes_printed() {
    let mut graph = edge_indexed_graph();
    assert_eq!(index_names(&graph), vec!["relationship:CLAIMS.text"]);
    assert_eq!(edge_index_state(&graph), "online");

    let stats = run(&mut graph, "DROP INDEX relationship:CLAIMS.text").unwrap();
    assert_eq!(stats.indexes_removed, 1);
    assert!(index_names(&graph).is_empty());
    assert_eq!(edge_index_state(&graph), "none");
    // DDL drops the accelerator, never the vectors.
    assert_eq!(graph.edge_embeddings.len(), 1);
}

#[test]
fn backticked_relationship_index_name_drops_the_same_index() {
    let mut graph = edge_indexed_graph();
    let stats = run(&mut graph, "DROP INDEX `relationship:CLAIMS.text`").unwrap();
    assert_eq!(stats.indexes_removed, 1);
    assert_eq!(edge_index_state(&graph), "none");
}

/// The shape that reported success while the index survived: backticks made it
/// past the parser, `IF EXISTS` swallowed the zero-drop the node-only routing
/// produced, and the statement returned green over an installed index.
#[test]
fn backticked_name_with_if_exists_does_not_report_a_drop_it_did_not_make() {
    let mut graph = edge_indexed_graph();
    let stats = run(
        &mut graph,
        "DROP INDEX `relationship:CLAIMS.text` IF EXISTS",
    )
    .unwrap();
    assert_eq!(stats.indexes_removed, 1);
    assert_eq!(edge_index_state(&graph), "none");
    assert!(index_names(&graph).is_empty());
}

/// `IF EXISTS` may only be a no-op when nothing carries the name. A name that
/// *did* resolve and then dropped nothing is a routing bug, and reporting
/// success for it leaves the index installed behind a green statement.
#[test]
fn if_exists_is_a_no_op_only_when_the_name_is_absent() {
    let mut graph = edge_indexed_graph();

    let stats = run(
        &mut graph,
        "DROP INDEX relationship:CLAIMS.missing IF EXISTS",
    )
    .unwrap();
    assert_eq!(stats.indexes_removed, 0);
    assert_eq!(edge_index_state(&graph), "online");

    let stats = run(&mut graph, "DROP INDEX relationship:CLAIMS.text IF EXISTS").unwrap();
    assert_eq!(stats.indexes_removed, 1);
    assert_eq!(edge_index_state(&graph), "none");

    // And a second IF EXISTS drop over the now-absent name stays a no-op.
    let stats = run(&mut graph, "DROP INDEX relationship:CLAIMS.text IF EXISTS").unwrap();
    assert_eq!(stats.indexes_removed, 0);
}

#[test]
fn dropping_an_unknown_relationship_name_errors_without_if_exists() {
    let mut graph = edge_indexed_graph();
    let err = run_err(&mut graph, "DROP INDEX relationship:CLAIMS.missing");
    assert!(err.contains("relationship:CLAIMS.missing"), "got: {err}");
    assert!(err.contains("relationship:CLAIMS.text"), "got: {err}");
    assert_eq!(edge_index_state(&graph), "online");
}

/// The node control: the unprefixed name still reaches the node vector index
/// and nothing else.
#[test]
fn node_vector_index_still_drops_under_its_unprefixed_name() {
    let mut graph = edge_indexed_graph();
    add_node_vector_index(&mut graph);

    let stats = run(&mut graph, "DROP INDEX Doc.text").unwrap();
    assert_eq!(stats.indexes_removed, 1);
    assert!(!crate::graph::embeddings::has_vector_index(
        &graph, "Doc", "text"
    ));
    assert_eq!(index_names(&graph), vec!["relationship:CLAIMS.text"]);
}

/// The collision the prefix exists to prevent: one `Doc.text` node index and
/// one `CLAIMS.text` relationship index, each addressed by its own name.
#[test]
fn node_and_relationship_indexes_sharing_a_property_drop_independently() {
    let mut graph = edge_indexed_graph();
    add_node_vector_index(&mut graph);
    assert_eq!(
        index_names(&graph),
        vec!["Doc.text", "relationship:CLAIMS.text"]
    );

    run(&mut graph, "DROP INDEX relationship:CLAIMS.text").unwrap();
    assert_eq!(index_names(&graph), vec!["Doc.text"]);
    assert!(crate::graph::embeddings::has_vector_index(
        &graph, "Doc", "text"
    ));

    run(&mut graph, "DROP INDEX Doc.text").unwrap();
    assert!(index_names(&graph).is_empty());
}

/// Schema is graph state, so a failed statement takes the drop back with it.
#[test]
fn a_rolled_back_statement_restores_the_relationship_index() {
    let mut graph = edge_indexed_graph();
    let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
    run(&mut graph, "DROP INDEX relationship:CLAIMS.text").unwrap();
    assert!(!list_edge_vector_indexes(&graph)[0].built);
    checkpoint.rollback(&mut graph);

    assert!(list_edge_vector_indexes(&graph)[0].built);
    assert_eq!(index_names(&graph), vec!["relationship:CLAIMS.text"]);
    assert_eq!(edge_index_state(&graph), "online");
}

/// A write scope judges a relationship write by its endpoints, not by the
/// relationship type, so the relationship arm may not be handed to the node
/// whitelist — `CLAIMS` is not a node type and never will be in the set.
#[test]
fn write_scope_judges_a_relationship_index_by_its_endpoint_types() {
    let mut graph = edge_indexed_graph();
    graph.active_write_scope = Some(HashSet::from(["Unrelated".to_string()]));
    let err = run_err(&mut graph, "DROP INDEX relationship:CLAIMS.text");
    assert!(err.contains("write scope violation"), "got: {err}");
    assert!(err.contains("CLAIMS"), "got: {err}");
    assert_eq!(edge_index_state(&graph), "online");

    graph.active_write_scope = Some(HashSet::from(["Doc".to_string()]));
    let stats = run(&mut graph, "DROP INDEX relationship:CLAIMS.text").unwrap();
    assert_eq!(stats.indexes_removed, 1);
}
