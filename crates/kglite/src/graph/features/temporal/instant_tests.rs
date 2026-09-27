//! The whole-graph filter at an instant: the Disk instant mask agrees with
//! the evaluator, is cached per instant and refused over its cap; the slice
//! it gives is cached per instant.

use std::collections::{BTreeSet, HashMap};

use petgraph::graph::NodeIndex;

use super::*;
use crate::datatypes::values::Value;
use crate::graph::features::temporal::declarations::{declare, TemporalTarget};
use crate::graph::features::temporal::eval::IntervalConvention;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

/// Wells (declared, closed) on a field; `IN` declared half-open. At
/// 2005-06-01 wells 1 and 3 are visible, well 2 has not started, and only
/// the relationship from well 3 runs (well 1's ended in 2004).
fn fixture(mode: StorageMode, dir: &tempfile::TempDir) -> DirGraph {
    let path = matches!(mode, StorageMode::Disk).then(|| dir.path());
    let mut g = new_dir_graph_in_mode(mode, path).expect("graph");
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(
        &mut g,
        "CREATE (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2009-12-31')}),
                (w2:Well {id: 2, vf: date('2008-01-01')}),
                (w3:Well {id: 3, vf: date('2001-01-01')}),
                (f:Field {id: 10}),
                (w1)-[:IN {f: date('2000-01-01'), t: date('2004-01-01')}]->(f),
                (w2)-[:IN {f: date('2000-01-01')}]->(f),
                (w3)-[:IN {f: date('2000-01-01')}]->(f)",
        &ExecuteOptions::eager(&params),
    )
    .expect("load");
    let closed = IntervalConvention::Closed;
    declare(
        &mut g,
        &TemporalTarget::Node("Well".into()),
        "vf",
        "vt",
        closed,
    )
    .unwrap();
    let rel = TemporalTarget::Relationship {
        rel_type: "IN".into(),
        source_type: None,
    };
    declare(&mut g, &rel, "f", "t", IntervalConvention::HalfOpen).unwrap();
    g
}

fn at(text: &str) -> Instant {
    Instant::Date(chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap())
}

/// The ids of the visible nodes, and the (source id, target id) of the
/// visible relationships, under `filter`.
fn visible(g: &DirGraph, filter: &ElementFilter) -> (BTreeSet<i64>, BTreeSet<(i64, i64)>) {
    let _arena_guard = g.graph.begin_query();
    let id = |idx: NodeIndex| match g.graph.get_node_id(idx) {
        Some(Value::Int64(id)) => id,
        other => panic!("id {other:?}"),
    };
    let nodes = g
        .graph
        .node_indices()
        .filter(|&idx| filter.admits_node(g, idx))
        .map(id)
        .collect();
    let mut edges = BTreeSet::new();
    for idx in g.graph.node_indices() {
        for edge in g.graph.edges(idx) {
            let weight = edge.weight();
            if filter.admits_relationship(g, edge.id(), weight.connection_type, idx, edge.target())
            {
                edges.insert((id(idx), id(edge.target())));
            }
        }
    }
    (nodes, edges)
}

#[test]
fn the_disk_instant_mask_agrees_with_the_in_memory_filter_and_is_cached() {
    let dir = tempfile::tempdir().unwrap();
    let memory = fixture(StorageMode::Memory, &dir);
    let disk_dir = tempfile::tempdir().unwrap();
    let disk = fixture(StorageMode::Disk, &disk_dir);
    for t in ["2005-06-01", "1999-01-01", "2011-06-01"] {
        let (_, reference) = instant_filter(&memory, at(t)).unwrap();
        let (key, masked) = instant_filter(&disk, at(t)).unwrap();
        assert_eq!(key.instant, Some(at(t)), "Disk slices key on the instant");
        let (reference, masked) = (reference.unwrap(), masked.unwrap());
        assert_eq!(visible(&memory, &reference), visible(&disk, &masked), "{t}");
    }
    let (nodes, edges) = visible(
        &disk,
        &instant_filter(&disk, at("2005-06-01")).unwrap().1.unwrap(),
    );
    assert_eq!(nodes, BTreeSet::from([1, 3, 10]));
    assert_eq!(edges, BTreeSet::from([(3, 10)]));
    let first = endpoint_index::cached_disk_masks(&disk, at("2005-06-01")).expect("cached");
    let (_, again) = instant_filter(&disk, at("2005-06-01")).unwrap();
    drop(again);
    let second = endpoint_index::cached_disk_masks(&disk, at("2005-06-01")).unwrap();
    assert!(Arc::ptr_eq(&first, &second), "one pass per instant");
}

#[test]
fn a_disk_mask_over_its_cap_is_refused_before_it_is_built() {
    let dir = tempfile::tempdir().unwrap();
    let disk = fixture(StorageMode::Disk, &dir);
    let err = instant_filter_capped(&disk, at("2005-06-01"), 1).unwrap_err();
    assert!(err.contains("Disk mask cap"), "{err}");
    assert!(err.contains(DISK_MASK_CAP_ENV), "{err}");
    assert!(endpoint_index::cached_disk_masks(&disk, at("2005-06-01")).is_none());
    // The default cap holds it.
    assert!(instant_filter(&disk, at("2005-06-01")).unwrap().1.is_some());
}

#[test]
fn memory_mode_builds_no_instant_mask_and_caches_one_slice_per_instant() {
    let dir = tempfile::tempdir().unwrap();
    let g = fixture(StorageMode::Memory, &dir);
    let (key, filter) = instant_filter(&g, at("2005-06-01")).unwrap();
    assert!(filter.is_some());
    assert!(
        key.instant.is_none(),
        "indexed targets key on their segments"
    );
    assert!(endpoint_index::cached_disk_masks(&g, at("2005-06-01")).is_none());
    let first = slice_for(&g, at("2005-06-01")).unwrap();
    let second = slice_for(&g, at("2006-06-01")).unwrap();
    assert!(Arc::ptr_eq(&first, &second), "one segment, one slice");
    assert_eq!(first.graph().graph.node_count(), 3);
    let other = slice_for(&g, at("2011-06-01")).unwrap();
    assert!(!Arc::ptr_eq(&first, &other));
    assert_eq!(endpoint_index::cached_slice_count(&g), 2);
}
