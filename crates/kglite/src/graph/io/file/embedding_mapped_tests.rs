//! Embedding stores in a v9 container: what is written to the data region,
//! how each load policy places it, and what writes, clones, corruption and a
//! save over the loaded path do to a mapped store.

use super::embedding_sections::{store_goes_to_data_region, WRITE_MIN_BYTES};
use super::mappable::SaveLayout;
use super::*;
use crate::graph::algorithms::vector::{
    vector_search, DistanceMetric, VectorSearchOptions, VectorSearchResult,
};
use crate::graph::edge_embeddings::{edge_store_key, upsert_edge_embeddings};
use crate::graph::schema::CurrentSelection;
use crate::graph::session::{execute_mut, ExecuteOptions};
use std::collections::HashMap;

const DIM: usize = 1024;
/// 1100 x 1024 x 4 bytes = 4.4 MiB, just over the writer's floor.
const DOCS: i64 = 1100;

fn vector_for(i: i64) -> Vec<f32> {
    (0..DIM)
        .map(|j| (((i * 31 + j as i64 * 17) % 101) as f32) / 50.0 - 1.0 + (i % 7) as f32 * 0.01)
        .collect()
}

/// `DOCS` nodes each with a vector, chained by `NEXT` relationships that each
/// have one too.
fn big_graph(with_edges: bool) -> DirGraph {
    let mut graph = DirGraph::new();
    execute_mut(
        &mut graph,
        &format!(
            "UNWIND range(1, {DOCS}) AS i CREATE (:Doc {{id: i, title: 'd', text: 'x', note: 'n'}})"
        ),
        &ExecuteOptions::eager(&HashMap::new()),
    )
    .unwrap();
    crate::graph::embeddings::set_embeddings(
        &mut graph,
        "Doc",
        "text",
        Some("cosine"),
        (1..=DOCS).map(|i| (Value::Int64(i), vector_for(i))),
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
        let entries: Vec<_> = edges
            .iter()
            .enumerate()
            .map(|(n, &edge)| (edge, vector_for(10_000 + n as i64)))
            .collect();
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

fn search(graph: &DirGraph, query: &[f32], exact: bool) -> Result<Vec<VectorSearchResult>, String> {
    vector_search(
        graph,
        &CurrentSelection::new(),
        "text_emb",
        query,
        &VectorSearchOptions::default()
            .with_top_k(5)
            .with_metric(DistanceMetric::Cosine)
            .with_exact(exact),
    )
}

fn ranking(results: &[VectorSearchResult]) -> Vec<(usize, u32)> {
    results
        .iter()
        .map(|r| (r.node_idx.index(), r.score.to_bits()))
        .collect()
}

fn magic(path: &std::path::Path) -> [u8; 4] {
    std::fs::read(path).unwrap()[..4].try_into().unwrap()
}

/// Flip one byte inside the first mappable section (the node store's vectors).
fn corrupt_first_section(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).unwrap();
    let (metadata, base) = {
        let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
        let end = 13 + u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
        let base = data_region_base(&metadata, end, bytes.len() as u64).unwrap();
        (metadata, base)
    };
    let entry = &metadata.mappable[0];
    bytes[(base + entry.offset) as usize + 100] ^= 0x55;
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_large_store_is_written_to_the_data_region_and_a_small_one_stays_inline() {
    let graph = big_graph(true);
    assert!(store_goes_to_data_region(
        node_store(&graph),
        SaveLayout::Auto
    ));
    assert!(!store_goes_to_data_region(
        node_store(&graph),
        SaveLayout::Compact
    ));
    assert!(node_store(&graph).data.len() * 4 >= WRITE_MIN_BYTES);

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(graph, &path, SaveLayout::Auto);
    assert_eq!(magic(&path), V9_MAGIC);
    let bytes = std::fs::read(&path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    let names: Vec<_> = metadata.mappable.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "emb:Doc:text_emb:data",
            "emb:Doc:text_emb:norms",
            "edge_emb:NEXT:text_emb:data",
            "edge_emb:NEXT:text_emb:norms"
        ]
    );

    let mut small = Arc::new(DirGraph::new());
    execute_mut(
        Arc::make_mut(&mut small),
        "CREATE (:Doc {id: 1, title: 'a', text: 'x'})",
        &ExecuteOptions::eager(&HashMap::new()),
    )
    .unwrap();
    crate::graph::embeddings::set_embeddings(
        Arc::make_mut(&mut small),
        "Doc",
        "text",
        None,
        [(Value::Int64(1), vec![1.0f32, 0.0])],
    )
    .unwrap();
    let small_path = tmp.path().join("small.kgl");
    save_graph(&mut small, small_path.to_str().unwrap()).unwrap();
    assert_eq!(
        magic(&small_path),
        V8_MAGIC,
        "a small store leaves the v8 bytes"
    );
}

#[test]
fn every_policy_serves_identical_vectors_norms_and_rankings() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    let original = save_to(big_graph(true), &path, SaveLayout::Auto);
    let query = vector_for(77);
    let expected = ranking(&search(&original, &query, true).unwrap());

    let mapped = load_with(&path, MapPolicy::Mapped);
    let memory = load_with(&path, MapPolicy::Memory);
    let auto = load_with(&path, MapPolicy::Auto);
    assert!(node_store(&mapped).is_mapped() && edge_store(&mapped).is_mapped());
    assert!(node_store(&mapped).norms.is_mapped());
    assert!(!node_store(&memory).is_mapped() && !edge_store(&memory).is_mapped());
    assert!(
        !node_store(&auto).is_mapped(),
        "4.4 MiB is under the 64 MiB Auto floor"
    );
    for graph in [&mapped, &memory, &auto] {
        assert_eq!(node_store(graph).data, node_store(&original).data);
        assert_eq!(node_store(graph).norms, node_store(&original).norms);
        assert_eq!(edge_store(graph).data, edge_store(&original).data);
        assert_eq!(edge_store(graph).norms, edge_store(&original).norms);
        assert_eq!(ranking(&search(graph, &query, true).unwrap()), expected);
        assert_eq!(node_store(graph).metric.as_deref(), Some("cosine"));
    }
}

#[test]
fn the_kill_switch_forces_the_heap_under_an_explicit_mapped_policy() {
    use super::mappable::{resolve_mapping, MapContext};
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(false), &path, SaveLayout::Auto);
    let file = mappable::MappedFile::open(&path).unwrap();
    let mut ctx = MapContext::host(Some(&file), u64::MAX);
    assert!(resolve_mapping(MapPolicy::Mapped, 0, &ctx));
    ctx.env_disabled = true;
    assert!(!resolve_mapping(MapPolicy::Mapped, 0, &ctx));
}

#[test]
fn from_bytes_never_maps() {
    let mut graph = Arc::new(big_graph(false));
    prepare_kgl_write(&mut graph);
    let mut bytes = Vec::new();
    write_kgl_to(&graph, &mut bytes).unwrap();
    assert_eq!(&bytes[..4], &V9_MAGIC);
    let loaded = load_kgl_bytes_with(
        &bytes,
        &LoadOptions::new().with_embeddings(MapPolicy::Mapped),
    )
    .unwrap();
    assert!(!node_store(&loaded).is_mapped());
    assert_eq!(node_store(&loaded).data, node_store(&graph).data);
}

#[test]
fn compact_layout_writes_v8_bytes_that_load_to_the_same_store() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("compact.kgl");
    let original = save_to(big_graph(true), &path, SaveLayout::Compact);
    assert_eq!(magic(&path), V8_MAGIC);
    let loaded = load_with(&path, MapPolicy::Mapped);
    assert!(
        !node_store(&loaded).is_mapped(),
        "a v8 file has nothing to map"
    );
    assert_eq!(node_store(&loaded).data, node_store(&original).data);
    assert_eq!(node_store(&loaded).norms, node_store(&original).norms);
    assert_eq!(edge_store(&loaded).data, edge_store(&original).data);
}

#[test]
fn the_first_write_promotes_the_written_store_and_leaves_its_clone_mapped() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(false), &path, SaveLayout::Auto);
    let loaded = load_with(&path, MapPolicy::Mapped);
    let store = node_store(&loaded);
    assert!(store.is_mapped());

    // A clone shares the mapping: same address, no copy.
    let mut clone = store.clone();
    assert!(clone.is_mapped());
    assert_eq!(clone.data.as_ptr(), store.data.as_ptr());

    // The clone's write promotes the clone only.
    let replacement = vec![0.5f32; DIM];
    clone.set_embedding(5, &replacement);
    assert!(!clone.is_mapped() && !clone.norms.is_mapped());
    assert!(store.is_mapped() && store.norms.is_mapped());
    let slot = clone.node_to_slot[&5];
    assert_eq!(&clone.data[slot * DIM..(slot + 1) * DIM], &replacement[..]);
    assert_ne!(
        &store.data[slot * DIM..(slot + 1) * DIM],
        &replacement[..],
        "the mapped original is untouched"
    );

    // Append, remove and restore all work on a promoted store.
    let mut grown = store.clone();
    grown.set_embedding(1_000_000, &replacement);
    assert_eq!(grown.len(), store.len() + 1);
    let removed = grown.remove_embedding(7).unwrap();
    grown.restore_embedding(7, &removed);
    assert_eq!(grown.len(), store.len() + 1);
    assert_eq!(grown.get_embedding(7), store.get_embedding(7));
}

#[test]
fn a_mapped_store_saves_back_to_a_v9_file_and_over_its_own_path() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(true), &path, SaveLayout::Auto);
    let mut loaded = load_with(&path, MapPolicy::Mapped);
    let before = node_store(&loaded).data.to_vec();

    // Save over the path the graph is mapped from. The old inode stays
    // readable for the live graph; the new file is complete.
    save_graph_with_layout(&mut loaded, path.to_str().unwrap(), true, SaveLayout::Auto).unwrap();
    assert_eq!(magic(&path), V9_MAGIC);
    assert!(node_store(&loaded).is_mapped());
    assert_eq!(node_store(&loaded).data, before);
    assert!(search(&loaded, &vector_for(3), true).is_ok());

    let reloaded = load_with(&path, MapPolicy::Mapped);
    assert_eq!(node_store(&reloaded).data, before);
    assert_eq!(edge_store(&reloaded).data, edge_store(&loaded).data);
}

#[test]
fn a_corrupt_mapped_section_is_found_at_first_use_and_by_verify_not_at_load() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(false), &path, SaveLayout::Auto);
    corrupt_first_section(&path);

    let report = verify_file(path.to_str().unwrap()).unwrap();
    assert!(!report.is_ok());
    let bad: Vec<_> = report
        .sections
        .iter()
        .filter(|s| s.error.is_some())
        .collect();
    assert_eq!(bad.len(), 1, "{report:?}");
    assert_eq!(bad[0].name, "emb:Doc:text_emb:data");
    assert!(bad[0].mapped);

    // Lazy verification: the load succeeds, the first search refuses.
    let loaded = load_with(&path, MapPolicy::Mapped);
    let error = search(&loaded, &vector_for(1), true).unwrap_err();
    assert!(
        error.contains("corrupt") && error.contains("Doc"),
        "{error}"
    );
    assert!(!verify_graph(&loaded).is_ok());
    // A save refuses to launder the corruption into a new valid file.
    let mut arc = loaded;
    let dest = tmp.path().join("copy.kgl");
    assert!(
        save_graph_with_layout(&mut arc, dest.to_str().unwrap(), true, SaveLayout::Auto).is_err()
    );
    assert!(!dest.exists());

    // Eager verification catches it at load.
    let eager = load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new()
            .with_embeddings(MapPolicy::Mapped)
            .with_verify(Verify::Eager),
    );
    assert!(eager.is_err());
    // A copied (memory) load reads the bytes without a CRC pass, like a
    // numeric column; verify_file is the on-demand check.
    assert!(load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new().with_embeddings(MapPolicy::Memory)
    )
    .is_ok());
}

#[test]
fn an_intact_file_and_graph_verify_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(true), &path, SaveLayout::Auto);
    let report = verify_file(path.to_str().unwrap()).unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert!(report
        .sections
        .iter()
        .any(|s| s.name == "topology" && !s.mapped));
    assert_eq!(report.sections.iter().filter(|s| s.mapped).count(), 4);

    let loaded = load_with(&path, MapPolicy::Mapped);
    let graph_report = verify_graph(&loaded);
    assert!(graph_report.is_ok(), "{graph_report:?}");
    assert_eq!(graph_report.sections.len(), 2);
    assert!(verify_graph(&load_with(&path, MapPolicy::Memory))
        .sections
        .is_empty());
}

#[test]
fn a_truncated_container_fails_at_open_not_at_first_touch() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(false), &path, SaveLayout::Auto);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 1000]).unwrap();
    let error = load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new().with_embeddings(MapPolicy::Mapped),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("truncated"), "{error}");
}

#[test]
fn hnsw_over_a_mapped_store_matches_the_heap_store() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    let mut graph = big_graph(false);
    graph
        .embeddings
        .values_mut()
        .next()
        .unwrap()
        .build_index(
            DistanceMetric::Cosine,
            crate::graph::algorithms::hnsw::HnswParams::default(),
            7,
        )
        .unwrap();
    save_to(graph, &path, SaveLayout::Auto);
    let mapped = load_with(&path, MapPolicy::Mapped);
    let memory = load_with(&path, MapPolicy::Memory);
    assert!(node_store(&mapped).has_index() && node_store(&memory).has_index());
    let query = vector_for(500);
    let from_mapped = search(&mapped, &query, false).unwrap();
    let from_memory = search(&memory, &query, false).unwrap();
    assert_eq!(ranking(&from_mapped), ranking(&from_memory));
    assert_eq!(from_mapped[0].node_idx.index(), 499);
}

#[test]
fn an_unclaimed_section_refuses_the_load() {
    use super::embedding_sections::{KIND_VECTORS, NODE_OWNER};
    use super::mappable::{MappableSection, SectionDtype};
    let graph = Arc::new(big_graph(false));
    // A stray section owned by `embeddings` that no store claims.
    let stray = MappableSection {
        name: "emb:Ghost:x:data".to_string(),
        owner: NODE_OWNER.to_string(),
        kind: KIND_VECTORS,
        dtype: SectionDtype::F32,
        elem_count: 2,
        flags: 0,
        data: std::borrow::Cow::Owned(vec![0u8; 8]),
    };
    let mut bytes = Vec::new();
    super::save_sections::write_kgl_container(&graph, &mut bytes, None, SaveLayout::Auto, {
        let mut sections = mappable::collect_mappable_sections(&graph, SaveLayout::Auto).unwrap();
        sections.push(stray);
        sections
    })
    .unwrap();
    let error = load_kgl_bytes(&bytes).err().unwrap();
    assert!(error.to_string().contains("names no store"), "{error}");
}

#[test]
fn a_delete_through_cypher_promotes_the_live_graph_and_not_its_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(false), &path, SaveLayout::Auto);
    let mut live = load_with(&path, MapPolicy::Mapped);
    let snapshot = Arc::clone(&live);
    assert!(node_store(&snapshot).is_mapped());

    execute_mut(
        Arc::make_mut(&mut live),
        "MATCH (n:Doc {id: 5}) DETACH DELETE n",
        &ExecuteOptions::eager(&HashMap::new()),
    )
    .unwrap();
    assert!(
        !node_store(&live).is_mapped(),
        "the writer's copy is promoted"
    );
    assert_eq!(node_store(&live).len(), DOCS as usize - 1);
    assert!(
        node_store(&snapshot).is_mapped(),
        "the snapshot keeps its mapping"
    );
    assert_eq!(node_store(&snapshot).len(), DOCS as usize);
    // Both still answer, and the deleted node is gone only from the live one.
    let everyone = |graph: &DirGraph| {
        vector_search(
            graph,
            &CurrentSelection::new(),
            "text_emb",
            &vector_for(5),
            &VectorSearchOptions::default()
                .with_top_k(DOCS as usize)
                .with_metric(DistanceMetric::Cosine)
                .with_exact(true),
        )
        .unwrap()
        .iter()
        .map(|r| r.node_idx.index())
        .collect::<Vec<_>>()
    };
    assert_eq!(everyone(&snapshot).len(), DOCS as usize);
    assert_eq!(everyone(&live).len(), DOCS as usize - 1);
    assert!(everyone(&snapshot).contains(&4) && !everyone(&live).contains(&4));
}

#[test]
fn the_windows_policy_releases_a_mapping_of_the_destination_before_a_save() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    save_to(big_graph(true), &path, SaveLayout::Auto);
    let other = tmp.path().join("other.kgl");

    // Not Windows: the mapping is kept (rename replaces the directory entry).
    let mut graph = load_with(&path, MapPolicy::Mapped);
    embedding_sections::release_mappings_of_destination(&mut graph, path.to_str().unwrap(), false);
    assert!(node_store(&graph).is_mapped() && edge_store(&graph).is_mapped());

    // Windows, destination elsewhere: kept.
    embedding_sections::release_mappings_of_destination(&mut graph, other.to_str().unwrap(), true);
    assert!(node_store(&graph).is_mapped());

    // Windows, destination is the mapped file: every store is copied out.
    embedding_sections::release_mappings_of_destination(&mut graph, path.to_str().unwrap(), true);
    assert!(!node_store(&graph).is_mapped() && !node_store(&graph).norms.is_mapped());
    assert!(!edge_store(&graph).is_mapped());
    assert!(embedding_sections::mapped_sources(&graph).is_empty());
}
