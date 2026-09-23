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

/// Recovery folds only frames above the checkpoint's LSN, so re-presenting a
/// frame the checkpoint already absorbed is skipped; a patch forced through
/// anyway (an `after_lsn` below its LSN) is a no-op because the group already
/// carries the patch's result digest; and a patch whose base *and* result both
/// disagree with the group — the checkpoint moved on — is refused by name
/// rather than applied onto the wrong base.
#[test]
fn replaying_an_absorbed_edge_frame_is_idempotent_and_a_moved_base_is_refused() {
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
    let raw = writer.graph.recording_mut().unwrap().take_ops();
    let frame = WalFrame {
        lsn: 7,
        ops: resolve_ops(&raw, &writer),
    };
    assert!(
        frame
            .ops
            .iter()
            .any(|op| matches!(op, MutationOp::PatchEdgeGroupEmbeddings { .. })),
        "a write over an existing store is captured as a patch"
    );
    let frames = std::slice::from_ref(&frame);
    let key = edge_store_key("ASSERTS", "description");
    let snapshot = |graph: &DirGraph| {
        let store = &graph.edge_embeddings[&key];
        (
            store.get(first).map(<[f32]>::to_vec),
            store.get(second).map(<[f32]>::to_vec),
        )
    };

    let applied =
        crate::graph::mutation::wal_replay::apply_frames(&mut checkpoint, frames, 0).unwrap();
    assert_eq!(applied, 7);
    let after_first = snapshot(&checkpoint);
    assert_eq!(after_first.0.as_deref(), Some(&[0.8, 0.2][..]));
    assert_eq!(after_first.1.as_deref(), Some(&[0.0, 1.0][..]));

    let skipped =
        crate::graph::mutation::wal_replay::apply_frames(&mut checkpoint, frames, 7).unwrap();
    assert_eq!(
        skipped, 7,
        "nothing above the checkpoint LSN: reports the checkpoint"
    );
    assert_eq!(snapshot(&checkpoint), after_first);

    let forced =
        crate::graph::mutation::wal_replay::apply_frames(&mut checkpoint, frames, 0).unwrap();
    assert_eq!(forced, 7);
    assert_eq!(
        snapshot(&checkpoint),
        after_first,
        "the group already holds the patch's result: idempotent, not double-applied"
    );

    upsert_edge_embeddings(
        &mut checkpoint,
        "ASSERTS",
        "description",
        vec![(second, vec![0.5, 0.5])],
        None,
    )
    .unwrap();
    let moved = snapshot(&checkpoint);
    let error = crate::graph::mutation::wal_replay::apply_frames(&mut checkpoint, frames, 0)
        .expect_err("neither the base nor the result digest matches the moved group");
    assert!(error.contains("base digest mismatch"), "got: {error}");
    assert_eq!(
        snapshot(&checkpoint),
        moved,
        "the refusal publishes nothing"
    );
}
