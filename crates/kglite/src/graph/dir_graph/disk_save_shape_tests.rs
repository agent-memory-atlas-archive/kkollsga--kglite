//! A reopened disk graph keeps its serving shape across saves: every type the
//! first save put in the mmap-served `columns.bin` stays there after a write
//! and after a no-op save, instead of moving to an all-`Mixed` heap sidecar.
use super::DirGraph;
use crate::datatypes::{DataFrame, Value};
use crate::graph::mutation::maintain;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::column_store::TypedColumn;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

const PAND: i64 = 2_000;
const OTHER: i64 = 500;

fn add(graph: &mut DirGraph, node_type: &str, rows: i64, extra: &str) {
    let rows = (1..=rows)
        .map(|i| {
            vec![
                Value::Int64(i),
                Value::String(format!("{node_type}-{i}")),
                Value::Int64(i * 10),
            ]
        })
        .collect();
    let frame =
        DataFrame::from_cypher_rows(vec!["id".into(), "title".into(), extra.into()], rows).unwrap();
    maintain::add_nodes(
        graph,
        frame,
        node_type.into(),
        "id".into(),
        Some("title".into()),
        None,
    )
    .unwrap();
}

fn load_owned(path: &str) -> DirGraph {
    match Arc::try_unwrap(crate::graph::io::file::load_file(path).unwrap()) {
        Ok(graph) => graph,
        Err(_) => panic!("fresh load unexpectedly shared"),
    }
}

fn run(graph: &mut DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let result = execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    result.result.rows
}

/// Every type is served from the mmap `columns.bin`, and the files agree.
fn assert_mmap_served(graph: &DirGraph, path: &str, when: &str) {
    for node_type in ["Pand", "Other"] {
        let store = graph.column_store(node_type).expect("columnar type");
        assert!(
            store.has_mmap_base(),
            "{when}: {node_type} is no longer served from columns.bin"
        );
    }
    let current = std::fs::read_to_string(format!("{path}/CURRENT")).unwrap_or_default();
    let generation = std::path::Path::new(path)
        .join("generations")
        .join(current.trim());
    assert!(
        generation.join("seg_000/columns.bin").exists(),
        "{when}: the generation has no columns.bin"
    );
    assert!(
        !generation.join("columns").exists(),
        "{when}: the generation carries per-type sidecars"
    );
}

fn saved(path: &str) {
    let mut graph = DirGraph::new();
    add(&mut graph, "Pand", PAND, "rec_to");
    add(&mut graph, "Other", OTHER, "v");
    graph.enable_disk_mode().unwrap();
    graph.save_disk(path).unwrap();
}

#[test]
fn a_reopened_disk_graph_stays_mmap_served_across_write_saves() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    saved(path);
    assert_mmap_served(&load_owned(path), path, "after the first save");

    for cycle in 1..=3i64 {
        let mut graph = load_owned(path);
        run(
            &mut graph,
            &format!("MATCH (n:Pand) WHERE n.id <= 100 SET n.rec_to = {cycle}"),
        );
        graph.save_disk(path).unwrap();
        drop(graph);

        let mut graph = load_owned(path);
        assert_mmap_served(&graph, path, &format!("cycle {cycle}"));
        let rows = run(
            &mut graph,
            "MATCH (n:Pand) WHERE n.id IN [1, 100, 101] \
             RETURN n.id AS id, n.rec_to AS r, n.title AS t ORDER BY id",
        );
        assert_eq!(
            rows,
            vec![
                vec![
                    Value::Int64(1),
                    Value::Int64(cycle),
                    Value::String("Pand-1".into())
                ],
                vec![
                    Value::Int64(100),
                    Value::Int64(cycle),
                    Value::String("Pand-100".into())
                ],
                vec![
                    Value::Int64(101),
                    Value::Int64(1010),
                    Value::String("Pand-101".into())
                ],
            ],
            "cycle {cycle}"
        );
        let other = run(&mut graph, "MATCH (n:Other {id: 7}) RETURN n.v AS v");
        assert_eq!(other, vec![vec![Value::Int64(70)]], "cycle {cycle}");
    }
}

#[test]
fn a_no_op_save_of_a_reopened_disk_graph_keeps_its_shape() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    saved(path);
    for cycle in 1..=2 {
        let mut graph = load_owned(path);
        graph.save_disk(path).unwrap();
        drop(graph);
        let mut graph = load_owned(path);
        assert_mmap_served(&graph, path, &format!("no-op cycle {cycle}"));
        assert_eq!(
            run(&mut graph, "MATCH (n:Pand {id: 5}) RETURN n.rec_to AS r"),
            vec![vec![Value::Int64(50)]]
        );
    }
}

/// A store an earlier build already drifted — every column `Mixed`, the
/// shape `write_packed_from_mmap` left behind — is re-typed on save and goes
/// back into `columns.bin`.
#[test]
fn a_drifted_all_mixed_store_heals_on_save() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_str().unwrap();
    saved(path);
    let mut graph = load_owned(path);
    for node_type in ["Pand", "Other"] {
        let store = graph.column_store(node_type).unwrap();
        let meta = graph
            .node_type_metadata
            .get(node_type)
            .cloned()
            .unwrap_or_default();
        let mut owned = store.flattened_owned(&meta, &graph.interner);
        owned.demote_to_mixed_for_test();
        assert!(owned
            .columns_ref()
            .all(|c| matches!(c, TypedColumn::Mixed { .. })));
        graph.install_column_store(node_type, Arc::new(owned));
    }
    graph.save_disk(path).unwrap();
    drop(graph);

    let mut graph = load_owned(path);
    assert_mmap_served(&graph, path, "after healing");
    assert_eq!(
        run(
            &mut graph,
            "MATCH (n:Pand {id: 9}) RETURN n.rec_to AS r, n.title AS t"
        ),
        vec![vec![Value::Int64(90), Value::String("Pand-9".into())]]
    );
}
