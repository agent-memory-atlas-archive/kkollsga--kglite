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

fn echo_mut(graph: &mut DirGraph, query: &str) -> Option<TemporalDiagnostics> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .diagnostics
        .expect("every execution attaches diagnostics")
        .temporal
        .map(|echo| *echo)
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

/// Wells 1 (closed in 2010) and 2 (from 2005), a `Project` they sit in, and a
/// declared `IN` relationship.
fn wells() -> DirGraph {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (w1:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
         (w2:Well {id: 2, vf: date('2005-01-01')}), (f:Project {id: 10}), \
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
            source: "explicit".into(),
            instant: "2003-06-30".into(),
            targets: vec!["(:Well)".into()],
            hidden: [("(:Well)".to_string(), 1)].into(),
            endpoint_invalid: Some(0),
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
         MATCH (w:Well)-[:IN]->(f:Project) RETURN w.id",
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
fn a_graph_without_declarations_has_no_echo_and_serializes_without_the_key() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:Well {id: 1})");
    for query in [
        "MATCH (w:Well) RETURN w.id",
        "FOR VALID_TIME ALL MATCH (w:Well) RETURN w.id",
    ] {
        assert_eq!(echo(&graph, query), None, "{query}");
    }
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
fn the_echo_names_where_the_context_came_from() {
    let mut graph = wells();
    let today = chrono::Utc::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let default = echo(&graph, "MATCH (w:Well) RETURN w.id").expect("the default echoes");
    assert_eq!(
        (default.source.as_str(), default.instant.as_str()),
        ("default", today.as_str())
    );
    assert_eq!(default.targets, ["(:Well)"]);
    let all = echo(&graph, "FOR VALID_TIME ALL MATCH (w:Well) RETURN w.id").unwrap();
    assert_eq!(
        (
            all.source.as_str(),
            all.instant.as_str(),
            all.route.as_str()
        ),
        ("all", "all", "plain")
    );
    assert!(all.targets.is_empty() && all.hidden.is_empty() && all.endpoint_invalid.is_none());
    let explicit = echo(
        &graph,
        "FOR VALID_TIME AS OF date('2003-06-30') MATCH (w:Well) RETURN w.id",
    )
    .unwrap();
    assert_eq!(explicit.source, "explicit");
    let valid_at = echo(
        &graph,
        "MATCH (w:Well) WHERE valid_at(w, date('2003-06-30')) RETURN w.id",
    )
    .unwrap();
    assert_eq!(
        (valid_at.source.as_str(), valid_at.instant.as_str()),
        ("skipped:valid_at", "all")
    );
    let procedure = echo(&graph, "CALL refresh_stats()").map(|e| e.source);
    assert_eq!(procedure.as_deref(), Some("skipped:procedure"));
    let write = echo_mut(&mut graph, "CREATE (:Well {id: 9})").unwrap();
    assert_eq!(write.source, "skipped:write");
    // A context that reaches no declared target is not filtering anything.
    let none = echo(&graph, "RETURN 1 AS x").unwrap();
    assert_eq!(
        (none.source.as_str(), none.route.as_str()),
        ("default", "plain")
    );
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
            "source": "explicit",
            "instant": "2003-06-30",
            "targets": ["(:Well)"],
            "hidden": {"(:Well)": 1},
            "endpoint_invalid": 0,
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

/// An org chart where each declaration is `convention`: departments `d1`
/// (to 2010-12-31), `d2` (from 2011-01-01), `d3` (from 2020-01-01);
/// employees `e1` (from 2005-01-01), `e2` (from 2013-01-01), `e3` (2012-01-01
/// to 2012-06-30); six `ASSIGNED` edges, each declared per source type.
fn org_chart(convention: &str) -> DirGraph {
    let mut graph = DirGraph::new();
    let edge = |name: &str, e: &str, d: &str, from: &str, to: Option<&str>| {
        let to = to.map_or(String::new(), |to| format!(", to: date('{to}')"));
        format!(
            "MATCH (e:Employee {{id: '{e}'}}), (d:Department {{id: '{d}'}}) \
             CREATE (e)-[:ASSIGNED {{name: '{name}', from: date('{from}'){to}}}]->(d)"
        )
    };
    let mut statements = vec![
        "CREATE (:Department {id: 'd1', f: date('2000-01-01'), t: date('2010-12-31')}), \
         (:Department {id: 'd2', f: date('2011-01-01')}), \
         (:Department {id: 'd3', f: date('2020-01-01')}), \
         (:Employee {id: 'e1', f: date('2005-01-01')}), \
         (:Employee {id: 'e2', f: date('2013-01-01')}), \
         (:Employee {id: 'e3', f: date('2012-01-01'), t: date('2012-06-30')})"
            .to_string(),
        // Valid, valid endpoints.
        edge("a1", "e1", "d2", "2011-01-01", None),
        // Valid, but the department has ended.
        edge("a2", "e1", "d1", "2005-01-01", None),
        edge("a3", "e3", "d2", "2012-01-01", Some("2012-12-31")),
        // Not yet started.
        edge("a4", "e1", "d2", "2015-01-01", None),
        // Valid, but the employee has not started.
        edge("a5", "e2", "d2", "2012-01-01", None),
        // Valid, with neither endpoint valid.
        edge("a6", "e2", "d3", "2012-01-01", None),
    ];
    for (label, bounds) in [("Department", "f"), ("Employee", "f")] {
        statements.push(format!(
            "CALL db.temporal.declare({{node: '{label}', from: '{bounds}', to: 't', \
             convention: '{convention}'}}) YIELD declared RETURN declared"
        ));
    }
    statements.push(format!(
        "CALL db.temporal.declare({{relationship: 'ASSIGNED', source_type: 'Employee', \
         from: 'from', to: 'to', convention: '{convention}'}}) YIELD declared RETURN declared"
    ));
    for statement in &statements {
        run(&mut graph, statement);
    }
    graph
}

const ASSIGNED_QUERY: &str = "MATCH (e:Employee)-[:ASSIGNED]->(d:Department) RETURN e.id";

fn counts_at(graph: &DirGraph, instant: &str) -> TemporalDiagnostics {
    echo(
        graph,
        &format!("FOR VALID_TIME AS OF date('{instant}') {ASSIGNED_QUERY}"),
    )
    .expect("a context echoes")
}

#[test]
fn the_echo_counts_hidden_rows_per_target_and_endpoint_invalid_edges() {
    let graph = org_chart("closed");
    let echo = counts_at(&graph, "2012-06-15");
    let hidden: Vec<_> = echo.hidden.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        hidden,
        [
            ("(:Department)", 2),
            ("(:Employee)", 1),
            ("[:ASSIGNED from :Employee]", 1),
        ]
    );
    // a2 (department ended), a5 (employee not started), a6 (both): one each.
    assert_eq!(echo.endpoint_invalid, Some(3));
    // The visible rows are what is left: a1 and a3.
    let params = HashMap::new();
    let rows = execute_read(
        &graph,
        &format!("FOR VALID_TIME AS OF date('2012-06-15') {ASSIGNED_QUERY}"),
        &ExecuteOptions::eager(&params),
    )
    .unwrap()
    .result
    .rows;
    assert_eq!(rows.len(), 2);
}

#[test]
fn the_counts_follow_the_convention_on_the_boundary_day() {
    // 2010-12-31 is the last day `d1` is valid under `closed` and its first
    // day gone under `half_open`.
    let closed = counts_at(&org_chart("closed"), "2010-12-31");
    assert_eq!(closed.hidden["(:Department)"], 2);
    assert_eq!(closed.hidden["[:ASSIGNED from :Employee]"], 5);
    assert_eq!(closed.endpoint_invalid, Some(0));
    let half_open = counts_at(&org_chart("half_open"), "2010-12-31");
    assert_eq!(half_open.hidden["(:Department)"], 3);
    assert_eq!(half_open.hidden["[:ASSIGNED from :Employee]"], 5);
    assert_eq!(half_open.endpoint_invalid, Some(1));
}

#[test]
fn a_timeless_instant_reports_zero_hidden_and_no_endpoint_invalid() {
    let graph = wells();
    let echo = echo(
        &graph,
        "FOR VALID_TIME AS OF date('2006-01-01') MATCH (w:Well) RETURN w.id",
    )
    .unwrap();
    assert_eq!(echo.route, "plain");
    assert_eq!(echo.hidden["(:Well)"], 0);
    assert_eq!(echo.endpoint_invalid, Some(0));
}

#[test]
fn a_statement_naming_no_declared_label_has_no_counts() {
    let graph = wells();
    let echo = echo(
        &graph,
        "FOR VALID_TIME AS OF date('2003-06-30') MATCH (f:Project) RETURN f.id",
    )
    .unwrap();
    assert!(echo.targets.is_empty());
    assert!(echo.hidden.is_empty());
    assert_eq!(echo.endpoint_invalid, Some(0));
}

/// The echo lists what a statement can reach: an undeclared secondary label
/// does not widen a statement that names a type nothing declared can sit on,
/// and a declared label carried as a secondary one is reached through every
/// type.
#[test]
fn the_echo_lists_the_targets_reachable_through_secondary_labels() {
    let mut graph = wells();
    run(&mut graph, "MATCH (f:Project) SET f:Tag");
    let at = "FOR VALID_TIME AS OF date('2003-06-30') ";
    let field = echo(&graph, &format!("{at}MATCH (f:Project) RETURN f.id")).unwrap();
    assert_eq!(
        field,
        TemporalDiagnostics {
            axis: "VALID_TIME".into(),
            source: "explicit".into(),
            instant: "2003-06-30".into(),
            targets: vec![],
            hidden: Default::default(),
            endpoint_invalid: Some(0),
            route: "plain".into(),
            retrieval: None,
            slice: false,
            session_version: graph.version(),
        }
    );
    let tag = echo(&graph, &format!("{at}MATCH (t:Tag) RETURN t.id")).unwrap();
    assert_eq!(tag.targets, ["(:Well)"]);
    assert_eq!(tag.hidden, [("(:Well)".to_string(), 1)].into());
    run(&mut graph, "MATCH (f:Project) SET f:Well");
    let field = echo(&graph, &format!("{at}MATCH (f:Project) RETURN f.id")).unwrap();
    assert_eq!(field.targets, ["(:Well)"]);
    assert_eq!(field.route, "guarded");
}

#[test]
fn the_counts_refresh_when_the_graph_changes() {
    let mut graph = org_chart("closed");
    assert_eq!(counts_at(&graph, "2012-06-15").endpoint_invalid, Some(3));
    run(
        &mut graph,
        "MATCH (d:Department {id: 'd1'}) SET d.t = date('2030-01-01')",
    );
    // d1 now covers 2012: a2 is visible.
    assert_eq!(counts_at(&graph, "2012-06-15").endpoint_invalid, Some(2));
}
