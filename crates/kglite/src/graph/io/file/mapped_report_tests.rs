//! What `graph_info()` and `save()` say about the parts of a graph that are
//! held without a mapped form.

use super::frozen_tests::{dump, saved_topo_big};
use super::index_sections::with_floors;
use super::index_sections_tests::{add_unique_id_type, load, load_owned, run, save};
use super::mappable::SaveLayout;
use super::*;
use crate::graph::dir_graph::MappedFallback;

fn part(info: &crate::graph::dir_graph::GraphInfo, name: &str) -> Option<String> {
    info.mapped_fallbacks
        .iter()
        .find(|f| f.part == name)
        .map(|f| f.reason.clone())
}

/// The topology status of a freshly loaded `mapped` file: Windows copies the
/// sections to the heap and says so.
fn loaded_status() -> (&'static str, Option<String>) {
    if cfg!(windows) {
        (
            "heap",
            Some("Windows copies the sections to the heap".to_string()),
        )
    } else {
        ("mapped", None)
    }
}

/// 70,000 `Doc` rows (a delta-compressed `n`), one node of `MixId` holding a
/// string id beside an int one, and a `Doc` column of mixed values.
fn mixed_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    add_unique_id_type(&mut graph, "Doc", 0..70_000);
    run(&mut graph, "MATCH (d:Doc {id: 5}) SET d.n = 'five'");
    run(&mut graph, "MATCH (d:Doc) SET d.m = toInteger(d.id * 3)");
    run(
        &mut graph,
        "CREATE (:MixId {id: 'x'}), (:MixId {id: 5}), (:Plain {id: 1})",
    );
    run(
        &mut graph,
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:L]->(b)",
    );
    graph
}

fn save_mixed(tmp: &tempfile::TempDir) -> (std::path::PathBuf, Option<String>) {
    let path = tmp.path().join("mixed.kgl");
    let mut graph = Arc::new(mixed_graph());
    with_floors(10, 10, || {
        save_graph_with_layout(&mut graph, path.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    let notice = graph.take_save_notice();
    (path, notice)
}

#[test]
fn a_save_records_and_announces_the_parts_it_wrote_without_a_mapped_form() {
    let tmp = tempfile::tempdir().unwrap();
    let (path, notice) = save_mixed(&tmp);
    let notice = notice.expect("a save that dropped parts says so");
    assert!(
        notice.contains("indexes:id:MixId") && notice.contains("columns:Doc.n"),
        "{notice}"
    );
    assert!(
        !notice.contains("delta-compressed"),
        "compressed integer columns are by design and not named: {notice}"
    );

    let mapped = load_owned(&path, StorageMode::Mapped);
    let info = mapped.graph_info();
    assert_eq!(
        (info.topology_status, info.topology_heap_reason.clone()),
        loaded_status()
    );
    assert!(
        part(&info, "indexes:id:MixId").is_some_and(|r| r.contains("integer and string")),
        "{:?}",
        info.mapped_fallbacks
    );
    assert!(part(&info, "columns:Doc.n").is_some_and(|r| r.contains("mixed kinds")));
    assert!(
        part(&info, "columns:Doc.m").is_some_and(|r| r.contains("delta-compressed")),
        "{:?}",
        info.mapped_fallbacks
    );
    assert_eq!(part(&info, "indexes:id:Plain"), None, "Plain is fine");

    // Memory storage holds everything on the heap by choice and lists nothing.
    let memory = load_owned(&path, StorageMode::Memory);
    assert!(memory.graph_info().mapped_fallbacks.is_empty());

    // A second take is empty, and a clean save parks nothing.
    let mut clean = Arc::new(graph_without_drops());
    let clean_path = tmp.path().join("clean.kgl");
    with_floors(10, 10, || {
        save_graph_with_layout(
            &mut clean,
            clean_path.to_str().unwrap(),
            true,
            SaveLayout::Auto,
        )
    })
    .unwrap();
    assert_eq!(clean.take_save_notice(), None);
}

fn graph_without_drops() -> DirGraph {
    let mut graph = DirGraph::new();
    add_unique_id_type(&mut graph, "Doc", 0..1000);
    run(
        &mut graph,
        "MATCH (a:Doc {id: 1}), (b:Doc {id: 2}) CREATE (a)-[:L]->(b)",
    );
    graph
}

#[test]
fn a_file_without_drops_writes_no_fallback_key() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("clean.kgl");
    with_floors(10, 10, || {
        save(graph_without_drops(), &path, SaveLayout::Auto)
    });
    let bytes = std::fs::read(&path).unwrap();
    let length = u32::from_le_bytes(bytes[9..13].try_into().unwrap()) as usize;
    let json = std::str::from_utf8(&bytes[13..13 + length - 4]).unwrap();
    assert!(!json.contains("mapped_fallbacks"), "byte-identical posture");
}

#[test]
fn a_rebased_save_reports_the_parts_of_the_file_it_now_serves() {
    let tmp = tempfile::tempdir().unwrap();
    let mut graph = {
        let (path, _) = save_mixed(&tmp);
        load(&path, StorageMode::Mapped)
    };
    assert!(part(&graph.graph_info(), "indexes:id:MixId").is_some());
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        // Repair the type, then write: the next file has no such part.
        run(writer, "MATCH (m:MixId) DETACH DELETE m");
        run(writer, "CREATE (:Doc {id: 900001, n: 1})");
    }
    assert_eq!(graph.graph_info().topology_status, "overlay");
    let again = tmp.path().join("again.kgl");
    with_floors(10, 10, || {
        save_graph_with_layout(&mut graph, again.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    let info = graph.graph_info();
    if cfg!(windows) {
        // The rebase is declined where sections are copied: the overlay keeps
        // serving and the report still describes the file it was loaded from.
        assert_eq!(info.topology_status, "overlay");
        return;
    }
    assert_eq!(info.topology_status, "mapped", "rebased onto the file");
    assert_eq!(
        part(&info, "indexes:id:MixId"),
        None,
        "{:?}",
        info.mapped_fallbacks
    );
    assert!(part(&info, "columns:Doc.n").is_some());
}

#[test]
fn the_topology_status_says_why_it_is_on_the_heap() {
    let (_tmp, path) = saved_topo_big();
    let status = |graph: &DirGraph| {
        let info = graph.graph_info();
        (info.topology_status, info.topology_heap_reason)
    };

    let mapped = load_owned(&path, StorageMode::Mapped);
    assert_eq!(status(&mapped), loaded_status());

    let memory = load_owned(&path, StorageMode::Memory);
    assert_eq!(
        status(&memory),
        ("heap", Some("memory storage".to_string()))
    );

    let policy = Arc::try_unwrap(
        load_file_with(
            path.to_str().unwrap(),
            &LoadOptions::new()
                .with_storage(StorageMode::Mapped)
                .with_topology(MapPolicy::Memory),
        )
        .unwrap(),
    )
    .unwrap_or_else(|_| panic!("owned"));
    let (state, reason) = status(&policy);
    assert_eq!(state, "heap");
    assert!(reason.unwrap().contains("topology=\"memory\""));

    // A write puts an overlay over the file; a whole-graph operation collapses
    // it and the reason says so.
    let mut graph = load_owned(&path, StorageMode::Mapped);
    run(&mut graph, "CREATE (:A {id: 9000, n: 1, s: 'x'})");
    assert_eq!(status(&graph), ("overlay", None));
    graph.vacuum();
    let (state, reason) = status(&graph);
    assert_eq!(state, "heap");
    assert!(reason.unwrap().contains("whole-graph operation"));
}

#[test]
fn a_file_the_writer_could_not_record_names_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("many.kgl");
    let mut graph = Arc::new(graph_with_connection_types(40));
    topology_sections::with_type_cap(25, || {
        with_floors(10, 10, || {
            save_graph_with_layout(&mut graph, path.to_str().unwrap(), true, SaveLayout::Auto)
        })
    })
    .unwrap();
    let notice = graph.take_save_notice().expect("announced");
    assert!(
        notice.contains("topology") && notice.contains("connection types"),
        "{notice}"
    );

    let mapped = load_owned(&path, StorageMode::Mapped);
    let info = mapped.graph_info();
    assert_eq!(info.topology_status, "heap");
    let reason = info.topology_heap_reason.clone().unwrap();
    assert!(reason.contains("connection types"), "{reason}");
    assert_eq!(
        part(&info, "topology").as_deref(),
        Some("more than 65,535 connection types")
    );
    // The graph itself is intact.
    assert_eq!(dump(&mapped), dump(&load_owned(&path, StorageMode::Memory)));
}

/// `types` node types of one node each, in a chain of `types - 1` edges of
/// `types - 1` connection types.
fn graph_with_types(types: usize) -> DirGraph {
    let mut graph = DirGraph::new();
    let nodes: Vec<String> = (0..types)
        .map(|i| format!("(n{i}:T{i} {{id: {i}}})"))
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

/// Three node types and `types` connection types between two of them.
fn graph_with_connection_types(types: usize) -> DirGraph {
    let mut graph = DirGraph::new();
    let edges: Vec<String> = (0..types).map(|i| format!("(a)-[:R{i}]->(b)")).collect();
    run(
        &mut graph,
        &format!(
            "CREATE (a:A {{id: 1}}), (b:B {{id: 2}}), (:C {{id: 3}}), {}",
            edges.join(", ")
        ),
    );
    graph
}

#[test]
fn more_node_types_than_the_index_cap_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("types.kgl");
    let mut graph = Arc::new(graph_with_types(1_100));
    with_floors(10, 10, || {
        save_graph_with_layout(&mut graph, path.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    let info = load_owned(&path, StorageMode::Mapped).graph_info();
    assert_eq!(
        info.topology_status,
        loaded_status().0,
        "the topology takes 65,535"
    );
    assert!(part(&info, "indexes:nodes").is_some_and(|r| r.contains("1,024")));
    assert!(part(&info, "indexes:peers").is_some_and(|r| r.contains("1,024")));
}

#[test]
fn a_long_report_is_summarised() {
    let many: Vec<MappedFallback> = (0..80)
        .map(|i| MappedFallback::new(format!("columns:T.c{i:02}"), "mixed"))
        .collect();
    let report = crate::graph::dir_graph::mapped_report::finalize(many);
    assert_eq!(report.len(), 50);
    assert_eq!(report[49].part, "more");
    assert!(report[49].reason.starts_with("31 further"));
}

#[test]
fn a_notice_a_save_parked_does_not_outlive_the_next_save() {
    let tmp = tempfile::tempdir().unwrap();
    let mut graph = Arc::new(mixed_graph());
    let save_to = |graph: &mut Arc<DirGraph>, name: &str| {
        let path = tmp.path().join(name);
        with_floors(10, 10, || {
            save_graph_with_layout(graph, path.to_str().unwrap(), true, SaveLayout::Auto)
        })
        .unwrap();
    };
    save_to(&mut graph, "first.kgl"); // the notice is parked and left untaken
    {
        let writer = crate::graph::handle::make_dir_graph_mut(&mut graph);
        run(writer, "MATCH (m:MixId) DETACH DELETE m");
        run(writer, "MATCH (d:Doc) REMOVE d.n");
    }
    save_to(&mut graph, "second.kgl");
    assert_eq!(
        graph.take_save_notice(),
        None,
        "the repaired graph saves quietly"
    );
}
