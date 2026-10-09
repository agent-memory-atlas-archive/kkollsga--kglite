//! Tagged result encoding: every typed value a result can hold renders as the
//! tag a query parameter accepts, so a cell read back and bound again returns
//! the value it came from.

use kglite_c::{
    kglite_cypher_result_free, kglite_cypher_result_rows_json, kglite_free_string,
    kglite_graph_new, kglite_session_execute_mut, kglite_session_execute_read, kglite_session_free,
    kglite_session_new, kglite_session_set_result_encoding, KgliteCypherResult, KgliteSession,
    KgliteStatusCode,
};
use std::ffi::{c_char, CStr, CString};

const NATURAL: u32 = 0;
const TAGGED: u32 = 1;

fn session(encoding: u32) -> *mut KgliteSession {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    assert_eq!(
        unsafe { kglite_session_new(graph, &mut session) },
        KgliteStatusCode::Ok
    );
    assert_eq!(
        unsafe { kglite_session_set_result_encoding(session, encoding) },
        KgliteStatusCode::Ok
    );
    session
}

fn run(session: *mut KgliteSession, query: &str, params: &str, mutate: bool) -> serde_json::Value {
    let (query_c, params_c) = (CString::new(query).unwrap(), CString::new(params).unwrap());
    let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        if mutate {
            kglite_session_execute_mut(
                session,
                query_c.as_ptr(),
                params_c.as_ptr(),
                &mut result,
                &mut error,
            )
        } else {
            kglite_session_execute_read(
                session,
                query_c.as_ptr(),
                params_c.as_ptr(),
                &mut result,
                &mut error,
            )
        }
    };
    if rc != KgliteStatusCode::Ok {
        let message = unsafe { CStr::from_ptr(error) }
            .to_str()
            .unwrap()
            .to_owned();
        panic!("{query:?}: {rc:?}: {message}");
    }
    let rows = unsafe { kglite_cypher_result_rows_json(result) };
    let text = unsafe { CStr::from_ptr(rows) }.to_str().unwrap().to_owned();
    unsafe {
        kglite_free_string(rows);
        kglite_cypher_result_free(result);
    }
    serde_json::from_str(&text).unwrap()
}

/// Non-finite floats have no Cypher literal; they enter as tagged parameters.
const SEED: &str = r#"{"nan":{"$float":"NaN"},"inf":{"$float":"inf"},"ninf":{"$float":"-inf"}}"#;

/// `(expression, the tagged JSON the cell must render as)`.
fn cases() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        ("date('2020-01-02')", json!({"$date": "2020-01-02"})),
        (
            "datetime('2020-01-02T03:04:05.250')",
            json!({"$datetime": "2020-01-02T03:04:05.250"}),
        ),
        (
            "duration({months: 1, days: 2, seconds: 3})",
            json!({"$duration": {"months": 1, "days": 2, "seconds": 3}}),
        ),
        (
            "point({latitude: 60.5, longitude: -5.25})",
            json!({"$point": {"lat": 60.5, "lon": -5.25}}),
        ),
        ("$nan", json!({"$float": "NaN"})),
        ("$inf", json!({"$float": "inf"})),
        ("$ninf", json!({"$float": "-inf"})),
        (
            "[date('2020-01-02'), {at: date('2021-03-04'), n: 1}, [$inf, null]]",
            json!([
                {"$date": "2020-01-02"},
                {"at": {"$date": "2021-03-04"}, "n": 1},
                [{"$float": "inf"}, null]
            ]),
        ),
        // A map that spells a tag is escaped.
        ("{`$date`: 'x'}", json!({"$map": {"$date": "x"}})),
    ]
}

#[test]
fn every_typed_value_renders_as_its_tag_and_round_trips_as_a_parameter() {
    let session = session(TAGGED);
    for (expression, expected) in cases() {
        let rows = run(session, &format!("RETURN {expression} AS x"), SEED, false);
        assert_eq!(rows[0]["x"], expected, "{expression}");
        // Result -> parameter -> result is the identity.
        let params = serde_json::json!({"x": rows[0]["x"]}).to_string();
        let again = run(session, "RETURN $x AS x", &params, false);
        assert_eq!(again, rows, "{expression}");
    }
    unsafe { kglite_session_free(session) };
}

#[test]
fn stored_properties_and_nodes_carry_tags() {
    let session = session(TAGGED);
    run(
        session,
        "CREATE (:N {id: 1, born: date('2020-01-02'), at: datetime('2020-01-02T03:04:05'), \
         span: duration({days: 1}), spot: point({latitude: 1.5, longitude: 2.5})})",
        "{}",
        true,
    );
    let rows = run(session, "MATCH (n:N) RETURN n.born AS born, n", "{}", false);
    assert_eq!(rows[0]["born"], serde_json::json!({"$date": "2020-01-02"}));
    let properties = &rows[0]["n"]["properties"];
    assert_eq!(
        properties["born"],
        serde_json::json!({"$date": "2020-01-02"})
    );
    assert_eq!(properties["span"]["$duration"]["days"], 1);
    assert_eq!(
        properties["spot"],
        serde_json::json!({"$point": {"lat": 1.5, "lon": 2.5}})
    );
    unsafe { kglite_session_free(session) };
}

#[test]
fn natural_results_stay_untagged_and_lose_the_type() {
    // The default is the published rendering, and it is what the tagged round
    // trip above would fail on were the encoder not in use.
    let natural = session(NATURAL);
    let rows = run(natural, "RETURN date('2020-01-02') AS x", "{}", false);
    assert_eq!(rows[0]["x"], serde_json::json!("2020-01-02"));
    let point = run(
        natural,
        "RETURN point({latitude: 1.0, longitude: 2.0}) AS x",
        "{}",
        false,
    );
    assert_eq!(
        point[0]["x"],
        serde_json::json!({"latitude": 1.0, "longitude": 2.0})
    );
    let nan = run(natural, "RETURN $nan AS x", SEED, false);
    assert_eq!(nan[0]["x"], serde_json::Value::Null);

    // Bind the natural cell back: it is a string now, not a date.
    let params = serde_json::json!({"x": rows[0]["x"]}).to_string();
    let tagged = session(TAGGED);
    let rebound = run(tagged, "RETURN $x AS x", &params, false);
    assert_ne!(rebound[0]["x"], serde_json::json!({"$date": "2020-01-02"}));
    unsafe {
        kglite_session_free(natural);
        kglite_session_free(tagged);
    }
}

#[test]
fn unknown_encoding_values_are_refused() {
    let session = session(NATURAL);
    assert_eq!(
        unsafe { kglite_session_set_result_encoding(session, 7) },
        KgliteStatusCode::InvalidArgument
    );
    assert_eq!(
        unsafe { kglite_session_set_result_encoding(std::ptr::null(), 1) },
        KgliteStatusCode::NullPointer
    );
    unsafe { kglite_session_free(session) };
}
