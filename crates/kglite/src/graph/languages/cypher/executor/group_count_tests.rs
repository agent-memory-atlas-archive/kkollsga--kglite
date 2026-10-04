//! The grouped-count histogram: when a fused grouped count builds one, that a
//! repeat reads it, and that it answers exactly what the per-group walk does
//! — at several instants, with no filter, and across a write.

use super::group_count::peer_hist_probe;
use crate::datatypes::values::Value;
use crate::graph::features::temporal::peer_hist::set_test_build_after_ns;
use crate::graph::schema::DirGraph;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;

fn write(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

/// Teams and members; the team `Core` is open-ended, `Pilot` ends in 2010 and
/// the membership `ann -> Core` ends in 2008. `Contractor` is a second label
/// carried by one person.
fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (:Team {id: 1, name: 'Core', vf: '2000-01-01', vt: null}), \
         (:Team {id: 2, name: 'Pilot', vf: '2000-01-01', vt: '2010-01-01'}), \
         (:Person {id: 1, name: 'ann', vf: '2000-01-01', vt: null}), \
         (:Person {id: 2, name: 'bob', vf: '2000-01-01', vt: '2012-01-01'}), \
         (:Person {id: 3, name: 'cy', vf: '2000-01-01', vt: null})",
        "MATCH (p:Person {id: 1}), (t:Team {id: 1}) \
         CREATE (p)-[:MEMBER_OF {vf: '2000-01-01', vt: '2008-01-01'}]->(t)",
        "MATCH (p:Person), (t:Team) WHERE p.id IN [2, 3] AND t.id = 1 \
         CREATE (p)-[:MEMBER_OF {vf: '2000-01-01', vt: null}]->(t)",
        "MATCH (p:Person {id: 2}), (t:Team {id: 2}) \
         CREATE (p)-[:MEMBER_OF {vf: '2000-01-01', vt: null}]->(t), \
         (p)-[:MEMBER_OF {vf: '2000-01-01', vt: null}]->(t)",
        "MATCH (p:Person {id: 3}) SET p:Contractor",
        "CALL db.temporal.declare({node: 'Team', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        "CALL db.temporal.declare({node: 'Person', from: 'vf', to: 'vt', convention: 'closed'}) \
         YIELD declared RETURN declared",
        "CALL db.temporal.declare({relationship: 'MEMBER_OF', from: 'vf', to: 'vt', \
         convention: 'half_open'}) YIELD declared RETURN declared",
    ] {
        write(&mut graph, query);
    }
    graph
}

const QUERIES: [&str; 3] = [
    "MATCH (p:Person)-[:MEMBER_OF]->(t:Team) RETURN t.name AS n, count(p) AS c ORDER BY n",
    "MATCH (t:Team) OPTIONAL MATCH (t)<-[:MEMBER_OF]-(p:Contractor) RETURN t.name AS n, count(p) AS c ORDER BY n",
    "MATCH (p:Person)-[:MEMBER_OF]->(t:Team) WITH t, count(p) AS c RETURN t.name AS n, c ORDER BY n",
];

const CONTEXTS: [&str; 4] = [
    "FOR VALID_TIME ALL ",
    "FOR VALID_TIME AS OF date('2006-01-01') ",
    "FOR VALID_TIME AS OF date('2009-01-01') ",
    "FOR VALID_TIME AS OF date('2011-01-01') ",
];

fn rows(graph: &DirGraph, query: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
}

/// Answers and `(builds, served)` of every query under every context at
/// the build threshold `after_ns`.
fn run_all(graph: &DirGraph, after_ns: u64) -> (Vec<Vec<Vec<Value>>>, (usize, usize)) {
    set_test_build_after_ns(Some(after_ns));
    peer_hist_probe::take();
    let mut answers = Vec::new();
    for context in CONTEXTS {
        for query in QUERIES {
            answers.push(rows(graph, &format!("{context}{query}")));
        }
    }
    let probe = peer_hist_probe::take();
    set_test_build_after_ns(None);
    (answers, probe)
}

#[test]
fn a_histogram_answers_what_the_walk_does_and_is_built_once_per_key() {
    let graph = graph();
    let (walked, probe) = run_all(&graph, u64::MAX);
    assert_eq!(probe, (0, 0), "a threshold never reached never builds");

    let (served, (built, reads)) = run_all(&graph, 0);
    assert_eq!(served, walked);
    assert!(built > 0 && reads > built, "built {built}, read {reads}");

    let (again, (rebuilt, reads)) = run_all(&graph, 0);
    assert_eq!(again, walked);
    assert_eq!(rebuilt, 0, "a repeat reads what is cached");
    assert!(reads > 0);
}

#[test]
fn a_write_between_two_counts_is_never_answered_from_the_old_histogram() {
    let mut graph = graph();
    let query = format!("{}{}", CONTEXTS[0], QUERIES[2]);
    set_test_build_after_ns(Some(0));
    let before = rows(&graph, &query);
    assert_eq!(rows(&graph, &query), before);
    write(
        &mut graph,
        "MATCH (p:Person {id: 1}), (t:Team {id: 2}) \
         CREATE (p)-[:MEMBER_OF {vf: '2000-01-01', vt: null}]->(t)",
    );
    peer_hist_probe::take();
    let after = rows(&graph, &query);
    let (built, _) = peer_hist_probe::take();
    set_test_build_after_ns(None);
    assert_eq!(built, 1, "the write moved the version");
    assert_ne!(after, before);
    let pilot = |rs: &[Vec<Value>]| rs[1][1].clone();
    assert_eq!(pilot(&before), Value::Int64(2));
    assert_eq!(pilot(&after), Value::Int64(3));
}

#[test]
fn shapes_the_histogram_cannot_answer_keep_walking() {
    let graph = graph();
    set_test_build_after_ns(Some(0));
    for body in [
        // distinct peers, several relationship types, a peer property filter
        // and a relationship property filter are all answered by the walk.
        "MATCH (p:Person)-[:MEMBER_OF]->(t:Team) WITH t, count(DISTINCT p) AS c \
         RETURN t.name AS n, c ORDER BY n",
        "MATCH (t:Team) OPTIONAL MATCH (t)<-[:MEMBER_OF|LEADS]-(p:Person) \
         RETURN t.name AS n, count(p) AS c ORDER BY n",
        "MATCH (t:Team) OPTIONAL MATCH (t)<-[:MEMBER_OF]-(p:Person {name: 'ann'}) \
         RETURN t.name AS n, count(p) AS c ORDER BY n",
        "MATCH (t:Team) OPTIONAL MATCH (t)<-[r:MEMBER_OF]-(p:Person) WHERE r.vf = '2000-01-01' \
         RETURN t.name AS n, count(p) AS c ORDER BY n",
    ] {
        for context in CONTEXTS {
            peer_hist_probe::take();
            rows(&graph, &format!("{context}{body}"));
            assert_eq!(peer_hist_probe::take(), (0, 0), "{context}{body}");
        }
    }
    set_test_build_after_ns(None);
}

#[test]
fn each_peer_label_has_its_own_histogram() {
    let graph = graph();
    set_test_build_after_ns(Some(0));
    let ask = |body: &str| {
        peer_hist_probe::take();
        let answer = rows(&graph, &format!("{}{body}", CONTEXTS[0]));
        (answer, peer_hist_probe::take().0)
    };
    let people = "MATCH (t:Team) OPTIONAL MATCH (t)<-[:MEMBER_OF]-(p:Person) \
                  RETURN t.name AS n, count(p) AS c ORDER BY n";
    let contractors = "MATCH (t:Team) OPTIONAL MATCH (t)<-[:MEMBER_OF]-(p:Contractor) \
                       RETURN t.name AS n, count(p) AS c ORDER BY n";
    let (all_people, built) = ask(people);
    assert_eq!(built, 1);
    let (some, built) = ask(contractors);
    assert_eq!(built, 1, "a second label needs its own histogram");
    assert_ne!(all_people, some);
    assert_eq!(ask(people), (all_people, 0));
    assert_eq!(ask(contractors), (some, 0));
    set_test_build_after_ns(None);
}
