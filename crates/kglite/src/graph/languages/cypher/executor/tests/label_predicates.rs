//! **Absolute goldens for label predicates inside parentheses.**
//!
//! `WHERE (a:Software OR a:Api)` was a `CypherSyntaxError` — the primary-
//! expression lookahead committed to a node pattern the moment it saw
//! `( <ident> :` and handed the rest to the MATCH-pattern parser, which
//! rejects `OR`. The unparenthesised `WHERE a:Software OR a:Api` parsed
//! fine, so the failure was purely the parenthesis.
//!
//! Every expectation below is an absolute count or value computed from
//! [`link_graph`] by hand; the differential corpus cannot see this class,
//! because the optimised and unoptimised paths both refused to parse.

use super::*;
use crate::graph::languages::cypher::planner;

/// Four nodes, four `LINKS_TO` edges:
///
/// ```text
/// s1:Software(stars 5) -> a1:Api(stars 2)
/// s1:Software          -> d1:Doc(stars 0)
/// a1:Api               -> s2:Software(stars 1)
/// d1:Doc               -> s1:Software
/// ```
fn link_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    let specs = [
        (1u32, "S1", "Software", 5i64),
        (2, "S2", "Software", 1),
        (3, "A1", "Api", 2),
        (4, "D1", "Doc", 0),
    ];
    let mut indices = Vec::new();
    for (id, title, label, stars) in specs {
        let node = NodeData::new(
            Value::UniqueId(id),
            Value::String(title.to_string()),
            label.to_string(),
            HashMap::from([
                ("name".to_string(), Value::String(title.to_string())),
                ("stars".to_string(), Value::Int64(stars)),
            ]),
            &mut graph.interner,
        );
        let idx = graph.graph.add_node(node);
        graph
            .type_indices
            .entry_or_default(label.to_string())
            .push(idx);
        indices.push(idx);
    }
    for (from, to) in [(0usize, 2usize), (0, 3), (2, 1), (3, 0)] {
        let edge = EdgeData::new("LINKS_TO".to_string(), HashMap::new(), &mut graph.interner);
        graph.graph.add_edge(indices[from], indices[to], edge);
    }
    graph.register_connection_type("LINKS_TO".to_string());
    graph
}

/// Run a read query on both the optimised and the unoptimised plan and
/// return the rows, panicking unless the two agree.
fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let no_params = HashMap::new();
    let parsed = parser::parse_cypher(query)
        .unwrap_or_else(|e| panic!("query failed to parse: {query}\n  error: {e}"));
    let plain = CypherExecutor::with_params(graph, &no_params, None)
        .execute(&parsed)
        .unwrap_or_else(|e| panic!("query failed: {query}\n  error: {e}"));
    let mut optimized = parsed;
    planner::optimize(&mut optimized, graph, &no_params);
    let planned = CypherExecutor::with_params(graph, &no_params, None)
        .execute(&optimized)
        .unwrap_or_else(|e| panic!("optimised query failed: {query}\n  error: {e}"));
    assert_eq!(
        plain.rows, planned.rows,
        "optimised and unoptimised plans disagree for: {query}"
    );
    planned.rows
}

/// Run a read query for its single cell.
fn one_cell(graph: &DirGraph, query: &str) -> Value {
    let rows = rows(graph, query);
    assert_eq!(rows.len(), 1, "expected one row from: {query}");
    assert_eq!(rows[0].len(), 1, "expected one column from: {query}");
    rows[0][0].clone()
}

/// The reported query. Qualifying edges are `s1->a1` and `a1->s2`;
/// `s1->d1` and `d1->s1` each have a `Doc` endpoint.
#[test]
fn parenthesised_label_disjunction_on_both_endpoints() {
    let graph = link_graph();
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (a)-[r:LINKS_TO]->(b) WHERE (a:Software OR a:Api) AND (b:Software OR b:Api) \
             RETURN count(r) AS c",
        ),
        Value::Int64(2),
    );
    // The same predicate without parentheses always worked; it must keep
    // answering identically.
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (a)-[r:LINKS_TO]->(b) WHERE a:Software OR a:Api AND b:Software OR b:Api \
             RETURN count(r) AS c",
        ),
        Value::Int64(3),
    );
}

/// The single-label forms around the fix: a bare `(n:Label)` keeps meaning
/// what it meant before the change (a node-pattern existence check on the
/// bound variable, which is truthy exactly when the label matches), and the
/// disjunction/negation/conjunction spellings now parse as expressions.
#[test]
fn parenthesised_label_predicates_count_the_right_nodes() {
    let graph = link_graph();
    for (query, expected) in [
        ("MATCH (n) WHERE (n:Software) RETURN count(n) AS c", 2),
        (
            "MATCH (n) WHERE (n:Software OR n:Api) RETURN count(n) AS c",
            3,
        ),
        (
            "MATCH (n) WHERE NOT (n:Software OR n:Api) RETURN count(n) AS c",
            1,
        ),
        (
            "MATCH (n) WHERE (n:Software AND n.stars > 1) RETURN count(n) AS c",
            1,
        ),
        (
            "MATCH (n) WHERE (n:Software XOR n:Api) RETURN count(n) AS c",
            3,
        ),
        (
            "MATCH (n) WHERE (n:Software OR n.stars = 0) RETURN count(n) AS c",
            3,
        ),
        (
            "MATCH (n) WHERE ((n:Software OR n:Api)) RETURN count(n) AS c",
            3,
        ),
        ("MATCH (n) WHERE (NOT n:Software) RETURN count(n) AS c", 2),
        // Label alternation is a node-pattern spelling, not an expression
        // operator; it stays on the pattern path.
        ("MATCH (n) WHERE (n:Software|Api) RETURN count(n) AS c", 3),
        // Two parenthesised label checks joined outside the parens — the
        // shape that already worked, pinned against the lookahead change.
        (
            "MATCH (a)-[:LINKS_TO]->(b) WHERE (a:Software) AND (b:Api) RETURN count(a) AS c",
            1,
        ),
    ] {
        assert_eq!(
            one_cell(&graph, query),
            Value::Int64(expected),
            "wrong count for: {query}"
        );
    }
}

/// A parenthesised node pattern with a relationship continuation still
/// desugars to `EXISTS { … }` rather than being parsed as an expression.
#[test]
fn parenthesised_pattern_predicates_still_reach_the_pattern_parser() {
    let graph = link_graph();
    // Nodes with an outgoing LINKS_TO: s1, a1, d1.
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (n) WHERE (n)-[:LINKS_TO]->() RETURN count(n) AS c"
        ),
        Value::Int64(3),
    );
    // Labelled source, so only s1.
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (n) WHERE (n:Software)-[:LINKS_TO]->(:Api) RETURN count(n) AS c",
        ),
        Value::Int64(1),
    );
    // Property map on the node pattern.
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (n) WHERE (n {name: 'S1'})-[:LINKS_TO]->() RETURN count(n) AS c",
        ),
        Value::Int64(1),
    );
}

/// Label disjunctions in value positions — RETURN, WITH and CASE all share
/// the same expression tower, so all three gain the parenthesised form.
#[test]
fn parenthesised_label_disjunction_in_value_positions() {
    let graph = link_graph();
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (n) WHERE n.name = 'A1' RETURN (n:Software OR n:Api) AS flag",
        ),
        Value::Boolean(true),
    );
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (n) WHERE n.name = 'D1' RETURN (n:Software OR n:Api) AS flag",
        ),
        Value::Boolean(false),
    );
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (n) WITH (n:Software OR n:Api) AS f WHERE f RETURN count(f) AS c",
        ),
        Value::Int64(3),
    );
    assert_eq!(
        one_cell(
            &graph,
            "MATCH (n) WHERE n.name = 'D1' \
             RETURN CASE WHEN (n:Software OR n:Api) THEN 'code' ELSE 'other' END AS kind",
        ),
        Value::String("other".to_string()),
    );
}
