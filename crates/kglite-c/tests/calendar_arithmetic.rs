//! Date and datetime arithmetic with calendar durations through the C ABI.

use std::ffi::{c_char, CStr, CString};

use kglite_c::{
    kglite_cypher_result_free, kglite_cypher_result_rows_json, kglite_free_string,
    kglite_graph_new, kglite_session_execute_read, kglite_session_free, kglite_session_new,
    KgliteCypherResult, KgliteSession, KgliteStatusCode,
};

fn one_value(query: &str) -> serde_json::Value {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    unsafe { kglite_session_new(graph, &mut session) };
    let query_c = CString::new(query).unwrap();
    let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        kglite_session_execute_read(
            session,
            query_c.as_ptr(),
            std::ptr::null(),
            &mut result,
            &mut error,
        )
    };
    assert_eq!(rc, KgliteStatusCode::Ok, "{query}");
    let json = unsafe { kglite_cypher_result_rows_json(result) };
    let rows: serde_json::Value =
        serde_json::from_str(unsafe { CStr::from_ptr(json) }.to_str().unwrap()).unwrap();
    unsafe {
        kglite_free_string(json);
        kglite_cypher_result_free(result);
        kglite_session_free(session);
    }
    rows[0]["v"].clone()
}

#[test]
fn a_date_shifts_by_calendar_months() {
    for (query, expected) in [
        ("date('2015-06-15') - duration({months: 11})", "2014-07-15"),
        ("date('2016-02-29') + duration({years: 1})", "2017-02-28"),
        ("date('2024-01-31') + duration({months: 1})", "2024-02-29"),
        (
            "date('2024-01-30') + duration({months: 1, days: 2})",
            "2024-03-02",
        ),
        ("date('2024-01-15') + duration({months: 1})", "2024-02-15"),
    ] {
        assert_eq!(
            one_value(&format!("RETURN {query} AS v")),
            expected,
            "{query}"
        );
    }
}

#[test]
fn a_datetime_shifts_by_months_then_days_then_time() {
    assert_eq!(
        one_value("RETURN datetime('2024-01-31T10:30:00') + duration({months: 1, hours: 2}) AS v"),
        "2024-02-29T12:30:00"
    );
}
