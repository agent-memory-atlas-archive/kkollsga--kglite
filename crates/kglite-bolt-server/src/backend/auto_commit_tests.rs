//! Auto-commit data writes at the backend: `session.run("CREATE ...")` is one
//! implicit transaction that commits inside `execute()`.

use std::collections::HashMap;

use boltr::error::BoltError;
use boltr::server::{BoltBackend, SessionHandle};
use boltr::types::{BoltDict, BoltValue};
use kglite::api::session::CsvImportPolicy;
use kglite::api::storage::{new_dir_graph_in_mode, StorageMode};
use kglite::api::Value;

use super::*;

fn backend_with(readonly: bool) -> KgliteBackend {
    let graph = new_dir_graph_in_mode(StorageMode::Memory, None).expect("memory graph");
    KgliteBackend::new(
        kglite::api::session::Session::new(graph),
        std::env::temp_dir().join("auto-commit-unused.kgl"),
        readonly,
        "127.0.0.1:0".into(),
        CsvImportPolicy::Denied,
        ServerIdentity::default(),
        None,
    )
}

fn session() -> SessionHandle {
    SessionHandle("auto-commit".into())
}

fn read_mode() -> BoltDict {
    BoltDict::from([("mode".to_string(), BoltValue::String("r".into()))])
}

async fn run(b: &KgliteBackend, query: &str, extra: &BoltDict) -> Result<ResultStream, BoltError> {
    b.execute(&session(), query, &HashMap::new(), extra, None)
        .await
}

fn count(b: &KgliteBackend, label: &str) -> i64 {
    let snapshot = b.session.snapshot();
    let params = HashMap::new();
    let opts = kglite::api::session::ExecuteOptions::new(&params);
    let out = kglite::api::session::execute_read(
        &snapshot,
        &format!("MATCH (n:{label}) RETURN count(n) AS c"),
        &opts,
    )
    .expect("count query")
    .result;
    match out.rows.first().and_then(|r| r.first()) {
        Some(Value::Int64(n)) => *n,
        other => panic!("expected Int64, got {other:?}"),
    }
}

fn summary_str(stream: &ResultStream, key: &str) -> String {
    match stream.summary.get(key) {
        Some(BoltValue::String(s)) => s.clone(),
        other => panic!("summary[{key}] is not a string: {other:?}"),
    }
}

#[tokio::test]
async fn a_data_write_commits_and_bumps_the_version() {
    let b = backend_with(false);
    let version = b.session.version();
    let out = run(&b, "CREATE (:Person {id: 1})", &BoltDict::new())
        .await
        .expect("auto-commit CREATE");
    assert_eq!(summary_str(&out, "type"), "w");
    assert!(out.records.is_empty());
    assert!(b.session.version() > version, "the write must be published");
    assert_eq!(count(&b, "Person"), 1);
    let Some(BoltValue::Dict(stats)) = out.summary.get("stats") else {
        panic!("a write reports stats: {:?}", out.summary);
    };
    assert_eq!(stats.get("nodes-created"), Some(&BoltValue::Integer(1)));
}

#[tokio::test]
async fn a_write_with_return_streams_its_rows_after_committing() {
    let b = backend_with(false);
    let out = run(
        &b,
        "CREATE (n:Person {id: 7}) RETURN n.id AS id",
        &BoltDict::new(),
    )
    .await
    .expect("auto-commit CREATE ... RETURN");
    assert_eq!(out.records.len(), 1);
    assert_eq!(out.records[0].values, vec![BoltValue::Integer(7)]);
    assert_eq!(summary_str(&out, "type"), "rw");
    assert_eq!(count(&b, "Person"), 1);
}

#[tokio::test]
async fn a_failing_write_applies_nothing() {
    let b = backend_with(false);
    run(
        &b,
        "CREATE CONSTRAINT FOR (n:Person) REQUIRE n.k IS UNIQUE",
        &BoltDict::new(),
    )
    .await
    .expect("constraint");
    run(&b, "CREATE (:Person {k: 1})", &BoltDict::new())
        .await
        .expect("seed");
    let version = b.session.version();
    // The first CREATE is valid; the second violates the constraint.
    let err = run(
        &b,
        "CREATE (:Person {k: 2}) WITH 1 AS x CREATE (:Person {k: 1})",
        &BoltDict::new(),
    )
    .await
    .expect_err("the duplicate id must fail the statement");
    assert!(!format!("{err:?}").is_empty());
    assert_eq!(
        b.session.version(),
        version,
        "a failed write publishes nothing"
    );
    assert_eq!(count(&b, "Person"), 1, "the valid half must not survive");
}

#[tokio::test]
async fn a_write_in_a_read_mode_run_is_an_access_mode_error() {
    let b = backend_with(false);
    let version = b.session.version();
    let err = run(&b, "CREATE (:Person {id: 1})", &read_mode())
        .await
        .expect_err("mode r must refuse a write");
    assert!(
        matches!(&err, BoltError::Query { code, message }
            if code == "Neo.ClientError.Statement.AccessMode"
                && message.contains("read access mode")),
        "unexpected error: {err:?}"
    );
    assert_eq!(b.session.version(), version);
    assert_eq!(count(&b, "Person"), 0);
    run(&b, "MATCH (n:Person) RETURN n", &read_mode())
        .await
        .expect("a read in a read-mode run is fine");
}

#[tokio::test]
async fn readonly_servers_still_refuse_auto_commit_writes() {
    let b = backend_with(true);
    let err = run(&b, "CREATE (:Person {id: 1})", &BoltDict::new())
        .await
        .expect_err("--readonly refuses writes");
    assert!(
        matches!(&err, BoltError::Query { code, message }
            if code == "Neo.ClientError.General.ReadOnly" && message.contains("--readonly")),
        "unexpected error: {err:?}"
    );
}

#[tokio::test]
async fn explain_of_a_write_publishes_nothing_in_auto_commit() {
    let b = backend_with(false);
    let version = b.session.version();
    let out = run(&b, "EXPLAIN CREATE (:Person {id: 1})", &BoltDict::new())
        .await
        .expect("EXPLAIN of a write");
    assert!(out.summary.contains_key("plan"), "EXPLAIN returns its plan");
    assert_eq!(
        b.session.version(),
        version,
        "EXPLAIN must not bump the version"
    );
    assert_eq!(count(&b, "Person"), 0);
}

#[tokio::test]
async fn explain_of_a_write_publishes_nothing_in_an_explicit_transaction() {
    let b = backend_with(false);
    let s = session();
    let version = b.session.version();
    let tx = b.begin_transaction(&s, &BoltDict::new()).await.unwrap();
    b.execute(
        &s,
        "EXPLAIN CREATE (:Person {id: 1})",
        &HashMap::new(),
        &BoltDict::new(),
        Some(&tx),
    )
    .await
    .expect("EXPLAIN in a transaction");
    b.commit(&s, &tx).await.unwrap();
    assert_eq!(
        b.session.version(),
        version,
        "committing a transaction that only EXPLAINed a write must not bump the version"
    );
}
