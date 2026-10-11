//! `LocalTime`, `Time` and `ZonedDateTime` through the C ABI: the `$localtime` /
//! `$time` / `$zdatetime` parameter tags, the natural ISO text in result JSON, and
//! equality by instant.

use std::ffi::{c_char, CStr, CString};

use kglite_c::{
    kglite_cypher_result_free, kglite_cypher_result_rows_json, kglite_free_string,
    kglite_graph_new, kglite_session_execute_read, kglite_session_free, kglite_session_new,
    KgliteCypherResult, KgliteSession, KgliteStatusCode,
};

fn row(query: &str, params: &str) -> serde_json::Value {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    unsafe { kglite_session_new(graph, &mut session) };
    let query_c = CString::new(query).unwrap();
    let params_c = CString::new(params).unwrap();
    let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        kglite_session_execute_read(
            session,
            query_c.as_ptr(),
            params_c.as_ptr(),
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
    rows[0].clone()
}

#[test]
fn time_tags_bind_as_typed_values_and_compare_by_instant() {
    let params = r#"{
        "l": {"$localtime": "10:00:00.5"},
        "z": {"$time": "10:00:00+01:00"},
        "u": {"$time": "09:00:00Z"},
        "later": {"$time": "09:35:14Z"}
    }"#;
    let out = row(
        "RETURN $l = localtime('10:00:00.500') AS local_same, $z = $u AS zoned_same, \
         $z < $later AS before, $l = $z AS mixed, toString($z) AS text, $z.offsetSeconds AS off",
        params,
    );
    assert_eq!(out["local_same"], true);
    assert_eq!(out["zoned_same"], true, "10:00+01:00 is 09:00Z");
    assert_eq!(out["before"], true);
    assert_eq!(out["mixed"], false);
    assert_eq!(out["text"], "10:00+01:00");
    assert_eq!(out["off"], 3600);
}

#[test]
fn result_json_renders_the_shortest_iso_text() {
    let out = row(
        "RETURN localtime('10:15:30.123456789') AS l, time('10:00:00.5-05:30') AS z, \
         time('21:40:00Z') AS utc",
        "{}",
    );
    assert_eq!(out["l"], "10:15:30.123456789");
    assert_eq!(out["z"], "10:00:00.500-05:30");
    assert_eq!(out["utc"], "21:40Z");
}

#[test]
fn a_malformed_time_tag_is_a_parameter_error() {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    unsafe { kglite_session_new(graph, &mut session) };
    let query = CString::new("RETURN $v AS v").unwrap();
    for bad in [
        r#"{"v": {"$localtime": "10:00+01:00"}}"#,
        r#"{"v": {"$time": "25:00"}}"#,
        r#"{"v": {"$time": "10:00[Europe/Oslo]"}}"#,
    ] {
        let params = CString::new(bad).unwrap();
        let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
        let mut error: *const c_char = std::ptr::null();
        let rc = unsafe {
            kglite_session_execute_read(
                session,
                query.as_ptr(),
                params.as_ptr(),
                &mut result,
                &mut error,
            )
        };
        assert_ne!(rc, KgliteStatusCode::Ok, "{bad} must be refused");
        unsafe { kglite_free_string(error) };
    }
    unsafe { kglite_session_free(session) };
}

#[test]
fn zoned_datetime_tags_keep_the_offset_and_compare_by_instant() {
    let params = r#"{
        "a": {"$zdatetime": "2020-01-02T03:04:05.25+02:00"},
        "b": {"$zdatetime": "2020-01-02T01:04:05.25Z"},
        "local": {"$datetime": "2020-01-02T01:04:05.25"},
        "off": {"$datetime": "2020-01-02T03:04:05.25+02:00"}
    }"#;
    let out = row(
        "RETURN $a = $b AS same, $a = $local AS local_same, $off = $a AS off_same, \
         $a.offsetSeconds AS off, $off.offsetSeconds AS off_from_datetime, \
         $local.offsetSeconds AS local_off, toString($a) AS text",
        params,
    );
    assert_eq!(out["same"], true, "03:04:05.25+02:00 is 01:04:05.25Z");
    assert_eq!(out["local_same"], true, "a local datetime reads as UTC");
    assert_eq!(out["off_same"], true);
    assert_eq!(out["off"], 7200);
    assert_eq!(
        out["off_from_datetime"], 7200,
        "$datetime with an offset is zoned"
    );
    assert_eq!(out["local_off"], serde_json::Value::Null);
    assert_eq!(out["text"], "2020-01-02T03:04:05.250+02:00");
}

#[test]
fn result_json_renders_zoned_datetimes_with_their_offset() {
    let out = row(
        "RETURN datetime('2020-01-02T03:04:05.5-05:30') AS z, datetime('2020-01-02T03:04Z') AS utc, \
         localdatetime('2020-01-02T03:04:05') AS l",
        "{}",
    );
    assert_eq!(out["z"], "2020-01-02T03:04:05.500-05:30");
    assert_eq!(out["utc"], "2020-01-02T03:04Z");
    assert_eq!(out["l"], "2020-01-02T03:04:05");
}

#[test]
fn a_malformed_zoned_tag_is_a_parameter_error() {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    unsafe { kglite_session_new(graph, &mut session) };
    let query = CString::new("RETURN $v AS v").unwrap();
    for bad in [
        r#"{"v": {"$zdatetime": "2020-01-02T03:04+19:00"}}"#,
        r#"{"v": {"$zdatetime": "2020-01-02T03:04[Mars/Base]"}}"#,
        r#"{"v": {"$zdatetime": "nonsense"}}"#,
        r#"{"v": {"$zdatetime": 5}}"#,
    ] {
        let params = CString::new(bad).unwrap();
        let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
        let mut error: *const c_char = std::ptr::null();
        let rc = unsafe {
            kglite_session_execute_read(
                session,
                query.as_ptr(),
                params.as_ptr(),
                &mut result,
                &mut error,
            )
        };
        assert_ne!(rc, KgliteStatusCode::Ok, "{bad} must be refused");
        unsafe { kglite_free_string(error) };
    }
    unsafe { kglite_session_free(session) };
}

/// A named zone rides the existing `$zdatetime` tag as ISO text with the zone in
/// brackets: no new symbol and no new tag, so a prebuilt consumer is unaffected.
/// The zone never takes part in equality, only in the text and the `timezone`
/// accessor.
#[test]
fn a_named_zone_rides_the_zdatetime_tag_with_its_name() {
    let params = r#"{
        "oslo": {"$zdatetime": "2026-07-15T12:00+02:00[Europe/Oslo]"},
        "utc": {"$zdatetime": "2026-07-15T10:00Z"}
    }"#;
    let out = row(
        "RETURN $oslo = $utc AS same_instant, $oslo.timezone AS zone, toString($oslo) AS text, \
         $oslo AS echoed, \
         datetime('2026-03-29T02:30[Europe/Oslo]') AS gap",
        params,
    );
    assert_eq!(out["same_instant"], true);
    assert_eq!(out["zone"], "Europe/Oslo");
    assert_eq!(out["text"], "2026-07-15T12:00+02:00[Europe/Oslo]");
    assert_eq!(out["echoed"], "2026-07-15T12:00+02:00[Europe/Oslo]");
    assert_eq!(out["gap"], "2026-03-29T03:30+02:00[Europe/Oslo]");
}
