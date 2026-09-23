use super::*;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::recording::{resolve_ops, wrap_for_durability};
use crate::graph::storage::GraphWrite;
use crate::graph::wal::{append_frame, WalFrame};
use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

const WARMUPS: usize = 20;
const ROUNDS: usize = 200;

fn parallel_graph(members: usize, dimension: Option<usize>) -> (DirGraph, Vec<EdgeIndex>) {
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
    let target = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(2),
            Value::String("target".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let edges: Vec<_> = (0..members)
        .map(|member| {
            GraphWrite::add_edge(
                &mut graph.graph,
                source,
                target,
                EdgeData::new(
                    "ASSERTS".into(),
                    HashMap::from([("member".into(), Value::Int64(member as i64))]),
                    &mut graph.interner,
                ),
            )
        })
        .collect();
    if let Some(dimension) = dimension {
        upsert_edge_embeddings(
            &mut graph,
            "ASSERTS",
            "description",
            edges
                .iter()
                .enumerate()
                .map(|(member, edge)| (*edge, vec![(member + 1) as f32; dimension]))
                .collect(),
            Some("cosine"),
        )
        .unwrap();
    }
    (graph, edges)
}

fn property_capture(mut graph: DirGraph, edge: EdgeIndex) -> WalFrame {
    wrap_for_durability(&mut graph).unwrap();
    graph.graph.recording_mut().unwrap().note_wal_group(edge);
    let revision = graph.interner.get_or_intern("revision");
    graph
        .graph
        .edge_weight_mut(edge)
        .unwrap()
        .properties
        .push((revision, Value::Int64(1)));
    let raw = graph.graph.recording_mut().unwrap().take_ops();
    WalFrame {
        lsn: 1,
        ops: resolve_ops(&raw, &graph),
    }
}

fn vector_capture(mut graph: DirGraph, edge: EdgeIndex, dimension: usize) -> WalFrame {
    wrap_for_durability(&mut graph).unwrap();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(edge, vec![0.5; dimension])],
        Some("cosine"),
    )
    .unwrap();
    let raw = graph.graph.recording_mut().unwrap().take_ops();
    WalFrame {
        lsn: 1,
        ops: resolve_ops(&raw, &graph),
    }
}

fn encoded_len(frame: &WalFrame) -> usize {
    let mut bytes = Vec::new();
    append_frame(&mut bytes, frame).unwrap();
    bytes.len()
}

fn capture_stats_ns(
    base: &DirGraph,
    edge: EdgeIndex,
    run: impl Fn(DirGraph, EdgeIndex) -> WalFrame,
) -> (u128, u128, usize) {
    for _ in 0..WARMUPS {
        black_box(run(base.clone(), edge));
    }
    let mut elapsed = 0u128;
    let mut minimum = u128::MAX;
    let mut bytes = 0;
    for _ in 0..ROUNDS {
        let graph = base.clone();
        let start = Instant::now();
        let frame = black_box(run(graph, edge));
        let sample = start.elapsed().as_nanos();
        elapsed += sample;
        minimum = minimum.min(sample);
        bytes = encoded_len(&frame);
    }
    (minimum, elapsed / ROUNDS as u128, bytes)
}

#[test]
#[ignore = "release-only edge WAL capture boundary measurement"]
fn measure_edge_wal_capture_boundary() {
    println!("members,dimension,variant,bytes,statistic,rounds,warmups,value_ns");
    for members in [1usize, 100] {
        for dimension in [384usize, 1536] {
            let (plain, plain_edges) = parallel_graph(members, None);
            let (vectors, vector_edges) = parallel_graph(members, Some(dimension));
            let (plain_min, plain_mean, plain_bytes) =
                capture_stats_ns(&plain, plain_edges[0], property_capture);
            let (property_min, property_mean, property_bytes) =
                capture_stats_ns(&vectors, vector_edges[0], property_capture);
            let (change_min, change_mean, change_bytes) =
                capture_stats_ns(&vectors, vector_edges[0], |graph, edge| {
                    vector_capture(graph, edge, dimension)
                });
            for (variant, bytes, minimum, mean) in [
                ("no_vector_property", plain_bytes, plain_min, plain_mean),
                (
                    "vector_property",
                    property_bytes,
                    property_min,
                    property_mean,
                ),
                ("one_vector_change", change_bytes, change_min, change_mean),
            ] {
                println!(
                    "{members},{dimension},{variant},{bytes},min,{ROUNDS},{WARMUPS},{minimum}"
                );
                println!("{members},{dimension},{variant},{bytes},mean,{ROUNDS},{WARMUPS},{mean}");
            }
        }
    }
}
