//! What a save's rebase keeps of the load that produced the graph (the
//! `columns`, `embeddings`, `topology` and `verify` choices and the write
//! budget setting), and how embedding writes count against the unsaved-write
//! budget and are released by that rebase.

use super::frozen_tests::flip_in_section;
use super::index_sections::with_floors;
use super::index_sections_tests::{opts, rows, run};
use super::mappable::SaveLayout;
use super::*;
use crate::datatypes::values::Value;
use crate::error::KgErrorCode;
use crate::graph::dir_graph::write_budget::{limit_for, WRITE_BUDGET_FLOOR_BYTES};
use crate::graph::dir_graph::{WriteBudgetSetting, WriteBudgetState};
use crate::graph::embeddings::{add_embeddings, set_embeddings};
use crate::graph::session::execute_mut;

const DIM: usize = 1024;
const DOCS: i64 = 1100;
const BIG: i64 = 40_000;
const MIB: u64 = 1 << 20;

fn vector_for(i: i64) -> Vec<f32> {
    (0..DIM)
        .map(|j| (((i * 31 + j as i64 * 17) % 101) as f32) / 50.0 - 1.0)
        .collect()
}

/// A file with a mappable column (`Big`), a mappable embedding store
/// (`Doc.text_emb`, 4.4 MiB) and relationships, saved with the topology
/// floors lowered so a mapped load serves it from the file.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("policy.kgl");
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        &format!(
            "UNWIND range(1, {BIG}) AS i CREATE (:Big {{id: (i * 7919) % 40009, seq: i, \
             score: (i * 7919) % 40009 * 104729, ratio: i / 7.0}})"
        ),
    );
    run(
        &mut graph,
        &format!("UNWIND range(1, {DOCS}) AS i CREATE (:Doc {{id: i, text: 'x', body: 'y'}})"),
    );
    run(
        &mut graph,
        "MATCH (a:Doc), (b:Doc) WHERE b.id = a.id + 1 CREATE (a)-[:NEXT]->(b)",
    );
    set_embeddings(
        &mut graph,
        "Doc",
        "text",
        Some("cosine"),
        (1..=DOCS).map(|i| (Value::Int64(i), vector_for(i))),
    )
    .unwrap();
    let mut arc = Arc::new(graph);
    with_floors(10, 10, || {
        save_graph_with_layout(&mut arc, path.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    (tmp, path)
}

/// A load that maps the 4.4 MiB embedding store (`Auto` maps from 64 MiB) and
/// the columns, over a topology served from the file.
fn mapped_options() -> LoadOptions {
    LoadOptions::new().with_embeddings(MapPolicy::Mapped)
}

fn load_with(path: &std::path::Path, options: LoadOptions) -> Arc<DirGraph> {
    load_file_with(
        path.to_str().unwrap(),
        &options.with_storage(StorageMode::Mapped),
    )
    .unwrap()
}

fn columns_mapped(graph: &DirGraph) -> bool {
    graph
        .column_store("Big")
        .is_some_and(|store| !store.shared_sections().is_empty())
}

fn embeddings_mapped(graph: &DirGraph) -> bool {
    graph.embeddings.values().all(|store| store.is_mapped())
}

/// Write one node, save to a fresh path (which rebases) and return the path.
fn write_and_save(
    graph: &mut Arc<DirGraph>,
    dir: &std::path::Path,
    name: &str,
) -> std::path::PathBuf {
    run(
        crate::graph::handle::make_dir_graph_mut(graph),
        "CREATE (:Big {id: 900001, seq: 900001, score: 1, ratio: 1.0})",
    );
    let target = dir.join(name);
    with_floors(10, 10, || {
        save_graph_with_layout(graph, target.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    target
}

#[cfg(not(windows))]
#[test]
fn a_default_load_serves_columns_and_embeddings_from_the_saved_file_after_a_save() {
    let (tmp, path) = fixture();
    let mut graph = load_with(&path, mapped_options());
    assert!(columns_mapped(&graph) && embeddings_mapped(&graph));
    write_and_save(&mut graph, tmp.path(), "after.kgl");
    assert!(graph.graph.is_frozen(), "rebased");
    assert!(columns_mapped(&graph), "columns mapped from the new file");
    assert!(
        embeddings_mapped(&graph),
        "embeddings mapped from the new file"
    );
}

#[cfg(not(windows))]
#[test]
fn a_memory_columns_load_keeps_its_columns_on_the_heap_after_a_save() {
    let (tmp, path) = fixture();
    let mut graph = load_with(&path, mapped_options().with_columns(MapPolicy::Memory));
    assert!(!columns_mapped(&graph));
    write_and_save(&mut graph, tmp.path(), "after.kgl");
    assert!(graph.graph.is_frozen(), "rebased");
    assert!(!columns_mapped(&graph), "columns stay on the heap");
    assert!(
        embeddings_mapped(&graph),
        "the other parts keep their policy"
    );
    assert_eq!(
        rows(&graph, "MATCH (b:Big) RETURN count(b)"),
        format!("[[Int64({})]]", BIG + 1)
    );
}

#[cfg(not(windows))]
#[test]
fn a_memory_embeddings_load_keeps_its_embeddings_on_the_heap_after_a_save() {
    let (tmp, path) = fixture();
    let mut graph = load_with(&path, mapped_options().with_embeddings(MapPolicy::Memory));
    assert!(!embeddings_mapped(&graph));
    write_and_save(&mut graph, tmp.path(), "after.kgl");
    assert!(graph.graph.is_frozen(), "rebased");
    assert!(!embeddings_mapped(&graph), "embeddings stay on the heap");
    assert!(columns_mapped(&graph));
    assert_eq!(
        graph.embeddings.values().next().unwrap().len(),
        DOCS as usize
    );
}

#[cfg(not(windows))]
#[test]
fn a_rebase_repeats_the_loads_verification_choice() {
    // The damaged copy never matches the live slot space, so the reason a
    // rebase declines for says how far its reload got: an eager reload stops
    // at the damaged section, a lazy one reads the file and fails the match.
    let outcome = |verify: Verify| {
        let (tmp, path) = fixture();
        let mut graph = load_with(&path, mapped_options().with_verify(verify));
        run(
            crate::graph::handle::make_dir_graph_mut(&mut graph),
            "CREATE (:Big {id: 900001, seq: 900001, score: 1, ratio: 1.0})",
        );
        let damaged = tmp.path().join("damaged.kgl");
        std::fs::copy(&path, &damaged).unwrap();
        flip_in_section(&damaged, "topo:out_edges");
        rebase::rebase_onto_saved(&mut graph, damaged.to_str().unwrap())
    };
    assert_eq!(
        outcome(Verify::Eager),
        rebase::Rebase::Declined("the saved file did not load"),
        "an eager load checks every section at the rebase's reload"
    );
    assert_eq!(
        outcome(Verify::Lazy),
        rebase::Rebase::Declined("the saved file does not describe the live slot space"),
        "a lazy load leaves the damaged section unchecked, as it did at load"
    );
}

#[cfg(not(windows))]
#[test]
fn a_rebase_keeps_the_write_budget_setting_and_recomputes_the_default() {
    for setting in [
        WriteBudgetSetting::Bytes(3 * MIB),
        WriteBudgetSetting::Unlimited,
        WriteBudgetSetting::Default,
    ] {
        let (tmp, path) = fixture();
        let mut graph = load_with(&path, mapped_options().with_write_budget(setting));
        let target = write_and_save(&mut graph, tmp.path(), "after.kgl");
        assert!(graph.graph.is_frozen(), "{setting:?}: rebased");
        let info = graph.graph_info();
        let file_bytes = std::fs::metadata(&target).unwrap().len();
        assert_eq!(
            info.write_budget_bytes,
            limit_for(setting, file_bytes),
            "{setting:?}"
        );
        match setting {
            WriteBudgetSetting::Unlimited => {
                assert_eq!(info.write_budget_state, WriteBudgetState::Unlimited);
            }
            WriteBudgetSetting::Default => {
                assert!(info.write_budget_bytes.unwrap() >= WRITE_BUDGET_FLOOR_BYTES);
            }
            WriteBudgetSetting::Bytes(bytes) => {
                assert_eq!(info.write_budget_bytes, Some(bytes));
            }
        }
    }
}

fn vectors(range: std::ops::Range<i64>) -> impl Iterator<Item = (Value, Vec<f32>)> {
    range.map(|i| (Value::Int64(i), vector_for(i)))
}

fn used(graph: &DirGraph) -> u64 {
    graph.graph_info().write_budget_used_bytes
}

/// 20 embedded-text nodes beyond the file's, so a new store has nodes to key on.
fn with_budget(limit: u64) -> (tempfile::TempDir, Arc<DirGraph>) {
    let (tmp, path) = fixture();
    let graph = load_with(
        &path,
        mapped_options().with_write_budget(WriteBudgetSetting::Bytes(limit)),
    );
    (tmp, graph)
}

fn is_budget_refusal(graph: &DirGraph) -> bool {
    graph
        .check_write_budget()
        .err()
        .is_some_and(|e| e.code() == KgErrorCode::WriteBudgetExceeded)
}

#[test]
fn embedding_heap_counts_against_the_budget_but_a_mapped_store_does_not() {
    let (_tmp, mut graph) = with_budget(3 * MIB);
    assert!(embeddings_mapped(&graph));
    assert_eq!(
        used(&graph),
        0,
        "a store served from the file costs nothing"
    );

    // A new store is heap: 400 x 1024 x 4 = 1.6 MiB, past half the limit.
    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    add_embeddings(dir, "Doc", "body", Some("cosine"), vectors(1..401)).unwrap();
    let after_first = used(&graph);
    assert!(after_first >= 400 * DIM as u64 * 4, "{after_first}");
    assert_eq!(state(&graph), WriteBudgetState::Warning);
    assert!(graph.check_write_budget().is_ok(), "under the limit");

    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    add_embeddings(dir, "Doc", "body", None, vectors(401..801)).unwrap();
    assert!(used(&graph) > 3 * MIB);
    assert_eq!(state(&graph), WriteBudgetState::Exceeded);
    assert!(is_budget_refusal(&graph), "the next write is refused");
}

fn state(graph: &DirGraph) -> WriteBudgetState {
    graph.graph_info().write_budget_state
}

#[test]
fn writing_to_a_mapped_store_counts_the_whole_copy_it_makes() {
    let (_tmp, mut graph) = with_budget(3 * MIB);
    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    add_embeddings(dir, "Doc", "text", None, vectors(1..2)).unwrap();
    assert!(
        !embeddings_mapped(&graph),
        "the write copied the store to the heap"
    );
    assert!(used(&graph) >= 4 * MIB, "{}", used(&graph));
    assert!(is_budget_refusal(&graph));
}

#[test]
fn a_replaced_store_gives_its_share_back() {
    let (_tmp, mut graph) = with_budget(8 * MIB);
    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    add_embeddings(dir, "Doc", "body", Some("cosine"), vectors(1..901)).unwrap();
    let big = used(&graph);
    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    set_embeddings(dir, "Doc", "body", Some("cosine"), vectors(1..11)).unwrap();
    assert!(used(&graph) < big / 10, "{} vs {big}", used(&graph));
}

#[test]
fn memory_storage_carries_no_embedding_budget() {
    let (_tmp, path) = fixture();
    let mut graph = load_file_with(
        path.to_str().unwrap(),
        &LoadOptions::new()
            .with_storage(StorageMode::Memory)
            .with_write_budget(WriteBudgetSetting::Bytes(1)),
    )
    .unwrap();
    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    add_embeddings(dir, "Doc", "body", Some("cosine"), vectors(1..401)).unwrap();
    assert_eq!(state(&graph), WriteBudgetState::NotApplicable);
    assert!(graph.check_write_budget().is_ok());
}

#[cfg(not(windows))]
#[test]
fn a_save_releases_embedding_heap_and_the_budget_starts_over() {
    let (tmp, mut graph) = with_budget(3 * MIB);
    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    // 1100 x 1024 x 4 bytes: over the floor a save writes a store raw from.
    add_embeddings(dir, "Doc", "body", Some("cosine"), vectors(1..1101)).unwrap();
    assert!(is_budget_refusal(&graph));

    // No overlay entry: only the embedding stores hold the unsaved heap.
    assert!(graph.graph.overlay_entries().is_none_or(|n| n == 0));
    let target = tmp.path().join("saved.kgl");
    with_floors(10, 10, || {
        save_graph_with_layout(&mut graph, target.to_str().unwrap(), true, SaveLayout::Auto)
    })
    .unwrap();
    assert!(
        embeddings_mapped(&graph),
        "both stores serve from the saved file"
    );
    assert_eq!(graph.embeddings.len(), 2);
    assert_eq!(used(&graph), 0);
    assert!(graph.check_write_budget().is_ok());
    let body = graph
        .embeddings
        .iter()
        .find(|((_, name), _)| name == "body_emb")
        .map(|(_, store)| store)
        .expect("the new store");
    assert_eq!(body.len(), 1100);
}

#[test]
fn a_statement_in_a_budgeted_session_is_refused_over_embedding_heap() {
    let (_tmp, mut graph) = with_budget(2 * MIB);
    let dir = crate::graph::handle::make_dir_graph_mut(&mut graph);
    add_embeddings(dir, "Doc", "body", Some("cosine"), vectors(1..801)).unwrap();
    let before = rows(&graph, "MATCH (n:Doc) RETURN count(n)");
    let error = execute_mut(
        crate::graph::handle::make_dir_graph_mut(&mut graph),
        "CREATE (:Doc {id: 777777, text: 'z'})",
        &opts(),
    )
    .err()
    .expect("refused");
    assert_eq!(error.code(), KgErrorCode::WriteBudgetExceeded);
    assert_eq!(before, rows(&graph, "MATCH (n:Doc) RETURN count(n)"));
}
