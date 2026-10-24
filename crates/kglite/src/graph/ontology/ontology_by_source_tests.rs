//! `by_source`: property contracts keyed by the source class of a
//! relationship's edges. A key matches by primary class, declared ancestor or
//! secondary label; a source no key matches is not judged, and the audit's
//! denominator counts judged edges only.

use super::ontology_gate_tests::{count, declare, frame, in_every_mode, run, run_outcome};
use crate::datatypes::Value;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::maintain::add_connections;
use crate::graph::ontology::violation::{DefineOntologyError, OntologyRule};
use crate::graph::ontology::{ontology_from_json, ontology_to_json};

/// `MANAGED_BY` leaves `Site` (carrying `siteFrom`) and `Contract` (carrying
/// `contractFrom`); both are `Asset`s. `by` is the per-source map.
fn spec(severity: &str, by: &str, extra: &str) -> String {
    format!(
        r#"{{"classes": {{"Asset": {{"abstract": true}},
            "Site": {{"is_a": "Asset"}}, "Contract": {{"is_a": "Asset"}},
            "Other": {{}}, "Company": {{}}}},
          "relationships": {{"MANAGED_BY": {{"range": "Company",
            "by_source": {by}, "enforcement": "{severity}"{extra}}}}}}}"#
    )
}

const PER_CLASS: &str = r#"{
    "Site": {"required_properties": ["siteFrom"], "property_types": {"siteFrom": "integer"}},
    "Contract": {"required_properties": ["contractFrom"],
                "property_values": {"role": {"enum": ["operator"]}}}}"#;

fn seed(graph: &mut DirGraph) {
    run(
        graph,
        "CREATE (:Site {id: 1}), (:Site {id: 2}), (:Contract {id: 3}), (:Contract {id: 4}), \
                (:Other {id: 5}), (:Company {id: 10})",
    )
    .unwrap();
}

fn edge(graph: &mut DirGraph, from: i64, props: &str) -> Result<(), Box<KgError>> {
    run(
        graph,
        &format!("MATCH (a {{id: {from}}}), (c {{id: 10}}) CREATE (a)-[:MANAGED_BY {props}]->(c)"),
    )
}

/// `(violations, exempted, total)` of one scorecard line.
fn audit_line(graph: &mut DirGraph, rule: &str, by: &str, key: Option<&str>) -> (i64, i64, i64) {
    let rows = run_outcome(
        graph,
        &format!(
            "CALL ontology_audit({by}) YIELD rule, property, domain_class, violations, exempted, \
             total RETURN rule, property, domain_class, violations, exempted, total"
        ),
    )
    .unwrap()
    .result
    .rows;
    let row = rows
        .iter()
        .find(|r| {
            r[0] == Value::String(rule.into())
                && (key.is_none()
                    || r[1] == Value::String(key.unwrap().into())
                    || r[2] == Value::String(key.unwrap().into()))
        })
        .unwrap_or_else(|| panic!("no {rule}/{key:?} line in {rows:?}"));
    let n = |i: usize| match row[i] {
        Value::Int64(n) => n,
        ref other => panic!("not a count: {other:?}"),
    };
    (n(3), n(4), n(5))
}

fn assert_violation(error: Box<KgError>, rule: OntologyRule, property: &str) {
    match *error {
        KgError::OntologyViolation {
            rule: got,
            property: got_property,
            message,
            ..
        } => {
            assert_eq!(got, rule.as_str(), "{message}");
            assert_eq!(got_property.as_deref(), Some(property), "{message}");
        }
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

#[test]
fn the_declaration_validates_and_round_trips() {
    let store = ontology_from_json(&spec("advisory", PER_CLASS, "")).unwrap();
    assert_eq!(store.relationships["MANAGED_BY"].by_source.len(), 2);
    let json = ontology_to_json(&store).unwrap().to_string();
    assert_eq!(ontology_from_json(&json).unwrap(), store, "{json}");
    let stored: crate::graph::ontology::OntologyStore = serde_json::from_str(&json).unwrap();
    assert_eq!(stored, store);

    let bad = |by: &str| ontology_from_json(&spec("advisory", by, "")).unwrap_err();
    assert!(bad(r#"["Site"]"#).contains("must be a {SourceClass"));
    assert!(bad(r#"{"Site": ["x"]}"#).contains("must be a map"));
    assert!(bad(r#"{"Site": {}}"#).contains("declares no property rule"));
    assert!(bad(r#"{"": {"required_properties": ["a"]}}"#).contains("empty class name"));
    assert!(bad(r#"{"Site": {"required_propertys": ["a"]}}"#).contains("Did you mean"));
    assert!(bad(r#"{"Site": {"property_types": {"a": "nope"}}}"#).contains("unknown type"));
    assert!(
        bad(r#"{"Site": {"property_values": {"a": {"pattern": "("}}}}"#)
            .contains("not a valid regex")
    );
    // A class named like a contract key is a class key, never reinterpreted.
    let named = ontology_from_json(&spec(
        "advisory",
        r#"{"property_types": {"required_properties": ["a"]}}"#,
        "",
    ))
    .unwrap();
    assert!(named.relationships["MANAGED_BY"]
        .by_source
        .contains_key("property_types"));
}

#[test]
fn the_audit_judges_each_source_by_its_own_contract() {
    in_every_mode(|mut graph| {
        seed(&mut graph);
        declare(&mut graph, &spec("advisory", PER_CLASS, ""));
        for (from, props) in [
            (1, "{siteFrom: 5}"),
            (2, "{contractFrom: 5}"),
            (3, "{contractFrom: 5, role: 'operator'}"),
            (4, "{role: 'owner'}"),
            (5, "{}"),
        ] {
            edge(&mut graph, from, props).unwrap();
        }
        // Site #2 lacks siteFrom (a Contract-only property does not help);
        // Contract #4 lacks contractFrom. The Other edge matches no key.
        // Judged: 2 Site + 2 Contract edges.
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.required_properties", "", None),
            (2, 0, 4)
        );
        // Only Site declares a type: judged = its two edges; both carry
        // integers or nothing, so no violation.
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.property_types", "", None),
            (0, 0, 2)
        );
        // Only Contract declares values: #4's 'owner' is outside the enum.
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.property_values", "", None),
            (1, 0, 2)
        );
        assert_eq!(
            audit_line(
                &mut graph,
                "MANAGED_BY.required_properties",
                "{by: 'property'}",
                Some("siteFrom")
            ),
            (1, 0, 4)
        );
        assert_eq!(
            audit_line(
                &mut graph,
                "MANAGED_BY.required_properties",
                "{by: 'domain_class'}",
                Some("Contract")
            ),
            (1, 0, 4)
        );
        let rows = run_outcome(
            &mut graph,
            "CALL edge_property_violation() YIELD relationship, check, source, properties \
             WHERE check = 'required_properties' RETURN source.id AS id, properties ORDER BY id",
        )
        .unwrap()
        .result
        .rows;
        let ids: Vec<_> = rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(ids, [Value::Int64(2), Value::Int64(4)]);
    });
}

#[test]
fn a_type_violation_is_judged_per_source() {
    in_every_mode(|mut graph| {
        seed(&mut graph);
        declare(&mut graph, &spec("advisory", PER_CLASS, ""));
        edge(&mut graph, 1, "{siteFrom: 'soon'}").unwrap();
        // The same value on a Contract is nobody's type violation.
        edge(&mut graph, 3, "{siteFrom: 'soon', contractFrom: 1}").unwrap();
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.property_types", "", None),
            (1, 0, 1)
        );
    });
}

#[test]
fn a_key_widens_over_declared_descendants_and_secondary_labels() {
    in_every_mode(|mut graph| {
        run(
            &mut graph,
            "CREATE (:Site {id: 1}), (:Contract {id: 3}), (:Other:Historical {id: 5}), \
                    (:Other {id: 6}), (:Company {id: 10})",
        )
        .unwrap();
        declare(
            &mut graph,
            &spec(
                "advisory",
                r#"{"Asset": {"required_properties": ["since"]},
                    "Historical": {"required_properties": ["until"]}}"#,
                "",
            ),
        );
        for from in [1, 3, 5, 6] {
            edge(&mut graph, from, "{}").unwrap();
        }
        // `Asset` is abstract: Site and Contract match it. #5 matches the
        // secondary label, #6 matches nothing and is not judged. The Asset
        // rule fails 2 edges, the Historical rule 1.
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.required_properties", "", None),
            (3, 0, 3)
        );
        assert_eq!(
            audit_line(
                &mut graph,
                "MANAGED_BY.required_properties",
                "{by: 'property'}",
                Some("until")
            ),
            (1, 0, 3)
        );
    });
}

#[test]
fn exempt_and_the_relationship_level_contract_still_apply() {
    in_every_mode(|mut graph| {
        seed(&mut graph);
        declare(
            &mut graph,
            &spec(
                "advisory",
                PER_CLASS,
                r#", "required_properties": ["source"],
                    "exempt": {"required_properties": ["Contract"]}"#,
            ),
        );
        edge(&mut graph, 1, "{siteFrom: 1, source: 'a'}").unwrap();
        edge(&mut graph, 2, "{source: 'a'}").unwrap();
        edge(&mut graph, 3, "{}").unwrap();
        edge(&mut graph, 5, "{}").unwrap();
        // Flat `source` binds all four edges: #3 and #5 fail it. Per-source:
        // #2 fails siteFrom, #3 fails contractFrom. #3 is exempt (Contract), so it is
        // one exempted edge; #2 and #5 count against the rule.
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.required_properties", "", None),
            (2, 1, 4)
        );
    });
}

#[test]
fn error_refuses_a_write_only_from_the_source_whose_contract_it_breaks() {
    in_every_mode(|mut graph| {
        seed(&mut graph);
        declare(&mut graph, &spec("error", PER_CLASS, ""));
        edge(&mut graph, 1, "{siteFrom: 5}").unwrap();
        edge(&mut graph, 3, "{contractFrom: 5}").unwrap();
        // Another source's property does not satisfy this source's contract.
        let error = edge(&mut graph, 1, "{contractFrom: 5}").unwrap_err();
        assert_violation(error, OntologyRule::RequiredProperty, "siteFrom");
        let error = edge(&mut graph, 3, "{siteFrom: 5}").unwrap_err();
        assert_violation(error, OntologyRule::RequiredProperty, "contractFrom");
        let error = edge(&mut graph, 1, "{siteFrom: 'x'}").unwrap_err();
        assert_violation(error, OntologyRule::PropertyType, "siteFrom");
        let error = edge(&mut graph, 3, "{contractFrom: 1, role: 'owner'}").unwrap_err();
        assert_violation(error, OntologyRule::PropertyValue, "role");
        // A source no key matches is free.
        edge(&mut graph, 5, "{}").unwrap();
        assert_eq!(
            count(&mut graph, "MATCH ()-[r:MANAGED_BY]->() RETURN count(r)"),
            3
        );
        // A statement may repair an edge before it ends.
        run(
            &mut graph,
            "MATCH (a {id: 2}), (c {id: 10}) CREATE (a)-[r:MANAGED_BY]->(c) SET r.siteFrom = 1",
        )
        .unwrap();
    });
}

#[test]
fn add_connections_judges_each_frame_by_its_source_type() {
    in_every_mode(|mut graph| {
        seed(&mut graph);
        declare(&mut graph, &spec("error", PER_CLASS, ""));
        let load = |graph: &mut DirGraph, source: &str, id: i64, column: Option<&str>| {
            let mut columns = vec!["s", "c"];
            let mut row = vec![Value::Int64(id), Value::Int64(10)];
            if let Some(column) = column {
                columns.push(column);
                row.push(Value::Int64(7));
            }
            add_connections(
                graph,
                frame(&columns, vec![row]),
                "MANAGED_BY".into(),
                source.into(),
                "s".into(),
                "Company".into(),
                "c".into(),
                None,
                None,
                None,
            )
            .map(|_| ())
        };
        let message = load(&mut graph, "Site", 1, Some("contractFrom")).unwrap_err();
        let typed = graph.take_constraint_error(&message).expect("typed");
        assert_violation(Box::new(typed), OntologyRule::RequiredProperty, "siteFrom");
        load(&mut graph, "Site", 1, Some("siteFrom")).unwrap();
        load(&mut graph, "Contract", 3, Some("contractFrom")).unwrap();
        let message = load(&mut graph, "Contract", 4, None).unwrap_err();
        let typed = graph.take_constraint_error(&message).expect("typed");
        assert_violation(
            Box::new(typed),
            OntologyRule::RequiredProperty,
            "contractFrom",
        );
    });
}

#[test]
fn declaring_error_over_violating_data_is_refused_per_source() {
    let mut graph = DirGraph::new();
    seed(&mut graph);
    for (from, props) in [
        (1, "{siteFrom: 1}"),
        (2, "{}"),
        (3, "{contractFrom: 1}"),
        (5, "{}"),
    ] {
        edge(&mut graph, from, props).unwrap();
    }
    match graph.define_ontology(ontology_from_json(&spec("error", PER_CLASS, "")).unwrap()) {
        Err(DefineOntologyError::Refused(refused)) => {
            let got: Vec<_> = refused
                .entries
                .iter()
                .map(|e| (e.rule, e.property.as_deref(), e.count))
                .collect();
            assert!(
                got.contains(&(OntologyRule::RequiredProperty, Some("siteFrom"), 1)),
                "{got:?}"
            );
            assert!(
                !got.iter().any(|g| g.1 == Some("contractFrom")),
                "the Contract edge is complete: {got:?}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_key_naming_nothing_declares_with_a_warning() {
    let mut graph = DirGraph::new();
    seed(&mut graph);
    let store = ontology_from_json(&spec(
        "advisory",
        r#"{"Feild": {"required_properties": ["a"]}}"#,
        "",
    ))
    .unwrap();
    let warnings = graph.define_ontology(store).unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("by_source['Feild']") && w.contains("check the spelling")),
        "{warnings:?}"
    );
}

#[test]
fn the_drill_down_reconciles_with_the_audit_beside_distinct() {
    in_every_mode(|mut graph| {
        seed(&mut graph);
        declare(
            &mut graph,
            &spec("advisory", PER_CLASS, r#", "distinct": true"#),
        );
        // A duplicate pair (surplus 1) whose Site edges lack siteFrom, and a
        // complete Contract edge.
        edge(&mut graph, 1, "{}").unwrap();
        edge(&mut graph, 1, "{}").unwrap();
        edge(&mut graph, 3, "{contractFrom: 1}").unwrap();
        let rows = run_outcome(
            &mut graph,
            "CALL edge_property_violation() YIELD check, source RETURN check, count(*) AS n",
        )
        .unwrap()
        .result
        .rows;
        let n = |check: &str| {
            rows.iter()
                .find(|r| r[0] == Value::String(check.into()))
                .map(|r| r[1].clone())
        };
        assert_eq!(n("required_properties"), Some(Value::Int64(2)));
        assert_eq!(n("distinct"), Some(Value::Int64(1)));
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.required_properties", "", None),
            (2, 0, 3)
        );
        assert_eq!(
            audit_line(&mut graph, "MANAGED_BY.distinct", "", None),
            (1, 0, 3)
        );
    });
}
