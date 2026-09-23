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
