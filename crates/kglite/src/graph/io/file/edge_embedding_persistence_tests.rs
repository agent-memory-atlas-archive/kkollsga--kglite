//! Portable relationship-embedding persistence and corruption contracts.

use super::*;
use crate::graph::edge_embeddings::{
    edge_store_key, remove_edge_embeddings, upsert_edge_embeddings,
};
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::GraphWrite;
use petgraph::graph::EdgeIndex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

fn graph_with_edges() -> (DirGraph, EdgeIndex, EdgeIndex, EdgeIndex) {
    let mut graph = DirGraph::new();
    let a = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(1),
            Value::String("a".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let b = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(2),
            Value::String("b".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    graph.register_connection_type("ASSERTS".into());
    let one = GraphWrite::add_edge(
        &mut graph.graph,
        a,
        b,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    let two = GraphWrite::add_edge(
        &mut graph.graph,
        a,
        b,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    let loop_edge = GraphWrite::add_edge(
        &mut graph.graph,
        a,
        a,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "text",
        vec![
            (one, vec![1.0, 0.0]),
            (two, vec![0.0, 1.0]),
            (loop_edge, vec![0.5, 0.5]),
        ],
        Some("cosine"),
    )
    .unwrap();
    (graph, one, two, loop_edge)
}

fn header_core_version(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[5..9].try_into().unwrap())
}

fn encode(mut graph: Arc<DirGraph>) -> Vec<u8> {
    prepare_kgl_write(&mut graph);
    let mut bytes = Vec::new();
    write_kgl_to(&graph, &mut bytes).unwrap();
    bytes
}

fn rewrite_metadata(bytes: &[u8], mutate: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let old_len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let mut metadata: serde_json::Value = serde_json::from_slice(&bytes[13..13 + old_len]).unwrap();
    mutate(&mut metadata);
    let encoded = serde_json::to_vec(&metadata).unwrap();
    let mut rebuilt = Vec::with_capacity(bytes.len() - old_len + encoded.len());
    rebuilt.extend_from_slice(&bytes[..9]);
    rebuilt.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
    rebuilt.extend_from_slice(&encoded);
    rebuilt.extend_from_slice(&bytes[13 + old_len..]);
    rebuilt
}

#[derive(Serialize)]
struct PersistedFixture {
    dimension: usize,
    data: Vec<f32>,
    edge_slots: Vec<usize>,
    metric: Option<String>,
    model_id: Option<String>,
    text_hashes: HashMap<usize, u64>,
}

fn replace_edge_section(
    bytes: &[u8],
    payload: &BTreeMap<(String, String), PersistedFixture>,
) -> Vec<u8> {
    let old_metadata_len = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&bytes[13..13 + old_metadata_len]).unwrap();
    let old_edge_len = metadata["edge_embeddings_compressed_size"]
        .as_u64()
        .unwrap() as usize;
    let before_edge = metadata["topology_compressed_size"].as_u64().unwrap() as usize
        + metadata["column_sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|section| section["compressed_size"].as_u64().unwrap() as usize)
            .sum::<usize>()
        + metadata["embeddings_compressed_size"].as_u64().unwrap() as usize;
    let old_edge_start = 13 + old_metadata_len + before_edge;
    let raw = codec_ser(serde_codec::CodecVersion::PostcardV1, payload).unwrap();
    let compressed = zstd_compress(&raw).unwrap();
    metadata["edge_embeddings_compressed_size"] = (compressed.len() as u64).into();
    metadata["section_digests"][EDGE_EMBEDDINGS_SECTION] = section_digest(&compressed).into();
    let encoded_metadata = serde_json::to_vec(&metadata).unwrap();
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&bytes[..9]);
    rebuilt.extend_from_slice(&(encoded_metadata.len() as u32).to_le_bytes());
    rebuilt.extend_from_slice(&encoded_metadata);
    rebuilt.extend_from_slice(&bytes[13 + old_metadata_len..old_edge_start]);
    rebuilt.extend_from_slice(&compressed);
    rebuilt.extend_from_slice(&bytes[old_edge_start + old_edge_len..]);
    rebuilt
}

#[test]
fn node_only_save_keeps_core_v3_and_edge_save_requires_v4() {
    let node_only = DirGraph::new();
    let mut bytes = encode(Arc::new(node_only));
    assert_eq!(header_core_version(&bytes), NODE_ONLY_CORE_DATA_VERSION);
    let metadata = read_metadata_head(&bytes, "buffer").unwrap();
    assert_eq!(metadata.edge_embeddings_compressed_size, 0);
    let node_only_bytes = bytes.clone();

    let (with_edges, ..) = graph_with_edges();
    bytes = encode(Arc::new(with_edges));
    assert_eq!(header_core_version(&bytes), CURRENT_CORE_DATA_VERSION);
    let metadata = read_metadata_head(&bytes, "buffer").unwrap();
    assert!(metadata.edge_embeddings_compressed_size > 0);

    if let Some(output) = std::env::var_os("KGLITE_P2_FIXTURE_DIR") {
        let output = std::path::PathBuf::from(output);
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("node-only-v3.kgl"), node_only_bytes).unwrap();
        std::fs::write(output.join("edge-bearing-v4.kgl"), bytes).unwrap();
    }
}

#[test]
fn parallel_edges_and_self_loop_round_trip_with_distinct_vectors() {
    let (graph, one, two, loop_edge) = graph_with_edges();
    let bytes = encode(Arc::new(graph));
    let loaded = load_kgl_bytes(&bytes).unwrap();
    let store = &loaded.edge_embeddings[&edge_store_key("ASSERTS", "text")];
    assert_eq!(store.get(one), Some(&[1.0, 0.0][..]));
    assert_eq!(store.get(two), Some(&[0.0, 1.0][..]));
    assert_eq!(store.get(loop_edge), Some(&[0.5, 0.5][..]));
}

#[test]
fn declared_empty_store_keeps_v4_dimension_and_metric() {
    let (mut graph, one, two, loop_edge) = graph_with_edges();
    remove_edge_embeddings(&mut graph, "ASSERTS", "text", &[one, two, loop_edge]).unwrap();
    let bytes = encode(Arc::new(graph));
    assert_eq!(header_core_version(&bytes), CURRENT_CORE_DATA_VERSION);

    let loaded = load_kgl_bytes(&bytes).unwrap();
    let store = &loaded.edge_embeddings[&edge_store_key("ASSERTS", "text")];
    assert!(store.is_empty());
    assert_eq!(store.dimension(), 2);
    assert_eq!(store.metric(), Some("cosine"));
}

#[test]
fn required_version_and_section_contract_rejects_inconsistent_files() {
    let (graph, ..) = graph_with_edges();
    let bytes = encode(Arc::new(graph));

    let missing = rewrite_metadata(&bytes, |metadata| {
        metadata["edge_embeddings_compressed_size"] = 0.into();
    });
    let error = load_kgl_bytes(&missing)
        .err()
        .expect("missing section must fail");
    assert!(error
        .to_string()
        .contains("requires a non-empty edge_embeddings section"));

    let mismatch = rewrite_metadata(&bytes, |metadata| {
        metadata["core_data_version"] = NODE_ONLY_CORE_DATA_VERSION.into();
    });
    let error = load_kgl_bytes(&mismatch)
        .err()
        .expect("version mismatch must fail");
    assert!(error.to_string().contains("versions disagree"));
}

#[test]
fn corrupt_later_store_rejects_the_whole_edge_section() {
    let (graph, one, ..) = graph_with_edges();
    let bytes = encode(Arc::new(graph));
    let payload = BTreeMap::from([
        (
            ("ASSERTS".into(), "a_emb".into()),
            PersistedFixture {
                dimension: 2,
                data: vec![1.0, 0.0],
                edge_slots: vec![one.index()],
                metric: Some("cosine".into()),
                model_id: None,
                text_hashes: HashMap::new(),
            },
        ),
        (
            ("ASSERTS".into(), "z_emb".into()),
            PersistedFixture {
                dimension: 2,
                data: vec![f32::NAN, 0.0],
                edge_slots: vec![one.index()],
                metric: Some("cosine".into()),
                model_id: None,
                text_hashes: HashMap::new(),
            },
        ),
    ]);
    let corrupt = replace_edge_section(&bytes, &payload);
    let error = load_kgl_bytes(&corrupt)
        .err()
        .expect("corrupt later store must fail")
        .to_string();
    assert!(error.contains("edge_embeddings store 'ASSERTS.z_emb' is invalid"));
    assert!(error.contains("finite"));
}

#[test]
fn v4_rejects_a_validly_framed_empty_store_map() {
    let (graph, ..) = graph_with_edges();
    let bytes = encode(Arc::new(graph));
    let empty = replace_edge_section(&bytes, &BTreeMap::new());
    let error = load_kgl_bytes(&empty)
        .err()
        .expect("v4 empty payload must fail")
        .to_string();
    assert!(error.contains("required payload contains no stores"));
}

#[test]
fn decoded_store_validation_rejects_nonfinite_dead_and_wrong_type() {
    let (graph, one, ..) = graph_with_edges();
    let mut nonfinite = crate::graph::edge_embeddings::EdgeEmbeddingStore::decoded_fixture(
        2,
        [(one, vec![f32::NAN, 0.0])],
    );
    assert!(nonfinite
        .validate_for_graph(&graph, "ASSERTS", "text_emb")
        .is_err());

    let mut dead = crate::graph::edge_embeddings::EdgeEmbeddingStore::decoded_fixture(
        2,
        [(EdgeIndex::new(9999), vec![1.0, 0.0])],
    );
    assert!(dead
        .validate_for_graph(&graph, "ASSERTS", "text_emb")
        .is_err());

    let mut wrong_type = crate::graph::edge_embeddings::EdgeEmbeddingStore::decoded_fixture(
        2,
        [(one, vec![1.0, 0.0])],
    );
    assert!(wrong_type
        .validate_for_graph(&graph, "MENTIONS", "text_emb")
        .is_err());
}
