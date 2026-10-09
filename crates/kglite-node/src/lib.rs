//! Node.js binding for kglite.
//!
//! Panic containment: a panic unwinding out of a sync `#[napi]` function aborts
//! the whole Node process (exit 134). Every exported function body MUST run
//! inside [`contain`], which converts a panic into a JS `Error` whose `code` is
//! `Internal`. Work shipped to the pool goes through `pool::spawn`, which wraps
//! the worker body and the JS-thread result-building step the same way.

// napi registers `#[napi]` exports through machinery that unit-test builds skip, so
// everything reachable only from an export looks dead to `clippy --all-targets`.
#![cfg_attr(test, allow(dead_code))]

use std::panic::{catch_unwind, AssertUnwindSafe};

use napi_derive::napi;

mod admin;
mod classes;
mod embedder;
mod errors;
mod graph;
mod pool;
mod stream;
mod tx;
// `pub` so the type-only declarations are not dead code: nothing constructs them.
pub mod typings;
mod values;

// mimalloc, v2 line: the allocator kglite-py and kglite-c ship, for the same
// reason (the engine is allocation-heavy; the system allocator cost 22-32% on
// macOS). See crates/kglite-py/src/lib.rs for why v2 and not v3.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Result type for every export; the error status doubles as the JS `code`.
pub type JsResult<T> = std::result::Result<T, napi::Error<&'static str>>;

/// Code attached to errors produced from a contained panic.
pub const CODE_INTERNAL: &str = "Internal";

pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}

/// Run `f`, turning a panic into an `Internal` JS error instead of aborting Node.
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

/// Test-only: a panic on a pool thread must reject the promise, not abort.
#[cfg(feature = "test-hooks")]
#[napi(js_name = "__panicInWorker", ts_return_type = "Promise<void>")]
pub fn panic_in_worker<'e>(
    env: &'e napi::Env,
) -> napi::Result<napi::bindgen_prelude::Object<'e>, &'static str> {
    contain(|| {
        pool::spawn(env, || panic!("deliberate worker panic"))
            .map_err(|e| errors::to_sync_error(e.into()))
    })
}

/// Test-only: a panic while building the JS result must reject the promise, not abort.
#[cfg(feature = "test-hooks")]
#[napi(js_name = "__panicInSettle", ts_return_type = "Promise<void>")]
pub fn panic_in_settle<'e>(
    env: &'e napi::Env,
) -> napi::Result<napi::bindgen_prelude::Object<'e>, &'static str> {
    contain(|| {
        pool::spawn(env, || Box::new(|_env| panic!("deliberate settle panic")))
            .map_err(|e| errors::to_sync_error(e.into()))
    })
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
