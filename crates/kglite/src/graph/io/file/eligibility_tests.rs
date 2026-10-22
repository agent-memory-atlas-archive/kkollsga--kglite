//! What a graph that grows through writes saves and reloads as: no duplicated
//! columns, no dropped index sections, no lost tombstones.

use super::frozen_tests::{dump, section_names};
use super::index_sections::with_floors;
use super::index_sections_tests::{add_unique_id_type, load, load_owned, rows, run, save};
use super::mappable::SaveLayout;
use super::*;

/// Schema key names per node type, sorted: the columns a type's store carries.
fn schema_keys(graph: &DirGraph) -> std::collections::BTreeMap<String, Vec<String>> {
    graph
        .type_schemas
        .iter()
        .map(|(node_type, schema)| {
            let mut names: Vec<String> = schema
                .iter()
                .map(|(_, key)| graph.interner.resolve(key).to_string())
                .collect();
            names.sort();
            (node_type.clone(), names)
        })
        .collect()
}

fn column_sections(path: &std::path::Path) -> Vec<String> {
    section_names(path)
        .into_iter()
        .filter(|name| name.starts_with("col:"))
        .collect()
}

fn save_to(graph: &mut Arc<DirGraph>, path: &std::path::Path) {
    with_floors(10, 10, || {
        save_graph_with_layout(graph, path.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
}

/// Rows of the frame fixture: enough that a `u32` id column and a title offset
/// column each pass the 256 KiB floor and are written as sections.
const FRAME_DOCS: u32 = 70_000;

/// A DataFrame-loaded type (`UniqueId` ids, a title, an int column) with
/// edges, saved with raw sections.
fn saved_frame_graph() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("frame.kgl");
    let mut graph = DirGraph::new();
    add_unique_id_type(&mut graph, "Doc", 0..FRAME_DOCS);
    run(
        &mut graph,
        "UNWIND range(0, 299) AS i MATCH (a:Doc {id: i}), (b:Doc {id: (i * 7) % 300}) CREATE (a)-[:L]->(b)",
    );
    with_floors(10, 10, || save(graph, &path, SaveLayout::Auto));
    (tmp, path)
}

#[test]
fn a_delete_on_a_mapped_load_saves_no_duplicate_id_or_title_columns() {
    let (tmp, path) = saved_frame_graph();
    let before = schema_keys(&load_owned(&path, StorageMode::Memory));
    let mut graph = load(&path, StorageMode::Mapped);
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        run(writer, "MATCH (b:Doc {id: 21}) DETACH DELETE b");
    }
    let saved = tmp.path().join("deleted.kgl");
    save_to(&mut graph, &saved);
    let after = schema_keys(&load_owned(&saved, StorageMode::Memory));
    assert_eq!(after, before, "a delete adds no column to any type");
    assert_eq!(
        column_sections(&saved),
        column_sections(&path),
        "a delete adds no column section"
    );
    let reloaded = load_owned(&saved, StorageMode::Mapped);
    let mut reference = load_owned(&path, StorageMode::Memory);
    run(&mut reference, "MATCH (b:Doc {id: 21}) DETACH DELETE b");
    assert_eq!(dump(&reloaded), dump(&reference));
}

#[test]
fn an_int_id_created_over_a_unique_id_type_widens_like_memory() {
    let (tmp, path) = saved_frame_graph();
    let mut mapped = load_owned(&path, StorageMode::Mapped);
    let mut memory = load_owned(&path, StorageMode::Memory);
    for graph in [&mut mapped, &mut memory] {
        run(graph, "CREATE (:Doc {id: 1000000, n: 1})");
    }
    assert!(mapped.graph.is_forked(), "the write is an overlay");
    assert_eq!(dump(&mapped), dump(&memory), "every id reads the same");
    let saved = tmp.path().join("grown.kgl");
    let mut graph = Arc::new(mapped);
    save_to(&mut graph, &saved);
    let names = section_names(&saved);
    assert!(
        names.iter().any(|n| n.starts_with("idx:id:Doc")),
        "the widened id column keeps its id-index sections: {names:?}"
    );
    assert_eq!(
        dump(&load_owned(&saved, StorageMode::Mapped)),
        dump(&memory)
    );
}

/// Rows every type's store holds, tombstoned ones included.
fn stored_rows(graph: &DirGraph) -> usize {
    graph
        .graph
        .column_stores_iter()
        .map(|(_, store)| store.row_count() as usize)
        .sum()
}

const DELETES: [&str; 5] = [
    "UNWIND range(100000, 100019) AS i CREATE (:Doc {id: i, n: i})",
    "MATCH (d:Doc) WHERE d.id IN [3, 100, 299] DETACH DELETE d",
    "MATCH (d:Doc) WHERE d.id IN [100001, 100005, 100019] DETACH DELETE d",
    "MATCH (d:Doc {id: 7}) SET d.n = 777",
    "MATCH (a:Doc {id: 100002}), (b:Doc {id: 8}) CREATE (a)-[:L]->(b)",
];

#[test]
fn deleted_base_and_appended_rows_do_not_reload_after_a_save() {
    let (tmp, path) = saved_frame_graph();
    let mut graph = load(&path, StorageMode::Mapped);
    let mut reference = load_owned(&path, StorageMode::Memory);
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        for step in DELETES {
            run(writer, step);
        }
    }
    for step in DELETES {
        run(&mut reference, step);
    }
    assert_eq!(
        dump(&graph),
        dump(&reference),
        "the overlay reads as memory"
    );
    let saved = tmp.path().join("deleted_rows.kgl");
    save_to(&mut graph, &saved);
    let live = FRAME_DOCS as usize + 20 - 6;
    let mapped = load_owned(&saved, StorageMode::Mapped);
    let memory = load_owned(&saved, StorageMode::Memory);
    for reloaded in [&mapped, &memory] {
        assert_eq!(dump(reloaded), dump(&reference));
        assert_eq!(
            stored_rows(reloaded),
            live,
            "no row of a deleted node is written"
        );
    }
    assert_eq!(dump(&graph), dump(&reference), "the rebased graph agrees");
    for gone in ["3", "100", "299", "100001", "100005", "100019"] {
        let found = rows(
            &mapped,
            &format!("MATCH (d:Doc {{id: {gone}}}) RETURN count(d)"),
        );
        assert!(found.contains("Int64(0)"), "{gone} is gone: {found}");
    }
}

#[test]
fn a_repeated_id_keeps_the_id_index_and_the_node_memory_answers_with() {
    let (tmp, path) = saved_frame_graph();
    let mut graph = load(&path, StorageMode::Mapped);
    let mut reference = load_owned(&path, StorageMode::Memory);
    let steps = [
        "CREATE (:Doc {id: 5, n: 555})",
        "CREATE (:Doc {id: 69999, n: 1}), (:Doc {id: 69999, n: 2})",
    ];
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        for step in steps {
            run(writer, step);
        }
    }
    for step in steps {
        run(&mut reference, step);
    }
    let saved = tmp.path().join("repeated.kgl");
    save_to(&mut graph, &saved);
    let names = section_names(&saved);
    assert!(
        names.iter().any(|n| n == "idx:id:Doc:keys")
            && names.iter().any(|n| n == "idx:id:Doc:shadowed"),
        "the index is written, with the nodes it does not serve named: {names:?}"
    );
    let mapped = load_owned(&saved, StorageMode::Mapped);
    let memory = load_owned(&saved, StorageMode::Memory);
    assert!(
        mapped.id_indices.contains_key("Doc"),
        "served from the file"
    );
    for id in [5, 69999, 6, 70_001] {
        let q = format!("MATCH (d:Doc {{id: {id}}}) RETURN d.n");
        let expected = rows(&reference, &q);
        assert_eq!(rows(&mapped, &q), expected, "mapped id {id}");
        assert_eq!(rows(&memory, &q), expected, "memory id {id}");
    }
}

#[test]
fn a_property_spelled_id_or_title_beside_declared_fields_survives_a_delete_and_save() {
    use crate::datatypes::DataFrame;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("aliased.kgl");
    let mut graph = DirGraph::new();
    let cells: Vec<Vec<Value>> = (0..300u32)
        .map(|i| {
            vec![
                Value::UniqueId(i),
                Value::String(format!("name-{i}")),
                Value::Int64(i64::from(i) * 3),
                Value::String(format!("plain-{i}")),
            ]
        })
        .collect();
    let columns = ["code", "name", "id", "title"].map(String::from).to_vec();
    let frame = DataFrame::from_cypher_rows(columns, cells).expect("frame");
    crate::graph::mutation::maintain::add_nodes(
        &mut graph,
        frame,
        "Doc".to_string(),
        "code".to_string(),
        Some("name".to_string()),
        None,
    )
    .expect("add_nodes");
    with_floors(10, 10, || save(graph, &path, SaveLayout::Auto));

    let mut mapped = load(&path, StorageMode::Mapped);
    let mut reference = load_owned(&path, StorageMode::Memory);
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut mapped);
        run(writer, "MATCH (d:Doc) WHERE d.code = 17 DETACH DELETE d");
    }
    run(
        &mut reference,
        "MATCH (d:Doc) WHERE d.code = 17 DETACH DELETE d",
    );
    let saved = tmp.path().join("aliased_deleted.kgl");
    save_to(&mut mapped, &saved);
    let reloaded = load_owned(&saved, StorageMode::Memory);
    assert_eq!(dump(&reloaded), dump(&reference));
    assert_eq!(
        schema_keys(&reloaded),
        schema_keys(&load_owned(&path, StorageMode::Memory)),
        "the delete adds no column"
    );
}

fn section_entry(path: &std::path::Path, name: &str) -> mappable::MapEntry {
    let bytes = std::fs::read(path).unwrap();
    let head = metadata_head::read_metadata_head(&bytes, "test").unwrap();
    head.mappable
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("{name} in {:?}", section_names(path)))
        .clone()
}

/// `n` edges, every one with a property, between two nodes of a frame-loaded
/// type, plus the frame itself.
fn graph_with_property_edges(n: usize) -> DirGraph {
    let mut graph = DirGraph::new();
    add_unique_id_type(&mut graph, "Doc", 0..400);
    run(
        &mut graph,
        &format!(
            "MATCH (a:Doc {{id: 1}}), (b:Doc {{id: 2}}) \
             UNWIND range(1, {n}) AS i CREATE (a)-[:L {{w: i, tag: 'edge'}}]->(b)"
        ),
    );
    graph
}

#[test]
fn a_graph_with_many_property_edges_keeps_its_mapped_topology() {
    // One edge more than the cap this replaced.
    let n = 262_145;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("weighted.kgl");
    with_floors(10, 10, || {
        save(graph_with_property_edges(n), &path, SaveLayout::Auto)
    });
    let names = section_names(&path);
    assert!(
        names.iter().any(|name| name == "topo:edges"),
        "topology sections are written: {names:?}"
    );
    let props = section_entry(&path, "topo:edge_props");
    assert_eq!(props.kind, 19, "the properties are compressed");
    assert!(
        props.len < 4 * n as u64,
        "{n} edges compress to {}",
        props.len
    );

    let mapped = load_owned(&path, StorageMode::Mapped);
    assert!(mapped.graph.is_frozen(), "served from the file");
    let memory = load_owned(&path, StorageMode::Memory);
    for q in [
        "MATCH ()-[r:L]->() RETURN count(r), sum(r.w), min(r.tag)",
        "MATCH (a:Doc {id: 1})-[r:L]->(b) WHERE r.w > 262140 RETURN r.w ORDER BY r.w",
    ] {
        assert_eq!(rows(&mapped, q), rows(&memory, q), "{q}");
    }
    assert!(
        mapped.graph_info().topology_heap_bytes > 0,
        "properties are heap"
    );
}

#[test]
fn compressed_and_uncompressed_edge_properties_both_load() {
    let tmp = tempfile::tempdir().unwrap();
    let (zstd_path, raw_path) = (tmp.path().join("z.kgl"), tmp.path().join("r.kgl"));
    with_floors(10, 10, || {
        save(
            graph_with_property_edges(3000),
            &zstd_path,
            SaveLayout::Auto,
        );
        topology_sections::with_raw_edge_props(|| {
            save(graph_with_property_edges(3000), &raw_path, SaveLayout::Auto)
        });
    });
    let (zstd_entry, raw_entry) = (
        section_entry(&zstd_path, "topo:edge_props"),
        section_entry(&raw_path, "topo:edge_props"),
    );
    assert_eq!((zstd_entry.kind, raw_entry.kind), (19, 17));
    assert!(
        zstd_entry.len * 4 < raw_entry.len,
        "{} vs {}",
        zstd_entry.len,
        raw_entry.len
    );
    let reference = load_owned(&raw_path, StorageMode::Memory);
    for path in [&zstd_path, &raw_path] {
        let mapped = load_owned(path, StorageMode::Mapped);
        assert!(mapped.graph.is_frozen());
        assert_eq!(dump(&mapped), dump(&reference));
    }
}

/// `types` node types of one node each and `types` connection types of one
/// edge each, over a chain, in one statement.
fn graph_with_many_types(types: usize) -> DirGraph {
    let mut graph = DirGraph::new();
    let nodes: Vec<String> = (0..types)
        .map(|i| format!("(n{i}:T{i} {{id: {i}, n: {}}})", i % 7))
        .collect();
    let edges: Vec<String> = (1..types)
        .map(|i| format!("(n{})-[:R{}]->(n{i})", i - 1, i - 1))
        .collect();
    run(
        &mut graph,
        &format!("CREATE {}, {}", nodes.join(", "), edges.join(", ")),
    );
    graph
}

#[test]
fn a_graph_with_thousands_of_types_keeps_its_mapped_topology() {
    let types = 2_000;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("types.kgl");
    with_floors(10, 10, || {
        save(graph_with_many_types(types), &path, SaveLayout::Auto)
    });
    let names = section_names(&path);
    assert!(
        names.iter().any(|name| name == "topo:out_ty"),
        "more than 1,024 node and connection types still record the topology"
    );
    let mapped = load_owned(&path, StorageMode::Mapped);
    let memory = load_owned(&path, StorageMode::Memory);
    assert!(mapped.graph.is_frozen());
    assert_eq!(dump(&mapped), dump(&memory));
    for i in [0, 31, 32, 33, 1023, 1024, 1500, 1998] {
        let q = format!("MATCH (a)-[r:R{i}]->(b) RETURN a.id, b.id, type(r)");
        assert_eq!(rows(&mapped, &q), rows(&memory, &q), "{q}");
        let miss = format!("MATCH (a:T{})-[r:R{i}]->(b) RETURN count(r)", i + 1);
        assert_eq!(rows(&mapped, &miss), rows(&memory, &miss), "{miss}");
    }
    assert_eq!(
        rows(&mapped, "MATCH ()-[r]->() RETURN count(r)"),
        rows(&memory, "MATCH ()-[r]->() RETURN count(r)")
    );
}

#[test]
fn a_sustained_commit_loop_never_flattens_the_id_index_into_the_heap() {
    use crate::graph::session::Session;
    use crate::graph::storage::disk::id_index_layer::flattens;
    let (_tmp, path) = saved_frame_graph();
    let session = Session::from_arc(load(&path, StorageMode::Mapped));
    // The published graph has a second holder after every commit, as it does
    // under a binding that keeps its own handle; publication cannot fold it.
    let mut handle = session.snapshot();
    let before = flattens();
    for i in 0..200 {
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            &format!("CREATE (:Doc {{id: {}, n: 1}})", 900_000 + i),
        );
        session.commit(tx, true);
        handle = session.snapshot();
    }
    assert_eq!(
        flattens(),
        before,
        "no commit paid O(type) for the id index"
    );
    let found = rows(&handle, "MATCH (d:Doc {id: 900199}) RETURN d.n");
    assert!(found.contains("Int64(1)"), "{found}");
    let old = rows(&handle, "MATCH (d:Doc {id: 4}) RETURN d.n");
    assert!(old.contains("Int64(8)"), "{old}");
}
