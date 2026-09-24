//! Endpoint-addressed relationship writes: row resolution and its refusals,
//! the shared store rules (dimension, metric, provenance, index freshness),
//! and the embed pass against the selection path `db.relationship_embeddings.embed`
//! runs.

use std::cell::Cell;
use std::collections::HashMap;

use super::*;
use crate::graph::edge_embedding_generation::embed_selected_relationships;
use crate::graph::edge_embeddings::carry::relationship_embeddings;
use crate::graph::edge_embeddings::vector_index::{
    build_edge_vector_index, list_edge_vector_indexes, refresh_edge_vector_index,
    EdgeVectorIndexOptions,
};
use crate::graph::embedding_hints::Surface;
use crate::graph::embedding_inventory::{embedding_info, EmbeddingEntity};
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("query failed: {query}: {e}"));
}

/// Docs 1, 2, 3 and an Author 7; `CLAIMS` 1→2 (uid 's') and a parallel group
/// of two `CLAIMS` 1→3 (uids 'p1', 'p2'), every one carrying `text`.
fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Doc {id: 1}), (:Doc {id: 2}), (:Doc {id: 3}), (:Author {id: 7})",
    );
    run(
        &mut graph,
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {uid: 's', text: 'single'}]->(b)",
    );
    for (uid, text) in [("p1", "first"), ("p2", "second")] {
        run(
            &mut graph,
            &format!(
                "MATCH (a:Doc {{id: 1}}), (c:Doc {{id: 3}}) \
                 CREATE (a)-[:CLAIMS {{uid: '{uid}', text: '{text}'}}]->(c)"
            ),
        );
    }
    graph
}

fn keys() -> RelationshipKeys {
    HashMap::from([("CLAIMS".to_string(), "uid".to_string())])
}

fn row(source: i64, target: i64, key: Option<&str>, vector: [f32; 2]) -> RelationshipVector {
    RelationshipVector {
        source_type: Some("Doc".into()),
        source_id: Value::Int64(source),
        target_type: Some("Doc".into()),
        target_id: Value::Int64(target),
        key: key.map(|key| Value::String(key.into())),
        vector: vector.to_vec(),
    }
}

fn untyped(mut row: RelationshipVector) -> RelationshipVector {
    row.source_type = None;
    row.target_type = None;
    row
}

fn all_three() -> Vec<RelationshipVector> {
    vec![
        row(1, 2, Some("s"), [1.0, 0.0]),
        row(1, 3, Some("p1"), [0.0, 1.0]),
        row(1, 3, Some("p2"), [0.6, 0.8]),
    ]
}

/// `(uid, vector)` for every stored vector, sorted by uid.
fn by_uid(graph: &DirGraph) -> Vec<(Value, Vec<f32>)> {
    let mut rows: Vec<_> = relationship_embeddings(graph, "CLAIMS", "text", &keys())
        .unwrap()
        .into_iter()
        .map(|row| (row.key.unwrap(), row.vector))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn uid(value: &str) -> Value {
    Value::String(value.into())
}

#[test]
fn keyed_rows_land_on_the_member_their_key_names() {
    let mut graph = graph();
    let report =
        set_relationship_embeddings(&mut graph, "CLAIMS", "text", all_three(), &keys(), None)
            .unwrap();
    assert_eq!(
        report,
        RelationshipIngestReport {
            stored: 3,
            dimension: 2,
            changed: 3,
            store_created: true,
        }
    );
    assert_eq!(
        by_uid(&graph),
        vec![
            (uid("p1"), vec![0.0, 1.0]),
            (uid("p2"), vec![0.6, 0.8]),
            (uid("s"), vec![1.0, 0.0]),
        ]
    );
}

#[test]
fn rows_read_back_write_into_another_graph_unchanged() {
    let mut source = graph();
    set_relationship_embeddings(&mut source, "CLAIMS", "text", all_three(), &keys(), None).unwrap();
    // The target builds the parallel group in the opposite order, so slot order
    // disagrees with the source's and only the key can pair them.
    let mut target = DirGraph::new();
    run(
        &mut target,
        "CREATE (:Doc {id: 1}), (:Doc {id: 2}), (:Doc {id: 3})",
    );
    for (from, to, uid) in [(1, 3, "p2"), (1, 3, "p1"), (1, 2, "s")] {
        run(
            &mut target,
            &format!(
                "MATCH (a:Doc {{id: {from}}}), (b:Doc {{id: {to}}}) \
                 CREATE (a)-[:CLAIMS {{uid: '{uid}', text: 't'}}]->(b)"
            ),
        );
    }
    let rows = relationship_embeddings(&source, "CLAIMS", "text", &keys()).unwrap();
    set_relationship_embeddings(
        &mut target,
        "CLAIMS",
        "text",
        rows.into_iter().map(RelationshipVector::from),
        &keys(),
        None,
    )
    .unwrap();
    assert_eq!(by_uid(&target), by_uid(&source));
}

/// A store holding every relationship's vector, then an index over it.
fn full_indexed_store() -> DirGraph {
    let mut graph = graph();
    set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        all_three(),
        &keys(),
        Some("euclidean"),
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

#[test]
fn set_replaces_the_store_leaving_only_the_written_rows() {
    let mut graph = full_indexed_store();
    let report = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 3, Some("p2"), [0.5, 0.5])],
        &keys(),
        None,
    )
    .unwrap();
    assert_eq!(
        report,
        RelationshipIngestReport {
            stored: 1,
            dimension: 2,
            changed: 1,
            store_created: true,
        }
    );
    assert_eq!(by_uid(&graph), vec![(uid("p2"), vec![0.5, 0.5])]);
    // As a replaced node store: the old metric and index go with the old store.
    let info = embedding_info(&graph, EmbeddingEntity::Relationship, "CLAIMS", "text").unwrap();
    assert_eq!((info.metric.as_str(), info.model), ("cosine", None));
    assert!(!list_edge_vector_indexes(&graph)[0].built);
}

#[test]
fn add_upserts_keeping_the_rows_it_does_not_name() {
    let mut graph = full_indexed_store();
    let report = add_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 3, Some("p2"), [0.5, 0.5])],
        &keys(),
        None,
    )
    .unwrap();
    assert_eq!(
        report,
        RelationshipIngestReport {
            stored: 3,
            dimension: 2,
            changed: 1,
            store_created: false,
        }
    );
    assert_eq!(
        by_uid(&graph),
        vec![
            (uid("p1"), vec![0.0, 1.0]),
            (uid("p2"), vec![0.5, 0.5]),
            (uid("s"), vec![1.0, 0.0]),
        ]
    );
    let status = &list_edge_vector_indexes(&graph)[0];
    assert_eq!((status.built, status.delta), (true, 1));
}

#[test]
fn a_parallel_group_without_a_key_property_is_refused_by_name() {
    let mut graph = graph();
    let before = graph.version();
    let error = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 3, None, [0.0, 1.0])],
        &RelationshipKeys::new(),
        None,
    )
    .unwrap_err();
    assert!(
        error.contains("rows[0] (Doc id=1)-[:CLAIMS]->(Doc id=3) is ambiguous")
            && error.contains("2 'CLAIMS' relationships connect")
            && error.contains("relationship_keys names no key property for 'CLAIMS'")
            && error.contains("relationship_keys={'CLAIMS': '<property>'}"),
        "{error}"
    );
    assert_eq!(graph.version(), before);
    assert!(graph.edge_embeddings.is_empty());
}

#[test]
fn a_parallel_row_without_a_key_or_with_an_unknown_one_is_refused() {
    let mut graph = graph();
    let missing = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![
            row(1, 2, Some("s"), [1.0, 0.0]),
            row(1, 3, None, [0.0, 1.0]),
        ],
        &keys(),
        None,
    )
    .unwrap_err();
    assert!(
        missing.contains("rows[1]") && missing.contains("the row gives no key value"),
        "{missing}"
    );
    let unknown = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 3, Some("p9"), [0.0, 1.0])],
        &keys(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        unknown,
        "rows[0] (Doc id=1)-[:CLAIMS]->(Doc id=3): 2 'CLAIMS' relationships connect (Doc id=1) \
         to (Doc id=3), and none has uid=\"p9\""
    );
    assert!(
        graph.edge_embeddings.is_empty(),
        "a refused batch writes nothing"
    );
}

#[test]
fn a_key_that_does_not_tell_members_apart_is_refused() {
    let mut graph = graph();
    run(
        &mut graph,
        "MATCH (:Doc {id: 1})-[r:CLAIMS {uid: 'p2'}]->(:Doc {id: 3}) SET r.uid = 'p1'",
    );
    let error = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 3, Some("p1"), [0.0, 1.0])],
        &keys(),
        None,
    )
    .unwrap_err();
    assert!(error.contains("repeats the value"), "{error}");
}

#[test]
fn an_unknown_endpoint_or_an_unconnected_pair_is_refused() {
    let mut graph = graph();
    let unknown = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 2, None, [1.0, 0.0]), row(9, 2, None, [1.0, 0.0])],
        &keys(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        unknown,
        "rows[1] (Doc id=9)-[:CLAIMS]->(Doc id=2): no 'Doc' node has id 9"
    );
    let unconnected = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(2, 3, None, [1.0, 0.0])],
        &keys(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        unconnected,
        "rows[0] (Doc id=2)-[:CLAIMS]->(Doc id=3): no 'CLAIMS' relationship connects those nodes"
    );
    assert!(graph.edge_embeddings.is_empty());
}

#[test]
fn a_singleton_row_with_a_foreign_key_is_refused() {
    let mut graph = graph();
    let error = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 2, Some("p1"), [1.0, 0.0])],
        &keys(),
        None,
    )
    .unwrap_err();
    assert!(error.contains(r#"has uid="s", not "p1""#), "{error}");
    let unnamed = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 2, Some("s"), [1.0, 0.0])],
        &RelationshipKeys::new(),
        None,
    )
    .unwrap_err();
    assert!(
        unnamed.contains("relationship_keys names no key property"),
        "{unnamed}"
    );
}

#[test]
fn two_rows_naming_one_relationship_are_refused() {
    let mut graph = graph();
    let error = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![
            row(1, 2, None, [1.0, 0.0]),
            row(1, 2, Some("s"), [0.0, 1.0]),
        ],
        &keys(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "rows[0] and rows[1] both name relationship (Doc id=1)-[:CLAIMS]->(Doc id=2); give \
         each relationship one row"
    );
}

#[test]
fn endpoint_types_may_be_left_out_only_when_the_type_has_one_of_each() {
    let mut graph = graph();
    set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        all_three().into_iter().map(untyped),
        &keys(),
        None,
    )
    .unwrap();
    assert_eq!(by_uid(&graph).len(), 3);

    run(
        &mut graph,
        "MATCH (a:Author {id: 7}), (b:Doc {id: 2}) CREATE (a)-[:CLAIMS {uid: 'x', text: 'y'}]->(b)",
    );
    let error = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![untyped(row(1, 2, None, [1.0, 0.0]))],
        &keys(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "rows[0] names no source node type, and 'CLAIMS' relationships have source nodes of \
         types Author, Doc; address it by (source_type, source_id, target_type, target_id)"
    );
}

#[test]
fn a_wrong_dimension_names_the_relationship_and_its_row() {
    let mut graph = graph();
    set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 2, None, [1.0, 0.0])],
        &keys(),
        None,
    )
    .unwrap();
    let before = graph.version();
    let mut wide = row(1, 3, Some("p1"), [0.0, 1.0]);
    wide.vector.push(0.5);
    let error = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 3, Some("p2"), [0.0, 1.0]), wide],
        &keys(),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "Embedding for relationship (Doc id=1)-[:CLAIMS]->(Doc id=3) (rows[1]) has dimension 3, \
         expected 2"
    );
    assert_eq!(graph.version(), before);
    assert_eq!(by_uid(&graph).len(), 1);
}

#[test]
fn a_metric_that_contradicts_the_store_is_refused() {
    let mut graph = graph();
    set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 2, None, [1.0, 0.0])],
        &keys(),
        Some("euclidean"),
    )
    .unwrap();
    let error = add_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 3, Some("p1"), [0.0, 1.0])],
        &keys(),
        Some("cosine"),
    )
    .unwrap_err();
    assert_eq!(
        error,
        "Store metric is 'euclidean', but this batch requested 'cosine'"
    );
}

#[test]
fn a_text_property_no_relationship_carries_is_refused() {
    let mut graph = graph();
    let error = set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "txet",
        vec![row(1, 2, None, [1.0, 0.0])],
        &keys(),
        None,
    )
    .unwrap_err();
    assert!(
        error.starts_with("Text column 'txet' not found on any 'CLAIMS' relationship"),
        "{error}"
    );
    let empty =
        set_relationship_embeddings(&mut graph, "CLAIMS", "txet", Vec::new(), &keys(), None)
            .unwrap();
    assert_eq!(empty, RelationshipIngestReport::default());
    assert!(graph.edge_embeddings.is_empty());
}

struct Stub {
    model_id: &'static str,
}

impl Stub {
    fn vector(text: &str) -> Vec<f32> {
        vec![text.len() as f32, 1.0]
    }
}

impl Embedder for Stub {
    fn dimension(&self) -> usize {
        2
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok(texts.iter().map(|text| Stub::vector(text)).collect())
    }

    fn model_id(&self) -> Option<String> {
        Some(self.model_id.into())
    }
}

#[test]
fn a_manual_upsert_keeps_the_metric_and_clears_generated_provenance() {
    let mut graph = graph();
    let model = Stub { model_id: "stub/a" };
    embed_relationship_texts(
        &mut graph,
        "CLAIMS",
        "text",
        EmbedMode::Missing,
        &model,
        &EmbedHooks::default(),
        Some("dot_product"),
    )
    .unwrap();
    let generated =
        embedding_info(&graph, EmbeddingEntity::Relationship, "CLAIMS", "text").unwrap();
    assert_eq!(
        (generated.model.as_deref(), generated.hashed),
        (Some("stub/a"), 3)
    );
    assert_eq!(generated.metric, "dot_product");

    add_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 2, None, [9.0, 9.0])],
        &keys(),
        None,
    )
    .unwrap();
    let manual = embedding_info(&graph, EmbeddingEntity::Relationship, "CLAIMS", "text").unwrap();
    assert_eq!((manual.model, manual.hashed), (None, 2));
    assert_eq!(manual.metric, "dot_product");
}

#[test]
fn a_write_after_an_index_build_is_the_index_delta() {
    let mut graph = graph();
    set_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![row(1, 2, None, [1.0, 0.0])],
        &keys(),
        None,
    )
    .unwrap();
    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions::default(),
    )
    .unwrap();
    add_relationship_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![
            row(1, 3, Some("p1"), [0.0, 1.0]),
            row(1, 3, Some("p2"), [0.6, 0.8]),
        ],
        &keys(),
        None,
    )
    .unwrap();
    let status = &list_edge_vector_indexes(&graph)[0];
    assert_eq!((status.built, status.stale, status.delta), (true, true, 2));
    assert_eq!(
        refresh_edge_vector_index(&graph, "CLAIMS", "text", Surface::Cypher).unwrap(),
        2
    );
    let status = &list_edge_vector_indexes(&graph)[0];
    assert_eq!((status.stale, status.delta), (false, 0));
}

/// The same pass through the procedure's selection path, over every relationship.
fn embed_by_selection(graph: &mut DirGraph, model: &Stub, mode: EmbedMode) {
    let selected = relationship_texts(graph, "CLAIMS", "text");
    let service = EmbeddingExecutionService {
        model,
        interrupt: Interrupt::default(),
    };
    embed_selected_relationships(
        graph,
        EdgeGenerationRequest {
            connection_type: "CLAIMS".into(),
            text_property: "text".into(),
            selected,
            mode,
            batch_size: 256,
            metric: None,
        },
        Some(&service),
    )
    .unwrap();
}

#[test]
fn embedding_every_relationship_equals_the_selection_pass() {
    let model = Stub { model_id: "stub/a" };
    let mut direct = graph();
    let calls = Cell::new(0usize);
    let started = Cell::new(0usize);
    let batched = Cell::new(0usize);
    let embed_batch = |texts: &[String]| {
        calls.set(calls.get() + 1);
        model.embed(texts)
    };
    let on_start = |total: usize| started.set(total);
    let on_batch = |done: usize| batched.set(batched.get() + done);
    let hooks = EmbedHooks {
        batch_size: 2,
        embed_batch: Some(&embed_batch),
        on_start: Some(&on_start),
        on_batch: Some(&on_batch),
        ..EmbedHooks::default()
    };
    let outcome = embed_relationship_texts(
        &mut direct,
        "CLAIMS",
        "text",
        EmbedMode::Missing,
        &model,
        &hooks,
        None,
    )
    .unwrap();
    assert_eq!(
        (outcome.embedded, outcome.skipped, outcome.dimension),
        (3, 0, 2)
    );
    assert_eq!((calls.get(), started.get(), batched.get()), (2, 3, 3));

    let mut selection = graph();
    embed_by_selection(&mut selection, &model, EmbedMode::Missing);
    assert_eq!(by_uid(&direct), by_uid(&selection));
    assert_eq!(
        by_uid(&direct),
        vec![
            (uid("p1"), Stub::vector("first")),
            (uid("p2"), Stub::vector("second")),
            (uid("s"), Stub::vector("single")),
        ]
    );
    for graph in [&direct, &selection] {
        let info = embedding_info(graph, EmbeddingEntity::Relationship, "CLAIMS", "text").unwrap();
        assert_eq!((info.model.as_deref(), info.hashed), (Some("stub/a"), 3));
    }
}

#[test]
fn modes_select_missing_changed_and_all() {
    let model = Stub { model_id: "stub/a" };
    let mut graph = graph();
    let hooks = EmbedHooks::default();
    let embed = |graph: &mut DirGraph, mode| {
        embed_relationship_texts(graph, "CLAIMS", "text", mode, &model, &hooks, None).unwrap()
    };
    assert_eq!(embed(&mut graph, EmbedMode::Missing).embedded, 3);
    let again = embed(&mut graph, EmbedMode::Missing);
    assert_eq!((again.embedded, again.skipped_existing), (0, 3));

    run(
        &mut graph,
        "MATCH ()-[r:CLAIMS {uid: 's'}]->() SET r.text = 'rewritten'",
    );
    let changed = embed(&mut graph, EmbedMode::Changed);
    assert_eq!(
        (
            changed.embedded,
            changed.reembedded_changed,
            changed.skipped_existing
        ),
        (1, 1, 2)
    );
    assert_eq!(by_uid(&graph)[2], (uid("s"), Stub::vector("rewritten")));

    run(
        &mut graph,
        "MATCH ()-[r:CLAIMS {uid: 'p1'}]->() REMOVE r.text",
    );
    let all = embed(&mut graph, EmbedMode::All);
    assert_eq!((all.embedded, all.skipped), (2, 1));
    assert_eq!(
        by_uid(&graph)
            .into_iter()
            .map(|(k, _)| k)
            .collect::<Vec<_>>(),
        vec![uid("p2"), uid("s")],
        "mode all removes the vector of a relationship whose text is gone"
    );
}

#[test]
fn embedding_refuses_by_kind() {
    let mut graph = graph();
    let hooks = EmbedHooks::default();
    let column = embed_relationship_texts(
        &mut graph,
        "CLAIMS",
        "txet",
        EmbedMode::Missing,
        &Stub { model_id: "stub/a" },
        &hooks,
        None,
    )
    .unwrap_err();
    assert!(matches!(column, EmbedError::Column(_)), "{column:?}");
    let unknown_type = embed_relationship_texts(
        &mut graph,
        "CITES",
        "text",
        EmbedMode::Missing,
        &Stub { model_id: "stub/a" },
        &hooks,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(&unknown_type, EmbedError::Column(message) if message.contains("'CITES'")),
        "{unknown_type:?}"
    );

    embed_relationship_texts(
        &mut graph,
        "CLAIMS",
        "text",
        EmbedMode::Missing,
        &Stub { model_id: "stub/a" },
        &hooks,
        None,
    )
    .unwrap();
    run(
        &mut graph,
        "MATCH ()-[r:CLAIMS {uid: 's'}]->() SET r.text = 'rewritten'",
    );
    let foreign = embed_relationship_texts(
        &mut graph,
        "CLAIMS",
        "text",
        EmbedMode::Changed,
        &Stub { model_id: "stub/b" },
        &hooks,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(&foreign, EmbedError::Output(message) if message.contains("model 'stub/a'")),
        "{foreign:?}"
    );

    // Same model id, so the width check is what refuses.
    struct Wide;
    impl Embedder for Wide {
        fn dimension(&self) -> usize {
            3
        }
        fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            Ok(texts.iter().map(|_| vec![1.0; 3]).collect())
        }
        fn model_id(&self) -> Option<String> {
            Some("stub/a".into())
        }
    }
    let dimension = embed_relationship_texts(
        &mut graph,
        "CLAIMS",
        "text",
        EmbedMode::Changed,
        &Wide,
        &hooks,
        None,
    )
    .unwrap_err();
    assert_eq!(dimension, EmbedError::Dimension { store: 2, model: 3 });

    struct Failing;
    impl Embedder for Failing {
        fn dimension(&self) -> usize {
            2
        }
        fn embed(&self, _: &[String]) -> Result<Vec<Vec<f32>>, String> {
            Err("model offline".into())
        }
    }
    let failed = embed_relationship_texts(
        &mut graph,
        "CLAIMS",
        "text",
        EmbedMode::All,
        &Failing,
        &hooks,
        None,
    )
    .unwrap_err();
    assert_eq!(failed, EmbedError::Model("model offline".into()));
}
