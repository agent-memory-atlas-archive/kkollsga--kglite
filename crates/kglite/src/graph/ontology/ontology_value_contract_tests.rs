//! `property_values`: per-property value contracts (`enum`, `pattern`,
//! `not_pattern`, `min`, `max`) on class and relationship properties. A null
//! or absent value is never a value violation, and a contract clause only
//! judges the value types it is defined for.

use super::ontology_gate_tests::{count, declare, frame, in_every_mode, run, run_outcome};
use crate::datatypes::Value;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::maintain::{add_connections, add_nodes};
use crate::graph::ontology::violation::{DefineOntologyError, OntologyRule};
use crate::graph::ontology::{ontology_from_json, ontology_to_json};

fn spec(severity: &str, extra_rel: &str) -> String {
    format!(
        r#"{{"classes": {{
            "Judge": {{"property_values": {{
                "name": {{"not_pattern": " og | and |, "}},
                "level": {{"enum": ["hoyesterett", "lagmannsrett", "tingrett"]}},
                "age": {{"min": 18, "max": 99}}}},
                "enforcement": "{severity}"}},
            "Matter": {{}}}},
          "relationships": {{"JUDGED_BY": {{"domain": "Matter", "range": "Judge",
            "property_values": {{"side": {{"enum": ["saksoker", "saksokt"]}},
                                 "ref": {{"pattern": "^[0-9]+/[0-9]+$"}}}},
            "enforcement": "{severity}"{extra_rel}}}}}}}"#
    )
}

fn assert_value_violation(error: KgError, entity: &str, property: &str) -> String {
    match error {
        KgError::OntologyViolation {
            rule,
            entity: got_entity,
            property: got_property,
            message,
            ..
        } => {
            assert_eq!(rule, OntologyRule::PropertyValue.as_str(), "{message}");
            assert_eq!(got_entity, entity, "{message}");
            assert_eq!(got_property.as_deref(), Some(property), "{message}");
            message
        }
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

/// `(violations, exempted, total)` of a rule's audit line, optionally for one
/// property of a `{by: 'property'}` census.
fn audit_line(graph: &mut DirGraph, rule: &str, property: Option<&str>) -> (i64, i64, i64) {
    let by = if property.is_some() {
        "{by: 'property'}"
    } else {
        ""
    };
    let rows = run_outcome(
        graph,
        &format!(
            "CALL ontology_audit({by}) YIELD rule, property, violations, exempted, total \
             RETURN rule, property, violations, exempted, total"
        ),
    )
    .unwrap()
    .result
    .rows;
    let row = rows
        .iter()
        .find(|r| {
            r[0] == Value::String(rule.into())
                && r[1] == property.map_or(Value::Null, |p| Value::String(p.into()))
        })
        .unwrap_or_else(|| panic!("no {rule}/{property:?} line in {rows:?}"));
    let n = |i: usize| match row[i] {
        Value::Int64(n) => n,
        ref other => panic!("not a count: {other:?}"),
    };
    (n(2), n(3), n(4))
}

#[test]
fn the_declaration_round_trips_and_validates() {
    let store = ontology_from_json(&spec("advisory", "")).unwrap();
    let json = ontology_to_json(&store).unwrap().to_string();
    assert_eq!(ontology_from_json(&json).unwrap(), store, "{json}");
    let stored: crate::graph::ontology::OntologyStore = serde_json::from_str(&json).unwrap();
    assert_eq!(stored, store);

    let bad = |contract: &str| {
        ontology_from_json(&format!(
            r#"{{"classes": {{"A": {{"property_values": {{"p": {contract}}}}}}}}}"#
        ))
        .unwrap_err()
    };
    assert!(bad(r#"{"pattern": "("}"#).contains("not a valid regex"));
    assert!(bad(r#"{"not_pattern": "[a"}"#).contains("not_pattern"));
    assert!(bad("{}").contains("declares no constraint"));
    assert!(bad(r#"{"enum": []}"#).contains("at least one value"));
    assert!(bad(r#"{"enum": [[1]]}"#).contains("strings, numbers or booleans"));
    assert!(bad(r#"{"min": 5, "max": 1}"#).contains("above max"));
    assert!(bad(r#"{"min": "1"}"#).contains("must be a number"));
    assert!(bad(r#"{"pattrn": "x"}"#).contains("Did you mean"));
    assert!(bad(r#"{"pattern": 3}"#).contains("regex string"));
    assert!(bad("3").contains("must be a map"));
}

fn seeded(graph: &mut DirGraph) {
    run(
        graph,
        "CREATE (:Judge {id: 1, name: 'A. Lagdommer', level: 'tingrett', age: 40}), \
                (:Judge {id: 2, name: 'A. Lagdommer og B. Lagdommer', level: 'tingrett', age: 40}), \
                (:Judge {id: 3, name: 'C', level: 'district', age: 17}), \
                (:Judge {id: 4, name: 'D', level: null}), \
                (:Judge {id: 5, name: 'E', level: 7, age: 'old'}), \
                (:Matter {id: 10}), (:Matter:Historical {id: 11})",
    )
    .unwrap();
}

#[test]
fn the_node_audit_counts_each_property_and_skips_null_and_foreign_types() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec("advisory", ""));
        // name: only #2 matches the not_pattern. level: #3 and #5 (a number is
        // outside the enum). age: #3 (17 < 18); 'old' is not range-judged and
        // null/absent are never judged. #3 fails two properties, so three
        // nodes (#2, #3, #5) break the rule.
        assert_eq!(
            audit_line(&mut graph, "Judge.property_values", None),
            (3, 0, 5)
        );
        for (property, violations) in [("name", 1), ("level", 2), ("age", 1)] {
            assert_eq!(
                audit_line(&mut graph, "Judge.property_values", Some(property)),
                (violations, 0, 5),
                "{property}"
            );
        }
        let rows = run_outcome(
            &mut graph,
            "CALL node_property_violation() YIELD class, check, node, property, properties \
             WHERE check = 'property_values' RETURN node.id AS id, properties ORDER BY id",
        )
        .unwrap()
        .result
        .rows;
        let ids: Vec<_> = rows.iter().map(|r| r[0].clone()).collect();
        assert_eq!(
            ids,
            [Value::Int64(2), Value::Int64(3), Value::Int64(5)]
                .into_iter()
                .collect::<Vec<_>>()
        );
    });
}

#[test]
fn the_edge_audit_counts_per_property_with_label_exemptions() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(
            &mut graph,
            &spec(
                "advisory",
                r#", "exempt": {"property_values": ["Historical"]}"#,
            ),
        );
        for (case, judge, props) in [
            (10, 1, "{side: 'saksoker', ref: '1/2'}"),
            (10, 2, "{side: 'plaintiff', ref: '1/2'}"),
            (10, 3, "{side: 'saksokt', ref: 'x'}"),
            (10, 4, "{side: 'saksokt'}"),
            (11, 1, "{side: 'plaintiff', ref: 'bad'}"),
        ] {
            run(
                &mut graph,
                &format!(
                    "MATCH (c {{id: {case}}}), (j {{id: {judge}}}) CREATE (c)-[:JUDGED_BY {props}]->(j)"
                ),
            )
            .unwrap();
        }
        assert_eq!(
            audit_line(&mut graph, "JUDGED_BY.property_values", None),
            (2, 1, 5)
        );
        assert_eq!(
            audit_line(&mut graph, "JUDGED_BY.property_values", Some("side")),
            (1, 1, 5)
        );
        assert_eq!(
            audit_line(&mut graph, "JUDGED_BY.property_values", Some("ref")),
            (1, 1, 5)
        );
        let rows = run_outcome(
            &mut graph,
            "CALL edge_property_violation() YIELD check, properties, exempt \
             WHERE check = 'property_values' RETURN count(*) AS n",
        )
        .unwrap()
        .result
        .rows;
        assert_eq!(rows[0][0], Value::Int64(3), "exempted rows stay listed");
    });
}

#[test]
fn error_refuses_a_node_write_that_breaks_a_contract() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &spec("error", ""));
        run(
            &mut graph,
            "CREATE (:Judge {id: 1, name: 'A', level: 'tingrett', age: 40}), (:Judge {id: 2})",
        )
        .unwrap();
        let error = run(&mut graph, "CREATE (:Judge {id: 3, level: 'district'})").unwrap_err();
        let message = assert_value_violation(*error, "node", "level");
        assert!(message.contains("enum [hoyesterett"), "{message}");
        let error = run(&mut graph, "MATCH (j:Judge {id: 1}) SET j.age = 5").unwrap_err();
        assert_value_violation(*error, "node", "age");
        let error = run(&mut graph, "CREATE (:Judge {id: 4, name: 'X og Y'})").unwrap_err();
        assert_value_violation(*error, "node", "name");
        assert_eq!(count(&mut graph, "MATCH (j:Judge) RETURN count(j)"), 2);
        // Null and a value of another type are not value violations.
        run(
            &mut graph,
            "CREATE (:Judge {id: 5, name: null, age: 'old', level: null})",
        )
        .unwrap();
    });
}

#[test]
fn add_nodes_refuses_the_whole_frame() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &spec("error", ""));
        let rows = frame(
            &["id", "level"],
            vec![
                vec![Value::Int64(1), Value::String("tingrett".into())],
                vec![Value::Int64(2), Value::String("nope".into())],
            ],
        );
        let message =
            add_nodes(&mut graph, rows, "Judge".into(), "id".into(), None, None).unwrap_err();
        let typed = graph.take_constraint_error(&message).expect("typed");
        assert_value_violation(typed, "node", "level");
        assert_eq!(count(&mut graph, "MATCH (j:Judge) RETURN count(j)"), 0);
    });
}

#[test]
fn error_refuses_an_edge_write_that_breaks_a_contract() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &spec("error", ""));
        run(&mut graph, "CREATE (:Matter {id: 10}), (:Judge {id: 1})").unwrap();
        let create = |graph: &mut DirGraph, props: &str| {
            run(
                graph,
                &format!(
                    "MATCH (c {{id: 10}}), (j {{id: 1}}) CREATE (c)-[:JUDGED_BY {props}]->(j)"
                ),
            )
        };
        create(&mut graph, "{side: 'saksoker', ref: '12/3'}").unwrap();
        let error = create(&mut graph, "{side: 'plaintiff'}").unwrap_err();
        assert_value_violation(*error, "relationship", "side");
        let error = create(&mut graph, "{ref: '12-3'}").unwrap_err();
        assert_value_violation(*error, "relationship", "ref");
        create(&mut graph, "{}").unwrap();
        assert_eq!(
            count(&mut graph, "MATCH ()-[r:JUDGED_BY]->() RETURN count(r)"),
            2
        );
    });
}

#[test]
fn add_connections_refuses_a_frame_with_a_bad_value() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &spec("error", ""));
        run(&mut graph, "CREATE (:Matter {id: 10}), (:Judge {id: 1})").unwrap();
        let rows = |side: &str| {
            frame(
                &["c", "j", "side"],
                vec![vec![
                    Value::Int64(10),
                    Value::Int64(1),
                    Value::String(side.into()),
                ]],
            )
        };
        let load = |graph: &mut DirGraph, side: &str| {
            add_connections(
                graph,
                rows(side),
                "JUDGED_BY".into(),
                "Matter".into(),
                "c".into(),
                "Judge".into(),
                "j".into(),
                None,
                None,
                None,
            )
            .map(|_| ())
        };
        let message = load(&mut graph, "plaintiff").unwrap_err();
        let typed = graph.take_constraint_error(&message).expect("typed");
        assert_value_violation(typed, "relationship", "side");
        load(&mut graph, "saksokt").unwrap();
    });
}

#[test]
fn warn_lands_the_write_and_reports_it() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &spec("warn", ""));
        let outcome =
            run_outcome(&mut graph, "CREATE (:Judge {id: 1, level: 'district'})").unwrap();
        let warnings = outcome.result.diagnostics.unwrap().warnings;
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("ontology warning (property_value)")),
            "{warnings:?}"
        );
        assert_eq!(count(&mut graph, "MATCH (j:Judge) RETURN count(j)"), 1);
    });
}

#[test]
fn declaring_error_over_violating_data_is_refused_with_the_property_count() {
    let mut graph = DirGraph::new();
    seeded(&mut graph);
    match graph.define_ontology(ontology_from_json(&spec("error", "")).unwrap()) {
        Err(DefineOntologyError::Refused(refused)) => {
            let got: Vec<_> = refused
                .entries
                .iter()
                .map(|e| (e.rule, e.property.as_deref(), e.count))
                .collect();
            assert!(
                got.contains(&(OntologyRule::PropertyValue, Some("level"), 2)),
                "{got:?}"
            );
            assert!(
                got.contains(&(OntologyRule::PropertyValue, Some("age"), 1)),
                "{got:?}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    graph
        .define_ontology(ontology_from_json(&spec("warn", "")).unwrap())
        .unwrap();
}

#[test]
fn a_cloned_declaration_shares_one_compiled_regex() {
    let store = ontology_from_json(&spec("advisory", "")).unwrap();
    let contract = &store.classes["Judge"].property_values["name"];
    let clone = contract.clone();
    assert!(!contract.violates(&Value::String("fine".into())));
    assert!(clone.violates(&Value::String("a og b".into())));
}
