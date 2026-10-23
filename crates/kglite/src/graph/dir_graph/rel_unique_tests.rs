//! Relationship `UNIQUE` constraints: declaration, enforcement on every edge
//! write path, ownership of the index, rollback, delete and persistence.
//!
//! The value-family table pins that the constraint admits and refuses exactly the
//! pairs `Value` equality says (the relation the node constraint uses), by
//! comparing against the evaluated values rather than hand-listed expectations.
//! [`assert_consistent`](super::super::rel_index::tests::assert_consistent) rebuilds the
//! index from the edges after each group of writes.

use std::collections::HashMap;
use std::sync::Arc;

use crate::datatypes::Value;
use crate::graph::dir_graph::rel_index::tests::assert_consistent;
use crate::graph::dir_graph::DirGraph;
use crate::graph::io::file::{load_file_with, prepare_save, write_kgl, LoadOptions};
use crate::graph::mutation::edge_specs::{add_edges_from_specs, EdgeSpec};
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};

const UNIQUE_ERROR: &str = "UNIQUE constraint";

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

fn evaluate(expression: &str) -> Value {
    rows(&DirGraph::new(), &format!("RETURN {expression} AS v"))[0][0].clone()
}

/// Three endpoints and a unique `T.k`.
fn constrained() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:P {id: 1}), (:P {id: 2}), (:P {id: 3}), (:M {id: 10}), (:M {id: 11})",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT rk FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE",
    );
    graph
}

fn link(graph: &mut DirGraph, person: i64, movie: i64, props: &str) -> Result<(), String> {
    try_run(
        graph,
        &format!(
            "MATCH (p:P {{id: {person}}}), (m:M {{id: {movie}}}) CREATE (p)-[:T {props}]->(m)"
        ),
    )
}

// ── Declaration ──

#[test]
fn declaring_builds_the_owned_index_and_lists_both_ends() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    assert!(graph.has_rel_property_index("T", "k"));
    let constraints = rows(
        &graph,
        "SHOW CONSTRAINTS YIELD name, type, entityType, labelsOrTypes, properties, ownedIndex \
         RETURN name, type, entityType, labelsOrTypes, properties, ownedIndex",
    );
    assert_eq!(
        constraints,
        vec![vec![
            Value::String("rk".into()),
            Value::String("RELATIONSHIP_UNIQUENESS".into()),
            Value::String("RELATIONSHIP".into()),
            Value::List(vec![Value::String("T".into())]),
            Value::List(vec![Value::String("k".into())]),
            Value::String("relationship:T.k".into()),
        ]]
    );
    let indexes = rows(
        &graph,
        "SHOW INDEXES YIELD name, entityType, owningConstraint RETURN name, entityType, owningConstraint",
    );
    assert_eq!(
        indexes,
        vec![vec![
            Value::String("relationship:T.k".into()),
            Value::String("RELATIONSHIP".into()),
            Value::String("rk".into()),
        ]]
    );
}

#[test]
fn declaring_over_repeated_values_is_refused_and_changes_nothing() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:P {id: 1}), (:M {id: 10}), (:M {id: 11})",
    );
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    link(&mut graph, 1, 11, "{k: 'a'}").unwrap();
    link(&mut graph, 1, 11, "{k: 'b'}").unwrap();
    let error = try_run(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE",
    )
    .unwrap_err();
    assert!(error.contains("duplicate"), "{error}");
    assert!(error.contains("'k' = 'a'"), "{error}");
    assert!(!graph.has_rel_property_indices(), "the built index must go");
    assert!(graph.list_rel_unique_constraints().is_empty());
}

#[test]
fn refusals_say_why() {
    let mut graph = constrained();
    let composite = try_run(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE (r.a, r.b) IS UNIQUE",
    )
    .unwrap_err();
    assert!(composite.contains("exactly one property"), "{composite}");
    let key = try_run(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.z IS RELATIONSHIP KEY",
    )
    .unwrap_err();
    assert!(
        key.contains("IS UNIQUE") && key.contains("IS NOT NULL"),
        "{key}"
    );
    let again = try_run(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE",
    )
    .unwrap_err();
    assert!(again.contains("already exists"), "{again}");
    run(
        &mut graph,
        "CREATE CONSTRAINT IF NOT EXISTS FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE",
    );
}

// ── Enforcement: the same "same value" relation as the node constraint ──

const FAMILIES: &[(&str, &str, &str)] = &[
    ("int", "1", "1"),
    ("int/other", "1", "2"),
    ("int vs float", "1", "1.0"),
    ("float", "1.5", "1.5"),
    ("float zero signs", "0.0", "-0.0"),
    ("string", "'a'", "'a'"),
    ("string case", "'a'", "'A'"),
    ("numeric string", "'1'", "1"),
    ("bool", "true", "true"),
    ("bool/other", "true", "false"),
    ("list", "[1, 2]", "[1, 2]"),
    ("list order", "[1, 2]", "[2, 1]"),
    ("date", "date('2020-01-01')", "date('2020-01-01')"),
    ("date/other", "date('2020-01-01')", "date('2020-01-02')"),
    ("duration", "duration({days: 1})", "duration({days: 1})"),
    (
        "point",
        "point({latitude: 1.0, longitude: 2.0})",
        "point({latitude: 1.0, longitude: 2.0})",
    ),
];

#[test]
fn the_constraint_admits_and_refuses_exactly_what_value_equality_says() {
    let mut refused = 0;
    let mut admitted = 0;
    for (family, first, second) in FAMILIES {
        let collides = evaluate(first) == evaluate(second);
        let mut graph = constrained();
        link(&mut graph, 1, 10, &format!("{{k: {first}}}")).unwrap();
        let created = link(&mut graph, 2, 10, &format!("{{k: {second}}}"));
        match (collides, &created) {
            (true, Err(error)) => {
                assert!(error.contains(UNIQUE_ERROR), "{family}: {error}");
                refused += 1;
            }
            (false, Ok(())) => admitted += 1,
            other => panic!(
                "{family}: values equal = {} but the write returned {:?}",
                other.0, other.1
            ),
        }
        // Through SET, and a refused SET leaves the edge's own claim in place.
        let mut graph = constrained();
        link(&mut graph, 1, 10, &format!("{{k: {first}}}")).unwrap();
        link(&mut graph, 2, 10, "{k: 'free'}").unwrap();
        let set = try_run(
            &mut graph,
            &format!("MATCH (:P {{id: 2}})-[r:T]->() SET r.k = {second}"),
        );
        assert_eq!(set.is_err(), collides, "{family} via SET: {set:?}");
        assert_consistent(&graph);
    }
    assert!(
        refused >= 8 && admitted >= 6,
        "the table must exercise both outcomes: refused {refused}, admitted {admitted}"
    );
}

#[test]
fn null_and_absent_values_claim_nothing_and_parallel_edges_may_differ() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{}").unwrap();
    link(&mut graph, 1, 10, "{}").unwrap();
    link(&mut graph, 1, 10, "{k: null}").unwrap();
    // Parallel edges between one pair are fine while their values differ.
    link(&mut graph, 2, 10, "{k: 'x'}").unwrap();
    link(&mut graph, 2, 10, "{k: 'y'}").unwrap();
    assert!(link(&mut graph, 2, 10, "{k: 'x'}").is_err());
    assert_eq!(count(&graph, "MATCH ()-[r:T]->() RETURN count(r)"), 5);
    assert_consistent(&graph);
}

#[test]
fn every_cypher_write_route_enforces() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    link(&mut graph, 2, 10, "{k: 'b'}").unwrap();
    // CREATE, and a repeat inside one statement.
    assert!(link(&mut graph, 3, 11, "{k: 'a'}")
        .unwrap_err()
        .contains(UNIQUE_ERROR));
    let repeated = try_run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 11}) UNWIND [1, 2] AS i CREATE (p)-[:T {k: 'same'}]->(m)",
    )
    .unwrap_err();
    assert!(repeated.contains(UNIQUE_ERROR), "{repeated}");
    assert_eq!(count(&graph, "MATCH ()-[r:T]->() RETURN count(r)"), 2);
    // SET, SET +=, SET =
    assert!(try_run(&mut graph, "MATCH (:P {id: 2})-[r:T]->() SET r.k = 'a'").is_err());
    assert!(try_run(&mut graph, "MATCH (:P {id: 2})-[r:T]->() SET r += {k: 'a'}").is_err());
    assert!(try_run(&mut graph, "MATCH (:P {id: 2})-[r:T]->() SET r = {k: 'a'}").is_err());
    // A SET to its own value is not a conflict with itself.
    run(&mut graph, "MATCH (:P {id: 1})-[r:T]->() SET r.k = 'a'");
    // CREATE then SET in one statement: the claim lands at the SET.
    assert!(try_run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[r:T]->(m) SET r.k = 'a'"
    )
    .is_err());
    assert_eq!(count(&graph, "MATCH ()-[r:T]->() RETURN count(r)"), 2);
    // MERGE hits instead of duplicating; its create arm is checked like CREATE.
    run(
        &mut graph,
        "MATCH (p:P {id: 1}), (m:M {id: 10}) MERGE (p)-[r:T {k: 'a'}]->(m)",
    );
    assert_eq!(count(&graph, "MATCH ()-[r:T]->() RETURN count(r)"), 2);
    assert!(try_run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 11}) MERGE (p)-[r:T {id: 1}]->(m) ON CREATE SET r.k = 'a'"
    )
    .is_err());
    // REMOVE frees the claim.
    run(&mut graph, "MATCH (:P {id: 1})-[r:T]->() REMOVE r.k");
    run(
        &mut graph,
        "CREATE (:P {id: 4})-[:T {k: 'a'}]->(:M {id: 12})",
    );
    assert!(try_run(
        &mut graph,
        "CREATE (:P {id: 5})-[:T {k: 'a'}]->(:M {id: 13})"
    )
    .is_err());
    assert_consistent(&graph);
}

// ── Bulk loaders ──

fn spec(source: i64, target: i64, edge_type: &str, k: &str) -> EdgeSpec {
    EdgeSpec {
        source_type: "P".to_string(),
        source_id: Value::Int64(source),
        target_type: "M".to_string(),
        target_id: Value::Int64(target),
        edge_type: edge_type.to_string(),
        properties: HashMap::from([("k".to_string(), Value::String(k.to_string()))]),
    }
}

#[test]
fn the_spec_loader_refuses_against_stored_edges_within_a_batch_and_across_groups() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    // Against a stored edge of another pair.
    let stored = add_edges_from_specs(&mut graph, vec![spec(2, 10, "T", "a")]).unwrap_err();
    assert!(stored.contains(UNIQUE_ERROR), "{stored}");
    // Twice in one batch.
    let batch = add_edges_from_specs(
        &mut graph,
        vec![spec(2, 10, "T", "n"), spec(3, 11, "T", "n")],
    )
    .unwrap_err();
    assert!(batch.contains(UNIQUE_ERROR), "{batch}");
    // Across groups: different endpoint pairs form different groups, which the
    // gate judges one after another before writing any.
    run(&mut graph, "CREATE (:Q {id: 1}), (:R {id: 1})");
    let mut other_group = spec(1, 1, "T", "g");
    other_group.source_type = "Q".to_string();
    other_group.target_type = "R".to_string();
    let across =
        add_edges_from_specs(&mut graph, vec![spec(2, 10, "T", "g"), other_group]).unwrap_err();
    assert!(across.contains(UNIQUE_ERROR), "{across}");
    // Nothing from any refused call was written.
    assert_eq!(count(&graph, "MATCH ()-[r:T]->() RETURN count(r)"), 1);
    // A legal batch lands, and an update that merges into its own edge is not a
    // conflict with itself.
    add_edges_from_specs(
        &mut graph,
        vec![spec(2, 10, "T", "b"), spec(1, 10, "T", "a")],
    )
    .unwrap();
    assert_eq!(count(&graph, "MATCH ()-[r:T]->() RETURN count(r)"), 2);
    assert_consistent(&graph);
}

#[test]
fn a_batch_that_moves_a_value_off_one_edge_frees_it_for_another() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    // Edge (1,10) moves 'a' -> 'z' while a new edge takes 'a': legal as a whole.
    add_edges_from_specs(
        &mut graph,
        vec![spec(1, 10, "T", "z"), spec(2, 10, "T", "a")],
    )
    .unwrap();
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'a'}]->() RETURN count(r)"),
        1
    );
    assert_eq!(
        count(&graph, "MATCH ()-[r:T {k: 'z'}]->() RETURN count(r)"),
        1
    );
    assert_consistent(&graph);
}

// ── Rollback, delete, vacuum ──

#[test]
fn a_failed_statement_gives_every_claim_back() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    link(&mut graph, 2, 10, "{k: 'b'}").unwrap();
    // A claim taken, then the statement fails: the value is free again.
    assert!(try_run(
        &mut graph,
        "MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'new'}]->(m) \
         WITH p, m CREATE (p)-[:T {k: 'a'}]->(m)"
    )
    .is_err());
    link(&mut graph, 3, 11, "{k: 'new'}").unwrap();
    run(&mut graph, "MATCH ()-[r:T {k: 'new'}]->() DELETE r");
    // A claim moved, then the statement fails: the old value is held again.
    assert!(try_run(
        &mut graph,
        "MATCH (:P {id: 1})-[r:T]->() SET r.k = 'moved' \
         WITH r MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'moved'}]->(m)"
    )
    .is_err());
    assert!(link(&mut graph, 3, 11, "{k: 'a'}").is_err());
    link(&mut graph, 3, 11, "{k: 'moved'}").unwrap();
    run(&mut graph, "MATCH ()-[r:T {k: 'moved'}]->() DELETE r");
    // A claim released by a delete, then the statement fails: held again.
    assert!(try_run(
        &mut graph,
        "MATCH ()-[r:T {k: 'b'}]->() DELETE r \
         WITH 1 AS x UNWIND [1, 0] AS d RETURN 1 / d"
    )
    .is_err());
    assert!(link(&mut graph, 3, 11, "{k: 'b'}").is_err());
    assert_consistent(&graph);
}

#[test]
fn deletes_release_claims_and_vacuum_renumbers_them() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    link(&mut graph, 2, 10, "{k: 'b'}").unwrap();
    run(&mut graph, "MATCH ()-[r:T {k: 'a'}]->() DELETE r");
    link(&mut graph, 3, 11, "{k: 'a'}").unwrap();
    run(&mut graph, "MATCH (p:P {id: 2}) DETACH DELETE p");
    link(&mut graph, 3, 10, "{k: 'b'}").unwrap();
    // Delete then re-create in one statement.
    run(
        &mut graph,
        "MATCH ()-[r:T {k: 'b'}]->() DELETE r WITH 1 AS x \
         MATCH (p:P {id: 3}), (m:M {id: 10}) CREATE (p)-[:T {k: 'b'}]->(m)",
    );
    assert!(link(&mut graph, 3, 10, "{k: 'b'}").is_err());
    graph.vacuum();
    assert_consistent(&graph);
    assert!(link(&mut graph, 3, 10, "{k: 'a'}").is_err());
    assert!(link(&mut graph, 3, 10, "{k: 'b'}").is_err());
    link(&mut graph, 3, 10, "{k: 'fresh'}").unwrap();
}

// ── Ownership ──

#[test]
fn drop_index_is_refused_and_drop_constraint_removes_the_index() {
    let mut graph = constrained();
    for statement in [
        "DROP INDEX `relationship:T.k`",
        "DROP INDEX `relationship:T.k` IF EXISTS",
        "DROP INDEX FOR ()-[r:T]-() ON (r.k)",
    ] {
        let error = try_run(&mut graph, statement).unwrap_err();
        assert!(
            error.contains("backs the constraint 'rk'"),
            "{statement}: {error}"
        );
    }
    assert!(graph.has_rel_property_index("T", "k"));
    run(&mut graph, "DROP CONSTRAINT rk");
    assert!(!graph.has_rel_property_indices());
    // Unconstrained again.
    link(&mut graph, 1, 10, "{k: 'x'}").unwrap();
    link(&mut graph, 2, 10, "{k: 'x'}").unwrap();
}

#[test]
fn create_index_on_an_owned_index_is_a_duplicate() {
    let mut graph = constrained();
    let error = try_run(&mut graph, "CREATE INDEX FOR ()-[r:T]-() ON (r.k)").unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    run(
        &mut graph,
        "CREATE INDEX IF NOT EXISTS FOR ()-[r:T]-() ON (r.k)",
    );
    // A no-op adopts nothing: the index still goes with its constraint.
    run(&mut graph, "DROP CONSTRAINT rk");
    assert!(!graph.has_rel_property_indices());
}

#[test]
fn an_index_declared_first_is_adopted_and_outlives_the_constraint() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:P {id: 1}), (:M {id: 10})");
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    run(&mut graph, "CREATE INDEX FOR ()-[r:T]-() ON (r.k)");
    run(
        &mut graph,
        "CREATE CONSTRAINT rk FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE",
    );
    assert_eq!(graph.rel.indices.len(), 1, "one index, not two");
    assert!(try_run(&mut graph, "DROP INDEX FOR ()-[r:T]-() ON (r.k)").is_err());
    run(&mut graph, "DROP CONSTRAINT rk");
    assert!(
        graph.has_rel_property_index("T", "k"),
        "the user's index stays"
    );
    run(&mut graph, "DROP INDEX FOR ()-[r:T]-() ON (r.k)");
    assert!(!graph.has_rel_property_indices());
}

// ── Temporal types and the disk backend ──

#[test]
fn a_type_with_a_validity_interval_cannot_be_unique_and_cannot_become_temporal() {
    use crate::graph::features::temporal::{declare, IntervalConvention, TemporalTarget};
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:P {id: 1})-[:V {k: 'a', valid_from: date('2020-01-01'), valid_to: date('2020-12-31')}]->(:M {id: 10})",
    );
    let target = TemporalTarget::Relationship {
        rel_type: "V".to_string(),
        source_type: None,
    };
    declare(
        &mut graph,
        &target,
        "valid_from",
        "valid_to",
        IntervalConvention::Closed,
    )
    .unwrap();
    let error = try_run(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:V]-() REQUIRE r.k IS UNIQUE",
    )
    .unwrap_err();
    assert!(error.contains("validity interval"), "{error}");
    assert!(graph.list_rel_unique_constraints().is_empty());

    // And the other order: unique first, then an interval is refused.
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:P {id: 1})-[:V {k: 'a', valid_from: date('2020-01-01'), valid_to: date('2020-12-31')}]->(:M {id: 10})",
    );
    run(
        &mut graph,
        "CREATE CONSTRAINT FOR ()-[r:V]-() REQUIRE r.k IS UNIQUE",
    );
    let error = declare(
        &mut graph,
        &target,
        "valid_from",
        "valid_to",
        IntervalConvention::Closed,
    )
    .unwrap_err();
    assert!(error.contains("UNIQUE constraint"), "{error}");
}

// ── Persistence ──

fn saved_and_loaded(graph: DirGraph, deferred: bool) -> DirGraph {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unique.kgl");
    let path = path.to_str().unwrap();
    let mut arc = Arc::new(graph);
    prepare_save(&mut arc);
    write_kgl(&arc, path).unwrap();
    let loaded =
        load_file_with(path, &LoadOptions::new().with_defer_index_rebuild(deferred)).unwrap();
    drop(dir);
    Arc::try_unwrap(loaded).ok().expect("sole owner")
}

#[test]
fn a_save_carries_the_declaration_not_the_owned_index() {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    graph.populate_index_keys();
    assert_eq!(
        graph.rel.unique.iter().collect::<Vec<_>>(),
        [&("T".to_string(), "k".to_string())]
    );
    assert!(
        graph.rel.index_keys.is_empty(),
        "an index only a constraint owns is derived, not persisted"
    );
    for deferred in [false, true] {
        let mut loaded = saved_and_loaded(constrained_with_data(), deferred);
        assert_eq!(loaded.indexes_deferred(), deferred);
        let listed = rows(
            &loaded,
            "SHOW INDEXES YIELD name, owningConstraint WHERE name = 'relationship:T.k' RETURN owningConstraint",
        );
        assert_eq!(listed, vec![vec![Value::String("rk".into())]]);
        // Enforcement works from the first write, deferred or not.
        let error = try_run(
            &mut loaded,
            "MATCH (p:P {id: 3}), (m:M {id: 11}) CREATE (p)-[:T {k: 'a'}]->(m)",
        )
        .unwrap_err();
        assert!(error.contains(UNIQUE_ERROR), "deferred={deferred}: {error}");
        assert_consistent(&loaded);
    }
}

fn constrained_with_data() -> DirGraph {
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    link(&mut graph, 2, 10, "{k: 'b'}").unwrap();
    graph
}

#[test]
fn an_adopted_index_is_persisted_with_the_user_lists() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:P {id: 1}), (:M {id: 10})");
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    run(&mut graph, "CREATE INDEX FOR ()-[r:T]-() ON (r.k)");
    run(
        &mut graph,
        "CREATE CONSTRAINT rk FOR ()-[r:T]-() REQUIRE r.k IS UNIQUE",
    );
    graph.populate_index_keys();
    assert_eq!(graph.rel.index_keys.len(), 1);
    let mut loaded = saved_and_loaded(graph, false);
    run(&mut loaded, "DROP CONSTRAINT rk");
    assert!(
        loaded.has_rel_property_index("T", "k"),
        "adoption survives a reload"
    );
}

#[test]
fn a_stale_index_entry_never_refuses_a_legal_write() {
    use crate::graph::dir_graph::rel_index::edge_slot;
    use petgraph::graph::EdgeIndex;
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    // A member for a dead slot under 'ghost', and one whose edge holds another
    // value under 'liar': neither is an occupant.
    let index = graph
        .rel
        .indices
        .get_mut(&("T".to_string(), "k".to_string()))
        .unwrap();
    index
        .entry_or_default(&Value::String("ghost".into()))
        .push(edge_slot(EdgeIndex::new(999)));
    index
        .entry_or_default(&Value::String("liar".into()))
        .push(edge_slot(EdgeIndex::new(0)));
    link(&mut graph, 2, 10, "{k: 'ghost'}").unwrap();
    link(&mut graph, 3, 10, "{k: 'liar'}").unwrap();
}

#[test]
fn a_refusal_surfaces_as_the_typed_constraint_error_with_the_neo4j_code() {
    use crate::error::{KgError, KgErrorCode};
    let mut graph = constrained();
    link(&mut graph, 1, 10, "{k: 'a'}").unwrap();
    // The write path parks the typed violation beside the message it returns.
    let message = graph
        .check_rel_unique_row("T", |p| (p == "k").then(|| Value::String("a".into())), None)
        .unwrap_err();
    let error = graph
        .take_constraint_error(&message)
        .expect("the typed violation is parked");
    assert_eq!(error.code(), KgErrorCode::ConstraintViolation);
    assert_eq!(
        error.code().neo4j_status_code(),
        "Neo.ClientError.Schema.ConstraintValidationFailed"
    );
    match error {
        KgError::ConstraintViolation { message, .. } => {
            assert!(message.contains("relationship of type 'T'"), "{message}");
        }
        other => panic!("not a constraint violation: {other:?}"),
    }
}
