//! A create/delete steady state keeps the column stores bounded: deleted
//! nodes' rows are reclaimed at commit instead of accumulating forever.

use super::{execute_mut, execute_read, CommitOutcome, ExecuteOptions, Session};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::storage::GraphRead;
use crate::graph::wal::DurabilityLevel;
use std::collections::HashMap;
use std::sync::Arc;

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

/// `(rows held by every column store, live node count)`.
fn rows(session: &Session) -> (usize, usize) {
    let graph = session.snapshot();
    let total = graph
        .graph
        .column_stores_iter()
        .map(|(_, store)| store.row_count() as usize)
        .sum();
    (total, graph.graph.node_count())
}

fn scalar(session: &Session, query: &str) -> Value {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let snapshot = session.snapshot();
    execute_read(&snapshot, query, &opts).unwrap().result.rows[0][0].clone()
}

const CREATE_1000: &str =
    "UNWIND range(1, 1000) AS i CREATE (:Repro {k: 'key' + toString(i), v: i})";

#[test]
fn delete_all_then_recreate_does_not_accumulate_rows() {
    let session = Session::new(DirGraph::new());
    for cycle in 0..25 {
        commit(&session, CREATE_1000);
        assert_eq!(rows(&session), (1000, 1000), "cycle {cycle} after create");
        commit(&session, "MATCH (n) DETACH DELETE n");
        assert_eq!(rows(&session), (0, 0), "cycle {cycle} after delete");
    }
}

#[test]
fn partial_churn_stays_bounded_and_survivors_read_back() {
    let session = Session::new(DirGraph::new());
    commit(
        &session,
        "UNWIND range(1, 300) AS i CREATE (:Repro {k: 'keep' + toString(i), v: i})",
    );
    for _ in 0..30 {
        commit(&session, CREATE_1000);
        commit(
            &session,
            "MATCH (n:Repro) WHERE n.k STARTS WITH 'key' DETACH DELETE n",
        );
        let (total, live) = rows(&session);
        assert_eq!(live, 300);
        // Dead rows are held to the vacuum ratio (0.3 of the store) or the
        // 100-row floor, never to the number of rows ever deleted.
        assert!(
            total <= 300 + 300,
            "store holds {total} rows for {live} nodes"
        );
    }
    assert_eq!(
        scalar(&session, "MATCH (n:Repro) RETURN sum(n.v)"),
        Value::Int64(300 * 301 / 2)
    );
    assert_eq!(
        scalar(&session, "MATCH (n:Repro {k: 'keep77'}) RETURN n.v"),
        Value::Int64(77)
    );
}

/// The rebuild renumbers rows without a logical write: a durable commit's
/// frame carries the deletes and nothing else, and the log replays to the
/// same graph.
#[test]
fn reclaiming_rows_adds_nothing_to_the_wal_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let p = path.to_string_lossy().into_owned();
    let session =
        Session::open_durable(Arc::new(DirGraph::new()), &p, DurabilityLevel::Full).unwrap();
    commit(
        &session,
        "UNWIND range(1, 300) AS i CREATE (:Repro {id: i, v: i})",
    );
    commit(
        &session,
        "UNWIND range(1001, 2000) AS i CREATE (:Repro {id: i, v: i})",
    );
    commit(
        &session,
        "MATCH (n:Repro) WHERE n.id > 1000 DETACH DELETE n",
    );
    assert_eq!(
        rows(&session),
        (300, 300),
        "the delete commit reclaimed the rows"
    );
    let frames = crate::graph::wal::recover(&crate::graph::wal::wal_path(&path)).unwrap();
    assert_eq!(
        frames.last().unwrap().ops.len(),
        1000,
        "one RemoveNode per deleted node, no update per surviving node"
    );
    drop(session);

    let recovered = super::Session::open_durable(
        crate::graph::io::file::load_file(&p).unwrap_or_else(|_| Arc::new(DirGraph::new())),
        &p,
        DurabilityLevel::Full,
    )
    .unwrap();
    assert_eq!(rows(&recovered).1, 300);
    assert_eq!(
        scalar(&recovered, "MATCH (n:Repro) RETURN sum(n.v)"),
        Value::Int64(300 * 301 / 2)
    );
}
