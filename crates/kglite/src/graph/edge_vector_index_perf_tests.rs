//! Release-only edge/node exact-versus-HNSW measurement instrument.

use super::*;
use crate::datatypes::Value;
use crate::graph::algorithms::hnsw::HnswParams;
use crate::graph::algorithms::vector::{vector_search, DistanceMetric, VectorSearchOptions};
use crate::graph::edge_embeddings::upsert_edge_embeddings;
use crate::graph::edge_embeddings::vector_index::{
    build_edge_vector_index, query_edge_embeddings, EdgeVectorIndexOptions, EdgeVectorQueryOptions,
};
use crate::graph::embeddings::set_embeddings;
use crate::graph::schema::{CurrentSelection, EdgeData, NodeData};
use crate::graph::storage::GraphWrite;
use std::collections::{HashMap, HashSet};
use std::hint::black_box;
use std::time::Instant;

const VECTORS: usize = 2_000;
const DIMENSION: usize = 64;
const TOP_K: usize = 10;
const QUERIES: usize = 64;
const WARMUP: usize = 20;
const ROUNDS: usize = 200;

fn corpus(count: usize, dimension: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        ((state >> 11) as f64 / ((1_u64 << 53) as f64)) as f32 - 0.5
    };
    (0..count)
        .map(|_| (0..dimension).map(|_| next()).collect())
        .collect()
}

fn fixture(vectors: &[Vec<f32>]) -> (DirGraph, DirGraph) {
    let mut edge_graph = DirGraph::new();
    let hub = GraphWrite::add_node(
        &mut edge_graph.graph,
        NodeData::new(
            Value::Int64(0),
            Value::String("hub".into()),
            "Hub".into(),
            HashMap::new(),
            &mut edge_graph.interner,
        ),
    );
    let mut edge_entries = Vec::with_capacity(vectors.len());
    for (ordinal, vector) in vectors.iter().enumerate() {
        let node = GraphWrite::add_node(
            &mut edge_graph.graph,
            NodeData::new(
                Value::Int64(ordinal as i64 + 1),
                Value::String(format!("d{ordinal}")),
                "Doc".into(),
                HashMap::new(),
                &mut edge_graph.interner,
            ),
        );
        let edge = GraphWrite::add_edge(
            &mut edge_graph.graph,
            hub,
            node,
            EdgeData::new("CLAIMS".into(), HashMap::new(), &mut edge_graph.interner),
        );
        edge_entries.push((edge, vector.clone()));
    }
    upsert_edge_embeddings(
        &mut edge_graph,
        "CLAIMS",
        "text",
        edge_entries,
        Some("cosine"),
    )
    .expect("edge vectors");

    let mut node_graph = DirGraph::new();
    let mut node_entries = Vec::with_capacity(vectors.len());
    for (ordinal, vector) in vectors.iter().enumerate() {
        let id = ordinal as i64 + 1;
        let properties = HashMap::from([("text".into(), Value::String(format!("text {id}")))]);
        let node = GraphWrite::add_node(
            &mut node_graph.graph,
            NodeData::new(
                Value::Int64(id),
                Value::String(format!("d{ordinal}")),
                "Doc".into(),
                properties,
                &mut node_graph.interner,
            ),
        );
        node_graph
            .type_indices
            .entry_or_default("Doc".into())
            .push(node);
        node_entries.push((Value::Int64(id), vector.clone()));
    }
    node_graph.build_id_index("Doc");
    set_embeddings(&mut node_graph, "Doc", "text", Some("cosine"), node_entries)
        .expect("node vectors");
    assert_eq!(
        node_graph.embeddings[&("Doc".into(), "text_emb".into())].len(),
        vectors.len(),
        "node control must contain the full seeded corpus"
    );
    (edge_graph, node_graph)
}

fn overlap(left: &[usize], right: &[usize]) -> usize {
    let expected: HashSet<_> = left.iter().copied().collect();
    right
        .iter()
        .filter(|value| expected.contains(value))
        .count()
}

fn measurement(mut action: impl FnMut(usize)) -> (f64, f64) {
    for round in 0..WARMUP {
        action(round);
    }
    let mut micros = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let started = Instant::now();
        action(round);
        micros.push(started.elapsed().as_secs_f64() * 1_000_000.0);
    }
    let mean = micros.iter().sum::<f64>() / micros.len() as f64;
    let min = micros.into_iter().fold(f64::INFINITY, f64::min);
    (mean, min)
}

#[test]
#[ignore = "release-only edge/node vector index measurement"]
fn measure_edge_vector_index_matrix() {
    let run = std::env::var("KGLITE_BENCH_RUN").unwrap_or_else(|_| "unknown".into());
    let vectors = corpus(VECTORS, DIMENSION, 0x5eed_1234);
    let queries = corpus(QUERIES, DIMENSION, 0xcafe_babe);
    let (mut edge_graph, mut node_graph) = fixture(&vectors);
    build_edge_vector_index(
        &mut edge_graph,
        "CLAIMS",
        "text",
        EdgeVectorIndexOptions::default(),
    )
    .expect("edge index");
    node_graph
        .embeddings
        .get_mut(&("Doc".into(), "text_emb".into()))
        .expect("node store")
        .build_index(DistanceMetric::Cosine, HnswParams::default(), 7)
        .expect("node index");

    let mut edge_exact = Vec::with_capacity(queries.len());
    let mut edge_ann = Vec::with_capacity(queries.len());
    let mut node_exact = Vec::with_capacity(queries.len());
    let mut node_ann = Vec::with_capacity(queries.len());
    let selection = CurrentSelection::new();
    for query in &queries {
        edge_exact.push(
            query_edge_embeddings(
                &edge_graph,
                "CLAIMS",
                "text",
                query,
                EdgeVectorQueryOptions {
                    top_k: TOP_K,
                    exact: true,
                    metric: None,
                },
            )
            .unwrap()
            .hits
            .into_iter()
            .map(|hit| hit.edge.index())
            .collect::<Vec<_>>(),
        );
        edge_ann.push(
            query_edge_embeddings(
                &edge_graph,
                "CLAIMS",
                "text",
                query,
                EdgeVectorQueryOptions {
                    top_k: TOP_K,
                    exact: false,
                    metric: None,
                },
            )
            .unwrap()
            .hits
            .into_iter()
            .map(|hit| hit.edge.index())
            .collect::<Vec<_>>(),
        );
        for (exact, output) in [(true, &mut node_exact), (false, &mut node_ann)] {
            output.push(
                vector_search(
                    &node_graph,
                    &selection,
                    "text_emb",
                    query,
                    &VectorSearchOptions::default()
                        .with_top_k(TOP_K)
                        .with_metric(DistanceMetric::Cosine)
                        .with_exact(exact),
                )
                .unwrap()
                .into_iter()
                .map(|hit| hit.node_idx.index())
                .collect::<Vec<_>>(),
            );
        }
    }
    let edge_recall = edge_exact
        .iter()
        .zip(&edge_ann)
        .map(|(exact, ann)| overlap(exact, ann))
        .sum::<usize>() as f64
        / (QUERIES * TOP_K) as f64;
    let node_recall = node_exact
        .iter()
        .zip(&node_ann)
        .map(|(exact, ann)| overlap(exact, ann))
        .sum::<usize>() as f64
        / (QUERIES * TOP_K) as f64;

    println!("run,entity,method,vectors,dimension,top_k,metric,mean_us,min_us,recall_at_k");
    for (entity, exact, recall) in [
        ("edge", true, 1.0),
        ("edge", false, edge_recall),
        ("node", true, 1.0),
        ("node", false, node_recall),
    ] {
        let (mean, min) = measurement(|round| {
            let query = &queries[round % queries.len()];
            if entity == "edge" {
                black_box(
                    query_edge_embeddings(
                        black_box(&edge_graph),
                        "CLAIMS",
                        "text",
                        black_box(query),
                        EdgeVectorQueryOptions {
                            top_k: TOP_K,
                            exact,
                            metric: None,
                        },
                    )
                    .unwrap(),
                );
            } else {
                black_box(
                    vector_search(
                        black_box(&node_graph),
                        &selection,
                        "text_emb",
                        black_box(query),
                        &VectorSearchOptions::default()
                            .with_top_k(TOP_K)
                            .with_metric(DistanceMetric::Cosine)
                            .with_exact(exact),
                    )
                    .unwrap(),
                );
            }
        });
        println!(
            "{run},{entity},{},{VECTORS},{DIMENSION},{TOP_K},cosine,{mean:.3},{min:.3},{recall:.6}",
            if exact { "exact" } else { "ann" }
        );
    }
}
