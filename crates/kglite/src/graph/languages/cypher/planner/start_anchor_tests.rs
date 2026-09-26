//! `optimize_pattern_start_node`: a point-anchored endpoint — a variable
//! bound earlier, or an `id` / index equality whose value is known per input
//! row — starts the pattern even when the other end's candidate estimate ties
//! it (a one-node hub type), whichever end it was written on.

use super::*;
use crate::graph::core::pattern_matching::PatternElement;
use crate::graph::languages::cypher::parser::parse_cypher;

/// One `Hub` node and 100 `Doc` nodes — the hub type's candidate count (1)
/// ties a point lookup, which is exactly the shape the tie-break decides.
fn hub_and_docs() -> DirGraph {
    let mut graph = DirGraph::new();
    graph
        .type_indices
        .entry_or_default("Hub".to_string())
        .push(petgraph::graph::NodeIndex::new(0));
    graph
        .type_indices
        .entry_or_default("Doc".to_string())
        .extend((1..101).map(petgraph::graph::NodeIndex::new));
    graph
}

/// Start-node variable of every MATCH / OPTIONAL MATCH pattern, in order.
fn start_vars(query: &str) -> Vec<Option<String>> {
    let mut parsed = parse_cypher(query).unwrap();
    let params = HashMap::from([
        ("x".to_string(), Value::Int64(5)),
        (
            "batch".to_string(),
            Value::List(vec![Value::Int64(5), Value::Int64(6)]),
        ),
    ]);
    optimize(&mut parsed, &hub_and_docs(), &params);
    parsed
        .clauses
        .iter()
        .filter_map(|clause| match clause {
            Clause::Match(m) | Clause::OptionalMatch(m) => Some(m),
            _ => None,
        })
        .flat_map(|m| m.patterns.iter())
        .map(|pattern| match &pattern.elements[0] {
            PatternElement::Node(np) => np.variable.clone(),
            PatternElement::Edge(_) => None,
        })
        .collect()
}

fn d() -> Option<String> {
    Some("d".to_string())
}

#[test]
fn row_bound_id_equality_anchors_the_far_end() {
    assert_eq!(
        start_vars("UNWIND $batch AS e MATCH (h:Hub)-[r:CLAIMS]->(d:Doc {id: e.id}) RETURN r"),
        vec![d()]
    );
}

#[test]
fn row_bound_id_equality_anchors_when_written_first() {
    assert_eq!(
        start_vars("UNWIND $batch AS e MATCH (d:Doc {id: e.id})<-[r:CLAIMS]-(h:Hub) RETURN r"),
        vec![d()]
    );
}

#[test]
fn constant_and_param_id_equalities_anchor() {
    assert_eq!(
        start_vars("MATCH (h:Hub)-[r:CLAIMS]->(d:Doc {id: 5}) RETURN r"),
        vec![d()]
    );
    assert_eq!(
        start_vars("MATCH (h:Hub)-[r:CLAIMS]->(d:Doc {id: $x}) RETURN r"),
        vec![d()]
    );
}

#[test]
fn with_bound_and_expression_id_equalities_anchor() {
    assert_eq!(
        start_vars("WITH 5 AS x MATCH (h:Hub)-[r:CLAIMS]->(d:Doc {id: x}) RETURN r"),
        vec![d()]
    );
    assert_eq!(
        start_vars("UNWIND $batch AS v MATCH (h:Hub)-[r:CLAIMS]->(d:Doc {id: v + 0}) RETURN r"),
        vec![d()]
    );
}

#[test]
fn optional_match_row_bound_id_anchors() {
    assert_eq!(
        start_vars(
            "UNWIND $batch AS e OPTIONAL MATCH (h:Hub)-[r:CLAIMS]->(d:Doc {id: e.id}) RETURN r"
        ),
        vec![d()]
    );
}

#[test]
fn earlier_bound_endpoint_anchors_a_later_clause() {
    assert_eq!(
        start_vars(
            "UNWIND $batch AS e MATCH (d:Doc {id: e.id}) \
             MATCH (h:Hub)-[r:CLAIMS]->(d) RETURN r"
        ),
        vec![d(), d()]
    );
}

#[test]
fn unindexed_row_equality_does_not_win_a_tie() {
    // `title` answers no point lookup here, so the hub keeps its place.
    assert_eq!(
        start_vars("UNWIND $batch AS e MATCH (h:Hub)-[r:CLAIMS]->(d:Doc {title: e.t}) RETURN r"),
        vec![Some("h".to_string())]
    );
}

#[test]
fn both_ends_anchored_keeps_the_written_order() {
    assert_eq!(
        start_vars("MATCH (h:Hub {id: 0})-[r:CLAIMS]->(d:Doc {id: 5}) RETURN r"),
        vec![Some("h".to_string())]
    );
}

/// Start variables of the MATCH patterns inside the query's `CALL { }` bodies.
fn call_body_start_vars(query: &str) -> Vec<Option<String>> {
    let mut parsed = parse_cypher(query).unwrap();
    optimize(&mut parsed, &hub_and_docs(), &HashMap::new());
    parsed
        .clauses
        .iter()
        .filter_map(|clause| match clause {
            Clause::CallSubquery { body, .. } => Some(body),
            _ => None,
        })
        .flat_map(|body| body.clauses.iter())
        .filter_map(|clause| match clause {
            Clause::Match(m) => Some(m),
            _ => None,
        })
        .flat_map(|m| m.patterns.iter())
        .map(|pattern| match &pattern.elements[0] {
            PatternElement::Node(np) => np.variable.clone(),
            PatternElement::Edge(_) => None,
        })
        .collect()
}

fn s() -> Option<String> {
    Some("s".to_string())
}

#[test]
fn call_import_anchors_against_a_labelled_far_end() {
    // The imported `s` resolves to one node per row; reversing onto the
    // `:Doc` label scan walked every Doc for every row.
    for query in [
        "MATCH (s:Hub) CALL { WITH s MATCH (s)--(:Doc) RETURN count(*) AS c } RETURN c",
        "MATCH (s:Hub) CALL (s) { MATCH (s)--(:Doc) RETURN count(*) AS c } RETURN c",
    ] {
        assert_eq!(call_body_start_vars(query), vec![s()], "{query}");
    }
}

#[test]
fn a_value_bound_by_with_or_unwind_anchors_a_later_match() {
    assert_eq!(
        start_vars("MATCH (h:Hub) WITH collect(h) AS hs UNWIND hs AS s MATCH (s)--(:Doc) RETURN s"),
        vec![Some("h".to_string()), s()]
    );
    assert_eq!(
        start_vars(
            "MATCH (d:Doc)<-[r:CLAIMS]-() WITH startNode(r) AS s MATCH (s)--(:Doc) RETURN s"
        ),
        vec![d(), s()]
    );
}

/// 20 `A` nodes valid throughout, and 60 `B` versions of which exactly one is
/// valid on 2020-06-01 — the rest are one-day versions in 2000.
fn versioned() -> DirGraph {
    use crate::graph::features::temporal::{declare, IntervalConvention, TemporalTarget};
    use crate::graph::session::execute::{execute_mut, ExecuteOptions};
    let mut graph = DirGraph::new();
    let params = HashMap::new();
    for query in [
        "UNWIND range(1, 20) AS i \
         CREATE (:A {id: i, vf: date('1990-01-01'), vt: date('2100-01-01')})",
        "UNWIND range(1, 59) AS i \
         CREATE (:B {id: i, vf: date('2000-01-01') + i, vt: date('2000-01-01') + i})",
        "CREATE (:B {id: 60, vf: date('2020-01-01'), vt: date('2100-01-01')})",
        "MATCH (a:A), (b:B) WHERE b.id % 20 + 1 = a.id CREATE (a)-[:R]->(b)",
    ] {
        execute_mut(&mut graph, query, &ExecuteOptions::eager(&params)).unwrap();
    }
    for label in ["A", "B"] {
        let target = TemporalTarget::Node(label.to_string());
        declare(&mut graph, &target, "vf", "vt", IntervalConvention::Closed).unwrap();
    }
    graph
}

fn versioned_start(query: &str) -> Option<String> {
    let graph = versioned();
    let mut parsed = parse_cypher(query).unwrap();
    let params = HashMap::from([("t".to_string(), Value::String("2020-06-01".into()))]);
    optimize(&mut parsed, &graph, &params);
    let Some(Clause::Match(m)) = parsed.clauses.first() else {
        panic!("{query}: no leading MATCH");
    };
    match &m.patterns[0].elements[0] {
        PatternElement::Node(np) => np.variable.clone(),
        PatternElement::Edge(_) => None,
    }
}

#[test]
fn a_literal_context_instant_anchors_on_the_label_nearly_empty_at_it() {
    let pattern = "MATCH (a:A)-[:R]->(b:B) RETURN a, b";
    let b = Some("b".to_string());
    let a = Some("a".to_string());
    // 20 A against 60 B: A starts without a context.
    assert_eq!(versioned_start(pattern), a);
    // One B is valid on the instant, so B starts.
    assert_eq!(
        versioned_start(&format!(
            "FOR VALID_TIME AS OF date('2020-06-01') {pattern}"
        )),
        b
    );
    assert_eq!(
        versioned_start(&format!("FOR VALID_TIME AS OF '2020-06-01' {pattern}")),
        b
    );
    // An instant known only per execution keeps the full counts.
    assert_eq!(
        versioned_start(&format!("FOR VALID_TIME AS OF $t {pattern}")),
        a
    );
    assert_eq!(
        versioned_start(&format!("FOR VALID_TIME AS OF date() {pattern}")),
        a
    );
}
