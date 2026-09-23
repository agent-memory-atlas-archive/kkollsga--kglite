use super::vector_index::*;
use super::*;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::GraphWrite;
use std::collections::HashMap;

fn fixture() -> (DirGraph, EdgeIndex, EdgeIndex, EdgeIndex) {
    let mut graph = DirGraph::new();
    let source = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(1),
            Value::String("source".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let mut edges = Vec::new();
    for ordinal in 0..3 {
        let target = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(ordinal + 2),
                Value::String(format!("target-{ordinal}")),
                "Doc".into(),
                HashMap::new(),
                &mut graph.interner,
            ),
        );
        edges.push(GraphWrite::add_edge(
            &mut graph.graph,
            source,
            target,
            EdgeData::new("CLAIMS".into(), HashMap::new(), &mut graph.interner),
        ));
    }
    upsert_edge_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![
            (edges[0], vec![1.0, 0.0]),
            (edges[1], vec![0.0, 1.0]),
            (edges[2], vec![-1.0, 0.0]),
        ],
        Some("cosine"),
    )
    .unwrap();
    (graph, edges[0], edges[1], edges[2])
}

#[test]
fn explicit_index_query_matches_exact_and_status_uses_text_property() {
    let (mut graph, first, _, _) = fixture();
    let exact = query_edge_embeddings(
        &graph,
        "CLAIMS",
        "text",
        &[1.0, 0.0],
        EdgeVectorQueryOptions {
            top_k: 2,
            exact: true,
            metric: None,
        },
    )
    .unwrap();
    assert_eq!(exact.hits[0].edge, first);
    assert_eq!(exact.search_method, "exact");

    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions::default(),
    )
    .unwrap();
    let approximate = query_edge_embeddings(
        &graph,
        "CLAIMS",
        "text",
        &[1.0, 0.0],
        EdgeVectorQueryOptions {
            top_k: 2,
            exact: false,
            metric: None,
        },
    )
    .unwrap();
    assert_eq!(approximate.hits, exact.hits);
    assert_eq!(approximate.search_method, "hnsw");
    assert_eq!(
        list_edge_vector_indexes(&graph),
        vec![EdgeVectorIndexStatus {
            connection_type: "CLAIMS".into(),
            text_property: "text".into(),
            built: true,
            stale: false,
            delta: 0,
            unembedded: 0,
        }]
    );
}

#[test]
fn unsupported_build_is_atomic_and_metric_mismatch_falls_back() {
    let (mut graph, _, _, _) = fixture();
    let before = graph.version();
    let error = build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions {
            metric: Some("poincare".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("not supported by HNSW"));
    assert_eq!(graph.version(), before);
    assert!(!list_edge_vector_indexes(&graph)[0].built);

    let error = build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions {
            m: Some(1),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("at least 2"));
    assert_eq!(graph.version(), before);
    assert!(!list_edge_vector_indexes(&graph)[0].built);
}

#[test]
fn build_and_drop_roll_back_without_rebuilding_vectors() {
    let (mut graph, _, _, _) = fixture();
    let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions::default(),
    )
    .unwrap();
    assert!(list_edge_vector_indexes(&graph)[0].built);
    checkpoint.rollback(&mut graph);
    assert!(!list_edge_vector_indexes(&graph)[0].built);

    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions::default(),
    )
    .unwrap();
    let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
    assert!(drop_edge_vector_index(&mut graph, "CLAIMS", "text").unwrap());
    assert!(!list_edge_vector_indexes(&graph)[0].built);
    checkpoint.rollback(&mut graph);
    assert!(list_edge_vector_indexes(&graph)[0].built);
}

#[test]
fn vector_mutation_marks_index_stale_and_exact_fallback_remains_correct() {
    let (mut graph, first, second, _) = fixture();
    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions {
            auto_refresh_limit: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    upsert_edge_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![(second, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let status = &list_edge_vector_indexes(&graph)[0];
    assert!(status.built);
    assert!(status.stale);
    assert_eq!(status.delta, 1);
    let result = query_edge_embeddings(
        &graph,
        "CLAIMS",
        "text",
        &[1.0, 0.0],
        EdgeVectorQueryOptions {
            top_k: 2,
            exact: false,
            metric: None,
        },
    )
    .unwrap();
    assert_eq!(result.search_method, "exact");
    assert_eq!(
        result.hits.iter().map(|hit| hit.edge).collect::<Vec<_>>(),
        vec![first, second]
    );
}

#[test]
fn wal_declaration_rebuilds_and_drops_after_vectors_are_present() {
    let (mut recovered, _, _, _) = fixture();
    crate::graph::mutation::wal_replay::apply_frames(
        &mut recovered,
        &[crate::graph::wal::WalFrame {
            lsn: 1,
            ops: vec![crate::graph::wal::MutationOp::SetEdgeVectorIndex {
                conn_type: "CLAIMS".into(),
                text_column: "text".into(),
                metric: Some("cosine".into()),
                m: Some(8),
                ef_construction: Some(32),
                ef_search: Some(16),
                auto_refresh_limit: Some(4),
                present: true,
            }],
        }],
        0,
    )
    .unwrap();
    assert!(list_edge_vector_indexes(&recovered)[0].built);

    crate::graph::mutation::wal_replay::apply_frames(
        &mut recovered,
        &[crate::graph::wal::WalFrame {
            lsn: 2,
            ops: vec![crate::graph::wal::MutationOp::SetEdgeVectorIndex {
                conn_type: "CLAIMS".into(),
                text_column: "text".into(),
                metric: None,
                m: None,
                ef_construction: None,
                ef_search: None,
                auto_refresh_limit: None,
                present: false,
            }],
        }],
        1,
    )
    .unwrap();
    assert!(!list_edge_vector_indexes(&recovered)[0].built);
}

#[test]
fn malformed_wal_index_options_refuse_without_installing_an_index() {
    let (mut recovered, _, _, _) = fixture();
    let result = crate::graph::mutation::wal_replay::apply_frames(
        &mut recovered,
        &[crate::graph::wal::WalFrame {
            lsn: 1,
            ops: vec![crate::graph::wal::MutationOp::SetEdgeVectorIndex {
                conn_type: "CLAIMS".into(),
                text_column: "text".into(),
                metric: Some("cosine".into()),
                m: Some(8),
                ef_construction: Some(0),
                ef_search: Some(16),
                auto_refresh_limit: Some(4),
                present: true,
            }],
        }],
        0,
    );
    assert!(result.unwrap_err().contains("greater than 0"));
    assert!(!list_edge_vector_indexes(&recovered)[0].built);
}

/// Same shape as [`fixture`], but the store is created without a declared
/// metric — the state `db.edge_embeddings.set` leaves behind when the caller
/// names no metric, and the one an explicit build metric may claim.
fn fixture_without_declared_metric() -> (DirGraph, EdgeIndex) {
    let mut graph = DirGraph::new();
    let source = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(1),
            Value::String("source".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let mut edges = Vec::new();
    for ordinal in 0..3 {
        let target = GraphWrite::add_node(
            &mut graph.graph,
            NodeData::new(
                Value::Int64(ordinal + 2),
                Value::String(format!("target-{ordinal}")),
                "Doc".into(),
                HashMap::new(),
                &mut graph.interner,
            ),
        );
        edges.push(GraphWrite::add_edge(
            &mut graph.graph,
            source,
            target,
            EdgeData::new("CLAIMS".into(), HashMap::new(), &mut graph.interner),
        ));
    }
    upsert_edge_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![
            (edges[0], vec![3.0, 0.0]),
            (edges[1], vec![0.0, 1.0]),
            (edges[2], vec![-1.0, 0.0]),
        ],
        None,
    )
    .unwrap();
    (graph, edges[0])
}

/// A build metric the store does not contradict becomes the store's metric.
///
/// Before the fix the store kept its `None` (which resolves to cosine) beside a
/// euclidean index, so every metric-less query resolved cosine, failed the
/// index's metric check and fell back to the exact scan for good — while
/// `list` reported a metric nothing used.
#[test]
fn explicit_build_metric_becomes_the_store_metric_and_serves_default_queries() {
    let (mut graph, _) = fixture_without_declared_metric();
    let report = build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions {
            metric: Some("euclidean".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.metric, "euclidean");

    let store = graph
        .edge_embeddings
        .get(&("CLAIMS".to_string(), "text_emb".to_string()))
        .unwrap();
    assert_eq!(store.metric(), Some("euclidean"));

    let served = query_edge_embeddings(
        &graph,
        "CLAIMS",
        "text",
        &[3.0, 0.0],
        EdgeVectorQueryOptions {
            top_k: 2,
            exact: false,
            metric: None,
        },
    )
    .unwrap();
    assert_eq!(served.search_method, "hnsw");
}

/// The store's declared metric wins over a contradicting build argument, and
/// says so: the vectors were written under it, and an index answering under
/// another one is the defect above wearing an explicit store metric.
#[test]
fn build_metric_contradicting_the_store_is_refused_and_changes_nothing() {
    let (mut graph, _, _, _) = fixture();
    let before = graph.version();
    let error = build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions {
            metric: Some("euclidean".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("declares metric 'cosine'"), "{error}");
    assert!(error.contains("requested 'euclidean'"), "{error}");
    assert_eq!(graph.version(), before);
    assert!(!list_edge_vector_indexes(&graph)[0].built);
    assert_eq!(
        graph
            .edge_embeddings
            .get(&("CLAIMS".to_string(), "text_emb".to_string()))
            .unwrap()
            .metric(),
        Some("cosine")
    );
}

/// The metric a build persists is statement state like any other: a rolled-back
/// statement leaves the store scoring the way it did before.
#[test]
fn rolled_back_build_restores_the_stores_prior_metric() {
    let (mut graph, _) = fixture_without_declared_metric();
    let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
    build_edge_vector_index(
        &mut graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions {
            metric: Some("euclidean".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        graph
            .edge_embeddings
            .get(&("CLAIMS".to_string(), "text_emb".to_string()))
            .unwrap()
            .metric(),
        Some("euclidean")
    );
    checkpoint.rollback(&mut graph);
    assert_eq!(
        graph
            .edge_embeddings
            .get(&("CLAIMS".to_string(), "text_emb".to_string()))
            .unwrap()
            .metric(),
        None
    );
    assert!(!list_edge_vector_indexes(&graph)[0].built);
}

/// Recovery reproduces the store the build left behind, metric included — so a
/// reopened graph does not go back to resolving cosine against a euclidean
/// index.
#[test]
fn wal_replay_restores_the_metric_the_index_was_built_for() {
    let (mut recovered, _) = fixture_without_declared_metric();
    crate::graph::mutation::wal_replay::apply_frames(
        &mut recovered,
        &[crate::graph::wal::WalFrame {
            lsn: 1,
            ops: vec![crate::graph::wal::MutationOp::SetEdgeVectorIndex {
                conn_type: "CLAIMS".into(),
                text_column: "text".into(),
                metric: Some("euclidean".into()),
                m: Some(8),
                ef_construction: Some(32),
                ef_search: Some(16),
                auto_refresh_limit: Some(4),
                present: true,
            }],
        }],
        0,
    )
    .unwrap();
    assert_eq!(
        recovered
            .edge_embeddings
            .get(&("CLAIMS".to_string(), "text_emb".to_string()))
            .unwrap()
            .metric(),
        Some("euclidean")
    );
    let served = query_edge_embeddings(
        &recovered,
        "CLAIMS",
        "text",
        &[3.0, 0.0],
        EdgeVectorQueryOptions {
            top_k: 2,
            exact: false,
            metric: None,
        },
    )
    .unwrap();
    assert_eq!(served.search_method, "hnsw");
}
