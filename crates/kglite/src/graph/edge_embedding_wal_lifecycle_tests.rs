use super::*;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::recording::{resolve_ops, wrap_for_durability, RawOp};
use crate::graph::storage::GraphWrite;
use crate::graph::wal::MutationOp;
use std::collections::HashMap;

fn graph_with_edge() -> (DirGraph, EdgeIndex) {
    let mut graph = DirGraph::new();
    let source = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(1),
            Value::String("source".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let target = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(2),
            Value::String("target".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let edge = GraphWrite::add_edge(
        &mut graph.graph,
        source,
        target,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    (graph, edge)
}

fn note_group(graph: &mut DirGraph, edge: EdgeIndex) {
    graph
        .graph
        .recording_mut()
        .expect("durability wrapper")
        .note_wal_group(edge);
}
fn take_raw(graph: &mut DirGraph) -> Vec<RawOp> {
    graph
        .graph
        .recording_mut()
        .expect("durability wrapper")
        .take_ops()
}
fn set_revision(graph: &mut DirGraph, edge: EdgeIndex, revision: i64) {
    let key = graph.interner.get_or_intern("revision");
    let data = graph.graph.edge_weight_mut(edge).expect("live edge");
    data.properties.retain(|(name, _)| *name != key);
    data.properties.push((key, Value::Int64(revision)));
}

#[test]
fn vectorless_group_touch_retains_no_embedding_base_payload() {
    let (mut graph, edge) = graph_with_edge();
    wrap_for_durability(&mut graph).unwrap();
    note_group(&mut graph, edge);
    let raw = take_raw(&mut graph);
    let bases: Vec<_> = raw
        .iter()
        .filter_map(|op| match op {
            RawOp::WalGroup { base_members, .. } => Some(base_members),
            _ => None,
        })
        .collect();
    assert_eq!(bases.len(), 1);
    assert!(
        bases[0].is_empty(),
        "no-vector capture must retain no group snapshot"
    );
}

#[test]
fn first_store_creation_enables_later_property_delta_without_reopen() {
    let (mut graph, edge) = graph_with_edge();
    wrap_for_durability(&mut graph).unwrap();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(edge, vec![1.0, 0.0])],
        Some("cosine"),
    )
    .unwrap();
    take_raw(&mut graph);
    note_group(&mut graph, edge);
    set_revision(&mut graph, edge, 2);
    let raw = take_raw(&mut graph);
    let ops = resolve_ops(&raw, &graph);
    assert!(ops
        .iter()
        .any(|op| matches!(op, MutationOp::PatchEdgeGroupEmbeddings { .. })));
    assert!(!ops
        .iter()
        .any(|op| matches!(op, MutationOp::ReplaceEdgeGroupEmbeddings { .. })));
}

#[test]
fn repeated_group_touches_resolve_one_topology_and_one_embedding_op() {
    let (mut graph, edge) = graph_with_edge();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(edge, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    wrap_for_durability(&mut graph).unwrap();
    note_group(&mut graph, edge);
    set_revision(&mut graph, edge, 1);
    note_group(&mut graph, edge);
    set_revision(&mut graph, edge, 2);
    let raw = take_raw(&mut graph);
    let ops = resolve_ops(&raw, &graph);
    let topology = ops
        .iter()
        .filter(|op| matches!(op, MutationOp::ReplaceEdgeGroup { .. }))
        .count();
    let embeddings = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                MutationOp::PatchEdgeGroupEmbeddings { .. }
                    | MutationOp::ReplaceEdgeGroupEmbeddings { .. }
            )
        })
        .count();
    assert_eq!((topology, embeddings), (1, 1));
}

#[test]
fn dropped_store_still_closes_the_touched_group_embedding_state() {
    let (mut writer, edge) = graph_with_edge();
    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(edge, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let mut checkpoint = writer.clone();
    wrap_for_durability(&mut writer).unwrap();

    capture_wal_edge_embedding_bases(&mut writer, "ASSERTS", "description", std::iter::once(edge))
        .unwrap();
    writer
        .graph
        .recording_mut()
        .unwrap()
        .note_wal_edge_embedding_store("ASSERTS", "description");
    writer
        .edge_embeddings
        .remove(&edge_store_key("ASSERTS", "description"));
    note_group(&mut writer, edge);

    let ops = resolve_ops(&take_raw(&mut writer), &writer);
    assert!(ops.iter().any(|op| matches!(
        op,
        MutationOp::SetEdgeEmbeddingStore {
            state: crate::graph::wal::EdgeEmbeddingStoreState::Absent,
            ..
        }
    )));
    let closeouts: Vec<_> = ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                MutationOp::ReplaceEdgeGroupEmbeddings {
                    member_count: 1,
                    stores,
                    ..
                } if stores.is_empty()
            ) || matches!(
                op,
                MutationOp::PatchEdgeGroupEmbeddings { patch, .. }
                    if patch.stores.is_empty()
            )
        })
        .collect();
    assert_eq!(closeouts.len(), 1);

    crate::graph::mutation::wal_replay::apply_frames(
        &mut checkpoint,
        &[crate::graph::wal::WalFrame { lsn: 1, ops }],
        0,
    )
    .unwrap();
    assert!(!checkpoint
        .edge_embeddings
        .contains_key(&edge_store_key("ASSERTS", "description")));
}
