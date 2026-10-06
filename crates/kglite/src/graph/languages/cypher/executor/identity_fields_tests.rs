//! A node has one title. `CREATE` and `MERGE`'s create arm refuse a declared
//! title field and `title` that disagree; on a type with no declared title
//! field, `title` is the title and `name` stays a readable property. With no
//! title, a type titled by its ids takes the id, any other `<Label>_<id>`.

use crate::datatypes::values::{DataFrame, Value};
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::maintain::add_nodes;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn write(graph: &mut DirGraph, query: &str) -> Result<(), String> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows
}

fn s(value: &str) -> Value {
    Value::String(value.into())
}

#[test]
fn a_declared_title_field_and_a_different_title_are_refused() {
    let mut graph = DirGraph::new();
    let df = DataFrame::from_cypher_rows(
        vec!["id".into(), "label".into()],
        vec![vec![Value::Int64(1), s("L1")]],
    )
    .unwrap();
    add_nodes(
        &mut graph,
        df,
        "T".into(),
        "id".into(),
        Some("label".into()),
        None,
    )
    .unwrap();
    for query in [
        "CREATE (:T {id: 2, label: 'L2', title: 'Ann'})",
        "MERGE (:T {id: 2, label: 'L2', title: 'Ann'})",
    ] {
        let error = write(&mut graph, query).unwrap_err();
        assert!(error.contains("two different titles"), "{query}: {error}");
    }
    write(&mut graph, "CREATE (:T {id: 3, label: 'L3', title: 'L3'})").unwrap();
    assert_eq!(
        rows(&graph, "MATCH (n:T) RETURN n.id, n.title ORDER BY n.id"),
        vec![
            vec![Value::Int64(1), s("L1")],
            vec![Value::Int64(3), s("L3")]
        ]
    );
}

#[test]
fn title_is_the_title_and_name_stays_a_property() {
    let mut graph = DirGraph::new();
    write(&mut graph, "CREATE (:Q {id: 1, name: 'Nan', title: 'Ann'})").unwrap();
    write(&mut graph, "MERGE (:Q {id: 2, name: 'Nim', title: 'Bea'})").unwrap();
    write(&mut graph, "CREATE (:Q {id: 3, name: null, title: 'Cid'})").unwrap();
    write(&mut graph, "CREATE (:Q {id: 4, name: 'Dan'})").unwrap();
    assert_eq!(
        rows(
            &graph,
            "MATCH (n:Q) RETURN n.id, n.title, n.name ORDER BY n.id"
        ),
        vec![
            vec![Value::Int64(1), s("Ann"), s("Nan")],
            vec![Value::Int64(2), s("Bea"), s("Nim")],
            vec![Value::Int64(3), s("Cid"), s("Cid")],
            vec![Value::Int64(4), s("Dan"), s("Dan")],
        ]
    );
}

/// A type whose titles are its ids — `add_nodes` with no title column and no
/// `node_title_field` — titles a `CREATE` or `MERGE` with no title by its id,
/// as `add_nodes` would have; its title column stays typed. Every storage mode.
#[test]
fn an_untitled_create_on_an_id_titled_type_is_titled_by_its_id() {
    use crate::datatypes::values::{ColumnData, ColumnType};
    use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let dir = tempfile::tempdir().unwrap();
        let path = matches!(mode, StorageMode::Disk).then(|| dir.path());
        let mut graph = new_dir_graph_in_mode(mode, path).unwrap();
        let mut df = DataFrame::new(Vec::new());
        df.add_column(
            "id".into(),
            ColumnType::UniqueId,
            ColumnData::UniqueId((1..=3).map(Some).collect()),
        )
        .unwrap();
        add_nodes(&mut graph, df, "P".into(), "id".into(), None, None).unwrap();
        write(&mut graph, "CREATE (:P {id: 10})").unwrap();
        write(&mut graph, "MERGE (:P {id: 11})").unwrap();
        write(&mut graph, "CREATE (:P {id: 12, title: 'twelve'})").unwrap();
        let got = rows(&graph, "MATCH (n:P) RETURN n.id, n.title ORDER BY n.id");
        let number = |value: &Value| match *value {
            Value::Int64(n) => n,
            Value::UniqueId(n) => i64::from(n),
            ref other => panic!("{mode:?}: not an integer: {other:?}"),
        };
        let titles: Vec<_> = got
            .iter()
            .map(|row| (number(&row[0]), row[1].clone()))
            .collect();
        assert_eq!(titles.len(), 6, "{mode:?}");
        for (id, title) in &titles[..5] {
            assert_eq!(number(title), *id, "{mode:?}: id {id}");
        }
        assert_eq!(titles[5].1, s("twelve"), "{mode:?}");
        if matches!(mode, StorageMode::Memory) {
            // The supplied string title demotes; the id titles before it did not.
            let mut typed = DirGraph::new();
            let mut df = DataFrame::new(Vec::new());
            df.add_column(
                "id".into(),
                ColumnType::UniqueId,
                ColumnData::UniqueId((1..=3).map(Some).collect()),
            )
            .unwrap();
            add_nodes(&mut typed, df, "P".into(), "id".into(), None, None).unwrap();
            write(&mut typed, "CREATE (:P {id: 10})").unwrap();
            write(&mut typed, "MERGE (:P {id: 11})").unwrap();
            let store = typed.column_store("P").unwrap();
            assert_eq!(store.title_type_str(), Some("int64"));
        }
    }
}

/// String titles keep the `<Label>_<id>` fallback: a type titled from a
/// declared title field, and one whose titles are text other than its ids.
#[test]
fn an_untitled_create_on_a_text_titled_type_keeps_the_label_fallback() {
    let mut graph = DirGraph::new();
    let df = DataFrame::from_cypher_rows(
        vec!["id".into(), "label".into()],
        vec![vec![Value::Int64(1), s("L1")]],
    )
    .unwrap();
    add_nodes(
        &mut graph,
        df,
        "T".into(),
        "id".into(),
        Some("label".into()),
        None,
    )
    .unwrap();
    write(&mut graph, "CREATE (:T {id: 2})").unwrap();
    write(&mut graph, "CREATE (:U {id: 5, title: 'five'})").unwrap();
    write(&mut graph, "CREATE (:U {id: 6})").unwrap();
    assert_eq!(
        rows(
            &graph,
            "MATCH (n) WHERE n.id IN [2, 6] RETURN n.title ORDER BY n.id"
        ),
        vec![vec![s("T_2")], vec![s("U_6")]]
    );
}
