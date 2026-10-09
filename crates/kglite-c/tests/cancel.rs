//! `KgliteCancelToken`: stopping a running query from another thread.

use kglite_c::*;
use std::ffi::{c_char, CStr, CString};
use std::time::{Duration, Instant};

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

/// A session over 2000 `:T` nodes; the cartesian query below would run for
/// minutes uncancelled.
fn session() -> *mut KgliteSession {
    let graph = kglite_graph_new();
    let mut session = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_session_new(graph, &mut session) },
        KgliteStatusCode::Ok
    );
    let q = cstr("UNWIND range(1, 2000) AS i CREATE (:T {id: i})");
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

const LONG: &str = "MATCH (a:T), (b:T), (c:T) WHERE a.id + b.id + c.id = -1 RETURN count(*) AS c";

fn token() -> *mut KgliteCancelToken {
    let mut token = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_cancel_token_new(&mut token) },
        KgliteStatusCode::Ok
    );
    token
}

fn options(token: *const KgliteCancelToken) -> KgliteExecuteOptions {
    KgliteExecuteOptions {
        struct_size: std::mem::size_of::<KgliteExecuteOptions>(),
        timeout_ms: 0,
        max_work_units: 0,
        row_limit: 0,
        flags: 0,
        reserved: 0,
        cancel: token,
    }
}

fn run_read(
    session: *const KgliteSession,
    query: &str,
    opts: &KgliteExecuteOptions,
) -> (KgliteStatusCode, Duration) {
    let q = cstr(query);
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let started = Instant::now();
    let status = unsafe {
        kglite_session_execute_read_ex(
            session,
            q.as_ptr(),
            std::ptr::null(),
            opts,
            &mut result,
            &mut error,
        )
    };
    let elapsed = started.elapsed();
    take(error);
    unsafe { kglite_cypher_result_free(result) };
    (status, elapsed)
}

#[test]
fn cancel_from_another_thread_stops_a_long_query_promptly() {
    let session = session();
    let token = token();
    let canceller = token as usize;
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        unsafe { kglite_cancel_token_cancel(canceller as *const KgliteCancelToken) }
    });
    let (status, elapsed) = run_read(session, LONG, &options(token));
    assert_eq!(thread.join().unwrap(), KgliteStatusCode::Ok);
    assert_eq!(status, KgliteStatusCode::Cancelled);
    assert!(
        elapsed < Duration::from_secs(10),
        "stopped after {elapsed:?}"
    );
    unsafe { kglite_cancel_token_free(token) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_token_freed_during_the_call_is_safe() {
    let session = session();
    let token = token();
    let address = token as usize;
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        // Cancel, then free, while the query is still running.
        let token = address as *mut KgliteCancelToken;
        unsafe { kglite_cancel_token_cancel(token) };
        unsafe { kglite_cancel_token_free(token) };
    });
    let (status, elapsed) = run_read(session, LONG, &options(token));
    thread.join().unwrap();
    assert_eq!(status, KgliteStatusCode::Cancelled);
    assert!(
        elapsed < Duration::from_secs(10),
        "stopped after {elapsed:?}"
    );
    // The recycled flag slot did not leak a cancellation into a new token.
    let fresh = self::token();
    let (status, _) = run_read(session, "MATCH (n:T) RETURN count(n) AS c", &options(fresh));
    assert_eq!(status, KgliteStatusCode::Ok);
    unsafe { kglite_cancel_token_free(fresh) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn cancelling_one_query_leaves_another_running() {
    let session = session();
    let doomed = token();
    let survivor = token();
    let address = doomed as usize;
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        unsafe { kglite_cancel_token_cancel(address as *const KgliteCancelToken) }
    });
    let session_address = session as usize;
    let survivor_address = survivor as usize;
    let other = std::thread::spawn(move || {
        let opts = options(survivor_address as *const KgliteCancelToken);
        run_read(
            session_address as *const KgliteSession,
            "MATCH (a:T), (b:T) WHERE a.id + b.id = -1 RETURN count(*) AS c",
            &opts,
        )
    });
    let (status, _) = run_read(session, LONG, &options(doomed));
    thread.join().unwrap();
    assert_eq!(status, KgliteStatusCode::Cancelled);
    let (other_status, _) = other.join().unwrap();
    assert_eq!(
        other_status,
        KgliteStatusCode::Ok,
        "the other query ran to completion"
    );
    unsafe { kglite_cancel_token_free(doomed) };
    unsafe { kglite_cancel_token_free(survivor) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_cancelled_token_stays_cancelled_and_an_uncancelled_one_is_inert() {
    let session = session();
    let token = token();
    let quick = "MATCH (n:T) RETURN count(n) AS c";
    assert_eq!(
        run_read(session, quick, &options(token)).0,
        KgliteStatusCode::Ok
    );
    unsafe { kglite_cancel_token_cancel(token) };
    assert_eq!(
        run_read(session, quick, &options(token)).0,
        KgliteStatusCode::Cancelled
    );
    // An options block that predates the field never reads it.
    let mut old = options(token);
    old.struct_size = std::mem::offset_of!(KgliteExecuteOptions, cancel);
    assert_eq!(run_read(session, quick, &old).0, KgliteStatusCode::Ok);
    assert_eq!(
        unsafe { kglite_cancel_token_cancel(std::ptr::null()) },
        KgliteStatusCode::NullPointer
    );
    unsafe { kglite_cancel_token_free(std::ptr::null_mut()) };
    unsafe { kglite_cancel_token_free(token) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_transaction_statement_is_cancellable() {
    let session = session();
    let mut tx = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_session_begin(session, false, &mut tx, std::ptr::null_mut()) },
        KgliteStatusCode::Ok
    );
    let token = token();
    let address = token as usize;
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        unsafe { kglite_cancel_token_cancel(address as *const KgliteCancelToken) }
    });
    let q = cstr(LONG);
    let mut result = std::ptr::null_mut();
    let started = Instant::now();
    let status = unsafe {
        kglite_tx_execute(
            tx,
            q.as_ptr(),
            std::ptr::null(),
            &options(token),
            &mut result,
            std::ptr::null_mut(),
        )
    };
    thread.join().unwrap();
    assert_eq!(status, KgliteStatusCode::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));
    unsafe { kglite_tx_free(tx) };
    unsafe { kglite_cancel_token_free(token) };
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_cancelled_mutation_applies_nothing() {
    let session = session();
    let token = token();
    unsafe { kglite_cancel_token_cancel(token) };
    let q = cstr("MATCH (n:T) SET n.touched = true");
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        kglite_session_execute_mut_ex(
            session,
            q.as_ptr(),
            std::ptr::null(),
            &options(token),
            &mut result,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, KgliteStatusCode::Cancelled);
    let (status, _) = run_read(
        session,
        "MATCH (n:T) WHERE n.touched IS NOT NULL RETURN count(n) AS c",
        &options(std::ptr::null()),
    );
    assert_eq!(status, KgliteStatusCode::Ok);
    unsafe { kglite_cancel_token_free(token) };
    unsafe { kglite_session_free(session) };
}
