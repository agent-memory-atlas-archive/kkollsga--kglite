//! Heap held by the post-CSR overlays of an in-process-built disk graph.
//!
//! The Python bulk loader leaves `defer_csr` false, so every edge lands in
//! `overflow_out`/`overflow_in`/`appended_edge_endpoints` (~32 B/edge). Once
//! compaction or a save has folded those edges into the CSR, the overlays must
//! give their capacity back, not just their length.
use super::graph::DiskGraph;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData, StringInterner};
use crate::graph::storage::GraphRead;
use crate::graph::{dir_graph::DirGraph, storage::backend::GraphBackend};
use crate::test_alloc::{live_now, peak_during};
use std::collections::HashMap;

const NODES: usize = 4_000;
const EDGES: usize = 60_000;

fn build(root: &std::path::Path, interner: &mut StringInterner) -> DiskGraph {
    let mut g = DiskGraph::new_at_path(root).unwrap();
    assert!(!g.defer_csr, "the bulk loader's configuration");
    let nodes: Vec<_> = (0..NODES)
        .map(|i| {
            g.add_node(NodeData::new(
                Value::Int64(i as i64),
                Value::Null,
                "N".into(),
                HashMap::new(),
                interner,
            ))
        })
        .collect();
    for e in 0..EDGES {
        let a = nodes[e % NODES];
        let b = nodes[(e * 7 + 1) % NODES];
        g.add_edge(a, b, EdgeData::new("R".into(), HashMap::new(), interner));
    }
    g
}

/// Capacity-based bytes held by the overlays this change releases.
fn overlay_bytes(g: &DiskGraph) -> usize {
    let lists = |m: &HashMap<u32, Vec<crate::graph::storage::disk::csr::CsrEdge>>| {
        m.capacity() * (4 + std::mem::size_of::<Vec<()>>())
            + m.values()
                .map(|v| {
                    v.capacity() * std::mem::size_of::<crate::graph::storage::disk::csr::CsrEdge>()
                })
                .sum::<usize>()
    };
    g.appended_edge_endpoints.capacity()
        * std::mem::size_of::<crate::graph::storage::disk::csr::EdgeEndpoints>()
        + lists(&g.overflow_out)
        + lists(&g.overflow_in)
        + g.free_edge_slots.capacity() * 4
        + g.appended_node_slots.capacity()
            * std::mem::size_of::<crate::graph::storage::disk::csr::DiskNodeSlot>()
}

/// What may remain after compaction: the node-slot overlay, which `compact`
/// does not fold (a save does) and which scales with nodes, not edges.
const RELEASED_BOUND: usize = NODES * 16 + 4096;

#[test]
fn compact_releases_overflow_capacity() {
    let root = tempfile::tempdir().unwrap();
    let mut interner = StringInterner::new();
    let mut g = build(root.path(), &mut interner);
    let before = overlay_bytes(&g);
    let live_before = live_now();
    assert_eq!(g.compact().unwrap(), EDGES);
    let after = overlay_bytes(&g);
    eprintln!(
        "compact: overlay bytes {before} -> {after}; thread live delta {}",
        live_now() - live_before
    );
    assert_eq!(g.edge_count(), EDGES);
    assert!(
        before > EDGES * 32,
        "build holds the overlays: {before} bytes"
    );
    assert!(after <= RELEASED_BOUND, "overlays kept {after} bytes");
}

#[test]
fn save_releases_overlay_capacity_and_matches_a_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut interner = StringInterner::new();
    let disk = build(root.path(), &mut interner);
    let mut graph = DirGraph::new();
    graph.interner = interner;
    graph.graph = GraphBackend::Disk(Box::new(disk));
    let before = overlay_bytes(graph.graph.as_disk().unwrap());
    graph.save_disk(root.path().to_str().unwrap()).unwrap();
    let held = overlay_bytes(graph.graph.as_disk().unwrap());
    assert_eq!(graph.graph.edge_count(), EDGES);

    let opened = crate::graph::io::file::load_file(root.path().to_str().unwrap()).unwrap();
    assert_eq!(opened.graph.edge_count(), EDGES);
    let floor = overlay_bytes(opened.graph.as_disk().unwrap());
    eprintln!("save: overlay bytes {before} -> {held}; reopened handle holds {floor}");
    assert!(held <= RELEASED_BOUND.max(floor * 2), "kept {held} bytes");
}

#[test]
fn write_after_compact_clone_copies_no_large_overlay() {
    let root = tempfile::tempdir().unwrap();
    let mut interner = StringInterner::new();
    let mut g = build(root.path(), &mut interner);
    let (clone, built_cost) = peak_during(|| g.try_clone().unwrap());
    drop(clone);
    g.compact().unwrap();
    let (clone, compacted_cost) = peak_during(|| g.try_clone().unwrap());
    drop(clone);
    eprintln!("clone peak: built {built_cost}, compacted {compacted_cost}");
    assert!(built_cost > EDGES * 32);
    assert!(compacted_cost <= RELEASED_BOUND, "cloned {compacted_cost}");
}
