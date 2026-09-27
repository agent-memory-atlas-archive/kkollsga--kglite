//! `QueryDiagnostics.temporal`: the valid-time echo each execution route
//! leaves on its result.

use std::collections::HashMap;
use std::sync::Arc;

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::features::temporal::view::view_at;
use crate::graph::languages::cypher::result::TemporalDiagnostics;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn echo(graph: &DirGraph, query: &str) -> Option<TemporalDiagnostics> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .diagnostics
        .expect("every execution attaches diagnostics")
        .temporal
        .map(|echo| *echo)
}

/// Wells 1 (closed in 2010) and 2 (from 2005), a `Field` they sit in, and a
/// declared `IN` relationship.
fn wells() -> DirGraph {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
         (w2:Well {id: 2, vf: date('2005-01-01')}), (f:Field {id: 10}), \
         (w1)-[:IN {from: date('2000-01-01'), to: date('2040-01-01')}]->(f), (w2)-[:IN {from: date('2005-01-01')}]->(f)",
        "CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        "CALL db.temporal.declare({relationship: 'IN', source_type: 'Well', from: 'from', \
         to: 'to', convention: 'half_open'}) YIELD declared RETURN declared",
    ] {
        run(&mut graph, query);
    }
    graph
}

#[test]
fn a_filtering_context_echoes_its_instant_targets_and_the_guarded_route() {
    let graph = wells();
    let echo = echo(
        &graph,
        "FOR VALID_TIME AS OF date('2003-06-30') MATCH (w:Well) RETURN w.id",
    )
    .expect("a context echoes");
    assert_eq!(
        echo,
        TemporalDiagnostics {
            axis: "VALID_TIME".into(),
            instant: "2003-06-30".into(),
            targets: vec!["(:Well)".into()],
            route: "guarded".into(),
            retrieval: None,
            slice: false,
            session_version: graph.version(),
        }
    );
}

#[test]
fn a_hop_echoes_the_relationship_target_and_a_datetime_its_time() {
    let graph = wells();
    let echo = echo(
        &graph,
        "FOR VALID_TIME AS OF datetime('2003-06-30T12:30:00') \
         MATCH (w:Well)-[:IN]->(f:Field) RETURN w.id",
    )
    .expect("a context echoes");
    assert_eq!(echo.instant, "2003-06-30T12:30:00");
    assert_eq!(echo.targets, ["(:Well)", "[:IN from :Well]"]);
}

#[test]
fn a_timeless_instant_echoes_the_plain_route() {
    let graph = wells();
    // Both wells and both memberships are valid in 2006: the filter removes
    // nothing, so the statement runs its plain plan.
    let echo = echo(
        &graph,
        "FOR VALID_TIME AS OF date('2006-01-01') MATCH (w:Well) RETURN w.id",
    )
    .expect("the plain route still echoes");
    assert_eq!(echo.route, "plain");
    assert_eq!(echo.instant, "2006-01-01");
    assert_eq!(echo.targets, ["(:Well)"]);
}

#[test]
fn explain_echoes_the_guarded_plan_it_renders() {
    let graph = wells();
    let echo = echo(
        &graph,
        "EXPLAIN FOR VALID_TIME AS OF date('2006-01-01') MATCH (w:Well) RETURN w.id",
    )
    .expect("EXPLAIN echoes");
    assert_eq!(echo.route, "guarded");
    assert_eq!(echo.instant, "2006-01-01");
}

#[test]
fn a_statement_without_a_context_has_no_echo_and_serializes_without_the_key() {
    let graph = wells();
    assert_eq!(echo(&graph, "MATCH (w:Well) RETURN w.id"), None);
    let params = HashMap::new();
    let diagnostics = execute_read(
        &graph,
        "MATCH (w:Well) RETURN w.id",
        &ExecuteOptions::eager(&params),
    )
    .unwrap()
    .result
    .diagnostics
    .unwrap();
    let json = serde_json::to_value(&diagnostics).unwrap();
    assert!(json.get("temporal").is_none(), "{json}");
}

#[test]
fn a_routed_algorithm_echoes_the_slice() {
    let graph = wells();
    let echo = echo(
        &graph,
        "FOR VALID_TIME AS OF date('2003-06-30') CALL pagerank() YIELD node, score \
         RETURN node.id AS id",
    )
    .expect("a context echoes");
    assert!(echo.slice, "{echo:?}");
    let plain = self::echo(
        &graph,
        "FOR VALID_TIME AS OF date('2003-06-30') MATCH (w:Well) RETURN w.id",
    )
    .unwrap();
    assert!(!plain.slice);
}

#[test]
fn a_view_echoes_the_view_route_and_serializes_the_echo() {
    let graph = Arc::new(wells());
    let view = view_at(Arc::clone(&graph), &Value::String("2003-06-30".into())).unwrap();
    let params = HashMap::new();
    let outcome = view
        .execute_read(
            "MATCH (w:Well) RETURN w.id",
            &ExecuteOptions::eager(&params),
        )
        .unwrap();
    assert_eq!(outcome.result.rows, vec![vec![Value::Int64(1)]]);
    let diagnostics = outcome.result.diagnostics.unwrap();
    let echo = diagnostics.temporal.as_ref().unwrap();
    assert_eq!(echo.route, "view");
    let json = serde_json::to_value(&diagnostics).unwrap();
    assert_eq!(
        json["temporal"],
        serde_json::json!({
            "axis": "VALID_TIME",
            "instant": "2003-06-30",
            "targets": ["(:Well)"],
            "route": "view",
            "retrieval": null,
            "slice": false,
            "session_version": graph.version(),
        })
    );
    // A second instant on the view is refused, naming the view's.
    let Err(err) = view.execute_read(
        "FOR VALID_TIME AS OF date('2001-01-01') MATCH (w:Well) RETURN w.id",
        &ExecuteOptions::eager(&params),
    ) else {
        panic!("a second instant on a view runs");
    };
    assert!(
        err.to_string().contains("already as of date('2003-06-30')"),
        "{err}"
    );
}
