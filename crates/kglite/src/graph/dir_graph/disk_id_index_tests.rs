//! A saved disk graph serves an all-`Int64` id index from the published file,
//! layers a write delta over it instead of copying it, resolves edge endpoints
//! by probing it, and keeps every answer across reopen, append and save cycles.
use super::disk_test_support::{current_generation, load_owned, run};
use super::DirGraph;
use crate::datatypes::{DataFrame, Value};
use crate::graph::mutation::edge_specs::{add_edges_from_specs, EdgeSpec};
use crate::graph::mutation::maintain;
use crate::graph::storage::disk::id_index::{full_maps_built, IdIndexBase};
use crate::graph::storage::lookups::graph_scans;
use std::collections::HashMap;
use std::path::Path;
use tempfile::TempDir;

/// Register-shaped ids: past `u32`, so the index cannot be the compact `Integer`
/// variant.
const BIG: i64 = 3_100_000_000_000;

fn add_versions(graph: &mut DirGraph, from: i64, to: i64) {
    let rows = (from..to)
        .map(|i| vec![Value::Int64(BIG + i * 7), Value::Int64(i)])
        .collect();
    let frame = DataFrame::from_cypher_rows(vec!["id".into(), "seq".into()], rows).unwrap();
    maintain::add_nodes(graph, frame, "Employment".into(), "id".into(), None, None).unwrap();
}

fn add_anchors(graph: &mut DirGraph, count: i64) {
    let rows = (0..count)
        .map(|i| vec![Value::Int64(9_000_000_000_000 + i)])
        .collect();
    let frame = DataFrame::from_cypher_rows(vec!["id".into()], rows).unwrap();
    maintain::add_nodes(graph, frame, "Employee".into(), "id".into(), None, None).unwrap();
}

fn seq_of(graph: &mut DirGraph, id: i64) -> Vec<Vec<Value>> {
    run(
        graph,
        &format!("MATCH (n:Employment {{id: {id}}}) RETURN n.seq"),
    )
}

fn published_pairs(graph: &DirGraph, root: &Path) -> usize {
    let base = IdIndexBase::load_from(&current_generation(root), &graph.interner)
        .unwrap()
        .expect("id_indices.bin");
    base.int64_parts("Employment")
        .expect("Employment persists as Int64Sorted")
        .0
        .len()
        / 8
}

/// The spike's finding, held: after a save the store serves the index from the
/// file (no heap overlay), an append layers a delta over the mapping, and old
/// and new ids resolve across two save/reload cycles without a full map.
#[test]
fn int64_ids_are_served_from_the_published_file_across_save_and_append() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("g");
    let path = root.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_versions(&mut graph, 0, 20_000);
    graph.save_disk(path).unwrap();
    assert_eq!(published_pairs(&graph, &root), 20_000);
    assert_eq!(
        graph.id_indices.overlay_len("Employment"),
        None,
        "the heap map that built the index is gone after the save"
    );
    assert_eq!(
        seq_of(&mut graph, BIG + 12_345 * 7),
        vec![vec![Value::Int64(12_345)]]
    );

    let built = full_maps_built();
    add_versions(&mut graph, 20_000, 20_100);
    assert_eq!(graph.id_indices.overlay_len("Employment"), Some(20_100));
    assert_eq!(
        seq_of(&mut graph, BIG + 12_345 * 7),
        vec![vec![Value::Int64(12_345)]]
    );
    assert_eq!(
        seq_of(&mut graph, BIG + 20_050 * 7),
        vec![vec![Value::Int64(20_050)]]
    );
    graph.save_disk(path).unwrap();
    assert_eq!(
        full_maps_built(),
        built,
        "an append and a save built a full map"
    );
    assert_eq!(published_pairs(&graph, &root), 20_100);
    assert_eq!(graph.id_indices.overlay_len("Employment"), None);
    drop(graph);

    // Reopen -> append -> save -> reopen, twice: the reload never builds a map.
    let mut expected = 20_100i64;
    for cycle in 0..2 {
        let mut graph = load_owned(path);
        assert_eq!(
            seq_of(&mut graph, BIG + 20_099 * 7),
            vec![vec![Value::Int64(20_099)]]
        );
        add_versions(&mut graph, expected, expected + 50);
        run(
            &mut graph,
            &format!(
                "MATCH (n:Employment {{id: {}}}) DETACH DELETE n",
                BIG + (cycle * 7 + 3) * 7
            ),
        );
        graph.save_disk(path).unwrap();
        expected += 50;
        assert_eq!(
            published_pairs(&graph, &root),
            expected as usize - 1 - cycle as usize
        );
        drop(graph);
    }
    let mut graph = load_owned(path);
    assert_eq!(full_maps_built(), built);
    assert_eq!(seq_of(&mut graph, BIG + 3 * 7), Vec::<Vec<Value>>::new());
    assert_eq!(seq_of(&mut graph, BIG + 10 * 7), Vec::<Vec<Value>>::new());
    assert_eq!(
        seq_of(&mut graph, BIG + 20_149 * 7),
        vec![vec![Value::Int64(20_149)]]
    );
    assert_eq!(seq_of(&mut graph, BIG + 4 * 7), vec![vec![Value::Int64(4)]]);
    assert_eq!(
        run(&mut graph, "MATCH (n:Employment) RETURN count(n)"),
        vec![vec![Value::Int64(20_198)]]
    );
}

/// A delete on a reopened type served from the file tombstones the deleted ids
/// in a delta over the mapping. Dropping the type's index instead made the next
/// id lookup rebuild it by scanning every node of the type.
#[test]
fn a_delete_on_a_mapped_type_leaves_the_index_in_the_file() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("g");
    let path = root.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_versions(&mut graph, 0, 5_000);
    graph.save_disk(path).unwrap();
    drop(graph);

    let mut graph = load_owned(path);
    let built = full_maps_built();
    let scans = super::id_index_scans();
    run(
        &mut graph,
        &format!(
            "MATCH (n:Employment) WHERE n.id IN [{}, {}] DETACH DELETE n",
            BIG + 3 * 7,
            BIG + 4_000 * 7
        ),
    );
    assert_eq!(seq_of(&mut graph, BIG + 3 * 7), Vec::<Vec<Value>>::new());
    assert_eq!(
        seq_of(&mut graph, BIG + 4_000 * 7),
        Vec::<Vec<Value>>::new()
    );
    assert_eq!(
        seq_of(&mut graph, BIG + 4_001 * 7),
        vec![vec![Value::Int64(4_001)]]
    );
    assert_eq!(
        graph.id_indices.overlay_len("Employment"),
        Some(4_998),
        "the index is a delta over the file, minus the deleted ids"
    );
    assert_eq!(
        super::id_index_scans(),
        scans,
        "an id lookup after the delete scanned the type to rebuild its index"
    );
    assert_eq!(
        full_maps_built(),
        built,
        "the delete copied the file's index onto the heap"
    );

    // The deletion survives a save and a reopen.
    graph.save_disk(path).unwrap();
    drop(graph);
    let mut graph = load_owned(path);
    assert_eq!(seq_of(&mut graph, BIG + 3 * 7), Vec::<Vec<Value>>::new());
    assert_eq!(
        run(&mut graph, "MATCH (n:Employment) RETURN count(n)"),
        vec![vec![Value::Int64(4_998)]]
    );
}

/// The rebase is an optimisation after a durable publish: a failure keeps the
/// live heap entry, which is right, and never turns the save into an error.
#[test]
fn a_failed_id_index_rebase_does_not_fail_a_published_save() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("g");
    let path = root.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_versions(&mut graph, 0, 3_000);
    let before = std::fs::read_to_string(root.join("CURRENT")).ok();

    let result = super::with_failing_stage("rebase_id_indices", || graph.save_disk(path));
    assert!(
        result.is_ok(),
        "a post-publish step failed the save: {result:?}"
    );
    assert_ne!(std::fs::read_to_string(root.join("CURRENT")).ok(), before);
    assert_eq!(
        graph.id_indices.overlay_len("Employment"),
        Some(3_000),
        "the live index stays the heap one"
    );
    assert_eq!(seq_of(&mut graph, BIG + 7), vec![vec![Value::Int64(1)]]);

    // The next save rebases normally, and what was published is right.
    graph.save_disk(path).unwrap();
    assert_eq!(graph.id_indices.overlay_len("Employment"), None);
    drop(graph);
    let mut reloaded = load_owned(path);
    assert_eq!(
        seq_of(&mut reloaded, BIG + 2_999 * 7),
        vec![vec![Value::Int64(2_999)]]
    );
}

/// Edge endpoints of types still served from the mapping resolve one probe per
/// row, never by copying either type's index onto the heap — through the
/// DataFrame path and the spec path alike, with float spellings of the ids.
#[test]
fn edge_endpoints_are_resolved_by_probing_the_mapped_index() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("g");
    let path = root.to_str().unwrap();
    let mut graph = DirGraph::new();
    graph.enable_disk_mode().unwrap();
    add_versions(&mut graph, 0, 5_000);
    add_anchors(&mut graph, 400);
    graph.save_disk(path).unwrap();
    drop(graph);

    let mut graph = load_owned(path);
    let built = full_maps_built();
    let scans = graph_scans();
    let frame = DataFrame::from_cypher_rows(
        vec!["version".into(), "anchor".into()],
        vec![
            vec![Value::Int64(BIG + 7), Value::Int64(9_000_000_000_001)],
            vec![
                Value::Int64(BIG + 4_999 * 7),
                Value::Int64(9_000_000_000_399),
            ],
            // A whole float, as a pandas nullable-int column promotes it.
            vec![
                Value::Float64((BIG + 14) as f64),
                Value::Int64(9_000_000_000_002),
            ],
        ],
    )
    .unwrap();
    let report = maintain::add_connections(
        &mut graph,
        frame,
        "OF".into(),
        "Employment".into(),
        "version".into(),
        "Employee".into(),
        "anchor".into(),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(report.connections_created, 3, "{report:?}");
    let spec = |source: i64, target: i64| EdgeSpec {
        source_type: "Employment".into(),
        source_id: Value::Int64(source),
        target_type: "Employee".into(),
        target_id: Value::Int64(target),
        edge_type: "MENTIONS".into(),
        properties: HashMap::new(),
    };
    let report = add_edges_from_specs(
        &mut graph,
        vec![
            spec(BIG + 21, 9_000_000_000_003),
            spec(BIG + 28, 9_000_000_000_004),
            spec(BIG + 1, 9_000_000_000_004),
        ],
    )
    .unwrap();
    assert_eq!(report.connections_created, 2, "{report:?}");
    assert_eq!(report.skipped_missing_endpoint, 1);

    assert_eq!(
        run(
            &mut graph,
            "MATCH (:Employment)-[e:OF]->(:Employee) RETURN count(e)"
        ),
        vec![vec![Value::Int64(3)]]
    );
    assert_eq!(
        run(
            &mut graph,
            "MATCH (p:Employment {id: 3100000000014})-[:OF]->(o:Employee) RETURN o.id"
        ),
        vec![vec![Value::Int64(9_000_000_000_002)]]
    );
    assert_eq!(
        full_maps_built(),
        built,
        "endpoint resolution built a full map"
    );
    assert_eq!(
        graph_scans(),
        scans,
        "endpoint resolution scanned the graph instead of probing the index"
    );
    assert_eq!(graph.id_indices.overlay_len("Employment"), None);
    assert_eq!(graph.id_indices.overlay_len("Employee"), None);
}
