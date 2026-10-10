//! Absolute goldens for the openCypher scalar gaps: radix integer literals,
//! the `^` operator, `tail`, `toBoolean`, `toInteger` on decimal strings and
//! list ordering.
//!
//! These are parser/executor semantics shared by the optimised and
//! unoptimised paths, so the differential corpus cannot see them; each
//! expectation is the value the openCypher specification gives. Every query
//! was a syntax error, an unknown function or `null` before.

use super::*;

fn cell(query: &str) -> Value {
    let graph = DirGraph::new();
    let parsed = parser::parse_cypher(query)
        .unwrap_or_else(|e| panic!("query failed to parse: {query}\n  error: {e}"));
    let no_params = HashMap::new();
    let result = CypherExecutor::with_params(&graph, &no_params, None)
        .execute(&parsed)
        .unwrap_or_else(|e| panic!("query failed: {query}\n  error: {e}"));
    assert_eq!(result.rows.len(), 1, "expected one row from: {query}");
    result.rows[0][0].clone()
}

fn int(n: i64) -> Value {
    Value::Int64(n)
}

fn ints(items: &[i64]) -> Value {
    Value::List(items.iter().map(|n| int(*n)).collect())
}

#[test]
fn hex_and_octal_literals() {
    for (query, expected) in [
        ("RETURN 0x1F", 31),
        ("RETURN 0X1f", 31),
        ("RETURN 0o17", 15),
        ("RETURN 0O17", 15),
        ("RETURN 0x0", 0),
        ("RETURN -0x0", 0),
        ("RETURN -0x162CD4F6", -372036854),
        ("RETURN 0x7FFFFFFFFFFFFFFF", i64::MAX),
        ("RETURN -0x8000000000000000", i64::MIN),
        ("RETURN 0o777777777777777777777", i64::MAX),
        ("RETURN -0o1000000000000000000000", i64::MIN),
        ("RETURN 0x10 + 1", 17),
    ] {
        assert_eq!(cell(query), int(expected), "{query}");
    }
    assert_eq!(cell("RETURN [-0x1, 0o77, 0xA4C]"), ints(&[-1, 63, 2636]));
}

#[test]
fn radix_literals_that_overflow_or_are_malformed_are_refused() {
    for query in [
        "RETURN 0x8000000000000000",
        "RETURN 0o1000000000000000000000",
        "RETURN -0x8000000000000001",
        "RETURN 0x1G",
        "RETURN 0o18",
    ] {
        assert!(parser::parse_cypher(query).is_err(), "{query}");
    }
}

#[test]
fn power_operator_precedence_and_float_result() {
    for (query, expected) in [
        ("RETURN 2 ^ 3", 8.0),
        ("RETURN 2 ^ 3 ^ 2", 64.0),
        ("RETURN 2 ^ (3 ^ 2)", 512.0),
        ("RETURN -3 ^ 2", 9.0),
        ("RETURN -(3 ^ 2)", -9.0),
        ("RETURN 4 ^ 3 * 2 ^ 3", 512.0),
        ("RETURN 4 ^ 3 + 2 ^ 3", 72.0),
        ("RETURN 2 ^ 0.5 * 2 ^ 0.5 > 1.99", 1.0),
        ("RETURN 9 ^ 0.5", 3.0),
        ("RETURN 2 ^ -1", 0.5),
    ] {
        let got = cell(query);
        match got {
            Value::Float64(f) => assert_eq!(f, expected, "{query}"),
            Value::Boolean(true) if expected == 1.0 => {}
            other => panic!("{query} gave {other:?}"),
        }
    }
    assert_eq!(cell("RETURN 2 ^ null"), Value::Null);
    assert_eq!(cell("RETURN null ^ 2"), Value::Null);
}

#[test]
fn tail_drops_the_first_element() {
    assert_eq!(cell("RETURN tail([1, 2, 3])"), ints(&[2, 3]));
    assert_eq!(cell("RETURN tail([1])"), ints(&[]));
    assert_eq!(cell("RETURN tail([])"), ints(&[]));
    assert_eq!(cell("RETURN tail(null)"), Value::Null);
    assert!(cell("RETURN tail([[1], [2]])") == Value::List(vec![ints(&[2])]));
}

#[test]
fn to_boolean_converts_booleans_and_boolean_text() {
    let t = Value::Boolean(true);
    let f = Value::Boolean(false);
    assert_eq!(cell("RETURN toBoolean(true)"), t);
    assert_eq!(cell("RETURN toBoolean(false)"), f);
    assert_eq!(cell("RETURN toBoolean('true')"), t);
    assert_eq!(cell("RETURN toBoolean('TrUe')"), t);
    assert_eq!(cell("RETURN toBoolean('FALSE')"), f);
    for invalid in ["''", "' tru '", "'f alse'", "'yes'", "null"] {
        assert_eq!(
            cell(&format!("RETURN toBoolean({invalid})")),
            Value::Null,
            "{invalid}"
        );
    }
    let graph = DirGraph::new();
    let parsed = parser::parse_cypher("RETURN toBoolean(1)").unwrap();
    let no_params = HashMap::new();
    assert!(CypherExecutor::with_params(&graph, &no_params, None)
        .execute(&parsed)
        .is_err());
}

#[test]
fn to_integer_parses_decimal_strings_and_truncates() {
    assert_eq!(cell("RETURN toInteger('1.7')"), int(1));
    assert_eq!(cell("RETURN toInteger('-1.7')"), int(-1));
    assert_eq!(cell("RETURN toInteger('3')"), int(3));
    assert_eq!(cell("RETURN toInteger('2.9')"), int(2));
    assert_eq!(cell("RETURN toInteger('1e3')"), int(1000));
    assert_eq!(
        cell("RETURN toInteger('9223372036854775807')"),
        int(i64::MAX)
    );
    for text in ["'abc'", "''", "' 3 '", "'1.7x'", "'NaN'", "'inf'", "'1e30'"] {
        assert_eq!(
            cell(&format!("RETURN toInteger({text})")),
            Value::Null,
            "{text}"
        );
    }
    assert_eq!(
        cell("RETURN [x IN [2, 2.9, '1.7'] | toInteger(x)]"),
        ints(&[2, 2, 1])
    );
}

#[test]
fn list_ordering_comparisons_are_lexicographic_and_three_valued() {
    let t = Value::Boolean(true);
    let f = Value::Boolean(false);
    for (query, expected) in [
        ("RETURN [1, 2] < [1, 3]", &t),
        ("RETURN [1, 2] < [1, 2]", &f),
        ("RETURN [1, 2] <= [1, 2]", &t),
        ("RETURN [1, 2] > [1]", &t),
        ("RETURN [1, 0] >= [1]", &t),
        ("RETURN [1] < [1, 0]", &t),
        ("RETURN [] < [0]", &t),
        ("RETURN [2] > [1, 9]", &t),
        ("RETURN [1, null] >= [1]", &t),
        ("RETURN [1, 2] >= [3, null]", &f),
        ("RETURN [1, 2] < [3, null]", &t),
        ("RETURN [[1], [2]] < [[1], [3]]", &t),
        ("RETURN [1.5] > [1]", &t),
    ] {
        assert_eq!(&cell(query), expected, "{query}");
    }
    for query in [
        "RETURN [1, 2] >= [1, null]",
        "RETURN [1, 'a'] >= [1, null]",
        "RETURN [1] < ['a']",
        "RETURN [null] < [1]",
        "RETURN [1] < 1",
    ] {
        assert_eq!(cell(query), Value::Null, "{query}");
    }
}

#[test]
fn order_by_places_lists_lexicographically() {
    let graph = DirGraph::new();
    let parsed =
        parser::parse_cypher("UNWIND [[2], [1, 2], [1], [], [1, 1]] AS l RETURN l ORDER BY l")
            .unwrap();
    let no_params = HashMap::new();
    let result = CypherExecutor::with_params(&graph, &no_params, None)
        .execute(&parsed)
        .unwrap();
    let got: Vec<Value> = result.rows.iter().map(|r| r[0].clone()).collect();
    assert_eq!(
        got,
        vec![
            ints(&[]),
            ints(&[1]),
            ints(&[1, 1]),
            ints(&[1, 2]),
            ints(&[2])
        ]
    );
}
