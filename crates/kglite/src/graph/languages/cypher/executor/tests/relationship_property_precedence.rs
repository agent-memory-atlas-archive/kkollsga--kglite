//! **Absolute goldens for `r.<key>` on relationships: the stored property
//! first, the envelope as the fallback** — the rule `n.type` already follows on
//! nodes.
//!
//! Before the fix a bound relationship read `type` / `connection_type` from the
//! envelope ahead of the property, while the pushed-down WHERE filter read the
//! property: `WHERE r.type = 'user-type' RETURN r.type = 'user-type'` kept the
//! row and returned `false`. A relationship *value* (`collect`, `UNWIND`,
//! `relationships(p)`, `YIELD relationship`) read `id` / `type` / `start` /
//! `end` from the envelope only. With the optimizer off the WHERE read the
//! envelope as well, so the two plans disagreed; the value arm was wrong under
//! both plans.

use super::*;
use crate::graph::edge_embeddings::upsert_edge_embeddings;
use petgraph::graph::EdgeIndex;

/// Edge 0 `(a)-[:REL]->(b)` stores every envelope-named key; edge 1
/// `(c)-[:REL {w: 1}]->(d)` stores none of them.
fn precedence_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    run_write(
        &mut graph,
        "CREATE (:E {id:'a'})-[:REL {id:'user-7', type:'user-type', connection_type:'ct', \
         start:'s', `end`:'e', start_id:'si', end_id:'ei', text:'x'}]->(:E {id:'b'}) \
         CREATE (:E {id:'c'})-[:REL {w: 1, text:'y'}]->(:E {id:'d'})",
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

fn assert_rows(graph: &DirGraph, source: &str, expected: Vec<Vec<Value>>) {
    for optimize in [false, true] {
        assert_eq!(
            read_with(graph, source, optimize).rows,
            expected,
            "rows for `{source}` (optimize={optimize})"
        );
    }
}

fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

fn t() -> Value {
    Value::Boolean(true)
}

/// `end` is a reserved word, so it is read back-quoted.
const STORED: [&str; 7] = [
    "id",
    "type",
    "connection_type",
    "start",
    "`end`",
    "start_id",
    "end_id",
];
const STORED_VALUES: [&str; 7] = ["user-7", "user-type", "ct", "s", "e", "si", "ei"];

fn stored_row() -> Vec<Value> {
    STORED_VALUES.iter().map(|v| s(v)).collect()
}

fn read_every_key(var: &str) -> String {
    STORED
        .iter()
        .map(|key| format!("{var}.{key}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ========================================================================
// MATCH bindings
// ========================================================================

#[test]
fn where_return_with_and_order_by_agree_on_a_stored_type() {
    let graph = precedence_graph();
    // Pre-fix: ['REL', false] — the row passed the filter.
    assert_rows(
        &graph,
        "MATCH ()-[r:REL]->() WHERE r.type = 'user-type' RETURN r.type, r.type = 'user-type'",
        vec![vec![s("user-type"), t()]],
    );
    // Anchored, so the pushdown pass compiles the WHERE into the matcher.
    assert_rows(
        &graph,
        "MATCH (a:E {id:'a'})-[r:REL]->(b) WHERE r.type = 'user-type' RETURN b.id",
        vec![vec![s("b")]],
    );
    assert_rows(
        &graph,
        "MATCH (a:E {id:'a'})-[r:REL]->(b) WHERE r.type = 'REL' RETURN b.id",
        vec![],
    );
    assert_rows(
        &graph,
        "MATCH ()-[r:REL]->() WITH r.type AS t ORDER BY t RETURN t",
        vec![vec![s("REL")], vec![s("user-type")]],
    );
    assert_rows(
        &graph,
        "MATCH ()-[r:REL]->() RETURN r.type ORDER BY r.type DESC",
        vec![vec![s("user-type")], vec![s("REL")]],
    );
    assert_rows(
        &graph,
        "MATCH ()-[r:REL]->() RETURN CASE WHEN r.type = 'user-type' THEN r.id ELSE 'none' END AS k \
         ORDER BY k",
        vec![vec![s("none")], vec![s("user-7")]],
    );
}

#[test]
fn every_stored_envelope_key_wins_on_a_binding() {
    let graph = precedence_graph();
    assert_rows(
        &graph,
        &format!(
            "MATCH ()-[r:REL {{text:'x'}}]->() RETURN {}",
            read_every_key("r")
        ),
        vec![stored_row()],
    );
    assert_rows(
        &graph,
        "MATCH (a:E {id:'a'})-[r:REL]->(b) WHERE r.id = 'user-7' AND r.start = 's' RETURN b.id",
        vec![vec![s("b")]],
    );
}

#[test]
fn without_a_stored_key_a_binding_falls_back_to_the_envelope() {
    let graph = precedence_graph();
    assert_rows(
        &graph,
        "MATCH ()-[r:REL {w: 1}]->() RETURN r.type = type(r), r.connection_type = type(r), \
         r.id = id(r), r.start = id(startNode(r)), r.start_id = id(startNode(r)), \
         r.`end` = id(endNode(r)), r.end_id = id(endNode(r)), r.start = startNode(r).id",
        vec![vec![t(); 8]],
    );
    // The pushed-down filter falls back the same way.
    assert_rows(
        &graph,
        "MATCH (c:E {id:'c'})-[r:REL]->(d) WHERE r.type = 'REL' RETURN d.id",
        vec![vec![s("d")]],
    );
}

#[test]
fn set_of_a_type_property_is_read_back() {
    let mut graph = precedence_graph();
    run_write(
        &mut graph,
        "MATCH ()-[r:REL {w: 1}]->() SET r.type = 'set-type'",
    );
    assert_rows(
        &graph,
        "MATCH ()-[r:REL {w: 1}]->() RETURN r.type, type(r), properties(r).type",
        vec![vec![s("set-type"), s("REL"), s("set-type")]],
    );
}

// ========================================================================
// Relationship values
// ========================================================================

#[test]
fn every_stored_envelope_key_wins_on_a_value() {
    let graph = precedence_graph();
    for prefix in [
        "MATCH ()-[r:REL {text:'x'}]->() WITH collect(r) AS rs UNWIND rs AS x ",
        "MATCH ()-[r:REL {text:'x'}]->() WITH head(collect(r)) AS x ",
        "MATCH p = ()-[:REL {text:'x'}]->() WITH relationships(p)[0] AS x ",
    ] {
        // Pre-fix: [0, 'REL', 0, 1, …] — the slot id, type and endpoints.
        assert_rows(
            &graph,
            &format!("{prefix}RETURN {}", read_every_key("x")),
            vec![stored_row()],
        );
    }
    assert_rows(
        &graph,
        "MATCH p = ()-[:REL {text:'x'}]->() RETURN [x IN relationships(p) | x.type]",
        vec![vec![Value::List(vec![s("user-type")])]],
    );
}

#[test]
fn without_a_stored_key_a_value_falls_back_to_the_envelope() {
    let graph = precedence_graph();
    assert_rows(
        &graph,
        "MATCH ()-[r:REL {w: 1}]->() WITH collect(r)[0] AS x \
         RETURN x.type = type(x), x.id = id(x), x.start = id(startNode(x)), \
         x.end_id = id(endNode(x))",
        vec![vec![t(); 4]],
    );
}

#[test]
fn a_yielded_relationship_reads_its_stored_id() {
    let mut graph = precedence_graph();
    upsert_edge_embeddings(
        &mut graph,
        "REL",
        "text",
        vec![
            (EdgeIndex::new(0), vec![1.0, 0.0]),
            (EdgeIndex::new(1), vec![0.0, 1.0]),
        ],
        Some("cosine"),
    )
    .unwrap();
    // Pre-fix: [0, 'REL'] — knwler's `relation.id` came back as the slot.
    assert_rows(
        &graph,
        "CALL db.relationship_embeddings.query({type:'REL', text_column:'text', vector:[1.0, 0.0], \
         top_k:1, exact:true}) YIELD relationship RETURN relationship.id, relationship.type",
        vec![vec![s("user-7"), s("user-type")]],
    );
}
