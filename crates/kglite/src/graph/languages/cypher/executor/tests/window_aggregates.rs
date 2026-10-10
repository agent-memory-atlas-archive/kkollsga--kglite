//! Absolute goldens for `count/sum/avg/min/max … OVER ([PARTITION BY …])`.
//!
//! The differential corpus checks that the planner leaves these queries alone;
//! it cannot see an executor that answers the same wrong number on both
//! paths, so the expected values here are computed by hand.

use super::*;

const ROWS: &str = "UNWIND [{d: 'a', s: 1}, {d: 'a', s: 3}, {d: 'b', s: 10}, {d: 'b', s: null}, \
                    {d: 'c', s: null}] AS r ";

fn rows(tail: &str) -> Vec<Vec<Value>> {
    let graph = DirGraph::new();
    let query = format!("{ROWS}{tail}");
    let parsed = parser::parse_cypher(&query)
        .unwrap_or_else(|e| panic!("query failed to parse: {query}\n  error: {e}"));
    let no_params = HashMap::new();
    CypherExecutor::with_params(&graph, &no_params, None)
        .execute(&parsed)
        .unwrap_or_else(|e| panic!("query failed: {query}\n  error: {e}"))
        .rows
}

fn error_of(query: &str) -> String {
    match parser::parse_cypher(query) {
        Err(e) => e.to_string(),
        Ok(parsed) => {
            let graph = DirGraph::new();
            let no_params = HashMap::new();
            CypherExecutor::with_params(&graph, &no_params, None)
                .execute(&parsed)
                .expect_err("query unexpectedly succeeded")
        }
    }
}

fn int(n: i64) -> Value {
    Value::Int64(n)
}

fn float(f: f64) -> Value {
    Value::Float64(f)
}

fn text(s: &str) -> Value {
    Value::String(s.to_string())
}

#[test]
fn whole_result_aggregates_repeat_one_value_on_every_row() {
    let got = rows(
        "RETURN r.d AS d, r.s AS s, max(r.s) OVER () AS hi, min(r.s) OVER () AS lo, \
         sum(r.s) OVER () AS total, avg(r.s) OVER () AS mean, \
         count(r.s) OVER () AS present, count(*) OVER () AS n",
    );
    assert_eq!(got.len(), 5, "windows keep every input row");
    for row in &got {
        assert_eq!(
            row[2..],
            [int(10), int(1), int(14), float(14.0 / 3.0), int(3), int(5)]
        );
    }
    assert_eq!(got[0][..2], [text("a"), int(1)]);
    assert_eq!(got[4][..2], [text("c"), Value::Null]);
}

#[test]
fn partitioned_aggregates_see_only_their_partition() {
    let mut got = rows(
        "RETURN r.d AS d, r.s AS s, sum(r.s) OVER (PARTITION BY r.d) AS total, \
         max(r.s) OVER (PARTITION BY r.d) AS hi, count(r.s) OVER (PARTITION BY r.d) AS present, \
         count(*) OVER (PARTITION BY r.d) AS n, avg(r.s) OVER (PARTITION BY r.d) AS mean",
    );
    got.sort_by_key(|row| (row[0].clone().to_string(), format!("{:?}", row[1])));
    let expected = [
        (text("a"), 4, 3, 2, 2, 2.0),
        (text("a"), 4, 3, 2, 2, 2.0),
        (text("b"), 10, 10, 1, 2, 10.0),
        (text("b"), 10, 10, 1, 2, 10.0),
    ];
    for (row, (d, total, hi, present, n, mean)) in got.iter().zip(expected) {
        assert_eq!(row[0], d);
        assert_eq!(
            row[2..],
            [int(total), int(hi), int(present), int(n), float(mean)]
        );
    }
    // Partition c holds only a null: sum is 0, max and avg are null, count 0.
    let c = got.iter().find(|row| row[0] == text("c")).unwrap();
    assert_eq!(c[2..], [int(0), Value::Null, int(0), int(1), Value::Null]);
}

#[test]
fn distinct_inside_an_aggregate_window() {
    let got = rows("RETURN count(DISTINCT r.d) OVER () AS kinds, sum(DISTINCT r.s) OVER () AS s");
    assert!(got.iter().all(|row| row[0] == int(3) && row[1] == int(14)));
}

#[test]
fn aggregate_windows_and_ranking_windows_share_a_return() {
    let got = rows(
        "RETURN r.d AS d, r.s AS s, sum(r.s) OVER (PARTITION BY r.d) AS total, \
         row_number() OVER (PARTITION BY r.d ORDER BY r.s) AS rn WITH d, rn, total \
         WHERE d = 'a' RETURN d, rn, total ORDER BY rn",
    );
    assert_eq!(got.len(), 2);
    assert_eq!(got[0], [text("a"), int(1), int(4)]);
    assert_eq!(got[1], [text("a"), int(2), int(4)]);
}

#[test]
fn an_aggregate_window_over_no_rows_returns_no_rows() {
    let graph = DirGraph::new();
    let parsed = parser::parse_cypher("UNWIND [] AS x RETURN x, max(x) OVER () AS m").unwrap();
    let no_params = HashMap::new();
    let result = CypherExecutor::with_params(&graph, &no_params, None)
        .execute(&parsed)
        .unwrap();
    assert!(result.rows.is_empty());
}

#[test]
fn order_by_inside_an_aggregate_window_is_refused() {
    let message = error_of("UNWIND [1, 2] AS x RETURN sum(x) OVER (ORDER BY x) AS running");
    assert!(
        message.contains("ORDER BY inside OVER is not supported for the aggregate sum()"),
        "{message}"
    );
}

#[test]
fn unsupported_window_functions_and_malformed_calls_are_refused() {
    let message = error_of("UNWIND [1] AS x RETURN collect(x) OVER () AS c");
    assert!(message.contains("not 'collect'"), "{message}");
    let message = error_of("UNWIND [1] AS x RETURN row_number() OVER () AS n");
    assert!(message.contains("requires ORDER BY"), "{message}");
    let message = error_of("UNWIND [1] AS x RETURN rank(x) OVER (ORDER BY x) AS n");
    assert!(message.contains("takes no arguments"), "{message}");
}

/// Before the projection kept the earlier scope readable, a window whose
/// argument or ORDER BY named a variable the same RETURN did not project
/// (`UNWIND`/`WITH` variables live only in the projected scope) read `null`
/// for every row: `max(x) OVER ()` was `null` and `row_number() OVER (ORDER BY
/// x)` numbered the rows in input order.
#[test]
fn windows_read_variables_the_return_does_not_project() {
    let graph = DirGraph::new();
    let parsed = parser::parse_cypher(
        "UNWIND [3, 1, 2] AS x RETURN x * 10 AS y, max(x) OVER () AS hi, \
         row_number() OVER (ORDER BY x) AS rn ORDER BY y",
    )
    .unwrap();
    let no_params = HashMap::new();
    let result = CypherExecutor::with_params(&graph, &no_params, None)
        .execute(&parsed)
        .unwrap();
    assert_eq!(result.columns, ["y", "hi", "rn"]);
    assert_eq!(
        result.rows,
        [
            [int(10), int(3), int(1)],
            [int(20), int(3), int(2)],
            [int(30), int(3), int(3)],
        ]
    );
}
