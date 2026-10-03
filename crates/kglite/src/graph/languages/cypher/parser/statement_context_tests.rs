//! Parse goldens for the statement prefix `FOR <axis> AS OF <instant>`.

use super::super::parse_cypher;
use crate::datatypes::values::Value;
use crate::graph::languages::cypher::ast::{ContextInstant, CypherQuery, Expression};

fn parse(query: &str) -> CypherQuery {
    parse_cypher(query).unwrap_or_else(|e| panic!("{query}: {e}"))
}

fn parse_err(query: &str) -> String {
    match parse_cypher(query) {
        Ok(q) => panic!("{query} parsed: {:?}", q.clauses),
        Err(e) => e.to_string(),
    }
}

/// Both EXPLAIN / PROFILE orders give the same statement.
#[test]
fn prefix_is_accepted_before_or_after_explain_and_profile() {
    let body = "MATCH (n:Well) RETURN n.id";
    for (flag, explain, profile) in [("EXPLAIN", true, false), ("PROFILE", false, true)] {
        let before = parse(&format!(
            "{flag} FOR VALID_TIME AS OF date('2020-01-01') {body}"
        ));
        let after = parse(&format!(
            "FOR VALID_TIME AS OF date('2020-01-01') {flag} {body}"
        ));
        for query in [&before, &after] {
            assert_eq!((query.explain, query.profile), (explain, profile));
            let context = query.context.as_ref().expect("context");
            assert_eq!(context.axis, "VALID_TIME");
        }
        assert_eq!(
            format!("{:?}", before.clauses),
            format!("{:?}", after.clauses)
        );
        assert_eq!(
            format!("{:?}", before.context.as_ref().unwrap().instant),
            format!("{:?}", after.context.as_ref().unwrap().instant)
        );
    }
}

#[test]
fn prefix_admits_each_constant_instant_form() {
    for instant in [
        "date('2020-01-01')",
        "datetime('2020-01-01T10:00:00')",
        "date($t)",
        "datetime($t)",
        "$t",
        "'2020-01-01'",
        "date()",
        "DATE('2020-01-01')",
    ] {
        let query = parse(&format!(
            "FOR valid_time as of {instant} MATCH (n) RETURN n"
        ));
        let context = query.context.expect("context");
        assert_eq!(context.axis, "valid_time", "the axis is kept as written");
        assert!(context.refusal.is_none());
        assert_eq!(query.clauses.len(), 2, "{instant}");
    }
    let query = parse("FOR VALID_TIME AS OF $t RETURN 1 AS x");
    assert!(matches!(
        query.context.unwrap().instant,
        ContextInstant::AsOf(Expression::Parameter(ref p)) if p == "t"
    ));
    let query = parse("FOR VALID_TIME AS OF '2020-01-01' RETURN 1 AS x");
    assert!(matches!(
        query.context.unwrap().instant,
        ContextInstant::AsOf(Expression::Literal(Value::String(ref s))) if s == "2020-01-01"
    ));
}

#[test]
fn prefix_refuses_a_non_constant_instant() {
    for instant in [
        "n.x",
        "date() + duration('P1D')",
        "datetime()",
        "date('2020-01-01', 'x')",
        "42",
        "toString($t)",
        "date(n.x)",
    ] {
        let err = parse_err(&format!(
            "FOR VALID_TIME AS OF {instant} MATCH (n) RETURN n"
        ));
        assert!(err.contains("takes a constant instant"), "{instant}: {err}");
    }
    // The caret points at the instant.
    let err = parse_err("FOR VALID_TIME AS OF n.x RETURN 1");
    assert!(err.contains("col 22"), "{err}");
}

#[test]
fn any_axis_parses_so_lowering_can_refuse_it() {
    let query = parse("FOR SYSTEM_TIME AS OF date('2020-01-01') RETURN 1 AS x");
    assert_eq!(query.context.unwrap().axis, "SYSTEM_TIME");
}

#[test]
fn malformed_prefixes_name_what_they_expected() {
    let err = parse_err("FOR VALID_TIME OF date('2020-01-01') RETURN 1");
    assert!(
        err.contains("Expected AS OF or ALL after FOR VALID_TIME"),
        "{err}"
    );
    let err = parse_err("FOR VALID_TIME AS date('2020-01-01') RETURN 1");
    assert!(err.contains("Expected OF"), "{err}");
    let err = parse_err("FOR MATCH AS OF date('2020-01-01') RETURN 1");
    assert!(err.contains("Expected a time axis"), "{err}");
}

#[test]
fn a_statement_takes_one_context_and_one_explain() {
    for query in [
        "FOR VALID_TIME AS OF $a FOR VALID_TIME AS OF $b RETURN 1",
        "FOR VALID_TIME AS OF $a EXPLAIN FOR VALID_TIME AS OF $b RETURN 1",
    ] {
        let err = parse_err(query);
        assert!(err.contains("takes one FOR <axis> AS OF context"), "{err}");
    }
    let err = parse_err("EXPLAIN FOR VALID_TIME AS OF $a PROFILE RETURN 1");
    assert!(err.contains("takes one EXPLAIN or PROFILE"), "{err}");
    let err = parse_err("EXPLAIN EXPLAIN RETURN 1");
    assert!(err.contains("takes one EXPLAIN or PROFILE"), "{err}");
}

/// Red before the prefix/body split: a top-level UNION right arm was parsed
/// as a whole statement, so `EXPLAIN` there was accepted and set on the arm.
#[test]
fn union_right_arm_refuses_statement_prefixes() {
    for arm in ["EXPLAIN RETURN 2 AS x", "PROFILE RETURN 2 AS x"] {
        for op in ["UNION", "UNION ALL"] {
            let err = parse_err(&format!("RETURN 1 AS x {op} {arm}"));
            assert!(err.contains("must lead the statement"), "{op} {arm}: {err}");
        }
    }
    let err = parse_err("RETURN 1 AS x UNION FOR VALID_TIME AS OF $t RETURN 2 AS x");
    assert!(err.contains("one context per statement"), "{err}");
    // A prefix on the whole statement is fine.
    let query = parse("FOR VALID_TIME AS OF $t RETURN 1 AS x UNION RETURN 2 AS x");
    assert!(query.context.is_some());
}

#[test]
fn call_subquery_body_refuses_a_context() {
    let err =
        parse_err("MATCH (n) CALL { FOR VALID_TIME AS OF $t MATCH (m) RETURN m } RETURN n, m");
    assert!(err.contains("one context per statement"), "{err}");
    let err = parse_err("MATCH (n) CALL { EXPLAIN MATCH (m) RETURN m } RETURN n, m");
    assert!(err.contains("must lead the statement"), "{err}");
}

#[test]
fn exists_and_count_bodies_reject_a_context_as_a_pattern_error() {
    for query in [
        "MATCH (n) WHERE EXISTS { FOR VALID_TIME AS OF $t (n)-->() } RETURN n",
        "MATCH (n) RETURN COUNT { FOR VALID_TIME AS OF $t (n)-->() } AS c",
    ] {
        parse_err(query);
    }
}

#[test]
fn for_after_a_clause_names_the_prefix_rule() {
    let err = parse_err("UNWIND [1] AS x FOR VALID_TIME AS OF $t RETURN x");
    assert!(err.contains("statement prefix"), "{err}");
}

#[test]
fn a_statement_without_a_prefix_has_no_context() {
    let query = parse("MATCH (n) RETURN n");
    assert!(query.context.is_none() && query.guard.is_none());
}

#[test]
fn all_is_a_context_in_either_prefix_order_and_takes_no_instant() {
    use crate::graph::languages::cypher::ast::ContextOrigin;
    for text in [
        "FOR VALID_TIME ALL MATCH (n) RETURN n",
        "for valid_time all MATCH (n) RETURN n",
        "EXPLAIN FOR VALID_TIME ALL MATCH (n) RETURN n",
        "FOR VALID_TIME ALL EXPLAIN MATCH (n) RETURN n",
    ] {
        let context = parse(text).context.expect(text);
        assert!(matches!(context.instant, ContextInstant::All), "{text}");
        assert_eq!(context.origin, ContextOrigin::Explicit);
    }
    let err = parse_err("FOR VALID_TIME ALL FOR VALID_TIME AS OF date() RETURN 1");
    assert!(err.contains("one FOR"), "{err}");
    let err = parse_err("FOR VALID_TIME ALL date('2020-01-01') RETURN 1");
    assert!(!err.is_empty());
}
