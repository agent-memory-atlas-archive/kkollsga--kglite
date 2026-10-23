//! Plan-shape and scope tests for `fuse_with_top_k`.
//!
//! The differential corpus shows the fused plan answers what the unfused plan
//! answers; these show the pass fired on each trigger shape, stayed out of each
//! bail shape, and left the scope a `WITH` leaves.

use crate::graph::languages::cypher::parser::parse_cypher;
use crate::graph::languages::cypher::planner::with_boundary_tests::with_boundary_graph;
use crate::graph::languages::cypher::planner::*;

fn optimized(graph: &DirGraph, text: &str, disabled: &[&str]) -> CypherQuery {
    let mut query = parse_cypher(text).unwrap();
    let disabled: HashSet<String> = disabled.iter().map(|name| name.to_string()).collect();
    optimize_with_disabled(&mut query, graph, &HashMap::new(), &disabled);
    query
}

/// The WITH-scoped top-K operators in the optimised plan.
fn with_scoped_top_ks(query: &CypherQuery) -> usize {
    query
        .clauses
        .iter()
        .filter(|c| {
            matches!(
                c,
                Clause::FusedVectorScoreTopK {
                    with_scope: true,
                    ..
                } | Clause::FusedTextBm25TopK {
                    with_scope: true,
                    ..
                } | Clause::FusedOrderByTopK {
                    with_scope: true,
                    ..
                }
            )
        })
        .count()
}

fn rows(graph: &DirGraph, text: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let query = optimized(graph, text, &[]);
    crate::graph::languages::cypher::executor::CypherExecutor::with_params(graph, &params, None)
        .execute(&query)
        .unwrap()
        .rows
}

fn naive_rows(graph: &DirGraph, text: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let mut query = parse_cypher(text).unwrap();
    let all: HashSet<String> = all_pass_names().into_iter().collect();
    optimize_with_disabled(&mut query, graph, &params, &all);
    crate::graph::languages::cypher::executor::CypherExecutor::with_params(graph, &params, None)
        .execute(&query)
        .unwrap()
        .rows
}

fn s(text: &str) -> Value {
    Value::String(text.into())
}

#[test]
fn a_with_top_k_fuses_with_a_clause_behind_it() {
    let graph = with_boundary_graph();
    for text in [
        "MATCH (p:P) WITH p, p.age AS a ORDER BY a DESC LIMIT 2 \
         OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u",
        "MATCH (p:P) WITH p, p.age AS a ORDER BY a DESC LIMIT 2 \
         MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u",
        "MATCH (p:P) WITH p, p.age AS a ORDER BY a DESC LIMIT 2 \
         WITH p, a WHERE a > 0 RETURN p.title AS t, a",
        // Two keys, one DESC and one ASC.
        "MATCH (p:P) WITH p, p.city AS c, p.age AS a ORDER BY c DESC, a LIMIT 3 \
         OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u",
    ] {
        let query = optimized(&graph, text, &[]);
        assert_eq!(with_scoped_top_ks(&query), 1, "should fuse: {text}");
        assert!(
            !query.clauses.iter().any(|c| matches!(c, Clause::Limit(_))),
            "the LIMIT should be gone: {text}"
        );
    }
}

#[test]
fn a_retrieval_score_sort_takes_its_own_operator() {
    let graph = with_boundary_graph();
    let vector = optimized(
        &graph,
        "MATCH (p:P) WITH p, vector_score(p, 'title_emb', [1.0, 0.0]) AS s ORDER BY s DESC LIMIT 2 \
         OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, s, q.title AS u",
        &[],
    );
    assert!(vector.clauses.iter().any(|c| matches!(
        c,
        Clause::FusedVectorScoreTopK {
            with_scope: true,
            ..
        }
    )));
    let bm25 = optimized(
        &graph,
        "MATCH (p:P) WITH p, text_bm25(p, 'title', 'a') AS s ORDER BY s DESC LIMIT 2 \
         OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, s, q.title AS u",
        &[],
    );
    assert!(bm25.clauses.iter().any(|c| matches!(
        c,
        Clause::FusedTextBm25TopK {
            with_scope: true,
            ..
        }
    )));
    // The existing rule: only DESC with NULLS FIRST reaches the HNSW operator.
    for tail in ["ORDER BY s ASC", "ORDER BY s DESC NULLS LAST"] {
        let query = optimized(
            &graph,
            &format!(
                "MATCH (p:P) WITH p, vector_score(p, 'title_emb', [1.0, 0.0]) AS s {tail} LIMIT 2 \
                 OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, s, q.title AS u"
            ),
            &[],
        );
        assert!(
            !query
                .clauses
                .iter()
                .any(|c| matches!(c, Clause::FusedVectorScoreTopK { .. })),
            "{tail} must stay off the vector operator"
        );
        assert!(
            query.clauses.iter().any(|c| matches!(
                c,
                Clause::FusedOrderByTopK {
                    with_scope: true,
                    ..
                }
            )),
            "{tail} still takes the generic heap"
        );
    }
}

#[test]
fn every_bail_shape_leaves_the_with_alone() {
    let graph = with_boundary_graph();
    for (text, why) in [
        (
            "MATCH (p:P) WITH DISTINCT p.city AS c ORDER BY c LIMIT 1 \
             OPTIONAL MATCH (q:P {city: c}) RETURN c, q.title AS u",
            "W1 DISTINCT",
        ),
        (
            "MATCH (p:P) WITH p.city AS c, count(*) AS k ORDER BY k DESC LIMIT 1 \
             OPTIONAL MATCH (q:P {city: c}) RETURN c, k, q.title AS u",
            "W1 aggregating WITH",
        ),
        (
            "MATCH (p:P) WITH * ORDER BY p.age LIMIT 2 \
             OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u",
            "W1 WITH *",
        ),
        (
            "MATCH (p:P) WITH p, p.age AS a ORDER BY a SKIP 1 LIMIT 2 \
             OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u",
            "W2 SKIP",
        ),
        (
            "MATCH (p:P) WITH p, p.age AS a ORDER BY a LIMIT $k \
             OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u",
            "W2 non-literal LIMIT",
        ),
        (
            "MATCH (p:P) WITH p, p.age AS a ORDER BY a + 1 LIMIT 2 \
             OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u",
            "W3 a key that reads a WITH alias",
        ),
        (
            "MATCH (p:P) WITH p, p.age AS a ORDER BY a LIMIT 2 \
             SET p.seen = true RETURN count(*) AS n",
            "W4 write clause",
        ),
    ] {
        let query = optimized(&graph, text, &[]);
        assert_eq!(
            with_scoped_top_ks(&query),
            0,
            "must not fuse ({why}): {text}"
        );
    }
}

#[test]
fn disabling_the_pass_keeps_the_with() {
    let graph = with_boundary_graph();
    let text = "MATCH (p:P) WITH p, p.age AS a ORDER BY a DESC LIMIT 2 \
         OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u";
    let query = optimized(&graph, text, &["fuse_with_top_k"]);
    assert_eq!(with_scoped_top_ks(&query), 0);
    assert!(query.clauses.iter().any(|c| matches!(c, Clause::With(_))));
}

/// A variable the WITH does not project must not be visible behind it. The
/// fused clause starts from the leaked binding `p` (node 1) if the executor
/// skips the restriction: `MATCH (p:P {id: 3})` then finds `p` already bound
/// to a node that is not id 3 and answers nothing.
#[test]
fn a_non_projected_variable_does_not_leak_past_the_with() {
    let graph = with_boundary_graph();
    let text = "MATCH (p:P {id: 1}) WITH p.age AS a ORDER BY a DESC LIMIT 1 \
         MATCH (p:P {id: 3}) RETURN p.title AS t, a";
    assert_eq!(with_scoped_top_ks(&optimized(&graph, text, &[])), 1);
    assert_eq!(rows(&graph, text), vec![vec![s("c"), Value::Int64(25)]]);
    assert_eq!(naive_rows(&graph, text), rows(&graph, text));
}

#[test]
fn the_fused_plan_answers_what_the_with_answered() {
    let graph = with_boundary_graph();
    for text in [
        "MATCH (p:P) WITH p, p.age AS a ORDER BY a DESC LIMIT 2 \
         OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, a, q.title AS u ORDER BY t, u",
        "MATCH (p:P) WITH p, p.city AS c, p.age AS a ORDER BY c DESC, a LIMIT 3 \
         MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u ORDER BY t, u",
        // A tie on the sort key: both paths keep input order.
        "MATCH (p:P) WITH p, p.city AS c ORDER BY c LIMIT 3 \
         OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u ORDER BY t, u",
    ] {
        assert_eq!(
            with_scoped_top_ks(&optimized(&graph, text, &[])),
            1,
            "{text}"
        );
        assert_eq!(rows(&graph, text), naive_rows(&graph, text), "{text}");
    }
    assert_eq!(
        rows(
            &graph,
            "MATCH (p:P) WITH p, p.age AS a ORDER BY a DESC LIMIT 2 \
             OPTIONAL MATCH (p)-[:K]->(q:P) RETURN p.title AS t, q.title AS u ORDER BY t, u"
        ),
        vec![vec![s("c"), s("d")], vec![s("d"), Value::Null],]
    );
}
