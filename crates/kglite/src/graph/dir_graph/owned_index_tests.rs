//! The index a UNIQUE / NODE KEY constraint owns: enforcement, ownership
//! listing, the DDL rules around it, rollback, delete, and what a save carries.
//!
//! Each group answers one question a reader could get wrong. The value-family
//! table pins that the owned index admits and refuses exactly the pairs the
//! retired single-occupant map did, by comparing against `Value`'s own equality
//! (the relation `CompositeValue` derives) rather than against hand-listed
//! expectations.

use std::collections::HashMap;
use std::sync::Arc;

use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::io::file::{load_file_with, prepare_save, write_kgl, LoadOptions};
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use crate::graph::session::Session;

fn try_run(graph: &mut DirGraph, query: &str) -> Result<(), String> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(graph, query, &opts)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn run(graph: &mut DirGraph, query: &str) {
    try_run(graph, query).unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_read(graph, query, &opts)
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
}

fn count(graph: &DirGraph, query: &str) -> i64 {
    match rows(graph, query)[0][0] {
        Value::Int64(n) => n,
        ref other => panic!("not a count: {other:?}"),
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => panic!("not a string: {other:?}"),
    }
}

// ── Enforcement: the same "same value" relation as the retired map ──

/// `(family, first, second)` as Cypher expressions. Whether the second may
/// join the first is decided by `Value` equality, not by this table.
const FAMILIES: &[(&str, &str, &str)] = &[
    ("int", "1", "1"),
    ("int/other", "1", "2"),
    ("int vs float", "1", "1.0"),
    ("float", "1.5", "1.5"),
    ("float zero signs", "0.0", "-0.0"),
    ("float nan", "toFloat('NaN')", "toFloat('NaN')"),
    ("string", "'a'", "'a'"),
    ("string case", "'a'", "'A'"),
    ("bool", "true", "true"),
    ("bool/other", "true", "false"),
    ("list", "[1, 2]", "[1, 2]"),
    ("list order", "[1, 2]", "[2, 1]"),
    ("list int vs float", "[1]", "[1.0]"),
    ("date", "date('2020-01-01')", "date('2020-01-01')"),
    ("date/other", "date('2020-01-01')", "date('2020-01-02')"),
    (
        "datetime",
        "datetime('2020-01-01T10:00:00')",
        "datetime('2020-01-01T10:00:00')",
    ),
    (
        "zoned same instant",
        "datetime('2020-01-01T10:00:00+01:00')",
        "datetime('2020-01-01T09:00:00Z')",
    ),
    (
        "zoned other instant",
        "datetime('2020-01-01T10:00:00+01:00')",
        "datetime('2020-01-01T10:00:00Z')",
    ),
    ("duration", "duration({days: 1})", "duration({days: 1})"),
    (
        "duration units",
        "duration({days: 1})",
        "duration({hours: 24})",
    ),
    (
        "point",
        "point({latitude: 1.0, longitude: 2.0})",
        "point({latitude: 1.0, longitude: 2.0})",
    ),
    (
        "point/other",
        "point({latitude: 1.0, longitude: 2.0})",
        "point({latitude: 1.0, longitude: 3.0})",
    ),
];

fn evaluate(expression: &str) -> Value {
    rows(&DirGraph::new(), &format!("RETURN {expression} AS v"))[0][0].clone()
}

#[test]
fn the_owned_index_admits_and_refuses_exactly_what_value_equality_says() {
    let mut refused = 0;
    let mut admitted = 0;
    for (family, first, second) in FAMILIES {
        let mut graph = DirGraph::new();
        run(
            &mut graph,
            "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
        );
        run(&mut graph, &format!("CREATE (:T {{k: {first}}})"));
        let collides = evaluate(first) == evaluate(second);
        let result = try_run(&mut graph, &format!("CREATE (:T {{k: {second}}})"));
        match (collides, &result) {
            (true, Err(error)) => {
                assert!(error.contains("UNIQUE"), "{family}: {error}");
                refused += 1;
            }
            (false, Ok(())) => admitted += 1,
            (collides, result) => {
                panic!("{family}: values equal = {collides} but the write returned {result:?}")
            }
        }
        // The same relation holds through SET, and a refused SET leaves the
        // node's own claim in place.
        let mut graph = DirGraph::new();
        run(
            &mut graph,
            "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
        );
        run(&mut graph, &format!("CREATE (:T {{id: 1, k: {first}}})"));
        run(&mut graph, "CREATE (:T {id: 2, k: 'free'})");
        let result = try_run(
            &mut graph,
            &format!("MATCH (n:T {{id: 2}}) SET n.k = {second}"),
        );
        assert_eq!(result.is_err(), collides, "{family} via SET: {result:?}");
    }
    assert!(
        refused >= 10 && admitted >= 8,
        "the table must exercise both outcomes: refused {refused}, admitted {admitted}"
    );
}

#[test]
fn integer_and_float_tuples_stay_two_claims() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(&mut graph, "CREATE (:T {k: 1}), (:T {k: 1.0})");
    assert_eq!(count(&graph, "MATCH (n:T) RETURN count(n)"), 2);
    assert!(try_run(&mut graph, "CREATE (:T {k: 1})").is_err());
    assert!(try_run(&mut graph, "CREATE (:T {k: 1.0})").is_err());
}

#[test]
fn a_composite_constraint_claims_the_whole_tuple_in_either_declaration_order() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE (n.b, n.a) IS UNIQUE",
    );
    run(&mut graph, "CREATE (:T {a: 1, b: 2}), (:T {a: 2, b: 1})");
    let duplicate = try_run(&mut graph, "CREATE (:T {b: 2, a: 1})").unwrap_err();
    assert!(duplicate.contains("UNIQUE"), "{duplicate}");
    // Half a tuple is exempt, as before.
    run(&mut graph, "CREATE (:T {a: 1}), (:T {a: 1}), (:T {b: 2})");
    // The declared order is what the violation reports.
    assert!(
        duplicate.contains("b") && duplicate.contains("a"),
        "{duplicate}"
    );
}

#[test]
fn null_and_absent_values_claim_nothing() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE (:T {id: 1}), (:T {id: 2}), (:T {id: 3, k: null})",
    );
    assert_eq!(count(&graph, "MATCH (n:T) RETURN count(n)"), 3);
}

#[test]
fn every_write_route_enforces() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE (:T {id: 1, k: 'a'}), (:T {id: 2, k: 'b'})",
    );
    // CREATE
    assert!(try_run(&mut graph, "CREATE (:T {id: 3, k: 'a'})").is_err());
    // A repeat inside one statement.
    assert!(try_run(
        &mut graph,
        "UNWIND [1, 2] AS i CREATE (:T {id: 10 + i, k: 'c'})"
    )
    .is_err());
    assert_eq!(count(&graph, "MATCH (n:T) RETURN count(n)"), 2);
    // SET
    assert!(try_run(&mut graph, "MATCH (n:T {id: 2}) SET n.k = 'a'").is_err());
    assert!(try_run(&mut graph, "MATCH (n:T {id: 2}) SET n += {k: 'a'}").is_err());
    // A SET to its own value is not a conflict with itself.
    run(&mut graph, "MATCH (n:T {id: 1}) SET n.k = 'a'");
    // MERGE hits instead of duplicating, and its ON CREATE cannot duplicate.
    run(&mut graph, "MERGE (n:T {k: 'a'}) ON MATCH SET n.hit = true");
    assert_eq!(count(&graph, "MATCH (n:T {hit: true}) RETURN count(n)"), 1);
    assert_eq!(count(&graph, "MATCH (n:T) RETURN count(n)"), 2);
    // REMOVE frees the claim.
    run(&mut graph, "MATCH (n:T {id: 1}) REMOVE n.k");
    run(&mut graph, "CREATE (:T {id: 4, k: 'a'})");
    assert!(try_run(&mut graph, "CREATE (:T {id: 5, k: 'a'})").is_err());
}

// ── Rollback, delete, transactions ──

#[test]
fn a_failed_statement_gives_every_claim_back() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE (:T {id: 1, k: 'a'}), (:T {id: 2, k: 'b'})",
    );
    let before = graph.unique_occupancy();

    // A claim taken, then the statement fails: the value must be free again.
    assert!(try_run(
        &mut graph,
        "CREATE (:T {id: 3, k: 'new'}) WITH 1 AS x CREATE (:T {id: 4, k: 'a'})"
    )
    .is_err());
    assert_eq!(graph.unique_occupancy(), before);
    run(&mut graph, "CREATE (:T {id: 5, k: 'new'})");
    run(&mut graph, "MATCH (n:T {id: 5}) DETACH DELETE n");

    // A claim moved, then the statement fails: the old value is held again and
    // the new one is free.
    assert!(try_run(
        &mut graph,
        "MATCH (n:T {id: 1}) SET n.k = 'moved' WITH n CREATE (:T {id: 6, k: 'moved'})"
    )
    .is_err());
    assert_eq!(graph.unique_occupancy(), before);
    assert!(try_run(&mut graph, "CREATE (:T {id: 7, k: 'a'})").is_err());
    run(&mut graph, "CREATE (:T {id: 8, k: 'moved'})");
    run(&mut graph, "MATCH (n:T {id: 8}) DETACH DELETE n");

    // A claim released by a delete, then the statement fails: it is held again.
    assert!(try_run(
        &mut graph,
        "MATCH (n:T {id: 2}) DETACH DELETE n WITH 1 AS x CREATE (:T {id: 9, k: 'b'}) \
         WITH 1 AS y CREATE (:T {id: 10, k: 'b'})"
    )
    .is_err());
    assert_eq!(graph.unique_occupancy(), before);
    assert!(try_run(&mut graph, "CREATE (:T {id: 11, k: 'b'})").is_err());
}

#[test]
fn a_delete_releases_its_claims_in_the_same_statement_and_after() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE (:T {id: 1, k: 'a'}), (:T {id: 2, k: 'b'})",
    );
    run(&mut graph, "MATCH (n:T {id: 1}) DETACH DELETE n");
    run(&mut graph, "CREATE (:T {id: 3, k: 'a'})");
    // Delete then re-create in one statement.
    run(
        &mut graph,
        "MATCH (n:T {id: 2}) DETACH DELETE n CREATE (:T {id: 4, k: 'b'})",
    );
    assert_eq!(count(&graph, "MATCH (n:T {k: 'b'}) RETURN count(n)"), 1);
    assert!(try_run(&mut graph, "CREATE (:T {id: 5, k: 'b'})").is_err());
    // Deleting the whole type leaves the constraint declared and enforcing.
    run(&mut graph, "MATCH (n:T) DETACH DELETE n");
    run(&mut graph, "CREATE (:T {id: 6, k: 'z'})");
    assert!(try_run(&mut graph, "CREATE (:T {id: 7, k: 'z'})").is_err());
}

/// A delete small against its type evicts through the resolved buckets
/// (`mutation::delete_index_eviction`) rather than the sweep; the tiny fixtures
/// above never leave the sweep, so this one is large enough to take the
/// targeted route and still must release, and give back on rollback.
#[test]
fn a_small_delete_from_a_large_type_releases_and_restores_its_claim() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "UNWIND range(1, 300) AS i CREATE (:T {id: i, k: 'k' + toString(i), a: i, b: i})",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE",
    );
    let before = graph.unique_occupancy();
    // Rolled back: both claims are held again.
    assert!(try_run(
        &mut graph,
        "MATCH (n:T {id: 7}) DETACH DELETE n WITH 1 AS x CREATE (:T {id: 1000, k: 'k7', a: 7, b: 7}) \
         WITH 1 AS y CREATE (:T {id: 1001, k: 'k7'})"
    )
    .is_err());
    assert_eq!(graph.unique_occupancy(), before);
    assert!(try_run(&mut graph, "CREATE (:T {id: 1002, k: 'k7'})").is_err());
    assert!(try_run(&mut graph, "CREATE (:T {id: 1003, a: 7, b: 7})").is_err());
    // Committed: both are free.
    run(&mut graph, "MATCH (n:T {id: 7}) DETACH DELETE n");
    run(&mut graph, "CREATE (:T {id: 1004, k: 'k7', a: 7, b: 7})");
    assert_eq!(graph.unique_occupancy().len(), before.len());
}

#[test]
fn a_transaction_that_rolls_back_leaves_the_published_claims_alone() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(&mut graph, "CREATE (:T {id: 1, k: 'a'})");
    let session = Session::new(graph);
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);

    let mut tx = session.begin();
    execute_mut(
        tx.working_mut().unwrap(),
        "CREATE (:T {id: 2, k: 'b'})",
        &opts,
    )
    .unwrap();
    assert!(execute_mut(
        tx.working_mut().unwrap(),
        "CREATE (:T {id: 3, k: 'b'})",
        &opts
    )
    .is_err());
    session.rollback(tx);

    let mut tx = session.begin();
    // The rolled-back claim is free, and the committed one still holds.
    execute_mut(
        tx.working_mut().unwrap(),
        "CREATE (:T {id: 4, k: 'b'})",
        &opts,
    )
    .unwrap();
    assert!(execute_mut(
        tx.working_mut().unwrap(),
        "CREATE (:T {id: 5, k: 'a'})",
        &opts
    )
    .is_err());
    session.commit(tx, true);

    let published = session.snapshot();
    assert_eq!(count(&published, "MATCH (n:T) RETURN count(n)"), 2);
    let mut tx = session.begin();
    assert!(execute_mut(
        tx.working_mut().unwrap(),
        "CREATE (:T {id: 6, k: 'b'})",
        &opts
    )
    .is_err());
}

#[test]
fn a_transaction_fork_shares_the_owned_index_instead_of_copying_it() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "UNWIND range(1, 500) AS i CREATE (:T {id: i, k: 'k' + toString(i)})",
    );
    let session = Session::new(graph);
    let before = crate::graph::dir_graph::index_layer::merged_iters();
    let mut tx = session.begin();
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    execute_mut(
        tx.working_mut().unwrap(),
        "CREATE (:T {id: 900, k: 'fresh'})",
        &opts,
    )
    .unwrap();
    assert!(execute_mut(
        tx.working_mut().unwrap(),
        "CREATE (:T {id: 901, k: 'k7'})",
        &opts
    )
    .is_err());
    // No walk of the whole index happened for either the fork or the checks.
    assert_eq!(crate::graph::dir_graph::index_layer::merged_iters(), before);
}

// ── Ownership: listing, DDL rules, adoption ──

fn index_rows(graph: &DirGraph) -> Vec<(String, String, Option<String>)> {
    rows(
        graph,
        "SHOW INDEXES YIELD name, type, owningConstraint RETURN name, type, owningConstraint \
         ORDER BY name, type",
    )
    .into_iter()
    .map(|row| {
        let owner = match &row[2] {
            Value::Null => None,
            other => Some(text(other)),
        };
        (text(&row[0]), text(&row[1]), owner)
    })
    .collect()
}

fn owned_index_of(graph: &DirGraph, constraint: &str) -> Value {
    rows(
        graph,
        &format!(
            "SHOW CONSTRAINTS YIELD name, ownedIndex WHERE name = '{constraint}' RETURN ownedIndex"
        ),
    )[0][0]
        .clone()
}

#[test]
fn show_lists_the_owned_index_and_names_both_ends() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.b, n.a) IS NODE KEY",
    );
    run(&mut graph, "CREATE INDEX FOR (n:T) ON (n.plain)");
    assert_eq!(
        index_rows(&graph),
        [
            (
                "T.(a,b)".to_string(),
                "PROPERTY".to_string(),
                Some("composite".to_string())
            ),
            (
                "T.k".to_string(),
                "PROPERTY".to_string(),
                Some("single".to_string())
            ),
            ("T.plain".to_string(), "PROPERTY".to_string(), None),
        ]
    );
    assert_eq!(
        owned_index_of(&graph, "single"),
        Value::String("T.k".into())
    );
    assert_eq!(
        owned_index_of(&graph, "composite"),
        Value::String("T.(a,b)".into())
    );
}

#[test]
fn dropping_the_constraint_drops_the_index_it_created() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(&mut graph, "CREATE (:T {k: 'a'})");
    assert!(graph.has_index("T", "k"));
    run(&mut graph, "DROP CONSTRAINT single");
    assert!(!graph.has_index("T", "k"));
    assert!(index_rows(&graph).is_empty());
    // And with it enforcement.
    run(&mut graph, "CREATE (:T {k: 'a'})");
}

#[test]
fn drop_index_refuses_an_owned_index_and_names_the_constraint() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE",
    );
    for statement in [
        "DROP INDEX `T.k`",
        "DROP INDEX `T.k` IF EXISTS",
        "DROP INDEX FOR (n:T) ON (n.k)",
        "DROP INDEX `T.(a,b)`",
        "DROP INDEX FOR (n:T) ON (n.b, n.a)",
    ] {
        let error = try_run(&mut graph, statement).unwrap_err();
        assert!(
            error.contains("backs the constraint"),
            "{statement}: {error}"
        );
    }
    assert!(try_run(&mut graph, "DROP INDEX `T.k`")
        .unwrap_err()
        .contains("'single'"));
    // Nothing was removed, and enforcement still works.
    assert!(graph.has_index("T", "k") && graph.has_composite_index("T", &["a".into(), "b".into()]));
    run(&mut graph, "CREATE (:T {k: 'x', a: 1, b: 1})");
    assert!(try_run(&mut graph, "CREATE (:T {k: 'x'})").is_err());
    assert!(graph.drop_index("T", "k").is_err());
    assert!(graph
        .drop_composite_index("T", &["a".into(), "b".into()])
        .is_err());
}

#[test]
fn create_index_equivalent_to_an_owned_index_is_refused_or_skipped() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE",
    );
    for statement in [
        "CREATE INDEX FOR (n:T) ON (n.k)",
        "CREATE INDEX FOR (n:T) ON (n.b, n.a)",
    ] {
        let error = try_run(&mut graph, statement).unwrap_err();
        assert!(error.contains("already exists"), "{statement}: {error}");
    }
    for statement in [
        "CREATE INDEX IF NOT EXISTS FOR (n:T) ON (n.k)",
        "CREATE INDEX IF NOT EXISTS FOR (n:T) ON (n.a, n.b)",
    ] {
        run(&mut graph, statement);
    }
    // A no-op adopts nothing: the index still goes with its constraint.
    run(&mut graph, "DROP CONSTRAINT single");
    assert!(!graph.has_index("T", "k"));
    assert_eq!(index_rows(&graph).len(), 1);
}

#[test]
fn an_index_created_first_is_adopted_and_survives_the_constraint() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:T {k: 'a', a: 1, b: 2})");
    run(&mut graph, "CREATE INDEX FOR (n:T) ON (n.k)");
    run(&mut graph, "CREATE INDEX FOR (n:T) ON (n.a, n.b)");
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE",
    );
    // One index per slot, not two.
    assert_eq!(graph.property_indices.len(), 1);
    assert_eq!(graph.composite_indices.len(), 1);
    assert_eq!(index_rows(&graph).len(), 2);
    // The user's index is owned too while the constraint stands.
    assert!(try_run(&mut graph, "DROP INDEX `T.k`").is_err());
    run(&mut graph, "DROP CONSTRAINT single");
    run(&mut graph, "DROP CONSTRAINT composite");
    assert!(graph.has_index("T", "k"));
    assert!(graph.has_composite_index("T", &["a".into(), "b".into()]));
    assert!(index_rows(&graph)
        .iter()
        .all(|(_, _, owner)| owner.is_none()));
    // Back to an ordinary index: droppable.
    run(&mut graph, "DROP INDEX `T.k`");
    assert!(!graph.has_index("T", "k"));
}

#[test]
fn a_range_index_asked_for_after_the_constraint_adopts_its_equality_half() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(&mut graph, "CREATE RANGE INDEX FOR (n:T) ON (n.k)");
    assert_eq!(graph.property_indices.len(), 1);
    assert_eq!(graph.range_indices.len(), 1);
    run(&mut graph, "DROP CONSTRAINT single");
    assert!(
        graph.has_index("T", "k"),
        "the user's RANGE index keeps its hash half"
    );
    assert_eq!(graph.range_indices.len(), 1);
}

#[test]
fn naming_the_index_through_the_api_adopts_it() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    graph.create_property_index_routed("T", "k").unwrap();
    assert_eq!(graph.property_indices.len(), 1);
    run(&mut graph, "DROP CONSTRAINT single");
    assert!(graph.has_index("T", "k"));
}

#[test]
fn redeclaring_a_constraint_keeps_what_was_adopted() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE INDEX FOR (n:T) ON (n.k)");
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT single IF NOT EXISTS FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(&mut graph, "DROP CONSTRAINT single");
    assert!(graph.has_index("T", "k"));
}

#[test]
fn the_owned_index_serves_lookups() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "UNWIND range(1, 100) AS i CREATE (:T {id: i, k: 'k' + toString(i)})",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    let hits = graph
        .lookup_by_index("T", "k", &Value::String("k42".into()))
        .expect("the owned index answers point lookups");
    assert_eq!(hits.len(), 1);
    assert!(graph.index_serves_lookups("T", "k"));
    assert_eq!(count(&graph, "MATCH (n:T {k: 'k42'}) RETURN count(n)"), 1);
    assert_eq!(count(&graph, "MATCH (n:T {k: 'nope'}) RETURN count(n)"), 0);
    // And it follows writes, so a lookup never answers from a stale bucket.
    run(&mut graph, "MATCH (n:T {k: 'k42'}) SET n.k = 'moved'");
    assert_eq!(count(&graph, "MATCH (n:T {k: 'k42'}) RETURN count(n)"), 0);
    assert_eq!(count(&graph, "MATCH (n:T {k: 'moved'}) RETURN count(n)"), 1);
    run(&mut graph, "MATCH (n:T {k: 'moved'}) DETACH DELETE n");
    assert_eq!(count(&graph, "MATCH (n:T {k: 'moved'}) RETURN count(n)"), 0);
}

/// A node that stores no `name` is matched through its title, so the index
/// files it there; a constraint has always meant the stored property, and must
/// not see that title-filed node as a holder.
#[test]
fn a_title_filed_node_does_not_hold_a_name_claim() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:T {id: 1, title: 'Ann'})");
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR (n:T) REQUIRE n.name IS UNIQUE",
    );
    // The first node stores no `name`; the second claims 'Ann' as one.
    run(&mut graph, "CREATE (:T {id: 2, name: 'Ann'})");
    assert!(try_run(&mut graph, "CREATE (:T {id: 3, name: 'Ann'})").is_err());
}

// ── Persistence ──

fn saved_and_loaded(graph: DirGraph, deferred: bool) -> Arc<DirGraph> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owned.kgl");
    let path = path.to_str().unwrap();
    let mut arc = Arc::new(graph);
    prepare_save(&mut arc);
    write_kgl(&arc, path).unwrap();
    let loaded =
        load_file_with(path, &LoadOptions::new().with_defer_index_rebuild(deferred)).unwrap();
    // Keep the directory alive only as long as the graph needs it: loading
    // reads the file fully.
    drop(dir);
    loaded
}

#[test]
fn a_save_carries_the_declaration_and_not_the_derived_index() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE",
    );
    run(&mut graph, "CREATE INDEX FOR (n:T) ON (n.plain)");
    run(&mut graph, "CREATE (:T {k: 'a', a: 1, b: 2, plain: 'p'})");
    graph.populate_index_keys();
    assert_eq!(
        graph.property_index_keys,
        [("T".to_string(), "plain".to_string())],
        "only the index the user declared is a user index in the file"
    );
    assert!(graph.composite_index_keys.is_empty());
    assert_eq!(graph.unique_constraint_keys.len(), 2);
}

#[test]
fn a_loaded_file_rebuilds_the_owned_index_and_keeps_enforcing() {
    for deferred in [false, true] {
        let mut graph = DirGraph::new();
        run(
            &mut graph,
            "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
        );
        run(
            &mut graph,
            "CREATE CONSTRAINT composite FOR (n:T) REQUIRE (n.a, n.b) IS UNIQUE",
        );
        run(&mut graph, "CREATE (:T {id: 1, k: 'a', a: 1, b: 2})");
        let listed = index_rows(&graph);
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|(_, _, owner)| owner.is_some()));
        let loaded = saved_and_loaded(graph, deferred);
        assert_eq!(loaded.indexes_deferred(), deferred);
        // Listing is the same before the save and after the load, built or not.
        assert_eq!(index_rows(&loaded), listed, "deferred = {deferred}");
        let mut owned = Arc::try_unwrap(loaded)
            .ok()
            .expect("a fresh load is unshared");
        assert!(try_run(&mut owned, "CREATE (:T {id: 2, k: 'a'})").is_err());
        assert!(try_run(&mut owned, "CREATE (:T {id: 3, a: 1, b: 2})").is_err());
        run(&mut owned, "CREATE (:T {id: 4, k: 'b', a: 5, b: 6})");
        assert!(!owned.indexes_deferred());
        // Still owned-only after the round trip: the constraint takes it away.
        run(&mut owned, "DROP CONSTRAINT single");
        assert!(!owned.has_index("T", "k"));
    }
}

#[test]
fn an_adopted_index_is_a_user_index_in_the_file_and_stays_adopted_after_a_load() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE INDEX FOR (n:T) ON (n.k)");
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(&mut graph, "CREATE (:T {id: 1, k: 'a'})");
    graph.populate_index_keys();
    assert_eq!(
        graph.property_index_keys,
        [("T".to_string(), "k".to_string())]
    );
    let loaded = saved_and_loaded(graph, false);
    let mut graph = Arc::try_unwrap(loaded).ok().unwrap();
    assert!(try_run(&mut graph, "CREATE (:T {id: 2, k: 'a'})").is_err());
    run(&mut graph, "DROP CONSTRAINT single");
    assert!(
        graph.has_index("T", "k"),
        "the user's index outlives the constraint"
    );
}

#[test]
fn a_file_without_the_index_list_entry_opens_as_a_file_from_before_ownership() {
    // What an older build writes: the constraint key, and no index entry for it.
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE CONSTRAINT single FOR (n:T) REQUIRE n.k IS UNIQUE",
    );
    run(&mut graph, "CREATE (:T {id: 1, k: 'a'})");
    graph.populate_index_keys();
    assert!(graph.property_index_keys.is_empty());
    let loaded = saved_and_loaded(graph, false);
    assert!(
        loaded.has_index("T", "k"),
        "the load builds the owned index"
    );
    assert_eq!(
        loaded
            .lookup_by_index("T", "k", &Value::String("a".into()))
            .map(|hits| hits.len()),
        Some(1)
    );
}

#[test]
fn verify_still_reports_a_duplicate_a_file_already_held() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:T {id: 1, k: 'a'}), (:T {id: 2, k: 'a'})",
    );
    graph
        .unique_constraint_keys
        .push(("T".to_string(), vec!["k".to_string()]));
    let loaded = saved_and_loaded(graph, false);
    let mut graph = Arc::try_unwrap(loaded).ok().unwrap();
    let violations = graph.verify_unique_constraints();
    assert_eq!(violations.len(), 1);
    // The first holder keeps the claim; the file still opened.
    let holder = graph.unique_occupant(
        &("T".to_string(), vec!["k".to_string()]),
        &crate::graph::schema::CompositeValue(vec![Value::String("a".into())]),
    );
    assert!(holder.is_some());
}
