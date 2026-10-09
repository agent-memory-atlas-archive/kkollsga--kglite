//! `Transaction` `#[pyclass]` + its `#[pymethods]`.

use crate::datatypes::py_in;
use crate::datatypes::values::Value;
use crate::graph::languages::cypher;
use crate::graph::KnowledgeGraph;
use kglite_core::api::session::CsvImportPolicy;
use kglite_core::api::session::Transaction as CoreTransaction;
use kglite_core::api::CowSelection;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::{Bound, IntoPyObjectExt};
use std::collections::HashMap;
use std::sync::Arc;

/// Mutable working copy during a transaction.
///
/// Created by `graph.begin()`, provides a separate `DirGraph` that can be
/// modified without affecting the original. Call `commit()` to apply changes
/// back, or let it drop to discard.
/// The source embedder binding is captured by `begin()` or `begin_read()`.
///
/// ## Isolation semantics
///
/// - **Snapshot isolation**: `begin()` takes an `Arc` snapshot (O(1)) and
///   defers its backend-specific working fork until the first mutation.
///   Memory/mapped modes clone then; disk mode remaps immutable bases and
///   copies only overlays. Read-only cycles pay no fork cost.
/// - **Write isolation**: the first mutation swaps the transaction from
///   snapshot-only mode into its isolated backend working fork.
///   Subsequent mutations all land on the working copy without touching
///   the original graph.
/// - **Commit**: `commit()` of a no-write transaction is a no-op (no
///   version bump, no Arc swap). `commit()` of a tx that did mutate
///   replaces the owner's `Arc<DirGraph>` with the working copy via an
///   atomic pointer swap.
/// - **No concurrent-transaction guarantees**: if two transactions are
///   created from the same graph, each gets an independent snapshot.
///   The first commit wins; the second raises a `Transaction conflict`
///   error via optimistic concurrency control (version check).
/// - **No read-snapshot across transactions**: reads on the original graph
///   while a transaction is open will see the pre-transaction state. After
///   commit, they see the post-transaction state.
///
/// ## State transitions
///
/// The snapshot/working/CoW/OCC state machine is delegated to the engine's
/// [`CoreTransaction`] (`kglite_core::graph::session::Transaction`) — the same
/// type the bolt-server drives — so deferred forking, materialisation,
/// and version tracking live in one place. This wrapper adds only the
/// binding-specific concerns: the owning `KnowledgeGraph` (so `commit()`
/// swaps *its* `Arc`), the optional transaction-level deadline, and the
/// Python result marshalling.
///   - **Deferred** (initial): `CoreTransaction::current()` reads the Arc
///     snapshot; no clone cost.
///   - **Materialized** (after first mutation): `CoreTransaction::working_mut()`
///     materialises the working copy (in place if uniquely held, else a deep
///     clone). All reads + writes run against it.
///
/// `inner` is `None` after `commit()` / `rollback()` — any further use errors.
#[pyclass(module = "kglite")]
pub struct Transaction {
    pub(crate) defaults: super::query_defaults::QueryDefaults,
    pub(crate) ownership_epoch: u64,
    /// Execution binding captured when begin/begin_read takes its snapshot.
    pub(crate) embedder: Option<Arc<dyn crate::graph::embedder::Embedder>>,
    /// Back-reference to the owning KnowledgeGraph (for commit).
    pub(crate) owner: Py<KnowledgeGraph>,
    /// The engine transaction holding the snapshot/working/CoW/OCC state.
    /// `None` once the transaction has been committed or rolled back.
    pub(crate) inner: Option<CoreTransaction>,
    /// Optional transaction-level deadline — all operations fail after this instant.
    pub(crate) deadline: Option<std::time::Instant>,
    /// When `begin()` resolved `deadline`: a statement stopped by it reports
    /// the transaction's configured limit, measured from here.
    pub(crate) deadline_origin: Option<std::time::Instant>,
}

#[pymethods]
impl Transaction {
    /// Whether this is a read-only transaction.
    #[getter]
    fn is_read_only(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(CoreTransaction::is_read_only)
    }

    /// Execute a Cypher query — read **or write** — within this transaction.
    ///
    /// Mutations (CREATE, MERGE, SET, REMOVE, DELETE, DETACH DELETE, FOREACH,
    /// and schema DDL) are applied to the transaction's working copy, not the
    /// original graph, and become visible to other readers only at `commit()`;
    /// `rollback()` discards them. Read queries also operate on the working
    /// copy (seeing uncommitted changes).
    /// `text_score()` uses the captured embedder in reads and mutations.
    ///
    /// Args:
    ///     query: A Cypher query string.
    ///     params: Optional dict of query parameters.
    ///     to_df: If True, return a pandas DataFrame instead of list of dicts.
    ///     timeout_ms: Per-call deadline in milliseconds.
    ///     max_work_units: Work budget for the query, not a result-row cap;
    ///         exceeding it is an error.
    ///     write_scope: Role-scoped write whitelist — every node write is
    ///         judged by the node's *stored* type (a pattern label cannot
    ///         widen it), and a relationship write needs at least one
    ///         endpoint's stored type in the list. `None` (default) =
    ///         unrestricted; `[]` denies every mutation. See
    ///         `KnowledgeGraph.cypher` for the exact perimeter.
    ///     row_limit: Cap on the result rows kept. The query still runs in
    ///         full; only retention stops at the cap, and truncation warns
    ///         and reports the exact pre-truncation `total_rows`.
    ///     git_sha, modified_by: Freshness provenance stamped alongside
    ///         `updated_at` on types that declare `auto_timestamp`.
    ///     valid_at: Run the query under `FOR VALID_TIME AS OF` this instant.
    ///
    /// Returns:
    ///     Query results (same format as KnowledgeGraph.cypher).
    // Python boundary mirrors the public query option surface.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (query, params=None, to_df=false, timeout_ms=None, max_work_units=None, row_limit=None, write_scope=None, git_sha=None, modified_by=None, valid_at=None))]
    fn cypher(
        &mut self,
        py: Python<'_>,
        query: &str,
        params: Option<&Bound<'_, PyDict>>,
        to_df: bool,
        timeout_ms: Option<u64>,
        max_work_units: Option<usize>,
        row_limit: Option<usize>,
        write_scope: Option<Vec<String>>,
        git_sha: Option<String>,
        modified_by: Option<String>,
        valid_at: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Py<PyAny>> {
        let query = crate::graph::valid_time::prefixed_query(query, valid_at)?;
        let query = query.as_ref();
        let write_scope_set: Option<std::collections::HashSet<String>> =
            write_scope.map(|v| v.into_iter().collect());
        // Check transaction-level deadline first
        if let (Some(tx_deadline), Some(origin)) = (self.deadline, self.deadline_origin) {
            if std::time::Instant::now() >= tx_deadline {
                // Typed exception, not the built-in PyTimeoutError.
                return Err(crate::error_py::kg_to_pyerr(
                    crate::error::KgError::CypherTimeout {
                        elapsed_ms: origin.elapsed().as_millis() as u64,
                        limit_ms: tx_deadline.saturating_duration_since(origin).as_millis() as u64,
                        message: "Transaction deadline expired before the statement ran. \
                                  Begin a new transaction, or raise timeout_ms on begin()."
                            .to_string(),
                    },
                ));
            }
        }

        // Merge per-query timeout with transaction deadline (use the earlier one).
        // timeout_ms == 0 is the documented escape hatch: "no per-query deadline"
        // (the transaction-level deadline still applies if set).
        let effective = self.defaults.resolve(timeout_ms, max_work_units, row_limit);
        let max_work_units = effective.max_work_units;
        let row_limit = effective.row_limit;
        // The earlier deadline wins, and brings its own origin so a timeout
        // reports that deadline's configured limit.
        let transaction_deadline = self.deadline.map(|dl| (dl, self.deadline_origin));
        let query_deadline = effective.deadline.map(|dl| (dl, effective.deadline_origin));
        let (deadline, deadline_origin) = match (transaction_deadline, query_deadline) {
            (Some(tx), Some(query)) if tx.0 <= query.0 => (Some(tx.0), tx.1),
            (_, Some(query)) => (Some(query.0), query.1),
            (Some(tx), None) => (Some(tx.0), tx.1),
            (None, None) => (None, None),
        };

        // Convert params
        let param_map: HashMap<String, Value> = match params {
            Some(d) => {
                let mut map = HashMap::new();
                for (k, v) in d.iter() {
                    let key: String = k.extract()?;
                    let val = py_in::py_query_parameter_to_value(&key, &v)?;
                    map.insert(key, val);
                }
                map
            }
            None => HashMap::new(),
        };

        // Both the execution pipeline and the snapshot/working state
        // machine live in core: the engine `CoreTransaction` holds the
        // snapshot/working copy and `session::execute_*` runs
        // parse+validate+optimize+execute. This wrapper only routes and
        // marshals.
        //
        // Decision routing:
        //   - is_mutation + read_only → reject (begin()-appropriate message)
        //   - is_mutation + RW → tx.working_mut() (materialize), execute_mut
        //   - read → execute_read against tx.current() (working or snapshot)
        //
        // The pre-parse below is on the cached parser (~700ns hit)
        // so session::execute's own parse inside is free.
        let pre_parsed = cypher::parse_cypher(query).map_err(crate::error_py::kg_to_pyerr)?;
        let is_mut = cypher::is_mutation_query(&pre_parsed);

        // The engine transaction owns the snapshot/working state; `None` means
        // the tx was already committed or rolled back.
        let tx = self.inner.as_mut().ok_or_else(|| -> PyErr {
            crate::error_py::kg_to_pyerr(crate::error::KgError::Argument(
                "Transaction already committed or rolled back".to_string(),
            ))
        })?;

        // Reject mutations on a read-only tx with a begin()-appropriate message
        // (core's working_mut() message references Session::begin, the wrong
        // entry point for a wheel `graph.begin_read()` caller).
        if is_mut && tx.is_read_only() {
            return Err(crate::error_py::kg_to_pyerr(
                crate::error::KgError::read_only(
                    "Read-only transaction does not support mutations \
                 (CREATE, SET, DELETE, REMOVE, MERGE). Use begin() for read-write.",
                ),
            ));
        }

        let output_csv = pre_parsed.output_format == cypher::OutputFormat::Csv;
        let opts = kglite_core::api::session::ExecuteOptions {
            params: &param_map,
            deadline,
            deadline_origin,
            max_work_units,
            row_limit,
            // No lazy materializer is wired through the tx ResultView, so
            // rows are materialized; the streaming aggregate pipeline needs
            // none.
            lazy_eligible: false,
            streaming: true,
            parallel: false,
            disabled_passes: None,
            embedder: self.embedder.clone(),
            value_codecs: None,
            // This wrapper does not install a SIGINT cancellation flag.
            // Deadline and work-budget failures still restore the shared
            // statement checkpoint, including an existing working fork.
            cancel: None,
            write_scope: write_scope_set.as_ref(),
            git_sha: git_sha.as_deref(),
            modified_by: modified_by.as_deref(),
            csv_import: CsvImportPolicy::LocalFilesystem,
        };

        // A Python-backed embedder needs to reacquire the GIL; keep the
        // existing no-service execution path and cancellation policy intact.
        let mut execute = || -> Result<cypher::CypherResult, Box<crate::error::KgError>> {
            if is_mut {
                let working = tx.working_mut().map_err(Box::new)?;
                Ok(
                    kglite_core::api::session::execute_mut(working, query, &opts)
                        .map_err(Box::new)?
                        .result,
                )
            } else {
                let graph = tx.current().ok_or_else(|| {
                    Box::new(crate::error::KgError::Argument(
                        "Transaction already committed or rolled back".to_string(),
                    ))
                })?;
                Ok(kglite_core::api::session::execute_read(graph, query, &opts)
                    .map_err(Box::new)?
                    .result)
            }
        };
        let result = if opts.embedder.is_some() {
            py.detach(execute)
        } else {
            execute()
        }
        .map_err(|error| crate::error_py::kg_to_pyerr(*error))?;

        crate::warning_policy::announce(py, result.diagnostics.as_ref())?;
        if pre_parsed.explain {
            let view = crate::graph::pyapi::result_view::ResultView::from_cypher_result(result);
            return Py::new(py, view).map(|v| v.into_any());
        }
        if output_csv {
            result.to_csv().into_py_any(py)
        } else if to_df {
            cypher::py_convert::rows_to_dataframe(py, &result.columns, &result.rows)
        } else {
            let view = crate::graph::pyapi::result_view::ResultView::from_cypher_result(result);
            Py::new(py, view).map(|v| v.into_any())
        }
    }

    /// Commit the transaction — apply all changes to the original graph.
    ///
    /// For read-only transactions, this is a no-op.
    /// For a read-write transaction that performed no mutations (deferred
    /// state never materialized), this is also a no-op — no version bump,
    /// no Arc swap, no OCC check needed.
    /// After commit, the transaction cannot be used again.
    fn commit(&mut self) -> PyResult<()> {
        let tx = self.inner.take().ok_or_else(|| -> PyErr {
            crate::error_py::kg_to_pyerr(crate::error::KgError::Argument(
                "Transaction already committed or rolled back".to_string(),
            ))
        })?;

        // `take_working()` yields the working copy (Some only if a mutation
        // materialised it) plus the version captured at begin(). No working
        // copy → read-only or deferred-never-materialised → no-op commit.
        let (working, base_version) = tx.take_working();
        let Some(mut working) = working else {
            return Ok(());
        };

        // The rules that demand something be present are judged on the
        // transaction's stored end state, not statement by statement. A
        // refusal drops the working copy: nothing is published.
        let ontology_warnings = working
            .judge_transaction_end()
            .map_err(crate::error_py::kg_to_pyerr)?;

        // Optimistic concurrency control: the owner graph must not have moved
        // since begin(). (The OCC check stays here because the commit target
        // is the owner KnowledgeGraph's Arc, not a core Session's.)
        Python::attach(|py| {
            let mut kg = self
                .owner
                .try_borrow_mut(py)
                .map_err(|_| super::kg_core::concurrent_access_pyerr())?;
            if kg.lifecycle.epoch() != self.ownership_epoch {
                return Err(crate::error_py::kg_to_pyerr(
                    crate::error::KgError::Argument(
                        "Transaction cannot commit: its graph's persistence ownership has ended"
                            .to_string(),
                    ),
                ));
            }
            kg.check_durable_owner()?;
            let current_version = kg.inner.version;
            if current_version != base_version {
                return Err(crate::error_py::kg_to_pyerr(
                    crate::error::KgError::TransactionConflict {
                        base_version,
                        current_version,
                    },
                ));
            }
            working.set_version(current_version + 1);
            kg.inner = Arc::new(working);
            kg.cursor.selection = CowSelection::new();
            // Durability: one WAL frame for the whole transaction, appended
            // here and nowhere earlier. `Transaction::cypher` must not log —
            // its writes are uncommitted and `rollback()` must leave no trace
            // of them. `take_working` *moves* the working copy, so every op
            // buffered during the transaction arrives on `kg.inner` intact and
            // this single flush emits them together, which is exactly the
            // atomicity the caller asked for.
            kg.commit_wal()?;
            super::super::warn_all(py, &ontology_warnings)
        })?;
        Ok(())
    }

    /// Roll back the transaction — discard all changes.
    ///
    /// After rollback, the transaction cannot be used again.
    fn rollback(&mut self) -> PyResult<()> {
        // Dropping the engine transaction discards its working copy / snapshot.
        if self.inner.take().is_none() {
            return Err(crate::error_py::kg_to_pyerr(
                crate::error::KgError::Argument(
                    "Transaction already committed or rolled back".to_string(),
                ),
            ));
        }
        Ok(())
    }

    /// Context manager entry — returns self.
    fn __enter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    /// Context manager exit — commits on success, rolls back on exception.
    fn __exit__(
        &mut self,
        exc_type: Option<&Bound<'_, pyo3::types::PyAny>>,
        _exc_val: Option<&Bound<'_, pyo3::types::PyAny>>,
        _exc_tb: Option<&Bound<'_, pyo3::types::PyAny>>,
    ) -> PyResult<bool> {
        // A transaction is active while it still holds engine state.
        if self.inner.is_none() {
            // Already committed or rolled back
            return Ok(false);
        }

        if exc_type.is_some() {
            // Exception occurred — rollback (drop the engine transaction).
            self.inner = None;
        } else {
            // No exception — commit
            self.commit()?;
        }

        // Return false = don't suppress exception
        Ok(false)
    }
}
