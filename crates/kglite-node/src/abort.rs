//! `AbortSignal` support: a per-call [`CancelToken`] wired to a signal's `abort` event.
//!
//! The signal is validated and wired on the JS thread; the token travels with
//! the query arguments into the pool job, so the clone inside `ExecuteOptions`
//! keeps the token's flag slot alive for as long as the query runs. A small
//! JavaScript wrapper owns the listener lifecycle (added at call time, removed
//! when the call settles) and attaches `signal.reason` as the rejection's `cause`.

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kglite::api::session::CancelToken;
use kglite::api::KgErrorCode;
use napi::{sys, Env, JsValue, Unknown};

use crate::errors::{JsErr, JsRes};
use crate::values::FromJs;

pub const CODE_CANCELLED: &str = KgErrorCode::Cancelled.as_str();

pub fn cancelled_error() -> JsErr {
    JsErr::new(CODE_CANCELLED, "the query was cancelled")
}

/// The cancel state shared by a call's JS listener and its pool job.
pub struct AbortHandle {
    token: CancelToken,
    started: AtomicBool,
}

impl AbortHandle {
    pub fn new() -> Self {
        Self {
            token: CancelToken::new(),
            started: AtomicBool::new(false),
        }
    }

    pub fn token(&self) -> CancelToken {
        self.token.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Called by the worker before it runs anything: `false` means the call was
    /// aborted while queued and must not run.
    pub fn begin(&self) -> bool {
        self.started.store(true, Ordering::SeqCst);
        !self.token.is_cancelled()
    }

    /// Raises the flag; `true` when no worker has picked the call up yet, so the
    /// JS side can reject at once. SeqCst on both sides: either the worker sees
    /// the flag in `begin`, or this sees `started`.
    fn cancel(&self) -> bool {
        self.token.cancel();
        !self.started.load(Ordering::SeqCst)
    }
}

/// A validated `signal` option.
pub struct Signal {
    pub value: sys::napi_value,
    pub aborted: bool,
}

pub fn read_signal(f: &FromJs, v: sys::napi_value) -> JsRes<Signal> {
    let bad = || JsErr::arg("signal must be an AbortSignal");
    if !f.kind(v)?.is_object() {
        return Err(bad());
    }
    for method in ["addEventListener", "removeEventListener"] {
        if !f.kind(f.get_property(v, method)?)?.is_function() {
            return Err(bad());
        }
    }
    let aborted = f.get_property(v, "aborted")?;
    if !f.kind(aborted)?.is_boolean() {
        return Err(bad());
    }
    Ok(Signal {
        value: v,
        aborted: f.get_bool(aborted)?,
    })
}

const QUERY_WRAPPER: &str = r#"(function (signal, cancel, promise, code) {
  return new Promise((resolve, reject) => {
    const cleanup = () => signal.removeEventListener('abort', onAbort);
    const onAbort = () => {
      if (!cancel()) return;
      cleanup();
      const e = new Error('the query was cancelled');
      e.code = code;
      e.name = 'KgliteError';
      e.cause = signal.reason;
      reject(e);
    };
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (v) => { cleanup(); resolve(v); },
      (e) => {
        cleanup();
        if (e && e.code === code && e.cause === undefined) e.cause = signal.reason;
        reject(e);
      }
    );
  });
})"#;

const STREAM_WRAPPER: &str = r#"(function (signal, cancel, stream, code) {
  const cleanup = () => signal.removeEventListener('abort', onAbort);
  const onAbort = () => { cancel(); };
  signal.addEventListener('abort', onAbort, { once: true });
  const next = stream.next;
  const ret = stream.return;
  stream.next = function () {
    return next.call(this).then(
      (r) => { if (r.done) cleanup(); return r; },
      (e) => {
        cleanup();
        if (e && e.code === code && e.cause === undefined) e.cause = signal.reason;
        throw e;
      }
    );
  };
  stream.return = function () { cleanup(); return ret.call(this); };
  return stream;
})"#;

fn wire(
    env: &Env,
    script: &str,
    signal: &Signal,
    handle: &Arc<AbortHandle>,
    subject: sys::napi_value,
) -> JsRes<sys::napi_value> {
    let h = Arc::clone(handle);
    let cancel = env
        .create_function_from_closure::<(), bool, _>("cancel", move |_| Ok(h.cancel()))
        .map_err(JsErr::from)?;
    let wrapper: Unknown = env.run_script(script).map_err(JsErr::from)?;
    let code = env.create_string(CODE_CANCELLED).map_err(JsErr::from)?;
    let argv = [signal.value, cancel.raw(), subject, code.raw()];
    let raw = env.raw();
    let mut undefined = ptr::null_mut();
    let mut out = ptr::null_mut();
    unsafe {
        sys::napi_get_undefined(raw, &mut undefined);
        let status = sys::napi_call_function(
            raw,
            undefined,
            wrapper.raw(),
            argv.len(),
            argv.as_ptr(),
            &mut out,
        );
        if status != sys::Status::napi_ok {
            return Err(JsErr::internal("abort wiring failed"));
        }
    }
    Ok(out)
}

/// Wraps a query's promise so `signal` cancels it and the listener is removed on settle.
pub fn wire_query(
    env: &Env,
    signal: &Signal,
    handle: &Arc<AbortHandle>,
    promise: sys::napi_value,
) -> JsRes<sys::napi_value> {
    wire(env, QUERY_WRAPPER, signal, handle, promise)
}

/// Wires `signal` to a stream: abort cancels its query and fails the next `next()`.
pub fn wire_stream(
    env: &Env,
    signal: &Signal,
    handle: &Arc<AbortHandle>,
    stream: sys::napi_value,
) -> JsRes<sys::napi_value> {
    wire(env, STREAM_WRAPPER, signal, handle, stream)
}
