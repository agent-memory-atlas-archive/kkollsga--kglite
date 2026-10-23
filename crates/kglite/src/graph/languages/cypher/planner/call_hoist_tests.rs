//! Plan-shape and answer tests for `hoist_where_over_call_subquery`.
//!
//! The differential corpus proves the hoisted plan answers what the plain one
//! does; these prove the hoist *happened* on each trigger shape and *did not*
//! on each bail, which the corpus cannot see in either direction.

use super::with_boundary_tests::with_boundary_graph;
use super::*;
use crate::graph::languages::cypher::parser::parse_cypher;

const PASS: &str = "hoist_where_over_call_subquery";

/// Every other pass off: the clause list shows this pass's rewrite alone.
fn only_this_pass(text: &str, enabled: bool) -> Vec<String> {
    let graph = with_boundary_graph();
    let mut query = parse_cypher(text).unwrap();
    let disabled: HashSet<String> = all_pass_names()
        .into_iter()
        .filter(|n| !(enabled && n == PASS))
        .collect();
    optimize_with_disabled(&mut query, &graph, &HashMap::new(), &disabled);
    query
        .clauses
        .iter()
        .map(|c| {
            let name = format!("{c:?}");
            let kind = name.split([' ', '(', '{']).next().unwrap().to_string();
            match c {
                Clause::With(w) if w.where_clause.is_some() => format!("{kind}+where"),
                _ => kind,
            }
        })
        .collect()
}

fn hoisted(text: &str) -> bool {
    let shape = only_this_pass(text, true);
    let call = shape.iter().position(|k| k == "CallSubquery").unwrap();
    shape[call - 1] == "Where"
}

const CALL_COUNT: &str = "CALL (p) { MATCH (p)-[:K]->(q) RETURN count(q) AS c }";

#[test]
fn the_hoist_fires_on_every_trigger_shape() {
    for text in [
        // Standalone WHERE directly after the CALL.
        format!("MATCH (p:P) {CALL_COUNT} WHERE p.age > 30 RETURN p.title AS t, c"),
        // WHERE attached to a WITH after the CALL.
        format!("MATCH (p:P) {CALL_COUNT} WITH p, c WHERE p.age > 30 RETURN p.title AS t, c"),
        // The GraphBridge Q15 shape: collect + LIMIT body, two aliasing WITHs.
        "MATCH (p:P) CALL (p) { OPTIONAL MATCH (a)-[:K]->(p) WITH a LIMIT 2 RETURN collect(a) AS ps } \
         WITH p, size(ps) AS n, CASE WHEN size(ps) = 1 THEN ps[0] ELSE null END AS a \
         WITH p, n, a AS s WHERE p.age = 45 RETURN p.title AS t, n"
            .to_string(),
        // The idedge shape: a count body over a correlated WHERE.
        "MATCH (p:P)-[r:K]->(q:P) CALL (r) { MATCH ()-[x:K]->() WHERE x.nope = r.nope \
         WITH x LIMIT 2 RETURN count(*) AS c } WITH p, r, q, c WHERE q.age > 30 RETURN c"
            .to_string(),
        // A legacy importing WITH.
        "MATCH (p:P) CALL { WITH p MATCH (p)-[:K]->(q) RETURN count(q) AS c } \
         WITH p, c WHERE p.age > 30 RETURN c"
            .to_string(),
    ] {
        assert!(hoisted(&text), "expected a hoist: {text}");
        assert!(!only_this_pass(&text, true).iter().any(|k| k == "With+where"), "{text}");
    }
}

#[test]
fn a_disabled_pass_leaves_the_filter_behind_the_call() {
    let text = format!("MATCH (p:P) {CALL_COUNT} WHERE p.age > 30 RETURN c");
    assert_eq!(
        only_this_pass(&text, false),
        ["Match", "CallSubquery", "Where", "Return"]
    );
    assert_eq!(
        only_this_pass(&text, true),
        ["Match", "Where", "CallSubquery", "Return"]
    );
}

#[test]
fn only_the_pre_call_conjuncts_move() {
    let text = format!("MATCH (p:P) {CALL_COUNT} WHERE p.age > 30 AND c > 0 RETURN c");
    assert_eq!(
        only_this_pass(&text, true),
        ["Match", "Where", "CallSubquery", "Where", "Return"]
    );
}

#[test]
fn the_hoist_declines_every_bail_shape() {
    for (text, why) in [
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) WHERE rand() < 0.5 RETURN count(q) AS c } \
             WHERE p.age > 30 RETURN c",
            "B3 - a volatile function in the body",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN q.age AS a } \
             WHERE p.age > 30 RETURN a",
            "B1 - no aggregate: zero or many rows per input",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN q.age AS a, count(*) AS c } \
             WHERE p.age > 30 RETURN a, c",
            "B1 - a grouping key: many rows per input",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN count(q) AS c } WHERE c > 0 RETURN c",
            "H1 - the conjunct reads a CALL output",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN count(q) AS c } \
             WITH p.age AS p, c WHERE p > 30 RETURN c",
            "H2 - an intervening WITH rebinds the name",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN count(q) AS c } \
             WITH p, c ORDER BY c LIMIT 2 WHERE p.age > 30 RETURN c",
            "an ORDER BY / LIMIT sits between the CALL and the filter",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN count(q) AS c } \
             WITH DISTINCT p, c WHERE p.age > 30 RETURN c",
            "a DISTINCT WITH between",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN count(q) AS c } \
             WHERE p.age > rand() * 100 RETURN c",
            "H3 - a volatile function in the conjunct",
        ),
        (
            "MATCH (p:P) CALL (p) { MATCH (p)-[:K]->(q) RETURN count(q) AS c } \
             WHERE p.age > 30 RETURN c UNION MATCH (p:P) RETURN 1 AS c",
            "a UNION in the scope",
        ),
    ] {
        match parse_cypher(text) {
            Ok(query) => {
                let _ = query;
                let shape = only_this_pass(text, true);
                if let Some(call) = shape.iter().position(|k| k == "CallSubquery") {
                    assert_ne!(shape[call - 1], "Where", "must not hoist ({why}): {text}");
                }
            }
            Err(e) => panic!("fixture does not parse ({why}): {e}: {text}"),
        }
    }
}

/// The parser refuses a write inside a CALL body today, so the write guard
/// (B2) is exercised on a body that gained one after parsing.
#[test]
fn a_body_with_a_write_clause_is_not_hoisted_over() {
    let graph = with_boundary_graph();
    let write = parse_cypher("MATCH (q:P) DELETE q")
        .unwrap()
        .clauses
        .pop()
        .unwrap();
    assert!(matches!(write, Clause::Delete(_)));
    let mut query = parse_cypher(&format!(
        "MATCH (p:P) {CALL_COUNT} WHERE p.age > 30 RETURN c"
    ))
    .unwrap();
    let Clause::CallSubquery { body, .. } = &mut query.clauses[1] else {
        panic!("no CALL");
    };
    body.clauses.insert(1, write);
    let disabled: HashSet<String> = all_pass_names().into_iter().filter(|n| n != PASS).collect();
    optimize_with_disabled(&mut query, &graph, &HashMap::new(), &disabled);
    assert!(matches!(query.clauses[1], Clause::CallSubquery { .. }));
}

const TOP_K_TAIL: &str = "RETURN p.title AS t, c ORDER BY p.age DESC LIMIT 3";

#[test]
fn the_top_k_moves_ahead_of_the_call() {
    for (text, expected) in [
        (
            format!("MATCH (p:P) {CALL_COUNT} {TOP_K_TAIL}"),
            vec![
                "Match",
                "OrderBy",
                "Limit",
                "CallSubquery",
                "Return",
                "OrderBy",
                "Limit",
            ],
        ),
        // The filter moves too, and the sort lands behind it.
        (
            format!("MATCH (p:P) {CALL_COUNT} WITH p, c WHERE p.age > 30 {TOP_K_TAIL}"),
            vec![
                "Match",
                "Where",
                "OrderBy",
                "Limit",
                "CallSubquery",
                "With",
                "Return",
                "OrderBy",
                "Limit",
            ],
        ),
    ] {
        assert_eq!(only_this_pass(&text, true), expected, "{text}");
    }
}

#[test]
fn the_top_k_declines_every_bail_shape() {
    for (tail, why) in [
        (
            "RETURN p.title AS t, c ORDER BY p.age DESC SKIP 1 LIMIT 3",
            "a SKIP",
        ),
        (
            "RETURN p.title AS t, c ORDER BY c DESC LIMIT 3",
            "a key over a CALL output",
        ),
        (
            "RETURN p.title AS t, c ORDER BY t LIMIT 3",
            "a key over a RETURN alias",
        ),
        (
            "RETURN p.age AS p, c ORDER BY p LIMIT 3",
            "a RETURN alias that rebinds a live name",
        ),
        (
            "RETURN count(*) AS n ORDER BY n LIMIT 3",
            "an aggregating RETURN",
        ),
        (
            "RETURN DISTINCT p.city AS t ORDER BY t LIMIT 3",
            "a DISTINCT RETURN",
        ),
        (
            "RETURN p.title AS t, c ORDER BY p.age * rand() LIMIT 3",
            "a volatile key",
        ),
        ("RETURN p.title AS t, c LIMIT 3", "no ORDER BY"),
        ("RETURN p.title AS t, c ORDER BY p.age", "no LIMIT"),
    ] {
        let text = format!("MATCH (p:P) {CALL_COUNT} {tail}");
        let shape = only_this_pass(&text, true);
        assert_eq!(shape[1], "CallSubquery", "must not hoist ({why}): {text}");
    }
    // A filter that stays behind the CALL would drop rows after an early cut.
    let text = format!("MATCH (p:P) {CALL_COUNT} WHERE c > 1 {TOP_K_TAIL}");
    assert_eq!(only_this_pass(&text, true)[1], "CallSubquery", "{text}");
    // A WITH that thins the stream.
    let text = format!(
        "MATCH (p:P) {CALL_COUNT} WITH DISTINCT p, c RETURN p.title AS t, c ORDER BY p.age LIMIT 3"
    );
    assert_eq!(only_this_pass(&text, true)[1], "CallSubquery", "{text}");
}
