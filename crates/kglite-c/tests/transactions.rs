//! Explicit transactions: `kglite_session_begin` / `kglite_tx_*`.

use kglite_c::*;
use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;

struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kglite-c-tx-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn graph(&self) -> PathBuf {
        self.0.join("g.kgl")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn take(p: *const c_char) -> Option<String> {
    (!p.is_null()).then(|| {
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
        unsafe { kglite_free_string(p) };
        s
    })
}

fn open_session(dir: &TestDir, options: serde_json::Value) -> *mut KgliteSession {
    let path = cstr(dir.graph().to_str().unwrap());
    let options = cstr(&options.to_string());
    let mut session = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe {
        kglite_open_session(
            path.as_ptr(),
            options.as_ptr(),
            &mut session,
            std::ptr::null_mut(),
            &mut error,
        )
    };
    assert_eq!(status, KgliteStatusCode::Ok, "{:?}", take(error));
    session
}

fn create(level: &str) -> serde_json::Value {
    serde_json::json!({"durability": level, "create_if_missing": true})
}

fn begin(session: *mut KgliteSession, read_only: bool) -> (KgliteStatusCode, *mut KgliteTx) {
    let mut tx = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe { kglite_session_begin(session, read_only, &mut tx, &mut error) };
    take(error);
    (status, tx)
}

fn begin_ok(session: *mut KgliteSession) -> *mut KgliteTx {
    let (status, tx) = begin(session, false);
    assert_eq!(status, KgliteStatusCode::Ok);
    assert!(!tx.is_null());
    tx
}

fn tx_run(tx: *mut KgliteTx, query: &str) -> (KgliteStatusCode, serde_json::Value, Option<String>) {
    let q = cstr(query);
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe {
        kglite_tx_execute(
            tx,
            q.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            &mut result,
            &mut error,
        )
    };
    let rows = if result.is_null() {
        serde_json::Value::Null
    } else {
        let json = take(unsafe { kglite_cypher_result_rows_json(result) }).unwrap();
        unsafe { kglite_cypher_result_free(result) };
        serde_json::from_str(&json).unwrap()
    };
    (status, rows, take(error))
}

fn tx_count(tx: *mut KgliteTx) -> i64 {
    let (status, rows, error) = tx_run(tx, "MATCH (n:T) RETURN count(n) AS c");
    assert_eq!(status, KgliteStatusCode::Ok, "{error:?}");
    rows[0]["c"].as_i64().unwrap()
}

fn commit(tx: *mut KgliteTx) -> (KgliteStatusCode, Option<String>) {
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe { kglite_tx_commit(tx, &mut error) };
    (status, take(error))
}

/// Row count of `:T` through a fresh read-only transaction (the committed state).
fn committed_count(session: *mut KgliteSession) -> i64 {
    let (status, tx) = begin(session, true);
    assert_eq!(status, KgliteStatusCode::Ok);
    let n = tx_count(tx);
    unsafe { kglite_tx_free(tx) };
    n
}

fn autocommit(session: *mut KgliteSession, query: &str) {
    let q = cstr(query);
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe {
        kglite_session_execute_mut(
            session,
            q.as_ptr(),
            std::ptr::null(),
            &mut result,
            &mut error,
        )
    };
    assert_eq!(status, KgliteStatusCode::Ok, "{:?}", take(error));
    unsafe { kglite_cypher_result_free(result) };
}

#[test]
fn commit_publishes_and_rollback_and_free_discard() {
    let dir = TestDir::new("basic");
    let session = open_session(&dir, create("off"));

    let tx = begin_ok(session);
    assert_eq!(tx_run(tx, "CREATE (:T {id: 1})").0, KgliteStatusCode::Ok);
    assert_eq!(commit(tx).0, KgliteStatusCode::Ok);
    unsafe { kglite_tx_free(tx) };
    assert_eq!(committed_count(session), 1);

    let tx = begin_ok(session);
    tx_run(tx, "CREATE (:T {id: 2})");
    assert_eq!(unsafe { kglite_tx_rollback(tx) }, KgliteStatusCode::Ok);
    unsafe { kglite_tx_free(tx) };
    assert_eq!(committed_count(session), 1, "rollback discards");

    // Free of an open transaction rolls back; it never commits.
    let tx = begin_ok(session);
    tx_run(tx, "CREATE (:T {id: 3})");
    unsafe { kglite_tx_free(tx) };
    assert_eq!(committed_count(session), 1, "free without commit discards");

    unsafe { kglite_tx_free(std::ptr::null_mut()) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn statements_see_earlier_uncommitted_rows_and_others_do_not() {
    let dir = TestDir::new("intermediate");
    let session = open_session(&dir, create("off"));
    let tx = begin_ok(session);
    assert_eq!(tx_count(tx), 0);
    tx_run(tx, "CREATE (:T {id: 1})");
    tx_run(tx, "CREATE (:T {id: 2})");
    assert_eq!(tx_count(tx), 2, "own writes are visible between statements");
    assert_eq!(committed_count(session), 0, "other readers see nothing yet");
    // A failed statement leaves the transaction open and its earlier rows intact.
    let (status, _, _) = tx_run(tx, "CREATE (:T {id: ");
    assert_eq!(status, KgliteStatusCode::CypherSyntax);
    assert_eq!(tx_count(tx), 2);
    assert_eq!(commit(tx).0, KgliteStatusCode::Ok);
    unsafe { kglite_tx_free(tx) };
    assert_eq!(committed_count(session), 2);
    unsafe { kglite_session_free(session) };
}

#[test]
fn second_committer_conflicts_and_a_retry_succeeds() {
    let dir = TestDir::new("conflict");
    let session = open_session(&dir, create("off"));
    let first = begin_ok(session);
    let second = begin_ok(session);
    tx_run(first, "CREATE (:T {id: 1})");
    tx_run(second, "CREATE (:T {id: 2})");
    assert_eq!(commit(first).0, KgliteStatusCode::Ok);
    let (status, message) = commit(second);
    assert_eq!(status, KgliteStatusCode::TransactionConflict);
    assert!(message.unwrap().contains("retry"));
    // The loser is finished.
    assert_eq!(
        tx_run(second, "RETURN 1").0,
        KgliteStatusCode::InvalidArgument
    );
    unsafe { kglite_tx_free(second) };
    unsafe { kglite_tx_free(first) };
    assert_eq!(committed_count(session), 1, "the loser applied nothing");

    let retry = begin_ok(session);
    tx_run(retry, "CREATE (:T {id: 2})");
    assert_eq!(commit(retry).0, KgliteStatusCode::Ok);
    unsafe { kglite_tx_free(retry) };
    assert_eq!(committed_count(session), 2);
    unsafe { kglite_session_free(session) };
}

#[test]
fn read_only_transaction_refuses_writes_and_keeps_its_snapshot() {
    let dir = TestDir::new("readonly");
    let session = open_session(&dir, create("off"));
    autocommit(session, "CREATE (:T {id: 1})");
    let (status, tx) = begin(session, true);
    assert_eq!(status, KgliteStatusCode::Ok);
    let (status, _, message) = tx_run(tx, "CREATE (:T {id: 2})");
    assert_eq!(status, KgliteStatusCode::ReadOnly);
    assert!(message.unwrap().contains("read-only"));
    assert_eq!(tx_count(tx), 1);
    autocommit(session, "CREATE (:T {id: 3})");
    assert_eq!(
        tx_count(tx),
        1,
        "a read-only transaction reads one snapshot"
    );
    assert_eq!(
        commit(tx).0,
        KgliteStatusCode::Ok,
        "commit of a read tx is a no-op"
    );
    unsafe { kglite_tx_free(tx) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn begin_read_write_on_a_read_only_session_is_refused() {
    let dir = TestDir::new("ro-session");
    let writer = open_session(&dir, create("full"));
    autocommit(writer, "CREATE (:T {id: 1})");
    unsafe { kglite_session_close(writer, std::ptr::null_mut()) };
    unsafe { kglite_session_free(writer) };

    let session = open_session(&dir, serde_json::json!({"lock_timeout_ms": -1}));
    let (status, tx) = begin(session, false);
    assert_eq!(status, KgliteStatusCode::ReadOnly);
    assert!(tx.is_null());
    let (status, tx) = begin(session, true);
    assert_eq!(status, KgliteStatusCode::Ok);
    assert_eq!(tx_count(tx), 1);
    unsafe { kglite_tx_free(tx) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn finished_transaction_reports_a_clear_status() {
    let dir = TestDir::new("finished");
    let session = open_session(&dir, create("off"));
    let tx = begin_ok(session);
    assert_eq!(commit(tx).0, KgliteStatusCode::Ok);
    let (status, _, message) = tx_run(tx, "RETURN 1");
    assert_eq!(status, KgliteStatusCode::InvalidArgument);
    assert!(message
        .unwrap()
        .contains("already committed or rolled back"));
    let (status, message) = commit(tx);
    assert_eq!(status, KgliteStatusCode::InvalidArgument);
    assert!(message
        .unwrap()
        .contains("already committed or rolled back"));
    assert_eq!(unsafe { kglite_tx_rollback(tx) }, KgliteStatusCode::Ok);
    unsafe { kglite_tx_free(tx) };

    let tx = begin_ok(session);
    unsafe { kglite_tx_rollback(tx) };
    assert_eq!(tx_run(tx, "RETURN 1").0, KgliteStatusCode::InvalidArgument);
    unsafe { kglite_tx_free(tx) };

    // Null handles are refused, not dereferenced.
    assert_eq!(
        commit(std::ptr::null_mut()).0,
        KgliteStatusCode::NullPointer
    );
    assert_eq!(
        tx_run(std::ptr::null_mut(), "RETURN 1").0,
        KgliteStatusCode::NullPointer
    );
    unsafe { kglite_session_free(session) };
}

#[test]
fn durable_commit_survives_a_crash_and_an_open_transaction_does_not() {
    for level in ["full", "normal"] {
        let dir = TestDir::new(&format!("durable-{level}"));
        let session = open_session(&dir, create(level));
        let committed = begin_ok(session);
        tx_run(committed, "CREATE (:T {id: 1})");
        tx_run(committed, "CREATE (:T {id: 2})");
        assert_eq!(commit(committed).0, KgliteStatusCode::Ok);
        let uncommitted = begin_ok(session);
        tx_run(uncommitted, "CREATE (:T {id: 3})");
        // Freed, never closed or checkpointed: only the log can hold the first.
        unsafe { kglite_tx_free(uncommitted) };
        unsafe { kglite_tx_free(committed) };
        unsafe { kglite_session_free(session) };

        let reopened = open_session(&dir, serde_json::json!({"durability": level}));
        assert_eq!(committed_count(reopened), 2, "level {level}");
        unsafe { kglite_session_free(reopened) };
    }
}

#[test]
fn transactions_on_one_session_run_from_different_threads() {
    let dir = TestDir::new("threads");
    let session = open_session(&dir, create("off"));
    let address = session as usize;
    let handles: Vec<_> = (0..4)
        .map(|n| {
            std::thread::spawn(move || {
                let session = address as *mut KgliteSession;
                // Each thread owns its transaction; conflicts retry.
                loop {
                    let tx = begin_ok(session);
                    tx_run(tx, &format!("CREATE (:T {{id: {n}}})"));
                    let (status, _) = commit(tx);
                    unsafe { kglite_tx_free(tx) };
                    match status {
                        KgliteStatusCode::Ok => break,
                        KgliteStatusCode::TransactionConflict => continue,
                        other => panic!("{other:?}"),
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(committed_count(session), 4);
    unsafe { kglite_session_free(session) };
}
