//! One rule for NaN and infinities over the JSON C ABI.
//!
//! JSON has no spelling for them, so a parameter takes the `{"$float": ...}`
//! tag and a result renders it as the same tag once the session opts in; the
//! default result rendering stays `null`. A parameter is never nulled.

use kglite_c::{
    kglite_cypher_result_free, kglite_cypher_result_rows_json, kglite_free_string,
    kglite_graph_new, kglite_session_execute_mut, kglite_session_execute_read,
    kglite_session_execute_read_batch, kglite_session_free, kglite_session_new,
    kglite_session_set_result_encoding, KgliteCypherResult, KgliteSession, KgliteStatusCode,
};
use std::ffi::{c_char, CStr, CString};

fn session() -> *mut KgliteSession {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    let rc = unsafe { kglite_session_new(graph, &mut session) };
    assert_eq!(rc, KgliteStatusCode::Ok);
    session
}

fn take(ptr: *const c_char) -> String {
    assert!(!ptr.is_null());
    let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { kglite_free_string(ptr) };
    text
}

fn run(
    session: *mut KgliteSession,
    query: &str,
    params: &str,
    mutate: bool,
) -> (KgliteStatusCode, String) {
    let query = CString::new(query).unwrap();
    let params = CString::new(params).unwrap();
    let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        if mutate {
            kglite_session_execute_mut(
                session,
                query.as_ptr(),
                params.as_ptr(),
                &mut result,
                &mut error,
            )
        } else {
            kglite_session_execute_read(
                session,
                query.as_ptr(),
                params.as_ptr(),
                &mut result,
                &mut error,
            )
        }
    };
    if rc != KgliteStatusCode::Ok {
        return (rc, take(error));
    }
    let rows = take(unsafe { kglite_cypher_result_rows_json(result) });
    unsafe { kglite_cypher_result_free(result) };
    (rc, rows)
}

fn echo(session: *mut KgliteSession, params: &str) -> String {
    let (rc, rows) = run(session, "RETURN $x AS x", params, false);
    assert_eq!(rc, KgliteStatusCode::Ok, "{rows}");
    rows
}

const TAGGED: [(&str, &str); 3] = [("NaN", "NaN"), ("inf", "inf"), ("-inf", "-inf")];

#[test]
fn tagged_parameters_bind_the_value_and_results_default_to_null() {
    let session = session();
    for (payload, _) in TAGGED {
        let rows = echo(session, &format!(r#"{{"x":{{"$float":"{payload}"}}}}"#));
        assert_eq!(rows, r#"[{"x":null}]"#, "legacy default result rendering");
    }
    unsafe { kglite_session_free(session) };
}

#[test]
fn opted_in_results_render_the_tag_and_round_trip_as_parameters() {
    let session = session();
    unsafe { kglite_session_set_result_encoding(session, 1) };
    for (payload, _) in TAGGED {
        let rows = echo(session, &format!(r#"{{"x":{{"$float":"{payload}"}}}}"#));
        assert_eq!(rows, format!(r#"[{{"x":{{"$float":"{payload}"}}}}]"#));
        // The tag read back binds the same value again.
        let again = echo(session, &format!(r#"{{"x":{}}}"#, &rows[6..rows.len() - 2]));
        assert_eq!(again, rows);
    }
    // Negative zero and ordinary floats stay plain numbers.
    assert_eq!(echo(session, r#"{"x":-0.0}"#), r#"[{"x":-0.0}]"#);
    assert_eq!(echo(session, r#"{"x":1.5}"#), r#"[{"x":1.5}]"#);
    // Nested in a list and a map.
    let rows = echo(
        session,
        r#"{"x":[{"$float":"NaN"},{"k":{"$float":"-inf"}}]}"#,
    );
    assert_eq!(
        rows,
        r#"[{"x":[{"$float":"NaN"},{"k":{"$float":"-inf"}}]}]"#
    );
    unsafe { kglite_session_free(session) };
}

#[test]
fn opted_in_batch_results_use_the_tag() {
    let session = session();
    unsafe { kglite_session_set_result_encoding(session, 1) };
    let queries =
        CString::new(r#"[{"query":"RETURN $x AS x","params":{"x":{"$float":"inf"}}}]"#).unwrap();
    let mut out: *const c_char = std::ptr::null();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        kglite_session_execute_read_batch(session, queries.as_ptr(), &mut out, &mut error)
    };
    assert_eq!(rc, KgliteStatusCode::Ok);
    let parsed: serde_json::Value = serde_json::from_str(&take(out)).unwrap();
    assert_eq!(
        parsed[0]["rows"][0]["x"],
        serde_json::json!({"$float": "inf"})
    );
    unsafe { kglite_session_free(session) };
}

#[test]
fn stored_non_finite_property_renders_per_the_session_choice() {
    let session = session();
    let (rc, msg) = run(
        session,
        "CREATE (:N {v: $x})",
        r#"{"x":{"$float":"NaN"}}"#,
        true,
    );
    assert_eq!(rc, KgliteStatusCode::Ok, "{msg}");
    let (_, plain) = run(session, "MATCH (n:N) RETURN n.v AS v", "{}", false);
    assert_eq!(plain, r#"[{"v":null}]"#);
    unsafe { kglite_session_set_result_encoding(session, 1) };
    let (_, tagged) = run(session, "MATCH (n:N) RETURN n.v AS v, n AS n", "{}", false);
    assert!(tagged.contains(r#""v":{"$float":"NaN"}"#), "{tagged}");
    assert_eq!(
        tagged.matches(r#""v":{"$float":"NaN"}"#).count(),
        2,
        "{tagged}"
    );
    unsafe { kglite_session_free(session) };
}

#[test]
fn malformed_float_tag_is_refused_never_nulled() {
    let session = session();
    for params in [
        r#"{"x":{"$float":"nan"}}"#,
        r#"{"x":{"$float":1}}"#,
        r#"{"x":[{"$float":"Infinity"}]}"#,
    ] {
        let (rc, message) = run(session, "RETURN $x AS x", params, false);
        assert_eq!(rc, KgliteStatusCode::InvalidArgument, "{params}: {message}");
        assert!(message.contains("float"), "{message}");
    }
    unsafe { kglite_session_free(session) };
}
