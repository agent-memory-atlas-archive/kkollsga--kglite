//! Derived indexes in a v9 container: which saves record them, what a mapped
//! load serves from the file, and that a graph loaded that way answers exactly
//! as one that built the same indexes on the heap, through writes, a rollback
//! and a save-and-reload.

use super::index_sections::{with_floor, KIND_ID_KEYS, KIND_ID_NODES, KIND_TYPE_MEMBERS, OWNER};
use super::mappable::SaveLayout;
use super::*;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use crate::graph::storage::disk::type_index::TypeNodesRef;
use std::collections::HashMap;

const DOCS: i64 = 120;

fn opts() -> ExecuteOptions<'static> {
    // A leaked empty map keeps the options 'static for the whole test binary.
    let empty: &'static HashMap<String, Value> = Box::leak(Box::new(HashMap::new()));
    ExecuteOptions::eager(empty)
}

fn run(graph: &mut DirGraph, query: &str) {
    execute_mut(graph, query, &opts()).unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn rows(graph: &DirGraph, query: &str) -> String {
    format!(
        "{:?}",
        execute_read(graph, query, &opts())
            .unwrap_or_else(|e| panic!("{query}: {e}"))
            .result
            .rows
    )
}

/// A type whose ids are `UniqueId`, as a DataFrame load stores them.
fn add_unique_id_type(graph: &mut DirGraph, node_type: &str, ids: impl Iterator<Item = u32>) {
    use crate::datatypes::DataFrame;
    let rows: Vec<Vec<Value>> = ids
        .map(|id| {
            vec![
                Value::UniqueId(id),
                Value::String(format!("{node_type}-{id}")),
                Value::Int64(i64::from(id) * 2),
            ]
        })
        .collect();
    let frame = DataFrame::from_cypher_rows(
        vec!["id".to_string(), "title".to_string(), "n".to_string()],
        rows,
    )
    .expect("frame");
    crate::graph::mutation::maintain::add_nodes(
        graph,
        frame,
        node_type.to_string(),
        "id".to_string(),
        Some("title".to_string()),
        None,
    )
    .expect("add_nodes");
}

/// `Doc` has unique int64 ids (so it gets an id section), `Uni` unique
/// `UniqueId` ids (a `u32`-keyed one), `Mix` a `UniqueId` column a Cypher
/// `CREATE` appended to, `Tag` string ids,
/// `Twin` repeated int64 ids, and `A:b%c` an int64 type whose name needs
/// escaping in a section name. A deleted `Doc` leaves a hole in the node array.
fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    add_unique_id_type(&mut graph, "Uni", (0..90).map(|i| 500 + i * 2));
    add_unique_id_type(&mut graph, "Mix", 1..40);
    run(&mut graph, "CREATE (:Mix {id: 99999, n: 0})");
    run(
        &mut graph,
        &format!("UNWIND range(1, {DOCS}) AS i CREATE (:Doc {{id: 1000 + i * 3, n: i}})"),
    );
    run(
        &mut graph,
        "UNWIND range(1, 20) AS i CREATE (:Tag {id: 'tag-' + toString(i), n: i})",
    );
    run(
        &mut graph,
        "UNWIND range(1, 30) AS i CREATE (:Twin {id: i % 5, n: i})",
    );
    run(
        &mut graph,
        "UNWIND range(1, 25) AS i CREATE (:`A:b%c` {id: 7000 - i, n: i})",
    );
    run(
        &mut graph,
        &format!(
            "UNWIND range(1, {DOCS}) AS i MATCH (a:Doc {{id: 1000 + i * 3}}), \
             (b:Doc {{id: 1000 + ((i * 7) % {DOCS} + 1) * 3}}) CREATE (a)-[:LINKS]->(b)"
        ),
    );
    run(
        &mut graph,
        &format!(
            "UNWIND range(1, {DOCS}) AS i MATCH (a:Doc {{id: 1000 + i * 3}}), \
             (t:Tag {{id: 'tag-' + toString(i % 20 + 1)}}) CREATE (a)-[:TAGS]->(t)"
        ),
    );
    run(&mut graph, "MATCH (d:Doc {id: 1030}) DETACH DELETE d");
    graph
}

/// Queries that touch every served structure: id lookups, type scans, typed
/// and grouped edge aggregates in both directions.
const PROBES: [&str; 16] = [
    "MATCH (d:Doc {id: 1015}) RETURN d.n",
    "MATCH (d:Doc {id: 1030}) RETURN d.n",
    "MATCH (d:Doc {id: 1360}) RETURN d.n",
    "MATCH (d:Doc) RETURN count(d), sum(d.n), min(d.id), max(d.id)",
    "MATCH (t:Tag {id: 'tag-3'}) RETURN t.n",
    "MATCH (t:Twin {id: 3}) RETURN count(t)",
    "MATCH (u:Uni {id: 508}) RETURN u.n",
    "MATCH (u:Uni {id: 509}) RETURN u.n",
    "MATCH (u:Uni) WHERE u.id > 600 RETURN count(u), sum(u.n)",
    "MATCH (m:Mix {id: 17}) RETURN m.n",
    "MATCH (m:Mix {id: 99999}) RETURN m.n",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) RETURN b.id AS i, count(a) AS c ORDER BY c DESC, i LIMIT 7",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) RETURN a.id AS i, count(b) AS c ORDER BY c DESC, i LIMIT 7",
    "MATCH (a:Doc)-[:TAGS]->(t:Tag) RETURN t.id AS i, count(a) AS c ORDER BY c DESC, i LIMIT 5",
    "MATCH (a:Doc {id: 1006})-[:LINKS]->(b)-[:TAGS]->(t) RETURN count(t)",
    "MATCH (a:Doc)-[:LINKS]->(b:Doc) RETURN count(*)",
];

/// The probes' answers plus an id lookup in the type Cypher cannot spell.
fn probe(graph: &DirGraph) -> Vec<String> {
    let mut out: Vec<String> = PROBES.iter().map(|q| rows(graph, q)).collect();
    for id in [6990, 6975, 6974, 0] {
        out.push(format!(
            "{:?}",
            graph.lookup_by_id_readonly("A:b%c", &Value::Int64(id))
        ));
    }
    out
}

fn save(graph: DirGraph, path: &std::path::Path, layout: SaveLayout) -> Arc<DirGraph> {
    let mut arc = Arc::new(graph);
    save_graph_with_layout(&mut arc, path.to_str().unwrap(), true, layout).unwrap();
    arc
}

fn load(path: &std::path::Path, storage: StorageMode) -> Arc<DirGraph> {
    load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new().with_storage(storage),
    )
    .unwrap()
}

/// The graph a load returned, owned: its backend is the loaded one, not an
/// overlay over it (which is what cloning the graph would give).
fn load_owned(path: &std::path::Path, storage: StorageMode) -> DirGraph {
    match Arc::try_unwrap(load(path, storage)) {
        Ok(graph) => graph,
        Err(_) => panic!("a fresh load is uniquely owned"),
    }
}

fn saved(layout: SaveLayout) -> (tempfile::TempDir, std::path::PathBuf, Arc<DirGraph>) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("g.kgl");
    let original = with_floor(50, || save(graph(), &path, layout));
    (tmp, path, original)
}

fn section_names(path: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    metadata.mappable.iter().map(|e| e.name.clone()).collect()
}

fn type_is_mapped(graph: &DirGraph, name: &str) -> bool {
    matches!(
        graph.type_indices.get(name),
        Some(TypeNodesRef::Mmap(_)) | Some(TypeNodesRef::MmapLayered { .. })
    )
}

#[test]
fn a_large_graph_records_its_indexes_and_a_small_one_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let small = tmp.path().join("small.kgl");
    save(graph(), &small, SaveLayout::Auto);
    assert_eq!(
        &std::fs::read(&small).unwrap()[..4],
        &V8_MAGIC,
        "a graph under the floors stays a v8 container"
    );

    let (_tmp, path, _) = saved(SaveLayout::Auto);
    assert_eq!(&std::fs::read(&path).unwrap()[..4], &V9_MAGIC);
    let names = section_names(&path);
    for expected in [
        "idx:type:Doc",
        "idx:type:Tag",
        "idx:type:Twin",
        "idx:type:A%3Ab%25c",
        "idx:id:Doc:keys",
        "idx:id:Doc:nodes",
        "idx:id:Uni:keys",
        "idx:id:Uni:nodes",
        "idx:id:A%3Ab%25c:keys",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
    assert!(
        names.iter().all(|n| !["idx:id:Tag", "idx:id:Twin"]
            .iter()
            .any(|p| n.starts_with(p))),
        "string and repeated ids are left to the lazy build: {names:?}"
    );

    let compact = tmp.path().join("compact.kgl");
    with_floor(50, || save(graph(), &compact, SaveLayout::Compact));
    assert_eq!(&std::fs::read(&compact).unwrap()[..4], &V8_MAGIC);
}

#[test]
fn saving_twice_writes_the_same_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a.kgl"), tmp.path().join("b.kgl"));
    with_floor(50, || {
        save(graph(), &a, SaveLayout::Auto);
        save(graph(), &b, SaveLayout::Auto);
    });
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
}

#[test]
fn a_mapped_load_serves_the_recorded_indexes_and_answers_like_the_original() {
    let (_tmp, path, original) = saved(SaveLayout::Auto);
    let mapped = load(&path, StorageMode::Mapped);

    for name in ["Doc", "Tag", "Twin", "A:b%c"] {
        assert!(
            type_is_mapped(&mapped, name),
            "{name} members are in the file"
        );
    }
    assert!(
        mapped.id_indices.contains_key("Doc") && mapped.id_indices.overlay_len("Doc").is_none(),
        "the Doc id index is served from the file, with no heap copy"
    );
    assert!(mapped.id_indices.contains_key("A:b%c"));
    assert!(
        mapped.id_indices.contains_key("Uni") && mapped.id_indices.overlay_len("Uni").is_none(),
        "a UniqueId index is served from the file too"
    );
    assert!(
        mapped.id_indices.contains_key("Mix"),
        "a row appended to a UniqueId column keeps that spelling"
    );
    assert!(
        !mapped.id_indices.contains_key("Tag") && !mapped.id_indices.contains_key("Twin"),
        "types the file recorded no id index for are built on first use"
    );

    assert_eq!(probe(&mapped), probe(&original));
    let memory = load(&path, StorageMode::Memory);
    assert_eq!(probe(&mapped), probe(&memory));
    assert_eq!(
        rows(&mapped, "MATCH (d:Doc {id: 1030}) RETURN d.n"),
        "[]",
        "the deleted Doc's id is absent from the recorded index"
    );
}

#[test]
fn a_memory_load_ignores_the_recorded_indexes() {
    let (_tmp, path, original) = saved(SaveLayout::Auto);
    let memory = load(&path, StorageMode::Memory);
    assert!(matches!(
        memory.type_indices.get("Doc"),
        Some(TypeNodesRef::Overlay(_))
    ));
    assert!(!memory.id_indices.contains_key("Doc"));
    assert_eq!(probe(&memory), probe(&original));
}

#[test]
fn a_v8_file_loads_into_mapped_storage_as_before() {
    let (_tmp, path, original) = saved(SaveLayout::Compact);
    assert_eq!(&std::fs::read(&path).unwrap()[..4], &V8_MAGIC);
    let mapped = load(&path, StorageMode::Mapped);
    assert!(!type_is_mapped(&mapped, "Doc"));
    assert!(!mapped.id_indices.contains_key("Doc"));
    assert_eq!(probe(&mapped), probe(&original));
}

/// The same write sequence on a file-served mapped graph and on a memory graph
/// that built every index itself must leave them answering identically after
/// each step, including a rolled-back statement and a save-and-reload.
/// `overlay` runs the writes through a copy-on-write overlay over the loaded
/// backend (what a held reader gives the writer) instead of in place.
fn writes_rollback_and_resave_match(overlay: bool) {
    let (tmp, path, _) = saved(SaveLayout::Auto);
    let mut mapped = load_owned(&path, StorageMode::Mapped);
    let mut memory = load_owned(&path, StorageMode::Memory);
    if overlay {
        mapped = mapped.clone();
        memory = memory.clone();
        assert!(mapped.graph.is_forked(), "the overlay must be in play");
    }
    assert!(type_is_mapped(&mapped, "Doc"));

    let steps = [
        "CREATE (:Doc {id: 5000, n: 5000})",
        "MATCH (d:Doc {id: 1015}) SET d.n = 111",
        "MATCH (d:Doc {id: 1006}) DETACH DELETE d",
        "MATCH (a:Doc {id: 1357}), (b:Doc {id: 1360}) CREATE (a)-[:LINKS]->(b)",
        "MATCH (a:Doc {id: 1009})-[r:LINKS]->() DELETE r",
        "CREATE (:Doc {id: 1006, n: 1})",
        "MATCH (d:Doc {id: 1360}) MERGE (m:Doc {id: 6000}) ON CREATE SET m.n = 6",
        "UNWIND range(1, 3) AS i CREATE (:Twin {id: 100 + i, n: i})",
        "CREATE (:Uni {id: 509, n: 509})",
        "MATCH (u:Uni {id: 508}) DETACH DELETE u",
    ];
    let links = |g: &DirGraph| rows(g, "MATCH ()-[r:LINKS]->() RETURN count(r)");
    let links_before = links(&mapped);
    for step in steps {
        run(&mut mapped, step);
        run(&mut memory, step);
        assert_eq!(probe(&mapped), probe(&memory), "after: {step}");
        for q in [
            "MATCH (d:Doc {id: 5000}) RETURN d.n",
            "MATCH (d:Doc {id: 6000}) RETURN d.n",
            "MATCH (d:Doc {id: 1015}) RETURN d.n",
            "MATCH (d:Doc) RETURN count(d), sum(d.n)",
            "MATCH (u:Uni {id: 509}) RETURN u.n",
            "MATCH (u:Uni {id: 508}) RETURN u.n",
        ] {
            assert_eq!(rows(&mapped, q), rows(&memory, q), "{q} after: {step}");
        }
    }

    assert_ne!(
        links(&mapped),
        links_before,
        "the edge steps must change the graph"
    );
    let before = probe(&mapped);
    let failed = execute_mut(
        &mut mapped,
        "CREATE (:Doc {id: 9001, n: 1}), \
         (:Doc {id: 9002, bad: duration({months: 2147483648})})",
        &opts(),
    );
    assert!(failed.is_err(), "the statement must fail so it rolls back");
    assert_eq!(probe(&mapped), before, "rollback restores every answer");
    assert_eq!(rows(&mapped, "MATCH (d:Doc {id: 9001}) RETURN d.n"), "[]");

    let resaved = tmp.path().join("resaved.kgl");
    let mapped = save(mapped, &resaved, SaveLayout::Auto);
    let reloaded = load(&resaved, StorageMode::Mapped);
    assert_eq!(probe(&reloaded), probe(&memory));
    assert_eq!(probe(&reloaded), probe(&mapped));
    assert_eq!(
        rows(&reloaded, "MATCH (d:Doc {id: 6000}) RETURN d.n"),
        rows(&memory, "MATCH (d:Doc {id: 6000}) RETURN d.n")
    );
}

#[test]
fn writes_rollback_and_resave_match_a_graph_that_built_its_own_indexes() {
    writes_rollback_and_resave_match(false);
}

#[test]
fn writes_through_an_overlay_match_a_graph_that_built_its_own_indexes() {
    writes_rollback_and_resave_match(true);
}

fn flip_in_section(path: &std::path::Path, name: &str) {
    let mut bytes = std::fs::read(path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    let end = 13 + u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let base = data_region_base(&metadata, end, bytes.len() as u64).unwrap();
    let entry = metadata.mappable.iter().find(|e| e.name == name).unwrap();
    bytes[(base + entry.offset) as usize + entry.len as usize / 2] ^= 0x40;
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_damaged_index_section_refuses_a_mapped_load() {
    for name in [
        "idx:type:Doc",
        "idx:id:Doc:keys",
        "idx:id:Doc:nodes",
        "idx:id:Uni:keys",
        "idx:id:Uni:nodes",
    ] {
        let (_tmp, path, _) = saved(SaveLayout::Auto);
        flip_in_section(&path, name);
        let error = load_file_with(
            path.to_str().unwrap(),
            &LoadOptions::new().with_storage(StorageMode::Mapped),
        )
        .err()
        .unwrap_or_else(|| panic!("a flipped byte in {name} must refuse the mapped load"));
        assert!(error.to_string().contains("corrupt"), "{name}: {error}");

        let verify = verify_file(path.to_str().unwrap()).unwrap();
        assert!(
            verify
                .sections
                .iter()
                .any(|s| s.name == name && s.error.is_some()),
            "verify_file names {name}"
        );
    }
}

#[test]
fn a_truncated_index_region_is_refused_at_open() {
    let (_tmp, path, _) = saved(SaveLayout::Auto);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() - 100]).unwrap();
    for storage in [StorageMode::Mapped, StorageMode::Memory] {
        let error = load_file_with(
            path.to_str().unwrap(),
            &LoadOptions::new().with_storage(storage),
        )
        .err()
        .expect("a truncated file is refused");
        assert!(error.to_string().contains("truncated"), "{error}");
    }
}

#[test]
fn sections_that_disagree_with_the_topology_are_not_installed() {
    let (_tmp, path, _) = saved(SaveLayout::Auto);
    let bytes = std::fs::read(&path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    let end = 13 + u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let base = data_region_base(&metadata, end, bytes.len() as u64).unwrap();
    let table =
        mappable::MappableTable::open(&metadata.mappable, &bytes, base, None, Verify::Lazy, |_| {
            false
        })
        .unwrap();
    let mut sections = index_sections::IndexSections::from_table(&table);
    assert!(sections.has_types());

    let mut graph = (*load(&path, StorageMode::Memory)).clone();
    run(&mut graph, "MATCH (d:Doc {id: 1006}) DETACH DELETE d");
    let before = rows(&graph, "MATCH (d:Doc) RETURN count(d)");
    assert!(
        !sections.install_node_indexes(&mut graph).unwrap(),
        "a member count that disagrees with the node count is not installed"
    );
    assert!(matches!(
        graph.type_indices.get("Doc"),
        Some(TypeNodesRef::Overlay(_))
    ));
    assert_eq!(rows(&graph, "MATCH (d:Doc) RETURN count(d)"), before);
}

/// A disk-mode save of a graph whose indexes came from `.kgl` sections writes
/// the same lookups and member lists into `id_indices.bin` / `type_indices.bin`.
#[test]
fn section_bases_write_the_disk_index_files_they_serve() {
    use crate::graph::storage::disk::id_index::{write_id_indices_bin, IdIndexBase};
    use crate::graph::storage::disk::type_index::{write_type_indices_bin, TypeIndexBase};

    let (_tmp, path, _) = saved(SaveLayout::Auto);
    let mapped = load(&path, StorageMode::Mapped);
    let dir = tempfile::tempdir().unwrap();
    write_id_indices_bin(
        dir.path(),
        &mapped.id_indices,
        &mapped.type_indices,
        &mapped.interner,
    )
    .unwrap();
    write_type_indices_bin(dir.path(), &mapped.type_indices, &mapped.interner).unwrap();
    let ids = IdIndexBase::load_from(dir.path(), &mapped.interner)
        .unwrap()
        .expect("id_indices.bin was written");
    let types = TypeIndexBase::load_from(dir.path(), &mapped.interner)
        .unwrap()
        .expect("type_indices.bin was written");

    for (node_type, id) in [
        ("Doc", Value::Int64(1015)),
        ("Doc", Value::Int64(1360)),
        ("Uni", Value::UniqueId(508)),
        ("A:b%c", Value::Int64(6990)),
    ] {
        let served = mapped.id_indices.lookup(node_type, &id);
        assert!(served.is_some(), "{node_type} {id:?}");
        assert_eq!(ids.lookup(node_type, &id), served, "{node_type} {id:?}");
    }
    assert_eq!(ids.lookup("Doc", &Value::Int64(1030)), None);
    for node_type in ["Doc", "Uni", "Tag", "Twin", "A:b%c"] {
        let Some(TypeNodesRef::Mmap(served)) = mapped.type_indices.get(node_type) else {
            panic!("{node_type} members are served from the file");
        };
        assert_eq!(types.slice_for(node_type), Some(served), "{node_type}");
    }
}

#[test]
fn every_index_section_carries_the_index_owner_and_a_known_kind() {
    let (_tmp, path, _) = saved(SaveLayout::Auto);
    let bytes = std::fs::read(&path).unwrap();
    let metadata = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    let index: Vec<_> = metadata
        .mappable
        .iter()
        .filter(|e| e.name.starts_with("idx:"))
        .collect();
    assert!(!index.is_empty());
    assert!(index
        .iter()
        .all(|e| e.owner == OWNER
            && [KIND_TYPE_MEMBERS, KIND_ID_KEYS, KIND_ID_NODES].contains(&e.kind)));
}
