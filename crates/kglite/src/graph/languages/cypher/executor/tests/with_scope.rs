//! **Absolute goldens for `WITH` as a scope barrier.**
//!
//! A non-aggregating `WITH` projected the row's values and left its *identity*
//! bindings — the node, edge and path variables — untouched, so every name the
//! projection dropped stayed silently bound. A later `MATCH (a:N {id:'z'})`
//! then re-used the stale `a` as an anchor and filtered against it instead of
//! binding afresh: `MATCH (a:N {id:'x'}) WITH 1 AS u MATCH (a:N {id:'y'})`
//! answered zero rows, `SET`/`MERGE`/`CREATE` behind the same shape wrote
//! nothing, and `RETURN *` listed a column for a variable that was out of
//! scope. An *aggregating* `WITH` rebuilt its rows from the projection alone
//! and was correct throughout, which is why the class survived so long.
//!
//! Every case runs with the optimizer off *and* on. The two agreed on every
//! wrong answer here, so the differential corpus was structurally blind to all
//! of it — these are absolute answers, not a comparison.

use super::*;

/// Three `N` nodes (`x`, `y`, `z`) and two `R` edges out of `x`.
fn scope_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    run_write(
        &mut graph,
        "CREATE (x:N {id:'x'}) CREATE (y:N {id:'y'}) CREATE (z:N {id:'z'}) \
         CREATE (x)-[:R {w:1}]->(y) CREATE (x)-[:R {w:2}]->(z)",
    );
    graph
}

fn run_write(graph: &mut DirGraph, source: &str) -> CypherResult {
    let query = parser::parse_cypher(source)
        .unwrap_or_else(|e| panic!("failed to parse: {source}\n  error: {e}"));
    execute_mutable(
        graph,
        &query,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::default(),
    )
    .unwrap_or_else(|e| panic!("failed to execute: {source}\n  error: {e}"))
}

/// Run `source` under one plan profile.
fn read_with(graph: &DirGraph, source: &str, optimize: bool) -> CypherResult {
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

/// Assert the query's rows under both plan profiles.
fn assert_rows(graph: &DirGraph, source: &str, expected: Vec<Vec<Value>>) {
    for optimize in [false, true] {
        let result = read_with(graph, source, optimize);
        assert_eq!(
            result.rows, expected,
            "rows for `{source}` (optimize={optimize})"
        );
    }
}

/// Assert the query's output column names under both plan profiles.
fn assert_columns(graph: &DirGraph, source: &str, expected: &[&str]) {
    for optimize in [false, true] {
        let result = read_with(graph, source, optimize);
        assert_eq!(
            result.columns, expected,
            "columns for `{source}` (optimize={optimize})"
        );
    }
}

fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

// ========================================================================
// The dropped name binds afresh
// ========================================================================
//
// Pre-fix every query in this group answered zero rows: the second MATCH
// verified its property map against the node the first MATCH had left bound.

#[test]
fn a_name_the_with_drops_binds_afresh_in_a_later_match() {
    let graph = scope_graph();
    // Pre-fix: [] — `b` stayed bound to `y`, which is not `z`.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}), (b:N {id:'y'}) WITH 1 AS u \
         MATCH (a:N {id:'x'}), (b:N {id:'z'}) RETURN a.id, b.id",
        vec![vec![s("x"), s("z")]],
    );
    // Pre-fix: []. The single-variable spelling of the same thing.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH 1 AS u MATCH (a:N {id:'y'}) RETURN a.id",
        vec![vec![s("y")]],
    );
    // The control: renaming the second MATCH's variables was always right.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}), (b:N {id:'y'}) WITH 1 AS u \
         MATCH (c:N {id:'x'}), (d:N {id:'z'}) RETURN c.id, d.id",
        vec![vec![s("x"), s("z")]],
    );
}

#[test]
fn every_non_aggregating_with_spelling_drops_the_binding() {
    let graph = scope_graph();
    for source in [
        // Plain projection.
        "MATCH (a:N {id:'x'}) WITH 1 AS u MATCH (a:N {id:'y'}) RETURN a.id",
        // DISTINCT — pre-fix [], while the aggregating WITH below was correct.
        "MATCH (a:N {id:'x'}) WITH DISTINCT 1 AS u MATCH (a:N {id:'y'}) RETURN a.id",
        // A WITH-attached WHERE.
        "MATCH (a:N {id:'x'}) WITH 1 AS u WHERE u = 1 MATCH (a:N {id:'y'}) RETURN a.id",
        // ORDER BY / LIMIT bound to the WITH.
        "MATCH (a:N {id:'x'}) WITH 1 AS u ORDER BY u LIMIT 1 MATCH (a:N {id:'y'}) RETURN a.id",
        // A chain of projections — the last one is the barrier.
        "MATCH (a:N {id:'x'}) WITH 1 AS u WITH u AS v MATCH (a:N {id:'y'}) RETURN a.id",
        // An aggregating WITH followed by a plain one: the plain one leaked.
        "MATCH (a:N {id:'x'}) WITH a, count(*) AS c WITH 1 AS u \
         MATCH (a:N {id:'y'}) RETURN a.id",
    ] {
        assert_rows(&graph, source, vec![vec![s("y")]]);
    }
    // The aggregating WITH rebuilds its rows from the projection, so it was
    // already correct. Pinned here as the contrast the class was hiding behind.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}), (b:N {id:'y'}) WITH count(*) AS n \
         MATCH (a:N {id:'x'}), (b:N {id:'z'}) RETURN n, a.id, b.id",
        vec![vec![Value::Int64(1), s("x"), s("z")]],
    );
}

#[test]
fn an_alias_moves_the_binding_and_frees_the_old_name() {
    let graph = scope_graph();
    // `WITH a AS k` renames the binding: `k` is the node, `a` is gone.
    // Pre-fix: [] — `a` was still bound to `x`.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH a AS k MATCH (a:N {id:'y'}) RETURN a.id, k.id",
        vec![vec![s("y"), s("x")]],
    );
    // And the renamed binding still drives a pattern.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH a AS k MATCH (k)-[:R]->(m) RETURN k.id, m.id ORDER BY m.id",
        vec![vec![s("x"), s("y")], vec![s("x"), s("z")]],
    );
}

#[test]
fn optional_match_after_a_with_rebinds_instead_of_reading_a_stale_node() {
    let graph = scope_graph();
    // Pre-fix: [["x"]] — the OPTIONAL MATCH found nothing (`a` was pinned to
    // `x`, which is not `y`), null-extended the row, and `a.id` then read the
    // stale binding straight back out.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH 1 AS u OPTIONAL MATCH (a:N {id:'y'}) RETURN a.id",
        vec![vec![s("y")]],
    );
    // A genuine optional miss still null-extends.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH 1 AS u OPTIONAL MATCH (a:N {id:'nope'}) RETURN a.id",
        vec![vec![Value::Null]],
    );
}

#[test]
fn an_edge_variable_the_with_drops_binds_afresh() {
    let graph = scope_graph();
    // Pre-fix: [] — `r` stayed bound to the x→y edge.
    assert_rows(
        &graph,
        "MATCH (:N {id:'x'})-[r:R]->(:N {id:'y'}) WITH 1 AS u \
         MATCH (:N {id:'x'})-[r:R]->(q:N {id:'z'}) RETURN q.id, r.w",
        vec![vec![s("z"), Value::Int64(2)]],
    );
}

#[test]
fn a_dropped_name_does_not_constrain_a_later_scan() {
    let graph = scope_graph();
    // Pre-fix: [[1]] — the scan was pinned to the one stale node instead of
    // scanning the label. A silent undercount, with nothing to see in the plan.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH 1 AS u MATCH (a:N) RETURN count(*) AS c",
        vec![vec![Value::Int64(3)]],
    );
    // Row multiplicity too: the WITH keeps both driving rows, and the dropped
    // `b` rebinds to `z` in each. Pre-fix: one row, because the stale `b`
    // filtered the x→y row away.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'})-[:R]->(b) WITH a MATCH (b:N {id:'z'}) RETURN a.id, b.id",
        vec![vec![s("x"), s("z")], vec![s("x"), s("z")]],
    );
}

// ========================================================================
// The dropped name is out of scope for `RETURN *`
// ========================================================================

#[test]
fn return_star_after_a_with_lists_only_the_projected_names() {
    let graph = scope_graph();
    // Pre-fix: columns ["u", "a"], with `a` the node the WITH had dropped.
    assert_columns(&graph, "MATCH (a:N {id:'x'}) WITH 1 AS u RETURN *", &["u"]);
    // Same for an edge and a path variable.
    assert_columns(
        &graph,
        "MATCH (:N {id:'x'})-[r:R]->() WITH 1 AS u RETURN *",
        &["u"],
    );
    assert_columns(
        &graph,
        "MATCH p = (:N {id:'x'})-[:R]->() WITH 1 AS u RETURN *",
        &["u"],
    );
}

// ========================================================================
// Controls — what a WITH keeps in scope
// ========================================================================

#[test]
fn a_with_keeps_the_names_it_projects() {
    let graph = scope_graph();
    // A bare variable stays bound, and a re-match on it verifies rather than
    // rebinds.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH a MATCH (a:N) RETURN a.id",
        vec![vec![s("x")]],
    );
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH a MATCH (a)-[:R]->(m) RETURN m.id ORDER BY m.id",
        vec![vec![s("y")], vec![s("z")]],
    );
    // `WITH *` carries the whole incoming scope, bindings included.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH * MATCH (a)-[:R]->(m) RETURN m.id ORDER BY m.id",
        vec![vec![s("y")], vec![s("z")]],
    );
    // A path variable survives its own projection.
    assert_rows(
        &graph,
        "MATCH p = (:N {id:'x'})-[:R]->(b) WITH p, b RETURN length(p) AS l, b.id ORDER BY b.id",
        vec![vec![Value::Int64(1), s("y")], vec![Value::Int64(1), s("z")]],
    );
    // An aggregating WITH's group key keeps driving patterns.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH a, count(*) AS c MATCH (a)-[:R]->(m) RETURN m.id ORDER BY m.id",
        vec![vec![s("y")], vec![s("z")]],
    );
    // A correlated CALL body still sees its imported variable.
    assert_rows(
        &graph,
        "MATCH (a:N {id:'x'}) WITH a \
         CALL { WITH a MATCH (a)-[:R]->(m) RETURN m.id AS mid } RETURN mid ORDER BY mid",
        vec![vec![s("y")], vec![s("z")]],
    );
}

// ========================================================================
// The write path — the same barrier, silently losing writes
// ========================================================================

#[test]
fn create_after_a_with_rebinds_both_endpoints() {
    let mut graph = scope_graph();
    // Pre-fix: one edge. The second CREATE re-used the stale `a`/`b`, so the
    // second MATCH matched nothing and the CREATE never ran — no error, no
    // warning, one missing edge.
    let result = run_write(
        &mut graph,
        "MATCH (a:N {id:'x'}), (b:N {id:'y'}) CREATE (a)-[:K]->(b) WITH 1 AS u \
         MATCH (a:N {id:'x'}), (b:N {id:'z'}) CREATE (a)-[:K]->(b)",
    );
    assert_eq!(result.stats.unwrap().relationships_created, 2);
    assert_rows(
        &graph,
        "MATCH (:N {id:'x'})-[:K]->(t) RETURN t.id ORDER BY t.id",
        vec![vec![s("y")], vec![s("z")]],
    );
}

#[test]
fn set_and_merge_after_a_with_act_on_the_rebound_node() {
    let mut graph = scope_graph();
    // Pre-fix: properties_set 0 — the MATCH matched nothing, so SET was a
    // silent no-op.
    let result = run_write(
        &mut graph,
        "MATCH (a:N {id:'x'}) WITH 1 AS u MATCH (a:N {id:'y'}) SET a.tag = 'hit'",
    );
    assert_eq!(result.stats.unwrap().properties_set, 1);
    assert_rows(
        &graph,
        "MATCH (n:N) WHERE n.tag = 'hit' RETURN n.id",
        vec![vec![s("y")]],
    );

    // Pre-fix: MERGE matched the stale `a` and created nothing, returning `x`
    // for a pattern that asked for `w`.
    let mut graph = scope_graph();
    let result = run_write(
        &mut graph,
        "MATCH (a:N {id:'x'}) WITH 1 AS u MERGE (a:N {id:'w'}) RETURN a.id",
    );
    assert_eq!(result.rows, vec![vec![s("w")]]);
    assert_rows(
        &graph,
        "MATCH (n:N) RETURN count(*) AS c",
        vec![vec![Value::Int64(4)]],
    );
}
