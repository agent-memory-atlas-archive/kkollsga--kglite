//! `CALL db.backup(...)` at the backend: the recognizer and the refusals.

use std::collections::HashMap;

use boltr::error::BoltError;
use boltr::server::{BoltBackend, SessionHandle};
use boltr::types::{BoltDict, BoltValue};
use kglite::api::session::CsvImportPolicy;
use kglite::api::storage::{new_dir_graph_in_mode, StorageMode};

use super::intercepts::{parse_backup_call, BackupArg, BACKUP_COLUMNS};
use super::*;

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "kglite-bolt-backup-{tag}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn backend(dir: &std::path::Path, readonly: bool, policy: BackupPolicy) -> KgliteBackend {
    let graph = new_dir_graph_in_mode(StorageMode::Memory, None).expect("memory graph");
    KgliteBackend::new(
        kglite::api::session::Session::new(graph),
        dir.join("served.kgl"),
        readonly,
        "127.0.0.1:0".into(),
        CsvImportPolicy::Denied,
        ServerIdentity::default(),
        None,
    )
    .with_backup_policy(policy)
}

async fn call(
    b: &KgliteBackend,
    query: &str,
    params: HashMap<String, BoltValue>,
    tx: Option<&TransactionHandle>,
) -> Result<ResultStream, BoltError> {
    b.execute(
        &SessionHandle("backup".into()),
        query,
        &params,
        &BoltDict::new(),
        tx,
    )
    .await
}

fn policy_for(dir: &std::path::Path) -> BackupPolicy {
    BackupPolicy::from_flags(Some(dir), false, false).unwrap()
}

#[test]
fn backup_parser_accepts_literal_param_and_yield() {
    let all = BACKUP_COLUMNS.to_vec();
    let lit = |s: &str| BackupArg::Literal(s.to_string());
    let cases: Vec<(&str, BackupArg, Vec<&str>)> = vec![
        ("CALL db.backup('a.kgl')", lit("a.kgl"), all.clone()),
        ("call db.backup( \"a.kgl\" ) ;", lit("a.kgl"), all.clone()),
        (
            "CALL db.backup($name)",
            BackupArg::Param("name".into()),
            all.clone(),
        ),
        (
            "CALL db.backup($n) YIELD path, bytes",
            BackupArg::Param("n".into()),
            vec!["path", "bytes"],
        ),
    ];
    for (query, arg, columns) in cases {
        let parsed = parse_backup_call(query).unwrap_or_else(|| panic!("{query} must parse"));
        assert_eq!(parsed.arg, arg, "{query}");
        assert_eq!(parsed.columns, columns, "{query}");
    }
}

#[test]
fn backup_parser_rejects_extra_args_and_other_shapes() {
    for query in [
        "CALL db.backup()",
        "CALL db.backup('a.kgl', 'b.kgl')",
        "CALL db.backup($a, $b)",
        "CALL db.backup('a.kgl' + 'b')",
        "CALL db.backup(a)",
        "CALL db.backup($)",
        "CALL db.backup('a\\b.kgl')",
        "CALL db.backup('a.kgl)",
        "CALL db.backup('a.kgl') YIELD nope",
        "CALL db.backup('a.kgl') YIELD path, path",
        "CALL db.backups('a.kgl')",
        "MATCH (n) CALL db.backup('a.kgl')",
    ] {
        assert!(parse_backup_call(query).is_none(), "{query} must not parse");
    }
}

#[tokio::test]
async fn backup_writes_inside_the_dir_and_reports_the_columns() {
    let dir = scratch_dir("ok");
    let b = backend(&dir, false, policy_for(&dir));
    let stream = call(&b, "CALL db.backup('b.kgl')", HashMap::new(), None)
        .await
        .expect("backup succeeds");
    assert_eq!(stream.metadata.columns, BACKUP_COLUMNS.to_vec());
    let values = &stream.records[0].values;
    assert_eq!(values[0], BoltValue::Boolean(true));
    assert_eq!(
        values[1],
        BoltValue::String(dir.join("b.kgl").display().to_string())
    );
    assert_eq!(values[2], BoltValue::Null, "non-durable session has no lsn");
    assert!(dir.join("b.kgl").is_file());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn backup_is_allowed_on_a_readonly_server() {
    let dir = scratch_dir("ro");
    let b = backend(&dir, true, policy_for(&dir));
    call(
        &b,
        "CALL db.backup($n)",
        HashMap::from([("n".into(), BoltValue::String("r.kgl".into()))]),
        None,
    )
    .await
    .expect("readonly may back up");
    assert!(dir.join("r.kgl").is_file());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn backup_without_a_backup_dir_is_refused_and_names_the_flag() {
    let dir = scratch_dir("off");
    let b = backend(&dir, false, BackupPolicy::Disabled);
    let err = call(&b, "CALL db.backup('x.kgl')", HashMap::new(), None)
        .await
        .expect_err("disabled");
    assert!(matches!(err, BoltError::Forbidden(_)), "{err:?}");
    assert!(err.to_string().contains("--backup-dir"), "{err}");
    assert!(!dir.join("x.kgl").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn backup_names_that_escape_the_dir_are_refused_and_write_nothing() {
    let dir = scratch_dir("esc");
    let inner = dir.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let b = backend(&dir, false, policy_for(&inner));
    for name in [
        "../escaped.kgl",
        "sub/x.kgl",
        "/tmp/abs.kgl",
        "a\\b.kgl",
        "",
    ] {
        let params = HashMap::from([("n".to_string(), BoltValue::String(name.into()))]);
        let err = call(&b, "CALL db.backup($n)", params, None)
            .await
            .expect_err(name);
        assert!(matches!(err, BoltError::Forbidden(_)), "{name}: {err:?}");
    }
    assert!(!dir.join("escaped.kgl").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn backup_inside_an_explicit_transaction_is_refused() {
    let dir = scratch_dir("tx");
    let b = backend(&dir, false, policy_for(&dir));
    let tx = b
        .begin_transaction(&SessionHandle("backup".into()), &BoltDict::new())
        .await
        .unwrap();
    let err = call(&b, "CALL db.backup('t.kgl')", HashMap::new(), Some(&tx))
        .await
        .expect_err("in-tx");
    assert!(matches!(err, BoltError::Protocol(_)), "{err:?}");
    assert!(!dir.join("t.kgl").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn backup_over_the_served_graph_is_refused() {
    let dir = scratch_dir("alias");
    let b = backend(&dir, false, policy_for(&dir));
    let err = call(&b, "CALL db.backup('served.kgl')", HashMap::new(), None)
        .await
        .expect_err("alias");
    assert!(matches!(err, BoltError::Forbidden(_)), "{err:?}");
    assert!(!dir.join("served.kgl").exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn second_concurrent_backup_gets_busy() {
    let dir = scratch_dir("busy");
    let graph = new_dir_graph_in_mode(StorageMode::Memory, None).unwrap();
    let service = crate::backup::BackupService::new(
        Arc::new(kglite::api::session::Session::new(graph)),
        dir.join("served.kgl"),
        policy_for(&dir),
    );
    // Hold the gate the way an in-flight backup does.
    let held = service.clone();
    held.hold_for_test();
    assert_eq!(
        service.run_blocking(&dir.join("b.kgl")).unwrap_err(),
        crate::backup::BackupError::Busy
    );
    held.release_for_test();
    service
        .run_blocking(&dir.join("b.kgl"))
        .expect("free again");
    std::fs::remove_dir_all(&dir).ok();
}
