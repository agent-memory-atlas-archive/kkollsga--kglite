//! Issue #222 acceptance criteria over the C ABI. The Python and Bolt
//! surfaces run the same criteria in `tests/test_ontology_acceptance.py`;
//! AC10 pins the same rule/entity-type/property tuples in both places.
//!
//! The ABI reports a refusal as status 22 plus a message; the structured
//! `(rule, entity_type, property)` triple rides the declaration report
//! (AC9) and is named in the write-refusal message (AC7).

use kglite_c::*;
use std::ffi::{c_char, CStr, CString};

const ONTOLOGY: &str = r#"{
  "classes": {"Person": {"required_properties": ["name"], "property_types": {"name": "string"},
                         "enforcement": "LEVEL"}, "Company": {}},
  "relationships": {"WORKS_AT": {"domain": "Person", "range": "Company", "enforcement": "LEVEL"}}
}"#;

const Q_NO_NAME: &str = "CREATE (:Person {id: 2})";
const Q_NAME_5: &str = "CREATE (:Person {id: 3, name: 5})";
const Q_REVERSED: &str =
    "CREATE (:Company {id: 11, name: 'Acme'})-[:WORKS_AT]->(:Person {id: 4, name: 'B'})";

fn ontology(level: &str) -> String {
    ONTOLOGY.replace("LEVEL", level)
}

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kglite-c-ontology-acceptance-{}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn take(slot: *const c_char) -> Option<String> {
    if slot.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(slot) }
        .to_str()
        .unwrap()
        .to_string();
    unsafe { kglite_free_string(slot) };
    Some(text)
}

fn new_session() -> *mut KgliteSession {
    let mut session = std::ptr::null_mut();
    // Pointers are live locals for these synchronous calls.
    unsafe {
        assert_eq!(
            kglite_session_new(kglite_graph_new(), &mut session),
            KgliteStatusCode::Ok
        );
    }
    session
}

fn session_from_file(path: &std::path::Path) -> *mut KgliteSession {
    let path_c = CString::new(path.to_str().unwrap()).unwrap();
    let (mut graph, mut session) = (std::ptr::null_mut(), std::ptr::null_mut());
    let mut error: *const c_char = std::ptr::null();
    unsafe {
        assert_eq!(
            kglite_load_file(path_c.as_ptr(), &mut graph, &mut error),
            KgliteStatusCode::Ok
        );
        assert_eq!(
            kglite_session_new(graph, &mut session),
            KgliteStatusCode::Ok
        );
    }
    session
}

fn declare(
    session: *mut KgliteSession,
    json: &str,
) -> (KgliteStatusCode, Option<String>, Option<String>) {
    let json = CString::new(json).unwrap();
    let mut warnings: *const c_char = std::ptr::null();
    let mut error: *const c_char = std::ptr::null();
    let status = unsafe {
        kglite_session_define_ontology(session, json.as_ptr(), &mut warnings, &mut error)
    };
    (status, take(warnings), take(error))
}

/// A mutating query: status, error message, and the statement's warnings.
fn mutate(
    session: *mut KgliteSession,
    query: &str,
) -> (KgliteStatusCode, Option<String>, Vec<String>) {
    let q = CString::new(query).unwrap();
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    unsafe {
        let status = kglite_session_execute_mut(
            session,
            q.as_ptr(),
            std::ptr::null(),
            &mut result,
            &mut error,
        );
        let mut warnings = Vec::new();
        if !result.is_null() {
            if let Some(json) = take(kglite_cypher_result_diagnostics_json(result)) {
                let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
                warnings = parsed["warnings"]
                    .as_array()
                    .map(|w| w.iter().map(|s| s.as_str().unwrap().to_string()).collect())
                    .unwrap_or_default();
            }
            kglite_cypher_result_free(result);
        }
        (status, take(error), warnings)
    }
}

fn read_rows(session: *mut KgliteSession, query: &str) -> serde_json::Value {
    let q = CString::new(query).unwrap();
    let mut result = std::ptr::null_mut();
    let mut error: *const c_char = std::ptr::null();
    unsafe {
        assert_eq!(
            kglite_session_execute_read(
                session,
                q.as_ptr(),
                std::ptr::null(),
                &mut result,
                &mut error
            ),
            KgliteStatusCode::Ok,
            "{:?}",
            take(error)
        );
        let rows = take(kglite_cypher_result_rows_json(result)).unwrap();
        kglite_cypher_result_free(result);
        serde_json::from_str(&rows).unwrap()
    }
}

fn count(session: *mut KgliteSession) -> i64 {
    read_rows(session, "MATCH (n) RETURN count(n) AS c")[0]["c"]
        .as_i64()
        .unwrap()
}

fn refused(session: *mut KgliteSession, query: &str) -> String {
    let (status, error, _) = mutate(session, query);
    assert_eq!(status, KgliteStatusCode::OntologyViolation, "{query}");
    assert_eq!(status as i32, 22);
    error.expect("a refusal carries a message")
}

fn declared(level: &str) -> *mut KgliteSession {
    let session = new_session();
    let (status, _, error) = declare(session, &ontology(level));
    assert_eq!(status, KgliteStatusCode::Ok, "{error:?}");
    session
}

fn shows_issue_ontology(session: *mut KgliteSession) -> bool {
    let rows = read_rows(session, "SHOW ONTOLOGY");
    let names: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    names.contains(&"Person") && names.contains(&"WORKS_AT")
}

#[test]
fn ac1_declared_visible_and_persisted_across_a_reload() {
    let dir = TestDirectory::new("ac1");
    let session = declared("error");
    assert!(shows_issue_ontology(session));
    let path = dir.0.join("g.kgl");
    let path_c = CString::new(path.to_str().unwrap()).unwrap();
    let mut error: *const c_char = std::ptr::null();
    unsafe {
        assert_eq!(
            kglite_session_save(session, path_c.as_ptr(), 0, &mut error),
            KgliteStatusCode::Ok
        );
        kglite_session_free(session);
    }
    let reopened = session_from_file(&path);
    assert!(shows_issue_ontology(reopened));
    refused(reopened, Q_NO_NAME);
    unsafe { kglite_session_free(reopened) };
}

#[test]
fn ac2_to_ac5_and_ac7_each_rule_refuses_with_a_specific_message() {
    let session = declared("error");
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1, name: 'A'})").0,
        KgliteStatusCode::Ok
    );
    assert_eq!(count(session), 1);

    let message = refused(session, Q_NO_NAME);
    assert!(
        message.contains("required_property") || message.contains("requires"),
        "{message}"
    );
    assert!(
        message.contains("Person") && message.contains("name"),
        "{message}"
    );

    let message = refused(session, Q_NAME_5);
    assert!(
        message.contains("Person") && message.contains("name"),
        "{message}"
    );

    let message = refused(session, Q_REVERSED);
    assert!(
        message.contains("domain") && message.contains("WORKS_AT"),
        "{message}"
    );

    assert_eq!(count(session), 1, "refused writes persist nothing");
    unsafe { kglite_session_free(session) };
}

#[test]
fn ac6_a_late_violation_rolls_back_the_whole_batch() {
    let session = declared("error");
    let batch = CString::new(
        serde_json::json!([
            {"query": "CREATE (:Person {id: 1, name: 'A'})"},
            {"query": "CREATE (:Person {id: 2, name: 'B'})"},
            {"query": Q_NO_NAME},
        ])
        .to_string(),
    )
    .unwrap();
    let mut out: *const c_char = std::ptr::null();
    let mut error: *const c_char = std::ptr::null();
    let status =
        unsafe { kglite_session_execute_mut_batch(session, batch.as_ptr(), &mut out, &mut error) };
    assert_eq!(status, KgliteStatusCode::OntologyViolation);
    assert!(take(out).is_none());
    assert!(take(error).unwrap().contains("Person"));
    assert_eq!(count(session), 0);
    unsafe { kglite_session_free(session) };
}

#[test]
fn ac8_warn_accepts_and_reports() {
    let session = declared("warn");
    let (status, _, warnings) = mutate(session, Q_NO_NAME);
    assert_eq!(status, KgliteStatusCode::Ok);
    assert_eq!(count(session), 1);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("Person") && w.contains("name")),
        "{warnings:?}"
    );
    let (_, _, clean) = mutate(session, "CREATE (:Person {id: 5, name: 'ok'})");
    assert!(clean.is_empty(), "{clean:?}");
    unsafe { kglite_session_free(session) };
}

#[test]
fn ac9_and_ac10_declaring_over_violating_data_is_status_22_with_the_report() {
    let session = new_session();
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1})").0,
        KgliteStatusCode::Ok
    );
    let (status, report, error) = declare(session, &ontology("error"));
    assert_eq!(status, KgliteStatusCode::OntologyViolation);
    let report: serde_json::Value = serde_json::from_str(&report.unwrap()).unwrap();
    // The shared AC10 tuple: (required_property, node, Person, name).
    assert_eq!(report[0]["rule"], "required_property");
    assert_eq!(report[0]["entity"], "node");
    assert_eq!(report[0]["entity_type"], "Person");
    assert_eq!(report[0]["property"], "name");
    assert_eq!(report[0]["count"], 1);
    assert!(error.unwrap().contains("Person"));
    assert_eq!(
        read_rows(session, "SHOW ONTOLOGY")
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(mutate(session, Q_NO_NAME).0, KgliteStatusCode::Ok);
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_declaration_refused_through_the_procedure_is_also_status_22() {
    let session = new_session();
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1})").0,
        KgliteStatusCode::Ok
    );
    let (status, error, _) = mutate(
        session,
        "CALL db.ontology.declare({classes: {Person: {required_properties: ['name'], \
         enforcement: 'error'}}})",
    );
    assert_eq!(status, KgliteStatusCode::OntologyViolation);
    assert!(error.unwrap().contains("Person"));
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_backup_of_an_error_graph_keeps_enforcement() {
    let dir = TestDirectory::new("backup");
    let session = declared("error");
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1, name: 'A'})").0,
        KgliteStatusCode::Ok
    );
    let dest = dir.0.join("copy.kgl");
    let dest_c = CString::new(dest.to_str().unwrap()).unwrap();
    let mut report: *const c_char = std::ptr::null();
    let mut error: *const c_char = std::ptr::null();
    unsafe {
        assert_eq!(
            kglite_session_backup(
                session,
                dest_c.as_ptr(),
                std::ptr::null(),
                &mut report,
                &mut error
            ),
            KgliteStatusCode::Ok,
            "{:?}",
            take(error)
        );
        take(report);
        kglite_session_free(session);
    }
    let reopened = session_from_file(&dest);
    assert!(shows_issue_ontology(reopened));
    refused(reopened, Q_NO_NAME);
    assert_eq!(count(reopened), 1);
    unsafe { kglite_session_free(reopened) };
}
