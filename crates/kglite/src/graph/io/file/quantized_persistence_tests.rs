//! Quantized scan copies across save and load: sections in the data region,
//! mapped and heap placement, rebuild when no section travels, corruption,
//! promotion on write, and two-stage search against exact search.

use super::mappable::SaveLayout;
use super::*;
use crate::graph::algorithms::vector::{
    vector_search, DistanceMetric, Scorer, VectorSearchOptions, VectorSearchResult,
};
use crate::graph::edge_embeddings::{edge_store_key, upsert_edge_embeddings};
use crate::graph::quantized::tests::gaussian_vectors;
use crate::graph::quantized::Precision;
use crate::graph::schema::CurrentSelection;
use crate::graph::session::{execute_mut, ExecuteOptions};
use std::collections::HashMap;

const DIM: usize = 1024;
/// 1100 x 1024 x 4 bytes = 4.4 MiB, just over the writer's floor.
const DOCS: usize = 1100;

fn corpus() -> Vec<Vec<f32>> {
    gaussian_vectors(DOCS, DIM, 42)
}

fn graph_with(vectors: &[Vec<f32>], with_edges: bool) -> DirGraph {
    let mut graph = DirGraph::new();
    execute_mut(
        &mut graph,
        &format!(
            "UNWIND range(1, {}) AS i CREATE (:Doc {{id: i, title: 'd', text: 'x'}})",
            vectors.len()
        ),
        &ExecuteOptions::eager(&HashMap::new()),
    )
    .unwrap();
    crate::graph::embeddings::set_embeddings(
        &mut graph,
        "Doc",
        "text",
        Some("cosine"),
        vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (Value::Int64(i as i64 + 1), v.clone())),
    )
    .unwrap();
    if with_edges {
        execute_mut(
            &mut graph,
            "MATCH (a:Doc), (b:Doc) WHERE b.id = a.id + 1 CREATE (a)-[:NEXT {text: 't'}]->(b)",
            &ExecuteOptions::eager(&HashMap::new()),
        )
        .unwrap();
        let edges: Vec<_> = graph.graph.edge_indices().collect();
        let edge_vectors = gaussian_vectors(edges.len(), DIM, 9);
        let entries: Vec<_> = edges.into_iter().zip(edge_vectors).collect();
        upsert_edge_embeddings(&mut graph, "NEXT", "text", entries, None).unwrap();
    }
    graph
}

fn node_store(graph: &DirGraph) -> &EmbeddingStore {
    graph.embeddings.values().next().expect("one node store")
}

fn edge_store(graph: &DirGraph) -> &EmbeddingStore {
    graph
        .edge_embeddings
        .get(&edge_store_key("NEXT", "text"))
        .expect("the NEXT store")
        .numeric()
}

fn quantize_node(graph: &mut DirGraph, precision: Precision) {
    graph
        .embeddings
        .values_mut()
        .next()
        .unwrap()
        .quantize(precision, None)
        .unwrap();
}

fn save_to(graph: DirGraph, path: &std::path::Path, layout: SaveLayout) -> Arc<DirGraph> {
    let mut arc = Arc::new(graph);
    save_graph_with_layout(&mut arc, path.to_str().unwrap(), true, layout).unwrap();
    arc
}

fn load_with(path: &std::path::Path, policy: MapPolicy) -> Arc<DirGraph> {
    load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new().with_embeddings(policy),
    )
    .unwrap()
}

fn search(graph: &DirGraph, query: &[f32], top_k: usize, exact: bool) -> Vec<VectorSearchResult> {
    vector_search(
        graph,
        &CurrentSelection::new(),
        "text_emb",
        query,
        &VectorSearchOptions::default()
            .with_top_k(top_k)
            .with_metric(DistanceMetric::Cosine)
            .with_exact(exact),
    )
    .unwrap()
}

fn ranking(results: &[VectorSearchResult]) -> Vec<(usize, u32)> {
    results
        .iter()
        .map(|r| (r.node_idx.index(), r.score.to_bits()))
        .collect()
}

fn mappable_names(path: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    metadata_head::read_metadata_head(&bytes, "test")
        .unwrap()
        .mappable
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

fn codes_equal(left: &EmbeddingStore, right: &EmbeddingStore) -> bool {
    use crate::graph::quantized::Codes;
    let (a, b) = (left.quant_store().unwrap(), right.quant_store().unwrap());
    let codes = match (a.codes(), b.codes()) {
        (Codes::F16(x), Codes::F16(y)) => x.as_slice() == y.as_slice(),
        (Codes::Int8(x), Codes::Int8(y)) => x.as_slice() == y.as_slice(),
        (Codes::Bits(x), Codes::Bits(y)) => x.as_slice() == y.as_slice(),
        _ => false,
    };
    codes && a.scales().as_slice() == b.scales().as_slice() && a.oversample() == b.oversample()
}

#[test]
fn rescoring_returns_exact_scores_and_f16_matches_exact_search() {
    let vectors = corpus();
    let mut graph = graph_with(&vectors, false);
    let queries = gaussian_vectors(20, DIM, 5);
    let exact: Vec<_> = queries
        .iter()
        .map(|q| ranking(&search(&graph, q, 5, true)))
        .collect();
    for precision in Precision::ALL {
        quantize_node(&mut graph, precision);
        let (mut identical, mut total) = (0, 0);
        for (query, expected) in queries.iter().zip(&exact) {
            let got = search(&graph, query, 5, false);
            // Every returned score is the exact f32 score of that node.
            let scorer = Scorer::new(DistanceMetric::Cosine, query);
            let store = node_store(&graph);
            for hit in &got {
                let (embedding, norm) =
                    store.get_embedding_with_norm(hit.node_idx.index()).unwrap();
                assert_eq!(
                    hit.score.to_bits(),
                    scorer.score(query, embedding, norm).to_bits()
                );
            }
            // `exact=true` bypasses the copy.
            assert_eq!(&ranking(&search(&graph, query, 5, true)), expected);
            total += 1;
            if &ranking(&got) == expected {
                identical += 1;
            }
        }
        println!("{precision:?}: {identical}/{total} queries returned the exact top-5");
        match precision {
            Precision::F16 => assert_eq!(identical, total),
            Precision::Int8 => assert!(identical >= total - 1),
            Precision::Binary => assert!(identical >= 1),
        }
    }
}

#[test]
fn a_default_layout_save_writes_sections_and_every_policy_loads_the_same_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    let mut graph = graph_with(&corpus(), true);
    quantize_node(&mut graph, Precision::Int8);
    graph
        .edge_embeddings
        .values_mut()
        .next()
        .unwrap()
        .numeric_mut()
        .quantize(Precision::F16, Some(6))
        .unwrap();
    let original = save_to(graph, &path, SaveLayout::Auto);
    let names = mappable_names(&path);
    for expected in [
        "emb:Doc:text_emb:qcodes",
        "emb:Doc:text_emb:qscales",
        "edge_emb:NEXT:text_emb:qcodes",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
    assert!(!names.iter().any(|n| n == "edge_emb:NEXT:text_emb:qscales"));

    let query = &gaussian_vectors(1, DIM, 77)[0];
    let expected = ranking(&search(&original, query, 5, false));
    for policy in [MapPolicy::Mapped, MapPolicy::Memory] {
        let loaded = load_with(&path, policy);
        let (node, edge) = (node_store(&loaded), edge_store(&loaded));
        assert_eq!(node.precision(), Some(Precision::Int8));
        assert_eq!(edge.precision(), Some(Precision::F16));
        assert_eq!(edge.quant_store().unwrap().oversample(), 6);
        let mapped = policy == MapPolicy::Mapped;
        assert_eq!(node.quant_store().unwrap().is_mapped(), mapped);
        assert_eq!(edge.quant_store().unwrap().is_mapped(), mapped);
        assert!(codes_equal(node, node_store(&original)));
        assert!(codes_equal(edge, edge_store(&original)));
        assert_eq!(ranking(&search(&loaded, query, 5, false)), expected);
        assert!(verify_graph(&loaded).is_ok());
    }
}

#[test]
fn compact_layout_and_small_stores_write_the_record_and_rebuild_the_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("compact.kgl");
    let mut graph = graph_with(&corpus(), false);
    quantize_node(&mut graph, Precision::Binary);
    let original = save_to(graph, &path, SaveLayout::Compact);
    assert_eq!(&std::fs::read(&path).unwrap()[..4], &V8_MAGIC);
    let loaded = load_with(&path, MapPolicy::Mapped);
    assert_eq!(node_store(&loaded).precision(), Some(Precision::Binary));
    assert!(!node_store(&loaded).quant_store().unwrap().is_mapped());
    assert!(codes_equal(node_store(&loaded), node_store(&original)));

    // A store under the writer floor stays inline, record only.
    let small_vectors = gaussian_vectors(50, 16, 8);
    let mut small = graph_with(&small_vectors, false);
    quantize_node(&mut small, Precision::F16);
    let small_path = tmp.path().join("small.kgl");
    let small_original = save_to(small, &small_path, SaveLayout::Auto);
    assert_eq!(&std::fs::read(&small_path).unwrap()[..4], &V8_MAGIC);
    let small_loaded = load_with(&small_path, MapPolicy::Auto);
    assert_eq!(node_store(&small_loaded).precision(), Some(Precision::F16));
    assert!(codes_equal(
        node_store(&small_loaded),
        node_store(&small_original)
    ));
}

#[test]
fn an_unquantized_graph_writes_no_record_and_no_section() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(graph_with(&corpus(), false), &path, SaveLayout::Auto);
    assert!(mappable_names(&path)
        .iter()
        .all(|n| !n.ends_with(":qcodes") && !n.ends_with(":qscales")));
    let bytes = std::fs::read(&path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    assert!(metadata.quantization.is_empty());
    let loaded = load_with(&path, MapPolicy::Mapped);
    assert_eq!(node_store(&loaded).precision(), None);
}

#[test]
fn a_corrupt_quantized_section_is_found_at_first_search_and_by_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    let mut graph = graph_with(&corpus(), false);
    quantize_node(&mut graph, Precision::F16);
    save_to(graph, &path, SaveLayout::Auto);

    let mut bytes = std::fs::read(&path).unwrap();
    let (metadata, base) = {
        let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
        let end = 13 + u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        let base = data_region_base(&metadata, end, bytes.len() as u64).unwrap();
        (metadata, base)
    };
    let entry = metadata
        .mappable
        .iter()
        .find(|e| e.name == "emb:Doc:text_emb:qcodes")
        .unwrap();
    bytes[(base + entry.offset) as usize + 64] ^= 0x55;
    std::fs::write(&path, bytes).unwrap();

    let report = verify_file(path.to_str().unwrap()).unwrap();
    let bad: Vec<_> = report
        .sections
        .iter()
        .filter(|s| s.error.is_some())
        .collect();
    assert_eq!(bad.len(), 1, "{report:?}");
    assert_eq!(bad[0].name, "emb:Doc:text_emb:qcodes");

    let loaded = load_with(&path, MapPolicy::Mapped);
    assert!(!verify_graph(&loaded).is_ok());
    let result = vector_search(
        &loaded,
        &CurrentSelection::new(),
        "text_emb",
        &gaussian_vectors(1, DIM, 1)[0],
        &VectorSearchOptions::default(),
    );
    assert!(result.unwrap_err().contains("corrupt"));
}

#[test]
fn the_first_write_promotes_the_copy_with_the_vectors_and_keeps_it_in_step() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    let mut graph = graph_with(&corpus(), false);
    quantize_node(&mut graph, Precision::Int8);
    save_to(graph, &path, SaveLayout::Auto);
    let loaded = load_with(&path, MapPolicy::Mapped);
    let store = node_store(&loaded);
    assert!(store.quant_store().unwrap().is_mapped());

    let mut clone = store.clone();
    assert!(clone.quant_store().unwrap().is_mapped());
    let replacement = gaussian_vectors(1, DIM, 31)[0].clone();
    clone.set_embedding(5, &replacement);
    clone.set_embedding(9_999_999, &replacement);
    let removed = clone.remove_embedding(7).unwrap();
    assert!(!clone.quant_store().unwrap().is_mapped());
    let rebuilt =
        crate::graph::quantized::QuantStore::build(Precision::Int8, &clone.data, DIM, None)
            .unwrap();
    assert_eq!(
        clone.quant_store().unwrap().slot_count(),
        rebuilt.slot_count()
    );
    clone.restore_embedding(7, &removed);
    assert!(clone.quantized().is_some());
    // The mapped original is untouched.
    assert!(store.quant_store().unwrap().is_mapped());
    assert_eq!(store.quant_store().unwrap().slot_count(), DOCS);
}

#[test]
fn a_relationship_search_reports_the_quantized_route_and_exact_bypasses_it() {
    let graph = {
        let mut graph = graph_with(&corpus(), true);
        graph
            .edge_embeddings
            .values_mut()
            .next()
            .unwrap()
            .numeric_mut()
            .quantize(Precision::F16, None)
            .unwrap();
        graph
    };
    let store = edge_store(&graph);
    let query = &gaussian_vectors(1, DIM, 3)[0];
    let (hits, method) = crate::graph::edge_embeddings::vector_index::query_dense_store(
        store,
        query,
        5,
        false,
        DistanceMetric::Cosine,
        false,
    );
    assert_eq!(method, "quantized");
    let (exact_hits, exact_method) = crate::graph::edge_embeddings::vector_index::query_dense_store(
        store,
        query,
        5,
        true,
        DistanceMetric::Cosine,
        false,
    );
    assert_eq!(exact_method, "exact");
    assert_eq!(
        hits, exact_hits,
        "f16 candidates plus the exact re-score agree"
    );
    // Poincare cannot be ranked by a quantized dot product.
    let (_, poincare) = crate::graph::edge_embeddings::vector_index::query_dense_store(
        store,
        query,
        5,
        false,
        DistanceMetric::Poincare,
        false,
    );
    assert_eq!(poincare, "exact");
}

#[test]
fn cypher_deletes_and_vacuum_keep_the_copy_in_step() {
    let vectors = gaussian_vectors(60, 16, 21);
    let mut graph = graph_with(&vectors, true);
    quantize_node(&mut graph, Precision::Int8);
    graph
        .edge_embeddings
        .values_mut()
        .next()
        .unwrap()
        .numeric_mut()
        .quantize(Precision::F16, None)
        .unwrap();
    let in_step = |graph: &DirGraph, label: &str| {
        for store in [node_store(graph), edge_store(graph)] {
            let held = store.quant_store().expect(label);
            let rebuilt = crate::graph::quantized::QuantStore::build(
                held.precision(),
                &store.data,
                store.dimension,
                None,
            )
            .unwrap();
            assert!(store.quantized().is_some(), "{label}: guard");
            assert_eq!(held.slot_count(), rebuilt.slot_count(), "{label}");
            assert_eq!(
                (held.precision(), held.byte_size()),
                (rebuilt.precision(), rebuilt.byte_size())
            );
        }
    };
    execute_mut(
        &mut graph,
        "MATCH (d:Doc) WHERE d.id % 3 = 0 DETACH DELETE d",
        &ExecuteOptions::eager(&HashMap::new()),
    )
    .unwrap();
    assert!(node_store(&graph).len() < 60);
    in_step(&graph, "after delete");
    graph.vacuum();
    in_step(&graph, "after vacuum");
}
