//! Plan-shape tests for `CALL { }` bodies whose pattern reads an import.
//!
//! The differential corpus (`call_map_ref_*`) proves the answers agree; these
//! prove *why*: a body whose pattern reaches an imported variable only through
//! a property map (`{city: p.city}`, `{city: city}`) must keep every
//! graph-global fusion off, exactly as a body that anchors on the import
//! does — and a body that reaches no import must still fuse, or the guard
//! has quietly become a blanket bail.

use super::with_boundary_tests::with_boundary_graph;
use super::*;
use crate::graph::languages::cypher::parser::parse_cypher;

/// The optimised `CALL { }` body's clauses, for the first subquery in `text`.
fn optimised_call_body(text: &str) -> Vec<Clause> {
    let graph = with_boundary_graph();
    let mut query = parse_cypher(text).unwrap();
    optimize(&mut query, &graph, &HashMap::new());
    query
        .clauses
        .into_iter()
        .find_map(|clause| match clause {
            Clause::CallSubquery { body, .. } => Some(body.clauses),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no CALL body in {text}"))
}

/// A graph-global fused operator — one that scans the graph instead of the
/// seeded rows. `FusedOrderByTopK` is excluded on purpose: it is a row
/// operator over the (still seeded) MATCH ahead of it, so it stays legal.
fn body_has_graph_global_fusion(text: &str) -> bool {
    optimised_call_body(text).iter().any(|clause| {
        let shape = format!("{clause:?}");
        shape.starts_with("Fused") && !shape.starts_with("FusedOrderByTopK")
    })
}

#[test]
fn map_reference_to_import_keeps_fusions_off() {
    for text in [
        // node-scan aggregate
        "MATCH (p:P) CALL { WITH p MATCH (q:P {city: p.city}) RETURN count(q) AS k } RETURN k",
        // node-scan top-k
        "MATCH (p:P) CALL { WITH p MATCH (q:P {city: p.city}) RETURN q.title AS t \
         ORDER BY q.age DESC LIMIT 1 } RETURN t",
        // anchored edge count through a map on the start node
        "MATCH (p:P) CALL { WITH p MATCH (q:P {title: p.title})-[:K]->(f) RETURN count(f) AS k } \
         RETURN k",
        // scoped-call spelling
        "MATCH (p:P) CALL (p) { MATCH (q:P {city: p.city}) RETURN count(q) AS k } RETURN k",
        // an imported scalar, not a node
        "MATCH (p:P) WITH p, p.city AS city CALL { WITH city MATCH (q:P {city: city}) \
         RETURN count(q) AS k } RETURN k",
    ] {
        assert!(
            !body_has_graph_global_fusion(text),
            "a map reference to an import must disable graph-global fusion: {text} plan={:?}",
            optimised_call_body(text)
        );
    }
}

#[test]
fn body_without_import_reference_still_fuses() {
    for text in [
        "MATCH (p:P) CALL { WITH p MATCH (q:P {city: 'X'}) RETURN count(q) AS k } RETURN k",
        "MATCH (p:P) CALL { MATCH (q:P) RETURN count(q) AS k } RETURN k",
    ] {
        assert!(
            body_has_graph_global_fusion(text),
            "a body that reads no import must keep fusing: {text} plan={:?}",
            optimised_call_body(text)
        );
    }
}

#[test]
fn import_read_outside_the_pattern_keeps_fusions_off() {
    for text in [
        // import read from the WHERE: node-scan aggregate
        "MATCH (p:P) CALL (p) { MATCH (q:P) WHERE q.age >= p.age RETURN count(*) AS k } RETURN k",
        // import read from an aggregate argument
        "MATCH (p:P) CALL (p) { MATCH (q:P) RETURN sum(q.age + p.age) AS k } RETURN k",
        // import read from the ORDER BY: node-scan top-k
        "MATCH (p:P) CALL (p) { MATCH (q:P) RETURN q.title AS t \
         ORDER BY abs(q.age - p.age) LIMIT 1 } RETURN t",
        // legacy spelling
        "MATCH (p:P) CALL { WITH p MATCH (q:P) WHERE q.age IN [p.age, p.age + 1] \
         RETURN count(*) AS k } RETURN k",
    ] {
        assert!(
            !body_has_graph_global_fusion(text),
            "an import read outside the pattern must disable graph-global fusion: {text} plan={:?}",
            optimised_call_body(text)
        );
    }
}

/// The `limit_hint` on the first MATCH of the optimised `CALL` body, if any.
fn call_body_limit_hint(text: &str) -> Option<usize> {
    optimised_call_body(text)
        .iter()
        .find_map(|clause| match clause {
            Clause::Match(m) => Some(m.limit_hint),
            _ => None,
        })?
}

#[test]
fn call_body_with_residual_where_gets_no_limit_hint() {
    for text in [
        "MATCH (p:P) CALL (p) { MATCH (q:P) WHERE q.age + 0 > p.age RETURN q.age AS a LIMIT 2 } RETURN a",
        "CALL { MATCH (q:P) WHERE q.age + 0 > 30 RETURN q.age AS a LIMIT 2 } RETURN a",
        "MATCH (p:P) CALL (p) { MATCH (q:P) WHERE q.age + 0 > p.age WITH q LIMIT 2 RETURN count(*) AS a } RETURN a",
    ] {
        assert_eq!(
            call_body_limit_hint(text),
            None,
            "a CALL body's first MATCH runs on a seed row; a hint ahead of its WHERE under-fills: {text}"
        );
    }
}

#[test]
fn call_body_without_where_keeps_its_limit_hint() {
    // Non-vacuity for the bail above: it is scoped to a residual WHERE.
    let text = "MATCH (p:P) CALL (p) { MATCH (q:P) RETURN q.age AS a LIMIT 2 } RETURN a";
    assert_eq!(call_body_limit_hint(text), Some(2));
}

/// The leading MATCH's `limit_hint` in the optimised top-level query.
fn top_level_limit_hint(text: &str) -> Option<usize> {
    let graph = with_boundary_graph();
    let mut query = parse_cypher(text).unwrap();
    optimize(&mut query, &graph, &HashMap::new());
    query.clauses.iter().find_map(|clause| match clause {
        Clause::Match(m) => Some(m.limit_hint),
        _ => None,
    })?
}

#[test]
fn top_level_read_keeps_its_limit_hint_over_a_residual_where() {
    // The top-level read route folds the WHERE ahead of the cap, so the early
    // stop must survive there.
    assert_eq!(
        top_level_limit_hint("MATCH (q:P) WHERE q.age + 0 > 30 RETURN q.age AS a LIMIT 2"),
        Some(2)
    );
}

#[test]
fn mutation_query_with_residual_where_gets_no_limit_hint() {
    for text in [
        "MATCH (q:P) WHERE q.age + 0 > 30 WITH q LIMIT 2 DELETE q RETURN count(*) AS n",
        "MATCH (q:P) WHERE q.age + 0 > 30 WITH q LIMIT 2 SET q.flag = 1 RETURN count(*) AS n",
    ] {
        assert_eq!(top_level_limit_hint(text), None, "{text}");
    }
}
