//! The durable half of the Python `Session`: `kglite.open_session(path,
//! durable=...)`, the same core entry (`open_path`) the Bolt server, the Node
//! binding and the C ABI open through.
//!
//! A durable session owns the path's writer lease from open until `close()`.
//! Writes go through the core's `execute_auto_commit` with no PyO3 borrow held
//! and the GIL detached, so threads queue into the core's group commit (one
//! log barrier per batch at `full`) instead of raising a borrow error; reads
//! take a snapshot GIL-free, as on every `Session`. Only what is specific to
//! Python lives here: exception classes, result marshalling, the context
//! manager, and the thread that folds the log into the checkpoint.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock, RwLockReadGuard};

use super::query_defaults::QueryDefaults;
use super::result_view::ResultView;
use super::session::{decode_params, marshal_result, QueryOpts, Session};
use crate::datatypes::values::Value;
use crate::error::KgError;
use crate::graph::languages::cypher;
use crate::util::EnterKg;
use kglite_core::api::durable::{DurabilityLevel, DurableOpenError};
use kglite_core::api::io::GraphWriterLease;
use kglite_core::api::session::{
    execute_mut, execute_read, open_path, CommitOutcome, CsvImportPolicy, ExecuteOptions,
    LogOpenFailure, OpenError, OpenSpec, Session as CoreSession, Transaction as CoreTransaction,
    QUERY_THREAD_STACK_SIZE,
};
use kglite_core::api::storage::StorageMode;

/// Attempts a one-shot write makes while it loses an optimistic race. The
/// grouped and in-place paths never lose one, so this bounds only the fork
/// path (an embedder, schema DDL, a procedure call).
const AUTO_COMMIT_ATTEMPTS: u32 = 8;

/// What a durable `Session` holds beside the core session.
pub(crate) struct DurableHandle {
    path: String,
    level: DurabilityLevel,
    /// Dropping it releases the path to the next opener.
    lease: Mutex<Option<GraphWriterLease>>,
    /// Writers hold the read side across a statement or a commit; `close`
    /// holds the write side, so it waits for in-flight writes and none begins
    /// once it has run.
    gate: RwLock<()>,
    closed: AtomicBool,
    /// The graph version last written to the checkpoint by this process.
    last_checkpoint: Mutex<Option<u64>>,
    open_version: u64,
    created: bool,
    /// Online checkpoints in flight, so `close` can wait them out.
    background: Mutex<u32>,
    background_idle: Condvar,
}

fn closed_error() -> KgError {
    KgError::DurabilityFailed {
        message: "this session is closed, so it no longer accepts writes, checkpoints or \
                  commits; reopen the path with kglite.open_session(...)"
            .to_string(),
    }
}

fn not_durable_error(verb: &str) -> PyErr {
    crate::error_py::kg_to_pyerr(KgError::not_durable(format!(
        "Session.{verb}() needs a session opened with kglite.open_session(path, durable=...). \
         This one was created by KnowledgeGraph.session() or kglite.open_session(path) without \
         `durable`, which own no persistence."
    )))
}

fn durability_failed(message: String) -> PyErr {
    crate::error_py::kg_to_pyerr(KgError::DurabilityFailed { message })
}

impl DurableHandle {
    /// The write side of the lifecycle gate, refused once the session closed.
    fn writer(&self) -> Result<RwLockReadGuard<'_, ()>, Box<KgError>> {
        let guard = self.gate.read().unwrap_or_else(PoisonError::into_inner);
        if self.closed.load(Ordering::Acquire) {
            return Err(Box::new(closed_error()));
        }
        Ok(guard)
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn record_checkpoint(&self, version: u64) {
        *self
            .last_checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(version);
    }

    /// Whether `close` has anything to write: a change since the last
    /// checkpoint (or, with none yet, since open), or a graph with no file yet.
    fn dirty(&self, core: &CoreSession) -> bool {
        let last = *self
            .last_checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match last {
            Some(version) => core.version() != version,
            None => self.created || core.version() != self.open_version,
        }
    }

    /// After a commit: when the log outgrew its bound, fold it into the
    /// checkpoint on a thread of its own. The commit's result is already on
    /// its way and the online checkpoint holds the session locks only to fix
    /// its snapshot and to trim the log, so writers keep committing through the
    /// file write. A failure is not the commit's failure: the core backs the
    /// policy off by one bound of log growth, and `save()` / `close()` report
    /// their own errors.
    fn kick_checkpoint(self: &Arc<Self>, core: &Arc<CoreSession>) {
        if !core.needs_checkpoint() {
            return;
        }
        {
            let mut running = self
                .background
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            // Registered under the lock with the closed check, so `close`
            // either waits for this checkpoint or this call sees the close.
            if self.closed.load(Ordering::Acquire) {
                return;
            }
            *running += 1;
        }
        let handle = Arc::clone(self);
        let core = Arc::clone(core);
        let spawned = std::thread::Builder::new()
            .name("kglite-checkpoint".to_string())
            .stack_size(QUERY_THREAD_STACK_SIZE)
            .spawn({
                let handle = Arc::clone(&handle);
                move || {
                    if let Ok(Some(report)) = core.maybe_checkpoint_online() {
                        handle.record_checkpoint(report.graph_version);
                    }
                    drop(core);
                    handle.finish_background();
                }
            });
        if spawned.is_err() {
            handle.finish_background();
        }
    }

    fn finish_background(&self) {
        let mut running = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *running -= 1;
        if *running == 0 {
            self.background_idle.notify_all();
        }
    }

    fn wait_background_idle(&self) {
        let mut running = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while *running > 0 {
            running = self
                .background_idle
                .wait(running)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Checkpoint (when asked and there is something to write), then end the
    /// session's right to publish and release the lease. A failed checkpoint
    /// leaves the session open and the lease held, so the caller can retry
    /// instead of losing the changes. Idempotent.
    fn close(&self, core: &CoreSession, checkpoint: bool) -> Result<(), String> {
        let _exclusive = self.gate.write().unwrap_or_else(PoisonError::into_inner);
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        if checkpoint && self.dirty(core) {
            let version = core.version();
            core.save(&self.path, true)?;
            self.record_checkpoint(version);
        }
        self.closed.store(true, Ordering::Release);
        self.wait_background_idle();
        // Retire before the lease goes: a retained session or transaction
        // must not append to a log another process now owns.
        core.retire();
        drop(
            self.lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        Ok(())
    }
}

/// Map a failed `open_path` onto the exception classes `kglite.open` raises for
/// the same failure.
fn open_error_to_pyerr(
    error: OpenError,
    path: &str,
    requested_mode: Option<StorageMode>,
    existed: bool,
) -> PyErr {
    match error {
        OpenError::Lease(refusal) => crate::lease_refusal_to_pyerr(refusal),
        OpenError::Open(io) => {
            // The unrecovered-log refusal rides in the I/O error's source, so
            // it keeps the `ValueError` that `kglite.open` raises for it.
            if let Some(refused) = io
                .get_ref()
                .and_then(|source| source.downcast_ref::<DurableOpenError>())
            {
                let failure = match refused {
                    DurableOpenError::Io(_) => LogOpenFailure::Io,
                    DurableOpenError::Replay(_) => LogOpenFailure::Replay,
                    DurableOpenError::Refused(_) => LogOpenFailure::Refused,
                };
                return log_open_failure_to_pyerr(failure, io.to_string());
            }
            crate::open_err_to_pyerr(io, path, requested_mode, existed)
        }
        OpenError::Session {
            failure, message, ..
        } => log_open_failure_to_pyerr(failure, message),
    }
}

fn log_open_failure_to_pyerr(failure: LogOpenFailure, message: String) -> PyErr {
    match failure {
        LogOpenFailure::Io => PyErr::new::<pyo3::exceptions::PyIOError, _>(message),
        LogOpenFailure::Replay => PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(message),
        LogOpenFailure::Refused => PyErr::new::<pyo3::exceptions::PyValueError, _>(message),
    }
}

/// Open (or create) the graph at `path` as a durable, thread-safe `Session`.
pub(crate) fn open_durable_session(
    py: Python<'_>,
    path: String,
    storage: Option<&str>,
    level: DurabilityLevel,
    auto_checkpoint_wal_mib: Option<u64>,
) -> PyResult<Session> {
    let requested_mode = storage
        .map(|mode| {
            StorageMode::parse(mode).map_err(|e| crate::error_py::kg_to_pyerr(KgError::Argument(e)))
        })
        .transpose()?;
    let existed = Path::new(&path).exists();
    let spec = OpenSpec {
        // A mode is a *request* in core, so an existing checkpoint keeps its
        // own mode unless the caller named one.
        storage: requested_mode.or_else(|| (!existed).then_some(StorageMode::Memory)),
        durability: level,
        durability_explicit: true,
        // Fail fast, as `kglite.open` does: a blocked open is a worse failure
        // on a request path than a clear error the caller can retry around.
        lease_timeout: Some(std::time::Duration::ZERO),
        valid_time_default: None,
    };
    let opened = py
        .detach(|| open_path(Path::new(&path), &spec))
        .map_err(|e| open_error_to_pyerr(e, &path, requested_mode, existed))?;
    crate::warn_data_advisories(py, &opened.session.snapshot());
    if let Some(mib) = auto_checkpoint_wal_mib {
        opened
            .session
            .set_auto_checkpoint_wal_bytes((mib > 0).then_some(mib.saturating_mul(1 << 20)));
    }
    let handle = DurableHandle {
        path: path.clone(),
        level: opened.durability,
        lease: Mutex::new(opened.lease),
        gate: RwLock::new(()),
        closed: AtomicBool::new(false),
        last_checkpoint: Mutex::new(None),
        open_version: opened.session.version(),
        created: !existed,
        background: Mutex::new(0),
        background_idle: Condvar::new(),
    };
    Ok(Session::durable_owner(
        opened.session,
        Arc::new(handle),
        std::path::PathBuf::from(path),
    ))
}

impl Session {
    fn durable_handle(&self, verb: &str) -> PyResult<&Arc<DurableHandle>> {
        self.durable.as_ref().ok_or_else(|| not_durable_error(verb))
    }

    /// A write on a durable session: no PyO3 borrow is held (the class is
    /// `frozen`) and the GIL is released for the whole commit, including the
    /// wait on the gates, the group-commit queue and the log barrier.
    // The enter_kg closure preserves the engine's structured KgError until PyErr conversion.
    #[allow(clippy::result_large_err)]
    pub(super) fn run_durable_write(
        &self,
        py: Python<'_>,
        durable: &Arc<DurableHandle>,
        query: &str,
        param_map: HashMap<String, Value>,
        qopts: QueryOpts,
    ) -> PyResult<Py<PyAny>> {
        let core = Arc::clone(&self.inner);
        let handle = Arc::clone(durable);
        let query_owned = query.to_string();
        let write_scope = qopts.write_scope.clone();
        let git_sha = qopts.git_sha.clone();
        let modified_by = qopts.modified_by.clone();
        let (deadline, deadline_origin) = (qopts.deadline, qopts.deadline_origin);
        let (max_work_units, row_limit) = (qopts.max_work_units, qopts.row_limit);
        let outcome = py.enter_kg(move |cancel| {
            let opts = ExecuteOptions {
                params: &param_map,
                deadline,
                deadline_origin,
                max_work_units,
                row_limit,
                lazy_eligible: false,
                streaming: true,
                parallel: false,
                disabled_passes: None,
                embedder: None,
                value_codecs: None,
                cancel,
                write_scope: write_scope.as_ref(),
                git_sha: git_sha.as_deref(),
                modified_by: modified_by.as_deref(),
                csv_import: CsvImportPolicy::LocalFilesystem,
            };
            let outcome = {
                let _writer = handle.writer().map_err(|e| *e)?;
                core.execute_auto_commit(&query_owned, &opts, AUTO_COMMIT_ATTEMPTS)?
            };
            handle.kick_checkpoint(&core);
            Ok::<_, KgError>(outcome)
        })?;
        if outcome.explain {
            crate::warning_policy::announce(py, outcome.result.diagnostics.as_ref())?;
            return Py::new(py, ResultView::from_cypher_result(outcome.result))
                .map(|view| view.into_any());
        }
        marshal_result(py, outcome.result, qopts.to_df, qopts.output_csv)
    }
}

#[pymethods]
impl Session {
    /// Begin a read-write transaction on a durable session; commit is one log frame.
    fn begin(slf: &Bound<'_, Self>) -> PyResult<SessionTransaction> {
        let this = slf.get();
        let durable = this.durable_handle("begin")?;
        // Refused up front on a closed session, not only at commit.
        drop(
            durable
                .writer()
                .map_err(|e| crate::error_py::kg_to_pyerr(*e))?,
        );
        Ok(SessionTransaction {
            session: slf.clone().unbind(),
            inner: Some(this.inner.begin()),
            defaults: this.defaults,
        })
    }

    /// Write a checkpoint of the live graph to the session's path.
    #[pyo3(signature = (path=None))]
    fn save(&self, py: Python<'_>, path: Option<std::path::PathBuf>) -> PyResult<()> {
        let durable = self.durable_handle("save")?;
        if let Some(path) = path {
            if !same_target(&durable.path, &path) {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "Session.save() writes the session's own checkpoint ({:?}); to write a \
                     copy elsewhere use Session.backup(dest).",
                    durable.path
                )));
            }
        }
        let (core, handle) = (Arc::clone(&self.inner), Arc::clone(durable));
        py.detach(move || -> Result<(), Box<KgError>> {
            let _writer = handle.writer()?;
            let version = core.version();
            core.save(&handle.path, true)
                .map_err(|message| Box::new(KgError::DurabilityFailed { message }))?;
            handle.record_checkpoint(version);
            Ok(())
        })
        .map_err(|e| crate::error_py::kg_to_pyerr(*e))
    }

    /// Flush every committed log frame to stable storage (a no-op at `full`).
    fn sync(&self, py: Python<'_>) -> PyResult<()> {
        let durable = self.durable_handle("sync")?;
        if durable.level == DurabilityLevel::Off {
            return Err(crate::error_py::kg_to_pyerr(KgError::not_durable(
                "sync() needs a session with a write-ahead log. This one was opened with \
                 durable='off', so there is nothing to flush and no power-safe point to \
                 take; call save() to write a checkpoint, or reopen with \
                 kglite.open_session(path, durable='normal').",
            )));
        }
        let (core, handle) = (Arc::clone(&self.inner), Arc::clone(durable));
        py.detach(move || -> Result<(), Box<KgError>> {
            let _writer = handle.writer()?;
            core.sync()
                .map_err(|message| Box::new(KgError::DurabilityFailed { message }))
        })
        .map_err(|e| crate::error_py::kg_to_pyerr(*e))
    }

    /// Checkpoint and end the session, releasing the writer lease. Idempotent.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        self.close_with(py, true)
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Close on exit; a clean exit checkpoints, an exception skips the checkpoint.
    #[pyo3(signature = (exc_type, _exc_value, _traceback))]
    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: &Bound<'_, PyAny>,
        _exc_value: &Bound<'_, PyAny>,
        _traceback: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        if self.durable.is_some() {
            self.close_with(py, exc_type.is_none())?;
        }
        Ok(false)
    }

    /// Whether `close()` has completed (always False on a non-durable session).
    #[getter]
    fn closed(&self) -> bool {
        self.durable
            .as_ref()
            .is_some_and(|d| d.closed.load(Ordering::Acquire))
    }

    /// The path a durable session owns, or None.
    #[getter]
    fn path(&self) -> Option<String> {
        self.durable.as_ref().map(|d| d.path.clone())
    }

    /// The durability level in force (`full`, `normal`, `off`), or None for a non-durable session.
    #[getter]
    fn durability(&self) -> Option<&'static str> {
        self.durable.as_ref().map(|d| match d.level {
            DurabilityLevel::Full => "full",
            DurabilityLevel::Normal => "normal",
            DurabilityLevel::Off => "off",
        })
    }
}

impl Session {
    fn close_with(&self, py: Python<'_>, checkpoint: bool) -> PyResult<()> {
        let durable = self.durable_handle("close")?;
        let (core, handle) = (Arc::clone(&self.inner), Arc::clone(durable));
        py.detach(move || handle.close(&core, checkpoint))
            .map_err(|message| {
                // A failed checkpoint is a save failure; the session stays open.
                durability_failed(message)
            })
    }
}

/// Whether `candidate` names the checkpoint `owned`, lexically or on disk.
fn same_target(owned: &str, candidate: &Path) -> bool {
    let owned = Path::new(owned);
    owned == candidate
        || matches!(
            (owned.canonicalize(), candidate.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        )
}

/// A read-write transaction on a durable [`Session`]. One log frame per commit.
#[pyclass(module = "kglite")]
pub struct SessionTransaction {
    session: Py<Session>,
    inner: Option<CoreTransaction>,
    defaults: QueryDefaults,
}

fn finished_error() -> PyErr {
    crate::error_py::kg_to_pyerr(KgError::Argument(
        "Transaction already committed or rolled back".to_string(),
    ))
}

#[pymethods]
impl SessionTransaction {
    /// Run a Cypher query, read or write, against this transaction's working copy.
    // The Python boundary mirrors the public query-option surface; the enter_kg
    // closure preserves the engine's structured KgError until PyErr conversion.
    #[allow(clippy::too_many_arguments, clippy::result_large_err)]
    #[pyo3(signature = (query, params=None, to_df=false, timeout_ms=None, max_work_units=None, row_limit=None))]
    fn cypher(
        &mut self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyDict>>,
        to_df: bool,
        timeout_ms: Option<u64>,
        max_work_units: Option<usize>,
        row_limit: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        let pre_parsed = cypher::parse_cypher(query).map_err(crate::error_py::kg_to_pyerr)?;
        let is_mutation = cypher::is_mutation_query(&pre_parsed);
        let output_csv = pre_parsed.output_format == cypher::OutputFormat::Csv;
        let param_map = decode_params(params)?;
        let effective = self.defaults.resolve(timeout_ms, max_work_units, row_limit);
        let tx = self.inner.as_mut().ok_or_else(finished_error)?;
        let result = py.enter_kg(move |cancel| -> Result<cypher::CypherResult, KgError> {
            let opts = ExecuteOptions {
                params: &param_map,
                deadline: effective.deadline,
                deadline_origin: effective.deadline_origin,
                max_work_units: effective.max_work_units,
                row_limit: effective.row_limit,
                lazy_eligible: false,
                streaming: true,
                parallel: false,
                disabled_passes: None,
                embedder: None,
                value_codecs: None,
                cancel,
                write_scope: None,
                git_sha: None,
                modified_by: None,
                csv_import: CsvImportPolicy::LocalFilesystem,
            };
            if is_mutation {
                let working = tx.working_mut()?;
                Ok(execute_mut(working, query, &opts)?.result)
            } else {
                let graph = tx.current().ok_or_else(|| {
                    KgError::Argument("Transaction already committed or rolled back".to_string())
                })?;
                Ok(execute_read(graph, query, &opts)?.result)
            }
        })?;
        if pre_parsed.explain {
            crate::warning_policy::announce(py, result.diagnostics.as_ref())?;
            return Py::new(py, ResultView::from_cypher_result(result)).map(|v| v.into_any());
        }
        marshal_result(py, result, to_df, output_csv)
    }

    /// Commit the transaction as one log frame; a conflicting commit raises `TransactionConflictError`.
    fn commit(&mut self, py: Python<'_>) -> PyResult<()> {
        let tx = self.inner.take().ok_or_else(finished_error)?;
        let session = self.session.get();
        let durable = session
            .durable
            .as_ref()
            .ok_or_else(|| not_durable_error("begin"))?;
        let (core, handle) = (Arc::clone(&session.inner), Arc::clone(durable));
        let (outcome, warnings) = py.detach(move || {
            let committed = {
                let _writer = match handle.writer() {
                    Ok(guard) => guard,
                    Err(error) => return (Err(*error), Vec::new()),
                };
                let (outcome, warnings) = core.commit_reporting(tx, true);
                let result = match outcome {
                    CommitOutcome::Committed { .. } | CommitOutcome::NoWritesNoOp => Ok(()),
                    CommitOutcome::ConflictDetected {
                        current_version,
                        base_version,
                    } => Err(KgError::TransactionConflict {
                        current_version,
                        base_version,
                    }),
                    CommitOutcome::DurabilityFailed { error } => {
                        Err(KgError::DurabilityFailed { message: error })
                    }
                    CommitOutcome::OntologyViolated { error } => Err(*error),
                    other => Err(KgError::Internal {
                        message: format!("Unexpected Session commit outcome: {other:?}"),
                        location: "SessionTransaction::commit",
                    }),
                };
                (result, warnings)
            };
            handle.kick_checkpoint(&core);
            committed
        });
        outcome.map_err(crate::error_py::kg_to_pyerr)?;
        super::super::warn_all(py, &warnings)
    }

    /// Discard the transaction's changes.
    fn rollback(&mut self) -> PyResult<()> {
        self.inner.take().map(drop).ok_or_else(finished_error)
    }

    /// Whether the transaction has not yet been committed or rolled back.
    #[getter]
    fn active(&self) -> bool {
        self.inner.is_some()
    }

    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Commit on a clean exit, roll back on an exception.
    fn __exit__(
        &mut self,
        py: Python<'_>,
        exc_type: Option<&Bound<'_, PyAny>>,
        _exc_val: Option<&Bound<'_, PyAny>>,
        _exc_tb: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        if self.inner.is_none() {
            return Ok(false);
        }
        if exc_type.is_some() {
            self.inner = None;
        } else {
            self.commit(py)?;
        }
        Ok(false)
    }
}
