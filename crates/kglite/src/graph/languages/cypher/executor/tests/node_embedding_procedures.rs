//! `db.node_embeddings.*`, `db.node_text_index.*` and the `db.embeddings.*` /
//! `db.text_index.*` routers: each node procedure against the node writer it
//! stands in for, the refusals it shares with the relationship lane, statement
//! rollback of its writes, and the router's dispatch.

use super::*;
use crate::graph::embeddings::{self, store_key};

/// Through the session entry, which opens the statement checkpoint a
/// rollback needs; `execute_mutable` alone runs without one.
fn mutate(graph: &mut DirGraph, query: &str) -> Result<CypherResult, String> {
    let params = HashMap::new();
    crate::graph::session::execute::execute_mut(
        graph,
        query,
        &crate::graph::session::execute::ExecuteOptions::eager(&params),
    )
    .map(|outcome| outcome.result)
    .map_err(|error| error.to_string())
}

fn mutate_with(
    graph: &mut DirGraph,
    query: &str,
    model: std::sync::Arc<dyn crate::graph::embedder::Embedder>,
) -> Result<CypherResult, String> {
    let params = HashMap::new();
    let mut options = crate::graph::session::execute::ExecuteOptions::eager(&params);
    options.embedder = Some(model);
    crate::graph::session::execute::execute_mut(graph, query, &options)
        .map(|outcome| outcome.result)
        .map_err(|error| error.to_string())
}

fn read(graph: &DirGraph, query: &str) -> Result<CypherResult, String> {
    let parsed = parser::parse_cypher(query).map_err(|error| error.to_string())?;
    CypherExecutor::with_params(graph, &HashMap::new(), None).execute(&parsed)
}

fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    mutate(
        &mut graph,
        "CREATE (:Doc {id: 1, text: 'alpha'}), (:Doc {id: 2, text: 'beta'}), \
         (:Doc {id: 3, text: 'gamma'}), (:Note {id: 10, text: 'delta'}), \
         (:Note {id: 11, text: 'epsilon'})",
    )
    .unwrap();
    mutate(
        &mut graph,
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CITES {text: 'x'}]->(b)",
    )
    .unwrap();
    graph
}

fn vectors(graph: &DirGraph, node_type: &str) -> Vec<(i64, Vec<f32>)> {
    let store = graph.embeddings.get(&store_key(node_type, "text")).unwrap();
    let mut out: Vec<_> = store
        .slot_to_node
        .iter()
        .map(|&slot| {
            let id = match graph
                .graph
                .node_view(petgraph::graph::NodeIndex::new(slot))
                .unwrap()
                .id()
                .into_owned()
            {
                Value::Int64(id) => id,
                other => panic!("{other:?}"),
            };
            (id, store.get_embedding(slot).unwrap().to_vec())
        })
        .collect();
    out.sort_by_key(|(id, _)| *id);
    out
}

const SET_DOCS: &str = "MATCH (d:Doc) WITH collect(d) AS ds \
    CALL db.node_embeddings.set({type: 'Doc', text_property: 'text', entries: \
    [{node: ds[0], vector: [1.0, 0.0]}, {node: ds[1], vector: [0.0, 1.0]}, \
    {node: ds[2], vector: [0.6, 0.8]}]}) YIELD stored, dimension RETURN stored, dimension";

#[test]
fn set_writes_what_add_embeddings_writes() {
    let mut by_procedure = graph();
    let result = mutate(&mut by_procedure, SET_DOCS).unwrap();
    assert_eq!(result.rows, vec![vec![Value::Int64(3), Value::Int64(2)]]);

    let mut by_writer = graph();
    let ids: Vec<Value> = {
        let store_order = read(&by_writer, "MATCH (d:Doc) RETURN d.id").unwrap();
        store_order
            .rows
            .into_iter()
            .map(|row| row[0].clone())
            .collect()
    };
    let rows = [vec![1.0f32, 0.0], vec![0.0, 1.0], vec![0.6, 0.8]];
    embeddings::add_embeddings(
        &mut by_writer,
        "Doc",
        "text",
        None,
        ids.into_iter().zip(rows),
    )
    .unwrap();
    assert_eq!(vectors(&by_procedure, "Doc"), vectors(&by_writer, "Doc"));
}

#[test]
fn set_refuses_what_the_relationship_twin_refuses() {
    let mut graph = graph();
    let unknown = mutate(
        &mut graph,
        "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: \
         'text', entries: [{node: d, vector: [1.0]}], metrc: 'cosine'}) YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(
        unknown.contains("metrc") && unknown.contains("Accepted"),
        "{unknown}"
    );

    let missing_column = mutate(
        &mut graph,
        "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: \
         'summary', entries: [{node: d, vector: [1.0]}]}) YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(
        missing_column.contains("Source column 'summary' not found on any 'Doc' node"),
        "{missing_column}"
    );

    let wrong_type = mutate(
        &mut graph,
        "MATCH (n:Note {id: 10}) CALL db.node_embeddings.set({type: 'Doc', text_property: \
         'text', entries: [{node: n, vector: [1.0]}]}) YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(
        wrong_type.contains("entries[0] has type 'Note', expected 'Doc'"),
        "{wrong_type}"
    );

    let repeated = mutate(
        &mut graph,
        "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: \
         'text', entries: [{node: d, vector: [1.0]}, {node: d, vector: [2.0]}]}) \
         YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(
        repeated.contains("appears more than once (entries[0] and entries[1])"),
        "{repeated}"
    );

    mutate(&mut graph, SET_DOCS).unwrap();
    let metric = mutate(
        &mut graph,
        "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: \
         'text', entries: [{node: d, vector: [1.0, 0.0]}], metric: 'euclidean'}) \
         YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(
        metric.contains("Store metric is 'cosine', but this batch requested 'euclidean'"),
        "{metric}"
    );
}

#[test]
fn a_failing_later_clause_rolls_the_node_store_back() {
    let mut graph = graph();
    mutate(&mut graph, SET_DOCS).unwrap();
    let before = vectors(&graph, "Doc");
    let error = mutate(
        &mut graph,
        "MATCH (d:Doc {id: 1}) CALL db.node_embeddings.set({type: 'Doc', text_property: \
         'text', entries: [{node: d, vector: [9.0, 9.0]}]}) YIELD stored \
         WITH d CALL db.node_embeddings.set({type: 'Doc', text_property: 'text', \
         entries: [{node: d, vector: [1.0, 2.0, 3.0]}]}) YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(error.contains("dimension"), "{error}");
    assert_eq!(vectors(&graph, "Doc"), before);

    let error = mutate(
        &mut graph,
        "CALL db.node_embeddings.drop({type: 'Doc', text_property: 'text'}) YIELD dropped \
         WITH dropped CALL db.node_embeddings.set({type: 'Doc', text_property: 'text', \
         entries: 'nope'}) YIELD stored RETURN stored",
    )
    .unwrap_err();
    assert!(error.contains("'entries' must be a list"), "{error}");
    assert_eq!(vectors(&graph, "Doc"), before);
}

struct Stub;

impl crate::graph::embedder::Embedder for Stub {
    fn dimension(&self) -> usize {
        2
    }
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok(texts
            .iter()
            .map(|text| vec![text.len() as f32, 1.0])
            .collect())
    }
    fn model_id(&self) -> Option<String> {
        Some("stub".into())
    }
}

#[test]
fn embed_generates_for_the_selection_and_refuses_without_a_model() {
    let mut graph = graph();
    let result = mutate_with(
        &mut graph,
        "MATCH (d:Doc) WHERE d.id < 3 WITH collect(d) AS ds \
         CALL db.node_embeddings.embed({type: 'Doc', text_property: 'text', nodes: ds}) \
         YIELD embedded, skipped, dimension, model RETURN embedded, skipped, dimension, model",
        std::sync::Arc::new(Stub),
    )
    .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![
            Value::Int64(2),
            Value::Int64(0),
            Value::Int64(2),
            Value::String("stub".into())
        ]]
    );
    assert_eq!(
        vectors(&graph, "Doc"),
        vec![(1, vec![5.0, 1.0]), (2, vec![4.0, 1.0])]
    );

    let missing = mutate(
        &mut graph,
        "MATCH (d:Doc {id: 3}) CALL db.node_embeddings.embed({type: 'Doc', text_property: \
         'text', nodes: [d]}) YIELD embedded RETURN embedded",
    )
    .unwrap_err();
    assert!(
        missing.contains("requires a registered embedder"),
        "{missing}"
    );

    let wrong_type = mutate_with(
        &mut graph,
        "MATCH (n:Note) WITH collect(n) AS ns CALL db.node_embeddings.embed({type: 'Doc', \
         text_property: 'text', nodes: ns}) YIELD embedded RETURN embedded",
        std::sync::Arc::new(Stub),
    )
    .unwrap_err();
    assert!(
        wrong_type.contains("nodes[0] has type 'Note', expected 'Doc'"),
        "{wrong_type}"
    );

    let both = mutate_with(
        &mut graph,
        "MATCH (n) WHERE n:Doc OR n:Note WITH collect(n) AS ns \
         CALL db.node_embeddings.embed({types: ['Doc', 'Note'], text_property: 'text', \
         nodes: ns}) YIELD embedded RETURN embedded",
        std::sync::Arc::new(Stub),
    )
    .unwrap();
    // Two Doc vectors already exist (mode missing): one Doc and two Notes.
    assert_eq!(both.rows, vec![vec![Value::Int64(3)]]);
}

#[test]
fn the_index_lifecycle_and_list_mirror_the_node_writer() {
    let mut graph = graph();
    mutate(&mut graph, SET_DOCS).unwrap();
    let listed = read(
        &graph,
        "CALL db.node_embeddings.list({type: 'Doc'}) YIELD entity, type, text_property, store, \
         dimension, count, metric, index_state, delta, unembedded \
         RETURN entity, type, text_property, store, dimension, count, metric, index_state, \
         delta, unembedded",
    )
    .unwrap();
    assert_eq!(
        listed.rows,
        vec![vec![
            Value::String("node".into()),
            Value::String("Doc".into()),
            Value::String("text".into()),
            Value::String("text_emb".into()),
            Value::Int64(2),
            Value::Int64(3),
            Value::String("cosine".into()),
            Value::String("none".into()),
            Value::Int64(3),
            Value::Int64(0),
        ]]
    );
    let refused = mutate(
        &mut graph,
        "CALL db.node_embeddings.refresh_index({type: 'Doc', text_property: 'text'}) \
         YIELD refreshed RETURN refreshed",
    )
    .unwrap_err();
    assert!(refused.contains("no vector index"), "{refused}");

    let built = mutate(
        &mut graph,
        "CALL db.node_embeddings.build_index({type: 'Doc', text_property: 'text', m: 8}) \
         YIELD indexed, metric, m RETURN indexed, metric, m",
    )
    .unwrap();
    assert_eq!(
        built.rows,
        vec![vec![
            Value::Int64(3),
            Value::String("cosine".into()),
            Value::Int64(8)
        ]]
    );
    assert!(embeddings::has_vector_index(&graph, "Doc", "text"));
    mutate(
        &mut graph,
        "CALL db.node_embeddings.refresh_index({type: 'Doc', text_property: 'text'}) \
         YIELD refreshed RETURN refreshed",
    )
    .unwrap();
    let dropped = mutate(
        &mut graph,
        "CALL db.node_embeddings.drop_index({type: 'Doc', text_property: 'text'}) \
         YIELD dropped RETURN dropped",
    )
    .unwrap();
    assert_eq!(dropped.rows, vec![vec![Value::Boolean(true)]]);
    assert!(!embeddings::has_vector_index(&graph, "Doc", "text"));

    let removed = mutate(
        &mut graph,
        "MATCH (d:Doc {id: 2}) CALL db.node_embeddings.remove({type: 'Doc', text_property: \
         'text', nodes: [d]}) YIELD removed RETURN removed",
    )
    .unwrap();
    assert_eq!(removed.rows, vec![vec![Value::Int64(1)]]);
    assert_eq!(vectors(&graph, "Doc").len(), 2);

    let dropped = mutate(
        &mut graph,
        "CALL db.node_embeddings.drop({type: 'Doc', text_property: 'text'}) YIELD dropped \
         RETURN dropped",
    )
    .unwrap();
    assert_eq!(dropped.rows, vec![vec![Value::Boolean(true)]]);
    assert!(!embeddings::store_exists(&graph, "Doc", "text"));
}

#[test]
fn query_ranks_one_or_several_stores_and_refuses_mixed_metrics() {
    let mut graph = graph();
    mutate(&mut graph, SET_DOCS).unwrap();
    let single = read(
        &graph,
        "CALL db.node_embeddings.query({type: 'Doc', text_property: 'text', \
         vector: [1.0, 0.0], top_k: 2}) YIELD node, score, search_method, type \
         RETURN node.id, score, search_method, type",
    )
    .unwrap();
    assert_eq!(single.rows.len(), 2, "{single:?}");
    assert_eq!(single.rows[0][0], Value::Int64(1));
    assert_eq!(single.rows[0][2], Value::String("exact".into()));
    assert_eq!(single.rows[1][0], Value::Int64(3));

    mutate(
        &mut graph,
        "MATCH (n:Note) WITH collect(n) AS ns CALL db.node_embeddings.set({type: 'Note', \
         text_property: 'text', entries: [{node: ns[0], vector: [0.9, 0.1]}, \
         {node: ns[1], vector: [0.0, 1.0]}]}) YIELD stored RETURN stored",
    )
    .unwrap();
    let merged = read(
        &graph,
        "CALL db.node_embeddings.query({text_property: 'text', vector: [1.0, 0.0], top_k: 3}) \
         YIELD node, type RETURN node.id, type",
    )
    .unwrap();
    assert_eq!(
        merged.rows,
        vec![
            vec![Value::Int64(1), Value::String("Doc".into())],
            vec![Value::Int64(10), Value::String("Note".into())],
            vec![Value::Int64(3), Value::String("Doc".into())],
        ]
    );

    let mut mixed = graph;
    mutate(
        &mut mixed,
        "MATCH (n:Note) WITH collect(n) AS ns CALL db.node_embeddings.drop({type: 'Note', \
         text_property: 'text'}) YIELD dropped WITH ns \
         CALL db.node_embeddings.set({type: 'Note', text_property: 'text', \
         entries: [{node: ns[0], vector: [0.9, 0.1]}], metric: 'euclidean'}) \
         YIELD stored RETURN stored",
    )
    .unwrap();
    let refused = read(
        &mixed,
        "CALL db.node_embeddings.query({types: ['Doc', 'Note'], text_property: 'text', \
         vector: [1.0, 0.0]}) YIELD node RETURN node",
    )
    .unwrap_err();
    assert!(
        refused.contains("Node embedding stores 'Doc.text' (metric 'cosine')"),
        "{refused}"
    );
    let missing = read(
        &mixed,
        "CALL db.node_embeddings.query({type: 'Nope', text_property: 'text', \
         vector: [1.0, 0.0]}) YIELD node RETURN node",
    )
    .unwrap_err();
    assert!(
        missing.contains("No node embedding store 'Nope.text'"),
        "{missing}"
    );
}

#[test]
fn node_text_index_lifecycle_and_rollback() {
    let mut graph = graph();
    let built = mutate(
        &mut graph,
        "CALL db.node_text_index.build({type: 'Doc', property: 'text'}) \
         YIELD indexed, skipped RETURN indexed, skipped",
    )
    .unwrap();
    assert_eq!(built.rows, vec![vec![Value::Int64(3), Value::Int64(0)]]);
    let listed = read(
        &graph,
        "CALL db.node_text_index.list() YIELD entity, type, property, documents \
         RETURN entity, type, property, documents",
    )
    .unwrap();
    assert_eq!(
        listed.rows,
        vec![vec![
            Value::String("node".into()),
            Value::String("Doc".into()),
            Value::String("text".into()),
            Value::Int64(3)
        ]]
    );
    let refused = mutate(
        &mut graph,
        "CALL db.node_text_index.refresh({type: 'Note', property: 'text'}) \
         YIELD refreshed RETURN refreshed",
    )
    .unwrap_err();
    assert!(
        refused.contains("no node text index on 'Note.text'"),
        "{refused}"
    );

    let error = mutate(
        &mut graph,
        "CALL db.node_text_index.drop({type: 'Doc', property: 'text'}) YIELD dropped \
         WITH dropped CALL db.node_text_index.build({type: 'Nope', property: 'text'}) \
         YIELD indexed RETURN indexed",
    )
    .unwrap_err();
    assert!(error.contains("Unknown node type 'Nope'"), "{error}");
    assert!(crate::graph::text_indexes::has_text_index(
        &graph, "Doc", "text"
    ));
}

#[test]
fn the_router_defaults_to_nodes_and_routes_relationships() {
    let mut graph = graph();
    let routed = mutate(
        &mut graph,
        &SET_DOCS.replace("db.node_embeddings.set", "db.embeddings.set"),
    )
    .unwrap();
    assert_eq!(routed.rows, vec![vec![Value::Int64(3), Value::Int64(2)]]);
    assert_eq!(vectors(&graph, "Doc").len(), 3);

    mutate(
        &mut graph,
        "MATCH ()-[r:CITES]->() CALL db.embeddings.set({entity: 'relationship', type: 'CITES', \
         text_property: 'text', entries: [{relationship: r, vector: [1.0, 0.0]}]}) \
         YIELD stored RETURN stored",
    )
    .unwrap();
    assert_eq!(graph.edge_embeddings.len(), 1);
    assert_eq!(graph.embeddings.len(), 1);

    let node_rows = read(
        &graph,
        "CALL db.embeddings.query({type: 'Doc', text_property: 'text', vector: [1.0, 0.0]}) \
         YIELD node, score RETURN node.id, score",
    )
    .unwrap();
    let specific = read(
        &graph,
        "CALL db.node_embeddings.query({type: 'Doc', text_property: 'text', \
         vector: [1.0, 0.0]}) YIELD node, score RETURN node.id, score",
    )
    .unwrap();
    assert_eq!(node_rows.rows, specific.rows);
    let relationship_rows = read(
        &graph,
        "CALL db.embeddings.query({entity: 'relationship', type: 'CITES', \
         text_property: 'text', vector: [1.0, 0.0]}) YIELD relationship, score \
         RETURN type(relationship), score",
    )
    .unwrap();
    assert_eq!(relationship_rows.rows.len(), 1, "{relationship_rows:?}");
    assert_eq!(relationship_rows.rows[0][0], Value::String("CITES".into()));

    let belongs = mutate(
        &mut graph,
        "MATCH ()-[r:CITES]->() CALL db.embeddings.remove({type: 'CITES', \
         text_property: 'text', relationships: [r]}) YIELD removed RETURN removed",
    )
    .unwrap_err();
    assert!(
        belongs.contains("`relationships` belongs to entity:'relationship'"),
        "{belongs}"
    );
    let not_literal = read(
        &graph,
        "CALL db.embeddings.list({entity: $entity}) YIELD type RETURN type",
    )
    .unwrap_err();
    assert!(
        not_literal.contains("'entity' must be the string literal"),
        "{not_literal}"
    );
    let text = mutate(
        &mut graph,
        "CALL db.text_index.build({entity: 'relationship', type: 'CITES', property: 'text'}) \
         YIELD indexed RETURN indexed",
    )
    .unwrap();
    assert_eq!(text.rows, vec![vec![Value::Int64(1)]]);
    assert_eq!(graph.edge_text_indexes.len(), 1);
    assert!(graph.text_indexes.is_empty());
}

#[test]
fn the_unreleased_edge_names_are_unknown_procedures() {
    let graph = graph();
    for name in ["db.edge_embeddings.list", "db.edge_text_index.list"] {
        let error = read(&graph, &format!("CALL {name}() YIELD type RETURN type")).unwrap_err();
        assert!(error.contains("Unknown procedure"), "{name}: {error}");
    }
}
