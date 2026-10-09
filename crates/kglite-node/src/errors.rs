//! Error codes and the JS `Error` shape every rejection carries.
//!
//! Core errors keep the engine's own code string (`KgErrorCode::as_str`, e.g.
//! `CypherSyntax`), so a message and a code mean the same thing on every
//! binding. `WriterLeaseHeld` (another writer owns the path; carries a
//! `holder`) and `ReadOnly` (a write on a `readOnly` handle) are core codes too:
//! their strings are `KgErrorCode`'s, not spellings of this crate. Codes the
//! binding itself raises are `Internal` (a contained panic), `QueueFull`,
//! `Closed` (the handle was closed), `NotDurable` (`sync()` on a handle with no
//! write-ahead log) and `TransactionClosed` (a transaction that is already
//! committed, rolled back or aborted).

use std::ffi::c_char;
use std::ptr;

use kglite::api::{EntityKind, KgError, KgErrorCode, OntologyReportEntry};
use napi::sys;

pub const CODE_INTERNAL: &str = "Internal";
pub const CODE_QUEUE_FULL: &str = "QueueFull";
pub const CODE_CLOSED: &str = "Closed";
pub const CODE_READ_ONLY: &str = KgErrorCode::ReadOnly.as_str();
pub const CODE_NOT_DURABLE: &str = "NotDurable";
pub const CODE_TX_CLOSED: &str = "TransactionClosed";
const CODE_ARGUMENT: &str = "InvalidArgument";

/// Who holds a writer lease, as published by the holder (best effort: a
/// contender that loses a startup race can read an empty record).
#[derive(Debug, Clone)]
pub struct Holder {
    pub pid: Option<u32>,
    pub since: Option<String>,
    pub label: Option<String>,
    /// The holder is this very process (an un-closed handle opened the path).
    pub is_self: bool,
}

/// The structured part of an `OntologyViolation`: which rule refused what.
#[derive(Debug)]
pub struct OntologyDetail {
    pub rule: &'static str,
    pub entity: &'static str,
    pub entity_type: String,
    pub property: Option<String>,
    /// Per-rule breakdown of a refused declaration; empty for a refused write.
    pub report: Vec<OntologyReportEntry>,
}

/// A failure on its way to becoming a JS `Error` with a `code` property.
#[derive(Debug)]
pub struct JsErr {
    pub code: &'static str,
    pub message: String,
    /// Set on `WriterLeaseHeld`; becomes the error's `holder` property.
    pub holder: Option<Holder>,
    /// Set on `OntologyViolation`.
    pub ontology: Option<Box<OntologyDetail>>,
}

pub type JsRes<T> = std::result::Result<T, JsErr>;

impl JsErr {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            holder: None,
            ontology: None,
        }
    }

    pub fn arg(message: impl Into<String>) -> Self {
        Self::new(CODE_ARGUMENT, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(CODE_INTERNAL, message)
    }

    pub fn from_kg(err: &KgError) -> Self {
        let mut out = Self::new(err.code().as_str(), err.to_string());
        if let KgError::WriterLeaseHeld { holder, .. } = err {
            out.holder = Some(Holder {
                pid: holder.pid,
                since: holder.since.clone(),
                label: holder.label.clone(),
                is_self: holder.is_self(),
            });
        }
        if let KgError::OntologyViolation {
            rule,
            entity,
            entity_type,
            property,
            report,
            ..
        } = err
        {
            out.ontology = Some(Box::new(OntologyDetail {
                rule,
                entity,
                entity_type: entity_type.clone(),
                property: property.clone(),
                report: report.clone(),
            }));
        }
        out
    }

    pub fn from_panic(payload: &(dyn std::any::Any + Send)) -> Self {
        Self::internal(format!("internal error: {}", crate::panic_message(payload)))
    }
}

impl From<napi::Error> for JsErr {
    fn from(e: napi::Error) -> Self {
        Self::internal(format!("{}: {}", e.status, e.reason))
    }
}

fn check(status: sys::napi_status, what: &str) -> JsRes<()> {
    if status == sys::Status::napi_ok {
        Ok(())
    } else {
        Err(JsErr::internal(format!("{what} failed (status {status})")))
    }
}

fn utf8(env: sys::napi_env, s: &str) -> JsRes<sys::napi_value> {
    let mut out = ptr::null_mut();
    check(
        unsafe {
            sys::napi_create_string_utf8(
                env,
                s.as_ptr().cast::<c_char>(),
                s.len() as isize,
                &mut out,
            )
        },
        "create string",
    )?;
    Ok(out)
}

fn set_prop(
    env: sys::napi_env,
    obj: sys::napi_value,
    key: &std::ffi::CStr,
    value: sys::napi_value,
) -> JsRes<()> {
    check(
        unsafe { sys::napi_set_named_property(env, obj, key.as_ptr(), value) },
        "set property",
    )
}

fn holder_object(env: sys::napi_env, h: &Holder) -> JsRes<sys::napi_value> {
    let mut obj = ptr::null_mut();
    check(
        unsafe { sys::napi_create_object(env, &mut obj) },
        "create object",
    )?;
    let nullable = |v: Option<sys::napi_value>| -> JsRes<sys::napi_value> {
        match v {
            Some(v) => Ok(v),
            None => {
                let mut n = ptr::null_mut();
                check(unsafe { sys::napi_get_null(env, &mut n) }, "get null")?;
                Ok(n)
            }
        }
    };
    let pid = match h.pid {
        Some(p) => {
            let mut v = ptr::null_mut();
            check(
                unsafe { sys::napi_create_uint32(env, p, &mut v) },
                "create number",
            )?;
            Some(v)
        }
        None => None,
    };
    set_prop(env, obj, c"pid", nullable(pid)?)?;
    let since = h.since.as_deref().map(|s| utf8(env, s)).transpose()?;
    set_prop(env, obj, c"since", nullable(since)?)?;
    let label = h.label.as_deref().map(|s| utf8(env, s)).transpose()?;
    set_prop(env, obj, c"label", nullable(label)?)?;
    let mut flag = ptr::null_mut();
    check(
        unsafe { sys::napi_get_boolean(env, h.is_self, &mut flag) },
        "create boolean",
    )?;
    set_prop(env, obj, c"self", flag)?;
    Ok(obj)
}

fn entity_name(e: EntityKind) -> &'static str {
    match e {
        EntityKind::Node => "node",
        EntityKind::Relationship => "relationship",
    }
}

fn array_of(env: sys::napi_env, items: Vec<sys::napi_value>) -> JsRes<sys::napi_value> {
    let mut arr = ptr::null_mut();
    check(
        unsafe { sys::napi_create_array_with_length(env, items.len(), &mut arr) },
        "create array",
    )?;
    for (i, item) in items.into_iter().enumerate() {
        check(
            unsafe { sys::napi_set_element(env, arr, i as u32, item) },
            "set element",
        )?;
    }
    Ok(arr)
}

fn null_or_string(env: sys::napi_env, s: Option<&str>) -> JsRes<sys::napi_value> {
    match s {
        Some(s) => utf8(env, s),
        None => {
            let mut n = ptr::null_mut();
            check(unsafe { sys::napi_get_null(env, &mut n) }, "get null")?;
            Ok(n)
        }
    }
}

fn new_object(env: sys::napi_env) -> JsRes<sys::napi_value> {
    let mut obj = ptr::null_mut();
    check(
        unsafe { sys::napi_create_object(env, &mut obj) },
        "create object",
    )?;
    Ok(obj)
}

fn report_entry(env: sys::napi_env, e: &OntologyReportEntry) -> JsRes<sys::napi_value> {
    let obj = new_object(env)?;
    set_prop(env, obj, c"rule", utf8(env, e.rule.as_str())?)?;
    set_prop(env, obj, c"entity", utf8(env, entity_name(e.entity))?)?;
    set_prop(env, obj, c"entityType", utf8(env, &e.entity_type)?)?;
    set_prop(
        env,
        obj,
        c"property",
        null_or_string(env, e.property.as_deref())?,
    )?;
    let mut count = ptr::null_mut();
    check(
        unsafe { sys::napi_create_double(env, e.count as f64, &mut count) },
        "create number",
    )?;
    set_prop(env, obj, c"count", count)?;
    Ok(obj)
}

fn attach_ontology(env: sys::napi_env, err: sys::napi_value, d: &OntologyDetail) -> JsRes<()> {
    set_prop(env, err, c"rule", utf8(env, d.rule)?)?;
    set_prop(env, err, c"entity", utf8(env, d.entity)?)?;
    set_prop(env, err, c"entityType", utf8(env, &d.entity_type)?)?;
    set_prop(
        env,
        err,
        c"property",
        null_or_string(env, d.property.as_deref())?,
    )?;
    let entries = d
        .report
        .iter()
        .map(|e| report_entry(env, e))
        .collect::<JsRes<Vec<_>>>()?;
    set_prop(env, err, c"report", array_of(env, entries)?)
}

/// A JS `Error` with `.code` set and `.name === "KgliteError"`.
pub fn make_error(env: sys::napi_env, e: &JsErr) -> JsRes<sys::napi_value> {
    let code = utf8(env, e.code)?;
    let msg = utf8(env, &e.message)?;
    let mut err = ptr::null_mut();
    check(
        unsafe { sys::napi_create_error(env, code, msg, &mut err) },
        "create error",
    )?;
    let name = utf8(env, "KgliteError")?;
    check(
        unsafe { sys::napi_set_named_property(env, err, c"name".as_ptr(), name) },
        "set error name",
    )?;
    if let Some(h) = &e.holder {
        set_prop(env, err, c"holder", holder_object(env, h)?)?;
    }
    if let Some(d) = &e.ontology {
        attach_ontology(env, err, d)?;
    }
    Ok(err)
}

/// An already-rejected promise. Resolving a deferred with this adopts the
/// rejection, which is how a worker reports a coded error: `JsDeferred` can
/// only reject with a napi `Status`, never an arbitrary code string.
pub fn rejected_promise(env: sys::napi_env, e: &JsErr) -> JsRes<sys::napi_value> {
    let err = make_error(env, e)?;
    let mut deferred = ptr::null_mut();
    let mut promise = ptr::null_mut();
    check(
        unsafe { sys::napi_create_promise(env, &mut deferred, &mut promise) },
        "create promise",
    )?;
    check(
        unsafe { sys::napi_reject_deferred(env, deferred, err) },
        "reject promise",
    )?;
    Ok(promise)
}

/// The sync-throw form used by exports that fail before returning a promise.
pub fn to_sync_error(e: JsErr) -> napi::Error<&'static str> {
    napi::Error::new(e.code, e.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kglite::api::{KgError, KgErrorCode};

    #[test]
    fn core_codes_pass_through_as_str() {
        let e = JsErr::from_kg(&KgError::CypherSyntax {
            message: "bad".into(),
            line: Some(1),
            col: Some(2),
        });
        assert_eq!(e.code, KgErrorCode::CypherSyntax.as_str());
        assert!(e.message.contains("bad"));
    }

    #[test]
    fn ontology_violation_keeps_its_structured_fields() {
        let e = JsErr::from_kg(&KgError::OntologyViolation {
            rule: "domain",
            entity: "relationship",
            entity_type: "WORKS_AT".into(),
            property: None,
            message: "m".into(),
            report: Vec::new(),
        });
        assert_eq!(e.code, "OntologyViolation");
        let d = e.ontology.expect("detail");
        assert_eq!(
            (d.rule, d.entity, d.entity_type.as_str()),
            ("domain", "relationship", "WORKS_AT")
        );
    }

    #[test]
    fn read_only_and_lease_codes_are_the_core_codes() {
        assert_eq!(CODE_READ_ONLY, KgErrorCode::ReadOnly.as_str());
        let e = JsErr::from_kg(&KgError::WriterLeaseHeld {
            message: "held".into(),
            holder: kglite::api::io::LeaseHolder {
                pid: Some(std::process::id()),
                since: None,
                label: Some("me".into()),
            },
        });
        assert_eq!(e.code, "WriterLeaseHeld");
        let holder = e.holder.expect("holder");
        assert!(holder.is_self);
        assert_eq!(holder.label.as_deref(), Some("me"));
    }

    #[test]
    fn argument_errors_use_the_engine_code() {
        assert_eq!(JsErr::arg("x").code, KgErrorCode::InvalidArgument.as_str());
    }
}
