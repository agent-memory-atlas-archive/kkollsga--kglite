//! A stored non-finite embedding coordinate refuses the load, for node and
//! relationship stores, on every load route: a portable `.kgl` in memory and
//! mapped mode, and a disk directory.

use super::*;
use crate::graph::edge_embeddings::{edge_store_key, upsert_edge_embeddings, EdgeEmbeddingStore};
use crate::graph::session::{execute_mut, ExecuteOptions};
use crate::graph::storage::mode::StorageMode;
use crate::graph::storage::GraphRead;
use std::collections::HashMap;
use std::sync::Arc;

/// Three nodes and two relationships, each with a finite 2-d vector.
fn graph_with_vectors() -> DirGraph {
    let mut graph = DirGraph::new();
    execute_mut(
        &mut graph,
        "CREATE (a:Doc {id: 1, title: 'a', text: 'x'}), (b:Doc {id: 2, title: 'b', text: 'y'}), (c:Doc {id: 3, title: 'c', text: 'z'}), \
         (a)-[:ASSERTS {text: 'p', summary: 'q'}]->(b), (b)-[:ASSERTS {text: 'r', summary: 's'}]->(c)",
        &ExecuteOptions::eager(&HashMap::new()),
    )
    .unwrap();
    crate::graph::embeddings::set_embeddings(
        &mut graph,
        "Doc",
        "text",
        None,
        [
            (Value::Int64(1), vec![1.0f32, 0.0]),
            (Value::Int64(2), vec![0.0, 1.0]),
            (Value::Int64(3), vec![0.5, 0.5]),
        ],
    )
    .unwrap();
    let edges: Vec<_> = graph.graph.edge_indices().collect();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "text",
        vec![(edges[0], vec![1.0, 0.0]), (edges[1], vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    graph
}

/// Coordinate 3 of the node store (the second vector's second coordinate).
fn with_nonfinite_node_coordinate(value: f32) -> DirGraph {
    let mut graph = graph_with_vectors();
    graph.embeddings.values_mut().next().unwrap().data[3] = value;
    graph
}

/// A second relationship store whose coordinate 2 is non-finite.
fn with_nonfinite_edge_coordinate(value: f32) -> DirGraph {
    let mut graph = graph_with_vectors();
    let edges: Vec<_> = graph.graph.edge_indices().collect();
    graph.edge_embeddings.insert(
        edge_store_key("ASSERTS", "summary"),
        EdgeEmbeddingStore::decoded_fixture(
            2,
            [(edges[0], vec![1.0, 0.0]), (edges[1], vec![value, 0.0])],
        ),
    );
    graph
}

/// The error each load route gives, portable (memory, mapped) then disk.
fn load_errors(graph: DirGraph) -> Vec<io::Error> {
    let tmp = tempfile::tempdir().unwrap();
    let kgl = tmp.path().join("g.kgl");
    let mut portable = Arc::new(graph);
    save_graph(&mut portable, kgl.to_str().unwrap()).unwrap();
    let mut errors = Vec::new();
    for options in [
        LoadOptions::new(),
        LoadOptions::new().with_storage(StorageMode::Mapped),
    ] {
        errors.push(
            load_file_with(kgl.to_str().unwrap(), &options)
                .err()
                .expect("a non-finite stored coordinate must refuse the load"),
        );
    }
    let dir = tmp.path().join("disk");
    let mut disk = Arc::try_unwrap(portable).ok().unwrap();
    disk.enable_disk_mode().unwrap();
    let mut disk = Arc::new(disk);
    save_graph(&mut disk, dir.to_str().unwrap()).unwrap();
    drop(disk);
    errors.push(
        load_file(dir.to_str().unwrap())
            .err()
            .expect("a non-finite stored coordinate must refuse the disk load"),
    );
    errors
}

#[test]
fn finite_vectors_load_with_their_norms() {
    let tmp = tempfile::tempdir().unwrap();
    let kgl = tmp.path().join("g.kgl");
    let mut graph = Arc::new(graph_with_vectors());
    save_graph(&mut graph, kgl.to_str().unwrap()).unwrap();
    let loaded = load_file(kgl.to_str().unwrap()).unwrap();
    let store = loaded.embeddings.values().next().unwrap();
    assert_eq!(store.norms.len(), 3);
    assert!((store.norms[2] - 0.5f32.hypot(0.5)).abs() < 1e-6);
}

#[test]
fn nonfinite_node_coordinate_refuses_every_load_route() {
    for (value, shown) in [
        (f32::NAN, "NaN"),
        (f32::INFINITY, "inf"),
        (f32::NEG_INFINITY, "-inf"),
    ] {
        for error in load_errors(with_nonfinite_node_coordinate(value)) {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
            let message = error.to_string();
            assert!(
                message.contains(&format!("vector coordinate 3 must be finite (got {shown})")),
                "{message}"
            );
        }
    }
}

#[test]
fn nonfinite_relationship_coordinate_refuses_every_load_route() {
    for (value, shown) in [(f32::NAN, "NaN"), (f32::INFINITY, "inf")] {
        for error in load_errors(with_nonfinite_edge_coordinate(value)) {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
            let message = error.to_string();
            assert!(message.contains("ASSERTS"), "{message}");
            assert!(
                message.contains(&format!("vector coordinate 2 must be finite (got {shown})")),
                "{message}"
            );
        }
    }
}

#[test]
fn large_finite_coordinates_whose_norm_overflows_still_load() {
    let mut graph = graph_with_vectors();
    let store = graph.embeddings.values_mut().next().unwrap();
    store.data[2] = 3.0e38;
    store.data[3] = 3.0e38;
    let tmp = tempfile::tempdir().unwrap();
    let kgl = tmp.path().join("g.kgl");
    let mut graph = Arc::new(graph);
    save_graph(&mut graph, kgl.to_str().unwrap()).unwrap();
    let loaded = load_file(kgl.to_str().unwrap()).unwrap();
    assert!(loaded.embeddings.values().next().unwrap().norms[1].is_infinite());
}
