//! The "must exist" rules (required relationship, minimum degree, inverse,
//! symmetric, stored transitive closure) are judged when a transaction ends:
//! satisfiable across statements, refused whole at commit, and judged per
//! statement or per call where the graph has no transaction around it.

use super::ontology_gate_tests::{count, declare, frame, in_every_mode, run};
use crate::datatypes::Value;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::checked;
use crate::graph::session::{CommitOutcome, Session};

fn works_at(extra: &str, severity: &str) -> String {
    format!(
        r#"{{"classes": {{"Person": {{}}, "Company": {{}}}},
        "relationships": {{"WORKS_AT": {{"domain": "Person", "range": "Company",
            {extra} "enforcement": "{severity}"}}}}}}"#
    )
}

fn required(severity: &str) -> String {
    works_at(r#""required": true,"#, severity)
}

fn people(graph: &mut DirGraph) -> i64 {
    count(graph, "MATCH (p:Person) RETURN count(p)")
}

fn edges(graph: &mut DirGraph, rel: &str) -> i64 {
    count(graph, &format!("MATCH ()-[r:{rel}]->() RETURN count(r)"))
}

/// The rule and message of a refused commit.
fn refused(outcome: CommitOutcome) -> (String, String) {
    match outcome {
        CommitOutcome::OntologyViolated { error } => match *error {
            KgError::OntologyViolation { rule, message, .. } => (rule.to_string(), message),
            other => panic!("expected OntologyViolation, got {other:?}"),
        },
        other => panic!("expected the commit to be refused, got {other:?}"),
    }
}

fn rule_of(error: KgError) -> String {
    match error {
        KgError::OntologyViolation { rule, .. } => rule.to_string(),
        other => panic!("expected OntologyViolation, got {other:?}"),
    }
}

fn committed(outcome: CommitOutcome) {
    assert!(
        matches!(outcome, CommitOutcome::Committed { .. }),
        "{outcome:?}"
    );
}

#[test]
fn the_gate_follows_the_declared_severities() {
    let mut graph = DirGraph::new();
    declare(&mut graph, &required("advisory"));
    assert!(!graph.ontology_tx_gate);
    declare(&mut graph, &required("warn"));
    assert!(graph.ontology_tx_gate);
    declare(&mut graph, &required("error"));
    assert!(graph.ontology_tx_gate);
    // `required` without a domain enrols nothing, exactly as the audit.
    declare(
        &mut graph,
        r#"{"relationships": {"WORKS_AT": {"required": true, "enforcement": "error"}}}"#,
    );
    assert!(!graph.ontology_tx_gate);
    declare(
        &mut graph,
        &works_at(r#""cardinality": {"min": 0},"#, "error"),
    );
    assert!(!graph.ontology_tx_gate, "a zero minimum demands nothing");
    declare(
        &mut graph,
        &works_at(r#""cardinality": {"min": 1},"#, "error"),
    );
    assert!(graph.ontology_tx_gate);
    graph.clear_ontology().unwrap();
    assert!(!graph.ontology_tx_gate);
}

#[test]
fn a_node_and_its_required_edge_in_later_statements_commit() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &required("error"));
        let session = Session::new(graph);
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(working, "CREATE (:Company {id: 7})").unwrap();
        run(working, "CREATE (:Person {id: 1})").unwrap();
        run(
            working,
            "MATCH (p:Person {id: 1}), (c:Company {id: 7}) CREATE (p)-[:WORKS_AT]->(c)",
        )
        .unwrap();
        committed(session.commit(tx, true));
        let mut published = session.snapshot().try_clone().unwrap();
        assert_eq!(people(&mut published), 1);
        assert_eq!(edges(&mut published, "WORKS_AT"), 1);
    });
}

#[test]
fn a_missing_required_edge_refuses_the_commit_and_rolls_the_transaction_back() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &required("error"));
        let session = Session::new(graph);
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(working, "CREATE (:Company {id: 7})").unwrap();
        run(working, "CREATE (:Person {id: 1})").unwrap();
        run(working, "CREATE (:Person {id: 2})").unwrap();
        let (rule, message) = refused(session.commit(tx, true));
        assert_eq!(rule, "required_relationship");
        assert!(message.contains("'WORKS_AT'"), "{message}");
        let mut published = session.snapshot().try_clone().unwrap();
        assert_eq!(
            people(&mut published),
            0,
            "nothing of the transaction landed"
        );
        assert_eq!(session.version(), 0);
    });
}

#[test]
fn an_auto_commit_statement_and_a_direct_statement_are_their_own_transaction() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &required("error"));
        run(&mut graph, "CREATE (:Company {id: 7})").unwrap();
        // The statement that creates both ends is complete.
        run(
            &mut graph,
            "CREATE (:Person {id: 1})-[:WORKS_AT]->(:Company {id: 8})",
        )
        .unwrap();
        // One that leaves a Person bare is refused, with nothing left behind,
        // including the single-node CREATE that otherwise skips the checkpoint.
        for statement in [
            "CREATE (:Person {id: 2})",
            "UNWIND [3, 4] AS i CREATE (:Person {id: i})",
        ] {
            let error = run(&mut graph, statement).unwrap_err();
            assert_eq!(rule_of(*error), "required_relationship");
            assert_eq!(people(&mut graph), 1, "{statement}");
        }
        let session = Session::new(graph);
        let version = session.version();
        let params = std::collections::HashMap::new();
        let opts = crate::graph::session::execute::ExecuteOptions::eager(&params);
        let error = session
            .execute_auto_commit("CREATE (:Person {id: 5})", &opts, 1)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(rule_of(error), "required_relationship");
        assert_eq!(session.version(), version);
        session
            .execute_auto_commit(
                "CREATE (:Person {id: 6})-[:WORKS_AT]->(:Company {id: 9})",
                &opts,
                1,
            )
            .unwrap();
    });
}

#[test]
fn deleting_the_only_required_edge_is_refused() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &required("error"));
        run(
            &mut graph,
            "CREATE (:Person {id: 1})-[:WORKS_AT]->(:Company {id: 7}), \
             (:Person {id: 1})-[:WORKS_AT]->(:Company {id: 8})",
        )
        .ok();
        let mut graph = graph;
        run(&mut graph, "MATCH (p:Person) DETACH DELETE p").unwrap();
        run(
            &mut graph,
            "CREATE (p:Person {id: 2})-[:WORKS_AT]->(:Company {id: 7}), \
             (p)-[:WORKS_AT]->(:Company {id: 8})",
        )
        .unwrap();
        // One of two edges may go; the last may not.
        run(
            &mut graph,
            "MATCH (:Person {id: 2})-[r:WORKS_AT]->(:Company {id: 7}) DELETE r",
        )
        .unwrap();
        let error = run(
            &mut graph,
            "MATCH (:Person {id: 2})-[r:WORKS_AT]->() DELETE r",
        )
        .unwrap_err();
        assert_eq!(rule_of(*error), "required_relationship");
        assert_eq!(
            edges(&mut graph, "WORKS_AT"),
            1,
            "the delete was rolled back"
        );
        // Detaching the company strands the person as well.
        let error = run(&mut graph, "MATCH (c:Company {id: 8}) DETACH DELETE c").unwrap_err();
        assert_eq!(rule_of(*error), "required_relationship");
        assert_eq!(edges(&mut graph, "WORKS_AT"), 1);
        // Deleting the person is how the requirement is lifted.
        run(&mut graph, "MATCH (p:Person {id: 2}) DETACH DELETE p").unwrap();
    });
}

#[test]
fn a_delete_and_a_replacement_edge_in_one_transaction_commit() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &required("error"));
        run(
            &mut graph,
            "CREATE (:Person {id: 1})-[:WORKS_AT]->(:Company {id: 7}), (:Company {id: 8})",
        )
        .unwrap();
        let session = Session::new(graph);
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(working, "MATCH ()-[r:WORKS_AT]->() DELETE r").unwrap();
        run(
            working,
            "MATCH (p:Person {id: 1}), (c:Company {id: 8}) CREATE (p)-[:WORKS_AT]->(c)",
        )
        .unwrap();
        committed(session.commit(tx, true));
    });
}

#[test]
fn bulk_add_nodes_is_its_own_transaction() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &required("error"));
        run(&mut graph, "CREATE (:Company {id: 7})").unwrap();
        let rows = frame(&["id"], vec![vec![Value::Int64(1)], vec![Value::Int64(2)]]);
        let message =
            checked::add_nodes(&mut graph, rows, "Person".into(), "id".into(), None, None)
                .unwrap_err();
        let typed = graph
            .take_constraint_error(&message)
            .expect("typed refusal");
        assert_eq!(rule_of(typed), "required_relationship");
        assert_eq!(people(&mut graph), 0, "the frame was rolled back");
        // Inside a transaction the same call is deferred to the commit.
        let session = Session::new(graph);
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        let rows = frame(&["id"], vec![vec![Value::Int64(1)]]);
        checked::add_nodes(working, rows, "Person".into(), "id".into(), None, None).unwrap();
        run(
            working,
            "MATCH (p:Person {id: 1}), (c:Company {id: 7}) CREATE (p)-[:WORKS_AT]->(c)",
        )
        .unwrap();
        committed(session.commit(tx, true));
    });
}

#[test]
fn warn_commits_and_reports() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &required("warn"));
        // A direct statement reports on its own result, and lands.
        let outcome =
            super::ontology_gate_tests::run_outcome(&mut graph, "CREATE (:Person {id: 1})")
                .unwrap();
        let warnings = outcome.result.diagnostics.unwrap().warnings;
        assert!(
            warnings.iter().any(|w| w
                .contains("ontology warning (required_relationship): 1 node without the required")),
            "{warnings:?}"
        );
        assert_eq!(people(&mut graph), 1, "the write landed");
        // A transaction reports once, at its end.
        let session = Session::new(graph);
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "CREATE (:Person {id: 2}), (:Person {id: 3})",
        )
        .unwrap();
        let (outcome, warnings) = session.commit_reporting(tx, true);
        committed(outcome);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains(
                "ontology warning (required_relationship): 2 nodes without the required outgoing"
            ),
            "{warnings:?}"
        );
    });
}

#[test]
fn a_minimum_degree_is_judged_at_commit() {
    in_every_mode(|mut graph| {
        declare(
            &mut graph,
            &works_at(r#""cardinality": {"min": 2},"#, "error"),
        );
        run(
            &mut graph,
            "CREATE (:Company {id: 7}), (:Company {id: 8}), (:Company {id: 9})",
        )
        .unwrap();
        let session = Session::new(graph);
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(working, "CREATE (:Person {id: 1})").unwrap();
        run(
            working,
            "MATCH (p:Person {id: 1}), (c:Company {id: 7}) CREATE (p)-[:WORKS_AT]->(c)",
        )
        .unwrap();
        let (rule, message) = refused(session.commit(tx, true));
        assert_eq!(rule, "min_cardinality");
        assert!(message.contains("holds 1 outgoing 'WORKS_AT'"), "{message}");
        assert!(message.contains("minimum of 2"), "{message}");

        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(working, "CREATE (:Person {id: 1})").unwrap();
        for company in [7, 8] {
            run(
                working,
                &format!(
                    "MATCH (p:Person {{id: 1}}), (c:Company {{id: {company}}}) \
                     CREATE (p)-[:WORKS_AT]->(c)"
                ),
            )
            .unwrap();
        }
        committed(session.commit(tx, true));
        // Dropping to one edge breaks it again.
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH (:Person {id: 1})-[r:WORKS_AT]->(:Company {id: 8}) DELETE r",
        )
        .unwrap();
        assert_eq!(refused(session.commit(tx, true)).0, "min_cardinality");
    });
}

fn pairs(rule: &str, severity: &str) -> String {
    format!(
        r#"{{"classes": {{"Node": {{}}}}, "relationships": {{
            "PARENT_OF": {{"inverse_name": "CHILD_OF", "inverse_enforced": {inv},
                "symmetric": false, "enforcement": "{severity}"}},
            "CHILD_OF": {{}},
            "KNOWS": {{"symmetric": {sym}, "enforcement": "{severity}"}}}}}}"#,
        inv = rule == "inverse",
        sym = rule == "symmetric",
    )
}

#[test]
fn inverse_pairs_created_in_two_statements_commit_and_one_side_cannot_go() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &pairs("inverse", "error"));
        run(&mut graph, "CREATE (:Node {id: 1}), (:Node {id: 2})").unwrap();
        let session = Session::new(graph);
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(
            working,
            "MATCH (a:Node {id: 1}), (b:Node {id: 2}) CREATE (a)-[:PARENT_OF]->(b)",
        )
        .unwrap();
        run(
            working,
            "MATCH (a:Node {id: 1}), (b:Node {id: 2}) CREATE (b)-[:CHILD_OF]->(a)",
        )
        .unwrap();
        committed(session.commit(tx, true));
        // Half a pair is refused.
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH (a:Node {id: 2}), (b:Node {id: 1}) CREATE (a)-[:PARENT_OF]->(b)",
        )
        .unwrap();
        let (rule, message) = refused(session.commit(tx, true));
        assert_eq!(rule, "inverse");
        assert!(message.contains("no inverse 'CHILD_OF'"), "{message}");
        // So is deleting the answering half of a stored pair. The rule runs one
        // way: the PARENT_OF may go on its own, the CHILD_OF that answers it
        // may not.
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH ()-[r:CHILD_OF]->() DELETE r",
        )
        .unwrap();
        assert_eq!(refused(session.commit(tx, true)).0, "inverse");
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH ()-[r:PARENT_OF]->() DELETE r",
        )
        .unwrap();
        committed(session.commit(tx, true));
        // Deleting both is fine.
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH ()-[r:PARENT_OF|CHILD_OF]->() DELETE r",
        )
        .unwrap();
        committed(session.commit(tx, true));
    });
}

#[test]
fn symmetric_pairs_commit_across_statements_and_one_side_cannot_go() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &pairs("symmetric", "error"));
        run(&mut graph, "CREATE (:Node {id: 1}), (:Node {id: 2})").unwrap();
        let session = Session::new(graph);
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(
            working,
            "MATCH (a:Node {id: 1}), (b:Node {id: 2}) CREATE (a)-[:KNOWS]->(b)",
        )
        .unwrap();
        run(
            working,
            "MATCH (a:Node {id: 1}), (b:Node {id: 2}) CREATE (b)-[:KNOWS]->(a)",
        )
        .unwrap();
        committed(session.commit(tx, true));
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH (:Node {id: 1})-[r:KNOWS]->(:Node {id: 2}) DELETE r",
        )
        .unwrap();
        assert_eq!(refused(session.commit(tx, true)).0, "symmetric");
        // A lone edge, and a self-loop (its own mirror).
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH (a:Node {id: 1}) CREATE (a)-[:KNOWS]->(a)",
        )
        .unwrap();
        committed(session.commit(tx, true));
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "CREATE (:Node {id: 3})-[:KNOWS]->(:Node {id: 4})",
        )
        .unwrap();
        assert_eq!(refused(session.commit(tx, true)).0, "symmetric");
    });
}

fn closure(severity: &str) -> String {
    format!(
        r#"{{"classes": {{"Node": {{}}}}, "relationships": {{
            "BELOW": {{"transitive": true, "enforcement": "{severity}"}}}}}}"#
    )
}

#[test]
fn a_stored_closure_must_hold_every_two_hop_chain() {
    in_every_mode(|mut graph| {
        declare(&mut graph, &closure("error"));
        run(
            &mut graph,
            "CREATE (:Node {id: 1}), (:Node {id: 2}), (:Node {id: 3}), (:Node {id: 4})",
        )
        .unwrap();
        let link = |from: i64, to: i64| {
            format!("MATCH (a:Node {{id: {from}}}), (b:Node {{id: {to}}}) CREATE (a)-[:BELOW]->(b)")
        };
        let session = Session::new(graph);
        // 1 -> 2 -> 3 without 1 -> 3.
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(working, &link(1, 2)).unwrap();
        run(working, &link(2, 3)).unwrap();
        let (rule, message) = refused(session.commit(tx, true));
        assert_eq!(rule, "transitive");
        assert!(message.contains("'BELOW'"), "{message}");
        // Closed in a later statement of the same transaction.
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        run(working, &link(1, 2)).unwrap();
        run(working, &link(2, 3)).unwrap();
        run(working, &link(1, 3)).unwrap();
        committed(session.commit(tx, true));
        // Extending the chain at the far end, and at the near end, both open one.
        for extension in [link(3, 4), link(4, 1)] {
            let mut tx = session.begin();
            run(tx.working_mut().unwrap(), &extension).unwrap();
            assert_eq!(
                refused(session.commit(tx, true)).0,
                "transitive",
                "{extension}"
            );
        }
        // The direct edge cannot be removed while the chain stands.
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH (:Node {id: 1})-[r:BELOW]->(:Node {id: 3}) DELETE r",
        )
        .unwrap();
        assert_eq!(refused(session.commit(tx, true)).0, "transitive");
        // Removing the chain's middle link and the direct edge together is fine.
        let mut tx = session.begin();
        run(
            tx.working_mut().unwrap(),
            "MATCH (:Node {id: 2})-[r:BELOW]->(:Node {id: 3}) DELETE r",
        )
        .unwrap();
        run(
            tx.working_mut().unwrap(),
            "MATCH (:Node {id: 1})-[r:BELOW]->(:Node {id: 3}) DELETE r",
        )
        .unwrap();
        committed(session.commit(tx, true));
    });
}

#[test]
fn a_transaction_without_must_exist_rules_pays_nothing() {
    let mut graph = DirGraph::new();
    declare(
        &mut graph,
        r#"{"classes": {"Person": {"required_properties": ["name"]}}}"#,
    );
    assert!(!graph.ontology_tx_gate);
    run(&mut graph, "CREATE (:Person {id: 1, name: 'a'})").unwrap();
    assert!(graph.ontology_tx.is_empty(), "nothing is logged while off");
}

fn declared_over(setup: &str, json: &str) -> Result<(), super::violation::DefineOntologyError> {
    let mut graph = DirGraph::new();
    run(&mut graph, setup).unwrap();
    graph
        .define_ontology(super::ontology_from_json(json).unwrap())
        .map(|_| ())
}

fn refusal_of(
    outcome: Result<(), super::violation::DefineOntologyError>,
) -> Vec<(super::violation::OntologyRule, String, u64)> {
    match outcome {
        Err(super::violation::DefineOntologyError::Refused(refused)) => refused
            .entries
            .into_iter()
            .map(|e| (e.rule, e.entity_type, e.count))
            .collect(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn declaring_an_error_rule_over_violating_data_is_refused() {
    use super::violation::OntologyRule as Rule;
    let people = "CREATE (:Person {id: 1}), (:Person {id: 2}), (:Company {id: 7})";
    assert_eq!(
        refusal_of(declared_over(people, &required("error"))),
        [(Rule::RequiredRelationship, "WORKS_AT".to_string(), 2)]
    );
    declared_over(people, &required("warn")).unwrap();
    declared_over(people, &required("advisory")).unwrap();
    assert_eq!(
        refusal_of(declared_over(
            people,
            &works_at(r#""cardinality": {"min": 1},"#, "error")
        )),
        [(Rule::MinCardinality, "WORKS_AT".to_string(), 2)]
    );

    let pair =
        "CREATE (a:Node {id: 1}), (b:Node {id: 2}), (a)-[:PARENT_OF]->(b), (a)-[:KNOWS]->(b)";
    let rules = refusal_of(declared_over(pair, &pairs("inverse", "error")));
    assert_eq!(rules, [(Rule::Inverse, "PARENT_OF".to_string(), 1)]);
    assert_eq!(
        refusal_of(declared_over(pair, &pairs("symmetric", "error"))),
        [(Rule::Symmetric, "KNOWS".to_string(), 1)]
    );
    assert_eq!(
        refusal_of(declared_over(
            "CREATE (a:Node {id: 1})-[:BELOW]->(:Node {id: 2})-[:BELOW]->(:Node {id: 3})",
            &closure("error")
        )),
        [(Rule::Transitive, "BELOW".to_string(), 1)]
    );
    // Satisfied data is accepted at error.
    declared_over(
        "CREATE (a:Node {id: 1})-[:BELOW]->(b:Node {id: 2}), (b)-[:BELOW]->(c:Node {id: 3}), \
         (a)-[:BELOW]->(c)",
        &closure("error"),
    )
    .unwrap();
}

fn audit_violations(graph: &mut DirGraph, rule: &str) -> i64 {
    count(
        graph,
        &format!(
            "CALL ontology_audit() YIELD rule, violations WHERE rule = '{rule}' \
             RETURN violations"
        ),
    )
}

#[test]
fn the_audit_counts_must_exist_violations_when_the_relationship_type_is_absent() {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        "CREATE (:Person {id: 1}), (:Person {id: 2}), (:Company {id: 7})",
    )
    .unwrap();
    declare(
        &mut graph,
        &works_at(r#""required": true, "cardinality": {"min": 1},"#, "warn"),
    );
    assert_eq!(edges(&mut graph, "WORKS_AT"), 0);
    assert_eq!(audit_violations(&mut graph, "WORKS_AT.required"), 2);
    assert_eq!(audit_violations(&mut graph, "WORKS_AT.cardinality"), 2);

    // A maximum is trivially met by zero edges.
    let mut graph = DirGraph::new();
    run(&mut graph, "CREATE (:Person {id: 1})").unwrap();
    declare(
        &mut graph,
        &works_at(r#""cardinality": {"max": 1},"#, "warn"),
    );
    assert_eq!(audit_violations(&mut graph, "WORKS_AT.cardinality"), 0);

    // The gate agrees: with no edge ever written, a Person is refused.
    let mut graph = DirGraph::new();
    declare(&mut graph, &required("error"));
    assert_eq!(
        rule_of(*run(&mut graph, "CREATE (:Person {id: 1})").unwrap_err()),
        "required_relationship"
    );
}
