//! A failed property-`SET`-only disk statement rolls back exactly through the
//! cell journal: cells, titles, grown schema and the column *types* a write
//! demoted. (That such a statement copies no column is `flush_tests`.)
use crate::datatypes::Value;
use crate::graph::schema::DirGraph;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::column_store::TypedColumn;
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};
use crate::graph::storage::GraphRead;
use std::collections::HashMap;

fn run(graph: &mut DirGraph, query: &str) -> Result<(), String> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(graph, query, &opts)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// A row fails the statement at `i = 250`: the duration's month count does not
/// fit, after 249 rows' worth of writes.
const FAIL_AT_250: &str = "CASE WHEN i = 250 THEN duration({months: 2147483648}) ELSE i END";

fn prop(graph: &DirGraph, id: i64, key: &str) -> Option<Value> {
    let idx = graph
        .graph
        .node_indices()
        .find(|i| graph.graph.get_node_id(*i) == Some(Value::Int64(id)))
        .expect("node exists");
    let _guard = graph.graph.begin_query();
    graph
        .graph
        .node_view(idx)
        .and_then(|n| n.get_property_value(key))
}

/// 2,000 staff: `id`, an int64 column `grade`, a string column `name` and a
/// string title.
fn staff_graph(dir: &std::path::Path) -> DirGraph {
    let mut graph = new_dir_graph_in_mode(StorageMode::Disk, Some(dir)).expect("disk graph");
    run(
        &mut graph,
        "UNWIND range(1, 2000) AS i \
         CREATE (:Staff {id: i, title: 'Employee ' + toString(i), grade: 0, name: 'n' + toString(i)})",
    )
    .unwrap();
    assert!(graph.graph.is_disk());
    graph
}

fn column_kind(graph: &DirGraph, key: &str) -> &'static str {
    let type_key = crate::graph::schema::InternedKey::from_str("Staff");
    let store = graph.graph.column_store(type_key).expect("Staff store");
    let slot = store
        .slot(crate::graph::schema::InternedKey::from_str(key))
        .expect("column exists");
    match store.column(slot as usize).expect("column") {
        TypedColumn::Int64 { .. } => "int64",
        TypedColumn::Str { .. } => "str",
        TypedColumn::Mixed { .. } => "mixed",
        _ => "other",
    }
}

fn title_kind(graph: &DirGraph) -> &'static str {
    let type_key = crate::graph::schema::InternedKey::from_str("Staff");
    let store = graph.graph.column_store(type_key).expect("Staff store");
    match store.title_column_ref() {
        Some(TypedColumn::Str { .. }) => "str",
        Some(TypedColumn::Mixed { .. }) => "mixed",
        Some(_) => "other",
        None => "none",
    }
}

#[test]
fn a_failed_disk_set_only_statement_restores_cells_titles_and_schema() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = staff_graph(dir.path());
    run(&mut graph, "MATCH (n:Staff {id: 2}) SET n.grade = 7").unwrap();
    let before_keys = graph.type_schemas.get("Staff").map(|s| s.len());
    let err = run(
        &mut graph,
        &format!(
            "UNWIND range(1, 300) AS i MATCH (n:Staff {{id: i}}) \
             SET n.grade = i * 10, n.fresh = i, n.title = 'changed', n.extra = {FAIL_AT_250}"
        ),
    );
    assert!(
        err.is_err(),
        "the statement must fail after its first writes"
    );
    for (id, grade) in [(1, 0), (2, 7), (249, 0), (250, 0), (300, 0)] {
        assert_eq!(
            prop(&graph, id, "grade"),
            Some(Value::Int64(grade)),
            "grade of {id}"
        );
        assert_eq!(prop(&graph, id, "fresh"), None, "fresh of {id}");
        assert_eq!(prop(&graph, id, "extra"), None, "extra of {id}");
        assert_eq!(
            prop(&graph, id, "title"),
            Some(Value::String(format!("Employee {id}"))),
            "title of {id}"
        );
    }
    assert_eq!(
        graph.type_schemas.get("Staff").map(|s| s.len()),
        before_keys
    );
    // The graph stays writable and the next statement lands.
    run(&mut graph, "MATCH (n:Staff {id: 3}) SET n.grade = 3").unwrap();
    assert_eq!(prop(&graph, 3, "grade"), Some(Value::Int64(3)));
    assert_eq!(prop(&graph, 4, "grade"), Some(Value::Int64(0)));
}

/// A `SET` the int64 column cannot hold demotes it to `Mixed`; rolled back, the
/// column must be the int64 column again — a `Mixed` column has no file
/// representation, so leaving it behind changes what the next save writes.
#[test]
fn a_rolled_back_demotion_restores_the_column_type() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = staff_graph(dir.path());
    assert_eq!(column_kind(&graph, "grade"), "int64");
    let err = run(
        &mut graph,
        &format!(
            "UNWIND range(1, 300) AS i MATCH (n:Staff {{id: i}}) \
             SET n.grade = CASE WHEN i = 100 THEN 'unknown' ELSE i END, n.extra = {FAIL_AT_250}"
        ),
    );
    assert!(err.is_err());
    assert_eq!(
        column_kind(&graph, "grade"),
        "int64",
        "the demotion was not undone"
    );
    for id in [1, 99, 100, 101, 300] {
        assert_eq!(
            prop(&graph, id, "grade"),
            Some(Value::Int64(0)),
            "grade of {id}"
        );
    }
    // Non-vacuity: the same write that succeeds does demote, so the assertion
    // above is about the rollback and not about a column that never changed.
    run(
        &mut graph,
        "MATCH (n:Staff {id: 100}) SET n.grade = 'unknown'",
    )
    .unwrap();
    assert_eq!(column_kind(&graph, "grade"), "mixed");
    assert_eq!(
        prop(&graph, 100, "grade"),
        Some(Value::String("unknown".into()))
    );
}

#[test]
fn a_rolled_back_title_demotion_restores_the_title_column() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = staff_graph(dir.path());
    let before = title_kind(&graph);
    assert_eq!(before, "str");
    let err = run(
        &mut graph,
        &format!(
            "UNWIND range(1, 300) AS i MATCH (n:Staff {{id: i}}) \
             SET n.title = CASE WHEN i = 100 THEN 42 ELSE 'changed' END, n.extra = {FAIL_AT_250}"
        ),
    );
    assert!(err.is_err());
    assert_eq!(
        title_kind(&graph),
        before,
        "the title demotion was not undone"
    );
    assert_eq!(
        prop(&graph, 100, "title"),
        Some(Value::String("Employee 100".into()))
    );
}
