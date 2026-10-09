//! The node write gate: an ontology rule declared at `error` refuses the
//! statement or call that would break it and leaves the graph as it was; at
//! `warn` the write lands with a warning; at `advisory` nothing changes.
//!
//! Each Cypher shape runs against memory, mapped and disk storage.

use std::collections::HashMap;

use crate::datatypes::{DataFrame, Value};
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::maintain::add_nodes;
use crate::graph::ontology::ontology_from_json;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

const MODES: [StorageMode; 3] = [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk];

/// Run `check` on a fresh graph of every storage mode.
pub(super) fn in_every_mode(check: impl Fn(DirGraph)) {
    for mode in MODES {
        let dir = tempfile::tempdir().expect("tempdir");
        let graph = new_dir_graph_in_mode(mode, Some(dir.path())).expect("graph");
        check(graph);
    }
}

pub(super) fn run_outcome(
    graph: &mut DirGraph,
    query: &str,
) -> Result<crate::graph::session::execute::ExecuteOutcome, Box<KgError>> {
    let params: HashMap<String, Value> = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params)).map_err(Box::new)
}

pub(super) fn run(graph: &mut DirGraph, query: &str) -> Result<(), Box<KgError>> {
    run_outcome(graph, query).map(|_| ())
}

pub(super) fn count(graph: &mut DirGraph, query: &str) -> i64 {
    let outcome = run_outcome(graph, query).unwrap();
    match outcome.result.rows[0][0] {
        Value::Int64(n) => n,
        ref other => panic!("not a count: {other:?}"),
    }
}

fn people(graph: &mut DirGraph) -> i64 {
    count(graph, "MATCH (n:Person) RETURN count(n)")
}

pub(super) fn declare(graph: &mut DirGraph, json: &str) {
    let store = ontology_from_json(json).unwrap();
    graph.define_ontology(store).expect("declaration accepted");
}

/// `Person` requires `email` and a numeric `age`; `Student` inherits both.
fn person_ontology(severity: &str) -> String {
    format!(
        r#"{{"classes": {{
            "Person": {{"required_properties": ["email"],
                        "property_types": {{"age": "integer"}},
                        "enforcement": "{severity}"}},
            "Student": {{"is_a": "Person", "enforcement": "{severity}"}}
        }}}}"#
    )
}

fn enforced(graph: &mut DirGraph, severity: &str) {
    declare(graph, &person_ontology(severity));
    // `Person` is the live type the rest of the test writes into.
    run(graph, "CREATE (:Person {id: 0, email: 'seed', age: 1})").unwrap();
}

/// The ontology violation a refused statement carries.
fn assert_violation(error: KgError, rule: &str, property: Option<&str>) -> String {
    match error {
        KgError::OntologyViolation {
            rule: r,
            entity,
            property: p,
            message,
            ..
        } => {
            assert_eq!(r, rule, "{message}");
            assert_eq!(entity, "node");
            assert_eq!(p.as_deref(), property, "{message}");
            message
        }
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

/// [`assert_violation`] for a loader, whose `String` error is paired with the
/// typed violation parked on the graph.
fn assert_loader_violation(
    graph: &mut DirGraph,
    message: &str,
    rule: &str,
    property: Option<&str>,
) {
    let typed = graph
        .take_constraint_error(message)
        .expect("the refusal parked a typed violation");
    assert_violation(typed, rule, property);
}

#[test]
fn the_gate_follows_the_declared_severities() {
    let mut graph = DirGraph::new();
    assert!(!graph.ontology_node_gate);
    declare(&mut graph, &person_ontology("advisory"));
    assert!(!graph.ontology_node_gate, "advisory enforces nothing");
    declare(&mut graph, &person_ontology("warn"));
    assert!(graph.ontology_node_gate);
    declare(&mut graph, &person_ontology("error"));
    assert!(graph.ontology_node_gate);
    graph.clear_ontology().unwrap();
    assert!(!graph.ontology_node_gate);
    // A relationship-only ontology binds no node.
    declare(
        &mut graph,
        r#"{"classes": {"A": {}}, "relationships": {"R": {"domain": "A", "enforcement": "error"}}}"#,
    );
    assert!(!graph.ontology_node_gate);
}

#[test]
fn create_without_a_required_property_is_refused_and_leaves_nothing() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        // The single-node CREATE shape that used to skip the rollback
        // checkpoint: the refusal comes after the insert, so it needs one.
        let error = run(&mut graph, "CREATE (:Person {id: 1})").unwrap_err();
        let message = assert_violation(*error, "required_property", Some("email"));
        assert!(message.contains("'email'"), "{message}");
        assert_eq!(people(&mut graph), 1, "only the seed remains");
    });
}

#[test]
fn a_later_set_in_the_same_statement_repairs_an_earlier_create() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        run(&mut graph, "CREATE (p:Person {id: 1}) SET p.email = 'A'").unwrap();
        run(
            &mut graph,
            "MERGE (p:Person {id: 2}) ON CREATE SET p.email = 'B'",
        )
        .unwrap();
        assert_eq!(people(&mut graph), 3);
        let message = run(&mut graph, "MERGE (p:Person {id: 3})").unwrap_err();
        assert_violation(*message, "required_property", Some("email"));
        assert_eq!(people(&mut graph), 3);
    });
}

#[test]
fn a_late_violation_rolls_back_the_whole_statement() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        let message = run(
            &mut graph,
            "UNWIND [1, 2, 3] AS i CREATE (:Person {id: i, email: CASE WHEN i = 3 THEN null ELSE 'ok' END})",
        )
        .unwrap_err();
        assert_violation(*message, "required_property", Some("email"));
        assert_eq!(people(&mut graph), 1, "rows 1 and 2 were rolled back too");
        // And the graph is still usable.
        run(&mut graph, "CREATE (:Person {id: 9, email: 'fine'})").unwrap();
        assert_eq!(people(&mut graph), 2);
    });
}

#[test]
fn set_to_a_wrong_type_is_refused_and_the_value_is_restored() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        let message = run(&mut graph, "MATCH (p:Person {id: 0}) SET p.age = 'old'").unwrap_err();
        assert_violation(*message, "property_type", Some("age"));
        assert_eq!(
            count(&mut graph, "MATCH (p:Person {id: 0}) RETURN p.age"),
            1,
            "the refused SET left the stored value"
        );
        run(&mut graph, "MATCH (p:Person {id: 0}) SET p.age = 41").unwrap();
    });
}

#[test]
fn remove_of_a_required_property_is_refused() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        let message = run(&mut graph, "MATCH (p:Person {id: 0}) REMOVE p.email").unwrap_err();
        assert_violation(*message, "required_property", Some("email"));
        assert_eq!(
            count(
                &mut graph,
                "MATCH (p:Person {id: 0}) WHERE p.email = 'seed' RETURN count(p)"
            ),
            1
        );
        let message = run(&mut graph, "MATCH (p:Person {id: 0}) SET p.email = null").unwrap_err();
        assert_violation(*message, "required_property", Some("email"));
    });
}

#[test]
fn a_declared_ancestor_binds_its_descendants() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        let message = run(&mut graph, "CREATE (:Student {id: 5})").unwrap_err();
        assert_violation(*message, "required_property", Some("email"));
        run(&mut graph, "CREATE (:Student {id: 5, email: 'S'})").unwrap();
    });
}

#[test]
fn closed_labels_refuse_an_undeclared_primary_label_only() {
    in_every_mode(|mut graph| {
        declare(
            &mut graph,
            r#"{"closed_labels": true, "enforcement": "error",
                "classes": {"Person": {}}}"#,
        );
        let message = run(&mut graph, "CREATE (:Ghost {id: 1})").unwrap_err();
        assert_violation(*message, "closed_labels", None);
        // Secondary labels are never judged (D3).
        run(&mut graph, "CREATE (p:Person:Ghost {id: 1})").unwrap();
        run(&mut graph, "MATCH (p:Person) SET p:Other").unwrap();
        run(&mut graph, "MATCH (p:Person) REMOVE p:Other").unwrap();
        assert_eq!(count(&mut graph, "MATCH (n) RETURN count(n)"), 1);
    });
}

#[test]
fn warn_lets_the_write_land_and_reports_it() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "warn");
        let outcome =
            run_outcome(&mut graph, "UNWIND [1, 2] AS i CREATE (:Person {id: i})").unwrap();
        let warnings = outcome.result.diagnostics.unwrap().warnings;
        let ontology: Vec<_> = warnings
            .iter()
            .filter(|w| w.contains("ontology warning (required_property)"))
            .collect();
        assert_eq!(ontology.len(), 1, "one line per rule: {warnings:?}");
        assert!(ontology[0].contains("2 nodes"), "{ontology:?}");
        assert!(ontology[0].contains("'email'"), "{ontology:?}");
        assert_eq!(people(&mut graph), 3, "the nodes were written");
    });
}

#[test]
fn an_advisory_ontology_changes_nothing() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &person_ontology("advisory"));
        let outcome = run_outcome(&mut graph, "CREATE (:Person {id: 1, age: 'x'})").unwrap();
        assert!(outcome
            .result
            .diagnostics
            .map(|d| d.warnings)
            .unwrap_or_default()
            .iter()
            .all(|w| !w.contains("ontology")));
        assert_eq!(people(&mut graph), 1);
    });
}

#[test]
fn a_violating_node_is_judged_when_touched_not_before() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:Person {id: 1})").unwrap();
    // Declared at warn over a violating node: accepted, nothing refused later
    // for writes that do not touch it.
    declare(&mut graph, &person_ontology("warn"));
    run(&mut graph, "CREATE (:Person {id: 2, email: 'ok'})").unwrap();
    // Raising it to error is refused by the declaration check (B3), so lower
    // the other way: an untouched violator never blocks an unrelated write.
    declare(&mut graph, &person_ontology("advisory"));
    run(&mut graph, "CREATE (:Person {id: 3, email: 'ok'})").unwrap();
}

#[test]
fn off_state_records_no_touched_nodes() {
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (p:Person {id: 2}) SET p.x = 1").unwrap();
    assert!(graph.ontology_touched.is_empty());
    assert!(!graph.ontology_node_gate);
}

pub(super) fn frame(columns: &[&str], rows: Vec<Vec<Value>>) -> DataFrame {
    let columns = columns.iter().map(|c| c.to_string()).collect();
    DataFrame::from_cypher_rows(columns, rows).unwrap()
}

#[test]
fn add_nodes_refuses_the_whole_frame_before_writing() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        let rows = frame(
            &["id", "email"],
            vec![
                vec![Value::Int64(1), Value::String("a".into())],
                vec![Value::Int64(2), Value::Null],
            ],
        );
        let message =
            add_nodes(&mut graph, rows, "Person".into(), "id".into(), None, None).unwrap_err();
        assert_loader_violation(&mut graph, &message, "required_property", Some("email"));
        assert_eq!(people(&mut graph), 1, "row 1 was not written either");
        let rows = frame(
            &["id", "email"],
            vec![vec![Value::Int64(1), Value::String("a".into())]],
        );
        add_nodes(&mut graph, rows, "Person".into(), "id".into(), None, None).unwrap();
        assert_eq!(people(&mut graph), 2);
    });
}

fn email_frame(rows: &[(i64, Option<&str>)]) -> DataFrame {
    frame(
        &["id", "email"],
        rows.iter()
            .map(|(id, email)| {
                vec![
                    Value::Int64(*id),
                    email.map_or(Value::Null, |e| Value::String(e.into())),
                ]
            })
            .collect(),
    )
}

#[test]
fn add_nodes_judges_the_node_each_conflict_mode_leaves() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        // The seed (id 0) holds an email; a frame without the column leaves
        // it in place under update, so the update is admitted.
        let no_email = frame(&["id", "age"], vec![vec![Value::Int64(0), Value::Int64(5)]]);
        add_nodes(
            &mut graph,
            no_email,
            "Person".into(),
            "id".into(),
            None,
            None,
        )
        .unwrap();
        // Replace overwrites the whole node: the email is gone, and refused.
        let no_email = frame(&["id", "age"], vec![vec![Value::Int64(0), Value::Int64(5)]]);
        let message = add_nodes(
            &mut graph,
            no_email,
            "Person".into(),
            "id".into(),
            None,
            Some("replace".into()),
        )
        .unwrap_err();
        assert_loader_violation(&mut graph, &message, "required_property", Some("email"));
        // Skip leaves the existing node unwritten, so it is unjudged.
        let no_email = frame(&["id", "age"], vec![vec![Value::Int64(0), Value::Int64(5)]]);
        add_nodes(
            &mut graph,
            no_email,
            "Person".into(),
            "id".into(),
            None,
            Some("skip".into()),
        )
        .unwrap();
        // A wrong type is refused under every mode that writes it.
        let bad = frame(
            &["id", "age"],
            vec![vec![Value::Int64(0), Value::String("old".into())]],
        );
        let message =
            add_nodes(&mut graph, bad, "Person".into(), "id".into(), None, None).unwrap_err();
        assert_loader_violation(&mut graph, &message, "property_type", Some("age"));
    });
}

#[test]
fn add_nodes_at_warn_reports_and_writes() {
    in_every_mode(|mut graph| {
        enforced(&mut graph, "warn");
        let report = add_nodes(
            &mut graph,
            email_frame(&[(1, None), (2, None), (3, Some("c"))]),
            "Person".into(),
            "id".into(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(report.nodes_created, 3);
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("2 nodes") && w.contains("'email'")),
            "{:?}",
            report.warnings
        );
        assert_eq!(people(&mut graph), 4);
    });
}

#[test]
fn add_nodes_into_an_undeclared_type_is_refused_when_labels_are_closed() {
    in_every_mode(|mut graph| {
        declare(
            &mut graph,
            r#"{"closed_labels": true, "enforcement": "error", "classes": {"Person": {}}}"#,
        );
        let message = add_nodes(
            &mut graph,
            email_frame(&[(1, Some("a"))]),
            "Ghost".into(),
            "id".into(),
            None,
            None,
        )
        .unwrap_err();
        assert_loader_violation(&mut graph, &message, "closed_labels", None);
        assert_eq!(count(&mut graph, "MATCH (n) RETURN count(n)"), 0);
    });
}

#[test]
fn connection_stubs_are_deferred_and_promotion_is_judged() {
    use crate::graph::mutation::maintain::add_connections;
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        // Both endpoints are auto-vivified stubs carrying only their id.
        let edges = frame(&["s", "t"], vec![vec![Value::Int64(10), Value::Int64(11)]]);
        add_connections(
            &mut graph,
            edges,
            "KNOWS".into(),
            "Person".into(),
            "s".into(),
            "Person".into(),
            "t".into(),
            None,
            None,
            None,
        )
        .expect("stubs defer the required property");
        assert_eq!(people(&mut graph), 3);
        // The row that promotes a stub is an ordinary write, judged in full.
        let message = add_nodes(
            &mut graph,
            email_frame(&[(10, None)]),
            "Person".into(),
            "id".into(),
            None,
            None,
        )
        .unwrap_err();
        assert_loader_violation(&mut graph, &message, "required_property", Some("email"));
        add_nodes(
            &mut graph,
            email_frame(&[(10, Some("x"))]),
            "Person".into(),
            "id".into(),
            None,
            None,
        )
        .unwrap();
    });
}

#[test]
fn update_writers_judge_the_node_the_write_leaves() {
    use crate::graph::mutation::property_updates::{
        update_node_properties, update_node_property_set,
    };
    in_every_mode(|mut graph| {
        enforced(&mut graph, "error");
        let seed = graph.lookup_by_id("Person", &Value::Int64(0)).unwrap();
        let message = update_node_properties(
            &mut graph,
            &[(Some(seed), Value::String("old".into()))],
            "age",
        )
        .unwrap_err();
        assert_loader_violation(&mut graph, &message, "property_type", Some("age"));
        let message =
            update_node_properties(&mut graph, &[(Some(seed), Value::Null)], "email").unwrap_err();
        assert_loader_violation(&mut graph, &message, "required_property", Some("email"));
        // The set form judges the end state: both writes together are legal.
        update_node_property_set(
            &mut graph,
            &[seed],
            &[
                ("email".to_string(), Value::String("z".into())),
                ("age".to_string(), Value::Int64(9)),
            ],
        )
        .unwrap();
        assert_eq!(
            count(&mut graph, "MATCH (p:Person {id: 0}) RETURN p.age"),
            9
        );
    });
}

#[test]
fn extend_refuses_before_any_group_lands() {
    use crate::graph::mutation::extend::extend_graph;
    let mut target = DirGraph::new();
    enforced(&mut target, "error");
    let mut source = DirGraph::new();
    run(
        &mut source,
        "CREATE (:Person {id: 5, email: 'a'}), (:Person {id: 6})",
    )
    .unwrap();
    let before = people(&mut target);
    let message = extend_graph(&mut target, &source, None).unwrap_err();
    assert_loader_violation(&mut target, &message, "required_property", Some("email"));
    assert_eq!(people(&mut target), before, "nothing was merged");
}

#[test]
fn a_minted_title_does_not_satisfy_a_required_name() {
    in_every_mode(|mut graph| {
        declare(
            &mut graph,
            r#"{"classes": {"Person": {"required_properties": ["name"],
                                       "enforcement": "error"}}}"#,
        );
        run(&mut graph, "CREATE (:Person {name: 'Ada', age: 1})").unwrap();
        let error = run(&mut graph, "CREATE (:Person {age: 3})").unwrap_err();
        assert_violation(*error, "required_property", Some("name"));
        let error = run(&mut graph, "MATCH (n:Person) REMOVE n.name").unwrap_err();
        assert_violation(*error, "required_property", Some("name"));
        assert_eq!(people(&mut graph), 1);
    });
}
