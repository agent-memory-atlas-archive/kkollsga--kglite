//! `KgliteCursor`: a read query pulled a batch at a time.

use kglite_c::*;
use std::ffi::{c_char, CStr, CString};

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

fn session(nodes: u32) -> *mut KgliteSession {
    let graph = kglite_graph_new();
    let mut session = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_session_new(graph, &mut session) },
        KgliteStatusCode::Ok
    );
    let q = cstr(&format!(
        "UNWIND range(1, {nodes}) AS i CREATE (:T {{id: i, name: 'n' + toString(i)}})"
    ));
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
    session
}

fn open(
    session: *const KgliteSession,
    query: &str,
    options: *const KgliteExecuteOptions,
) -> (KgliteStatusCode, *mut KgliteCursor, Option<String>) {
    let q = cstr(query);
    let mut cursor = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe {
        kglite_session_cursor_open(
            session,
            q.as_ptr(),
            std::ptr::null(),
            options,
            &mut cursor,
            &mut error,
        )
    };
    (status, cursor, take(error))
}

fn batch(cursor: *mut KgliteCursor, max: usize) -> (KgliteStatusCode, Vec<serde_json::Value>) {
    let mut rows: *const c_char = std::ptr::null();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe { kglite_cursor_next_batch(cursor, max, &mut rows, &mut error) };
    take(error);
    let parsed = take(rows).map_or_else(Vec::new, |t| serde_json::from_str(&t).unwrap());
    (status, parsed)
}

fn eager_rows(session: *const KgliteSession, query: &str) -> Vec<serde_json::Value> {
    let q = cstr(query);
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe {
        kglite_session_execute_read(
            session,
            q.as_ptr(),
            std::ptr::null(),
            &mut result,
            &mut error,
        )
    };
    assert_eq!(status, KgliteStatusCode::Ok, "{:?}", take(error));
    let rows =
        serde_json::from_str(&take(unsafe { kglite_cypher_result_rows_json(result) }).unwrap())
            .unwrap();
    unsafe { kglite_cypher_result_free(result) };
    rows
}

fn drain(cursor: *mut KgliteCursor, max: usize) -> Vec<serde_json::Value> {
    let mut all = Vec::new();
    loop {
        let (status, rows) = batch(cursor, max);
        assert_eq!(status, KgliteStatusCode::Ok);
        if rows.is_empty() {
            return all;
        }
        assert!(rows.len() <= max);
        all.extend(rows);
    }
}

#[test]
fn cursor_rows_equal_the_one_shot_result_for_streamed_and_materialised_shapes() {
    let session = session(3000);
    for (query, streamed) in [
        ("MATCH (n:T) RETURN n.id AS id, n.name AS name", true),
        ("MATCH (n:T) WHERE n.id % 4 = 0 RETURN n.id AS id", true),
        ("MATCH (n:T) RETURN n.id AS id ORDER BY id DESC", false),
        ("MATCH (n:T) RETURN count(n) AS c", false),
    ] {
        let (status, cursor, error) = open(session, query, std::ptr::null());
        assert_eq!(status, KgliteStatusCode::Ok, "{query}: {error:?}");
        assert_eq!(
            unsafe { kglite_cursor_streamed(cursor) },
            streamed,
            "{query}"
        );
        let columns: Vec<String> =
            serde_json::from_str(&take(unsafe { kglite_cursor_columns_json(cursor) }).unwrap())
                .unwrap();
        assert!(!columns.is_empty());
        assert_eq!(drain(cursor, 700), eager_rows(session, query), "{query}");
        // Exhausted stays exhausted.
        assert_eq!(batch(cursor, 10), (KgliteStatusCode::Ok, Vec::new()));
        unsafe { kglite_cursor_free(cursor) };
    }
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_parse_error_is_reported_at_open_and_null_arguments_are_refused() {
    let session = session(5);
    let (status, cursor, error) = open(session, "MATCH (n RETURN n", std::ptr::null());
    assert_ne!(status, KgliteStatusCode::Ok);
    assert!(cursor.is_null());
    assert!(error.is_some());
    let (status, cursor, _) = open(session, "CREATE (:X)", std::ptr::null());
    assert_ne!(
        status,
        KgliteStatusCode::Ok,
        "a mutation has no read cursor"
    );
    assert!(cursor.is_null());
    let mut rows: *const c_char = std::ptr::null();
    assert_eq!(
        unsafe {
            kglite_cursor_next_batch(std::ptr::null_mut(), 10, &mut rows, std::ptr::null_mut())
        },
        KgliteStatusCode::NullPointer
    );
    unsafe { kglite_cursor_free(std::ptr::null_mut()) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn the_cursor_keeps_its_snapshot_after_a_commit_and_after_the_session_is_freed() {
    let session = session(2000);
    let (status, cursor, _) = open(session, "MATCH (n:T) RETURN n.id AS id", std::ptr::null());
    assert_eq!(status, KgliteStatusCode::Ok);
    let (_, first) = batch(cursor, 10);
    assert_eq!(first.len(), 10);
    let q = cstr("UNWIND range(1, 500) AS i CREATE (:T {id: 100000 + i})");
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    assert_eq!(
        unsafe {
            kglite_session_execute_mut(
                session,
                q.as_ptr(),
                std::ptr::null(),
                &mut result,
                &mut error,
            )
        },
        KgliteStatusCode::Ok
    );
    unsafe { kglite_cypher_result_free(result) };
    unsafe { kglite_session_free(session) };
    assert_eq!(drain(cursor, 300).len(), 1990);
    unsafe { kglite_cursor_free(cursor) };
}

#[test]
fn cancelling_the_token_ends_the_cursor_with_cancelled() {
    let session = session(60_000);
    let mut token = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_cancel_token_new(&mut token) },
        KgliteStatusCode::Ok
    );
    let options = KgliteExecuteOptions {
        struct_size: std::mem::size_of::<KgliteExecuteOptions>(),
        timeout_ms: 0,
        max_work_units: 0,
        row_limit: 0,
        flags: 0,
        reserved: 0,
        cancel: token,
    };
    let (status, cursor, _) = open(session, "MATCH (n:T) RETURN n.id AS id", &options);
    assert_eq!(status, KgliteStatusCode::Ok);
    assert_eq!(batch(cursor, 10).1.len(), 10);
    unsafe { kglite_cancel_token_cancel(token) };
    let mut last = KgliteStatusCode::Ok;
    for _ in 0..200 {
        last = batch(cursor, 1000).0;
        if last != KgliteStatusCode::Ok {
            break;
        }
    }
    assert_eq!(last, KgliteStatusCode::Cancelled);
    assert_eq!(batch(cursor, 10), (KgliteStatusCode::Ok, Vec::new()));
    unsafe { kglite_cursor_free(cursor) };
    unsafe { kglite_cancel_token_free(token) };
    unsafe { kglite_session_free(session) };
}
