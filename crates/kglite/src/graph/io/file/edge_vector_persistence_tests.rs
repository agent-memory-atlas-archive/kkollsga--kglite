//! Relationship HNSW persistence in `.kgl` (the `edge_vector_index` section).

use super::super::{load_kgl_bytes, prepare_kgl_write, read_metadata_head, write_kgl_to};
use super::EDGE_VECTOR_INDEX_SECTION;
use crate::datatypes::values::Value;
use crate::graph::edge_embeddings::vector_index::{
    build_edge_vector_index, list_edge_vector_indexes, query_edge_embeddings,
    EdgeVectorIndexOptions, EdgeVectorQueryOptions,
};
use crate::graph::edge_embeddings::{edge_store_key, upsert_edge_embeddings};
use crate::graph::schema::{DirGraph, EdgeData, NodeData};
use crate::graph::storage::GraphWrite;
use petgraph::graph::EdgeIndex;
use std::collections::HashMap;
use std::sync::Arc;

/// A hub with `count` outgoing `conn` edges, each carrying a unit vector on a
/// distinct angle, so the nearest neighbour of any query is unambiguous.
fn graph_with_store(conns: &[&str], count: usize) -> (DirGraph, Vec<EdgeIndex>) {
    let mut graph = DirGraph::new();
    let hub = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(0),
            Value::String("hub".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let mut edges = Vec::new();
    for conn in conns {
        graph.register_connection_type(conn.to_string());
        let mut entries = Vec::new();
        for i in 0..count {
            let leaf = GraphWrite::add_node(
                &mut graph.graph,
                NodeData::new(
                    Value::Int64((edges.len() + 1) as i64),
                    Value::String(format!("leaf{i}")),
                    "Doc".into(),
                    HashMap::new(),
                    &mut graph.interner,
                ),
            );
            let edge = GraphWrite::add_edge(
                &mut graph.graph,
                hub,
                leaf,
                EdgeData::new(conn.to_string(), HashMap::new(), &mut graph.interner),
            );
            let angle = std::f32::consts::TAU * i as f32 / count as f32;
            entries.push((edge, vec![angle.cos(), angle.sin()]));
            edges.push(edge);
        }
        upsert_edge_embeddings(&mut graph, conn, "text", entries, Some("cosine")).unwrap();
    }
    (graph, edges)
}

fn build(graph: &mut DirGraph, conn: &str, auto_refresh_limit: Option<usize>) {
    let options = EdgeVectorIndexOptions {
        auto_refresh_limit,
        ..EdgeVectorIndexOptions::default()
    };
    build_edge_vector_index(graph, conn, "text", options).unwrap();
}

fn encode(graph: DirGraph) -> Vec<u8> {
    let mut graph = Arc::new(graph);
    prepare_kgl_write(&mut graph);
    let mut bytes = Vec::new();
    write_kgl_to(&graph, &mut bytes).unwrap();
    bytes
}

fn search_method(graph: &DirGraph, conn: &str) -> &'static str {
    let options = EdgeVectorQueryOptions {
        top_k: 3,
        exact: false,
        metric: None,
    };
    query_edge_embeddings(graph, conn, "text", &[1.0, 0.0], options)
        .unwrap()
        .search_method
}

#[test]
fn a_built_relationship_index_round_trips_online_and_answers_through_hnsw() {
    let (mut graph, _) = graph_with_store(&["CLAIMS"], 16);
    build(&mut graph, "CLAIMS", None);
    let bytes = encode(graph);

    let metadata = read_metadata_head(&bytes, "buffer").unwrap();
    assert!(metadata.edge_vector_index_compressed_size > 0);
    assert!(metadata
        .section_digests
        .contains_key(EDGE_VECTOR_INDEX_SECTION));

    let loaded = load_kgl_bytes(&bytes).unwrap();
    let status = &list_edge_vector_indexes(&loaded)[0];
    assert!(status.built && !status.stale && status.delta == 0);
    assert_eq!(search_method(&loaded, "CLAIMS"), "hnsw");
}

#[test]
fn a_stale_relationship_index_reloads_owing_the_same_delta() {
    let (mut graph, edges) = graph_with_store(&["CLAIMS"], 8);
    build(&mut graph, "CLAIMS", Some(0));
    // An in-place replacement: a dirty slot the index has not folded in.
    upsert_edge_embeddings(
        &mut graph,
        "CLAIMS",
        "text",
        vec![(edges[0], vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let before = list_edge_vector_indexes(&graph)[0].clone();
    assert!(before.built && before.stale && before.delta == 1);

    let loaded = load_kgl_bytes(&encode(graph)).unwrap();
    assert_eq!(list_edge_vector_indexes(&loaded)[0], before);
}

#[test]
fn no_relationship_index_means_no_section_and_no_metadata_key() {
    // Node-only, and relationship vectors with no index: neither may write the
    // section or its key, or node-only files would change bytes.
    let (unindexed, _) = graph_with_store(&["CLAIMS"], 4);
    for bytes in [encode(DirGraph::new()), encode(unindexed)] {
        let len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        let metadata: serde_json::Value = serde_json::from_slice(&bytes[13..13 + len]).unwrap();
        assert!(metadata.get("edge_vector_index_compressed_size").is_none());
        assert!(metadata["section_digests"]
            .get(EDGE_VECTOR_INDEX_SECTION)
            .is_none());
    }
}

#[test]
fn several_relationship_indexes_round_trip_and_encode_independent_of_map_order() {
    // The HNSW build itself is concurrent and not bit-reproducible, so the
    // determinism under test is the store order: the same indexes re-housed in
    // fresh maps (each with its own hash seed) must encode to the same bytes.
    const CONNS: [&str; 6] = ["F_REL", "B_REL", "D_REL", "A_REL", "E_REL", "C_REL"];
    let (mut graph, _) = graph_with_store(&CONNS, 6);
    for conn in CONNS {
        build(&mut graph, conn, None);
    }
    let first = encode(graph.clone());
    for _ in 0..8 {
        let mut rehoused = graph.clone();
        rehoused.edge_embeddings = graph
            .edge_embeddings
            .iter()
            .map(|(key, store)| (key.clone(), store.clone()))
            .collect::<HashMap<_, _>>();
        assert!(
            first == encode(rehoused),
            "store-map order leaked into the bytes"
        );
    }

    let loaded = load_kgl_bytes(&first).unwrap();
    for conn in CONNS {
        assert!(loaded.edge_embeddings[&edge_store_key(conn, "text")]
            .index_store()
            .has_index());
        assert_eq!(search_method(&loaded, conn), "hnsw");
    }
}
