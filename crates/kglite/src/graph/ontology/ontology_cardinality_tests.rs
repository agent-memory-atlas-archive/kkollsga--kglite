//! The maximum side of declared cardinality at write time: a source holding
//! more outgoing relationships of a type than the declared `max` refuses the
//! statement or bulk call (`error`) or reports it (`warn`); the minimum stays
//! audit-only. Each shape runs against memory, mapped and disk storage.

use super::ontology_gate_tests::{count, declare, frame, in_every_mode, run, run_outcome};
use crate::datatypes::{DataFrame, Value};
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::maintain::{add_connections, replace_connections};
use crate::graph::ontology::ontology_from_json;
use crate::graph::ontology::violation::{DefineOntologyError, OntologyRule};

/// `Person` sources may hold at most two `KNOWS` (and, for `Manager`, a
/// descendant of `Person`, the same).
fn knows(card: &str, severity: &str) -> String {
    format!(
        r#"{{"classes": {{"Person": {{}}, "Manager": {{"is_a": "Person"}}, "Robot": {{}}}},
        "relationships": {{"KNOWS": {{"domain": "Person", "range": "Person",
            "cardinality": {card}, "enforcement": "{severity}"}}}}}}"#
    )
}

fn seeded(graph: &mut DirGraph, severity: &str) {
    run(
        graph,
        "CREATE (:Person {id: 1}), (:Person {id: 2}), (:Person {id: 3}), (:Person {id: 4}), \
         (:Manager {id: 5}), (:Robot {id: 9})",
    )
    .unwrap();
    declare(graph, &knows(r#"{"max": 2}"#, severity));
}

fn knows_count(graph: &mut DirGraph) -> i64 {
    count(graph, "MATCH ()-[r:KNOWS]->() RETURN count(r)")
}

fn befriend(graph: &mut DirGraph, source: i64, targets: &[i64]) -> Result<(), Box<KgError>> {
    for target in targets {
        run(
            graph,
            &format!("MATCH (a {{id: {source}}}), (b {{id: {target}}}) CREATE (a)-[:KNOWS]->(b)"),
        )?;
    }
    Ok(())
}

fn assert_cardinality(error: KgError) -> String {
    match error {
        KgError::OntologyViolation {
            rule,
            entity,
            message,
            ..
        } => {
            assert_eq!(rule, OntologyRule::Cardinality.as_str(), "{message}");
            assert_eq!(entity, "relationship", "{message}");
            message
        }
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

fn assert_loader_cardinality(graph: &mut DirGraph, message: &str) {
    let typed = graph
        .take_constraint_error(message)
        .expect("the refusal parked a typed violation");
    assert_cardinality(typed);
}

#[test]
fn the_gate_follows_the_declared_maximum_and_severity() {
    let mut graph = DirGraph::new();
    declare(&mut graph, &knows(r#"{"max": 2}"#, "advisory"));
    assert!(!graph.ontology_rel_gate);
    declare(&mut graph, &knows(r#"{"max": 2}"#, "warn"));
    assert!(graph.ontology_rel_gate);
    // A minimum alone is audit-only: it enrols no write rule.
    declare(
        &mut graph,
        r#"{"relationships": {"KNOWS": {"domain": "Person", "cardinality": {"min": 1},
            "enforcement": {"cardinality": "error"}}}}"#,
    );
    assert!(!graph.ontology_rel_gate);
    // A maximum with no domain enrols nothing, exactly as the audit.
    declare(
        &mut graph,
        r#"{"relationships": {"KNOWS": {"cardinality": {"max": 1}, "enforcement": "error"}}}"#,
    );
    assert!(!graph.ontology_rel_gate);
}

#[test]
fn a_create_over_the_maximum_is_refused_and_rolled_back() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        befriend(&mut graph, 1, &[2, 3]).unwrap();
        let error = befriend(&mut graph, 1, &[4]).unwrap_err();
        let message = assert_cardinality(*error);
        assert!(message.contains("3 'KNOWS'"), "{message}");
        assert_eq!(knows_count(&mut graph), 2);
        // One statement that creates three at once is refused whole.
        let error = run(
            &mut graph,
            "MATCH (a {id: 2}), (b:Person) WHERE b.id <> 2 CREATE (a)-[:KNOWS]->(b)",
        )
        .unwrap_err();
        assert_cardinality(*error);
        assert_eq!(knows_count(&mut graph), 2);
        // A source of a descendant type of the domain is bound too.
        befriend(&mut graph, 5, &[1, 2]).unwrap();
        let error = befriend(&mut graph, 5, &[3]).unwrap_err();
        assert_cardinality(*error);
    });
}

#[test]
fn within_the_maximum_and_outside_the_domain_are_not_refused() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        befriend(&mut graph, 1, &[2, 3]).unwrap();
        befriend(&mut graph, 2, &[1, 3]).unwrap();
        // The target side is unbounded: three incoming is fine.
        befriend(&mut graph, 4, &[3]).unwrap();
        assert_eq!(knows_count(&mut graph), 5);
        // A source outside the declared domain is refused by the domain rule
        // (three edges, over the maximum, but the maximum does not bind it).
        let error = run(
            &mut graph,
            "MATCH (r {id: 9}), (p:Person) WHERE p.id < 4 CREATE (r)-[:KNOWS]->(p)",
        )
        .unwrap_err();
        match *error {
            KgError::OntologyViolation { rule, .. } => assert_eq!(rule, "domain"),
            other => panic!("expected OntologyViolation, got {other:?}"),
        }
    });
}

#[test]
fn merge_on_an_existing_edge_is_not_counted_twice() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        befriend(&mut graph, 1, &[2, 3]).unwrap();
        run(
            &mut graph,
            "MATCH (a {id: 1}), (b {id: 2}) MERGE (a)-[:KNOWS]->(b)",
        )
        .unwrap();
        assert_eq!(knows_count(&mut graph), 2);
        let error = run(
            &mut graph,
            "MATCH (a {id: 1}), (b {id: 4}) MERGE (a)-[:KNOWS]->(b)",
        )
        .unwrap_err();
        assert_cardinality(*error);
    });
}

#[test]
fn deleting_then_creating_stays_within_the_maximum() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        befriend(&mut graph, 1, &[2, 3]).unwrap();
        run(
            &mut graph,
            "MATCH (:Person {id: 1})-[r:KNOWS]->(:Person {id: 2}) DELETE r",
        )
        .unwrap();
        befriend(&mut graph, 1, &[4]).unwrap();
        assert_eq!(knows_count(&mut graph), 2);
        // Within one statement the judge reads the stored end state.
        run(
            &mut graph,
            "MATCH (a {id: 1})-[r:KNOWS]->(:Person {id: 4}) DELETE r \
             WITH a MATCH (b {id: 2}) CREATE (a)-[:KNOWS]->(b)",
        )
        .unwrap();
        assert_eq!(knows_count(&mut graph), 2);
    });
}

#[test]
fn warn_lets_the_edge_land_and_reports_it() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "warn");
        befriend(&mut graph, 1, &[2, 3]).unwrap();
        let outcome = run_outcome(
            &mut graph,
            "MATCH (a {id: 1}), (b {id: 4}) CREATE (a)-[:KNOWS]->(b)",
        )
        .unwrap();
        let warnings = outcome.result.diagnostics.unwrap().warnings;
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("ontology warning (cardinality)")),
            "{warnings:?}"
        );
        assert_eq!(knows_count(&mut graph), 3, "the edge was written");
    });
}

fn pairs(rows: &[(i64, i64)]) -> DataFrame {
    frame(
        &["s", "t"],
        rows.iter()
            .map(|(s, t)| vec![Value::Int64(*s), Value::Int64(*t)])
            .collect(),
    )
}

fn connect(graph: &mut DirGraph, data: DataFrame, replace: bool) -> Result<(), String> {
    let args = (
        "KNOWS".to_string(),
        "Person".to_string(),
        "s".to_string(),
        "Person".to_string(),
        "t".to_string(),
    );
    if replace {
        replace_connections(
            graph, data, args.0, args.1, args.2, args.3, args.4, None, None, None,
        )
        .map(|_| ())
    } else {
        add_connections(
            graph, data, args.0, args.1, args.2, args.3, args.4, None, None, None,
        )
        .map(|_| ())
    }
}

#[test]
fn add_connections_over_the_maximum_writes_nothing() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        // A first load is `Independent`: one edge per row.
        let message = connect(&mut graph, pairs(&[(1, 2), (1, 3), (1, 4)]), false).unwrap_err();
        assert_loader_cardinality(&mut graph, &message);
        assert_eq!(knows_count(&mut graph), 0);
        connect(&mut graph, pairs(&[(1, 2), (1, 3), (2, 1)]), false).unwrap();
        assert_eq!(knows_count(&mut graph), 3);
        // Merging: a repeated pair is not a new edge, a new one is.
        connect(&mut graph, pairs(&[(1, 2), (1, 3)]), false).unwrap();
        assert_eq!(knows_count(&mut graph), 3);
        let message = connect(&mut graph, pairs(&[(1, 4)]), false).unwrap_err();
        assert_loader_cardinality(&mut graph, &message);
        assert_eq!(knows_count(&mut graph), 3);
        // Two new targets for a source holding one is also over.
        let message = connect(&mut graph, pairs(&[(2, 3), (2, 4)]), false).unwrap_err();
        assert_loader_cardinality(&mut graph, &message);
        assert_eq!(knows_count(&mut graph), 3);
    });
}

#[test]
fn add_connections_to_stub_sources_counts_the_frame() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let message = connect(&mut graph, pairs(&[(90, 1), (90, 2), (90, 3)]), false).unwrap_err();
        assert_loader_cardinality(&mut graph, &message);
        assert_eq!(count(&mut graph, "MATCH (n) RETURN count(n)"), 6);
        connect(&mut graph, pairs(&[(90, 1), (90, 2)]), false).unwrap();
        assert_eq!(knows_count(&mut graph), 2);
    });
}

#[test]
fn replace_connections_drops_the_replaced_edges_from_the_count() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        connect(&mut graph, pairs(&[(1, 2), (1, 3)]), false).unwrap();
        // The replace drops the source's stored edges, so one row leaves one.
        connect(&mut graph, pairs(&[(1, 4)]), true).unwrap();
        assert_eq!(knows_count(&mut graph), 1);
        // Three new targets are over the maximum however few were stored.
        let message = connect(&mut graph, pairs(&[(1, 2), (1, 3), (1, 4)]), true).unwrap_err();
        assert_loader_cardinality(&mut graph, &message);
        assert_eq!(knows_count(&mut graph), 1, "the stored edge survived");
    });
}

#[test]
fn add_connections_at_warn_reports_and_writes() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "warn");
        let report = add_connections(
            &mut graph,
            pairs(&[(1, 2), (1, 3), (1, 4)]),
            "KNOWS".into(),
            "Person".into(),
            "s".into(),
            "Person".into(),
            "t".into(),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("ontology warning (cardinality)")),
            "{:?}",
            report.warnings
        );
        assert_eq!(knows_count(&mut graph), 3);
    });
}

fn declared_over(card: &str, severity: &str) -> Result<(), DefineOntologyError> {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (a:Person {id: 1}), (b:Person {id: 2}), (c:Person {id: 3}), \
         (a)-[:KNOWS]->(b), (a)-[:KNOWS]->(c), (b)-[:KNOWS]->(c)",
    )
    .unwrap();
    graph
        .define_ontology(ontology_from_json(&knows(card, severity)).unwrap())
        .map(|_| ())
}

#[test]
fn declaring_an_error_maximum_over_violating_data_is_refused() {
    match declared_over(r#"{"max": 1}"#, "error") {
        Err(DefineOntologyError::Refused(refused)) => {
            assert_eq!(refused.entries.len(), 1, "{refused:?}");
            assert_eq!(refused.entries[0].rule, OntologyRule::Cardinality);
            assert_eq!(refused.entries[0].count, 1, "only the source holding two");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    declared_over(r#"{"max": 2}"#, "error").unwrap();
    declared_over(r#"{"max": 1}"#, "warn").unwrap();
    // The minimum never refuses a declaration: nobody holds three.
    declared_over(r#"{"min": 3}"#, "error").unwrap();
}
