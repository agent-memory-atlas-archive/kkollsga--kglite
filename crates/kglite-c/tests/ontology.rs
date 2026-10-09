use kglite_c::*;
use std::ffi::{c_char, CStr, CString};

fn new_session() -> *mut KgliteSession {
    let mut session = std::ptr::null_mut();
    // Pointers are live locals for these synchronous calls.
    unsafe {
        let graph = kglite_graph_new();
        assert_eq!(
            kglite_session_new(graph, &mut session),
            KgliteStatusCode::Ok
        );
    }
    session
}

fn mutate(session: *mut KgliteSession, query: &str) -> KgliteStatusCode {
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
        if !result.is_null() {
            kglite_cypher_result_free(result);
        }
        if !error.is_null() {
            kglite_free_string(error);
        }
        status
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

fn define(
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

const PERSON: &str =
    r#"{"classes": {"Person": {"required_properties": ["email"], "enforcement": "error"}}}"#;

#[test]
fn declared_ontology_is_enforced_and_clear_removes_it() {
    let session = new_session();
    let (status, warnings, error) = define(session, PERSON);
    assert_eq!(status, KgliteStatusCode::Ok, "{error:?}");
    // No Person node exists yet: the concrete-class advisory comes back.
    let warnings: serde_json::Value = serde_json::from_str(&warnings.unwrap()).unwrap();
    assert!(
        warnings[0].as_str().unwrap().contains("'Person'"),
        "{warnings}"
    );
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1})"),
        KgliteStatusCode::OntologyViolation
    );
    let mut error: *const c_char = std::ptr::null();
    assert_eq!(
        unsafe { kglite_session_clear_ontology(session, &mut error) },
        KgliteStatusCode::Ok
    );
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1})"),
        KgliteStatusCode::Ok
    );
    unsafe { kglite_session_free(session) };
}

#[test]
fn declaring_over_violating_data_is_status_22_with_the_report() {
    let session = new_session();
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1})"),
        KgliteStatusCode::Ok
    );
    let (status, report, error) = define(session, PERSON);
    assert_eq!(status, KgliteStatusCode::OntologyViolation);
    assert_eq!(status as i32, 22);
    let report: serde_json::Value = serde_json::from_str(&report.unwrap()).unwrap();
    assert_eq!(report[0]["rule"], "required_property");
    assert_eq!(report[0]["entity_type"], "Person");
    assert_eq!(report[0]["property"], "email");
    assert_eq!(report[0]["count"], 1);
    assert!(error.unwrap().contains("Person"));
    unsafe { kglite_session_free(session) };
}

#[test]
fn bad_documents_and_null_pointers_are_refused() {
    let session = new_session();
    assert_eq!(
        define(session, "{not json").0,
        KgliteStatusCode::InvalidArgument
    );
    assert_eq!(
        define(session, r#"{"nonsense": 1}"#).0,
        KgliteStatusCode::InvalidArgument
    );
    let mut error: *const c_char = std::ptr::null();
    assert_eq!(
        unsafe {
            kglite_session_define_ontology(
                session,
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut error,
            )
        },
        KgliteStatusCode::NullPointer
    );
    unsafe { kglite_session_free(session) };
}

fn details() -> Option<serde_json::Value> {
    take(kglite_last_error_details_json()).map(|text| serde_json::from_str(&text).unwrap())
}

#[test]
fn a_refused_write_exposes_rule_entity_type_and_property_through_the_details_channel() {
    let session = new_session();
    let (status, _, error) = define(session, PERSON);
    assert_eq!(status, KgliteStatusCode::Ok, "{error:?}");
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1})"),
        KgliteStatusCode::OntologyViolation
    );
    let detail = details().expect("an ontology refusal carries detail");
    assert_eq!(detail["code"], "OntologyViolation");
    assert_eq!(detail["rule"], "required_property");
    assert_eq!(detail["entity"], "node");
    assert_eq!(detail["entity_type"], "Person");
    assert_eq!(detail["property"], "email");
    assert_eq!(detail["report"], serde_json::json!([]));
    // A later successful call clears the slot, so detail never outlives its
    // failure.
    assert_eq!(
        mutate(session, "CREATE (:Other {id: 2})"),
        KgliteStatusCode::Ok
    );
    assert!(details().is_none());
    unsafe { kglite_session_free(session) };
}

#[test]
fn a_refused_declaration_exposes_the_headline_and_the_report() {
    let session = new_session();
    assert_eq!(
        mutate(session, "CREATE (:Person {id: 1})"),
        KgliteStatusCode::Ok
    );
    let (status, _, _) = define(session, PERSON);
    assert_eq!(status, KgliteStatusCode::OntologyViolation);
    let detail = details().expect("a refused declaration carries detail");
    assert_eq!(detail["rule"], "required_property");
    assert_eq!(detail["entity_type"], "Person");
    assert_eq!(detail["property"], "email");
    assert_eq!(detail["report"][0]["count"], 1);
    unsafe { kglite_session_free(session) };
}

#[test]
fn an_error_with_no_structure_leaves_the_details_empty() {
    let session = new_session();
    assert_eq!(
        mutate(session, "THIS IS NOT CYPHER"),
        KgliteStatusCode::CypherSyntax
    );
    assert!(details().is_none());
    unsafe { kglite_session_free(session) };
}
