use super::*;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::recording::{resolve_ops, wrap_for_durability};
use crate::graph::storage::GraphWrite;
use crate::graph::wal::{MutationOp, WalFrame};

fn graph() -> (DirGraph, EdgeIndex, EdgeIndex) {
    let mut graph = DirGraph::new();
    let a = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(1),
            Value::String("a".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let b = GraphWrite::add_node(
        &mut graph.graph,
        NodeData::new(
            Value::Int64(2),
            Value::String("b".into()),
            "Doc".into(),
            HashMap::new(),
            &mut graph.interner,
        ),
    );
    let r1 = GraphWrite::add_edge(
        &mut graph.graph,
        a,
        b,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    let r2 = GraphWrite::add_edge(
        &mut graph.graph,
        a,
        b,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    (graph, r1, r2)
}

fn write(
    dimension: usize,
    generated: Vec<(EdgeIndex, Vec<f32>, u64)>,
    remove_selected: Vec<EdgeIndex>,
    affected: Vec<EdgeIndex>,
) -> GeneratedEdgeEmbeddingWrite {
    GeneratedEdgeEmbeddingWrite {
        dimension,
        metric: Some("cosine".into()),
        final_model_id: Some("model-b".into()),
        generated,
        remove_selected,
        affected,
    }
}

#[test]
fn generated_successor_installs_hashes_metadata_and_bumps_once() {
    let (mut graph, r1, r2) = graph();
    let before = graph.version();
    let report = install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(
            2,
            vec![(r1, vec![1.0, 0.0], 11), (r2, vec![0.0, 1.0], 22)],
            vec![],
            vec![r1, r2],
        ),
    )
    .unwrap();
    assert_eq!(graph.version(), before + 1);
    assert_eq!(report.changed, 2);
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.model_id(), Some("model-b"));
    assert_eq!(store.metric(), Some("cosine"));
    assert_eq!(store.text_hash(r1), Some(11));
    assert_eq!(store.text_hash(r2), Some(22));
}

#[test]
fn generated_invalid_batch_is_atomic() {
    let (mut graph, r1, r2) = graph();
    install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(2, vec![(r1, vec![1.0, 0.0], 11)], vec![], vec![r1]),
    )
    .unwrap();
    let before = graph.clone();
    assert!(install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(2, vec![(r2, vec![f32::NAN, 0.0], 22)], vec![], vec![r2])
    )
    .is_err());
    assert_eq!(graph.version(), before.version());
    assert_eq!(
        graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].get(r1),
        Some(&[1.0, 0.0][..])
    );
}

#[test]
fn width_change_requires_all_stored_vectors_but_not_unstored_edges() {
    let (mut graph, r1, r2) = graph();
    install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(2, vec![(r1, vec![1.0, 0.0], 11)], vec![], vec![r1]),
    )
    .unwrap();
    install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(3, vec![(r1, vec![1.0, 0.0, 0.0], 12)], vec![], vec![r1]),
    )
    .unwrap();
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.dimension(), 3);
    assert_eq!(store.get(r1), Some(&[1.0, 0.0, 0.0][..]));
    assert_eq!(store.get(r2), None);

    assert!(install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(4, vec![(r2, vec![0.0; 4], 22)], vec![], vec![r2])
    )
    .is_err());
    assert_eq!(
        graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].dimension(),
        3
    );
}

#[test]
fn drop_records_absence_and_disables_capture_after_last_store() {
    let (mut writer, r1, _) = graph();
    install_generated_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        write(2, vec![(r1, vec![1.0, 0.0], 11)], vec![], vec![r1]),
    )
    .unwrap();
    let mut checkpoint = writer.clone();
    wrap_for_durability(&mut writer).unwrap();
    assert!(drop_edge_embedding_store(&mut writer, "ASSERTS", "description").unwrap());
    assert!(!drop_edge_embedding_store(&mut writer, "ASSERTS", "description").unwrap());
    let raw = writer.graph.recording_mut().unwrap().take_ops();
    let ops = resolve_ops(&raw, &writer);
    assert!(ops.iter().any(|op| matches!(
        op,
        MutationOp::SetEdgeEmbeddingStore {
            state: crate::graph::wal::EdgeEmbeddingStoreState::Absent,
            ..
        }
    )));
    crate::graph::mutation::wal_replay::apply_frames(
        &mut checkpoint,
        &[WalFrame { lsn: 1, ops }],
        0,
    )
    .unwrap();
    assert!(checkpoint.edge_embeddings.is_empty());
}

#[test]
fn rollback_restores_a_dropped_store_and_embedding_base_capture() {
    let (mut graph, r1, _) = graph();
    install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(2, vec![(r1, vec![1.0, 0.0], 11)], vec![], vec![r1]),
    )
    .unwrap();
    wrap_for_durability(&mut graph).unwrap();

    let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
    drop_edge_embedding_store(&mut graph, "ASSERTS", "description").unwrap();
    checkpoint.rollback(&mut graph);

    let key = graph.interner.get_or_intern("revision");
    graph
        .graph
        .edge_weight_mut(r1)
        .unwrap()
        .properties
        .push((key, Value::Int64(1)));
    let raw = graph.graph.recording_mut().unwrap().take_ops();
    assert!(raw.iter().any(|op| matches!(
        op,
        crate::graph::storage::recording::RawOp::WalGroup { base_members, .. }
            if !base_members.is_empty()
    )));
    assert_eq!(
        graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].get(r1),
        Some(&[1.0, 0.0][..])
    );
}

#[test]
fn generated_all_removes_missing_text_cells_in_one_successor() {
    let (mut graph, r1, r2) = graph();
    install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(
            2,
            vec![(r1, vec![1.0, 0.0], 11), (r2, vec![0.0, 1.0], 22)],
            vec![],
            vec![r1, r2],
        ),
    )
    .unwrap();
    let before = graph.version();
    let report = install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(2, vec![(r1, vec![0.5, 0.5], 33)], vec![r2], vec![r1, r2]),
    )
    .unwrap();
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(graph.version(), before + 1);
    assert_eq!(report.changed, 2);
    assert_eq!(store.get(r1), Some(&[0.5, 0.5][..]));
    assert_eq!(store.text_hash(r1), Some(33));
    assert_eq!(store.get(r2), None);
}

#[test]
fn identical_generated_successor_is_a_true_no_op() {
    let (mut graph, r1, _) = graph();
    let batch = || write(2, vec![(r1, vec![1.0, 0.0], 11)], vec![], vec![r1]);
    install_generated_edge_embeddings(&mut graph, "ASSERTS", "description", batch()).unwrap();
    let before = graph.version();
    let report =
        install_generated_edge_embeddings(&mut graph, "ASSERTS", "description", batch()).unwrap();
    assert_eq!(report.changed, 0);
    assert_eq!(graph.version(), before);
}
