//! The relationship write gate: domain, range, required properties and
//! property types declared at `error` refuse the statement or call that would
//! break them and leave the graph as it was; at `warn` the write lands with a
//! warning; at `advisory` nothing changes.
//!
//! Each shape runs against memory, mapped and disk storage.

use std::collections::HashMap;

use super::ontology_gate_tests::{count, declare, frame, in_every_mode, run, run_outcome};
use crate::datatypes::Value;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::edge_specs::{add_edges_from_specs, EdgeSpec};
use crate::graph::mutation::maintain::{add_connections, replace_connections};

/// `WORKS_AT` runs from `Agent` (abstract, widens to `Person`) to `Company`
/// and requires an integer `since`; `Contractor` sources are exempt from the
/// required-property check.
fn works_at(severity: &str) -> String {
    format!(
        r#"{{"classes": {{
            "Agent": {{"abstract": true}},
            "Person": {{"is_a": "Agent"}},
            "Contractor": {{"is_a": "Agent"}},
            "Company": {{}}
        }},
        "relationships": {{"WORKS_AT": {{
            "domain": "Agent", "range": "Company",
            "required_properties": ["since"],
            "property_types": {{"since": "integer"}},
            "exempt": {{"required_properties": ["Contractor"]}},
            "enforcement": "{severity}"}}}}}}"#
    )
}

fn seeded(graph: &mut DirGraph, severity: &str) {
    run(
        graph,
        "CREATE (:Person {id: 1}), (:Person {id: 2}), (:Company {id: 10}), (:Contractor {id: 3})",
    )
    .unwrap();
    declare(graph, &works_at(severity));
}

fn edges(graph: &mut DirGraph) -> i64 {
    count(graph, "MATCH ()-[r:WORKS_AT]->() RETURN count(r)")
}

fn nodes(graph: &mut DirGraph) -> i64 {
    count(graph, "MATCH (n) RETURN count(n)")
}

fn assert_rel_violation(error: KgError, rule: &str, property: Option<&str>) -> String {
    match error {
        KgError::OntologyViolation {
            rule: r,
            entity,
            property: p,
            message,
            ..
        } => {
            assert_eq!(r, rule, "{message}");
            assert_eq!(entity, "relationship", "{message}");
            assert_eq!(p.as_deref(), property, "{message}");
            message
        }
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

fn assert_loader_rel_violation(
    graph: &mut DirGraph,
    message: &str,
    rule: &str,
    property: Option<&str>,
) {
    let typed = graph
        .take_constraint_error(message)
        .expect("the refusal parked a typed violation");
    assert_rel_violation(typed, rule, property);
}

#[test]
fn the_relationship_gate_follows_the_declared_severities() {
    let mut graph = DirGraph::new();
    assert!(!graph.ontology_rel_gate);
    declare(&mut graph, &works_at("advisory"));
    assert!(!graph.ontology_rel_gate, "advisory enforces nothing");
    declare(&mut graph, &works_at("warn"));
    assert!(graph.ontology_rel_gate);
    assert!(!graph.ontology_node_gate, "no class rule is declared");
    graph.clear_ontology().unwrap();
    assert!(!graph.ontology_rel_gate);
    // A class-only ontology binds no relationship.
    declare(
        &mut graph,
        r#"{"classes": {"A": {"required_properties": ["x"], "enforcement": "error"}}}"#,
    );
    assert!(!graph.ontology_rel_gate);
    assert!(graph.ontology_node_gate);
}

#[test]
fn a_reversed_edge_is_refused_on_domain_and_nothing_persists() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let (nodes_before, edges_before) = (nodes(&mut graph), edges(&mut graph));
        let error = run(
            &mut graph,
            "MATCH (c:Company {id: 10}), (p:Person {id: 1}) CREATE (c)-[:WORKS_AT {since: 1}]->(p)",
        )
        .unwrap_err();
        let message = assert_rel_violation(*error, "domain", None);
        assert!(message.contains("'Company'"), "{message}");
        assert_eq!(
            (nodes(&mut graph), edges(&mut graph)),
            (nodes_before, edges_before)
        );
        // The same statement, the right way round.
        run(
            &mut graph,
            "MATCH (c:Company {id: 10}), (p:Person {id: 1}) CREATE (p)-[:WORKS_AT {since: 1}]->(c)",
        )
        .unwrap();
        assert_eq!(edges(&mut graph), edges_before + 1);
    });
}

#[test]
fn a_node_created_with_the_edge_is_judged_by_its_type() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let before = nodes(&mut graph);
        let error = run(
            &mut graph,
            "CREATE (:Company {id: 11})-[:WORKS_AT {since: 1}]->(:Person {id: 5})",
        )
        .unwrap_err();
        assert_rel_violation(*error, "domain", None);
        assert_eq!(
            nodes(&mut graph),
            before,
            "the endpoints were rolled back too"
        );
    });
}

#[test]
fn a_wrong_target_is_refused_on_range() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let error = run(
            &mut graph,
            "MATCH (a:Person {id: 1}), (b:Person {id: 2}) CREATE (a)-[:WORKS_AT {since: 1}]->(b)",
        )
        .unwrap_err();
        assert_rel_violation(*error, "range", None);
        assert_eq!(edges(&mut graph), 0);
    });
}

#[test]
fn an_abstract_domain_admits_each_declared_descendant() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        run(
            &mut graph,
            "MATCH (c:Company {id: 10}), (k:Contractor {id: 3}) CREATE (k)-[:WORKS_AT]->(c)",
        )
        .unwrap();
        assert_eq!(edges(&mut graph), 1);
    });
}

#[test]
fn required_and_typed_properties_are_judged_on_the_stored_edge() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let pair = "MATCH (p:Person {id: 1}), (c:Company {id: 10})";
        let error = run(&mut graph, &format!("{pair} CREATE (p)-[:WORKS_AT]->(c)")).unwrap_err();
        assert_rel_violation(*error, "required_property", Some("since"));
        // A later SET in the statement repairs the earlier CREATE.
        run(
            &mut graph,
            &format!("{pair} CREATE (p)-[r:WORKS_AT]->(c) SET r.since = 4"),
        )
        .unwrap();
        // A wrong type is refused and the stored value restored.
        let error = run(&mut graph, "MATCH ()-[r:WORKS_AT]->() SET r.since = 'old'").unwrap_err();
        assert_rel_violation(*error, "property_type", Some("since"));
        assert_eq!(
            count(&mut graph, "MATCH ()-[r:WORKS_AT]->() RETURN r.since"),
            4
        );
        let error = run(&mut graph, "MATCH ()-[r:WORKS_AT]->() REMOVE r.since").unwrap_err();
        assert_rel_violation(*error, "required_property", Some("since"));
        assert_eq!(edges(&mut graph), 1);
    });
}

#[test]
fn merge_on_create_set_repairs_the_created_edge() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let pair = "MATCH (p:Person {id: 2}), (c:Company {id: 10})";
        run(
            &mut graph,
            &format!("{pair} MERGE (p)-[r:WORKS_AT]->(c) ON CREATE SET r.since = 2"),
        )
        .unwrap();
        let error = run(
            &mut graph,
            "MATCH (p:Person {id: 1}), (c:Company {id: 10}) MERGE (p)-[:WORKS_AT]->(c)",
        )
        .unwrap_err();
        assert_rel_violation(*error, "required_property", Some("since"));
        assert_eq!(edges(&mut graph), 1);
    });
}

#[test]
fn an_exempt_source_class_is_excused_the_property_check_only() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        // No `since`, but the contractor is exempt from the required check.
        run(
            &mut graph,
            "MATCH (c:Company {id: 10}), (k:Contractor {id: 3}) CREATE (k)-[:WORKS_AT]->(c)",
        )
        .unwrap();
        // The exemption is not for the type rule.
        let error = run(
            &mut graph,
            "MATCH (c:Company {id: 10}), (k:Contractor {id: 3}) CREATE (k)-[:WORKS_AT {since: 'x'}]->(c)",
        )
        .unwrap_err();
        assert_rel_violation(*error, "property_type", Some("since"));
    });
}

#[test]
fn an_endpoint_label_change_leaves_the_incident_edge_alone() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        run(
            &mut graph,
            "MATCH (p:Person {id: 1}), (c:Company {id: 10}) CREATE (p)-[:WORKS_AT {since: 1}]->(c)",
        )
        .unwrap();
        // Labels beyond the primary one are never judged, and the primary
        // label cannot change, so no edge's domain or range can move.
        run(&mut graph, "MATCH (p:Person {id: 1}) SET p:Company").unwrap();
        run(&mut graph, "MATCH (p:Person {id: 1}) REMOVE p:Company").unwrap();
        assert_eq!(edges(&mut graph), 1);
    });
}

#[test]
fn warn_lets_the_edge_land_and_reports_it() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "warn");
        let outcome = run_outcome(
            &mut graph,
            "MATCH (c:Company {id: 10}), (p:Person) CREATE (c)-[:WORKS_AT]->(p)",
        )
        .unwrap();
        let warnings = outcome.result.diagnostics.unwrap().warnings;
        let domain: Vec<_> = warnings
            .iter()
            .filter(|w| w.contains("ontology warning (domain)"))
            .collect();
        assert_eq!(domain.len(), 1, "{warnings:?}");
        assert!(domain[0].contains("2 relationships"), "{domain:?}");
        assert_eq!(edges(&mut graph), 2, "the edges were written");
    });
}

#[test]
fn an_untouched_violation_is_not_judged_and_the_off_state_records_nothing() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (a:Person {id: 1})-[:KNOWS]->(b:Person {id: 2})",
    )
    .unwrap();
    assert!(graph.ontology_touched_edges.is_empty());
    assert!(!graph.ontology_rel_gate);
    in_every_mode(|mut graph| {
        seeded(&mut graph, "warn");
        run(
            &mut graph,
            "MATCH (c:Company {id: 10}), (p:Person {id: 1}) CREATE (c)-[:WORKS_AT]->(p)",
        )
        .unwrap();
        // The stored violator is only judged when something touches it.
        let outcome = run_outcome(&mut graph, "MATCH (p:Person {id: 2}) SET p.x = 1").unwrap();
        let warnings = outcome.result.diagnostics.map(|d| d.warnings);
        assert!(warnings.unwrap_or_default().is_empty());
    });
}

fn pairs(rows: &[(i64, i64)]) -> crate::datatypes::DataFrame {
    frame(
        &["s", "t"],
        rows.iter()
            .map(|(s, t)| vec![Value::Int64(*s), Value::Int64(*t)])
            .collect(),
    )
}

fn connect(
    graph: &mut DirGraph,
    data: crate::datatypes::DataFrame,
    types: (&str, &str),
) -> Result<crate::graph::introspection::reporting::ConnectionOperationReport, String> {
    add_connections(
        graph,
        data,
        "WORKS_AT".into(),
        types.0.into(),
        "s".into(),
        types.1.into(),
        "t".into(),
        None,
        None,
        None,
    )
}

#[test]
fn add_connections_judges_the_calls_type_pair_before_writing() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let message = connect(&mut graph, pairs(&[(10, 1)]), ("Company", "Person")).unwrap_err();
        assert_loader_rel_violation(&mut graph, &message, "domain", None);
        assert_eq!(edges(&mut graph), 0);
        let message = connect(&mut graph, pairs(&[(1, 2)]), ("Person", "Person")).unwrap_err();
        assert_loader_rel_violation(&mut graph, &message, "range", None);
        // Required property: the frame has no `since` column.
        let message = connect(&mut graph, pairs(&[(1, 10)]), ("Person", "Company")).unwrap_err();
        assert_loader_rel_violation(&mut graph, &message, "required_property", Some("since"));
        assert_eq!(edges(&mut graph), 0);
        // An exempt source class needs no `since`.
        connect(&mut graph, pairs(&[(3, 10)]), ("Contractor", "Company")).unwrap();
        assert_eq!(edges(&mut graph), 1);
    });
}

#[test]
fn add_connections_to_stubs_is_judged_on_the_calls_types_and_creates_none_when_refused() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let before = nodes(&mut graph);
        // Neither endpoint exists: both would be vivified as stubs of the
        // call's types, and the domain is judged against that type.
        let message = connect(&mut graph, pairs(&[(90, 91)]), ("Company", "Person")).unwrap_err();
        assert_loader_rel_violation(&mut graph, &message, "domain", None);
        assert_eq!(nodes(&mut graph), before, "no stub survived the refusal");
        // A legal pair of stubs lands, with the required property supplied.
        let data = frame(
            &["s", "t", "since"],
            vec![vec![Value::Int64(90), Value::Int64(91), Value::Int64(5)]],
        );
        connect(&mut graph, data, ("Person", "Company")).unwrap();
        assert_eq!(nodes(&mut graph), before + 2);
    });
}

#[test]
fn add_connections_at_warn_reports_and_writes() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "warn");
        let report = connect(&mut graph, pairs(&[(10, 1)]), ("Company", "Person")).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("ontology warning (domain)")),
            "{:?}",
            report.warnings
        );
        assert_eq!(edges(&mut graph), 1);
    });
}

#[test]
fn replace_connections_refuses_before_it_deletes() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let data = frame(
            &["s", "t", "since"],
            vec![vec![Value::Int64(1), Value::Int64(10), Value::Int64(2)]],
        );
        let call = |graph: &mut DirGraph, data, types: (&str, &str)| {
            replace_connections(
                graph,
                data,
                "WORKS_AT".into(),
                types.0.into(),
                "s".into(),
                types.1.into(),
                "t".into(),
                None,
                None,
                None,
            )
        };
        call(&mut graph, data, ("Person", "Company")).unwrap();
        let message = call(&mut graph, pairs(&[(10, 1)]), ("Company", "Person")).unwrap_err();
        assert_loader_rel_violation(&mut graph, &message, "domain", None);
        assert_eq!(edges(&mut graph), 1, "the stored edge survived the refusal");
    });
}

fn spec(source: (&str, i64), target: (&str, i64), since: Option<Value>) -> EdgeSpec {
    EdgeSpec {
        source_type: source.0.into(),
        source_id: Value::Int64(source.1),
        target_type: target.0.into(),
        target_id: Value::Int64(target.1),
        edge_type: "WORKS_AT".into(),
        properties: since
            .map(|v| HashMap::from([("since".to_string(), v)]))
            .unwrap_or_default(),
    }
}

#[test]
fn add_edges_from_specs_refuses_the_whole_call() {
    in_every_mode(|mut graph| {
        seeded(&mut graph, "error");
        let good = spec(("Person", 1), ("Company", 10), Some(Value::Int64(1)));
        let reversed = spec(("Company", 10), ("Person", 1), Some(Value::Int64(1)));
        let message = add_edges_from_specs(&mut graph, vec![good.clone(), reversed]).unwrap_err();
        assert_loader_rel_violation(&mut graph, &message, "domain", None);
        assert_eq!(
            edges(&mut graph),
            0,
            "the good group was not written either"
        );
        let untyped = spec(
            ("Person", 1),
            ("Company", 10),
            Some(Value::String("x".into())),
        );
        let message = add_edges_from_specs(&mut graph, vec![untyped]).unwrap_err();
        assert_loader_rel_violation(&mut graph, &message, "property_type", Some("since"));
        add_edges_from_specs(&mut graph, vec![good]).unwrap();
        assert_eq!(edges(&mut graph), 1);
    });
}
