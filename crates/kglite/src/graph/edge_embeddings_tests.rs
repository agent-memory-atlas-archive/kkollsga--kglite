use super::*;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::GraphWrite;
use std::collections::HashMap;
use std::collections::HashSet;

fn graph_with_parallel_edges() -> (DirGraph, EdgeIndex, EdgeIndex, EdgeIndex) {
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
    let other = GraphWrite::add_edge(
        &mut graph.graph,
        a,
        a,
        EdgeData::new("MENTIONS".into(), HashMap::new(), &mut graph.interner),
    );
    (graph, r1, r2, other)
}

#[test]
fn parallel_edges_keep_distinct_vectors_and_bump_once() {
    let (mut graph, r1, r2, _) = graph_with_parallel_edges();
    let before = graph.version();
    let report = upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0]), (r2, vec![0.0, 1.0])],
        Some("cosine"),
    )
    .unwrap();
    assert_eq!(report.changed, 2);
    assert_eq!(graph.version(), before + 1);
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.get(r1), Some(&[1.0, 0.0][..]));
    assert_eq!(store.get(r2), Some(&[0.0, 1.0][..]));
}

#[test]
fn identical_upsert_is_a_true_no_op() {
    let (mut graph, r1, _, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let before = graph.version();
    let report = upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    assert_eq!(report.changed, 0);
    assert_eq!(graph.version(), before);
}

#[test]
fn invalid_batch_is_atomic() {
    let (mut graph, r1, r2, other) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let before = graph.version();
    for bad in [
        vec![(r2, vec![0.0, 1.0]), (other, vec![1.0, 1.0])],
        vec![(r2, vec![0.0, 1.0]), (r2, vec![1.0, 1.0])],
        vec![(r2, vec![f32::NAN, 1.0])],
        vec![(r2, vec![1.0])],
    ] {
        assert!(upsert_edge_embeddings(&mut graph, "ASSERTS", "description", bad, None,).is_err());
        assert_eq!(graph.version(), before);
        let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(r1), Some(&[1.0, 0.0][..]));
        assert_eq!(store.get(r2), None);
    }
}

#[test]
fn removal_validates_every_edge_before_mutating() {
    let (mut graph, r1, r2, other) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0]), (r2, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let before = graph.version();
    assert!(remove_edge_embeddings(&mut graph, "ASSERTS", "description", &[r1, other],).is_err());
    assert_eq!(graph.version(), before);
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert!(store.get(r1).is_some());
    assert!(store.get(r2).is_some());
}

#[test]
fn detach_delete_prunes_incident_edge_vectors() {
    let (mut graph, r1, r2, self_loop) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0]), (r2, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    upsert_edge_embeddings(
        &mut graph,
        "MENTIONS",
        "description",
        vec![(self_loop, vec![0.5, 0.5])],
        None,
    )
    .unwrap();
    let (source, _) = graph.graph.edge_endpoints(r1).unwrap();

    crate::graph::mutation::maintain::detach_delete_nodes(&mut graph, &HashSet::from([source]));

    assert!(graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].is_empty());
    assert!(graph.edge_embeddings[&edge_store_key("MENTIONS", "description")].is_empty());
}

#[test]
fn detach_delete_prunes_edge_vectors_in_mapped_and_disk_modes() {
    use crate::graph::storage::mode::{convert_dir_graph_to_mode, StorageMode};

    for mode in [StorageMode::Mapped, StorageMode::Disk] {
        let (mut graph, r1, r2, _) = graph_with_parallel_edges();
        upsert_edge_embeddings(
            &mut graph,
            "ASSERTS",
            "description",
            vec![(r1, vec![1.0, 0.0]), (r2, vec![0.0, 1.0])],
            None,
        )
        .unwrap();
        let (source, _) = graph.graph.edge_endpoints(r1).unwrap();
        match mode {
            StorageMode::Mapped => convert_dir_graph_to_mode(&mut graph, mode).unwrap(),
            StorageMode::Disk => graph.enable_disk_mode().unwrap(),
            StorageMode::Memory => unreachable!(),
        }

        crate::graph::mutation::maintain::detach_delete_nodes(&mut graph, &HashSet::from([source]));

        assert!(graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].is_empty());
    }
}

#[test]
fn vacuum_remaps_vectors_to_their_surviving_parallel_edges() {
    let (mut graph, r1, r2, other) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r2, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    remove_edge_with_embeddings(&mut graph, r1).unwrap();

    graph.vacuum();

    let asserted: Vec<_> = graph
        .graph
        .edge_indices()
        .filter(|edge| {
            graph
                .graph
                .edge_weight(*edge)
                .unwrap()
                .connection_type_str(&graph.interner)
                == "ASSERTS"
        })
        .collect();
    assert_eq!(asserted.len(), 1);
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.get(asserted[0]), Some(&[0.0, 1.0][..]));
    assert_eq!(
        store.get(other),
        None,
        "a different edge never inherits the vector"
    );
}

#[test]
fn reused_edge_slot_never_inherits_the_deleted_vector() {
    let (mut graph, removed, survivor, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(removed, vec![1.0, 0.0]), (survivor, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let endpoints = graph.graph.edge_endpoints(removed).unwrap();
    remove_edge_with_embeddings(&mut graph, removed).unwrap();
    let replacement = GraphWrite::add_edge(
        &mut graph.graph,
        endpoints.0,
        endpoints.1,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );

    assert_eq!(replacement, removed, "the fixture must reuse the slot");
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.get(replacement), None);
    assert_eq!(store.get(survivor), Some(&[0.0, 1.0][..]));
}

#[test]
fn disk_compaction_remaps_vectors_after_tombstones_and_overflow() {
    let (mut graph, removed, survivor, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(survivor, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let endpoints = graph.graph.edge_endpoints(removed).unwrap();
    graph.enable_disk_mode().unwrap();
    remove_edge_with_embeddings(&mut graph, removed).unwrap();
    GraphWrite::add_edge(
        &mut graph.graph,
        endpoints.0,
        endpoints.1,
        EdgeData::new("MENTIONS".into(), HashMap::new(), &mut graph.interner),
    );

    assert_eq!(graph.compact_disk().unwrap(), 1);

    let surviving_assertion = {
        let _guard = graph.graph.begin_query();
        graph
            .graph
            .edge_indices()
            .find(|edge| {
                graph
                    .graph
                    .edge_weight(*edge)
                    .is_some_and(|weight| weight.connection_type_str(&graph.interner) == "ASSERTS")
            })
            .unwrap()
    };
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.len(), 1);
    assert_eq!(store.get(surviving_assertion), Some(&[0.0, 1.0][..]));
}

#[test]
fn heap_to_disk_materialization_remaps_vectors_across_sparse_edge_slots() {
    let (mut graph, removed, survivor, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(survivor, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    remove_edge_with_embeddings(&mut graph, removed).unwrap();

    graph.enable_disk_mode().unwrap();

    let surviving_assertion = {
        let _guard = graph.graph.begin_query();
        graph
            .graph
            .edge_indices()
            .find(|edge| {
                graph
                    .graph
                    .edge_weight(*edge)
                    .is_some_and(|weight| weight.connection_type_str(&graph.interner) == "ASSERTS")
            })
            .unwrap()
    };
    assert_eq!(surviving_assertion, EdgeIndex::new(0));
    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.get(surviving_assertion), Some(&[0.0, 1.0][..]));
    assert_eq!(store.len(), 1);
}

#[test]
fn rollback_restores_the_exact_edge_vector_slot() {
    let (mut graph, r1, r2, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0]), (r2, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
    remove_edge_with_embeddings(&mut graph, r1).unwrap();
    checkpoint.rollback(&mut graph);

    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.edges().collect::<Vec<_>>(), vec![r1, r2]);
    assert_eq!(store.get(r1), Some(&[1.0, 0.0][..]));
    assert_eq!(store.get(r2), Some(&[0.0, 1.0][..]));
}

#[test]
fn cloned_graphs_own_independent_edge_stores() {
    let (mut graph, r1, _, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let mut child = graph.clone();

    assert_eq!(
        remove_edge_embeddings(&mut child, "ASSERTS", "description", &[r1]).unwrap(),
        1
    );
    assert!(child.edge_embeddings[&edge_store_key("ASSERTS", "description")].is_empty());
    assert_eq!(
        graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].get(r1),
        Some(&[1.0, 0.0][..])
    );
}

#[test]
fn held_frozen_view_keeps_its_edge_vectors_after_writer_mutation() {
    let (mut writer, r1, _, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut writer,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let frozen = writer.clone();

    remove_edge_with_embeddings(&mut writer, r1).unwrap();

    assert!(writer.edge_embeddings[&edge_store_key("ASSERTS", "description")].is_empty());
    assert_eq!(
        frozen.edge_embeddings[&edge_store_key("ASSERTS", "description")].get(r1),
        Some(&[1.0, 0.0][..])
    );
}

#[test]
fn durable_private_writes_refuse_before_any_effect() {
    let (mut graph, r1, _, _) = graph_with_parallel_edges();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    graph.graph.wrap_for_durability();
    let before = graph.version();

    assert!(upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![0.0, 1.0])],
        None,
    )
    .is_err());
    assert!(remove_edge_embeddings(&mut graph, "ASSERTS", "description", &[r1]).is_err());

    assert_eq!(graph.version(), before);
    assert_eq!(
        graph.edge_embeddings[&edge_store_key("ASSERTS", "description")].get(r1),
        Some(&[1.0, 0.0][..])
    );
}

#[test]
fn extend_preserves_target_vector_and_never_copies_source_store() {
    let (mut target, target_edge, target_parallel, target_other) = graph_with_parallel_edges();
    remove_edge_with_embeddings(&mut target, target_parallel).unwrap();
    remove_edge_with_embeddings(&mut target, target_other).unwrap();
    upsert_edge_embeddings(
        &mut target,
        "ASSERTS",
        "description",
        vec![(target_edge, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let (mut source, source_edge, source_parallel, source_other) = graph_with_parallel_edges();
    remove_edge_with_embeddings(&mut source, source_parallel).unwrap();
    remove_edge_with_embeddings(&mut source, source_other).unwrap();
    upsert_edge_embeddings(
        &mut source,
        "ASSERTS",
        "description",
        vec![(source_edge, vec![0.0, 1.0])],
        None,
    )
    .unwrap();

    crate::graph::mutation::extend::extend_graph(&mut target, &source, Some("replace".to_string()))
        .unwrap();

    let store = &target.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.len(), 1);
    assert_eq!(store.get(target_edge), Some(&[1.0, 0.0][..]));
    assert_ne!(store.get(target_edge), Some(&[0.0, 1.0][..]));
}
