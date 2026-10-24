//! `distinct`: at most one edge of a type per source, target and listed
//! property values. Audit counts surplus edges; write gates refuse (`error`)
//! or report (`warn`) the write that leaves a group with more than one.

use super::ontology_gate_tests::{declare, frame, in_every_mode, run, run_outcome};
use crate::datatypes::{DataFrame, Value};
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::maintain::add_connections;
use crate::graph::ontology::violation::{DefineOntologyError, OntologyRule};
use crate::graph::ontology::{ontology_from_json, ontology_to_json};

fn spec(distinct: &str, severity: &str, extra: &str) -> String {
    format!(
        r#"{{"classes": {{"Law": {{}}, "Actor": {{}}}},
            "relationships": {{"HAS": {{"domain": "Law", "range": "Actor",
                "distinct": {distinct}, "enforcement": "{severity}"{extra}}}}}}}"#
    )
}

fn seeded(graph: &mut DirGraph) {
    run(
        graph,
        "CREATE (:Law {id: 1}), (:Law {id: 2}), (:Law:Historical {id: 3}), \
         (:Actor {id: 10}), (:Actor {id: 11})",
    )
    .unwrap();
}

fn link(graph: &mut DirGraph, source: i64, target: i64, props: &str) -> Result<(), Box<KgError>> {
    run(
        graph,
        &format!("MATCH (a {{id: {source}}}), (b {{id: {target}}}) CREATE (a)-[:HAS {props}]->(b)"),
    )
}

fn has_count(graph: &mut DirGraph) -> i64 {
    super::ontology_gate_tests::count(graph, "MATCH ()-[r:HAS]->() RETURN count(r)")
}

/// `(violations, exempted, total)` of the `HAS.distinct` audit line.
fn distinct_line(graph: &mut DirGraph) -> (i64, i64, i64) {
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
        .find(|r| r[0] == Value::String("HAS.distinct".into()))
        .unwrap_or_else(|| panic!("no HAS.distinct line in {rows:?}"));
    let n = |i: usize| match row[i] {
        Value::Int64(n) => n,
        ref other => panic!("not a count: {other:?}"),
    };
    (n(1), n(2), n(3))
}

fn assert_distinct(error: KgError) -> String {
    match error {
        KgError::OntologyViolation {
            rule,
            entity,
            message,
            ..
        } => {
            assert_eq!(rule, OntologyRule::Distinct.as_str(), "{message}");
            assert_eq!(entity, "relationship", "{message}");
            message
        }
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

#[test]
fn the_declaration_parses_both_forms_and_round_trips() {
    let pair = ontology_from_json(&spec("true", "advisory", "")).unwrap();
    assert!(pair.relationships["HAS"]
        .distinct
        .as_ref()
        .unwrap()
        .properties
        .is_empty());
    let keyed = ontology_from_json(&spec(r#"["side", "kind"]"#, "advisory", "")).unwrap();
    assert_eq!(
        keyed.relationships["HAS"]
            .distinct
            .as_ref()
            .unwrap()
            .properties,
        ["side", "kind"]
    );
    for store in [&pair, &keyed] {
        let json = ontology_to_json(store).unwrap().to_string();
        assert_eq!(&ontology_from_json(&json).unwrap(), store, "{json}");
        let stored: crate::graph::ontology::OntologyStore = serde_json::from_str(&json).unwrap();
        assert_eq!(&stored, store);
    }
    let off = ontology_from_json(&spec("false", "advisory", "")).unwrap();
    assert!(off.relationships["HAS"].distinct.is_none());
}

#[test]
fn malformed_declarations_are_refused_with_the_reason() {
    for (distinct, needle) in [
        ("[]", "use `true`"),
        (r#"["a", "a"]"#, "twice"),
        ("[1]", "non-empty strings"),
        (r#""yes""#, "`true` or a list"),
    ] {
        let err = ontology_from_json(&spec(distinct, "advisory", "")).unwrap_err();
        assert!(err.contains(needle), "{distinct}: {err}");
    }
}

#[test]
fn the_audit_counts_surplus_edges_over_all_edges_of_the_type() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec("true", "advisory", ""));
        link(&mut graph, 1, 10, "").unwrap();
        link(&mut graph, 1, 10, "").unwrap();
        link(&mut graph, 1, 10, "").unwrap();
        link(&mut graph, 1, 11, "").unwrap();
        // A group of three is two surplus edges; the type holds four edges.
        assert_eq!(distinct_line(&mut graph), (2, 0, 4));
    });
}

#[test]
fn listed_properties_are_part_of_the_group_and_null_equals_absent() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec(r#"["side"]"#, "advisory", ""));
        link(&mut graph, 1, 10, "{side: 'a'}").unwrap();
        link(&mut graph, 1, 10, "{side: 'b'}").unwrap();
        assert_eq!(distinct_line(&mut graph), (0, 0, 2));
        link(&mut graph, 1, 10, "{side: 'a'}").unwrap();
        assert_eq!(distinct_line(&mut graph), (1, 0, 3));
        link(&mut graph, 2, 10, "").unwrap();
        link(&mut graph, 2, 10, "{side: null}").unwrap();
        assert_eq!(distinct_line(&mut graph), (2, 0, 5));
    });
}

#[test]
fn a_secondary_label_exempts_its_surplus_and_it_is_counted_apart() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(
            &mut graph,
            &spec(
                "true",
                "advisory",
                r#", "exempt": {"distinct": ["Historical"]}"#,
            ),
        );
        link(&mut graph, 1, 10, "").unwrap();
        link(&mut graph, 1, 10, "").unwrap();
        link(&mut graph, 3, 10, "").unwrap();
        link(&mut graph, 3, 10, "").unwrap();
        link(&mut graph, 3, 10, "").unwrap();
        assert_eq!(distinct_line(&mut graph), (1, 2, 5));
    });
}

#[test]
fn the_domain_class_breakdown_partitions_the_surplus() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec("true", "advisory", ""));
        link(&mut graph, 1, 10, "").unwrap();
        link(&mut graph, 1, 10, "").unwrap();
        link(&mut graph, 3, 11, "").unwrap();
        link(&mut graph, 3, 11, "").unwrap();
        let rows = run_outcome(
            &mut graph,
            "CALL ontology_audit({by: 'domain_class'}) YIELD rule, domain_class, violations \
             WHERE rule = 'HAS.distinct' RETURN domain_class, violations",
        )
        .unwrap()
        .result
        .rows;
        assert_eq!(
            rows,
            vec![vec![Value::String("Law".into()), Value::Int64(2)]],
            "both sources are Law nodes; the secondary label does not split the class"
        );
    });
}

#[test]
fn error_refuses_the_duplicate_edge_and_rolls_back() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec("true", "error", ""));
        link(&mut graph, 1, 10, "").unwrap();
        let message = assert_distinct(*link(&mut graph, 1, 10, "").unwrap_err());
        assert!(message.contains("2 'HAS'"), "{message}");
        assert_eq!(has_count(&mut graph), 1);
        // A different target, and a different source, are fine.
        link(&mut graph, 1, 11, "").unwrap();
        link(&mut graph, 2, 10, "").unwrap();
        // Two copies created by one statement are one violating group.
        let error = run(
            &mut graph,
            "MATCH (a {id: 2}), (b {id: 11}) CREATE (a)-[:HAS]->(b), (a)-[:HAS]->(b)",
        )
        .unwrap_err();
        assert_distinct(*error);
        assert_eq!(has_count(&mut graph), 3);
        // MERGE finds the stored edge and adds none.
        run(
            &mut graph,
            "MATCH (a {id: 1}), (b {id: 10}) MERGE (a)-[:HAS]->(b)",
        )
        .unwrap();
        assert_eq!(has_count(&mut graph), 3);
    });
}

#[test]
fn error_with_properties_judges_the_group_key() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec(r#"["side"]"#, "error", ""));
        link(&mut graph, 1, 10, "{side: 'a'}").unwrap();
        link(&mut graph, 1, 10, "{side: 'b'}").unwrap();
        assert_distinct(*link(&mut graph, 1, 10, "{side: 'a'}").unwrap_err());
        // A later SET that makes two edges collide is judged too.
        let error = run(
            &mut graph,
            "MATCH (:Law {id: 1})-[r:HAS {side: 'b'}]->(:Actor {id: 10}) SET r.side = 'a'",
        )
        .unwrap_err();
        assert_distinct(*error);
        assert_eq!(has_count(&mut graph), 2);
    });
}

#[test]
fn an_exempt_label_excuses_the_write() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(
            &mut graph,
            &spec(
                "true",
                "error",
                r#", "exempt": {"distinct": ["Historical"]}"#,
            ),
        );
        link(&mut graph, 3, 10, "").unwrap();
        link(&mut graph, 3, 10, "").unwrap();
        link(&mut graph, 1, 10, "").unwrap();
        assert_distinct(*link(&mut graph, 1, 10, "").unwrap_err());
    });
}

#[test]
fn warn_lets_the_duplicate_land_and_reports_it() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec("true", "warn", ""));
        link(&mut graph, 1, 10, "").unwrap();
        let outcome = run_outcome(
            &mut graph,
            "MATCH (a {id: 1}), (b {id: 10}) CREATE (a)-[:HAS]->(b)",
        )
        .unwrap();
        let warnings = outcome.result.diagnostics.unwrap().warnings;
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("ontology warning (distinct)")),
            "{warnings:?}"
        );
        assert_eq!(has_count(&mut graph), 2);
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

fn connect(graph: &mut DirGraph, data: DataFrame) -> Result<(), String> {
    add_connections(
        graph,
        data,
        "HAS".into(),
        "Law".into(),
        "s".into(),
        "Actor".into(),
        "t".into(),
        None,
        None,
        None,
    )
    .map(|_| ())
}

#[test]
fn a_first_load_with_repeated_pairs_is_refused_whole() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec("true", "error", ""));
        let message = connect(&mut graph, pairs(&[(1, 10), (1, 11), (1, 10)])).unwrap_err();
        let typed = graph
            .take_constraint_error(&message)
            .expect("typed violation parked");
        assert_distinct(typed);
        assert_eq!(has_count(&mut graph), 0, "nothing was written");
        connect(&mut graph, pairs(&[(1, 10), (1, 11), (2, 10)])).unwrap();
        assert_eq!(has_count(&mut graph), 3);
        // A merging load folds a repeated pair onto its edge.
        connect(&mut graph, pairs(&[(1, 10), (1, 10)])).unwrap();
        assert_eq!(has_count(&mut graph), 3);
    });
}

#[test]
fn a_first_load_judges_the_listed_property_columns() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec(r#"["side"]"#, "error", ""));
        let rows = |sides: [&str; 2]| {
            frame(
                &["s", "t", "side"],
                sides
                    .iter()
                    .map(|side| {
                        vec![
                            Value::Int64(1),
                            Value::Int64(10),
                            Value::String((*side).into()),
                        ]
                    })
                    .collect(),
            )
        };
        let load = |graph: &mut DirGraph, data| {
            add_connections(
                graph,
                data,
                "HAS".into(),
                "Law".into(),
                "s".into(),
                "Actor".into(),
                "t".into(),
                None,
                None,
                None,
            )
            .map(|_| ())
        };
        let message = load(&mut graph, rows(["a", "a"])).unwrap_err();
        let typed = graph.take_constraint_error(&message).expect("typed");
        assert_distinct(typed);
        load(&mut graph, rows(["a", "b"])).unwrap();
        assert_eq!(has_count(&mut graph), 2);
    });
}

fn declared_over(severity: &str) -> Result<(), DefineOntologyError> {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (a:Law {id: 1}), (b:Actor {id: 10}), (c:Actor {id: 11}), \
         (a)-[:HAS]->(b), (a)-[:HAS]->(b), (a)-[:HAS]->(b), (a)-[:HAS]->(c)",
    )
    .unwrap();
    graph
        .define_ontology(ontology_from_json(&spec("true", severity, "")).unwrap())
        .map(|_| ())
}

#[test]
fn declaring_error_over_existing_duplicates_is_refused_with_the_surplus_count() {
    match declared_over("error") {
        Err(DefineOntologyError::Refused(refused)) => {
            assert_eq!(refused.entries.len(), 1, "{refused:?}");
            assert_eq!(refused.entries[0].rule, OntologyRule::Distinct);
            assert_eq!(refused.entries[0].entity_type, "HAS");
            assert_eq!(refused.entries[0].count, 2);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    declared_over("warn").unwrap();
    declared_over("advisory").unwrap();
}

#[test]
fn numeric_group_values_compare_by_value_not_by_type() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec(r#"["w"]"#, "advisory", ""));
        link(&mut graph, 1, 10, "{w: 1}").unwrap();
        link(&mut graph, 1, 10, "{w: 1.0}").unwrap();
        assert_eq!(
            distinct_line(&mut graph),
            (1, 0, 2),
            "1 and 1.0 are one group"
        );
        link(&mut graph, 1, 10, "{w: 1.5}").unwrap();
        link(&mut graph, 1, 10, "{w: '1'}").unwrap();
        assert_eq!(
            distinct_line(&mut graph),
            (1, 0, 4),
            "1.5 and the string '1' are groups of their own"
        );
        link(&mut graph, 2, 10, "{w: [1, 2]}").unwrap();
        link(&mut graph, 2, 10, "{w: [1.0, 2.0]}").unwrap();
        link(&mut graph, 2, 10, "{w: [1.0, 3]}").unwrap();
        assert_eq!(
            distinct_line(&mut graph),
            (2, 0, 7),
            "lists compare element-wise"
        );
    });
}

#[test]
fn the_write_gate_treats_one_and_one_point_zero_as_a_duplicate() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        declare(&mut graph, &spec(r#"["w"]"#, "error", ""));
        link(&mut graph, 1, 10, "{w: 1}").unwrap();
        assert_distinct(*link(&mut graph, 1, 10, "{w: 1.0}").unwrap_err());
        link(&mut graph, 1, 10, "{w: '1'}").unwrap();
        assert_eq!(has_count(&mut graph), 2);
    });
}

fn make_temporal(graph: &mut DirGraph) {
    use crate::graph::features::temporal::declarations::{declare, TemporalTarget};
    use crate::graph::features::temporal::IntervalConvention;
    declare(
        graph,
        &TemporalTarget::Relationship {
            rel_type: "HAS".into(),
            source_type: None,
        },
        "frm",
        "to",
        IntervalConvention::Closed,
    )
    .unwrap();
}

fn version(graph: &mut DirGraph, from: &str, to: &str) -> Result<(), Box<KgError>> {
    link(
        graph,
        1,
        10,
        &format!("{{frm: date('{from}'), to: date('{to}')}}"),
    )
}

#[test]
fn successive_versions_of_a_temporal_pair_are_not_duplicates() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        version(&mut graph, "2010-01-01", "2015-01-01").unwrap();
        make_temporal(&mut graph);
        declare(&mut graph, &spec("true", "advisory", ""));
        version(&mut graph, "2016-01-01", "2020-01-01").unwrap();
        version(&mut graph, "2021-01-01", "2022-01-01").unwrap();
        assert_eq!(distinct_line(&mut graph), (0, 0, 3));
        version(&mut graph, "2016-01-01", "2020-01-01").unwrap();
        assert_eq!(
            distinct_line(&mut graph),
            (1, 0, 4),
            "two edges with identical bounds are still duplicates"
        );
    });
}

#[test]
fn the_write_gate_accepts_new_versions_and_refuses_identical_bounds() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        version(&mut graph, "2010-01-01", "2015-01-01").unwrap();
        make_temporal(&mut graph);
        declare(&mut graph, &spec("true", "error", ""));
        version(&mut graph, "2016-01-01", "2020-01-01").unwrap();
        assert_distinct(*version(&mut graph, "2016-01-01", "2020-01-01").unwrap_err());
        assert_eq!(
            super::ontology_gate_tests::count(
                &mut graph,
                "FOR VALID_TIME ALL MATCH ()-[r:HAS]->() RETURN count(r)"
            ),
            2
        );
    });
}

#[test]
fn declaring_error_over_stored_temporal_versions_is_accepted() {
    in_every_mode(|mut graph| {
        seeded(&mut graph);
        version(&mut graph, "2010-01-01", "2015-01-01").unwrap();
        version(&mut graph, "2016-01-01", "2020-01-01").unwrap();
        make_temporal(&mut graph);
        declare(&mut graph, &spec("true", "error", ""));
    });
}
