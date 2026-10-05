//! **Correlated lookups keyed by a driving row's value.**
//!
//! `UNWIND $rows AS row OPTIONAL MATCH (m:L {key: row.key})` and
//! `NOT EXISTS { MATCH (m:L {key: row.key}) }` scanned the whole label once per
//! row when `key` carries no persistent index, so their cost grew with
//! rows x nodes. Past [`TRANSIENT_INDEX_THRESHOLD`] driving rows (or EXISTS
//! evaluations) they probe a query-local hash index instead. The goldens fix
//! the answers on both sides of that threshold; the deadline tests fail for a
//! per-row scan.

use super::*;
use crate::datatypes::prop_map::PropMap;
use crate::graph::languages::cypher::executor::transient_index::TRANSIENT_INDEX_THRESHOLD;
use std::time::{Duration, Instant};

/// Nodes keyed `k0..k49` (`n` = the index), two keyed `dup`, one keyed `7`
/// (an integer), one with no `key`.
fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for statement in [
        "UNWIND range(0, 49) AS i CREATE (:L {key: 'k' + toString(i), n: i})",
        "CREATE (:L {key: 'dup', n: 100}) CREATE (:L {key: 'dup', n: 101})",
        "CREATE (:L {key: 7, n: 200}) CREATE (:L {n: 300})",
    ] {
        let query = parser::parse_cypher(statement).unwrap();
        execute_mutable(
            &mut graph,
            &query,
            HashMap::new(),
            crate::graph::algorithms::Interrupt::default(),
        )
        .unwrap();
    }
    graph
}

fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

/// Row `i` of the driving set: cycles through a hit, a miss, a two-way hit, an
/// integer key, an explicit null and an absent member.
fn driving_row(i: i64) -> Value {
    let mut members = vec![("i".to_string(), Value::Int64(i))];
    match i % 6 {
        0 => members.push(("key".into(), s(&format!("k{}", i % 50)))),
        1 => members.push(("key".into(), s(&format!("absent{i}")))),
        2 => members.push(("key".into(), s("dup"))),
        3 => members.push(("key".into(), Value::Int64(7))),
        4 => members.push(("key".into(), Value::Null)),
        _ => {}
    }
    Value::Map(PropMap::from_pairs(members))
}

fn run(graph: &DirGraph, source: &str, rows: i64, optimize: bool) -> Vec<Vec<Value>> {
    let params = HashMap::from([(
        "rows".to_string(),
        Value::List((0..rows).map(driving_row).collect()),
    )]);
    let mut query = parser::parse_cypher(source).unwrap();
    if optimize {
        crate::graph::languages::cypher::planner::optimize(&mut query, graph, &params);
    }
    let mut rows = CypherExecutor::with_params(graph, &params, None)
        .execute(&query)
        .unwrap_or_else(|e| panic!("failed to execute: {source}\n  error: {e}"))
        .rows;
    rows.sort_by_key(|row| format!("{row:?}"));
    rows
}

/// What `source` answers for the first `rows` driving rows, for both optimizer
/// settings, which must agree.
fn answer(graph: &DirGraph, source: &str, rows: i64) -> Vec<Vec<Value>> {
    let raw = run(graph, source, rows, false);
    assert_eq!(raw, run(graph, source, rows, true), "optimizer: {source}");
    raw
}

/// The driving rows are independent, so a run over `rows` of them must hold,
/// for each of the first `prefix`, exactly what a run over `prefix` held.
fn assert_prefix_parity(graph: &DirGraph, source: &str, prefix: i64, rows: i64) {
    assert!(
        rows as usize > TRANSIENT_INDEX_THRESHOLD && (prefix as usize) < TRANSIENT_INDEX_THRESHOLD
    );
    let small = answer(graph, source, prefix);
    let mut large: Vec<Vec<Value>> = answer(graph, source, rows)
        .into_iter()
        .filter(|row| matches!(row[0], Value::Int64(i) if i < prefix))
        .collect();
    large.sort_by_key(|row| format!("{row:?}"));
    assert_eq!(small, large, "{source}");
}

const OPTIONAL: &str = "UNWIND $rows AS row OPTIONAL MATCH (m:L {key: row.key}) \
                        RETURN row.i AS i, m.n AS n";
const NOT_EXISTS: &str = "UNWIND $rows AS row WITH row \
                          WHERE NOT EXISTS { MATCH (m:L {key: row.key}) } RETURN row.i AS i";
const EXISTS: &str = "UNWIND $rows AS row WITH row \
                      WHERE EXISTS { MATCH (m:L {key: row.key}) } RETURN row.i AS i";

#[test]
fn optional_match_rows_are_the_same_above_the_index_threshold() {
    let graph = graph();
    // Row 0 -> k0 (n 0); 1 miss; 2 -> both dup nodes; 3 -> the integer key;
    // 4 null key and 5 absent key -> a null-padded row each.
    assert_eq!(
        answer(&graph, OPTIONAL, 6),
        vec![
            vec![Value::Int64(0), Value::Int64(0)],
            vec![Value::Int64(1), Value::Null],
            vec![Value::Int64(2), Value::Int64(100)],
            vec![Value::Int64(2), Value::Int64(101)],
            vec![Value::Int64(3), Value::Int64(200)],
            vec![Value::Int64(4), Value::Null],
            vec![Value::Int64(5), Value::Null],
        ]
    );
    assert_prefix_parity(&graph, OPTIONAL, 6, 300);
}

#[test]
fn optional_match_with_a_bound_variable_is_the_same_above_the_threshold() {
    let graph = graph();
    // `m` is bound before the OPTIONAL MATCH, so a probe would ignore it.
    let source = "MATCH (m:L {n: 2}) UNWIND $rows AS row \
                  OPTIONAL MATCH (m:L {key: row.key}) RETURN row.i AS i, m.n AS n";
    assert_prefix_parity(&graph, source, 6, 300);
}

#[test]
fn optional_match_keeps_its_where_above_the_threshold() {
    let graph = graph();
    let source = "UNWIND $rows AS row OPTIONAL MATCH (m:L {key: row.key}) \
                  WHERE m.n > 100 RETURN row.i AS i, m.n AS n";
    assert_prefix_parity(&graph, source, 6, 300);
}

#[test]
fn exists_subqueries_answer_the_same_above_the_threshold() {
    let graph = graph();
    // Hits: rows 0, 2 and 3 of the first six.
    assert_eq!(
        answer(&graph, EXISTS, 6),
        vec![
            vec![Value::Int64(0)],
            vec![Value::Int64(2)],
            vec![Value::Int64(3)]
        ]
    );
    assert_eq!(
        answer(&graph, NOT_EXISTS, 6),
        vec![
            vec![Value::Int64(1)],
            vec![Value::Int64(4)],
            vec![Value::Int64(5)]
        ]
    );
    assert_prefix_parity(&graph, EXISTS, 6, 300);
    assert_prefix_parity(&graph, NOT_EXISTS, 6, 300);
}

/// The index is built by the evaluation that crosses the threshold; the rows
/// before it, the row that builds it and the rows after must all agree.
#[test]
fn exists_answers_do_not_change_where_the_index_is_built() {
    let graph = graph();
    let around = TRANSIENT_INDEX_THRESHOLD as i64;
    for rows in [around - 1, around, around + 1] {
        let hits = answer(&graph, EXISTS, rows).len();
        let expected = (0..rows).filter(|i| matches!(i % 6, 0 | 2 | 3)).count();
        assert_eq!(hits, expected, "{rows} driving rows");
    }
}

/// A larger label than the 64-row threshold, with every row a hit or a miss.
fn large_graph(nodes: i64) -> DirGraph {
    let mut graph = DirGraph::new();
    let query = parser::parse_cypher(&format!(
        "UNWIND range(0, {}) AS i CREATE (:L {{key: 'k' + toString(i)}})",
        nodes - 1
    ))
    .unwrap();
    execute_mutable(
        &mut graph,
        &query,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::default(),
    )
    .unwrap();
    graph
}

/// Runs `source` over `nodes` rows against `nodes` nodes under a deadline a
/// per-row label scan (rows x nodes comparisons) cannot meet; returns the rows.
fn run_with_deadline(source: &str, nodes: i64) -> Vec<Vec<Value>> {
    let graph = large_graph(nodes);
    let rows = (0..nodes)
        .map(|i| {
            let key = if i % 2 == 0 {
                format!("k{i}")
            } else {
                format!("x{i}")
            };
            Value::Map(PropMap::from_pairs(vec![("key".to_string(), s(&key))]))
        })
        .collect();
    let params = HashMap::from([("rows".to_string(), Value::List(rows))]);
    let query = parser::parse_cypher(source).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    CypherExecutor::with_params(&graph, &params, Some(deadline))
        .execute(&query)
        .unwrap_or_else(|e| panic!("`{source}` missed its deadline: {e}"))
        .rows
}

#[test]
fn optional_match_does_not_scan_the_label_per_row() {
    let rows = run_with_deadline(
        "UNWIND $rows AS row OPTIONAL MATCH (m:L {key: row.key}) RETURN row.key AS k, m.key AS hit",
        30_000,
    );
    assert_eq!(rows.len(), 30_000);
    assert_eq!(
        rows.iter().filter(|row| row[1] != Value::Null).count(),
        15_000
    );
}

#[test]
fn not_exists_does_not_scan_the_label_per_row() {
    let rows = run_with_deadline(
        "UNWIND $rows AS row WITH row \
         WHERE NOT EXISTS { MATCH (m:L {key: row.key}) } RETURN count(*) AS n",
        30_000,
    );
    assert_eq!(rows, vec![vec![Value::Int64(15_000)]]);
}
