//! `add_connections` onto an existing `(type, source, target)` merges into
//! that relationship per `conflict_handling`; the report counts it as
//! updated, never as created.

use super::*;

fn docs(graph: &mut DirGraph) {
    let rows = vec![vec![Value::Int64(1)], vec![Value::Int64(2)]];
    let df = DataFrame::from_cypher_rows(vec!["id".to_string()], rows).unwrap();
    add_nodes(graph, df, "Doc".to_string(), "id".to_string(), None, None).unwrap();
}

fn connect(
    graph: &mut DirGraph,
    a: Value,
    b: Value,
    mode: Option<&str>,
) -> ConnectionOperationReport {
    let df = DataFrame::from_cypher_rows(
        vec![
            "src".to_string(),
            "tgt".to_string(),
            "a".to_string(),
            "b".to_string(),
        ],
        vec![vec![Value::Int64(1), Value::Int64(2), a, b]],
    )
    .unwrap();
    add_connections(
        graph,
        df,
        "LINKS".to_string(),
        "Doc".to_string(),
        "src".to_string(),
        "Doc".to_string(),
        "tgt".to_string(),
        None,
        None,
        mode.map(str::to_string),
    )
    .unwrap()
}

fn stored(graph: &DirGraph) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    crate::graph::session::execute_read(
        graph,
        "MATCH (:Doc)-[r:LINKS]->(:Doc) RETURN r.a AS a, r.b AS b",
        &crate::graph::session::ExecuteOptions::eager(&params),
    )
    .unwrap()
    .result
    .rows
}

/// The second call's row against the first call's edge, per mode: the
/// counters, and the one stored relationship's properties.
#[test]
fn a_second_call_on_the_same_endpoints_reports_an_update() {
    let first = || (Value::Int64(10), Value::Int64(20));
    for (mode, expected) in [
        // New non-null values overwrite; a null leaves the stored value.
        (None, vec![Value::Int64(30), Value::Int64(20)]),
        (Some("update"), vec![Value::Int64(30), Value::Int64(20)]),
        // The property set is the new row's.
        (Some("replace"), vec![Value::Int64(30), Value::Null]),
        // Stored values win.
        (Some("preserve"), vec![Value::Int64(10), Value::Int64(20)]),
        (Some("sum"), vec![Value::Int64(40), Value::Int64(20)]),
    ] {
        let mut graph = DirGraph::new();
        docs(&mut graph);
        let (a, b) = first();
        let report = connect(&mut graph, a, b, mode);
        assert_eq!(
            (report.connections_created, report.connections_updated),
            (1, 0),
            "{mode:?}: the first call creates"
        );
        let report = connect(&mut graph, Value::Int64(30), Value::Null, mode);
        assert_eq!(
            (report.connections_created, report.connections_updated),
            (0, 1),
            "{mode:?}: merging into the existing relationship creates nothing"
        );
        assert_eq!(stored(&graph), vec![expected], "{mode:?}");
    }
}

#[test]
fn skip_neither_creates_nor_updates() {
    let mut graph = DirGraph::new();
    docs(&mut graph);
    connect(&mut graph, Value::Int64(10), Value::Int64(20), Some("skip"));
    let report = connect(&mut graph, Value::Int64(30), Value::Null, Some("skip"));
    assert_eq!(
        (report.connections_created, report.connections_updated),
        (0, 0)
    );
    assert_eq!(
        stored(&graph),
        vec![vec![Value::Int64(10), Value::Int64(20)]]
    );
}
