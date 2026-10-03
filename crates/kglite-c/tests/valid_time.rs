//! Valid-time statements through the C ABI: the echo in the diagnostics
//! JSON, and the streaming aggregate shapes answering as the eager path.

use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};

use kglite::api::session::{execute_read, ExecuteOptions};
use kglite_c::{
    kglite_cypher_result_diagnostics_json, kglite_cypher_result_free,
    kglite_cypher_result_rows_json, kglite_free_string, kglite_graph_new,
    kglite_session_execute_mut, kglite_session_execute_read, kglite_session_execute_read_batch,
    kglite_session_free, kglite_session_new, KgliteCypherResult, KgliteSession, KgliteStatusCode,
};

/// A session over a fresh graph after `statements`.
fn session_after(statements: &[&str]) -> *mut KgliteSession {
    let graph = kglite_graph_new();
    let mut session: *mut KgliteSession = std::ptr::null_mut();
    unsafe { kglite_session_new(graph, &mut session) };
    for statement in statements {
        let query = CString::new(*statement).unwrap();
        let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
        let mut error: *const c_char = std::ptr::null();
        let rc = unsafe {
            kglite_session_execute_mut(
                session,
                query.as_ptr(),
                std::ptr::null(),
                &mut result,
                &mut error,
            )
        };
        assert_eq!(rc, KgliteStatusCode::Ok, "{statement}");
        unsafe { kglite_cypher_result_free(result) };
    }
    session
}

/// `(rows, diagnostics)` of a read, both as parsed JSON.
fn read(session: *mut KgliteSession, query: &str) -> (serde_json::Value, serde_json::Value) {
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
    let parse = |json: *const c_char| {
        let value = serde_json::from_str(unsafe { CStr::from_ptr(json) }.to_str().unwrap());
        unsafe { kglite_free_string(json) };
        value.unwrap()
    };
    let rows = parse(unsafe { kglite_cypher_result_rows_json(result) });
    let diagnostics = parse(unsafe { kglite_cypher_result_diagnostics_json(result) });
    unsafe { kglite_cypher_result_free(result) };
    (rows, diagnostics)
}

const WELLS: [&str; 2] = [
    "CREATE (:Well {id: 1, vf: date('2000-01-01'), vt: date('2010-01-01')}), \
     (:Well {id: 2, vf: date('2005-01-01')})",
    "CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'}) \
     YIELD declared RETURN declared",
];

#[test]
fn the_diagnostics_json_carries_the_valid_time_echo() {
    let session = session_after(&WELLS);
    let (rows, diagnostics) = read(
        session,
        "FOR VALID_TIME AS OF date('2003-06-30') MATCH (w:Well) RETURN w.id AS id",
    );
    assert_eq!(rows, serde_json::json!([{"id": 1}]));
    let echo = &diagnostics["temporal"];
    assert_eq!(echo["axis"], "VALID_TIME");
    assert_eq!(echo["instant"], "2003-06-30");
    assert_eq!(echo["targets"], serde_json::json!(["(:Well)"]));
    // Well 2 has not started at the instant.
    assert_eq!(echo["hidden"], serde_json::json!({"(:Well)": 1}));
    assert_eq!(echo["endpoint_invalid"], 0);
    assert_eq!(echo["route"], "guarded");
    assert_eq!(echo["retrieval"], serde_json::Value::Null);
    assert_eq!(echo["slice"], false);
    assert!(echo["session_version"].is_u64(), "{echo}");

    // Both wells are valid in 2006: the filter removes nothing.
    let (_, timeless) = read(
        session,
        "FOR VALID_TIME AS OF date('2006-06-30') MATCH (w:Well) RETURN w.id AS id",
    );
    assert_eq!(timeless["temporal"]["route"], "plain");

    // No context, no key: the JSON of every other statement is unchanged.
    let (_, plain) = read(session, "MATCH (w:Well) RETURN w.id AS id");
    assert!(plain.get("temporal").is_none(), "{plain}");

    // A batch reads one snapshot, each entry with its own echo.
    let batch = CString::new(
        serde_json::json!([
            {"query": "FOR VALID_TIME AS OF date('2003-06-30') MATCH (w:Well) RETURN count(w) AS n"},
            {"query": "FOR VALID_TIME AS OF date('2006-06-30') MATCH (w:Well) RETURN count(w) AS n"},
        ])
        .to_string(),
    )
    .unwrap();
    let mut output: *const c_char = std::ptr::null();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        kglite_session_execute_read_batch(session, batch.as_ptr(), &mut output, &mut error)
    };
    assert_eq!(rc, KgliteStatusCode::Ok);
    let batch: serde_json::Value =
        serde_json::from_str(unsafe { CStr::from_ptr(output) }.to_str().unwrap()).unwrap();
    unsafe { kglite_free_string(output) };
    assert_eq!(batch[0]["rows"], serde_json::json!([{"n": 1}]));
    assert_eq!(batch[1]["rows"], serde_json::json!([{"n": 2}]));
    assert_eq!(batch[0]["diagnostics"]["temporal"]["instant"], "2003-06-30");
    unsafe { kglite_session_free(session) };
}

/// The doubled context is the parser's error, as for every text surface.
#[test]
fn a_doubled_context_is_a_syntax_error() {
    let session = session_after(&WELLS);
    let query = CString::new(
        "FOR VALID_TIME AS OF date('2003-06-30') FOR VALID_TIME AS OF date('2004-01-01') \
         MATCH (w:Well) RETURN w.id",
    )
    .unwrap();
    let mut result: *mut KgliteCypherResult = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    let rc = unsafe {
        kglite_session_execute_read(
            session,
            query.as_ptr(),
            std::ptr::null(),
            &mut result,
            &mut error,
        )
    };
    assert_ne!(rc, KgliteStatusCode::Ok);
    let message = unsafe { CStr::from_ptr(error) }
        .to_str()
        .unwrap()
        .to_string();
    unsafe {
        kglite_free_string(error);
        kglite_session_free(session);
    }
    assert!(message.contains("this one has two"), "{message}");
}

const NETWORK: [&str; 3] = [
    "CREATE (s1:Stop {id: 1}), (s2:Stop {id: 2, vf: date('2000-01-01'), vt: date('2005-01-01')}), \
     (s3:Stop {id: 3}), (s4:Stop {id: 4}), (s5:Stop {id: 5}), \
     (s1)-[:LINK]->(s2), (s2)-[:LINK]->(s3), \
     (s1)-[:LINK {since: date('2000-01-01'), until: date('2005-01-01')}]->(s3), \
     (s1)-[:LINK]->(s4), \
     (s4)-[:LINK {since: date('2000-01-01'), until: date('2005-01-01')}]->(s5), \
     (s4)-[:LINK {since: date('2006-01-01')}]->(s5), (s5)-[:LINK]->(s3)",
    "CALL db.temporal.declare({node: 'Stop', from: 'vf', to: 'vt', convention: 'closed'}) \
     YIELD declared RETURN declared",
    "CALL db.temporal.declare({relationship: 'LINK', from: 'since', to: 'until', \
     convention: 'half_open'}) YIELD declared RETURN declared",
];

const STREAMING_SHAPES: [&str; 7] = [
    "MATCH (:Stop {id: 1})-[:LINK*1..3]->(t) RETURN count(DISTINCT t) AS c",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, count(t) AS c",
    "MATCH (s:Stop)-[r:LINK]->(t) RETURN count(DISTINCT t.id) AS d, count(*) AS c, count(r) AS r",
    "MATCH (s:Stop)-[:LINK]->(t) WITH s, count(t) AS c WHERE c > 0 RETURN s.id AS s, c",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, count(*) AS c ORDER BY s DESC LIMIT 2",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN min(t.id) AS lo, max(t.id) AS hi, sum(t.id) AS s, avg(t.id) AS a",
    "MATCH (s:Stop)-[:LINK]->(t) RETURN s.id AS s, sum(COUNT { (t)-[:LINK]->() }) AS n",
];

/// The C read path answers each streaming shape as the engine's eager path
/// does over the same graph, under a context and without one.
#[test]
fn the_streaming_shapes_answer_as_the_eager_path() {
    let session = session_after(&NETWORK);
    let mut reference = kglite::api::DirGraph::new();
    let params = HashMap::new();
    let eager = ExecuteOptions::eager(&params);
    for statement in NETWORK {
        kglite::api::session::execute_mut(&mut reference, statement, &eager).unwrap();
    }
    let sorted = |rows: serde_json::Value, ordered: bool| {
        let mut rows = rows.as_array().unwrap().clone();
        if !ordered {
            rows.sort_by_key(|row| row.to_string());
        }
        rows
    };
    for shape in STREAMING_SHAPES {
        let ordered = shape.contains("ORDER BY");
        for query in [
            shape.to_string(),
            format!("FOR VALID_TIME AS OF date('2008-01-01') {shape}"),
        ] {
            let (served, _) = read(session, &query);
            let outcome = execute_read(&reference, &query, &eager).unwrap();
            let expected: Vec<serde_json::Value> = outcome
                .result
                .rows
                .iter()
                .map(|row| {
                    let cells = outcome
                        .result
                        .columns
                        .iter()
                        .zip(row)
                        .map(|(column, value)| {
                            (
                                column.clone(),
                                kglite::api::param::kglite_value_to_json(value),
                            )
                        });
                    serde_json::Value::Object(cells.collect())
                })
                .collect();
            assert_eq!(
                sorted(served, ordered),
                sorted(serde_json::Value::Array(expected), ordered),
                "{query}"
            );
        }
    }
    unsafe { kglite_session_free(session) };
}
