//! `x IS [NOT] :: <type>` (also `IS [NOT] TYPED`) and `valueType(x)`.
//!
//! Absolute golden values: the differential corpus cannot see a defect the
//! optimised and unoptimised paths share, and a type predicate that is wrong
//! is wrong in both.

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn run(graph: &mut DirGraph, query: &str) -> Result<Vec<Vec<Value>>, String> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    if query.contains("CREATE") {
        execute_mut(graph, query, &opts)
            .map(|o| o.result.rows)
            .map_err(|e| e.to_string())
    } else {
        execute_read(graph, query, &opts)
            .map(|o| o.result.rows)
            .map_err(|e| e.to_string())
    }
}

fn scalar(graph: &mut DirGraph, query: &str) -> Value {
    let rows = run(graph, query).unwrap_or_else(|e| panic!("{query}: {e}"));
    assert_eq!(rows.len(), 1, "{query}");
    rows[0][0].clone()
}

fn is_type(graph: &mut DirGraph, expr: &str, ty: &str) -> bool {
    match scalar(graph, &format!("RETURN {expr} IS :: {ty}")) {
        Value::Boolean(b) => b,
        other => panic!("{expr} IS :: {ty} returned {other:?}"),
    }
}

fn value_type(graph: &mut DirGraph, expr: &str) -> String {
    match scalar(graph, &format!("RETURN valueType({expr})")) {
        Value::String(s) => s,
        other => panic!("valueType({expr}) returned {other:?}"),
    }
}

/// One value per type family against every scalar type name: each value is of
/// exactly the types in its row, and of no other.
#[test]
fn each_value_family_matches_exactly_its_own_type() {
    let mut g = DirGraph::new();
    let types = [
        "BOOLEAN",
        "STRING",
        "INTEGER",
        "FLOAT",
        "DATE",
        "LOCAL TIME",
        "ZONED TIME",
        "LOCAL DATETIME",
        "ZONED DATETIME",
        "DURATION",
        "POINT",
        "MAP",
        "LIST<ANY>",
    ];
    let cases: [(&str, &str); 13] = [
        ("true", "BOOLEAN"),
        ("'a'", "STRING"),
        ("1", "INTEGER"),
        ("1.5", "FLOAT"),
        ("date('2026-01-02')", "DATE"),
        ("localtime('10:00:00')", "LOCAL TIME"),
        ("time('10:00:00+01:00')", "ZONED TIME"),
        ("localdatetime('2026-01-02T10:00:00')", "LOCAL DATETIME"),
        ("datetime('2026-01-02T10:00:00+01:00')", "ZONED DATETIME"),
        ("duration('P1D')", "DURATION"),
        ("point({latitude: 1.0, longitude: 2.0})", "POINT"),
        ("{a: 1}", "MAP"),
        ("[1, 2]", "LIST<ANY>"),
    ];
    for (expr, own) in cases {
        for ty in types {
            assert_eq!(
                is_type(&mut g, expr, ty),
                ty == own,
                "{expr} IS :: {ty} (value family {own})"
            );
        }
    }
}

/// The GraphBridge Min/Max integer guard, verbatim.
#[test]
fn the_graphbridge_integer_guard_statements() {
    let mut g = DirGraph::new();
    run(
        &mut g,
        "CREATE (:T {p: 1}), (:T {p: 3.0}), (:T {p: 'x'}), (:T {p: 2147483648}), (:T {q: 1})",
    )
    .unwrap();
    let count = |g: &mut DirGraph, valid: &str| {
        let q = format!(
            "MATCH (n:T) WITH n.p AS p RETURN count(CASE WHEN {valid} THEN null ELSE 1 END) AS bad"
        );
        scalar(g, &q)
    };
    // 3.0 is FLOAT, 'x' is STRING, and the absent property is null: three
    // rows fail `IS :: INTEGER NOT NULL`.
    assert_eq!(count(&mut g, "p IS :: INTEGER NOT NULL"), Value::Int64(3));
    // The 32-bit bounds drop 2147483648 as well (comparison with a string is
    // null, so it is counted by the CASE's ELSE branch).
    assert_eq!(
        count(
            &mut g,
            "p IS :: INTEGER NOT NULL AND p >= -2147483648 AND p <= 2147483647"
        ),
        Value::Int64(4)
    );
    // Nullable columns: a null passes.
    assert_eq!(
        count(
            &mut g,
            "p IS NULL OR (p IS :: INTEGER NOT NULL AND p >= -2147483648 AND p <= 2147483647)"
        ),
        Value::Int64(3)
    );
}

#[test]
fn null_is_of_every_nullable_type_and_of_no_not_null_type() {
    let mut g = DirGraph::new();
    for ty in [
        "INTEGER",
        "STRING",
        "LIST<INTEGER>",
        "NODE",
        "MAP",
        "NULL",
        "NOTHING",
        "ANY",
    ] {
        assert!(is_type(&mut g, "null", ty), "null IS :: {ty}");
        assert!(
            !is_type(&mut g, "null", &format!("{ty} NOT NULL")),
            "null IS :: {ty} NOT NULL"
        );
    }
    assert!(is_type(&mut g, "1", "INTEGER NOT NULL"));
    assert!(is_type(&mut g, "1", "ANY NOT NULL"));
    assert!(!is_type(&mut g, "1", "NULL"));
    assert!(!is_type(&mut g, "1", "NOTHING"));
}

#[test]
fn not_form_typed_spelling_unions_and_synonyms() {
    let mut g = DirGraph::new();
    assert_eq!(
        scalar(&mut g, "RETURN 1 IS NOT :: STRING"),
        Value::Boolean(true)
    );
    assert_eq!(
        scalar(&mut g, "RETURN 1 IS NOT :: INTEGER"),
        Value::Boolean(false)
    );
    assert_eq!(
        scalar(&mut g, "RETURN 'a' IS TYPED STRING"),
        Value::Boolean(true)
    );
    assert_eq!(
        scalar(&mut g, "RETURN 'a' IS NOT TYPED STRING"),
        Value::Boolean(false)
    );
    assert!(is_type(&mut g, "1.5", "INTEGER | FLOAT"));
    assert!(is_type(&mut g, "1", "STRING | INTEGER NOT NULL"));
    assert!(!is_type(&mut g, "true", "INTEGER | FLOAT"));
    assert!(is_type(&mut g, "1", "int"));
    assert!(is_type(&mut g, "1", "signed integer"));
    assert!(is_type(&mut g, "1", "any value"));
    assert!(is_type(
        &mut g,
        "localdatetime('2026-01-02T10:00:00')",
        "timestamp without time zone"
    ));
    // Integer and float stay distinct.
    assert!(!is_type(&mut g, "3.0", "INTEGER"));
    assert!(!is_type(&mut g, "3", "FLOAT"));
}

#[test]
fn list_types_check_every_element() {
    let mut g = DirGraph::new();
    assert!(is_type(&mut g, "[1, 2, 3]", "LIST<INTEGER>"));
    assert!(is_type(&mut g, "[]", "LIST<INTEGER NOT NULL>"));
    assert!(!is_type(&mut g, "[1, 'a']", "LIST<INTEGER>"));
    assert!(is_type(&mut g, "[1, 'a']", "LIST<INTEGER | STRING>"));
    assert!(is_type(&mut g, "[1, null]", "LIST<INTEGER>"));
    assert!(!is_type(&mut g, "[1, null]", "LIST<INTEGER NOT NULL>"));
    assert!(is_type(&mut g, "[[1], [2]]", "LIST<LIST<INTEGER>>"));
    assert!(!is_type(&mut g, "[[1], ['x']]", "LIST<LIST<INTEGER>>"));
    assert!(!is_type(&mut g, "1", "LIST<INTEGER>"));
    // GraphBridge's diagnostic: every element is INTEGER or FLOAT.
    assert_eq!(
        scalar(
            &mut g,
            "RETURN none(x IN [1, 2.5] WHERE NOT (x IS :: INTEGER OR x IS :: FLOAT))"
        ),
        Value::Boolean(true)
    );
}

#[test]
fn graph_entities_and_paths() {
    let mut g = DirGraph::new();
    run(&mut g, "CREATE (:A {id: 1})-[:R]->(:B {id: 2})").unwrap();
    let q = |g: &mut DirGraph, text: &str| scalar(g, text);
    assert_eq!(
        q(&mut g, "MATCH (a:A)-[r:R]->(b:B) RETURN a IS :: NODE AND r IS :: RELATIONSHIP AND NOT (a IS :: RELATIONSHIP) AND NOT (r IS :: NODE)"),
        Value::Boolean(true)
    );
    assert_eq!(
        q(&mut g, "MATCH p = (:A)-[:R]->(:B) RETURN p IS :: PATH"),
        Value::Boolean(true)
    );
    assert_eq!(
        q(&mut g, "MATCH (a:A) RETURN a IS :: MAP"),
        Value::Boolean(false)
    );
    assert_eq!(
        q(&mut g, "MATCH (a:A) RETURN a.id IS :: INTEGER"),
        Value::Boolean(true)
    );
}

#[test]
fn works_as_a_where_predicate_and_a_column_name() {
    let mut g = DirGraph::new();
    run(&mut g, "CREATE (:T {id: 1, p: 1}), (:T {id: 2, p: 'x'})").unwrap();
    let rows = run(&mut g, "MATCH (n:T) WHERE n.p IS :: INTEGER RETURN n.id").unwrap();
    assert_eq!(rows, vec![vec![Value::Int64(1)]]);
    let rows = run(
        &mut g,
        "MATCH (n:T) WHERE n.p IS NOT :: INTEGER RETURN n.id",
    )
    .unwrap();
    assert_eq!(rows, vec![vec![Value::Int64(2)]]);
    let params = HashMap::new();
    let out = execute_read(
        &g,
        "MATCH (n:T) RETURN n.p IS NOT :: INTEGER NOT NULL ORDER BY n.id",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    assert_eq!(out.result.columns, vec!["n.p IS NOT :: INTEGER NOT NULL"]);
}

#[test]
fn unknown_and_malformed_types_are_rejected_with_the_type_list() {
    let mut g = DirGraph::new();
    for bad in [
        "RETURN 1 IS :: BLOB",
        "RETURN 1 IS :: ",
        "RETURN 1 IS :: LIST<INTEGER",
        "RETURN 1 IS :: INTEGER |",
    ] {
        let err = run(&mut g, bad).expect_err(bad);
        assert!(
            err.contains("type") || err.contains("expected"),
            "{bad}: error should name the problem, got {err}"
        );
    }
    let err = run(&mut g, "RETURN 1 IS :: BLOB").unwrap_err();
    assert!(err.contains("INTEGER"), "lists the accepted types: {err}");
}

#[test]
fn value_type_names_match_neo4j() {
    let mut g = DirGraph::new();
    let cases = [
        ("1", "INTEGER NOT NULL"),
        ("1.5", "FLOAT NOT NULL"),
        ("'a'", "STRING NOT NULL"),
        ("true", "BOOLEAN NOT NULL"),
        ("null", "NULL"),
        ("date('2026-01-02')", "DATE NOT NULL"),
        ("localtime('10:00:00')", "LOCAL TIME NOT NULL"),
        ("time('10:00:00+01:00')", "ZONED TIME NOT NULL"),
        (
            "localdatetime('2026-01-02T10:00:00')",
            "LOCAL DATETIME NOT NULL",
        ),
        (
            "datetime('2026-01-02T10:00:00+01:00')",
            "ZONED DATETIME NOT NULL",
        ),
        ("duration('P1D')", "DURATION NOT NULL"),
        ("point({latitude: 1.0, longitude: 2.0})", "POINT NOT NULL"),
        ("{a: 1}", "MAP NOT NULL"),
        ("[]", "LIST<NOTHING> NOT NULL"),
        ("[1, 2]", "LIST<INTEGER NOT NULL> NOT NULL"),
        ("[1, null]", "LIST<INTEGER> NOT NULL"),
        ("[null]", "LIST<NULL> NOT NULL"),
        (
            "[1, 'a']",
            "LIST<INTEGER NOT NULL | STRING NOT NULL> NOT NULL",
        ),
        (
            "[[1], []]",
            "LIST<LIST<INTEGER NOT NULL> NOT NULL | LIST<NOTHING> NOT NULL> NOT NULL",
        ),
    ];
    for (expr, want) in cases {
        assert_eq!(value_type(&mut g, expr), want, "valueType({expr})");
    }
    run(&mut g, "CREATE (:A {id: 1})-[:R]->(:B {id: 2})").unwrap();
    assert_eq!(
        scalar(
            &mut g,
            "MATCH (a:A)-[r]->() RETURN valueType(a) + ' ' + valueType(r)"
        ),
        Value::String("NODE NOT NULL RELATIONSHIP NOT NULL".into())
    );
    // The predicate and the function agree: a value is of its own valueType.
    for (expr, _) in cases {
        let own = value_type(&mut g, expr);
        assert!(
            is_type(&mut g, expr, &own),
            "{expr} IS :: valueType({expr})"
        );
    }
}

#[test]
fn constraint_and_expression_agree_on_scalar_names() {
    use crate::graph::property_types::DeclaredType;
    use crate::graph::value_type::TypeExpr;
    for name in DeclaredType::accepted_names() {
        let declared = DeclaredType::resolve(name).unwrap();
        let expr = TypeExpr::parse(name).unwrap();
        assert_eq!(expr.to_string(), declared.name());
    }
}

/// The parsed type is shared across rows and statements: many rows of mixed
/// types, two different specs in one statement and a repeated statement must
/// each see their own spec's answer.
#[test]
fn cached_type_specs_stay_distinct_across_many_mixed_rows() {
    let mut g = DirGraph::new();
    let row = |g: &mut DirGraph, q: &str| match scalar(g, q) {
        Value::Int64(n) => n,
        other => panic!("{q}: {other:?}"),
    };
    // 300 values cycling int / float / string / null.
    let list = "[x IN range(0, 299) | CASE x % 4 WHEN 0 THEN x WHEN 1 THEN x + 0.5 WHEN 2 THEN toString(x) ELSE null END]";
    let count =
        |ty: &str| format!("UNWIND {list} AS v RETURN count(CASE WHEN v IS :: {ty} THEN 1 END)");
    for _ in 0..2 {
        assert_eq!(row(&mut g, &count("INTEGER NOT NULL")), 75);
        assert_eq!(row(&mut g, &count("FLOAT")), 75 + 75);
        assert_eq!(row(&mut g, &count("STRING | INTEGER")), 75 + 75 + 75);
        assert_eq!(row(&mut g, &count("INTEGER")), 75 + 75);
    }
    let both = format!(
        "UNWIND {list} AS v RETURN count(CASE WHEN v IS :: INTEGER AND v IS NOT :: FLOAT THEN 1 END)"
    );
    assert_eq!(row(&mut g, &both), 75);
}

/// A failed parse is never cached: the same invalid spec errors the same way
/// after a valid spec of similar text has been cached.
#[test]
fn invalid_type_error_is_stable_after_valid_specs_are_cached() {
    let mut g = DirGraph::new();
    assert!(is_type(&mut g, "1", "INTEGER"));
    let first = run(&mut g, "RETURN 1 IS :: BLOB").unwrap_err();
    assert!(is_type(&mut g, "1", "INTEGER"));
    let second = run(&mut g, "RETURN 1 IS :: BLOB").unwrap_err();
    assert_eq!(first, second);
    assert!(first.contains("INTEGER"), "{first}");
}
