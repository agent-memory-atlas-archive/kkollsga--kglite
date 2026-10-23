//! Plan-shape goldens for the null-safe equality / IN seek candidates: a
//! non-NULL scalar (or NULL-free list) reaches the node matcher while the
//! original predicate stays in WHERE; a NULL param or NULL list element
//! leaves the plan unrewritten.

use super::*;
use crate::graph::core::pattern_matching::{PatternElement, PropertyMatcher};
use crate::graph::languages::cypher::parser::parse_cypher;

const EQ: &str = "MATCH (n:S) WHERE coalesce(n.p = $v, (n.p IS NULL AND $v IS NULL)) RETURN n";
const EQ_SWAPPED: &str =
    "MATCH (n:S) WHERE coalesce($v = n.p, ($v IS NULL AND n.p IS NULL)) RETURN n";
const IN: &str = "MATCH (n:S) WHERE (coalesce(n.p IN $v, false) OR (n.p IS NULL AND any(x IN $v WHERE x IS NULL))) RETURN n";

fn planned(cypher: &str, v: Value) -> CypherQuery {
    let mut query = parse_cypher(cypher).unwrap();
    optimize(
        &mut query,
        &DirGraph::new(),
        &HashMap::from([("v".to_string(), v)]),
    );
    query
}

fn matcher(query: &CypherQuery) -> Option<PropertyMatcher> {
    query.clauses.iter().find_map(|clause| match clause {
        Clause::Match(m) => m.patterns[0].elements.iter().find_map(|el| match el {
            PatternElement::Node(np) => np.properties.as_ref()?.get("p").cloned(),
            _ => None,
        }),
        _ => None,
    })
}

fn has_where(query: &CypherQuery) -> bool {
    query.clauses.iter().any(|c| matches!(c, Clause::Where(_)))
}

#[test]
fn non_null_param_seeks_and_keeps_where() {
    for cypher in [EQ, EQ_SWAPPED] {
        let q = planned(cypher, Value::Int64(7));
        assert!(
            matches!(matcher(&q), Some(PropertyMatcher::Equals(Value::Int64(7)))),
            "{cypher}: {:?}",
            matcher(&q)
        );
        assert!(has_where(&q), "{cypher}: safety-net WHERE dropped");
    }
}

#[test]
fn null_param_is_left_unrewritten() {
    for cypher in [EQ, EQ_SWAPPED] {
        let q = planned(cypher, Value::Null);
        assert!(matcher(&q).is_none(), "{cypher}: {:?}", matcher(&q));
        assert!(has_where(&q));
    }
}

#[test]
fn in_list_without_null_seeks_and_keeps_where() {
    let q = planned(IN, Value::List(vec![Value::Int64(1), Value::Int64(2)]));
    assert!(
        matches!(matcher(&q), Some(PropertyMatcher::In(v)) if v.len() == 2),
        "{:?}",
        matcher(&q)
    );
    assert!(has_where(&q));
}

#[test]
fn in_list_with_null_or_null_list_is_left_unrewritten() {
    for v in [Value::List(vec![Value::Int64(1), Value::Null]), Value::Null] {
        let q = planned(IN, v.clone());
        assert!(matcher(&q).is_none(), "{v:?}: {:?}", matcher(&q));
        assert!(has_where(&q));
    }
}

#[test]
fn near_miss_shapes_are_not_matched() {
    // Different property in the NULL arm: not the null-safe form.
    let q = planned(
        "MATCH (n:S) WHERE coalesce(n.p = $v, (n.q IS NULL AND $v IS NULL)) RETURN n",
        Value::Int64(7),
    );
    assert!(matcher(&q).is_none());
    // A default that is not the null-safe conjunction.
    let q = planned(
        "MATCH (n:S) WHERE coalesce(n.p = $v, true) RETURN n",
        Value::Int64(7),
    );
    assert!(matcher(&q).is_none());
}
