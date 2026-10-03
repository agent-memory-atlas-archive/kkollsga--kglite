//! The default valid-time context: a graph with validity declarations runs a
//! statement that writes none as of today (UTC), `FOR VALID_TIME ALL` reads
//! every version, and the statements the default must not govern say so.

use std::collections::HashMap;

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::features::temporal::view::view_at;
use crate::graph::languages::cypher::plan_cache;
use crate::graph::languages::cypher::result::TemporalDiagnostics;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use std::sync::Arc;

fn run(graph: &mut DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
}

fn read(
    graph: &DirGraph,
    query: &str,
) -> Result<crate::graph::session::execute::ExecuteOutcome, String> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params)).map_err(|e| e.to_string())
}

fn ids(graph: &DirGraph, query: &str) -> Vec<i64> {
    read(graph, query)
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
        .iter()
        .map(|row| match row[0] {
            Value::Int64(id) => id,
            ref other => panic!("{other:?}"),
        })
        .collect()
}

fn echo(graph: &DirGraph, query: &str) -> Option<TemporalDiagnostics> {
    read(graph, query)
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .diagnostics?
        .temporal
        .map(|echo| *echo)
}

/// Employees 1 (left in 2010), 2 (employed since 2005), 3 (starts in 2999).
fn staff() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Employee {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
         (:Employee {id: 2, vf: date('2005-01-01')}), \
         (:Employee {id: 3, vf: date('2999-01-01')})",
    );
    run(
        &mut graph,
        "CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', \
         convention: 'closed'}) YIELD declared RETURN declared",
    );
    graph
}

const LIST: &str = "MATCH (e:Employee) RETURN e.id AS id ORDER BY id";

#[test]
fn default_all_and_explicit_answer_differently_on_a_declared_graph() {
    let graph = staff();
    assert_eq!(ids(&graph, LIST), [2], "the default is valid today");
    assert_eq!(
        ids(&graph, &format!("FOR VALID_TIME ALL {LIST}")),
        [1, 2, 3]
    );
    let explicit = format!("FOR VALID_TIME AS OF date('2008-01-01') {LIST}");
    assert_eq!(ids(&graph, &explicit), [1, 2]);
    let today = format!("FOR VALID_TIME AS OF date() {LIST}");
    assert_eq!(ids(&graph, &today), ids(&graph, LIST));
}

#[test]
fn a_graph_with_no_declaration_gets_no_context_and_all_is_a_no_op() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Employee {id: 1}), (:Employee {id: 2})",
    );
    assert_eq!(ids(&graph, LIST), [1, 2]);
    assert_eq!(ids(&graph, &format!("FOR VALID_TIME ALL {LIST}")), [1, 2]);
    assert!(echo(&graph, LIST).is_none());
    let plan = format!(
        "{:?}",
        read(&graph, &format!("EXPLAIN {LIST}"))
            .unwrap()
            .result
            .rows
    );
    assert!(
        !plan.contains("ValidTimeContext"),
        "no context, no plan change: {plan}"
    );
}

#[test]
fn a_write_reads_every_version_and_says_it_skipped_the_default() {
    let mut graph = staff();
    let rows = run(
        &mut graph,
        "MATCH (e:Employee) SET e.seen = true RETURN count(e) AS n",
    );
    assert_eq!(
        rows,
        vec![vec![Value::Int64(3)]],
        "the write saw all history"
    );
    let params = HashMap::new();
    let outcome = execute_mut(
        &mut graph,
        "CREATE (:Employee {id: 9})",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    let echo = outcome.result.diagnostics.unwrap().temporal.unwrap();
    assert_eq!(
        (echo.source.as_str(), echo.instant.as_str()),
        ("skipped:write", "all")
    );
}

#[test]
fn a_statement_that_names_its_own_instants_is_left_alone() {
    let graph = staff();
    let query =
        "MATCH (e:Employee) WHERE valid_at(e, date('2008-01-01')) RETURN e.id AS id ORDER BY id";
    assert_eq!(ids(&graph, query), [1, 2]);
    assert_eq!(echo(&graph, query).unwrap().source, "skipped:valid_at");
}

#[test]
fn declaring_and_the_audit_procedures_run_under_the_default() {
    let mut graph = staff();
    run(
        &mut graph,
        "CALL db.temporal.undeclare({node: 'Employee'}) YIELD undeclared RETURN undeclared",
    );
    run(
        &mut graph,
        "CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', \
         convention: 'closed'}) YIELD declared RETURN declared",
    );
    assert_eq!(ids(&graph, LIST), [2]);
    let audit = read(
        &graph,
        "CALL duplicate_id({type: 'Employee'}) YIELD node RETURN node",
    );
    let audit = audit.unwrap_or_else(|e| panic!("duplicate_id refused: {e}"));
    assert_eq!(
        audit.result.diagnostics.unwrap().temporal.unwrap().source,
        "skipped:procedure"
    );
    // The explicit prefix still refuses it.
    let refused = read(
        &graph,
        "FOR VALID_TIME AS OF date('2008-01-01') CALL duplicate_id({type: 'Employee'}) \
         YIELD node RETURN node",
    )
    .err()
    .expect("an explicit context refuses a procedure that is not valid-time aware");
    assert!(refused.contains("not valid-time aware"), "{refused}");
}

#[test]
fn a_view_refuses_all_like_any_other_context() {
    let graph = Arc::new(staff());
    let view = view_at(Arc::clone(&graph), &Value::String("2008-01-01".into())).unwrap();
    let params = HashMap::new();
    let err = view
        .execute_read(
            &format!("FOR VALID_TIME ALL {LIST}"),
            &ExecuteOptions::eager(&params),
        )
        .err()
        .expect("ALL inside a view is refused");
    assert!(err.to_string().contains("already as of"), "{err}");
}

/// The text of a timeless-today graph is prepared twice — with the default
/// context, then, once the session sees nothing would be filtered, without
/// it. The two plans share the text and must not share a cache entry.
#[test]
fn the_default_plan_and_the_plain_plan_of_one_text_do_not_collide() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Employee {id: 1, vf: date('2000-01-01')})",
    );
    run(
        &mut graph,
        "CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', \
         convention: 'closed'}) YIELD declared RETURN declared",
    );
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    assert_eq!(ids(&graph, LIST), [1]);
    let key = |suppress| {
        plan_cache::get(
            plan_cache::PlanScope {
                graph_id: graph.graph_id(),
                version: graph.version(),
                schema_locked: graph.schema_locked,
                lazy: false,
                suppress_default: suppress,
            },
            LIST,
        )
    };
    let with_default = key(false).expect("the default plan is cached");
    let plain = key(true).expect("the plain plan is cached");
    assert!(with_default.plan.context.is_some());
    assert!(plain.plan.context.is_none());
    assert_eq!(echo(&graph, LIST).unwrap().route, "plain");
}

/// A timeless default re-prepares its own text, `PROFILE` included.
#[test]
fn a_profiled_default_statement_on_a_timeless_graph_runs_its_plain_plan() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Employee {id: 1, vf: date('2000-01-01')})",
    );
    run(
        &mut graph,
        "CALL db.temporal.declare({node: 'Employee', from: 'vf', to: 'vt', \
         convention: 'closed'}) YIELD declared RETURN declared",
    );
    let profiled = read(&graph, &format!("PROFILE {LIST}")).expect("PROFILE under the default");
    assert_eq!(profiled.result.rows, vec![vec![Value::Int64(1)]]);
    assert!(profiled.result.profile.is_some());
}
