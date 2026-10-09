//! `open`, `Graph.executeRead`, `Graph.executeWrite` and the graph lifecycle.
//!
//! Every method validates its arguments on the JS thread, ships the engine work
//! to the worker pool, and builds the result back on the JS thread. A bad
//! argument rejects the returned promise; it never throws synchronously.
//!
//! Lifecycle contract:
//! - A writable graph owns the path's writer lease from `open` until `close`
//!   (or until the handle is garbage collected). A second writable `open` of
//!   the same path, from another process or from this one, rejects
//!   `WriterLeaseHeld` carrying the holder; `holder.self` is true when the
//!   holder is this process. The OS frees the lease when a process dies, so
//!   `kill -9` never leaves one stale.
//! - `readOnly` takes no lease and loads the last checkpoint on disk. While a
//!   durable writer is live, commits since its last checkpoint sit only in the
//!   write-ahead log, which a lease-less reader does not read: it sees the
//!   state as of that checkpoint, never a torn one, and cannot disturb the
//!   writer. It never creates the path.
//! - `close()` checkpoints when there are unsaved changes (so `durability:
//!   'off'` loses nothing on a clean exit), then closes the log and releases
//!   the lease. A failed checkpoint leaves the graph open. Idempotent; every
//!   other call afterwards rejects `Closed`.
//! - A writable graph that is dropped without `close()` keeps what its
//!   durability level promises: `full`/`normal` recover from the log on the
//!   next open; `off` loses whatever was never checkpointed.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration as StdDuration;

use kglite::api::durable::DurabilityLevel;
use kglite::api::io::{load_file, GraphWriterLease};
use kglite::api::session::{
    open_path, ExecuteOptions, ExecuteOutcome, OpenError, OpenSpec, QueryDefaults, Session,
};
use kglite::api::storage::StorageMode;
use kglite::api::Value;
use napi::bindgen_prelude::{Object, ToNapiValue};
use napi::{sys, Env, JsValue, Unknown};
use napi_derive::napi;

use crate::contain;
use crate::errors::{
    to_sync_error, Holder, JsErr, JsRes, CODE_CLOSED, CODE_LEASE_HELD, CODE_NOT_DURABLE,
    CODE_READ_ONLY,
};
use crate::pool::{self, Settle};
use crate::tx::TxShared;
use crate::values::{FromJs, IntegerMode, ToJs};

/// Attempts an auto-commit write makes while it keeps losing an optimistic race.
/// Writes on one `Graph` are serialised, so only an outside committer causes a retry.
const WRITE_ATTEMPTS: u32 = 5;

pub(crate) struct Inner {
    /// `None` once [`Graph::close`] has run; taking it out is what finally closes
    /// the write-ahead log, so a successor writer never meets a live handle on it.
    pub(crate) session: Mutex<Option<Arc<Session>>>,
    /// Held for as long as the graph is writable; `None` for a `readOnly` graph.
    pub(crate) lease: Mutex<Option<GraphWriterLease>>,
    pub(crate) path: String,
    pub(crate) durability: DurabilityLevel,
    pub(crate) read_only: bool,
    /// The path did not hold a graph when it was opened.
    pub(crate) created: bool,
    /// Version right after open (and recovery), the baseline for "unchanged".
    pub(crate) open_version: u64,
    /// Version of the last checkpoint this handle wrote.
    pub(crate) last_checkpoint: Mutex<Option<u64>>,
    pub(crate) closed: AtomicBool,
    pub(crate) warnings: Vec<String>,
    pub(crate) defaults: QueryDefaults,
    pub(crate) ints: IntegerMode,
    /// Serialises auto-commit writes, checkpoints and `close` so none races another.
    pub(crate) write_lock: Mutex<()>,
    /// Open transactions, so `close` can roll them back.
    pub(crate) txs: Mutex<Vec<Weak<TxShared>>>,
}

impl Inner {
    pub(crate) fn session(&self) -> JsRes<Arc<Session>> {
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(closed_error)
    }
}

pub(crate) fn closed_error() -> JsErr {
    JsErr::new(CODE_CLOSED, "this graph is closed")
}

pub(crate) fn read_only_error(what: &str) -> JsErr {
    JsErr::new(
        CODE_READ_ONLY,
        format!("{what} is not available: this graph was opened with readOnly: true"),
    )
}

/// A graph opened by [`open`].
#[napi]
pub struct Graph {
    pub(crate) inner: Arc<Inner>,
}

// ------------------------------------------------------------------ options

struct OpenConfig {
    spec: OpenSpec,
    read_only: bool,
    defaults: QueryDefaults,
    ints: IntegerMode,
}

fn count(f: &FromJs, v: sys::napi_value, what: &str) -> JsRes<u64> {
    let n = f.get_f64(v)?;
    if n.is_finite() && n >= 0.0 && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0 {
        Ok(n as u64)
    } else {
        Err(JsErr::arg(format!("{what} must be a non-negative integer")))
    }
}

fn expect_number(f: &FromJs, v: sys::napi_value, what: &str) -> JsRes<u64> {
    if !f.kind(v)?.is_number() {
        return Err(JsErr::arg(format!("{what} must be a number")));
    }
    count(f, v, what)
}

pub(crate) fn expect_string(f: &FromJs, v: sys::napi_value, what: &str) -> JsRes<String> {
    if !f.kind(v)?.is_string() {
        return Err(JsErr::arg(format!("{what} must be a string")));
    }
    f.get_string(v)
}

/// The defined own entries of an options object; unknown keys are an error so a
/// misspelt option never silently does nothing.
pub(crate) fn option_entries(
    f: &mut FromJs,
    v: Option<sys::napi_value>,
    known: &[&str],
    what: &str,
) -> JsRes<Vec<(String, sys::napi_value)>> {
    let Some(v) = v else { return Ok(Vec::new()) };
    if f.is_nullish(v)? {
        return Ok(Vec::new());
    }
    if !f.is_plain_object(v)? {
        return Err(JsErr::arg(format!("{what} must be an object")));
    }
    let mut out = Vec::new();
    for key in f.own_keys(v)? {
        if !known.contains(&key.as_str()) {
            return Err(JsErr::arg(format!(
                "unknown {what} field `{key}` (expected one of: {})",
                known.join(", ")
            )));
        }
        let child = f.get_property(v, &key)?;
        if !f.is_nullish(child)? {
            out.push((key, child));
        }
    }
    Ok(out)
}

fn parse_open_options(f: &mut FromJs, v: Option<sys::napi_value>) -> JsRes<OpenConfig> {
    let mut spec = OpenSpec::writer();
    spec.durability = DurabilityLevel::Full;
    spec.durability_explicit = false;
    spec.storage = None;
    let mut defaults = QueryDefaults::default();
    let mut ints = IntegerMode::Safe;
    let mut read_only = false;
    let mut lock_explicit = false;
    let known = [
        "durability",
        "storage",
        "readOnly",
        "lockTimeoutMs",
        "timeoutMs",
        "rowLimit",
        "integers",
    ];
    for (key, val) in option_entries(f, v, &known, "open option")? {
        match key.as_str() {
            "durability" => {
                spec.durability = match expect_string(f, val, "durability")?.as_str() {
                    "full" => DurabilityLevel::Full,
                    "normal" => DurabilityLevel::Normal,
                    "off" => DurabilityLevel::Off,
                    other => {
                        return Err(JsErr::arg(format!(
                            "durability must be 'full', 'normal' or 'off', got '{other}'"
                        )))
                    }
                };
                spec.durability_explicit = true;
            }
            "storage" => {
                spec.storage = Some(match expect_string(f, val, "storage")?.as_str() {
                    "memory" => StorageMode::Memory,
                    "mapped" => StorageMode::Mapped,
                    "disk" => StorageMode::Disk,
                    other => {
                        return Err(JsErr::arg(format!(
                            "storage must be 'memory', 'mapped' or 'disk', got '{other}'"
                        )))
                    }
                });
            }
            "readOnly" => {
                if !f.kind(val)?.is_boolean() {
                    return Err(JsErr::arg("readOnly must be a boolean"));
                }
                read_only = f.get_bool(val)?;
            }
            "lockTimeoutMs" => {
                lock_explicit = true;
                spec.lease_timeout = Some(StdDuration::from_millis(expect_number(
                    f,
                    val,
                    "lockTimeoutMs",
                )?));
            }
            "timeoutMs" => defaults.timeout_ms = Some(expect_number(f, val, "timeoutMs")?),
            "rowLimit" => defaults.row_limit = Some(expect_number(f, val, "rowLimit")? as usize),
            "integers" => {
                ints = match expect_string(f, val, "integers")?.as_str() {
                    "safe" => IntegerMode::Safe,
                    "bigint" => IntegerMode::BigInt,
                    other => {
                        return Err(JsErr::arg(format!(
                            "integers must be 'safe' or 'bigint', got '{other}'"
                        )))
                    }
                };
            }
            _ => unreachable!("filtered by option_entries"),
        }
    }
    if read_only && (spec.durability_explicit || spec.storage.is_some() || lock_explicit) {
        return Err(JsErr::arg(
            "readOnly cannot be combined with durability, storage or lockTimeoutMs: \
             a read-only graph loads the last checkpoint and takes no lease",
        ));
    }
    Ok(OpenConfig {
        spec,
        read_only,
        defaults,
        ints,
    })
}

pub(crate) struct QueryArgs {
    pub(crate) cypher: String,
    pub(crate) params: std::collections::HashMap<String, Value>,
    pub(crate) timeout_ms: Option<u64>,
    pub(crate) row_limit: Option<usize>,
    pub(crate) max_work_units: Option<usize>,
}

pub(crate) fn parse_query_args(
    f: &mut FromJs,
    cypher: sys::napi_value,
    params: Option<sys::napi_value>,
    options: Option<sys::napi_value>,
) -> JsRes<QueryArgs> {
    let cypher = expect_string(f, cypher, "cypher")?;
    let params = f.params(params)?;
    let mut args = QueryArgs {
        cypher,
        params,
        timeout_ms: None,
        row_limit: None,
        max_work_units: None,
    };
    let known = ["timeoutMs", "rowLimit", "maxWorkUnits"];
    for (key, val) in option_entries(f, options, &known, "query option")? {
        let n = expect_number(f, val, &key)?;
        match key.as_str() {
            "timeoutMs" => args.timeout_ms = Some(n),
            "rowLimit" => args.row_limit = Some(n as usize),
            _ => args.max_work_units = Some(n as usize),
        }
    }
    Ok(args)
}

// ------------------------------------------------------------------ results

pub(crate) fn build_result(
    env: sys::napi_env,
    outcome: &ExecuteOutcome,
    ints: IntegerMode,
) -> JsRes<sys::napi_value> {
    let js = ToJs::new(env, ints);
    let result = &outcome.result;
    let out = js.object()?;

    let columns = js.array(result.columns.len())?;
    let mut keys = Vec::with_capacity(result.columns.len());
    for (i, name) in result.columns.iter().enumerate() {
        js.push(columns, i, js.string(name)?)?;
        keys.push(js.cached_key(name)?);
    }
    js.set(out, "columns", columns)?;

    let rows = js.array(result.rows.len())?;
    for (i, row) in result.rows.iter().enumerate() {
        js.scope(|| {
            let obj = js.object()?;
            for (key, value) in keys.iter().zip(row) {
                js.set_keyed(obj, key, js.value(value, 0)?)?;
            }
            js.push(rows, i, obj)
        })?;
    }
    js.set(out, "rows", rows)?;

    if let Some(stats) = &result.stats {
        let s = js.object()?;
        for (name, n) in [
            ("nodesCreated", stats.nodes_created),
            ("relationshipsCreated", stats.relationships_created),
            ("propertiesSet", stats.properties_set),
            ("nodesDeleted", stats.nodes_deleted),
            ("relationshipsDeleted", stats.relationships_deleted),
            ("propertiesRemoved", stats.properties_removed),
            ("indexesAdded", stats.indexes_added),
            ("indexesRemoved", stats.indexes_removed),
            ("constraintsAdded", stats.constraints_added),
            ("constraintsRemoved", stats.constraints_removed),
        ] {
            js.set(s, name, js.number(n as f64)?)?;
        }
        js.set(out, "stats", s)?;
    }

    let diagnostics = result.diagnostics.as_ref();
    let warnings_src: &[String] = diagnostics.map_or(&[], |d| d.warnings.as_slice());
    let warnings = js.array(warnings_src.len())?;
    for (i, w) in warnings_src.iter().enumerate() {
        js.push(warnings, i, js.string(w)?)?;
    }
    js.set(out, "warnings", warnings)?;

    if let Some((limit, total)) = diagnostics.and_then(|d| Some((d.row_limit?, d.total_rows?))) {
        let t = js.object()?;
        js.set(t, "rowLimit", js.number(limit as f64)?)?;
        js.set(t, "totalRows", js.number(total as f64)?)?;
        js.set(out, "truncated", t)?;
    }
    Ok(out)
}

// ------------------------------------------------------------------ dispatch

pub(crate) fn failed(e: JsErr) -> Settle {
    Box::new(move |_| Err(e))
}

pub(crate) fn err_promise<'e>(env: &'e Env, e: JsErr) -> napi::Result<Object<'e>, &'static str> {
    pool::settled(env, failed(e)).map_err(|e| to_sync_error(JsErr::from(e)))
}

fn undefined(env: sys::napi_env) -> JsRes<sys::napi_value> {
    let mut out = std::ptr::null_mut();
    if unsafe { sys::napi_get_undefined(env, &mut out) } != sys::Status::napi_ok {
        return Err(JsErr::internal("napi_get_undefined failed"));
    }
    Ok(out)
}

pub(crate) fn done() -> Settle {
    Box::new(|env: Env| undefined(env.raw()))
}

pub(crate) fn save_error(message: String) -> JsErr {
    JsErr::new("FileIo", message)
}

impl Inner {
    /// One call's execution options: the arguments laid over the open-time defaults.
    pub(crate) fn execute_options<'a>(&self, args: &'a QueryArgs) -> ExecuteOptions<'a> {
        let resolved = self
            .defaults
            .resolve(args.timeout_ms, args.max_work_units, args.row_limit);
        let mut opts = ExecuteOptions::eager(&args.params);
        opts.streaming = true;
        opts.deadline = resolved.deadline;
        opts.deadline_origin = resolved.deadline_origin;
        opts.max_work_units = resolved.max_work_units;
        opts.row_limit = resolved.row_limit;
        opts
    }

    /// Write a checkpoint unless nothing changed since this handle's last one.
    /// The caller holds `write_lock`.
    fn checkpoint_locked(&self, session: &Session) -> JsRes<()> {
        let mut last = self
            .last_checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        session
            .checkpoint_if_changed(Path::new(&self.path), &mut last)
            .map(|_| ())
            .map_err(save_error)
    }

    /// Whether `close` has anything to write: a change since the last checkpoint
    /// (or, with none yet, since open), or a graph that has no file yet.
    fn dirty(&self, session: &Session) -> bool {
        let last = *self
            .last_checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match last {
            Some(v) => session.version() != v,
            None => self.created || session.version() != self.open_version,
        }
    }
}

impl Graph {
    fn run<'e>(
        &self,
        env: &'e Env,
        write: bool,
        cypher: Unknown,
        params: Option<Unknown>,
        options: Option<Unknown>,
    ) -> napi::Result<Object<'e>, &'static str> {
        let mut f = FromJs::new(env.raw());
        let parsed = parse_query_args(
            &mut f,
            cypher.raw(),
            params.as_ref().map(|p| p.raw()),
            options.as_ref().map(|o| o.raw()),
        );
        let args = match parsed {
            Ok(a) => a,
            Err(e) => return err_promise(env, e),
        };
        if write && self.inner.read_only {
            return err_promise(env, read_only_error("executeWrite"));
        }
        let inner = Arc::clone(&self.inner);
        let promise = pool::spawn(env, move || {
            let opts = inner.execute_options(&args);
            let outcome = if write {
                let _serial = inner
                    .write_lock
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match inner.session() {
                    Ok(session) => session.execute_auto_commit(&args.cypher, &opts, WRITE_ATTEMPTS),
                    Err(e) => return failed(e),
                }
            } else {
                match inner.session() {
                    Ok(session) => {
                        let snapshot = session.snapshot();
                        kglite::api::session::execute_read(&snapshot, &args.cypher, &opts)
                    }
                    Err(e) => return failed(e),
                }
            };
            let ints = inner.ints;
            match outcome {
                Ok(outcome) => Box::new(move |env: Env| build_result(env.raw(), &outcome, ints)),
                Err(e) => failed(JsErr::from_kg(&e)),
            }
        });
        promise.map_err(|e| to_sync_error(JsErr::from(e)))
    }

    /// Ship `work` to the pool, rejecting at once when the graph is already closed.
    fn lifecycle<'e>(
        &self,
        env: &'e Env,
        work: impl FnOnce(&Inner) -> Settle + Send + 'static,
    ) -> napi::Result<Object<'e>, &'static str> {
        let inner = Arc::clone(&self.inner);
        pool::spawn(env, move || work(&inner)).map_err(|e| to_sync_error(JsErr::from(e)))
    }
}

#[napi]
impl Graph {
    /// Run a read-only Cypher statement. A mutating statement rejects with `InvalidArgument`.
    #[napi(
        ts_args_type = "cypher: string, params?: Params | null, options?: QueryOptions",
        ts_return_type = "Promise<QueryResult>"
    )]
    pub fn execute_read<'e>(
        &self,
        env: &'e Env,
        cypher: Unknown,
        params: Option<Unknown>,
        options: Option<Unknown>,
    ) -> napi::Result<Object<'e>, &'static str> {
        contain(|| self.run(env, false, cypher, params, options))
    }

    /// Run a Cypher statement that may write, as one auto-committed transaction.
    #[napi(
        ts_args_type = "cypher: string, params?: Params | null, options?: QueryOptions",
        ts_return_type = "Promise<QueryResult>"
    )]
    pub fn execute_write<'e>(
        &self,
        env: &'e Env,
        cypher: Unknown,
        params: Option<Unknown>,
        options: Option<Unknown>,
    ) -> napi::Result<Object<'e>, &'static str> {
        contain(|| self.run(env, true, cypher, params, options))
    }

    /// Write a checkpoint (folding the write-ahead log) unless nothing changed since this handle's last one.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn checkpoint<'e>(&self, env: &'e Env) -> napi::Result<Object<'e>, &'static str> {
        contain(|| {
            if self.inner.read_only {
                return err_promise(env, read_only_error("checkpoint()"));
            }
            self.lifecycle(env, |inner| {
                let _serial = inner
                    .write_lock
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let session = match inner.session() {
                    Ok(s) => s,
                    Err(e) => return failed(e),
                };
                match inner.checkpoint_locked(&session) {
                    Ok(()) => done(),
                    Err(e) => failed(e),
                }
            })
        })
    }

    /// Flush the write-ahead log to stable storage (the power-safe point at durability `normal`).
    #[napi(ts_return_type = "Promise<void>")]
    pub fn sync<'e>(&self, env: &'e Env) -> napi::Result<Object<'e>, &'static str> {
        contain(|| {
            if self.inner.read_only {
                return err_promise(env, read_only_error("sync()"));
            }
            self.lifecycle(env, |inner| {
                let session = match inner.session() {
                    Ok(s) => s,
                    Err(e) => return failed(e),
                };
                if session.durability().is_none() {
                    return failed(JsErr::new(
                        CODE_NOT_DURABLE,
                        "sync() needs durability 'full' or 'normal'; this graph has no write-ahead log \
                         (use checkpoint() to write it to disk)",
                    ));
                }
                match session.sync() {
                    Ok(()) => done(),
                    Err(m) => failed(JsErr::new("DurabilityFailed", m)),
                }
            })
        })
    }

    /// Checkpoint if there are unsaved changes (writable graphs), release the writer lease and close the graph. Idempotent.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn close<'e>(&self, env: &'e Env) -> napi::Result<Object<'e>, &'static str> {
        contain(|| {
            self.lifecycle(env, |inner| {
                let _serial = inner
                    .write_lock
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if inner.closed.load(Ordering::Acquire) {
                    return done();
                }
                if !inner.read_only {
                    let session = match inner.session() {
                        Ok(s) => s,
                        Err(e) => return failed(e),
                    };
                    if inner.dirty(&session) {
                        // A failed checkpoint leaves the graph open and the lease held,
                        // so the caller can retry rather than lose the changes.
                        if let Err(e) = inner.checkpoint_locked(&session) {
                            return failed(e);
                        }
                    }
                }
                inner.closed.store(true, Ordering::Release);
                inner.abandon_transactions();
                // Session first (closes the log), lease second (admits a successor).
                drop(
                    inner
                        .session
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take(),
                );
                drop(
                    inner
                        .lease
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .take(),
                );
                done()
            })
        })
    }

    /// The path this graph was opened at.
    #[napi(getter)]
    pub fn path(&self) -> napi::Result<String, &'static str> {
        contain(|| Ok(self.inner.path.clone()))
    }

    /// The durability level in force (`off` on a `readOnly` graph; an inherited level degrades to `off` on a disk graph).
    #[napi(getter, ts_return_type = "'full' | 'normal' | 'off'")]
    pub fn durability(&self) -> napi::Result<String, &'static str> {
        contain(|| {
            Ok(match self.inner.durability {
                DurabilityLevel::Full => "full",
                DurabilityLevel::Normal => "normal",
                DurabilityLevel::Off => "off",
            }
            .to_string())
        })
    }

    /// Whether the graph was opened with `readOnly: true`.
    #[napi(getter)]
    pub fn read_only(&self) -> napi::Result<bool, &'static str> {
        contain(|| Ok(self.inner.read_only))
    }

    /// Whether `close()` has completed.
    #[napi(getter)]
    pub fn closed(&self) -> napi::Result<bool, &'static str> {
        contain(|| Ok(self.inner.closed.load(Ordering::Acquire)))
    }

    /// Notices from opening the graph (a quarantined or repaired write-ahead log, a degraded durability level, a storage conversion).
    #[napi(getter)]
    pub fn open_warnings(&self) -> napi::Result<Vec<String>, &'static str> {
        contain(|| Ok(self.inner.warnings.clone()))
    }
}

fn io_error(io: &std::io::Error) -> JsErr {
    use std::io::ErrorKind;
    JsErr::new(
        match io.kind() {
            ErrorKind::NotFound => "FileNotFound",
            ErrorKind::InvalidData => "FileFormat",
            _ => "FileIo",
        },
        io.to_string(),
    )
}

fn open_error(e: &OpenError) -> JsErr {
    match e {
        OpenError::Lease(refusal) => match &refusal.holder {
            Some(h) if refusal.error.kind() == std::io::ErrorKind::WouldBlock => {
                let mut err = JsErr::new(CODE_LEASE_HELD, refusal.error.to_string());
                err.holder = Some(Holder {
                    is_self: h.pid == Some(std::process::id()),
                    pid: h.pid,
                    since: h.since.clone(),
                    label: h.label.clone(),
                });
                err
            }
            _ => io_error(&refusal.error),
        },
        OpenError::Open(io) => io_error(io),
        OpenError::Session { message, .. } => JsErr::new("FileIo", message.clone()),
    }
}

fn open_writer(path: &str, config: OpenConfig) -> Result<Inner, JsErr> {
    let p = Path::new(path);
    let mut spec = config.spec;
    let existed = p.exists();
    // A missing path is created in memory unless the caller chose a mode;
    // an existing one keeps whatever mode its checkpoint is in.
    if spec.storage.is_none() && !existed {
        spec.storage = Some(StorageMode::Memory);
    }
    spec.lease_timeout = Some(spec.lease_timeout.unwrap_or(StdDuration::ZERO));
    let opened = open_path(p, &spec).map_err(|e| open_error(&e))?;
    let mut warnings: Vec<String> = opened
        .advisories
        .iter()
        .map(|a| format!("{}: {}", a.code, a.message))
        .collect();
    if let Some(requested) = opened.degraded_from {
        warnings.push(format!(
            "durability '{}' is not available for storage 'disk'; running at 'off' (use checkpoint() to persist)",
            requested.name()
        ));
    }
    if let Some(from) = opened.converted_from {
        warnings.push(format!(
            "storage converted from '{}' to '{}'",
            from.as_str(),
            opened.live_mode.as_str()
        ));
    }
    let session = opened.session;
    Ok(Inner {
        open_version: session.version(),
        session: Mutex::new(Some(Arc::new(session))),
        lease: Mutex::new(opened.lease),
        path: path.to_string(),
        durability: opened.durability,
        read_only: false,
        created: !existed,
        last_checkpoint: Mutex::new(None),
        closed: AtomicBool::new(false),
        warnings,
        defaults: config.defaults,
        ints: config.ints,
        write_lock: Mutex::new(()),
        txs: Mutex::new(Vec::new()),
    })
}

/// A lease-less open: the last checkpoint on disk, with no write-ahead log read
/// and nothing published back. It never creates the path.
fn open_reader(path: &str, config: OpenConfig) -> Result<Inner, JsErr> {
    let graph = load_file(path).map_err(|e| io_error(&e))?;
    let warnings = graph
        .advisories
        .iter()
        .map(|a| format!("{}: {}", a.code, a.message))
        .collect();
    let session = Session::from_arc(graph);
    Ok(Inner {
        open_version: session.version(),
        session: Mutex::new(Some(Arc::new(session))),
        lease: Mutex::new(None),
        path: path.to_string(),
        durability: DurabilityLevel::Off,
        read_only: true,
        created: false,
        last_checkpoint: Mutex::new(None),
        closed: AtomicBool::new(false),
        warnings,
        defaults: config.defaults,
        ints: config.ints,
        write_lock: Mutex::new(()),
        txs: Mutex::new(Vec::new()),
    })
}

/// Open (or create) the graph at `path`.
#[napi(
    ts_args_type = "path: string, options?: OpenOptions",
    ts_return_type = "Promise<Graph>"
)]
pub fn open<'e>(
    env: &'e Env,
    path: Unknown,
    options: Option<Unknown>,
) -> napi::Result<Object<'e>, &'static str> {
    contain(|| {
        let mut f = FromJs::new(env.raw());
        let parsed = expect_string(&f, path.raw(), "path").and_then(|p| {
            if p.is_empty() {
                return Err(JsErr::arg("path must not be empty"));
            }
            parse_open_options(&mut f, options.as_ref().map(|o| o.raw())).map(|c| (p, c))
        });
        let (path, config) = match parsed {
            Ok(x) => x,
            Err(e) => return err_promise(env, e),
        };
        pool::spawn(env, move || {
            let opened = if config.read_only {
                open_reader(&path, config)
            } else {
                open_writer(&path, config)
            };
            match opened {
                Ok(inner) => {
                    let inner = Arc::new(inner);
                    Box::new(move |env: Env| {
                        unsafe { Graph::to_napi_value(env.raw(), Graph { inner }) }
                            .map_err(JsErr::from)
                    })
                }
                Err(e) => failed(e),
            }
        })
        .map_err(|e| to_sync_error(JsErr::from(e)))
    })
}
