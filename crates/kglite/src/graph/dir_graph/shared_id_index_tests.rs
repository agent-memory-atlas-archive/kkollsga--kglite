//! A lazily built id index is shared by every version of one lineage while the
//! type's members are unchanged, and by no version after they change.
//!
//! The lineage here is a writer graph and clones of it, which is what a
//! published session version is. `id_index_scans` counts `compute_id_index`
//! runs on this thread, so "reused" is observed as "no scan", and every case
//! also checks the answer against known ids.
use super::{id_index_scans, DirGraph};
use crate::datatypes::Value;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

const LEAVES: i64 = 50;

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

/// 50 `Leaf` nodes (ids 0..50, `k` repeats the id) and 5 `Product` nodes; no id
/// index built for either type. Writes select `Leaf` nodes by `k`, so the
/// writer itself never builds the type's id index and every version has to get
/// it from the shared slot.
fn lineage() -> DirGraph {
    let mut graph = DirGraph::new();
    for i in 0..LEAVES {
        run(&mut graph, &format!("CREATE (:Leaf {{id: {i}, k: {i}}})"));
    }
    for i in 0..5 {
        run(&mut graph, &format!("CREATE (:Product {{id: {i}}})"));
    }
    assert!(!graph.id_indices.contains_key("Leaf"), "fixture is cold");
    graph
}

fn find(graph: &DirGraph, node_type: &str, id: i64) -> Option<usize> {
    graph
        .lookup_by_id_readonly(node_type, &Value::Int64(id))
        .map(|idx| idx.index())
}

fn rows(graph: &DirGraph, query: &str) -> usize {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
        .len()
}

#[test]
fn an_index_a_reader_built_serves_later_versions_while_the_type_is_untouched() {
    let mut writer = lineage();
    let reader = writer.clone();
    let scans = id_index_scans();
    let expected = find(&reader, "Leaf", 7).expect("id 7 exists");
    assert_eq!(id_index_scans(), scans + 1, "the first lookup builds");

    for round in 0..3 {
        // The writer only ever touches Product.
        run(
            &mut writer,
            &format!("CREATE (:Product {{id: {}}})", 100 + round),
        );
        let version = writer.clone();
        assert_eq!(find(&version, "Leaf", 7), Some(expected));
        assert_eq!(find(&version, "Leaf", LEAVES + 5), None);
        assert_eq!(rows(&version, "MATCH (n:Leaf {id: 7}) RETURN n.id"), 1);
    }
    assert_eq!(
        id_index_scans(),
        scans + 1,
        "later versions must reuse the reader's index, not scan again"
    );
}

#[test]
fn a_node_added_to_the_type_is_found_on_later_versions_only() {
    let mut writer = lineage();
    let before = writer.clone();
    assert!(find(&before, "Leaf", 3).is_some());

    run(&mut writer, "CREATE (:Leaf {id: 1000})");
    let after = writer.clone();
    let created = find(&after, "Leaf", 1000).expect("the new id resolves");
    assert_eq!(find(&after, "Leaf", 3), find(&before, "Leaf", 3));
    assert_eq!(rows(&after, "MATCH (n:Leaf {id: 1000}) RETURN n.id"), 1);
    assert_ne!(Some(created), find(&before, "Leaf", 3));
}

#[test]
fn a_deleted_node_is_not_returned_by_later_versions() {
    let mut writer = lineage();
    let before = writer.clone();
    let doomed = find(&before, "Leaf", 7).expect("exists before the delete");

    run(&mut writer, "MATCH (n:Leaf {k: 7}) DETACH DELETE n");
    let after = writer.clone();
    assert_eq!(find(&after, "Leaf", 7), None, "deleted id must not resolve");
    assert_eq!(rows(&after, "MATCH (n:Leaf {id: 7}) RETURN n.id"), 0);
    assert!(find(&after, "Leaf", 8).is_some(), "its neighbours remain");
    assert_eq!(
        find(&before, "Leaf", 7),
        Some(doomed),
        "the older snapshot still has it"
    );
}

#[test]
fn an_id_deleted_and_recreated_resolves_to_the_new_node() {
    let mut writer = lineage();
    let before = writer.clone();
    let old = find(&before, "Leaf", 7).expect("exists");

    run(&mut writer, "MATCH (n:Leaf {k: 7}) DETACH DELETE n");
    run(&mut writer, "CREATE (:Leaf {id: 7, k: 70})");
    let after = writer.clone();
    let node = find(&after, "Leaf", 7).expect("the recreated id resolves");
    assert_eq!(
        rows(&after, "MATCH (n:Leaf {id: 7, k: 70}) RETURN n.id"),
        1,
        "the id must lead to the recreated node (slot {node}, was {old})"
    );
}

#[test]
fn a_snapshot_never_sees_an_entry_a_newer_version_built() {
    let mut writer = lineage();
    let oldest = writer.clone();
    run(&mut writer, "CREATE (:Leaf {id: 1000})");
    let newer = writer.clone();
    assert!(
        find(&newer, "Leaf", 1000).is_some(),
        "newer publishes its index"
    );

    assert_eq!(
        find(&oldest, "Leaf", 1000),
        None,
        "the older snapshot's members do not include id 1000"
    );
    assert!(find(&oldest, "Leaf", 3).is_some());
}

#[test]
fn forks_that_diverge_from_one_state_do_not_share_an_index() {
    let writer = lineage();
    let mut left = writer.clone();
    let mut right = writer.clone();
    run(&mut left, "CREATE (:Leaf {id: 500})");
    run(&mut right, "CREATE (:Leaf {id: 600})");

    assert!(find(&left, "Leaf", 500).is_some());
    assert_eq!(
        find(&right, "Leaf", 500),
        None,
        "left's node is not right's"
    );
    assert!(find(&right, "Leaf", 600).is_some());
    assert_eq!(find(&left, "Leaf", 600), None, "right's node is not left's");
}

/// The order that defeats dropping the entry on a write: a snapshot cut before
/// the write builds *after* it, publishing an index of the old members.
#[test]
fn a_stale_snapshots_late_build_is_not_served_to_a_newer_version() {
    let mut writer = lineage();
    let stale = writer.clone();

    run(&mut writer, "MATCH (n:Leaf {k: 7}) DETACH DELETE n");
    run(&mut writer, "CREATE (:Leaf {id: 1000})");
    let current = writer.clone();

    assert!(
        find(&stale, "Leaf", 7).is_some(),
        "the stale view builds late"
    );
    assert_eq!(find(&current, "Leaf", 7), None, "deleted on this version");
    assert!(
        find(&current, "Leaf", 1000).is_some(),
        "created on this version"
    );
    assert_eq!(find(&stale, "Leaf", 1000), None);
}
