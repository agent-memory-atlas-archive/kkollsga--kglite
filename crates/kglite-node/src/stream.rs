//! `Graph.stream`: a read query delivered as an async iterator of row objects.
//!
//! The engine hands back a fully materialised result (`execute_read` has no row
//! cursor), so the result sits in memory until the stream finishes, is
//! released with `return()`, or is dropped. What streams is the conversion to
//! JavaScript objects, the cost that blocks the event loop (~0.5 us per row):
//! each `next()` converts one row, and every `batchSize`-th row is requested
//! through the worker pool, so the loop runs timers and I/O between batches.
//! Rows inside a batch are separated by microtasks only.
//!
//! Lifecycle: the query starts on the first `next()`. A failure rejects that
//! `next()` once with the usual typed error; the stream is finished after it.
//! `return()` (what `break` calls) releases the result and makes every later
//! `next()` resolve `done`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use kglite::api::session::ExecuteOutcome;
use napi::bindgen_prelude::{Object, ToNapiValue};
use napi::{sys, Env};
use napi_derive::napi;

use crate::abort::{cancelled_error, wire_stream, AbortHandle};
use crate::contain;
use crate::errors::{rejected_promise, to_sync_error, JsErr, JsRes};
use crate::graph::{expect_number, parse_query_args_with, Inner, QueryArgs};
use crate::pool::{self, Settle};
use crate::values::{FromJs, ToJs};

const DEFAULT_BATCH: usize = 1000;

/// Options `stream` accepts beyond the query options (`signal` is a query option,
/// handled by the shared parser).
const STREAM_OPTIONS: &[&str] = &["batchSize"];

enum Slot {
    Unstarted(Option<QueryArgs>),
    Ready(Arc<ExecuteOutcome>),
    /// The error waits here until the `next()` that observes it rejects with it.
    Failed(Option<JsErr>),
    Finished,
}

struct Shared {
    inner: Arc<Inner>,
    batch: usize,
    claimed: AtomicUsize,
    stopped: AtomicBool,
    /// Present when the stream was opened with a `signal`.
    cancel: Option<Arc<AbortHandle>>,
    /// Serialises the one query run when several `next()` calls race the first.
    start: Mutex<()>,
    slot: Mutex<Slot>,
}

impl Shared {
    fn slot(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs the query on a worker unless an earlier call already did.
    fn ensure_started(&self) {
        let _one = self.start.lock().unwrap_or_else(PoisonError::into_inner);
        let args = match &mut *self.slot() {
            Slot::Unstarted(args) => args.take(),
            _ => return,
        };
        let outcome = match args {
            Some(args) => self.run_query(&args),
            None => Err(JsErr::internal("stream started twice")),
        };
        let mut slot = self.slot();
        *slot = if self.stopped.load(Ordering::Acquire) {
            Slot::Finished
        } else {
            match outcome {
                Ok(o) => Slot::Ready(Arc::new(o)),
                Err(e) => Slot::Failed(Some(e)),
            }
        };
    }

    fn run_query(&self, args: &QueryArgs) -> JsRes<ExecuteOutcome> {
        if self.cancel.as_ref().is_some_and(|c| !c.begin()) {
            return Err(cancelled_error());
        }
        let session = self.inner.session()?;
        let opts = self.inner.execute_options(args);
        let snapshot = session.snapshot();
        kglite::api::session::execute_read(&snapshot, &args.cypher, &opts)
            .map_err(|e| JsErr::from_kg(&e))
    }

    /// The iterator result for the `idx`-th row, built on the JS thread.
    fn step(&self, env: sys::napi_env, idx: usize) -> JsRes<sys::napi_value> {
        let js = ToJs::new(env, self.inner.ints);
        if !self.stopped.load(Ordering::Acquire)
            && self.cancel.as_ref().is_some_and(|c| c.is_cancelled())
        {
            let mut slot = self.slot();
            // A `Failed` slot carries the error the cancelled query returned.
            if matches!(&*slot, Slot::Ready(_) | Slot::Unstarted(_)) {
                *slot = Slot::Finished;
                return Err(cancelled_error());
            }
        }
        let ready = {
            let mut slot = self.slot();
            match &mut *slot {
                Slot::Ready(o) if !self.stopped.load(Ordering::Acquire) => Some(Arc::clone(o)),
                Slot::Failed(e) => {
                    let e = e.take();
                    *slot = Slot::Finished;
                    if let Some(e) = e {
                        return Err(e);
                    }
                    None
                }
                _ => None,
            }
        };
        let result = js.object()?;
        let row = ready.as_ref().and_then(|o| o.result.rows.get(idx));
        let Some(row) = row else {
            if ready.is_some() {
                *self.slot() = Slot::Finished;
            }
            js.set(result, "value", js.undefined()?)?;
            js.set(result, "done", js.boolean(true)?)?;
            return Ok(result);
        };
        let obj = js.object()?;
        let columns = ready
            .as_ref()
            .map_or(&[][..], |o| o.result.columns.as_slice());
        for (name, value) in columns.iter().zip(row) {
            js.set(obj, name, js.value(value, 0)?)?;
        }
        js.set(result, "value", obj)?;
        js.set(result, "done", js.boolean(false)?)?;
        Ok(result)
    }
}

/// The async iterator `Graph.stream` returns.
#[napi]
pub struct RowStream {
    shared: Arc<Shared>,
}

#[napi]
impl RowStream {
    #[napi(ts_return_type = "Promise<IteratorResult<KgMap, undefined>>")]
    pub fn next<'e>(&self, env: &'e Env) -> napi::Result<Object<'e>, &'static str> {
        contain(|| {
            let shared = Arc::clone(&self.shared);
            let idx = shared.claimed.fetch_add(1, Ordering::AcqRel);
            let started = !matches!(*shared.slot(), Slot::Unstarted(_));
            if started && !idx.is_multiple_of(shared.batch) {
                return immediate(env, shared.step(env.raw(), idx)).map_err(to_sync_error);
            }
            let s = Arc::clone(&shared);
            pool::spawn(env, move || {
                s.ensure_started();
                Box::new(move |env: Env| s.step(env.raw(), idx)) as Settle
            })
            .map_err(|e| to_sync_error(JsErr::from(e)))
        })
    }

    #[napi(
        js_name = "return",
        ts_return_type = "Promise<IteratorResult<KgMap, undefined>>"
    )]
    pub fn finish<'e>(&self, env: &'e Env) -> napi::Result<Object<'e>, &'static str> {
        contain(|| {
            let shared = Arc::clone(&self.shared);
            shared.stopped.store(true, Ordering::Release);
            *shared.slot() = Slot::Finished;
            immediate(env, shared.step(env.raw(), usize::MAX)).map_err(to_sync_error)
        })
    }
}

/// A promise settled right now, on the JS thread. Unlike `pool::settled` it does
/// not hop through a threadsafe function, so awaiting it costs a microtask and
/// the other event-loop phases run only at batch boundaries.
fn immediate<'e>(env: &'e Env, outcome: JsRes<sys::napi_value>) -> JsRes<Object<'e>> {
    let raw = env.raw();
    let promise = match outcome {
        Ok(value) => {
            let mut deferred = std::ptr::null_mut();
            let mut promise = std::ptr::null_mut();
            let created = unsafe { sys::napi_create_promise(raw, &mut deferred, &mut promise) };
            if created != sys::Status::napi_ok {
                return Err(JsErr::internal("create promise failed"));
            }
            let resolved = unsafe { sys::napi_resolve_deferred(raw, deferred, value) };
            if resolved != sys::Status::napi_ok {
                return Err(JsErr::internal("resolve promise failed"));
            }
            promise
        }
        Err(e) => rejected_promise(raw, &e)?,
    };
    Ok(Object::from_raw(raw, promise))
}

unsafe extern "C" fn return_this(
    env: sys::napi_env,
    info: sys::napi_callback_info,
) -> sys::napi_value {
    let mut this = std::ptr::null_mut();
    unsafe {
        sys::napi_get_cb_info(
            env,
            info,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut this,
            std::ptr::null_mut(),
        );
    }
    this
}

/// Makes `stream[Symbol.asyncIterator]()` return the stream itself.
fn make_async_iterable(env: sys::napi_env, stream: sys::napi_value) -> JsRes<()> {
    let ck = |status: sys::napi_status, what: &str| {
        if status == sys::Status::napi_ok {
            Ok(())
        } else {
            Err(JsErr::internal(format!("{what} failed")))
        }
    };
    let mut global = std::ptr::null_mut();
    let mut symbol = std::ptr::null_mut();
    let mut key = std::ptr::null_mut();
    let mut func = std::ptr::null_mut();
    unsafe {
        ck(sys::napi_get_global(env, &mut global), "get global")?;
        ck(
            sys::napi_get_named_property(env, global, c"Symbol".as_ptr(), &mut symbol),
            "get Symbol",
        )?;
        ck(
            sys::napi_get_named_property(env, symbol, c"asyncIterator".as_ptr(), &mut key),
            "get Symbol.asyncIterator",
        )?;
        ck(
            sys::napi_create_function(
                env,
                c"[Symbol.asyncIterator]".as_ptr(),
                -1, // NAPI_AUTO_LENGTH
                Some(return_this),
                std::ptr::null_mut(),
                &mut func,
            ),
            "create function",
        )?;
        ck(
            sys::napi_set_property(env, stream, key, func),
            "set Symbol.asyncIterator",
        )
    }
}

fn parse_batch(f: &FromJs, own: &[(String, sys::napi_value)]) -> JsRes<usize> {
    let mut batch = DEFAULT_BATCH;
    for (key, val) in own {
        if key == "batchSize" {
            let n = expect_number(f, *val, "batchSize")?;
            if n == 0 {
                return Err(JsErr::arg("batchSize must be at least 1"));
            }
            batch = n as usize;
        }
    }
    Ok(batch)
}

/// Builds the stream for `Graph.stream`. A bad argument does not throw here: the
/// stream is returned already failed, so the error rejects the first `next()`
/// the way a bad `executeRead` argument rejects its promise.
pub(crate) fn open_stream<'e>(
    env: &'e Env,
    inner: &Arc<Inner>,
    cypher: sys::napi_value,
    params: Option<sys::napi_value>,
    options: Option<sys::napi_value>,
) -> JsRes<Object<'e>> {
    let mut f = FromJs::new(env.raw());
    let parsed = parse_query_args_with(&mut f, cypher, params, options, STREAM_OPTIONS)
        .and_then(|(args, own, signal)| parse_batch(&f, &own).map(|b| (args, b, signal)));
    let (slot, batch, cancel, signal) = match parsed {
        Ok((args, batch, signal)) => {
            let cancel = args.cancel.clone();
            if signal.as_ref().is_some_and(|s| s.aborted) {
                (Slot::Failed(Some(cancelled_error())), batch, cancel, signal)
            } else {
                (Slot::Unstarted(Some(args)), batch, cancel, signal)
            }
        }
        Err(e) => (Slot::Failed(Some(e)), DEFAULT_BATCH, None, None),
    };
    let shared = Arc::new(Shared {
        inner: Arc::clone(inner),
        batch,
        claimed: AtomicUsize::new(0),
        stopped: AtomicBool::new(false),
        cancel: cancel.clone(),
        start: Mutex::new(()),
        slot: Mutex::new(slot),
    });
    let raw = unsafe { RowStream::to_napi_value(env.raw(), RowStream { shared }) }
        .map_err(JsErr::from)?;
    make_async_iterable(env.raw(), raw)?;
    let raw = match (&signal, &cancel) {
        (Some(signal), Some(cancel)) => wire_stream(env, signal, cancel, raw)?,
        _ => raw,
    };
    Ok(Object::from_raw(env.raw(), raw))
}
