//! `graph.setEmbedder` / `graph.clearEmbedder`: a JavaScript function as the
//! graph's text embedder.
//!
//! The engine calls [`Embedder::embed`] synchronously on a pool thread, but the
//! JS function may only run on the JS thread. Each call therefore hands the
//! texts to a napi threadsafe function and blocks the pool thread on a channel
//! until the JS side reports back through a one-shot `done` callback. The JS
//! thread is never blocked: it only runs the user's function and, when that
//! returns a promise, attaches `then` handlers to it.
//!
//! Deadlock rules:
//! - Every engine entry point that can embed (`executeRead`, `executeWrite`,
//!   transactions) runs on the pool, so the JS thread is free to serve the call.
//! - [`JsEmbedder::embed`] refuses, with an error, when it is reached on the JS
//!   thread itself: waiting there for a JS callback could never be answered.
//! - A call that is never answered (a promise that never settles, or an
//!   embedder that itself waits on queries queued behind the blocked pool
//!   threads) ends after `timeoutMs` with an error instead of hanging.
//!
//! The user's function runs inside a JS wrapper that catches a throw or a
//! rejection and reports it as a message; a throw routed through the
//! threadsafe-function machinery would end the process.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use kglite::api::Embedder;
use napi::bindgen_prelude::{FnArgs, FromNapiValue, Function, ToNapiValue, Unknown};
use napi::threadsafe_function::{ThreadsafeCallContext, ThreadsafeFunction};
use napi::{sys, Env, JsValue};
use napi_derive::napi;

use crate::contain;
use crate::errors::{to_sync_error, JsErr, JsRes};
use crate::graph::{closed_error, expect_string, option_entries, Graph};
use crate::panic_message;
use crate::pool::RawJs;
use crate::values::FromJs;

/// How long one `embed` call may wait for the JS side before it fails.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

type Reply = Result<Vec<Vec<f32>>, String>;

struct Request {
    texts: Vec<String>,
    reply: Sender<Reply>,
}

type Tsfn =
    ThreadsafeFunction<Request, (), FnArgs<(Vec<String>, RawJs)>, napi::Status, false, true>;

/// Wraps the user's function so a throw, a rejection and a non-array result all
/// reach `done(message)`; a success reaches `done(undefined, rows)`.
const WRAPPER: &str = r#"(function (embed) {
  const text = (e) => (e instanceof Error ? e.message : String(e));
  return function (texts, done) {
    let settled = false;
    const finish = (message, rows) => {
      if (settled) return;
      settled = true;
      try { done(message, rows); } catch (_) { /* done never throws */ }
    };
    const ok = (v) => {
      if (!Array.isArray(v)) {
        finish('the embedder must return an array of vectors, got ' + (v === null ? 'null' : typeof v));
        return;
      }
      finish(undefined, v.map((row) => (ArrayBuffer.isView(row) ? Array.from(row) : row)));
    };
    const fail = (e) => finish(text(e));
    try {
      const r = embed(texts);
      if (r && typeof r.then === 'function') r.then(ok, fail); else ok(r);
    } catch (e) {
      fail(e);
    }
  };
})"#;

/// The JavaScript function registered with `setEmbedder`, as an engine embedder.
pub(crate) struct JsEmbedder {
    tsfn: Tsfn,
    name: String,
    model_id: String,
    /// 0 until declared or learned from the first vector.
    dimension: AtomicUsize,
    timeout: Duration,
    js_thread: ThreadId,
}

impl JsEmbedder {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Run the registered function on the JS thread and wait for its answer.
    fn call(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if thread::current().id() == self.js_thread {
            return Err(
                "the JavaScript embedder cannot be called from the JavaScript thread: waiting \
                 for it there would deadlock (embedding runs on the query pool)"
                    .to_string(),
            );
        }
        let (reply, answer) = channel();
        let status = self.tsfn.call(
            Request {
                texts: texts.to_vec(),
                reply,
            },
            napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
        );
        if status != napi::Status::Ok {
            return Err(format!(
                "the JavaScript embedder '{}' is unavailable ({status:?})",
                self.name
            ));
        }
        match answer.recv_timeout(self.timeout) {
            Ok(Ok(vectors)) => Ok(vectors),
            Ok(Err(message)) => Err(format!("the JavaScript embedder '{}': {message}", self.name)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "the JavaScript embedder '{}' did not answer within {} ms",
                self.name,
                self.timeout.as_millis()
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(format!(
                "the JavaScript embedder '{}' went away before answering (the process is shutting down)",
                self.name
            )),
        }
    }
}

impl Embedder for JsEmbedder {
    fn dimension(&self) -> usize {
        self.dimension.load(Ordering::Acquire)
    }

    fn model_id(&self) -> Option<String> {
        Some(self.model_id.clone())
    }

    /// With no declared `dimension`, one probe embedding learns it.
    fn load(&self) -> Result<(), String> {
        if self.dimension() == 0 {
            self.embed(&["dimension probe".to_string()])?;
        }
        Ok(())
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let vectors = self.call(texts)?;
        if vectors.len() != texts.len() {
            return Err(format!(
                "the JavaScript embedder '{}' returned {} vectors for {} texts",
                self.name,
                vectors.len(),
                texts.len()
            ));
        }
        let declared = self.dimension();
        for vector in &vectors {
            if declared != 0 && vector.len() != declared {
                return Err(format!(
                    "the JavaScript embedder '{}' returned a vector of dimension {} (declared {declared})",
                    self.name,
                    vector.len()
                ));
            }
            if vector.is_empty() {
                return Err(format!(
                    "the JavaScript embedder '{}' returned an empty vector",
                    self.name
                ));
            }
            if declared == 0 {
                // The first answer fixes the width; a later disagreeing batch fails above.
                let _ = self.dimension.compare_exchange(
                    0,
                    vector.len(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                if vector.len() != self.dimension() {
                    return Err(format!(
                        "the JavaScript embedder '{}' returned vectors of different dimensions",
                        self.name
                    ));
                }
            }
        }
        Ok(vectors)
    }
}

/// `rows` as `Vec<Vec<f32>>`, or what is wrong with it.
fn read_vectors(env: sys::napi_env, rows: sys::napi_value) -> Reply {
    let f = FromJs::new(env);
    let len = array_len(env, rows).ok_or("the embedder must return an array of vectors")?;
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let row = element(env, rows, i).ok_or("could not read a vector")?;
        let width =
            array_len(env, row).ok_or_else(|| format!("vector {i} is not an array of numbers"))?;
        let mut vector = Vec::with_capacity(width as usize);
        for j in 0..width {
            let cell = element(env, row, j).ok_or("could not read a vector element")?;
            if !f.kind(cell).is_ok_and(|k| k.is_number()) {
                return Err(format!("vector {i} element {j} is not a number"));
            }
            let n = f.get_f64(cell).map_err(|e| e.message)?;
            if !n.is_finite() {
                return Err(format!("vector {i} element {j} is not finite"));
            }
            vector.push(n as f32);
        }
        out.push(vector);
    }
    Ok(out)
}

fn array_len(env: sys::napi_env, v: sys::napi_value) -> Option<u32> {
    let mut is_array = false;
    if unsafe { sys::napi_is_array(env, v, &mut is_array) } != sys::Status::napi_ok || !is_array {
        return None;
    }
    let mut len = 0u32;
    (unsafe { sys::napi_get_array_length(env, v, &mut len) } == sys::Status::napi_ok).then_some(len)
}

fn element(env: sys::napi_env, array: sys::napi_value, i: u32) -> Option<sys::napi_value> {
    let mut out = std::ptr::null_mut();
    (unsafe { sys::napi_get_element(env, array, i, &mut out) } == sys::Status::napi_ok)
        .then_some(out)
}

/// The JS thread's half of one call: the `done(message, rows)` function the wrapper reports to.
fn done_function(env: &Env, reply: Sender<Reply>) -> napi::Result<RawJs> {
    let slot = Mutex::new(Some(reply));
    let done = env.create_function_from_closure::<RawJs, (), _>("done", move |ctx| {
        let reply = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(reply) = reply else { return Ok(()) };
        let raw_env = ctx.env.raw();
        let answer = catch_unwind(AssertUnwindSafe(|| {
            let message = ctx.get::<Unknown>(0)?;
            let is_message = FromJs::new(raw_env)
                .kind(message.raw())
                .is_ok_and(|k| k.is_string());
            if is_message {
                let text = FromJs::new(raw_env)
                    .get_string(message.raw())
                    .unwrap_or_default();
                return Ok::<Reply, napi::Error>(Err(text));
            }
            let rows = ctx.get::<Unknown>(1)?;
            Ok(read_vectors(raw_env, rows.raw()))
        }))
        .unwrap_or_else(|p| {
            Ok(Err(format!(
                "internal error: {}",
                panic_message(p.as_ref())
            )))
        })
        .unwrap_or_else(|e| Err(e.reason.clone()));
        let _ = reply.send(answer);
        Ok(())
    })?;
    Ok(RawJs(unsafe { Function::to_napi_value(env.raw(), done)? }))
}

struct EmbedderOptions {
    dimension: usize,
    model_id: Option<String>,
    timeout: Duration,
}

fn parse_embedder_options(f: &mut FromJs, v: Option<sys::napi_value>) -> JsRes<EmbedderOptions> {
    let mut out = EmbedderOptions {
        dimension: 0,
        model_id: None,
        timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
    };
    for (key, val) in option_entries(
        f,
        v,
        &["dimension", "modelId", "timeoutMs"],
        "embedder option",
    )? {
        match key.as_str() {
            "modelId" => out.model_id = Some(expect_string(f, val, "modelId")?),
            _ => {
                if !f.kind(val)?.is_number() {
                    return Err(JsErr::arg(format!("{key} must be a number")));
                }
                let n = f.get_f64(val)?;
                if !(n.is_finite() && n >= 1.0 && n.fract() == 0.0 && n <= 4_294_967_295.0) {
                    return Err(JsErr::arg(format!("{key} must be a positive integer")));
                }
                if key == "dimension" {
                    out.dimension = n as usize;
                } else {
                    out.timeout = Duration::from_millis(n as u64);
                }
            }
        }
    }
    Ok(out)
}

fn build_embedder(
    env: &Env,
    name: String,
    embed: Unknown,
    options: EmbedderOptions,
) -> napi::Result<JsEmbedder> {
    let factory: Function<Unknown, Unknown> = env.run_script(WRAPPER)?;
    let wrapped = factory.call(embed)?;
    let wrapper: Function<FnArgs<(Vec<String>, RawJs)>, ()> =
        unsafe { Function::from_napi_value(env.raw(), wrapped.raw())? };
    let tsfn: Tsfn = wrapper
        .build_threadsafe_function::<Request>()
        .callee_handled::<false>()
        .weak::<true>()
        .build_callback(|ctx: ThreadsafeCallContext<Request>| {
            let Request { texts, reply } = ctx.value;
            let done = done_function(&ctx.env, reply)?;
            Ok(FnArgs::from((texts, done)))
        })?;
    Ok(JsEmbedder {
        tsfn,
        model_id: options.model_id.unwrap_or_else(|| name.clone()),
        name,
        dimension: AtomicUsize::new(options.dimension),
        timeout: options.timeout,
        js_thread: thread::current().id(),
    })
}

#[napi]
impl Graph {
    /// Use a JavaScript function as the graph's text embedder, for `text_score()` with a string query, `db.embeddings.embed` and `db.embeddings.query({text})`. Replaces any earlier embedder; not persisted.
    #[napi(
        ts_args_type = "name: string, embed: (texts: string[]) => Promise<ArrayLike<number>[]> | ArrayLike<number>[], options?: EmbedderOptions",
        ts_return_type = "void"
    )]
    pub fn set_embedder(
        &self,
        env: &Env,
        name: Unknown,
        embed: Unknown,
        options: Option<Unknown>,
    ) -> napi::Result<(), &'static str> {
        contain(|| {
            let mut f = FromJs::new(env.raw());
            let name = expect_string(&f, name.raw(), "name").map_err(to_sync_error)?;
            if name.is_empty() {
                return Err(to_sync_error(JsErr::arg("name must not be empty")));
            }
            if !f.kind(embed.raw()).is_ok_and(|k| k.is_function()) {
                return Err(to_sync_error(JsErr::arg("embed must be a function")));
            }
            let options = parse_embedder_options(&mut f, options.as_ref().map(|o| o.raw()))
                .map_err(to_sync_error)?;
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(to_sync_error(closed_error()));
            }
            let embedder = build_embedder(env, name, embed, options)
                .map_err(|e| to_sync_error(JsErr::from(e)))?;
            *self
                .inner
                .embedder
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(embedder));
            Ok(())
        })
    }

    /// Remove the registered embedder. With `name`, only when it is the one registered. Returns whether one was removed.
    #[napi]
    pub fn clear_embedder(&self, name: Option<String>) -> napi::Result<bool, &'static str> {
        contain(|| {
            let mut slot = self
                .inner
                .embedder
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let matches = match (&*slot, &name) {
                (Some(current), Some(n)) => current.name() == n,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if matches {
                *slot = None;
            }
            Ok(matches)
        })
    }
}

#[cfg(feature = "test-hooks")]
#[napi]
impl Graph {
    /// Test-only: call the registered embedder on the JS thread, where it must refuse.
    #[napi(js_name = "__embedOnJsThread")]
    pub fn embed_on_js_thread(&self) -> napi::Result<String, &'static str> {
        contain(|| {
            let slot = self
                .inner
                .embedder
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let Some(embedder) = slot else {
                return Err(to_sync_error(JsErr::arg("no embedder registered")));
            };
            Ok(match embedder.embed(&["x".to_string()]) {
                Ok(_) => "embedded".to_string(),
                Err(message) => message,
            })
        })
    }
}
