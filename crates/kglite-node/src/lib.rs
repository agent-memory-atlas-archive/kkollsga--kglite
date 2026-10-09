//! Node.js binding for kglite.
//!
//! Panic containment: a panic unwinding out of a sync `#[napi]` function aborts
//! the whole Node process (exit 134). Every exported function body MUST run
//! inside [`contain`], which converts a panic into a JS `Error` whose `code` is
//! `INTERNAL`. Async tasks (`Task::compute`) must wrap their body the same way.

use std::panic::{catch_unwind, AssertUnwindSafe};

use napi_derive::napi;

/// Result type for every export; the error status doubles as the JS `code`.
pub type JsResult<T> = std::result::Result<T, napi::Error<&'static str>>;

/// Code attached to errors produced from a contained panic.
pub const CODE_INTERNAL: &str = "INTERNAL";

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}

/// Run `f`, turning a panic into an `INTERNAL` JS error instead of aborting Node.
pub fn contain<T>(f: impl FnOnce() -> JsResult<T>) -> JsResult<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(p) => Err(napi::Error::new(
            CODE_INTERNAL,
            format!("internal error: {}", panic_message(p.as_ref())),
        )),
    }
}

/// Engine version, equal to the workspace package version.
#[napi]
pub fn version() -> napi::Result<String, &'static str> {
    contain(|| Ok(env!("CARGO_PKG_VERSION").to_string()))
}

/// Test-only: panics inside `contain` to prove containment.
#[cfg(feature = "test-hooks")]
#[napi(js_name = "__panic")]
pub fn panic_hook() -> napi::Result<(), &'static str> {
    contain(|| panic!("deliberate test panic"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contain_converts_panic() {
        let r: JsResult<()> = contain(|| panic!("boom"));
        let e = r.unwrap_err();
        assert_eq!(e.status, CODE_INTERNAL);
        assert!(e.reason.contains("boom"));
    }

    #[test]
    fn contain_passes_ok() {
        assert_eq!(contain(|| Ok(3)).unwrap(), 3);
    }
}
