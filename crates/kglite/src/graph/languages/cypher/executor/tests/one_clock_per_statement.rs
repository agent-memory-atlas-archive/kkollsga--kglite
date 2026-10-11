//! The no-argument temporal constructors hold one reading per statement
//! (openCypher / Neo4j semantics), so two calls in one statement are equal and
//! a statement stamps one instant onto every row it writes. Before this each
//! call read the system clock, and `duration.inSeconds(datetime(), datetime())`
//! was non-zero about one run in five.
//!
//! A clock that moves between two calls is a race, so the read-only cases
//! evaluate thousands of pairs in one statement: the old per-call reading
//! disagreed on a large share of them.

use super::temporal_functions::cell;
use super::*;
use crate::graph::languages::cypher::executor::budget::ExecutionBudget;
use crate::graph::languages::cypher::executor::statement_clock::StatementClock;
use chrono::NaiveDate;

const NO_ARG_CONSTRUCTORS: [&str; 5] = [
    "datetime()",
    "localdatetime()",
    "date()",
    "time()",
    "localtime()",
];

fn int(n: i64) -> Value {
    Value::Int64(n)
}

#[test]
fn two_calls_in_one_statement_are_equal_for_every_constructor() {
    for constructor in NO_ARG_CONSTRUCTORS {
        let query = format!(
            "UNWIND range(1, 4000) AS i \
             RETURN sum(CASE WHEN {constructor} = {constructor} THEN 0 ELSE 1 END)"
        );
        assert_eq!(cell(&query), int(0), "{constructor}");
    }
}

#[test]
fn the_duration_between_two_now_calls_is_zero() {
    let query = "UNWIND range(1, 4000) AS i \
                 WITH duration.inSeconds(datetime(), datetime()) AS d \
                 RETURN sum(CASE WHEN toString(d) = 'PT0S' THEN 0 ELSE 1 END)";
    assert_eq!(cell(query), int(0));
}

fn run_write(graph: &mut DirGraph, source: &str) -> CypherResult {
    let query = parser::parse_cypher(source).unwrap();
    execute_mutable(
        graph,
        &query,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::default(),
    )
    .unwrap_or_else(|e| panic!("{source}: {e}"))
}

#[test]
fn a_write_statement_stamps_one_instant_and_agrees_with_itself_per_row() {
    let mut graph = DirGraph::new();
    run_write(
        &mut graph,
        "UNWIND range(1, 1500) AS i CREATE (:T {id: i, a: datetime(), b: datetime()})",
    );
    let apart = run_write(
        &mut graph,
        "MATCH (n:T) WHERE n.a <> n.b RETURN count(n) AS apart",
    );
    assert_eq!(apart.rows[0][0], int(0));
    let stamped = run_write(
        &mut graph,
        "MATCH (n:T) SET n.c = datetime(), n.d = datetime() \
         RETURN sum(CASE WHEN n.c = n.d THEN 0 ELSE 1 END) AS apart, count(DISTINCT n.c) AS stamps",
    );
    assert_eq!(stamped.rows[0], vec![int(0), int(1)]);
}

#[test]
fn a_call_subquery_body_reads_the_outer_statements_clock() {
    let query = "UNWIND range(1, 1500) AS i \
                 CALL { WITH i RETURN datetime() AS inner } \
                 RETURN sum(CASE WHEN inner = datetime() THEN 0 ELSE 1 END)";
    assert_eq!(cell(query), int(0));
}

fn fixed_clock() -> StatementClock {
    StatementClock::at(
        NaiveDate::from_ymd_opt(2001, 2, 3)
            .unwrap()
            .and_hms_opt(23, 59, 58)
            .unwrap(),
    )
}

fn run_with_clock(query: &str) -> Value {
    let graph = DirGraph::new();
    let parsed = parser::parse_cypher(query).unwrap();
    let no_params = HashMap::new();
    let result = CypherExecutor::with_params(&graph, &no_params, None)
        .with_budget(ExecutionBudget::with_clock(fixed_clock()))
        .execute(&parsed)
        .unwrap();
    result.rows[0][0].clone()
}

#[test]
fn the_constructors_report_the_budgets_clock() {
    assert_eq!(
        run_with_clock("RETURN toString(datetime())"),
        Value::String("2001-02-03T23:59:58Z".to_string())
    );
    assert_eq!(
        run_with_clock("RETURN toString(date())"),
        Value::String("2001-02-03".to_string())
    );
    assert_eq!(
        run_with_clock("RETURN toString(time())"),
        Value::String("23:59:58Z".to_string())
    );
}

#[test]
fn the_valid_time_context_resolves_today_by_the_same_clock() {
    use crate::graph::languages::cypher::valid_time::execution_scope;
    let graph = DirGraph::new();
    let no_params = HashMap::new();
    let query = parser::parse_cypher("FOR VALID_TIME AS OF date() RETURN 1").unwrap();
    let scope = execution_scope(
        &query,
        &graph,
        &no_params,
        &ExecutionBudget::with_clock(fixed_clock()),
    )
    .unwrap();
    assert_eq!(
        scope.instant,
        Some(crate::graph::features::temporal::eval::Instant::Date(
            NaiveDate::from_ymd_opt(2001, 2, 3).unwrap()
        ))
    );
}
