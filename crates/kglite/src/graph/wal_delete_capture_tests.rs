//! A commit that deletes many nodes must hold memory in proportion to what the
//! log records for each one (a type and an id), not to the width of the
//! in-memory capture types. Counting assertions, not RSS: they have no
//! machine-load dependence.
use super::*;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::wal_replay::apply_frames;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::recording::{resolve_ops, resolve_ops_into, wrap_for_durability, RawOp};
use crate::graph::storage::GraphRead;
use std::collections::HashMap;

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("query failed: {query}: {e}"));
}

/// `n` `Item`s (every other one linked to the next) as a checkpoint, plus a
/// durable-wrapped copy to mutate. The second graph is the live one.
fn seeded(n: usize) -> (DirGraph, DirGraph) {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        &format!("UNWIND range(1, {n}) AS i CREATE (:Item {{id: i, k: 'k' + toString(i)}})"),
    );
    run(
        &mut graph,
        "MATCH (a:Item), (b:Item) WHERE a.id % 2 = 1 AND b.id = a.id + 1 CREATE (a)-[:LINK {w: a.id}]->(b)",
    );
    run(&mut graph, "CREATE (:Keep {id: 1, note: 'stays'})");
    let checkpoint = graph.clone();
    wrap_for_durability(&mut graph).unwrap();
    (checkpoint, graph)
}

fn drain(graph: &mut DirGraph) -> Vec<RawOp> {
    graph.graph.recording_mut().unwrap().take_ops()
}

/// Deleting a node records one capture op for it. A second, redundant marker
/// per node doubled the buffer the delete held until the commit resolved it.
#[test]
fn a_node_delete_records_exactly_one_capture_op() {
    let n = 2000;
    let (_, mut live) = seeded(n);
    run(&mut live, "MATCH (n:Item) DETACH DELETE n");
    let raw = drain(&mut live);
    let removed_nodes = raw
        .iter()
        .filter(|op| matches!(op, RawOp::RemoveNode { .. }))
        .count();
    assert_eq!(removed_nodes, n);
    let node_markers = raw
        .iter()
        .filter(|op| matches!(op, RawOp::RemoveNode { .. } | RawOp::WalNode { .. }))
        .count();
    assert_eq!(
        node_markers, n,
        "{node_markers} node capture ops for {n} deleted nodes: each delete must record one"
    );
}

/// The encoded frame is the only per-op storage the resolver keeps: well
/// under the 272 bytes a resolved `MutationOp` occupies in a `Vec`.
#[test]
fn resolving_a_delete_buffers_only_its_encoded_bytes() {
    let n = 2000;
    let (_, mut live) = seeded(n);
    run(&mut live, "MATCH (n:Item) DETACH DELETE n");
    let raw = drain(&mut live);
    let mut body = FrameBody::new();
    resolve_ops_into(&raw, &live, &mut |op| body.push(&op));
    assert!(body.error.is_none());
    let per_op = body.bytes.len() as f64 / body.count as f64;
    // n node removals + n/2 group removals.
    assert!(body.count >= n as u64);
    assert!(
        per_op < 40.0,
        "{per_op:.1} encoded bytes per resolved op; the streamed body must stay proportional to the log"
    );
    assert!(per_op * 4.0 < std::mem::size_of::<MutationOp>() as f64);
}

/// Streaming must not change the log: the same commit appended either way
/// yields byte-identical files.
#[test]
fn append_resolved_writes_the_bytes_append_writes() {
    let (_, mut live) = seeded(200);
    run(&mut live, "MATCH (n:Item) WHERE n.id <= 50 DETACH DELETE n");
    run(
        &mut live,
        "MATCH (n:Item) WHERE n.id > 150 SET n.k = 'changed'",
    );
    run(&mut live, "CREATE (:Item {id: 1000, k: 'new'})");
    let raw = drain(&mut live);
    let ops = resolve_ops(&raw, &live);
    assert!(!ops.is_empty());

    let tmp = tempfile::tempdir().unwrap();
    let whole = tmp.path().join("whole.wal");
    let streamed = tmp.path().join("streamed.wal");
    let mut a = Wal::open(whole.clone(), SyncMode::PageCache).unwrap();
    a.append(&WalFrame { lsn: 7, ops }).unwrap();
    let mut b = Wal::open(streamed.clone(), SyncMode::PageCache).unwrap();
    b.append_resolved(7, &raw, &live).unwrap();
    drop((a, b));
    assert_eq!(
        std::fs::read(&whole).unwrap(),
        std::fs::read(&streamed).unwrap()
    );
}

/// The size cap applies to the streamed frame as it does to a whole one, and
/// is reported rather than written.
#[test]
fn a_streamed_frame_over_the_cap_is_refused() {
    let mut body = FrameBody::bounded(crate::serde_codec::CURRENT_CODEC, 16);
    for i in 0..10 {
        body.push(&MutationOp::RemoveNode {
            node_type: "Item".into(),
            id: Value::Int64(i),
        });
    }
    assert!(body.finish(1).is_err());
}

/// A bulk delete's frame, replayed over the checkpoint that predates it,
/// reproduces the live graph: the same nodes and edges are gone and the
/// untouched node survives.
#[test]
fn a_bulk_delete_frame_replays_to_the_committed_state() {
    let n = 600;
    let (mut checkpoint, mut live) = seeded(n);
    run(&mut live, "MATCH (n:Item) DETACH DELETE n");
    let raw = drain(&mut live);
    let frame = WalFrame {
        lsn: 1,
        ops: resolve_ops(&raw, &live),
    };
    assert_eq!(live.graph.node_count(), 1);
    assert_eq!(checkpoint.graph.node_count(), n + 1, "non-vacuity");
    apply_frames(&mut checkpoint, &[frame], 0).unwrap();
    assert_eq!(checkpoint.graph.node_count(), 1);
    assert_eq!(checkpoint.graph.edge_count(), 0);
    assert!(checkpoint.lookup_by_id("Keep", &Value::Int64(1)).is_some());
}
