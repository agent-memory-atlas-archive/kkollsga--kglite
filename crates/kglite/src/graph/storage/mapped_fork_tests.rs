//! A `Mapped` graph forks through the copy-on-write overlay exactly as a
//! `Memory` one does: the first write under a held reader copies no nodes, the
//! graph keeps reporting as mapped, and folding the overlay back leaves no
//! lazy index answering for the pre-fold graph.

use std::collections::HashMap;
use std::sync::Arc;

use petgraph::graph::NodeIndex;
use petgraph::Direction;

use super::mapped_property_index_tests::{ids, mapped_row_storage_people, run};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::handle::make_dir_graph_mut;
use crate::graph::schema::InternedKey;
use crate::graph::session::{execute_mut, ExecuteOptions};
use crate::graph::storage::backend::{backend_clone_nodes, reset_backend_clone_count};
use crate::graph::storage::mode::{live_storage_mode, new_dir_graph_in_mode, StorageMode};
use crate::graph::storage::GraphRead;

const FIXTURE_NODES: usize = 10;

fn seeded_mapped_arc() -> Arc<DirGraph> {
    let mut graph = new_dir_graph_in_mode(StorageMode::Mapped, None).expect("mapped graph");
    let params = HashMap::new();
    for i in 0..FIXTURE_NODES {
        execute_mut(
            &mut graph,
            &format!("CREATE (:Item {{id: {i}, name: 'item-{i}'}})"),
            &ExecuteOptions::eager(&params),
        )
        .expect("fixture node");
    }
    assert!(graph.graph.is_mapped(), "fixture must be mapped");
    Arc::new(graph)
}

/// Mapped twin of `handle::held_reference_clone_tests::held_reader_copies_no_nodes`.
#[test]
fn held_reader_copies_no_nodes_on_a_mapped_graph() {
    let mut arc = seeded_mapped_arc();
    let reader = Arc::clone(&arc);

    reset_backend_clone_count();
    let _ = make_dir_graph_mut(&mut arc);
    let copied = backend_clone_nodes();

    assert_eq!(reader.graph.node_count(), FIXTURE_NODES);
    assert_eq!(
        copied, 0,
        "a live second Arc<DirGraph> must fork a mapped graph to the overlay, not \
         deep-copy it; getting {FIXTURE_NODES} means the Mapped clone arm regressed"
    );
    assert!(arc.graph.is_forked(), "the writer must be the overlay");
}

/// A forked mapped graph still is a mapped graph, in every report that
/// consumers branch on, and the reader's snapshot is untouched by the writes.
#[test]
fn a_forked_mapped_graph_reports_mapped_and_isolates_the_reader() {
    let mut arc = seeded_mapped_arc();
    let reader = Arc::clone(&arc);
    assert_eq!(live_storage_mode(&reader), StorageMode::Mapped);

    {
        let graph = make_dir_graph_mut(&mut arc);
        assert!(graph.graph.is_forked(), "precondition: forked");
        run(graph, "MATCH (n:Item {id: 3}) SET n.name = 'rewritten'");
        run(graph, "CREATE (:Item {id: 999, name: 'appended'})");
        assert!(
            graph.graph.is_forked(),
            "the writes must stay on the overlay"
        );
    }

    assert!(arc.graph.is_mapped(), "a forked mapped graph is mapped");
    assert!(!arc.graph.is_memory(), "...and not memory");
    assert_eq!(live_storage_mode(&arc), StorageMode::Mapped);
    assert_eq!(arc.graph.node_count(), FIXTURE_NODES + 1);
    assert_eq!(reader.graph.node_count(), FIXTURE_NODES);
    assert_eq!(
        ids(&reader, "MATCH (n:Item {name: 'rewritten'}) RETURN n.id"),
        Vec::<Value>::new(),
        "the reader must not see the writer's SET"
    );
    assert_eq!(
        ids(&arc, "MATCH (n:Item {name: 'rewritten'}) RETURN n.id"),
        vec![Value::Int64(3)]
    );

    drop(reader);
    let graph = make_dir_graph_mut(&mut arc);
    assert!(
        !graph.graph.is_forked(),
        "the reader is gone: the fold lands"
    );
    assert!(graph.graph.is_mapped());
    assert_eq!(live_storage_mode(graph), StorageMode::Mapped);
    assert_eq!(graph.graph.node_count(), FIXTURE_NODES + 1);
}

/// Folding must drop the property indexes the base built before the fork.
///
/// Red without `MappedGraph`'s `after_fold` invalidation: the cached block
/// still maps `name-1` to node 1 and knows nothing of `Bob`, and the matcher
/// returns a non-empty block's answer verbatim.
#[test]
fn a_fold_back_leaves_no_stale_mapped_property_index() {
    let mut arc = Arc::new(mapped_row_storage_people(3));
    assert_eq!(
        ids(&arc, "MATCH (p:Person {name:'name-1'}) RETURN p.id"),
        vec![Value::Int64(1)],
        "precondition: the base's index is built and answers"
    );
    assert_eq!(
        arc.graph
            .lookup_by_property_eq_any_type("name", "name-2")
            .expect("global index live"),
        vec![NodeIndex::new(1)],
        "precondition: the global index is built too"
    );

    let reader = Arc::clone(&arc);
    {
        let graph = make_dir_graph_mut(&mut arc);
        assert!(graph.graph.is_forked(), "precondition: forked");
        run(graph, "MATCH (p:Person) WHERE p.id = 1 SET p.name = 'Bob'");
        run(graph, "MATCH (p:Person) WHERE p.id = 2 SET p.name = 'Zoe'");
    }
    drop(reader);

    let graph = make_dir_graph_mut(&mut arc);
    assert!(!graph.graph.is_forked(), "precondition: folded back");
    assert!(graph.graph.is_mapped());
    assert!(
        ids(graph, "MATCH (p:Person {name:'name-1'}) RETURN p.id").is_empty(),
        "the overwritten value must stop matching after the fold"
    );
    assert_eq!(
        ids(graph, "MATCH (p:Person {name:'Bob'}) RETURN p.id"),
        vec![Value::Int64(1)],
        "the written value must match after the fold"
    );
    assert!(
        graph
            .graph
            .lookup_by_property_eq_any_type("name", "name-2")
            .is_none_or(|hits| hits.is_empty()),
        "the global index must not map the overwritten value"
    );
}

/// The typed-edge index likewise: edges the overlay added must be counted
/// after the fold.
#[test]
fn a_fold_back_leaves_no_stale_mapped_type_index() {
    let mut arc = Arc::new(mapped_row_storage_people(3));
    let knows = InternedKey::from_str("KNOWS");
    let count = |g: &DirGraph, node: usize| {
        g.graph
            .count_edges_filtered(
                NodeIndex::new(node),
                Direction::Outgoing,
                Some(knows),
                None,
                None,
            )
            .expect("count")
    };
    {
        let graph = Arc::get_mut(&mut arc).expect("unique");
        run(
            graph,
            "MATCH (a:Person {id: 1}), (b:Person {id: 2}) CREATE (a)-[:KNOWS]->(b)",
        );
    }
    assert_eq!(count(&arc, 0), 1, "precondition: the type index is built");

    let reader = Arc::clone(&arc);
    {
        let graph = make_dir_graph_mut(&mut arc);
        assert!(graph.graph.is_forked(), "precondition: forked");
        run(
            graph,
            "MATCH (a:Person {id: 1}), (b:Person {id: 3}) CREATE (a)-[:KNOWS]->(b)",
        );
        assert_eq!(count(graph, 0), 2, "the overlay counts its own edge");
    }
    assert_eq!(count(&reader, 0), 1, "the reader still sees one edge");
    drop(reader);

    let graph = make_dir_graph_mut(&mut arc);
    assert!(!graph.graph.is_forked(), "precondition: folded back");
    assert_eq!(
        count(graph, 0),
        2,
        "the fold must drop the type index built before the new edge"
    );
}

/// A save taken from a forked mapped graph writes the writer's graph, leaves
/// the overlay and the reader alone, and reloads to the same content.
#[test]
fn a_forked_mapped_graph_saves_and_reloads() {
    use crate::graph::io::file::{load_file, prepare_kgl_write, write_kgl};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forked-mapped.kgl");
    let path = path.to_str().unwrap();

    let mut arc = seeded_mapped_arc();
    let reader = Arc::clone(&arc);
    {
        let graph = make_dir_graph_mut(&mut arc);
        assert!(graph.graph.is_forked(), "precondition: forked");
        run(graph, "MATCH (n:Item {id: 3}) SET n.name = 'rewritten'");
        run(graph, "CREATE (:Item {id: 999, name: 'appended'})");
    }
    prepare_kgl_write(&mut arc);
    assert!(
        arc.graph.is_forked(),
        "precondition: the serializer must see the overlay"
    );
    assert_eq!(live_storage_mode(&arc), StorageMode::Mapped);
    write_kgl(&arc, path).unwrap();

    let loaded = load_file(path).unwrap();
    assert_eq!(loaded.graph.node_count(), FIXTURE_NODES + 1);
    assert_eq!(
        ids(&loaded, "MATCH (n:Item {name: 'rewritten'}) RETURN n.id"),
        vec![Value::Int64(3)]
    );
    assert_eq!(
        ids(&loaded, "MATCH (n:Item {name: 'appended'}) RETURN n.id"),
        vec![Value::Int64(999)]
    );
    assert!(
        arc.graph.is_forked(),
        "the save must not consume the overlay"
    );
    assert_eq!(
        ids(&reader, "MATCH (n:Item {name: 'rewritten'}) RETURN n.id"),
        Vec::<Value>::new()
    );
}

/// `Recording(Mapped)` — a durable mapped graph — forks too: the wrapper clones
/// its inner backend, which now takes the overlay.
#[test]
fn a_recording_mapped_graph_forks_without_copying() {
    let mut graph = new_dir_graph_in_mode(StorageMode::Mapped, None).expect("mapped graph");
    run(&mut graph, "CREATE (:Item {id: 1, name: 'a'})");
    crate::graph::storage::recording::wrap_for_durability(&mut graph).expect("wrap");
    assert!(matches!(
        graph.graph,
        crate::graph::schema::GraphBackend::Recording(_)
    ));
    let mut arc = Arc::new(graph);
    let reader = Arc::clone(&arc);

    reset_backend_clone_count();
    let graph = make_dir_graph_mut(&mut arc);
    assert_eq!(
        backend_clone_nodes(),
        0,
        "the wrapped mapped graph copied nodes"
    );
    assert!(graph.graph.is_forked(), "Recording(Mapped) must fork");
    assert!(graph.graph.is_mapped());
    assert_eq!(live_storage_mode(graph), StorageMode::Mapped);
    run(graph, "CREATE (:Item {id: 2, name: 'b'})");
    assert_eq!(graph.graph.node_count(), 2);
    assert_eq!(reader.graph.node_count(), 1);
}

/// Spilled columns under a fork: the writer's copy-on-write of a touched store
/// reads back correctly, the reader's stores stay mapped, and the spill pass
/// the next write runs still works on the overlay.
#[test]
fn a_forked_mapped_graph_keeps_spilling_and_reads_back() {
    use crate::datatypes::values::{ColumnData, ColumnType, DataFrame};
    use crate::graph::mutation::maintain::add_nodes;

    let mut graph = new_dir_graph_in_mode(StorageMode::Mapped, None).expect("mapped graph");
    // Past `MAPPED_SPILL_FLOOR` of heap, so the load spills.
    let rows = 12_000;
    let mut df = DataFrame::new(Vec::new());
    df.add_column(
        "id".to_string(),
        ColumnType::String,
        ColumnData::String((0..rows).map(|i| Some(format!("n{i}"))).collect()),
    )
    .expect("id column");
    df.add_column(
        "body".to_string(),
        ColumnType::String,
        ColumnData::String((0..rows).map(|_| Some("x".repeat(128))).collect()),
    )
    .expect("body column");
    add_nodes(
        &mut graph,
        df,
        "Doc".to_string(),
        "id".to_string(),
        None,
        None,
    )
    .expect("add nodes");
    let spilled = |g: &DirGraph| g.graph.column_stores_iter().any(|(_, s)| s.is_mapped());
    assert!(spilled(&graph), "precondition: the load spilled to mmap");

    let mut arc = Arc::new(graph);
    let reader = Arc::clone(&arc);
    {
        let graph = make_dir_graph_mut(&mut arc);
        assert!(graph.graph.is_forked(), "precondition: forked");
        run(graph, "MATCH (d:Doc {id: 'n7'}) SET d.body = 'changed'");
        run(graph, "CREATE (:Doc {id: 'fresh', body: 'new'})");
        graph.maybe_spill_columns();
    }
    assert_eq!(
        ids(&arc, "MATCH (d:Doc {body: 'changed'}) RETURN d.id"),
        vec![Value::String("n7".into())]
    );
    assert_eq!(arc.graph.node_count(), rows + 1);
    assert!(
        spilled(&reader),
        "the reader's stores must stay mmap-backed"
    );
    assert_eq!(
        ids(&reader, "MATCH (d:Doc {body: 'changed'}) RETURN d.id"),
        Vec::<Value>::new()
    );
    assert!(
        spilled(&arc),
        "the overlay's spill pass must leave its stores mmap-backed"
    );
}

fn spill_entries(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| {
                    1 + entry.file_type().map_or(0, |t| {
                        if t.is_dir() {
                            spill_entries(&entry.path())
                        } else {
                            0
                        }
                    })
                })
                .sum()
        })
        .unwrap_or(0)
}

/// A writer overlay appends to a heap tail until it folds. At `Mapped`'s zero
/// memory limit the spill pass used to move that tail to fresh column files
/// after every statement: a directory plus a file per column created, synced
/// and unlinked per fork for rows that live until the next commit. A tail below
/// the spill floor stays on the heap, and the fold appends it to the store's
/// mapped columns.
#[test]
fn a_small_overlay_tail_is_not_spilled_to_files_each_statement() {
    use crate::datatypes::values::{ColumnData, ColumnType, DataFrame};
    use crate::graph::mutation::maintain::add_nodes;

    let mut graph = new_dir_graph_in_mode(StorageMode::Mapped, None).expect("mapped graph");
    // Past `HEAP_TAIL_MIN_ROWS` (16,384), where an overlay append starts a tail.
    let rows = 17_500;
    let mut df = DataFrame::new(Vec::new());
    df.add_column(
        "id".to_string(),
        ColumnType::String,
        ColumnData::String((0..rows).map(|i| Some(format!("n{i}"))).collect()),
    )
    .expect("id column");
    df.add_column(
        "body".to_string(),
        ColumnType::String,
        ColumnData::String((0..rows).map(|_| Some("x".repeat(64))).collect()),
    )
    .expect("body column");
    add_nodes(
        &mut graph,
        df,
        "Doc".to_string(),
        "id".to_string(),
        None,
        None,
    )
    .expect("add nodes");

    let mut arc = Arc::new(graph);
    let reader = Arc::clone(&arc);
    let graph = make_dir_graph_mut(&mut arc);
    assert!(graph.graph.is_forked(), "precondition: forked");
    let spill_dir = graph
        .spill_dir
        .clone()
        .expect("precondition: the load spilled");
    let before = spill_entries(&spill_dir);
    for i in 0..20 {
        run(
            graph,
            &format!("CREATE (:Doc {{id: 'fresh{i}', body: 'new'}})"),
        );
    }
    assert!(
        graph
            .graph
            .column_stores_iter()
            .any(|(_, store)| store.has_heap_tail()),
        "precondition: the appends went to a heap tail"
    );
    assert_eq!(
        spill_entries(&spill_dir),
        before,
        "twenty one-row appends under a held reader must not create spill files"
    );
    assert_eq!(graph.graph.node_count(), rows + 20);
    assert_eq!(reader.graph.node_count(), rows);
}

/// A writer's first `SET` under a held reader clones the store and rewrites one
/// column on the heap. At `Mapped`'s zero memory limit that used to re-run the
/// spill pass on the forked statement, which wrote every store holding heap to a
/// fresh directory of column files, once per fork, under the session's writer
/// lock. Heap below the mapped spill floor stays where it is.
#[test]
fn a_small_cow_clone_under_a_held_reader_is_not_spilled_per_fork() {
    use crate::datatypes::values::{ColumnData, ColumnType, DataFrame};
    use crate::graph::mutation::maintain::add_nodes;

    let mut graph = new_dir_graph_in_mode(StorageMode::Mapped, None).expect("mapped graph");
    // Enough heap at load to cross the floor, so the load itself spills.
    let rows = 40_000;
    let mut df = DataFrame::new(Vec::new());
    df.add_column(
        "id".to_string(),
        ColumnType::String,
        ColumnData::String((0..rows).map(|i| Some(format!("n{i}"))).collect()),
    )
    .expect("id column");
    df.add_column(
        "body".to_string(),
        ColumnType::String,
        ColumnData::String((0..rows).map(|_| Some("x".repeat(40))).collect()),
    )
    .expect("body column");
    df.add_column(
        "qty".to_string(),
        ColumnType::Int64,
        ColumnData::Int64((0..rows).map(|i| Some(i as i64)).collect()),
    )
    .expect("qty column");
    add_nodes(
        &mut graph,
        df,
        "Doc".to_string(),
        "id".to_string(),
        None,
        None,
    )
    .expect("add nodes");

    let mut arc = Arc::new(graph);
    let spill_dir = arc
        .spill_dir
        .clone()
        .expect("precondition: the load spilled");
    let mut before = None;
    for i in 0..10 {
        let reader = Arc::clone(&arc);
        let graph = make_dir_graph_mut(&mut arc);
        assert!(graph.graph.is_forked(), "precondition: forked");
        run(
            graph,
            &format!("MATCH (n:Doc {{id: 'n5'}}) SET n.qty = {}", 1_000 + i),
        );
        drop(reader);
        // The first fork may legitimately spill once; later ones must not add.
        if i == 0 {
            before = Some(spill_entries(&spill_dir));
        }
    }
    assert_eq!(
        spill_entries(&spill_dir),
        before.expect("recorded"),
        "nine more fork-and-SET cycles must not create spill files"
    );
    assert_eq!(
        ids(&arc, "MATCH (n:Doc {id: 'n5'}) RETURN n.qty"),
        vec![Value::Int64(1_009)]
    );
}
