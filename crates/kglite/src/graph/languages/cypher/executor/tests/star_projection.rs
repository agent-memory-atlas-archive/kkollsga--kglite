//! **Absolute goldens for `*` beside other projection items.**
//!
//! `RETURN *` / `WITH *` was expanded only when the `*` was the clause's sole
//! unaliased item. Written beside anything else — `WITH *, a + 1 AS b` — the
//! `*` stayed an `Expression::Star` and was projected as an ordinary item: a
//! column literally named `*` holding `evaluate(Star)`, which is the `1` that
//! exists for `count(*)`. Every value-carrying name the incoming scope held
//! was dropped with the projection it replaced, so `a` read back as null.
//!
//! Node, edge and path *bindings* survived regardless — the projection never
//! touched them — so the loss was invisible for `MATCH (n) WITH *, 1 AS k`
//! and total for an `UNWIND` alias or an earlier aggregate's output. Worse,
//! `WITH *, count(*) AS c` grouped by the constant the `*` evaluated to: one
//! group for the whole input instead of one per row-scope.
//!
//! Every case runs with the optimizer off *and* on; the two agreed on every
//! wrong answer, so these are absolute answers rather than a comparison.

use super::*;

/// Three `N` nodes (`x`, `y`, `z`) and two `R` edges out of `x`.
fn star_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    let query = parser::parse_cypher(
        "CREATE (x:N {id:'x'}) CREATE (y:N {id:'y'}) CREATE (z:N {id:'z'}) \
         CREATE (x)-[:R {w:1}]->(y) CREATE (x)-[:R {w:2}]->(z)",
    )
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

fn run(graph: &DirGraph, source: &str, optimize: bool) -> CypherResult {
    let params = HashMap::new();
    let mut query = parser::parse_cypher(source)
        .unwrap_or_else(|e| panic!("failed to parse: {source}\n  error: {e}"));
    if optimize {
        crate::graph::languages::cypher::planner::optimize(&mut query, graph, &params);
    }
    CypherExecutor::with_params(graph, &params, None)
        .execute(&query)
        .unwrap_or_else(|e| panic!("failed to execute: {source}\n  error: {e}"))
}

/// Assert both the column names and the rows, under both plan profiles.
fn assert_result(graph: &DirGraph, source: &str, columns: &[&str], rows: Vec<Vec<Value>>) {
    for optimize in [false, true] {
        let result = run(graph, source, optimize);
        assert_eq!(
            result.columns, columns,
            "columns for `{source}` (optimize={optimize})"
        );
        assert_eq!(
            result.rows, rows,
            "rows for `{source}` (optimize={optimize})"
        );
    }
}

fn assert_rows(graph: &DirGraph, source: &str, rows: Vec<Vec<Value>>) {
    for optimize in [false, true] {
        let result = run(graph, source, optimize);
        assert_eq!(
            result.rows, rows,
            "rows for `{source}` (optimize={optimize})"
        );
    }
}

fn assert_columns(graph: &DirGraph, source: &str, columns: &[&str]) {
    for optimize in [false, true] {
        let result = run(graph, source, optimize);
        assert_eq!(
            result.columns, columns,
            "columns for `{source}` (optimize={optimize})"
        );
    }
}

fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

fn i(n: i64) -> Value {
    Value::Int64(n)
}

// ========================================================================
// The reported shape: a value-carrying name survives a mixed `*`
// ========================================================================

#[test]
fn a_mixed_star_carries_the_scalar_names_in_scope() {
    let graph = star_graph();
    // Pre-fix: `{a: null, b: 2}` — the WITH projected a column named `*`
    // holding 1, and `a` was gone.
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH *, a + 1 AS b RETURN a, b",
        &["a", "b"],
        vec![vec![i(1), i(2)]],
    );
    // Two explicit items beside the `*`.
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH *, a + 1 AS b, a + 2 AS c RETURN a, b, c",
        &["a", "b", "c"],
        vec![vec![i(1), i(2), i(3)]],
    );
    // A `*` written after the explicit item expands where it stands.
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH a + 1 AS b, * RETURN a, b",
        &["a", "b"],
        vec![vec![i(1), i(2)]],
    );
    // An earlier aggregate's output is a projected scalar like any other.
    assert_result(
        &graph,
        "MATCH (n:N) WITH count(*) AS c WITH *, c + 1 AS d RETURN c, d",
        &["c", "d"],
        vec![vec![i(3), i(4)]],
    );
    // A scalar beside a node binding: the node always survived, the scalar did
    // not. Pre-fix: `s` was null.
    assert_result(
        &graph,
        "MATCH (n:N {id:'x'}) WITH n, n.id AS s WITH *, 1 AS k RETURN n.id, s, k",
        &["n.id", "s", "k"],
        vec![vec![s("x"), s("x"), i(1)]],
    );
}

// ========================================================================
// No column is literally named `*`
// ========================================================================

#[test]
fn a_mixed_star_never_reaches_the_output_as_a_column() {
    let graph = star_graph();
    // Pre-fix: columns ["*", "b"], the `*` cell holding 1.
    assert_result(
        &graph,
        "UNWIND [1] AS a RETURN *, a + 1 AS b",
        &["a", "b"],
        vec![vec![i(1), i(2)]],
    );
    // And a `*` laundered through a WITH into a trailing sole `RETURN *`:
    // pre-fix the WITH wrote a real `*` column, which the RETURN then
    // faithfully expanded.
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH *, a + 1 AS b RETURN *",
        &["a", "b"],
        vec![vec![i(1), i(2)]],
    );
}

// ========================================================================
// The de-duplication rule: the explicit item wins, `*` fills the rest
// ========================================================================

#[test]
fn an_explicit_item_wins_over_the_name_the_star_would_carry() {
    let graph = star_graph();
    // A self-alias is the identity, and must not produce two `a` columns.
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH *, a AS a RETURN a",
        &["a"],
        vec![vec![i(1)]],
    );
    // A rebind under the same name: one column, the explicit value. This
    // already held before the fix (the duplicate key overwrote) and is pinned
    // so the expansion cannot turn it into two columns or the old value.
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH *, a + 1 AS a RETURN a",
        &["a"],
        vec![vec![i(2)]],
    );
    // The same rule at RETURN, where the duplicate would be visible as two
    // output columns of the same name.
    assert_result(
        &graph,
        "UNWIND [1] AS a RETURN *, a + 1 AS a",
        &["a"],
        vec![vec![i(2)]],
    );
    // A node re-projected as a scalar under its own name.
    assert_result(
        &graph,
        "MATCH (n:N {id:'x'}) RETURN *, n.id AS n",
        &["n"],
        vec![vec![s("x")]],
    );
}

// ========================================================================
// `*` is every name in scope — paths included
// ========================================================================

#[test]
fn a_star_carries_the_path_variable_too() {
    let graph = star_graph();
    // Pre-fix: columns ["a", "b"] — a path variable was in scope (`length(p)`
    // answers) but `*` never listed it.
    assert_columns(
        &graph,
        "MATCH p = (a:N {id:'x'})-[:R]->(b) RETURN *",
        &["a", "b", "p"],
    );
    assert_columns(
        &graph,
        "MATCH p = (a:N {id:'x'})-[:R]->(b) RETURN *, 1 AS k",
        &["a", "b", "p", "k"],
    );
    // An edge variable that is also a projected value must not be listed
    // twice: pre-fix the edge pass did not check the projected names.
    assert_columns(
        &graph,
        "MATCH (:N {id:'x'})-[r:R]->() WITH r RETURN *",
        &["r"],
    );
}

// ========================================================================
// Aggregation: the `*` is the grouping key, not a constant
// ========================================================================

#[test]
fn a_star_beside_an_aggregate_groups_by_the_scope() {
    let graph = star_graph();
    // Pre-fix: ONE row `{n.id: null, c: 3}` — the `*` evaluated to the
    // constant 1, so the whole input formed a single group.
    assert_rows(
        &graph,
        "MATCH (n:N) WITH *, count(*) AS c RETURN n.id, c ORDER BY n.id",
        vec![vec![s("x"), i(1)], vec![s("y"), i(1)], vec![s("z"), i(1)]],
    );
    // The RETURN spelling: columns must name the variable, not `*`.
    assert_columns(
        &graph,
        "MATCH (n:N {id:'x'}) RETURN *, count(*) AS c",
        &["n", "c"],
    );
}

// ========================================================================
// Composition with the WITH scope barrier
// ========================================================================

#[test]
fn a_mixed_star_keeps_every_binding_in_scope() {
    let graph = star_graph();
    // `WITH *` carries the whole incoming scope, so the scope-barrier
    // restriction must not drop a binding just because the clause also
    // projects something else.
    assert_rows(
        &graph,
        "MATCH (n:N {id:'x'}) WITH *, 1 AS k MATCH (n)-[:R]->(m) RETURN m.id ORDER BY m.id",
        vec![vec![s("y")], vec![s("z")]],
    );
    // Edge and path bindings too.
    assert_rows(
        &graph,
        "MATCH (:N {id:'x'})-[r:R]->(b) WITH *, 1 AS k RETURN r.w, b.id ORDER BY b.id",
        vec![vec![i(1), s("y")], vec![i(2), s("z")]],
    );
    assert_rows(
        &graph,
        "MATCH p = (:N {id:'x'})-[:R]->(b) WITH *, 1 AS k RETURN length(p) AS l, b.id ORDER BY b.id",
        vec![vec![i(1), s("y")], vec![i(1), s("z")]],
    );
    // A correlated subquery whose body ends in a mixed `RETURN *` exports the
    // body's own names. Pre-fix `m` never reached the outer scope.
    assert_rows(
        &graph,
        "MATCH (n:N {id:'x'}) CALL { WITH n MATCH (n)-[:R]->(m) RETURN *, 1 AS k } \
         RETURN m.id ORDER BY m.id",
        vec![vec![s("y")], vec![s("z")]],
    );
}

// ========================================================================
// Controls — shapes that were already right
// ========================================================================

#[test]
fn the_spellings_that_were_already_right_stay_right() {
    let graph = star_graph();
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH * RETURN a",
        &["a"],
        vec![vec![i(1)]],
    );
    assert_result(
        &graph,
        "UNWIND [1] AS a WITH * RETURN *",
        &["a"],
        vec![vec![i(1)]],
    );
    // A `WHERE` / `ORDER BY` / `LIMIT` block attached to a mixed-star WITH
    // reads the alias it introduces and the scope the `*` carried.
    assert_rows(
        &graph,
        "MATCH (n:N) WITH *, n.id AS i ORDER BY i LIMIT 2 RETURN n.id, i",
        vec![vec![s("x"), s("x")], vec![s("y"), s("y")]],
    );
    assert_rows(
        &graph,
        "MATCH (n:N) WITH *, n.id AS i WHERE i = 'y' RETURN n.id, i",
        vec![vec![s("y"), s("y")]],
    );
}

// ========================================================================
// DISTINCT over a mixed `*` deduplicates on the scope, not on a constant
// ========================================================================

#[test]
fn a_mixed_star_distinct_deduplicates_on_the_scope() {
    let graph = star_graph();
    // Pre-fix: ONE row. The projection wrote `{*: 1, k: 1}` for every input
    // row, so DISTINCT saw three identical rows and kept one — three nodes in,
    // one row out, with nothing in the plan to show for it.
    assert_rows(
        &graph,
        "MATCH (n:N) WITH DISTINCT *, 1 AS k RETURN count(*) AS c",
        vec![vec![i(3)]],
    );
    // And it still deduplicates when the scope really does repeat.
    assert_rows(
        &graph,
        "MATCH (:N {id:'x'})-[:R]->() WITH DISTINCT 1 AS k RETURN count(*) AS c",
        vec![vec![i(1)]],
    );
}
