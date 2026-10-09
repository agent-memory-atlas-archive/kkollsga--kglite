//! Dedicated worker threads that run queries off the JS event loop.
//!
//! Why not libuv's `AsyncTask` pool: the engine needs `QUERY_THREAD_STACK_SIZE`
//! of stack under every query ("a Rust stack overflow aborts the process"), and
//! a libuv worker's stack depends on the platform and `RLIMIT_STACK`. Owning the
//! threads makes that a guarantee instead of an assumption.
//!
//! A job returns a [`Settle`] closure; the worker hands it to a napi `JsDeferred`,
//! which runs it back on the JS thread (building JS values needs the `Env`) and
//! resolves the promise with its result.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread;

use kglite::api::cypher::{with_query_warning_sink, QueryWarningSink};
use kglite::api::session::QUERY_THREAD_STACK_SIZE;
use napi::bindgen_prelude::ToNapiValue;
use napi::{sys, Env, JsDeferred};

use crate::errors::{rejected_promise, JsErr, JsRes, CODE_QUEUE_FULL};

/// A napi value handed back to a promise, resolved or already-rejected.
pub struct RawJs(pub sys::napi_value);

impl ToNapiValue for RawJs {
    unsafe fn to_napi_value(_env: sys::napi_env, val: Self) -> napi::Result<sys::napi_value> {
        Ok(val.0)
    }
}

/// Runs on the JS thread and produces the promise's settlement: `Ok` resolves it
/// with the value, `Err` rejects it with a coded `Error`.
pub type Settle = Box<dyn FnOnce(Env) -> JsRes<sys::napi_value> + Send>;

type Deferred = JsDeferred<RawJs, Box<dyn FnOnce(Env) -> napi::Result<RawJs> + Send>>;

/// Queue ceiling. A burst past it is refused rather than buffered without bound.
const QUEUE_CAPACITY: usize = 4096;

type Job = Box<dyn FnOnce() + Send>;

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

fn pool() -> &'static Arc<Queue> {
    static POOL: OnceLock<Arc<Queue>> = OnceLock::new();
    POOL.get_or_init(|| {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        });
        for n in 0..worker_count() {
            let q = Arc::clone(&queue);
            thread::Builder::new()
                .name(format!("kglite-node-{n}"))
                .stack_size(worker_stack_size())
                .spawn(move || worker(&q))
                .expect("spawn kglite-node worker");
        }
        queue
    })
}

/// Stack for every worker: the engine's `QUERY_THREAD_STACK_SIZE`, doubled in
/// debug builds.
///
/// A debug build's frames are large enough that `execute_read` at the parser's
/// nesting ceiling (`RETURN [[...[1]...]]`, 511 levels) overflows 8 MiB and aborts
/// the process; it needs between 9 and 10 MiB. The engine's own stack test drives
/// a narrower path (`run_full_pipeline`) and does not see it. Release builds are
/// held to the engine constant.
pub const fn worker_stack_size() -> usize {
    if cfg!(debug_assertions) {
        QUERY_THREAD_STACK_SIZE * 2
    } else {
        QUERY_THREAD_STACK_SIZE
    }
}

/// `KGLITE_NODE_THREADS`, else `min(4, available cores)`.
fn worker_count() -> usize {
    std::env::var("KGLITE_NODE_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| thread::available_parallelism().map_or(2, |n| n.get().min(4)))
}

fn worker(queue: &Queue) {
    // The structured `warnings` array on every result is the only channel: the
    // engine's stderr echo would write into the host process's stderr.
    with_query_warning_sink(QueryWarningSink::Silent, || worker_loop(queue));
}

fn worker_loop(queue: &Queue) {
    loop {
        let job = {
            let mut jobs = queue.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(job) = jobs.pop_front() {
                    break job;
                }
                jobs = queue
                    .ready
                    .wait(jobs)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        job();
    }
}

fn enqueue(job: Job) -> Result<(), Job> {
    let queue = pool();
    let mut jobs = queue.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    if jobs.len() >= QUEUE_CAPACITY {
        return Err(job);
    }
    jobs.push_back(job);
    drop(jobs);
    queue.ready.notify_one();
    Ok(())
}

/// Wrap a settlement so a panic or napi failure while building the JS value still
/// settles the promise: a panic escaping a threadsafe-function callback aborts Node.
fn guarded(settle: Settle) -> Box<dyn FnOnce(Env) -> napi::Result<RawJs> + Send> {
    Box::new(move |env: Env| {
        let raw = env.raw();
        let outcome = catch_unwind(AssertUnwindSafe(|| settle(env)));
        let result = match outcome {
            Ok(Ok(value)) => return Ok(RawJs(value)),
            Ok(Err(e)) => e,
            Err(p) => JsErr::from_panic(p.as_ref()),
        };
        match rejected_promise(raw, &result) {
            Ok(promise) => Ok(RawJs(promise)),
            Err(e) => Err(napi::Error::from_reason(e.message)),
        }
    })
}

fn settle_now(deferred: Deferred, settle: Settle) {
    deferred.resolve(guarded(settle));
}

/// Run `work` on a pool thread and return the promise it settles.
///
/// `work` runs inside `catch_unwind`; a panic rejects with `INTERNAL`.
pub fn spawn<'e>(
    env: &'e Env,
    work: impl FnOnce() -> Settle + Send + 'static,
) -> napi::Result<napi::bindgen_prelude::Object<'e>> {
    let (deferred, promise) =
        env.create_deferred::<RawJs, Box<dyn FnOnce(Env) -> napi::Result<RawJs> + Send>>()?;
    // The deferred is needed by both the job and the full-queue path, so it
    // lives in a slot the first to run takes.
    let slot = Arc::new(Mutex::new(Some(deferred)));
    let job_slot = Arc::clone(&slot);
    let job: Job = Box::new(move || {
        let settle: Settle = match catch_unwind(AssertUnwindSafe(work)) {
            Ok(s) => s,
            Err(p) => {
                let e = JsErr::from_panic(p.as_ref());
                Box::new(move |_| Err(e))
            }
        };
        if let Some(d) = job_slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            settle_now(d, settle);
        }
    });
    if enqueue(job).is_err() {
        if let Some(d) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let e = JsErr::new(CODE_QUEUE_FULL, "the query queue is full; retry later");
            settle_now(d, Box::new(move |_| Err(e)));
        }
    }
    Ok(promise)
}

/// A promise that is already settled with `settle`'s outcome (argument errors).
pub fn settled<'e>(
    env: &'e Env,
    settle: Settle,
) -> napi::Result<napi::bindgen_prelude::Object<'e>> {
    let (deferred, promise) =
        env.create_deferred::<RawJs, Box<dyn FnOnce(Env) -> napi::Result<RawJs> + Send>>()?;
    settle_now(deferred, settle);
    Ok(promise)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kglite::api::session::{execute_read, open_path, ExecuteOptions, OpenSpec};
    use kglite::api::Value;
    use std::collections::HashMap;

    /// The deepest list literal the parser accepts runs end to end through the
    /// public entry point on a thread of exactly the pool's stack size.
    #[test]
    fn deepest_accepted_query_fits_the_worker_stack() {
        thread::Builder::new()
            .stack_size(worker_stack_size())
            .spawn(|| {
                let spec = OpenSpec {
                    lease_timeout: None,
                    ..OpenSpec::writer()
                };
                let opened = open_path(std::path::Path::new("kglite-node-stack-test.kgl"), &spec)
                    .expect("in-memory open");
                let params: HashMap<String, Value> = HashMap::new();
                let mut opts = ExecuteOptions::eager(&params);
                opts.streaming = true;
                let depth = 511;
                let q = format!("RETURN {}1{} AS v", "[".repeat(depth), "]".repeat(depth));
                execute_read(&opened.session.snapshot(), &q, &opts).expect("query within budget");
            })
            .expect("spawn")
            .join()
            .expect("deep query overflowed the worker stack");
    }
}
