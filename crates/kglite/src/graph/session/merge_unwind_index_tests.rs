//! `UNWIND … MERGE` against an indexed label: a miss is a point lookup, not a
//! walk of the whole index. Inside a transaction the index is layered over the
//! published one, and after the statement's first `CREATE` every later row's
//! lookup used to merge every level just to rule out a temporal probe.

use super::{execute_mut, execute_read, CommitOutcome, ExecuteOptions, Session};
use crate::datatypes::Value;
use crate::graph::dir_graph::index_layer::merged_iters;
use crate::graph::dir_graph::DirGraph;
use std::collections::HashMap;

fn commit(session: &Session, query: &str) {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut tx = session.begin();
    execute_mut(tx.working_mut().unwrap(), query, &opts).unwrap();
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
}

/// Run `query` in a transaction on a published graph and report how many
/// merging index walks it caused.
fn merged_walks(session: &Session, query: &str) -> usize {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut tx = session.begin();
    let before = merged_iters();
    execute_mut(tx.working_mut().unwrap(), query, &opts).unwrap();
    let walks = merged_iters() - before;
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
    walks
}

fn count(session: &Session, query: &str) -> Value {
    let params = HashMap::new();
    let snapshot = session.snapshot();
    execute_read(&snapshot, query, &ExecuteOptions::eager(&params))
        .unwrap()
        .result
        .rows[0][0]
        .clone()
}

fn seeded(index_ddl: &str) -> Session {
    let session = Session::new(DirGraph::new());
    commit(&session, index_ddl);
    commit(
        &session,
        "UNWIND range(1, 200) AS i CREATE (:Repro {k: 'L' + toString(i), a: i, b: 'x'})",
    );
    session
}

#[test]
fn merge_misses_in_unwind_do_not_walk_the_index() {
    let session = seeded("CREATE INDEX FOR (n:Repro) ON (n.k)");
    let walks = merged_walks(
        &session,
        "UNWIND range(1, 40) AS i MERGE (:Repro {k: 'M' + toString(i)})",
    );
    assert_eq!(walks, 0, "each miss must be one hash probe");
    assert_eq!(
        count(&session, "MATCH (n:Repro) RETURN count(n)"),
        Value::Int64(240)
    );
}

#[test]
fn composite_merge_misses_in_unwind_do_not_walk_the_index() {
    let session = seeded("CREATE INDEX FOR (n:Repro) ON (n.a, n.b)");
    let walks = merged_walks(
        &session,
        "UNWIND range(1000, 1040) AS i MERGE (:Repro {a: i, b: 'y'})",
    );
    assert_eq!(walks, 0);
    assert_eq!(
        count(&session, "MATCH (n:Repro) RETURN count(n)"),
        Value::Int64(241)
    );
}

/// Goldens for the answers the lookup gives, matched and created alike.
#[test]
fn unwind_merge_matches_existing_keys_and_creates_each_new_key_once() {
    let session = seeded("CREATE INDEX FOR (n:Repro) ON (n.k)");
    // Two existing keys, three new ones, one of them listed twice.
    commit(
        &session,
        "UNWIND ['L1', 'L2', 'N1', 'N2', 'N1', 'N3'] AS key MERGE (s:Repro {k: key}) SET s.seen = true",
    );
    assert_eq!(
        count(&session, "MATCH (n:Repro) RETURN count(n)"),
        Value::Int64(203)
    );
    assert_eq!(
        count(
            &session,
            "MATCH (n:Repro) WHERE n.seen = true RETURN count(n)"
        ),
        Value::Int64(5)
    );
    assert_eq!(
        count(&session, "MATCH (n:Repro {k: 'N1'}) RETURN count(n)"),
        Value::Int64(1),
        "a key repeated inside one UNWIND merges into the node the first row created"
    );
    assert_eq!(
        count(&session, "MATCH (n:Repro {k: 'L1'}) RETURN n.a"),
        Value::Int64(1)
    );
}
