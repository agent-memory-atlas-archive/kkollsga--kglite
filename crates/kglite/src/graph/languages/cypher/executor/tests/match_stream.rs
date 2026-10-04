//! A leading MATCH feeding a streamable aggregate row by row.
//!
//! Each shape runs with the streaming pipeline off and on and must answer
//! identically — values compared through their `Debug` form, which prints a
//! float at full round-trip precision, so a reassociated sum fails. The memory
//! tests read the probe the streamed source keeps: the widest chunk of matches
//! it ever held, against the total the materialized route holds at once.

use super::*;
use crate::graph::languages::cypher::executor::match_stream::match_stream_probe;
use crate::graph::languages::cypher::executor::stream::pipeline::absorbed_probe;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};

const TEAMS: i64 = 200;
const PEOPLE_PER_TEAM: i64 = 5;
const TASKS_PER_PERSON: i64 = 4;
const PATHS: usize = (TEAMS * PEOPLE_PER_TEAM * TASKS_PER_PERSON) as usize;

const CHAIN: &str = "MATCH (t:Team)<-[:IN_TEAM]-(p:Person)-[:OWNS]->(k:Task)";

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(graph, query, &opts).unwrap_or_else(|e| panic!("setup failed: {query}: {e}"));
}

/// Teams, their people and the people's tasks, with fractional float hours of
/// uneven magnitude so a summation order change moves the last digit.
fn org() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        &format!("UNWIND range(1, {TEAMS}) AS t CREATE (:Team {{tid: t, band: t % 7}})"),
    );
    run(
        &mut graph,
        &format!(
            "MATCH (t:Team) UNWIND range(1, {PEOPLE_PER_TEAM}) AS i \
             CREATE (:Person {{pid: t.tid * 10 + i, age: 20 + i * 7}})-[:IN_TEAM]->(t)"
        ),
    );
    run(
        &mut graph,
        &format!(
            "MATCH (p:Person) UNWIND range(1, {TASKS_PER_PERSON}) AS j \
             CREATE (p)-[:OWNS]->(:Task {{hours: (p.pid * 0.37 + j * 0.013) / 3.0, kind: j % 2}})"
        ),
    );
    graph
}

struct Outcome {
    rows: Vec<String>,
    absorbed: usize,
    widest_chunk: usize,
    chunks: usize,
}

fn execute(graph: &DirGraph, query: &str, streaming: bool, work: Option<usize>) -> Outcome {
    let params = HashMap::new();
    let opts = ExecuteOptions {
        streaming,
        max_work_units: work,
        ..ExecuteOptions::eager(&params)
    };
    absorbed_probe::take();
    match_stream_probe::take();
    let result = execute_read(graph, query, &opts).unwrap_or_else(|e| panic!("{query}: {e}"));
    let absorbed = absorbed_probe::take();
    let (widest_chunk, chunks) = match_stream_probe::take();
    Outcome {
        rows: result
            .result
            .rows
            .iter()
            .map(|row| format!("{row:?}"))
            .collect(),
        absorbed,
        widest_chunk,
        chunks,
    }
}

fn sorted(mut rows: Vec<String>) -> Vec<String> {
    rows.sort();
    rows
}

fn shapes() -> Vec<String> {
    let c = CHAIN;
    vec![
        format!("{c} RETURN sum(k.hours) AS s"),
        format!("{c} RETURN avg(k.hours) AS a, min(k.hours) AS lo, max(k.hours) AS hi"),
        format!("{c} RETURN t.tid AS t, sum(k.hours) AS s, avg(k.hours) AS a, min(k.hours) AS lo, max(k.hours) AS hi"),
        format!("{c} RETURN t.band AS band, count(*) AS n, sum(k.hours) AS s"),
        format!("{c} RETURN t.band AS band, count(DISTINCT k.kind) AS kinds, count(DISTINCT p) AS people"),
        format!("{c} RETURN count(DISTINCT p.age) AS ages, sum(DISTINCT k.kind) AS ks"),
        format!("{c} RETURN t.tid AS t, sum(k.hours) AS s ORDER BY s DESC, t LIMIT 5"),
        format!("{c} RETURN t.tid AS t, count(*) AS n LIMIT 3"),
        format!("{c} WHERE k.hours > 5.0 AND p.age < 40 RETURN t.band AS band, sum(k.hours) AS s"),
        format!("{c} WHERE t.band = 3 RETURN sum(k.hours) AS s, count(*) AS n"),
        format!("{c} WITH t.tid AS t, sum(k.hours) AS s WHERE s > 30.0 RETURN t, s"),
        "MATCH path = (t:Team)<-[:IN_TEAM]-(p:Person)-[:OWNS]->(k:Task) RETURN t.band AS band, sum(length(path)) AS hops, sum(k.hours) AS s".to_string(),
        "MATCH path = (t:Team)<-[:IN_TEAM]-(p:Person)-[:OWNS]->(k:Task) WHERE length(path) = 2 RETURN count(*) AS n, sum(k.hours) AS s".to_string(),
        // Nothing matches: a global aggregate still answers its identity row,
        // a grouped one answers none.
        "MATCH (t:Team {tid: -1})<-[:IN_TEAM]-(p:Person)-[:OWNS]->(k:Task) RETURN sum(k.hours) AS s, count(*) AS n, min(k.hours) AS lo".to_string(),
        "MATCH (t:Team {tid: -1})<-[:IN_TEAM]-(p:Person)-[:OWNS]->(k:Task) RETURN t.band AS band, count(*) AS n".to_string(),
        format!("{c} WHERE k.hours < 0.0 RETURN sum(k.hours) AS s, count(*) AS n"),
        // A single-node pattern streams too.
        "MATCH (k:Task) RETURN k.kind AS kind, sum(k.hours) AS s, avg(k.hours) AS a".to_string(),
    ]
}

#[test]
fn streamed_aggregates_answer_bit_for_bit_as_the_materialized_route() {
    let graph = org();
    for query in shapes() {
        let off = execute(&graph, &query, false, None);
        let on = execute(&graph, &query, true, None);
        assert_eq!(
            off.absorbed, 0,
            "{query}: streaming off must stream nothing"
        );
        assert_eq!(on.rows, off.rows, "{query}");
        // The single-node aggregate is fused by the planner, so nothing streams.
        assert!(
            on.absorbed > 0 || query.starts_with("MATCH (k:Task)"),
            "{query}: expected to stream"
        );
    }
}

#[test]
fn the_unfused_aggregates_really_stream_in_bounded_chunks() {
    let graph = org();
    for query in [
        format!("{CHAIN} RETURN t.tid AS t, sum(k.hours) AS s, avg(k.hours) AS a"),
        format!("{CHAIN} RETURN t.band AS band, count(DISTINCT k.kind) AS kinds"),
        format!("{CHAIN} WHERE p.age < 40 RETURN sum(k.hours) AS s"),
    ] {
        let off = execute(&graph, &query, false, None);
        assert_eq!(
            (off.chunks, off.widest_chunk),
            (0, 0),
            "{query}: the materialized route builds no chunks"
        );
        let on = execute(&graph, &query, true, None);
        assert!(on.absorbed > 0, "{query}: did not stream");
        assert!(on.chunks >= 3, "{query}: {} chunks", on.chunks);
        // The materialized route holds every match at once (PATHS of them);
        // a streamed chunk is a fraction of that. (The chunk size ramps up
        // geometrically from 16 start nodes, so on a graph this small the
        // widest chunk is a large fraction; the sizing itself is pinned in
        // `matcher_chunk_tests`.)
        assert!(
            on.widest_chunk * 3 < PATHS * 2,
            "{query}: widest chunk {} of {PATHS} matches",
            on.widest_chunk
        );
    }
}

/// An explicit `max_work_units` caps the matcher itself, so the query takes
/// the materialized route and reports exactly the error it always did.
#[test]
fn an_explicit_budget_keeps_the_materialized_route_and_its_error() {
    let graph = org();
    let query = format!("{CHAIN} RETURN t.tid AS t, sum(k.hours) AS s");
    let params = HashMap::new();
    let run_with = |streaming: bool| {
        let opts = ExecuteOptions {
            streaming,
            max_work_units: Some(PATHS / 2),
            ..ExecuteOptions::eager(&params)
        };
        match_stream_probe::take();
        let err = execute_read(&graph, &query, &opts)
            .err()
            .expect("the budget is below the match count")
            .to_string();
        (err, match_stream_probe::take())
    };
    let (off, _) = run_with(false);
    let (on, probe) = run_with(true);
    assert_eq!(on, off);
    assert!(on.contains("max_work_units"), "{on}");
    assert_eq!(probe, (0, 0), "a budgeted query builds no chunks");
    // A budget that fits answers and stays on the materialized route.
    let fits = execute(&graph, &query, true, Some(PATHS));
    assert_eq!(fits.chunks, 0);
    assert_eq!(fits.rows, execute(&graph, &query, false, None).rows);
}

/// Shapes whose materialized handling cannot be reproduced row by row stay
/// materialized: a comma pattern, OPTIONAL as the opening clause, and
/// `collect`.
#[test]
fn bails_stay_on_the_materialized_route() {
    let graph = org();
    for query in [
        format!("{CHAIN}, (t)<-[:IN_TEAM]-(q:Person) RETURN t.band AS band, sum(k.hours) AS s"),
        "OPTIONAL MATCH (t:Team)<-[:IN_TEAM]-(p:Person) RETURN t.band AS band, count(p) AS n"
            .to_string(),
        format!("{CHAIN} RETURN t.band AS band, collect(k.kind) AS kinds"),
    ] {
        let on = execute(&graph, &query, true, None);
        let off = execute(&graph, &query, false, None);
        assert_eq!(on.chunks, 0, "{query}: should not chunk");
        assert_eq!(sorted(on.rows), sorted(off.rows), "{query}");
    }
}

#[test]
fn an_interrupt_between_chunks_aborts_the_stream() {
    let graph = org();
    let query = format!("{CHAIN} RETURN t.band AS band, sum(k.hours) AS s");
    // The per-row poll fires on its first call; the aggregate must surface it.
    CypherExecutor::interrupt_after_periodic_polls(0);
    let params = HashMap::new();
    let opts = ExecuteOptions {
        streaming: true,
        ..ExecuteOptions::eager(&params)
    };
    let err = execute_read(&graph, &query, &opts)
        .err()
        .expect("the poll must abort");
    assert!(err.to_string().contains("test hook"), "{err}");
    assert!(
        execute(&graph, &query, true, None).absorbed > 0,
        "non-vacuity: the same query streams when no hook is armed"
    );
}
