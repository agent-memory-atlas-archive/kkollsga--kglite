//! `FrozenGraph` — an immutable, concurrently-readable snapshot.
//!
//! `KnowledgeGraph.freeze()` returns one of these. It shares the source
//! graph's `Arc<DirGraph>` (an O(1) clone — no deep copy), exposes *only*
//! read methods, and never takes an exclusive borrow. Because it has no
//! mutating method, no `borrow_mut` can ever fire — so any number of
//! threads can call `cypher()` on the *same* frozen handle concurrently
//! without tripping the single-owner borrow guard that a live
//! `KnowledgeGraph` enforces.
//!
//! Copy-on-write makes the snapshot stable: if the source graph is later
//! mutated, `Arc::make_mut` clones it, leaving this frozen view pointing
//! at the original bytes. That is the "build → freeze → share → swap"
//! model — build a fresh graph cheaply, `freeze()` it, hand it to readers,
//! and atomically swap in a new frozen snapshot when the data changes.
//!
//! `freeze(valid_at=…)` / `Session.snapshot(valid_at=…)` give the handle a
//! core `ValidTimeView`: every `cypher()` runs behind the view's
//! `FOR VALID_TIME AS OF` prefix on the same shared graph, with the view's
//! masks pinned for the handle's lifetime.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::IntoPyObjectExt;
use std::sync::Arc;

use super::query_defaults::QueryDefaults;
use crate::datatypes::py_in;
use crate::graph::languages::cypher;
use crate::graph::pyapi::result_view::ResultView;
use crate::graph::DirGraph;
use crate::util::EnterKg;
use kglite_core::api::session::CsvImportPolicy;
use kglite_core::api::session::{execute_read, ExecuteOptions};
use kglite_core::api::temporal::{view_at, ValidTimeView};
use kglite_core::api::GraphRead;

/// Immutable, `Send`-able read snapshot of a graph. See module docs.
#[pyclass(module = "kglite", frozen)]
pub struct FrozenGraph {
    defaults: QueryDefaults,
    pub(crate) inner: Arc<DirGraph>,
    pub(crate) embedder: Option<Arc<dyn crate::graph::embedder::Embedder>>,
    /// Set by `valid_at=`: the graph as of that instant.
    view: Option<Arc<ValidTimeView>>,
}

impl FrozenGraph {
    pub(crate) fn with_defaults(
        inner: Arc<DirGraph>,
        embedder: Option<Arc<dyn crate::graph::embedder::Embedder>>,
        defaults: QueryDefaults,
    ) -> Self {
        FrozenGraph {
            inner,
            embedder,
            defaults,
            view: None,
        }
    }

    /// [`Self::with_defaults`], as of `valid_at` when one is given. The view
    /// resolves (and may build the endpoint indexes) outside the GIL.
    // The detached closure preserves the engine's structured KgError until PyErr conversion.
    #[allow(clippy::result_large_err)]
    pub(crate) fn as_of(
        py: Python<'_>,
        inner: Arc<DirGraph>,
        embedder: Option<Arc<dyn crate::graph::embedder::Embedder>>,
        defaults: QueryDefaults,
        valid_at: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let mut frozen = Self::with_defaults(inner, embedder, defaults);
        if let Some(instant) = valid_at {
            let value = py_in::py_query_parameter_to_value("valid_at", instant)?;
            let base = Arc::clone(&frozen.inner);
            let view = py
                .detach(|| view_at(base, &value))
                .map_err(|err| pyo3::exceptions::PyValueError::new_err(err.to_string()))?;
            frozen.view = Some(Arc::new(view));
        }
        Ok(frozen)
    }
}

#[pymethods]
impl FrozenGraph {
    /// Run a **read-only** Cypher query against the snapshot.
    ///
    /// Identical semantics to `KnowledgeGraph.cypher` for reads —
    /// `MATCH` / `WHERE` / `RETURN` / aggregations, and semantic search via
    /// `text_score()` / `vector_score()`. A mutation query
    /// (`CREATE` / `SET` / `DELETE` / `REMOVE` / `MERGE`) is rejected: a
    /// frozen snapshot is immutable — mutate the source `KnowledgeGraph`,
    /// then take a fresh `freeze()`.
    ///
    /// Safe to call from many threads on the same `FrozenGraph` at once.
    #[pyo3(signature = (query, to_df=false, params=None, timeout_ms=None, max_work_units=None, row_limit=None))]
    // The detached closure preserves the engine's structured KgError until PyErr conversion.
    #[allow(clippy::result_large_err)]
    // The Python boundary mirrors the public query-option surface.
    #[allow(clippy::too_many_arguments)]
    fn cypher(
        &self,
        py: Python<'_>,
        query: &str,
        to_df: bool,
        params: Option<&Bound<'_, PyDict>>,
        timeout_ms: Option<u64>,
        max_work_units: Option<usize>,
        row_limit: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        let prefixed = match &self.view {
            Some(view) => Some(
                view.cypher_text(query)
                    .map_err(|err| pyo3::exceptions::PyValueError::new_err(err.to_string()))?,
            ),
            None => None,
        };
        let query = prefixed.as_deref().unwrap_or(query);
        // Reject mutations up front with a frozen-specific message (clearer
        // than execute_read's generic "use execute_mut").
        let pre_parsed = cypher::parse_cypher(query).map_err(crate::error_py::kg_to_pyerr)?;
        if cypher::is_mutation_query(&pre_parsed) {
            return Err(crate::error_py::kg_to_pyerr(
                crate::error::KgError::Argument(
                    "FrozenGraph is an immutable snapshot — CREATE/SET/DELETE/REMOVE/MERGE are \
                     not allowed. Mutate the source KnowledgeGraph, then take a fresh freeze()."
                        .to_string(),
                ),
            ));
        }

        // Decode params (PyDict → HashMap) under the GIL, before detaching.
        let param_map = if let Some(params_dict) = params {
            let mut map = std::collections::HashMap::new();
            for (key, val) in params_dict.iter() {
                let key_str: String = key.extract()?;
                let value = py_in::py_query_parameter_to_value(&key_str, &val)?;
                map.insert(key_str, value);
            }
            map
        } else {
            std::collections::HashMap::new()
        };
        let effective = self.defaults.resolve(timeout_ms, max_work_units, row_limit);
        let deadline = effective.deadline;
        let max_work_units = effective.max_work_units;
        let row_limit = effective.row_limit;

        let inner = Arc::clone(&self.inner);
        let embedder = self.embedder.clone();
        let query_owned = query.to_string();
        // GIL-free execution — the whole point of a frozen snapshot is that
        // many readers run in parallel against the shared, immutable graph.
        let result = py.enter_kg(
            move |cancel| -> Result<cypher::CypherResult, crate::error::KgError> {
                let opts = ExecuteOptions {
                    params: &param_map,
                    deadline,
                    max_work_units,
                    row_limit,
                    lazy_eligible: false,
                    streaming: true,
                    parallel: false,
                    disabled_passes: None,
                    embedder,
                    value_codecs: None,
                    cancel,
                    // FrozenGraph is read-only — write-scope + provenance never apply.
                    write_scope: None,
                    git_sha: None,
                    modified_by: None,
                    csv_import: CsvImportPolicy::LocalFilesystem,
                };
                let outcome = execute_read(&inner, &query_owned, &opts)?;
                Ok(outcome.result)
            },
        )?;

        crate::warning_policy::announce(py, result.diagnostics.as_ref())?;
        if pre_parsed.output_format == cypher::OutputFormat::Csv {
            return result.to_csv().into_py_any(py);
        }
        if to_df {
            cypher::py_convert::rows_to_dataframe(py, &result.columns, &result.rows)
        } else {
            let view = ResultView::from_cypher_result(result);
            Py::new(py, view).map(|v| v.into_any())
        }
    }

    /// Number of nodes in the snapshot; on a valid_at handle, those visible at its instant.
    fn node_count(&self, py: Python<'_>) -> PyResult<usize> {
        match &self.view {
            Some(view) => {
                let view = Arc::clone(view);
                py.detach(move || view.node_count().map_err(Box::new))
                    .map_err(|err| crate::error_py::kg_to_pyerr(*err))
            }
            None => Ok(self.inner.graph.node_count()),
        }
    }

    /// Node type names in the snapshot; on a valid_at handle, those with a node visible at its instant.
    #[getter]
    fn node_types(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        match &self.view {
            Some(view) => {
                let view = Arc::clone(view);
                py.detach(move || view.node_types().map_err(Box::new))
                    .map_err(|err| crate::error_py::kg_to_pyerr(*err))
            }
            None => Ok(self.inner.get_node_types()),
        }
    }

    /// The view's materialised valid slice as a new KnowledgeGraph; a test hook for the tier-agreement oracle.
    // The detached closure preserves the engine's structured KgError until PyErr conversion.
    #[allow(clippy::result_large_err)]
    fn _valid_time_slice(&self, py: Python<'_>) -> PyResult<crate::graph::KnowledgeGraph> {
        let view = self.view.as_ref().ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err("this FrozenGraph has no valid_at")
        })?;
        let view = Arc::clone(view);
        let slice = py
            .detach(move || view.slice())
            .map_err(crate::error_py::kg_to_pyerr)?;
        Ok(crate::graph::KnowledgeGraph::from_arc(Arc::clone(
            slice.graph(),
        )))
    }

    fn __repr__(&self) -> String {
        match &self.view {
            // Visible counts may walk the snapshot, too much for a repr.
            Some(view) => format!(
                "FrozenGraph(valid_at={}, snapshot_nodes={})",
                view.as_of(),
                self.inner.graph.node_count()
            ),
            None => format!(
                "FrozenGraph(nodes={}, types={})",
                self.inner.graph.node_count(),
                self.inner.get_node_types().len()
            ),
        }
    }
}
