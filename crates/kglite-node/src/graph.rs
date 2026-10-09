//! `open`, `Graph.executeRead` and `Graph.executeWrite`.
//!
//! Every method validates its arguments on the JS thread, ships the engine work
//! to the worker pool, and builds the result back on the JS thread. A bad
//! argument rejects the returned promise; it never throws synchronously.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration as StdDuration;

use kglite::api::durable::DurabilityLevel;
use kglite::api::session::{
    open_path, ExecuteOptions, ExecuteOutcome, OpenError, OpenSpec, OpenedSession, QueryDefaults,
};
use kglite::api::storage::StorageMode;
use kglite::api::Value;
use napi::bindgen_prelude::{Object, ToNapiValue};
use napi::{sys, Env, JsValue, Unknown};
use napi_derive::napi;

use crate::contain;
use crate::errors::{to_sync_error, JsErr, JsRes, CODE_LEASE_HELD};
use crate::pool::{self, Settle};
use crate::values::{FromJs, IntegerMode, ToJs};

/// Attempts an auto-commit write makes while it keeps losing an optimistic race.
/// Writes on one `Graph` are serialised, so only an outside committer causes a retry.
const WRITE_ATTEMPTS: u32 = 5;

struct Inner {
    opened: OpenedSession,
    path: String,
    defaults: QueryDefaults,
    ints: IntegerMode,
    /// Serialises auto-commit writes so they never race one another to commit.
    write_lock: Mutex<()>,
}

/// A graph opened by [`open`].
#[napi]
pub struct Graph {
    inner: Arc<Inner>,
}

// ------------------------------------------------------------------ options

struct OpenConfig {
    spec: OpenSpec,
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

fn expect_string(f: &FromJs, v: sys::napi_value, what: &str) -> JsRes<String> {
    if !f.kind(v)?.is_string() {
        return Err(JsErr::arg(format!("{what} must be a string")));
    }
    f.get_string(v)
}

/// The defined own entries of an options object; unknown keys are an error so a
/// misspelt option never silently does nothing.
fn option_entries(
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
                if f.get_bool(val)? {
                    return Err(JsErr::arg("readOnly is not supported yet"));
                }
            }
            "lockTimeoutMs" => {
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
    Ok(OpenConfig {
        spec,
        defaults,
        ints,
    })
}

struct QueryArgs {
    cypher: String,
    params: std::collections::HashMap<String, Value>,
    timeout_ms: Option<u64>,
    row_limit: Option<usize>,
    max_work_units: Option<usize>,
}

fn parse_query_args(
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

fn build_result(
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

fn failed(e: JsErr) -> Settle {
    Box::new(move |_| Err(e))
}

fn err_promise<'e>(env: &'e Env, e: JsErr) -> napi::Result<Object<'e>, &'static str> {
    pool::settled(env, failed(e)).map_err(|e| to_sync_error(JsErr::from(e)))
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
        let inner = Arc::clone(&self.inner);
        let resolved = inner
            .defaults
            .resolve(args.timeout_ms, args.max_work_units, args.row_limit);
        let promise = pool::spawn(env, move || {
            let mut opts = ExecuteOptions::eager(&args.params);
            opts.streaming = true;
            opts.deadline = resolved.deadline;
            opts.deadline_origin = resolved.deadline_origin;
            opts.max_work_units = resolved.max_work_units;
            opts.row_limit = resolved.row_limit;
            let session = &inner.opened.session;
            let outcome = if write {
                let _serial = inner
                    .write_lock
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                session.execute_auto_commit(&args.cypher, &opts, WRITE_ATTEMPTS)
            } else {
                let snapshot = session.snapshot();
                kglite::api::session::execute_read(&snapshot, &args.cypher, &opts)
            };
            let ints = inner.ints;
            match outcome {
                Ok(outcome) => Box::new(move |env: Env| build_result(env.raw(), &outcome, ints)),
                Err(e) => failed(JsErr::from_kg(&e)),
            }
        });
        promise.map_err(|e| to_sync_error(JsErr::from(e)))
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

    /// The path this graph was opened at.
    #[napi(getter)]
    pub fn path(&self) -> napi::Result<String, &'static str> {
        contain(|| Ok(self.inner.path.clone()))
    }

    /// The durability level in force (an inherited level degrades to `off` on a disk graph).
    #[napi(getter, ts_return_type = "'full' | 'normal' | 'off'")]
    pub fn durability(&self) -> napi::Result<String, &'static str> {
        contain(|| {
            Ok(match self.inner.opened.durability {
                DurabilityLevel::Full => "full",
                DurabilityLevel::Normal => "normal",
                DurabilityLevel::Off => "off",
            }
            .to_string())
        })
    }
}

fn open_error(e: &OpenError) -> JsErr {
    use std::io::ErrorKind;
    match e {
        OpenError::Lease(io) => JsErr::new(CODE_LEASE_HELD, io.to_string()),
        OpenError::Open(io) => JsErr::new(
            match io.kind() {
                ErrorKind::NotFound => "FileNotFound",
                ErrorKind::InvalidData => "FileFormat",
                _ => "FileIo",
            },
            io.to_string(),
        ),
        OpenError::Session { message, .. } => JsErr::new("FileIo", message.clone()),
    }
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
            let mut spec = config.spec;
            // A missing path is created in memory unless the caller chose a mode;
            // an existing one keeps whatever mode its checkpoint is in.
            if spec.storage.is_none() && !Path::new(&path).exists() {
                spec.storage = Some(StorageMode::Memory);
            }
            match open_path(Path::new(&path), &spec) {
                Ok(opened) => {
                    let inner = Arc::new(Inner {
                        opened,
                        path,
                        defaults: config.defaults,
                        ints: config.ints,
                        write_lock: Mutex::new(()),
                    });
                    Box::new(move |env: Env| {
                        unsafe { Graph::to_napi_value(env.raw(), Graph { inner }) }
                            .map_err(JsErr::from)
                    })
                }
                Err(e) => failed(open_error(&e)),
            }
        })
        .map_err(|e| to_sync_error(JsErr::from(e)))
    })
}
