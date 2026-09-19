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
