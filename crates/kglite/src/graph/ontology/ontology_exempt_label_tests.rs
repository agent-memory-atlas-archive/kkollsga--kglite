//! `exempt` entries match a source node's secondary labels as well as its
//! primary class (and, as before, that class's declared ancestors), and
//! `required` is an exemptable check.

use super::ontology_gate_tests::{declare, in_every_mode, run, run_outcome};
use crate::datatypes::Value;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::ontology::ontology_from_json;
use crate::graph::ontology::violation::{DefineOntologyError, OntologyRule};

fn spec(severity: &str, exempt: &str) -> String {
    format!(
        r#"{{"classes": {{"Section": {{}}, "Law": {{}}}},
            "relationships": {{"SECTION_OF": {{"domain": "Section", "range": "Law",
                "required": true, "required_properties": ["since"],
                "enforcement": "{severity}", "exempt": {exempt}}}}}}}"#
    )
}

fn seeded(graph: &mut DirGraph) {
    run(
        graph,
        "CREATE (:Law {id: 100}), (:Section {id: 1}), (:Section:Historical {id: 2}), \
         (:Section:Historical {id: 3}), (:Section {id: 4})",
    )
    .unwrap();
}

fn line(graph: &mut DirGraph, rule: &str) -> (i64, i64, i64) {
    let rows = run_outcome(
        graph,
        "CALL ontology_audit() YIELD rule, violations, exempted, total \
         RETURN rule, violations, exempted, total",
    )
    .unwrap()
    .result
    .rows;
    let row = rows
        .iter()
        .find(|r| r[0] == Value::String(rule.into()))
        .unwrap_or_else(|| panic!("no {rule} line in {rows:?}"));
    let n = |i: usize| match row[i] {
        Value::Int64(n) => n,
        ref other => panic!("not a count: {other:?}"),
    };
    (n(1), n(2), n(3))
}

#[test]
fn a_secondary_label_exempts_required_and_it_is_counted_apart() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(
            &mut graph,
            &spec("advisory", r#"{"required": ["Historical"]}"#),
        );
        // Four sections hold no edge; the two :Historical ones are excused.
        assert_eq!(line(&mut graph, "SECTION_OF.required"), (2, 2, 4));
        let by_class = run_outcome(
            &mut graph,
            "CALL ontology_audit({by: 'domain_class'}) YIELD rule, domain_class, violations \
             WHERE rule = 'SECTION_OF.required' RETURN domain_class, violations",
        )
        .unwrap()
        .result
        .rows;
        assert_eq!(
            by_class,
            vec![vec![Value::String("Section".into()), Value::Int64(2)]]
        );
        // The row listing keeps the excused nodes: violations + exempted.
        let listed = run_outcome(
            &mut graph,
            "CALL missing_required_edge() YIELD node, rule RETURN count(*)",
        )
        .unwrap()
        .result
        .rows;
        assert_eq!(listed[0][0], Value::Int64(4));
    });
}

#[test]
fn without_the_entry_the_labelled_nodes_count() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec("advisory", "{}"));
        assert_eq!(line(&mut graph, "SECTION_OF.required"), (4, 0, 4));
    });
}

#[test]
fn a_class_entry_still_matches_the_primary_class() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(
            &mut graph,
            &spec("advisory", r#"{"required": ["Section"]}"#),
        );
        assert_eq!(line(&mut graph, "SECTION_OF.required"), (0, 4, 4));
    });
}

#[test]
fn a_secondary_label_exempts_an_edge_property_check() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(
            &mut graph,
            &spec("advisory", r#"{"required_properties": ["Historical"]}"#),
        );
        for id in [1, 2, 3] {
            run(
                &mut graph,
                &format!("MATCH (s:Section {{id: {id}}}), (l:Law) CREATE (s)-[:SECTION_OF]->(l)"),
            )
            .unwrap();
        }
        // Three edges lack `since`; the two from :Historical sections are excused.
        assert_eq!(
            line(&mut graph, "SECTION_OF.required_properties"),
            (1, 2, 3)
        );
    });
}

fn assert_required(error: KgError) {
    match error {
        KgError::OntologyViolation { rule, message, .. } => {
            assert_eq!(
                rule,
                OntologyRule::RequiredRelationship.as_str(),
                "{message}"
            );
        }
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

#[test]
fn the_write_gate_excuses_an_exempt_label_for_required() {
    in_every_mode(|mut graph| {
        run(&mut graph, "CREATE (:Law {id: 100})").unwrap();
        declare(
            &mut graph,
            &spec("error", r#"{"required": ["Historical"]}"#),
        );
        run(&mut graph, "CREATE (:Section:Historical {id: 2})").unwrap();
        assert_required(*run(&mut graph, "CREATE (:Section {id: 1})").unwrap_err());
        // Adding the label later in the same statement repairs the write.
        run(&mut graph, "CREATE (s:Section {id: 5}) SET s:Historical").unwrap();
    });
}

#[test]
fn declaring_error_over_data_whose_violators_are_all_exempt_is_accepted() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Law {id: 100}), (:Section:Historical {id: 2}), (:Section {id: 1})",
    )
    .unwrap();
    let bad = spec("error", "{}");
    match graph.define_ontology(ontology_from_json(&bad).unwrap()) {
        Err(DefineOntologyError::Refused(r)) => {
            assert!(r
                .entries
                .iter()
                .any(|e| e.rule == OntologyRule::RequiredRelationship && e.count == 2));
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    run(&mut graph, "MATCH (s:Section {id: 1}) SET s:Historical").unwrap();
    graph
        .define_ontology(
            ontology_from_json(&spec(
                "error",
                r#"{"required": ["Historical"], "required_properties": ["Historical"]}"#,
            ))
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn checks_that_are_not_exemptable_are_still_refused_with_the_reason() {
    let err =
        ontology_from_json(&spec("advisory", r#"{"cardinality": ["Historical"]}"#)).unwrap_err();
    assert!(err.contains("does not accept check 'cardinality'"), "{err}");
    assert!(
        err.contains("required"),
        "the accept-list names `required`: {err}"
    );
}
