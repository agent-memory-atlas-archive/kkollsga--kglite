use super::*;
use crate::datatypes::Value;
use crate::graph::schema::{EdgeData, NodeData};
use crate::graph::storage::mode::StorageMode;
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

// ── manual-write rollback ───────────────────────────────────────────────
//
// `db.relationship_embeddings.set` / `.remove` write through `upsert_edge_embeddings`
// and `remove_edge_embeddings`. A statement that fails *after* one of those
// calls must leave the store exactly as it found it — vectors in their dense
// slot order, text hashes, `model_id`, store existence, and the HNSW index's
// coverage. Memory and Mapped reverse the write from the undo journal; Disk
// restores the whole-graph clone, so it is the control that must already pass.

/// Every observable property of the `ASSERTS.description` store, in dense slot
/// order — slot order is scan order, and scan order decides score ties.
#[derive(Debug, PartialEq)]
struct EdgeStoreFacts {
    dimension: usize,
    metric: Option<String>,
    model_id: Option<String>,
    cells: Vec<(usize, Vec<f32>, Option<u64>)>,
}

fn store_facts(graph: &DirGraph) -> Option<EdgeStoreFacts> {
    let store = graph
        .edge_embeddings
        .get(&edge_store_key("ASSERTS", "description"))?;
    Some(EdgeStoreFacts {
        dimension: store.dimension(),
        metric: store.metric().map(str::to_owned),
        model_id: store.model_id().map(str::to_owned),
        cells: store
            .edges()
            .map(|edge| {
                (
                    edge.index(),
                    store
                        .get(edge)
                        .expect("a listed edge holds a vector")
                        .to_vec(),
                    store.text_hash(edge),
                )
            })
            .collect(),
    })
}

/// Index coverage as `db.relationship_embeddings.list` reports it. Read *before* any
/// query: a query on a stale index auto-refreshes through interior
/// mutability, which would erase the very delta this compares.
fn index_facts(graph: &DirGraph) -> Vec<vector_index::EdgeVectorIndexStatus> {
    vector_index::list_edge_vector_indexes(graph)
}

fn index_query(graph: &DirGraph) -> Option<vector_index::EdgeVectorQueryReport> {
    vector_index::query_edge_embeddings(
        graph,
        "ASSERTS",
        "description",
        &[1.0, 0.0],
        vector_index::EdgeVectorQueryOptions {
            top_k: 5,
            exact: false,
            metric: None,
        },
    )
    .ok()
}

fn mode_graph(mode: StorageMode) -> (DirGraph, [EdgeIndex; 3]) {
    let (mut graph, r1, r2) = graph();
    let (source, target) = graph.graph.edge_endpoints(r1).unwrap();
    let r3 = GraphWrite::add_edge(
        &mut graph.graph,
        source,
        target,
        EdgeData::new("ASSERTS".into(), HashMap::new(), &mut graph.interner),
    );
    match mode {
        StorageMode::Memory => {}
        StorageMode::Mapped => {
            crate::graph::storage::mode::convert_dir_graph_to_mode(&mut graph, mode).unwrap()
        }
        StorageMode::Disk => graph.enable_disk_mode().unwrap(),
    }
    (graph, [r1, r2, r3])
}

/// Two of the three relationships embedded with hashes and a model stamp, and
/// an HNSW index covering both.
fn seeded_graph(mode: StorageMode) -> (DirGraph, [EdgeIndex; 3]) {
    let (mut graph, edges) = mode_graph(mode);
    install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(
            2,
            vec![
                (edges[0], vec![1.0, 0.0], 11),
                (edges[1], vec![0.0, 1.0], 22),
            ],
            vec![],
            vec![edges[0], edges[1]],
        ),
    )
    .unwrap();
    vector_index::build_edge_vector_index(
        &mut graph,
        "ASSERTS",
        "description",
        vector_index::EdgeVectorIndexOptions::default(),
    )
    .unwrap();
    (graph, edges)
}

#[test]
fn manual_set_rolls_back_with_its_statement_in_every_storage_mode() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let (mut graph, [r1, _r2, r3]) = seeded_graph(mode);
        let before_facts = store_facts(&graph);
        let before_index = index_facts(&graph);
        let before_query = index_query(&graph);
        let before_version = graph.version();

        let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
        upsert_edge_embeddings(
            &mut graph,
            "ASSERTS",
            "description",
            // One overwrite of an indexed slot and one append past the
            // index's watermark: the two shapes `set_embedding` takes.
            vec![(r1, vec![0.25, 0.75]), (r3, vec![0.6, 0.8])],
            None,
        )
        .unwrap();
        checkpoint.rollback(&mut graph);

        assert_eq!(store_facts(&graph), before_facts, "{mode:?}");
        assert_eq!(index_facts(&graph), before_index, "{mode:?}");
        assert_eq!(index_query(&graph), before_query, "{mode:?}");
        assert_eq!(graph.version(), before_version, "{mode:?}");
    }
}

#[test]
fn manual_set_that_creates_a_store_leaves_none_behind_after_rollback() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let (mut graph, [r1, ..]) = mode_graph(mode);
        let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
        upsert_edge_embeddings(
            &mut graph,
            "ASSERTS",
            "description",
            vec![(r1, vec![1.0, 0.0])],
            Some("cosine"),
        )
        .unwrap();
        checkpoint.rollback(&mut graph);

        assert!(
            graph.edge_embeddings.is_empty(),
            "{mode:?}: a rolled-back set must not leave the store it created"
        );
    }
}

#[test]
fn manual_remove_rolls_back_with_its_statement_in_every_storage_mode() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let (mut graph, [r1, ..]) = seeded_graph(mode);
        let before_facts = store_facts(&graph);
        let before_index = index_facts(&graph);
        let before_query = index_query(&graph);
        let before_version = graph.version();

        let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
        // Slot 0, so the removal also tail-swaps slot 1 down; the restore has
        // to reverse the swap, not merely re-add the vector.
        assert_eq!(
            remove_edge_embeddings(&mut graph, "ASSERTS", "description", &[r1]).unwrap(),
            1
        );
        checkpoint.rollback(&mut graph);

        assert_eq!(store_facts(&graph), before_facts, "{mode:?}");
        assert_eq!(index_facts(&graph), before_index, "{mode:?}");
        assert_eq!(index_query(&graph), before_query, "{mode:?}");
        assert_eq!(graph.version(), before_version, "{mode:?}");
    }
}

/// The user-visible shape of the defect: a `CALL db.relationship_embeddings.set`
/// followed by a clause that fails. Unlike the primitive tests above this runs
/// through `execute_mut`, which is where the statement checkpoint is opened —
/// so it also proves the manual write happens inside one.
fn cypher_seeded_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    run_cypher(
        &mut graph,
        "CREATE (a:Doc {id: 1}), (b:Doc {id: 2}), (c:Doc {id: 3}), \
         (a)-[:ASSERTS {text: 'alpha'}]->(b), \
         (a)-[:ASSERTS {text: 'beta'}]->(b), \
         (b)-[:ASSERTS]->(c)",
    )
    .unwrap();
    let asserts: Vec<EdgeIndex> = graph.graph.edge_indices().take(2).collect();
    install_generated_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        write(
            2,
            vec![
                (asserts[0], vec![1.0, 0.0], 11),
                (asserts[1], vec![0.0, 1.0], 22),
            ],
            vec![],
            vec![asserts[0], asserts[1]],
        ),
    )
    .unwrap();
    vector_index::build_edge_vector_index(
        &mut graph,
        "ASSERTS",
        "description",
        vector_index::EdgeVectorIndexOptions::default(),
    )
    .unwrap();
    graph
}

fn run_cypher(graph: &mut DirGraph, source: &str) -> Result<(), String> {
    let params = HashMap::new();
    crate::graph::session::execute::execute_mut(
        graph,
        source,
        &crate::graph::session::execute::ExecuteOptions::eager(&params),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// `c` still has an incoming `ASSERTS`, so the non-DETACH delete is refused —
/// a failure *after* the embedding write, not a validation refusal before it.
const FAILING_TAIL: &str = " MATCH (n:Doc {id: 3}) DELETE n RETURN kept";

#[test]
fn a_failed_statement_reverses_db_edge_embeddings_set() {
    let mut graph = cypher_seeded_graph();
    let before_facts = store_facts(&graph);
    let before_index = index_facts(&graph);
    let error = run_cypher(
        &mut graph,
        &format!(
            "MATCH ()-[r:ASSERTS]->() WHERE r.text = 'alpha' \
             CALL db.relationship_embeddings.set({{type:'ASSERTS', text_column:'description', \
             entries:[{{relationship:r, vector:[0.25,0.75]}}]}}) YIELD stored \
             WITH stored AS kept{FAILING_TAIL}"
        ),
    )
    .expect_err("the delete must fail after the embedding write");
    assert!(error.contains("DETACH DELETE"), "{error}");
    assert_eq!(store_facts(&graph), before_facts);
    assert_eq!(index_facts(&graph), before_index);
}

#[test]
fn a_failed_statement_reverses_db_edge_embeddings_remove() {
    let mut graph = cypher_seeded_graph();
    let before_facts = store_facts(&graph);
    let before_index = index_facts(&graph);
    let error = run_cypher(
        &mut graph,
        &format!(
            "MATCH ()-[r:ASSERTS]->() WHERE r.text = 'alpha' \
             CALL db.relationship_embeddings.remove({{type:'ASSERTS', text_column:'description', \
             relationships:[r]}}) YIELD removed \
             WITH removed AS kept{FAILING_TAIL}"
        ),
    )
    .expect_err("the delete must fail after the embedding write");
    assert!(error.contains("DETACH DELETE"), "{error}");
    assert_eq!(store_facts(&graph), before_facts);
    assert_eq!(index_facts(&graph), before_index);
}

/// A manual write takes ownership of the cell it writes, and `set_manual`
/// clears the generated `text_hash` that says otherwise. Selecting the batch by
/// vector equality alone skipped that clear whenever the caller wrote back a
/// byte-identical vector, so `embed(mode:'changed')` went on comparing the
/// generated hash and skipped a relationship the manual write owned.
#[test]
fn a_manual_set_of_an_identical_vector_still_clears_the_generated_hash() {
    let (mut graph, [r1, r2, _]) = seeded_graph(StorageMode::Memory);
    let report = upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    assert_eq!(report.changed, 1, "the hash clear is the change");

    let store = &graph.edge_embeddings[&edge_store_key("ASSERTS", "description")];
    assert_eq!(store.get(r1), Some(&[1.0, 0.0][..]));
    assert_eq!(store.text_hash(r1), None, "the manual write owns this cell");
    assert_eq!(
        store.text_hash(r2),
        Some(22),
        "an unselected cell keeps its generated hash"
    );
    assert_eq!(store.model_id(), None);
}

/// The undo journal has to cover the hash-only cells the fix above adds to the
/// batch, not just the ones whose vector moved.
#[test]
fn a_hash_only_manual_set_rolls_back_with_its_statement() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let (mut graph, [r1, ..]) = seeded_graph(mode);
        let before_facts = store_facts(&graph);
        let before_index = index_facts(&graph);
        let before_query = index_query(&graph);
        let before_version = graph.version();

        let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
        upsert_edge_embeddings(
            &mut graph,
            "ASSERTS",
            "description",
            vec![(r1, vec![1.0, 0.0])],
            None,
        )
        .unwrap();
        checkpoint.rollback(&mut graph);

        assert_eq!(store_facts(&graph), before_facts, "{mode:?}");
        assert_eq!(index_facts(&graph), before_index, "{mode:?}");
        assert_eq!(index_query(&graph), before_query, "{mode:?}");
        assert_eq!(graph.version(), before_version, "{mode:?}");
    }
}

/// A rolled-back relationship delete leaves the HNSW index where it found it.
/// The prune invalidates the index and the undo's `restore` invalidates it
/// again, so without the captured state the statement's failure silently
/// dropped an index it never touched — the vectors came back unindexed and
/// `list` reported `index_state: 'none'`.
#[test]
fn a_rolled_back_relationship_delete_restores_the_vector_index_in_every_storage_mode() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let (mut graph, [r1, ..]) = seeded_graph(mode);
        let before_facts = store_facts(&graph);
        let before_index = index_facts(&graph);
        let before_query = index_query(&graph);

        let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
        assert!(
            remove_edge_with_embeddings(&mut graph, r1).is_some(),
            "{mode:?}"
        );
        checkpoint.rollback(&mut graph);

        assert_eq!(store_facts(&graph), before_facts, "{mode:?}");
        assert_eq!(index_facts(&graph), before_index, "{mode:?}");
        assert_eq!(index_query(&graph), before_query, "{mode:?}");
    }
}

/// The same defect through the user-visible path: a `DELETE` followed by a
/// clause that fails.
#[test]
fn a_failed_statement_reverses_a_relationship_delete_with_its_vector_index() {
    let mut graph = cypher_seeded_graph();
    let before_facts = store_facts(&graph);
    let before_index = index_facts(&graph);
    let before_query = index_query(&graph);
    let error = run_cypher(
        &mut graph,
        &format!(
            "MATCH ()-[r:ASSERTS]->() WHERE r.text = 'alpha' DELETE r \
             WITH 1 AS kept{FAILING_TAIL}"
        ),
    )
    .expect_err("the delete must fail after the relationship delete");
    assert!(error.contains("DETACH DELETE"), "{error}");
    assert_eq!(store_facts(&graph), before_facts);
    assert_eq!(index_facts(&graph), before_index);
    assert_eq!(index_query(&graph), before_query);
}

/// An explicit `metric` on a manual write becomes the store's metric when the
/// store declares none, exactly as `build_index` records one — otherwise a
/// store created without a metric never declares one, the build-time
/// contradiction check has nothing to compare against, and a default query
/// route mismatches the index it just built.
#[test]
fn a_manual_set_with_an_explicit_metric_stamps_an_undeclared_store() {
    let (mut graph, r1, r2) = graph();
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        None,
    )
    .unwrap();
    let key = edge_store_key("ASSERTS", "description");
    assert_eq!(graph.edge_embeddings[&key].metric(), None);

    let version = graph.version();
    let report = upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "description",
        vec![(r1, vec![1.0, 0.0])],
        Some("cosine"),
    )
    .unwrap();
    assert_eq!(graph.edge_embeddings[&key].metric(), Some("cosine"));
    assert_eq!(report.changed, 0, "the vector itself did not move");
    assert!(graph.version() > version, "a metadata move is a mutation");

    // The declared metric now makes a contradicting build refuse.
    let error = vector_index::build_edge_vector_index(
        &mut graph,
        "ASSERTS",
        "description",
        vector_index::EdgeVectorIndexOptions {
            metric: Some("euclidean".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("declares metric 'cosine'"), "{error}");

    // A statement that stamps the metric and then fails restores the store.
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "note",
        vec![(r2, vec![0.0, 1.0])],
        None,
    )
    .unwrap();
    let note_key = edge_store_key("ASSERTS", "note");
    let checkpoint = crate::graph::dir_graph::rollback::StatementCheckpoint::open(&mut graph);
    upsert_edge_embeddings(
        &mut graph,
        "ASSERTS",
        "note",
        vec![(r2, vec![0.0, 1.0])],
        Some("cosine"),
    )
    .unwrap();
    assert_eq!(graph.edge_embeddings[&note_key].metric(), Some("cosine"));
    checkpoint.rollback(&mut graph);
    assert_eq!(graph.edge_embeddings[&note_key].metric(), None);
    assert_eq!(
        graph.edge_embeddings[&note_key].get(r2),
        Some(&[0.0, 1.0][..])
    );
}
