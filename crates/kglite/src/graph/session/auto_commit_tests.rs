//! `Session::execute_auto_commit` and `Session::checkpoint_if_changed`.

use super::auto_commit::CheckpointOutcome;
use super::{execute_mut, ExecuteOptions, Session};
use crate::error::{KgError, KgErrorCode};
use crate::graph::dir_graph::DirGraph;
use crate::graph::wal::DurabilityLevel;
use std::collections::HashMap;

fn count(session: &Session, label: &str) -> usize {
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    let q = format!("MATCH (n:{label}) RETURN count(n) AS c");
    let out = super::execute_read(&session.snapshot(), &q, &opts).unwrap();
    match &out.result.rows[0][0] {
        crate::datatypes::Value::Int64(n) => *n as usize,
        other => panic!("count was {other:?}"),
    }
}

/// Commit one `CREATE` on its own transaction: the competitor that lands
/// between an auto-commit write's execution and its commit.
fn commit_competitor(session: &Session) {
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    let mut tx = session.begin();
    execute_mut(tx.working_mut().unwrap(), "CREATE (:Rival)", &opts).unwrap();
    session.commit(tx, true);
}

#[test]
fn a_write_commits_and_returns_its_outcome() {
    let session = Session::new(DirGraph::new());
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    let out = session
        .execute_auto_commit("CREATE (:Item {id: 1})", &opts, 1)
        .unwrap();
    assert!(out.is_mutation);
    assert_eq!(count(&session, "Item"), 1);
}

#[test]
fn a_lost_race_is_retried_and_given_up_after_the_attempts() {
    let session = Session::new(DirGraph::new());
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);

    let mut last = 0;
    session
        .execute_auto_commit_observed("CREATE (:Item {id: 1})", &opts, 3, &mut |attempt| {
            last = attempt;
            if attempt <= 2 {
                commit_competitor(&session);
            }
        })
        .expect("the third attempt wins");
    assert_eq!(last, 3);
    assert_eq!(count(&session, "Item"), 1);
    assert_eq!(count(&session, "Rival"), 2);

    let err = session
        .execute_auto_commit_observed("CREATE (:Item {id: 2})", &opts, 3, &mut |_| {
            commit_competitor(&session)
        })
        .err()
        .expect("three lost races");
    assert_eq!(err.code(), KgErrorCode::TransactionConflict);
    assert_eq!(count(&session, "Item"), 1, "a lost race applies nothing");
    assert_eq!(count(&session, "Rival"), 5);
}

#[test]
fn one_attempt_surfaces_the_first_lost_race() {
    let session = Session::new(DirGraph::new());
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    let mut runs = 0;
    let err = session
        .execute_auto_commit_observed("CREATE (:Item {id: 1})", &opts, 1, &mut |_| {
            runs += 1;
            commit_competitor(&session)
        })
        .err()
        .expect("no retry");
    assert_eq!(runs, 1);
    assert!(matches!(err, KgError::TransactionConflict { .. }));
}

#[test]
fn a_statement_error_is_returned_untouched_and_not_retried() {
    let session = Session::new(DirGraph::new());
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    let mut runs = 0;
    let err = session
        .execute_auto_commit_observed("CREATE (", &opts, 3, &mut |_| runs += 1)
        .err()
        .expect("syntax error");
    assert_eq!(err.code(), KgErrorCode::CypherSyntax);
    assert_eq!(runs, 0);
}

#[test]
fn a_rejected_log_append_is_a_durability_failure_that_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.kgl");
    let session = Session::open_durable(
        std::sync::Arc::new(DirGraph::new()),
        &path.to_string_lossy(),
        DurabilityLevel::Full,
    )
    .unwrap();
    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    session
        .execute_auto_commit("CREATE (:Item {id: 1})", &opts, 3)
        .unwrap();
    let before = session.version();

    session.set_fail_append(true);
    let mut runs = 0;
    let err = session
        .execute_auto_commit_observed("CREATE (:Item {id: 2})", &opts, 3, &mut |_| runs += 1)
        .err()
        .expect("the log refuses the frame");
    assert_eq!(err.code(), KgErrorCode::DurabilityFailed);
    assert!(err.to_string().contains("NOT applied"), "{err}");
    assert_eq!(runs, 1, "a log failure is not retried");
    assert_eq!(session.version(), before);
    assert_eq!(count(&session, "Item"), 1);
}

#[test]
fn the_first_checkpoint_writes_then_unchanged_ones_skip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("served.kgl");
    let session = Session::new(DirGraph::new());
    let mut last = None;

    let first = session.checkpoint_if_changed(&path, &mut last).unwrap();
    assert_eq!(first, CheckpointOutcome::Written(session.version()));
    assert!(path.exists());
    let second = session.checkpoint_if_changed(&path, &mut last).unwrap();
    assert_eq!(second, CheckpointOutcome::Skipped(session.version()));

    let params = HashMap::new();
    let opts = ExecuteOptions::new(&params);
    session
        .execute_auto_commit("CREATE (:Item {id: 1})", &opts, 1)
        .unwrap();
    let third = session.checkpoint_if_changed(&path, &mut last).unwrap();
    assert_eq!(third, CheckpointOutcome::Written(session.version()));
    assert_eq!(last, Some(session.version()));
}

#[test]
fn a_failed_checkpoint_records_no_version() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::new(DirGraph::new());
    let mut last = None;
    session
        .checkpoint_if_changed(&dir.path().join("missing").join("g.kgl"), &mut last)
        .expect_err("the directory does not exist");
    assert_eq!(last, None);
    let retry = session
        .checkpoint_if_changed(&dir.path().join("g.kgl"), &mut last)
        .unwrap();
    assert!(matches!(retry, CheckpointOutcome::Written(_)));
}
