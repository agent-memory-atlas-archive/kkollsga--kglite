//! **Absolute goldens for inline-map values written as expressions.**
//!
//! `MATCH (d {id: row[0]})` was a parse error ("Expected property key or '}'")
//! while `CREATE (:D {id: row[0]})` accepted it: a MATCH pattern is
//! re-serialized into the secondary pattern lexer, which reads only a scalar
//! literal, `$param`, `var` or `var.prop` as a value. Such a value now parses
//! with the expression grammar CREATE uses. Every case runs with the optimizer
//! off and on.

use super::*;
use crate::graph::core::pattern_matching::{PatternElement, PropertyMatcher};

fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    let query = parser::parse_cypher(
        "CREATE (a:D {id:'a', n: 2})-[:R {w: 5}]->(b:D {id:'b', n: 3}) CREATE (:D {id:'c'})",
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

fn read_with(graph: &DirGraph, source: &str, optimize: bool) -> Result<CypherResult, String> {
    let params = HashMap::from([("list".to_string(), Value::List(vec![s("x"), s("b")]))]);
    let mut query = parser::parse_cypher(source)
        .unwrap_or_else(|e| panic!("failed to parse: {source}\n  error: {e}"));
    if optimize {
        crate::graph::languages::cypher::planner::optimize(&mut query, graph, &params);
    }
    CypherExecutor::with_params(graph, &params, None).execute(&query)
}

fn assert_rows(graph: &DirGraph, source: &str, expected: Vec<Vec<Value>>) {
    for optimize in [false, true] {
        let rows = read_with(graph, source, optimize)
            .unwrap_or_else(|e| panic!("failed to execute: {source}\n  error: {e}"))
            .rows;
        assert_eq!(rows, expected, "rows for `{source}` (optimize={optimize})");
    }
}

fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

fn first_node_matcher(source: &str, key: &str) -> PropertyMatcher {
    let query = parser::parse_cypher(source).unwrap();
    let Clause::Match(m) = &query.clauses[0] else {
        panic!("MATCH expected")
    };
    let PatternElement::Node(node) = &m.patterns[0].elements[0] else {
        panic!("node expected")
    };
    node.properties.as_ref().unwrap()[key].clone()
}

#[test]
fn values_the_pattern_lexer_reads_keep_their_matchers() {
    assert!(matches!(
        first_node_matcher("MATCH (d:D {id: 'a'}) RETURN d", "id"),
        PropertyMatcher::Equals(Value::String(_))
    ));
    assert!(matches!(
        first_node_matcher("MATCH (d:D {id: $p}) RETURN d", "id"),
        PropertyMatcher::EqualsParam(_)
    ));
    assert!(matches!(
        first_node_matcher("MATCH (d:D {id: x.k}) RETURN d", "id"),
        PropertyMatcher::EqualsNodeProp { .. }
    ));
    // Pre-fix: a parse error naming a missing property key.
    assert!(matches!(
        first_node_matcher("MATCH (d:D {id: row[0], n: 1 + 1}) RETURN d", "id"),
        PropertyMatcher::EqualsExpr(_)
    ));
}

#[test]
fn row_dependent_expressions_match_per_row() {
    let graph = graph();
    for source in [
        "UNWIND [['a', 1]] AS row MATCH (d:D {id: row[0]}) RETURN d.id",
        "UNWIND [{k: 'a'}] AS row MATCH (d:D {id: row['k']}) RETURN d.id",
        "WITH ['x', 'a'] AS list MATCH (d:D {id: list[1]}) RETURN d.id",
        "WITH {k: ['a']} AS map MATCH (d:D {id: map.k[0]}) RETURN d.id",
        "UNWIND ['A'] AS name MATCH (d:D {id: toLower(name)}) RETURN d.id",
        "UNWIND [1] AS x MATCH (d:D {n: x + 1}) RETURN d.id",
    ] {
        assert_rows(&graph, source, vec![vec![s("a")]]);
    }
}

#[test]
fn constant_expressions_match_in_an_opening_match() {
    let graph = graph();
    for source in [
        "MATCH (d:D {id: toLower('A')}) RETURN d.id",
        "MATCH (d:D {n: 1 + 1}) RETURN d.id",
        "MATCH (d:D {id: $list[1]}) RETURN d.id",
    ] {
        let expected = if source.contains("$list") { "b" } else { "a" };
        assert_rows(&graph, source, vec![vec![s(expected)]]);
    }
    // Fused count shapes see the folded literal.
    assert_rows(
        &graph,
        "MATCH (d:D {n: 1 + 1}) RETURN count(*)",
        vec![vec![Value::Int64(1)]],
    );
    assert_rows(&graph, "MATCH (d:D {id: null}) RETURN d.id", vec![]);
}

#[test]
fn optional_match_relationship_maps_and_exists_take_expressions() {
    let graph = graph();
    assert_rows(
        &graph,
        "UNWIND [['zz']] AS row OPTIONAL MATCH (d:D {id: row[0]}) RETURN row[0], d.id",
        vec![vec![s("zz"), Value::Null]],
    );
    assert_rows(
        &graph,
        "UNWIND [[5]] AS row MATCH (:D)-[r:R {w: row[0]}]->(b) RETURN b.id",
        vec![vec![s("b")]],
    );
    assert_rows(
        &graph,
        "UNWIND [['b'], ['zz']] AS row RETURN row[0], EXISTS { (d:D {id: row[0]}) } AS e",
        vec![
            vec![s("b"), Value::Boolean(true)],
            vec![s("zz"), Value::Boolean(false)],
        ],
    );
}

#[test]
fn an_expression_that_fails_to_evaluate_is_an_error() {
    let graph = graph();
    for source in [
        "UNWIND [0] AS z MATCH (d:D {n: 1 / z}) RETURN d.id",
        "MATCH (d:D {n: 1 / 0}) RETURN d.id",
    ] {
        for optimize in [false, true] {
            assert!(
                read_with(&graph, source, optimize).is_err(),
                "`{source}` (optimize={optimize}) must surface the evaluation error"
            );
        }
    }
}
