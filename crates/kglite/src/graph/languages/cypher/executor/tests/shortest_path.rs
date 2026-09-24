//! **Absolute goldens for `shortestPath` / `allShortestPaths` endpoints an
//! earlier clause already bound.**
//!
//! The endpoint work-list used a bound variable only when *both* endpoints
//! were ordinary pattern bindings on the first input row. Anything else — one
//! bound endpoint, a node VALUE from `WITH` / `UNWIND` / `startNode(r)` / a
//! parameter, an unmatched OPTIONAL MATCH — re-resolved both endpoints from
//! their bare patterns: all-pairs paths, the bound variable re-bound to other
//! nodes, the input rows discarded.
//!
//! Every case runs with the optimizer off *and* on. Both profiles gave the
//! same wrong answers, so the differential corpus could not see any of this.

use super::*;

/// `a -R-> b -R-> c -R-> d`; `a`, `b`, `c` are `:P`, `d` is `:Q`.
fn chain_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    run_write(
        &mut graph,
        "CREATE (a:P {id:1, name:'a'})-[:R]->(b:P {id:2, name:'b'})\
         -[:R]->(c:P {id:3, name:'c'})-[:R]->(d:Q {id:4, name:'d'})",
    )
    .unwrap_or_else(|e| panic!("fixture: {e}"));
    graph
}

fn run_write(graph: &mut DirGraph, source: &str) -> Result<CypherResult, String> {
    let query = parser::parse_cypher(source)
        .unwrap_or_else(|e| panic!("failed to parse: {source}\n  error: {e}"));
    execute_mutable(
        graph,
        &query,
        HashMap::new(),
        crate::graph::algorithms::Interrupt::default(),
    )
}

fn read_with(
    graph: &DirGraph,
    source: &str,
    params: &HashMap<String, Value>,
    optimize: bool,
) -> Result<CypherResult, String> {
    let mut query = parser::parse_cypher(source)
        .unwrap_or_else(|e| panic!("failed to parse: {source}\n  error: {e}"));
    if optimize {
        crate::graph::languages::cypher::planner::optimize(&mut query, graph, params);
    }
    CypherExecutor::with_params(graph, params, None).execute(&query)
}

fn assert_rows_with(
    graph: &DirGraph,
    source: &str,
    params: &HashMap<String, Value>,
    expected: Vec<Vec<Value>>,
) {
    for optimize in [false, true] {
        let result = read_with(graph, source, params, optimize)
            .unwrap_or_else(|e| panic!("failed to execute: {source}\n  error: {e}"));
        assert_eq!(
            result.rows, expected,
            "rows for `{source}` (optimize={optimize})"
        );
    }
}

fn assert_rows(graph: &DirGraph, source: &str, expected: Vec<Vec<Value>>) {
    assert_rows_with(graph, source, &HashMap::new(), expected);
}

fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

fn i(n: i64) -> Value {
    Value::Int64(n)
}

fn names(path: &[&str]) -> Value {
    Value::List(path.iter().map(|n| s(n)).collect())
}

// ========================================================================
// One endpoint bound by an earlier MATCH
// ========================================================================

#[test]
fn one_bound_endpoint_anchors_and_is_never_rebound() {
    let graph = chain_graph();
    // Pre-fix: three rows — `a` re-bound to b and c.
    assert_rows(
        &graph,
        "MATCH (a:P {name:'a'}) MATCH p = shortestPath((a)-[:R*..5]-(b:Q)) \
         RETURN a.name, [n IN nodes(p) | n.name]",
        vec![vec![s("a"), names(&["a", "b", "c", "d"])]],
    );
    // The bound endpoint on the right-hand side.
    assert_rows(
        &graph,
        "MATCH (b:P {name:'b'}) MATCH p = shortestPath((x:P)-[:R*..5]-(b)) \
         RETURN x.name, length(p) ORDER BY x.name",
        vec![vec![s("a"), i(1)], vec![s("c"), i(1)]],
    );
}

#[test]
fn all_shortest_paths_anchors_a_bound_endpoint() {
    let graph = chain_graph();
    assert_rows(
        &graph,
        "MATCH (a:P {name:'b'}) MATCH p = allShortestPaths((a)-[:R*]-(q:Q)) \
         RETURN a.name, length(p)",
        vec![vec![s("b"), i(2)]],
    );
}

#[test]
fn bound_endpoint_respects_direction() {
    let graph = chain_graph();
    assert_rows(
        &graph,
        "MATCH (a:P {name:'b'}) MATCH p = shortestPath((a)-[:R*]->(q:Q)) RETURN length(p)",
        vec![vec![i(2)]],
    );
    // Incoming from `c`: only its predecessors, and not `c` itself.
    assert_rows(
        &graph,
        "MATCH (c:P {name:'c'}) MATCH p = shortestPath((c)<-[:R*]-(x:P)) \
         RETURN c.name, x.name, length(p) ORDER BY x.name",
        vec![vec![s("c"), s("a"), i(2)], vec![s("c"), s("b"), i(1)]],
    );
    // Nothing downstream of `d`.
    assert_rows(
        &graph,
        "MATCH (d:Q) MATCH p = shortestPath((d)-[:R*]->(x:P)) RETURN length(p)",
        vec![],
    );
}

#[test]
fn free_endpoint_keeps_its_labels_and_properties() {
    let graph = chain_graph();
    assert_rows(
        &graph,
        "MATCH (a:P {name:'a'}) MATCH p = shortestPath((a)-[:R*]-(x {name:'c'})) \
         RETURN x.name, length(p)",
        vec![vec![s("c"), i(2)]],
    );
    // Pre-fix: [] — the row variable in the property map never resolved.
    assert_rows(
        &graph,
        "UNWIND ['a', 'b'] AS nm MATCH p = shortestPath((x:P {name: nm})-[:R*]-(q:Q)) \
         RETURN nm, length(p) ORDER BY nm",
        vec![vec![s("a"), i(3)], vec![s("b"), i(2)]],
    );
}

#[test]
fn a_bound_endpoint_must_satisfy_the_endpoint_labels() {
    let graph = chain_graph();
    // Pre-fix: one row with `a` re-bound to `d`.
    assert_rows(
        &graph,
        "MATCH (a:P {name:'a'}) MATCH p = shortestPath((a:Q)-[:R*]-(x:P)) RETURN a.name",
        vec![],
    );
}

// ========================================================================
// Both endpoints bound
// ========================================================================

#[test]
fn both_bound_endpoints_pair_per_row() {
    let graph = chain_graph();
    assert_rows(
        &graph,
        "MATCH (a:P {name:'a'}), (b:P {name:'c'}) MATCH p = shortestPath((a)-[:R*]-(b)) \
         RETURN length(p)",
        vec![vec![i(2)]],
    );
    assert_rows(
        &graph,
        "MATCH (a:P) MATCH (q:Q) MATCH p = shortestPath((a)-[:R*]-(q)) \
         RETURN a.name, length(p) ORDER BY a.name",
        vec![vec![s("a"), i(3)], vec![s("b"), i(2)], vec![s("c"), i(1)]],
    );
}

// ========================================================================
// Endpoints that are node VALUES
// ========================================================================

#[test]
fn node_values_from_collect_and_index_anchor() {
    let graph = chain_graph();
    // Pre-fix: twelve all-pairs paths.
    assert_rows(
        &graph,
        "MATCH (n:P) WITH n ORDER BY n.name WITH collect(n) AS ns \
         WITH ns[0] AS a, ns[2] AS b \
         MATCH p = shortestPath((a)-[:R*..5]-(b)) RETURN [n IN nodes(p) | n.name]",
        vec![vec![names(&["a", "b", "c"])]],
    );
}

#[test]
fn node_values_from_unwind_anchor_per_row() {
    let graph = chain_graph();
    assert_rows(
        &graph,
        "MATCH (n:P) WITH collect(n) AS ns UNWIND ns AS a \
         MATCH p = shortestPath((a)-[:R*..5]-(q:Q)) RETURN a.name, length(p) ORDER BY a.name",
        vec![vec![s("a"), i(3)], vec![s("b"), i(2)], vec![s("c"), i(1)]],
    );
}

#[test]
fn node_values_from_start_node_and_with_alias_anchor() {
    let graph = chain_graph();
    assert_rows(
        &graph,
        "MATCH (x:P {name:'a'})-[r:R]->() WITH startNode(r) AS a MATCH (q:Q) \
         MATCH p = shortestPath((a)-[:R*..5]-(q)) RETURN [n IN nodes(p) | n.name]",
        vec![vec![names(&["a", "b", "c", "d"])]],
    );
    assert_rows(
        &graph,
        "MATCH (n:P {name:'b'}) WITH n AS a MATCH p = shortestPath((a)-[:R*]-(q:Q)) \
         RETURN length(p)",
        vec![vec![i(2)]],
    );
}

#[test]
fn a_node_value_parameter_anchors() {
    let graph = chain_graph();
    let node = read_with(
        &graph,
        "MATCH (n:P {name:'b'}) RETURN n",
        &HashMap::new(),
        false,
    )
    .unwrap()
    .rows[0][0]
        .clone();
    assert!(matches!(node, Value::Node(_)), "fixture: {node:?}");
    let params = HashMap::from([("n".to_string(), node)]);
    assert_rows_with(
        &graph,
        "WITH $n AS a MATCH p = shortestPath((a)-[:R*]-(q:Q)) RETURN length(p)",
        &params,
        vec![vec![i(2)]],
    );
}

// ========================================================================
// NULL, non-node and stale endpoints
// ========================================================================

#[test]
fn a_null_endpoint_yields_no_rows() {
    let graph = chain_graph();
    // Pre-fix: three all-pairs paths.
    assert_rows(
        &graph,
        "OPTIONAL MATCH (a:P {name:'zzz'}) MATCH p = shortestPath((a)-[:R*]-(q:Q)) \
         RETURN length(p)",
        vec![],
    );
    assert_rows(
        &graph,
        "WITH null AS a MATCH p = shortestPath((a)-[:R*]-(q:Q)) RETURN length(p)",
        vec![],
    );
}

#[test]
fn a_relationship_value_endpoint_is_an_error() {
    let graph = chain_graph();
    for optimize in [false, true] {
        let err = read_with(
            &graph,
            "MATCH ()-[r:R]->() WITH collect(r)[0] AS a \
             MATCH p = shortestPath((a)-[:R*]-(q:Q)) RETURN length(p)",
            &HashMap::new(),
            optimize,
        )
        .expect_err("a relationship is not a node endpoint");
        assert!(err.contains("holds a relationship"), "{err}");
    }
}

#[test]
fn a_stale_node_value_is_an_error_not_a_scan() {
    let mut graph = chain_graph();
    let err = run_write(
        &mut graph,
        "MATCH (n:P {name:'c'}) WITH n, collect(n)[0] AS v DETACH DELETE n \
         WITH v MATCH p = shortestPath((v)-[:R*]-(q:Q)) RETURN length(p)",
    )
    .expect_err("a deleted node value cannot anchor a search");
    assert!(err.contains("no longer exists"), "{err}");
}

// ========================================================================
// Input rows survive the clause
// ========================================================================

#[test]
fn unbound_endpoints_keep_the_input_row_multiplicity() {
    let graph = chain_graph();
    // Pre-fix: one row with `x` NULL — the UNWIND rows were discarded.
    assert_rows(
        &graph,
        "UNWIND [1, 2] AS x MATCH p = shortestPath((a:P {name:'a'})-[:R*]-(q:Q)) \
         RETURN x, length(p) ORDER BY x",
        vec![vec![i(1), i(3)], vec![i(2), i(3)]],
    );
}

#[test]
fn one_variable_on_both_ends_is_one_node() {
    let graph = chain_graph();
    assert_rows(
        &graph,
        "MATCH p = shortestPath((a:P)-[:R*0..]-(a)) RETURN a.name, length(p) ORDER BY a.name",
        vec![vec![s("a"), i(0)], vec![s("b"), i(0)], vec![s("c"), i(0)]],
    );
}

#[test]
fn a_where_after_an_opening_shortest_path_filters() {
    let graph = chain_graph();
    // Pre-fix: three rows — the pipeline folded the WHERE into the MATCH,
    // and the shortest-path arm dropped it.
    assert_rows(
        &graph,
        "MATCH p = shortestPath((a:P)-[:R*]-(q:Q)) WHERE length(p) = 1 RETURN a.name",
        vec![vec![s("c")]],
    );
}
