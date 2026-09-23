use super::*;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::recording::{resolve_ops, wrap_for_durability};
use crate::graph::storage::GraphWrite;
use crate::graph::wal::{EdgeGroupMemberPatchWal, EdgeVectorCellPatchWal, MutationOp, WalFrame};
use std::collections::HashMap;

fn parallel_group() -> (DirGraph, EdgeIndex, EdgeIndex) {
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
    let first = GraphWrite::add_edge(
        &mut graph.graph,
        source,
        target,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    let second = GraphWrite::add_edge(
        &mut graph.graph,
        source,
        target,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    (graph, first, second)
}

fn resolve_and_replay(writer: &mut DirGraph, checkpoint: &mut DirGraph) -> Vec<MutationOp> {
    let raw = writer.graph.recording_mut().unwrap().take_ops();
    let ops = resolve_ops(&raw, writer);
    crate::graph::mutation::wal_replay::apply_frames(
        checkpoint,
        &[WalFrame {
            lsn: 1,
            ops: ops.clone(),
        }],
        0,
    )
    .unwrap();
    ops
}

#[test]
fn removal_patch_clears_the_selected_cell_on_replay() {
    let (mut writer, first, second) = parallel_group();
    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(first, vec![1.0, 0.0]), (second, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let mut checkpoint = writer.clone();
    wrap_for_durability(&mut writer).unwrap();

    assert_eq!(
        remove_edge_embeddings(&mut writer, "ASSERTS", "description", &[first]).unwrap(),
        1
    );
    let ops = resolve_and_replay(&mut writer, &mut checkpoint);
    assert!(ops.iter().any(|op| matches!(
        op,
        MutationOp::PatchEdgeGroupEmbeddings { patch, .. }
            if patch.members.iter().any(|member| matches!(
                member,
                EdgeGroupMemberPatchWal::Prior { cells, .. }
                    if cells.iter().any(|cell| matches!(cell, EdgeVectorCellPatchWal::Clear))
            ))
    )));

    let recovered = &checkpoint.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(recovered.get(first), None);
    assert_eq!(recovered.get(second), Some(&[0.0, 1.0][..]));
}

#[test]
fn distinct_cell_writes_in_one_group_retain_both_prior_cells() {
    let (mut writer, first, second) = parallel_group();
    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(first, vec![1.0, 0.0]), (second, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let mut checkpoint = writer.clone();
    wrap_for_durability(&mut writer).unwrap();

    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(first, vec![0.8, 0.2])],
        None,
    )
    .unwrap();
    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(second, vec![0.3, 0.7])],
        None,
    )
    .unwrap();
    resolve_and_replay(&mut writer, &mut checkpoint);

    let recovered = &checkpoint.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(recovered.get(first), Some(&[0.8, 0.2][..]));
    assert_eq!(recovered.get(second), Some(&[0.3, 0.7][..]));
}

#[test]
fn repeated_write_to_one_cell_retains_the_earliest_base() {
    let (mut writer, first, second) = parallel_group();
    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(first, vec![1.0, 0.0]), (second, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let mut checkpoint = writer.clone();
    wrap_for_durability(&mut writer).unwrap();

    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(first, vec![0.8, 0.2])],
        None,
    )
    .unwrap();
    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(first, vec![0.6, 0.4])],
        None,
    )
    .unwrap();
    resolve_and_replay(&mut writer, &mut checkpoint);

    let recovered = &checkpoint.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(recovered.get(first), Some(&[0.6, 0.4][..]));
    assert_eq!(recovered.get(second), Some(&[0.0, 1.0][..]));
}
