//! `CALL db.ontology.declare` / `db.ontology.clear` and the operator lock.

use std::collections::HashMap;

use super::ontology_gate_tests::{count, in_every_mode, run, run_outcome};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

const PERSON: &str =
    r#"{"classes": {"Person": {"required_properties": ["email"], "enforcement": "error"}}}"#;

fn declare_json(graph: &mut DirGraph, json: &str) -> Result<(), String> {
    let mut params = HashMap::new();
    params.insert("ontology".to_string(), Value::String(json.to_string()));
    execute_mut(
        graph,
        "CALL db.ontology.declare({ontology: $ontology})",
        &ExecuteOptions::eager(&params),
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[test]
fn declare_from_a_json_string_enforces_the_rule() {
    in_every_mode(|mut graph| {
        declare_json(&mut graph, PERSON).unwrap();
        assert!(!graph.ontology.is_empty());
        let err = run(&mut graph, "CREATE (:Person {id: 1})").unwrap_err();
        assert!(matches!(
            *err,
            crate::error::KgError::OntologyViolation { .. }
        ));
    });
}

#[test]
fn declare_from_an_inline_map_and_clear() {
    let mut graph = DirGraph::new();
    let outcome = run_outcome(
        &mut graph,
        "CALL db.ontology.declare({classes: {Person: {required_properties: ['email'], \
         enforcement: 'warn'}}}) YIELD declared, warnings RETURN declared, warnings",
    )
    .unwrap();
    assert_eq!(outcome.result.rows[0][0], Value::Boolean(true));
    assert!(!graph.ontology.is_empty());
    let cleared = run_outcome(
        &mut graph,
        "CALL db.ontology.clear() YIELD cleared RETURN cleared",
    )
    .unwrap();
    assert_eq!(cleared.result.rows[0][0], Value::Boolean(true));
    assert!(graph.ontology.is_empty());
}

#[test]
fn declare_over_violating_data_is_refused_with_the_report() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:Person {id: 1})").unwrap();
    let err = declare_json(&mut graph, PERSON).unwrap_err();
    assert!(err.contains("Person.required_properties"), "{err}");
    assert!(graph.ontology.is_empty());
}

#[test]
fn a_declaration_refused_through_the_procedure_is_a_typed_violation_with_the_report() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:Person {id: 1})").unwrap();
    let err = run(
        &mut graph,
        "CALL db.ontology.declare({classes: {Person: {required_properties: ['email'], \
         enforcement: 'error'}}})",
    )
    .unwrap_err();
    match *err {
        crate::error::KgError::OntologyViolation {
            rule,
            entity_type,
            property,
            report,
            ..
        } => {
            assert_eq!(rule, "required_property");
            assert_eq!(entity_type, "Person");
            assert_eq!(property.as_deref(), Some("email"));
            assert_eq!(report.len(), 1);
            assert_eq!(report[0].count, 1);
        }
        other => panic!("untyped: {other:?}"),
    }
}

#[test]
fn a_locked_graph_refuses_declare_and_clear_and_keeps_the_ontology() {
    let mut graph = DirGraph::new();
    declare_json(&mut graph, PERSON).unwrap();
    graph.lock_ontology();
    let err = declare_json(&mut graph, r#"{"classes": {"Other": {}}}"#).unwrap_err();
    assert!(err.contains("--ontology"), "{err}");
    let err = run(&mut graph, "CALL db.ontology.clear()").unwrap_err();
    assert!(err.to_string().contains("--ontology"), "{err}");
    assert!(graph.clear_ontology().is_err());
    assert!(graph
        .define_ontology(crate::graph::ontology::OntologyStore::default())
        .is_err());
    assert!(graph.ontology.classes.contains_key("Person"));
    assert_eq!(count(&mut graph, "MATCH (n) RETURN count(n)"), 0);
}

#[test]
fn the_lock_survives_a_transaction_fork() {
    let mut graph = DirGraph::new();
    graph.lock_ontology();
    let mut fork = graph.fork_transaction();
    assert!(fork.ontology_locked());
    assert!(fork.clear_ontology().is_err());
}

const RICH: &str = r#"{"enforcement": "warn", "closed_labels": true,
    "classes": {
      "Licensable": {"abstract": true},
      "Contract": {"is_a": "Licensable", "enforcement": "advisory",
                   "required_properties": ["id"]}
    },
    "relationships": {
      "MANAGED_BY": {"domain": "Licensable", "range": "Company",
        "cardinality": {"min": 0, "max": 1}, "property_types": {"validFrom": "date"},
        "enforcement": "error"}
    }}"#;

fn show_row(graph: &mut DirGraph) -> Vec<Value> {
    run_outcome(
        graph,
        "CALL db.ontology.show() YIELD ontology, locked, enforcement \
         RETURN ontology, locked, enforcement",
    )
    .unwrap()
    .result
    .rows
    .remove(0)
}

#[test]
fn show_returns_the_document_declare_accepts_back_unchanged() {
    let mut graph = DirGraph::new();
    declare_json(&mut graph, RICH).unwrap();
    let row = show_row(&mut graph);
    assert!(matches!(row[0], Value::Map(_)), "{:?}", row[0]);
    assert_eq!(row[1], Value::Boolean(false));
    assert_eq!(row[2], Value::String("warn".into()));

    let mut other = DirGraph::new();
    let mut params = HashMap::new();
    params.insert("doc".to_string(), row[0].clone());
    execute_mut(
        &mut other,
        "CALL db.ontology.declare({ontology: $doc})",
        &ExecuteOptions::eager(&params),
    )
    .unwrap();
    assert_eq!(*other.ontology, *graph.ontology);
    assert_eq!(show_row(&mut other)[0], row[0]);
}

#[test]
fn show_without_an_ontology_is_null_and_unlocked() {
    let mut graph = DirGraph::new();
    let row = show_row(&mut graph);
    assert_eq!(row[0], Value::Null);
    assert_eq!(row[1], Value::Boolean(false));
}

#[test]
fn show_reports_the_operator_lock_and_is_not_a_write() {
    let mut graph = DirGraph::new();
    declare_json(&mut graph, PERSON).unwrap();
    graph.lock_ontology();
    let row = show_row(&mut graph);
    assert_eq!(row[1], Value::Boolean(true));
    assert!(matches!(row[0], Value::Map(_)));
    let err = run(&mut graph, "CALL db.ontology.show({x: 1})").unwrap_err();
    assert!(err.to_string().contains("show"), "{err}");
}
