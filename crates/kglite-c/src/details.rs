//! Structured detail for the last failed call on this thread.
//!
//! A status code plus a message cannot carry an `OntologyViolation`'s rule,
//! entity, type and property without the caller parsing prose. The exports
//! keep their published signatures, so the detail rides a side channel read by
//! [`kglite_last_error_details_json`]: a per-thread slot that every
//! status-returning export clears on entry and that a failing export fills
//! when its error has structure to report.

use crate::strings::alloc_c_string;
use kglite::api::KgError;
use std::cell::RefCell;
use std::ffi::c_char;

thread_local! {
    static LAST: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn clear() {
    LAST.with(|slot| slot.borrow_mut().take());
}

/// Per-rule breakdown of a refused ontology declaration, as the JSON array
/// `kglite_session_define_ontology` also returns through `out_warnings_json`.
pub(crate) fn report_json(entries: &[kglite::api::OntologyReportEntry]) -> serde_json::Value {
    serde_json::Value::Array(
        entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "rule": e.rule.as_str(),
                    "entity": match e.entity {
                        kglite::api::EntityKind::Node => "node",
                        kglite::api::EntityKind::Relationship => "relationship",
                    },
                    "entity_type": e.entity_type,
                    "property": e.property,
                    "count": e.count,
                })
            })
            .collect(),
    )
}

/// Store the structured detail of `error`, if it has any.
pub(crate) fn record(error: &KgError) {
    let json = match error {
        KgError::OntologyViolation {
            rule,
            entity,
            entity_type,
            property,
            report,
            ..
        } => serde_json::json!({
            "code": "OntologyViolation",
            "rule": rule,
            "entity": entity,
            "entity_type": entity_type,
            "property": property,
            "report": report_json(report),
        }),
        _ => return,
    };
    LAST.with(|slot| *slot.borrow_mut() = Some(json.to_string()));
}

/// Structured detail of the most recent failed call **on the calling thread**,
/// as an owned JSON object, or null when that call had none (or succeeded).
///
/// Today only `KGLITE_STATUS_CODE_ONTOLOGY_VIOLATION` carries detail:
/// `{"code":"OntologyViolation","rule","entity","entity_type","property",
/// "report":[{rule,entity,entity_type,property,count}…]}`, where `rule` is
/// `required_property` / `property_type` / `closed_labels` / `domain` /
/// `range`, `entity` is `node` / `relationship`, `property` may be null, and
/// `report` is empty for a refused write and the per-rule breakdown for a
/// refused declaration. Read it right after the failing call: every
/// status-returning export on the same thread clears it on entry.
///
/// Free the string with [`kglite_free_string`](crate::kglite_free_string).
#[no_mangle]
pub extern "C" fn kglite_last_error_details_json() -> *const c_char {
    crate::ffi::value_boundary(std::ptr::null(), || {
        LAST.with(|slot| match slot.borrow().as_deref() {
            Some(json) => alloc_c_string(json),
            None => std::ptr::null(),
        })
    })
}
