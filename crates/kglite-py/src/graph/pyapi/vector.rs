use crate::datatypes::{py_in, py_out};
use petgraph::graph::NodeIndex;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::IntoPyObjectExt;
use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::{get_graph_mut, KnowledgeGraph, NodeKeyGuard, NodeKeyKind};
use kglite_core::api::embeddings::{EmbedError, EmbedHooks, EmbedMode};
use kglite_core::api::io as file;
use kglite_core::api::GraphRead;

/// One-arg `embeddings(text_column)` keys the *selection* by bare id, and a
/// selection can span node types. The two-arg form is the type-namespaced
/// way to read both stores.
const SELECTION_ID_KEY: NodeKeyGuard<'static> = NodeKeyGuard {
    surface: "embeddings()",
    kind: NodeKeyKind::Id,
    recipe: "call the two-arg form embeddings(node_type, text_column) once \
             per type, which keys a single type's id namespace",
};

#[pymethods]
impl KnowledgeGraph {
    /// Store embeddings for nodes of the given type.
    ///
    /// **Replaces** any existing store for ``(node_type, "{text_column}_emb")``.
    /// For incremental ingest where multiple batches must coexist, use
    /// ``add_embeddings()`` instead (it upserts without clobbering — no
    /// read-merge-write needed at the call site).
    ///
    /// Args:
    ///     node_type: The node type (e.g. 'Article')
    ///     text_column: Source column name (e.g. 'summary'). Stored as '{text_column}_emb'.
    ///     embeddings: Dict mapping node IDs to embedding vectors (list of floats)
    ///
    /// Returns:
    ///     dict: {'embeddings_stored': int, 'dimension': int, 'skipped': int}
    #[pyo3(signature = (node_type, text_column, embeddings, metric=None))]
    fn set_embeddings(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        embeddings: &Bound<'_, PyDict>,
        metric: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        self.check_durable_owner()?;
        let entries = marshal_embedding_batch(embeddings)?;
        let g = get_graph_mut(&mut self.inner);
        let report = kglite_core::api::embeddings::set_embeddings(
            g,
            node_type,
            text_column,
            metric,
            entries,
        )
        .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)?;

        self.commit_wal()?;

        let result = PyDict::new(py);
        result.set_item("embeddings_stored", report.embeddings_stored)?;
        result.set_item("dimension", report.dimension)?;
        result.set_item("skipped", report.skipped)?;
        Ok(result.into())
    }

    /// Add or update embeddings for nodes of the given type without
    /// discarding the existing store.
    ///
    /// Differs from ``set_embeddings`` (which replaces the store) by
    /// upserting entries into an existing ``(node_type, "{text_column}_emb")``
    /// store. If no store exists yet, behaves like ``set_embeddings`` —
    /// the first call creates one; subsequent calls extend it.
    ///
    /// Use this for incremental ingest workflows where multiple
    /// ``add_nodes`` + embedding batches need to coexist without a
    /// read-merge-write cycle through the user's process.
    ///
    /// Args:
    ///     node_type: The node type (e.g. 'Article')
    ///     text_column: Source column name (e.g. 'summary'). Stored as '{text_column}_emb'.
    ///     embeddings: Dict mapping node IDs to embedding vectors (list of floats).
    ///
    /// Returns:
    ///     dict: {'embeddings_stored': int, 'dimension': int, 'skipped': int, 'store_created': bool}
    #[pyo3(signature = (node_type, text_column, embeddings, metric=None))]
    fn add_embeddings(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        embeddings: &Bound<'_, PyDict>,
        metric: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        self.check_durable_owner()?;
        let entries = marshal_embedding_batch(embeddings)?;
        let g = get_graph_mut(&mut self.inner);
        let report = kglite_core::api::embeddings::add_embeddings(
            g,
            node_type,
            text_column,
            metric,
            entries,
        )
        .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)?;

        self.commit_wal()?;

        let result = PyDict::new(py);
        result.set_item("embeddings_stored", report.embeddings_stored)?;
        result.set_item("dimension", report.dimension)?;
        result.set_item("skipped", report.skipped)?;
        result.set_item("store_created", report.store_created)?;
        Ok(result.into())
    }

    /// Vector similarity search within the current selection.
    ///
    /// Args:
    ///     text_column: Source column name (e.g. 'summary'). Resolves to '{text_column}_emb'.
    ///     query_vector: The query embedding vector (list of floats)
    ///     top_k: Number of results to return (default 10)
    ///     metric: Distance metric - 'cosine', 'dot_product', 'euclidean', or 'poincare'.
    ///            If omitted, uses the unique metric stored by the selected
    ///            embedding stores, or cosine when none is stored. Selections
    ///            spanning different stored metrics must pass this explicitly.
    ///     to_df: If True, return a pandas DataFrame instead of list of dicts
    ///
    ///     returning: Optional list of fields to project onto each hit. When
    ///            omitted (default), a hit carries ``id``, ``title``, ``type``,
    ///            ``score``, and **all** node properties — so no follow-up join
    ///            is needed to recover them. When given, a hit carries only
    ///            ``id`` + ``score`` plus the named fields (each a property or a
    ///            structural field like ``title``/``type``) — trim the payload
    ///            for ranking-heavy or wide-node workloads.
    ///
    /// Returns:
    ///     List of dicts. By default each has ``id``, ``title``, ``type``,
    ///     ``score``, and all node properties (``score`` always present, every
    ///     metric; properties read live so a hit is identical before/after
    ///     save/reload). With ``returning=[...]`` each has ``id`` + ``score`` +
    ///     the requested fields only.
    ///
    /// Raises:
    ///     ValueError: if **no** selected node type has an embedding store for
    ///         ``text_column`` — a wrong column or an un-embedded type, which
    ///         used to come back as a silent ``[]``. A selection where *some*
    ///         type has the store is a partial result, not an error.
    #[pyo3(signature = (text_column, query_vector, top_k=10, metric=None, to_df=false, returning=None, exact=false))]
    #[allow(clippy::too_many_arguments)]
    fn vector_search(
        &self,
        py: Python<'_>,
        text_column: &str,
        query_vector: Vec<f32>,
        top_k: Option<usize>,
        metric: Option<&str>,
        to_df: Option<bool>,
        returning: Option<Vec<String>>,
        exact: Option<bool>,
    ) -> PyResult<Py<PyAny>> {
        let _arena_guard = self.inner.begin_read_pass(); // disk arena guard (no-op on memory/mapped)
        let top_k = top_k.unwrap_or(10);
        let exact = exact.unwrap_or(false);
        let embedding_property = kglite_core::api::embeddings::store_name(text_column);
        // `id` and `score` are always kept — identity + rank.
        let keep: Option<std::collections::HashSet<String>> =
            returning.map(|v| v.into_iter().collect());
        let want =
            |k: &str| k == "id" || k == "score" || keep.as_ref().is_none_or(|set| set.contains(k));

        let metric = metric
            .map(|name| {
                kglite_core::api::algorithms::DistanceMetric::from_name(name).ok_or_else(|| {
                    PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                        "Unknown metric '{}'. Use 'cosine', 'dot_product', 'euclidean', or 'poincare'.",
                        name
                    ))
                })
            })
            .transpose()?;
        let inner = self.inner.clone();
        let selection = self.cursor.selection.clone();
        let results = py
            .detach(|| {
                let options = kglite_core::api::algorithms::VectorSearchOptions::default()
                    .with_top_k(top_k)
                    .with_exact(exact);
                let options = match metric {
                    Some(metric) => options.with_metric(metric),
                    None => options.with_stored_metric(),
                };
                kglite_core::api::algorithms::vector_search(
                    &inner,
                    &selection,
                    &embedding_property,
                    &query_vector,
                    &options,
                )
            })
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)?;

        if to_df.unwrap_or(false) {
            let records: Vec<Py<PyAny>> = results
                .iter()
                .filter_map(|r| self.inner.graph.node_view(r.node_idx).map(|node| (r, node)))
                .map(|(r, node)| -> PyResult<Py<PyAny>> {
                    let dict = PyDict::new(py);
                    dict.set_item("id", py_out::value_to_py(py, &node.id())?)?;
                    if want("title") {
                        dict.set_item("title", py_out::value_to_py(py, &node.title())?)?;
                    }
                    if want("type") {
                        dict.set_item("type", node.node_type_str(&self.inner.interner))?;
                    }
                    dict.set_item("score", r.score)?;
                    // properties_cloned reads PropertyStorage::Columnar (the
                    // durable shape); property_iter yields nothing for it.
                    for (k, v) in node.properties_cloned(&self.inner.interner) {
                        if want(&k) {
                            dict.set_item(k, py_out::value_to_py(py, &v)?)?;
                        }
                    }
                    Ok(dict.into())
                })
                .collect::<PyResult<_>>()?;
            let py_list = PyList::new(py, &records)?;
            return crate::datatypes::pandas_out::dataframe(py, py_list.as_any(), None, None, None);
        }

        let py_list = PyList::empty(py);
        for r in &results {
            if let Some(node) = self.inner.graph.node_view(r.node_idx) {
                let dict = PyDict::new(py);
                dict.set_item("id", py_out::value_to_py(py, &node.id())?)?;
                if want("title") {
                    dict.set_item("title", py_out::value_to_py(py, &node.title())?)?;
                }
                if want("type") {
                    dict.set_item("type", node.node_type_str(&self.inner.interner))?;
                }
                dict.set_item("score", r.score)?;
                for (k, v) in node.properties_cloned(&self.inner.interner) {
                    if want(&k) {
                        dict.set_item(k, py_out::value_to_py(py, &v)?)?;
                    }
                }
                py_list.append(dict)?;
            }
        }

        py_list.into_py_any(py)
    }

    /// The vector dimension of the `(node_type, text_column)` embedding store,
    /// or ``None`` if no store exists for it.
    ///
    /// A cheap, direct way to detect an embedder/model change without
    /// bookkeeping: compare it against your model's dimension before
    /// `embed_texts`/`add_embeddings` (which reject a mismatch). `text_column`
    /// is the source column name (stored as ``{text_column}_emb``).
    fn embedding_dim(&self, node_type: &str, text_column: &str) -> Option<usize> {
        let key = kglite_core::api::embeddings::store_key(node_type, text_column);
        self.inner.embeddings.get(&key).map(|s| s.dimension)
    }

    /// Provenance for one node or relationship embedding store, or None.
    #[pyo3(signature = (node_type, text_column, *, entity="node"))]
    fn embedding_info(
        &self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        entity: &str,
    ) -> PyResult<Py<PyAny>> {
        use kglite_core::api::embeddings::{embedding_info, EmbeddingEntity};
        let entity =
            EmbeddingEntity::parse(entity).map_err(crate::error_py::ArgumentError::new_err)?;
        let Some(info) = embedding_info(&self.inner, entity, node_type, text_column) else {
            return Ok(py.None());
        };
        let d = PyDict::new(py);
        d.set_item(type_key(info.entity), info.type_name)?;
        d.set_item("text_column", info.text_column)?;
        d.set_item("dimension", info.dimension)?;
        d.set_item("count", info.count)?;
        d.set_item("model", info.model)?;
        d.set_item("metric", info.metric)?;
        d.set_item("hashed", info.hashed)?;
        d.into_py_any(py)
    }

    /// Copy every node and relationship embedding store from `other` into this graph.
    #[pyo3(signature = (other, *, relationship_keys=None))]
    fn copy_embeddings_from(
        &mut self,
        py: Python<'_>,
        other: &Bound<'_, KnowledgeGraph>,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        // Mirror extend()'s safe shape: clone the source Arc first (so a
        // self-copy doesn't double-borrow), then mutate self.
        let src_arc = match other.try_borrow() {
            Ok(o) => Arc::clone(&o.inner),
            Err(_) => Arc::clone(&self.inner),
        };
        self.check_durable_owner()?;
        let keys = relationship_keys.unwrap_or_default();
        let g = crate::graph::get_graph_mut(&mut self.inner);
        let report = g
            .copy_embeddings_with_relationships_from(&src_arc, &keys)
            .map_err(crate::error_py::ArgumentError::new_err)?;
        self.commit_wal()?;
        let d = PyDict::new(py);
        d.set_item("stores_copied", report.stores_copied)?;
        d.set_item("vectors_copied", report.vectors_copied)?;
        d.set_item("vectors_skipped", report.vectors_skipped)?;
        d.set_item("relationship_stores_copied", report.relationships.stores)?;
        d.set_item("relationship_vectors_copied", report.relationships.carried)?;
        d.set_item("relationship_vectors_skipped", report.relationships.skipped)?;
        d.into_py_any(py)
    }

    /// List every embedding store: node stores, then relationship stores.
    fn list_embeddings(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        use kglite_core::api::embeddings::{list_edge_embeddings, list_embeddings};
        let py_list = PyList::empty(py);
        for info in list_embeddings(&self.inner) {
            let dict = PyDict::new(py);
            dict.set_item("entity", "node")?;
            dict.set_item("node_type", info.node_type)?;
            dict.set_item("text_column", info.text_column)?;
            dict.set_item("store_name", info.store_name)?;
            dict.set_item("dimension", info.dimension)?;
            dict.set_item("count", info.count)?;
            dict.set_item("metric", info.metric)?;
            py_list.append(dict)?;
        }
        for info in list_edge_embeddings(&self.inner) {
            let dict = PyDict::new(py);
            dict.set_item("entity", "relationship")?;
            dict.set_item("relationship_type", info.relationship_type)?;
            dict.set_item("text_column", info.text_column)?;
            dict.set_item("store_name", info.store_name)?;
            dict.set_item("dimension", info.dimension)?;
            dict.set_item("count", info.count)?;
            dict.set_item("metric", info.metric)?;
            py_list.append(dict)?;
        }
        py_list.into_py_any(py)
    }

    /// Diagnose embedding coverage per (type, text column) for nodes and relationships.
    #[pyo3(signature = (node_type=None, *, relationship_type=None))]
    fn embedding_diagnostics(
        &self,
        py: Python<'_>,
        node_type: Option<&str>,
        relationship_type: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        use kglite_core::api::embeddings::{embedding_diagnostics, EmbeddingEntity};
        let rows = embedding_diagnostics(&self.inner, node_type, relationship_type)
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)?;
        let py_list = PyList::empty(py);
        for row in rows {
            let (with_key, embedded_key) = match row.entity {
                EmbeddingEntity::Node => ("nodes_with_property", "nodes_embedded"),
                EmbeddingEntity::Relationship => {
                    ("relationships_with_property", "relationships_embedded")
                }
            };
            let dict = PyDict::new(py);
            dict.set_item("entity", row.entity.as_str())?;
            dict.set_item(type_key(row.entity), row.type_name)?;
            dict.set_item("text_column", row.text_column)?;
            dict.set_item("embedding_key", row.embedding_key)?;
            dict.set_item(with_key, row.with_property)?;
            dict.set_item(embedded_key, row.embedded)?;
            dict.set_item("status", row.status.as_str())?;
            dict.set_item("dimension", row.dimension)?;
            dict.set_item("metric", row.metric)?;
            let length_stats = PyDict::new(py);
            length_stats.set_item("mean_length", row.length_stats.mean_length)?;
            length_stats.set_item("max_length", row.length_stats.max_length)?;
            length_stats.set_item("distinct_count", row.length_stats.distinct_count)?;
            length_stats.set_item("distinct_ratio", row.length_stats.distinct_ratio)?;
            dict.set_item("length_stats", length_stats)?;
            py_list.append(dict)?;
        }
        py_list.into_py_any(py)
    }

    /// Remove an embedding store.
    ///
    /// Args:
    ///     node_type: The node type
    ///     text_column: Source column name (e.g. 'summary')
    fn remove_embeddings(&mut self, node_type: &str, text_column: &str) -> PyResult<()> {
        self.check_durable_owner()?;
        get_graph_mut(&mut self.inner).remove_embedding_store(node_type, text_column);
        self.commit_wal()
    }

    /// Export node and relationship embeddings to a standalone .kgle file.
    #[pyo3(signature = (path, node_types=None, *, relationship_keys=None))]
    fn export_embeddings(
        &self,
        py: Python<'_>,
        path: &str,
        node_types: Option<Bound<'_, PyAny>>,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        let filter = match &node_types {
            None => None,
            Some(obj) => {
                if let Ok(list) = obj.cast::<PyList>() {
                    let types: Vec<String> = list.extract()?;
                    Some(file::EmbeddingExportFilter::Types(types))
                } else if let Ok(dict) = obj.cast::<PyDict>() {
                    let mut map: HashMap<String, Vec<String>> = HashMap::new();
                    for (k, v) in dict.iter() {
                        let key: String = k.extract()?;
                        let vals: Vec<String> = v.extract()?;
                        map.insert(key, vals);
                    }
                    Some(file::EmbeddingExportFilter::TypeProperties(map))
                } else {
                    return Err(PyErr::new::<pyo3::exceptions::PyTypeError, _>(
                        "node_types must be a list of strings or a dict of {str: list[str]}",
                    ));
                }
            }
        };

        let inner = self.inner.clone();
        let path_owned = path.to_string();
        let keys = relationship_keys.unwrap_or_default();
        let stats = py
            .detach(move || {
                file::export_embeddings_to_file(&inner, &path_owned, filter.as_ref(), &keys)
            })
            .map_err(embedding_file_error)?;

        let result = PyDict::new(py);
        result.set_item("stores", stats.stores)?;
        result.set_item("embeddings", stats.embeddings)?;
        result.set_item("relationship_stores", stats.relationship_stores)?;
        result.set_item("relationship_embeddings", stats.relationship_embeddings)?;
        result.into_py_any(py)
    }

    /// Import node and relationship embeddings from a .kgle file.
    #[pyo3(signature = (path, *, relationship_keys=None))]
    fn import_embeddings(
        &mut self,
        py: Python<'_>,
        path: &str,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        self.check_durable_owner()?;
        let keys = relationship_keys.unwrap_or_default();
        let g = get_graph_mut(&mut self.inner);
        let stats =
            file::import_embeddings_from_file(g, path, &keys).map_err(embedding_file_error)?;
        self.commit_wal()?;

        // Surface the silent-drop cases as a UserWarning: visible by default,
        // still suppressible via the standard `warnings` module.
        if stats.imported == 0 && stats.skipped > 0 {
            let msg = format!(
                "import_embeddings('{}'): imported 0 embeddings, skipped {} — \
                 no node IDs in the file match the current graph. The file \
                 may have been exported from a different graph, or the node \
                 ID/type schema has changed since export.",
                path, stats.skipped
            );
            let cmsg = std::ffi::CString::new(msg).unwrap_or_default();
            let _ = PyErr::warn(
                py,
                py.get_type::<pyo3::exceptions::PyUserWarning>().as_any(),
                cmsg.as_c_str(),
                1,
            );
        } else if stats.dropped_stores > 0 {
            let msg = format!(
                "import_embeddings('{}'): {} embedding store(s) had zero \
                 matches and were dropped (imported={}, skipped={}, \
                 stores_kept={}). Some types in the file don't exist in \
                 the current graph, or their node IDs don't match.",
                path, stats.dropped_stores, stats.imported, stats.skipped, stats.stores
            );
            let cmsg = std::ffi::CString::new(msg).unwrap_or_default();
            let _ = PyErr::warn(
                py,
                py.get_type::<pyo3::exceptions::PyUserWarning>().as_any(),
                cmsg.as_c_str(),
                1,
            );
        }

        let relationships = &stats.relationships;
        if relationships.carried == 0 && relationships.skipped > 0 {
            let msg = format!(
                "import_embeddings('{}'): imported 0 relationship embeddings, skipped {} — \
                 no relationship in the file connects nodes of this graph by the same \
                 type and endpoint ids.",
                path, relationships.skipped
            );
            let cmsg = std::ffi::CString::new(msg).unwrap_or_default();
            let _ = PyErr::warn(
                py,
                py.get_type::<pyo3::exceptions::PyUserWarning>().as_any(),
                cmsg.as_c_str(),
                1,
            );
        }

        let result = PyDict::new(py);
        result.set_item("stores", stats.stores)?;
        result.set_item("imported", stats.imported)?;
        result.set_item("skipped", stats.skipped)?;
        result.set_item("dropped_stores", stats.dropped_stores)?;
        result.set_item("relationship_stores", relationships.stores)?;
        result.set_item("relationship_imported", relationships.carried)?;
        result.set_item("relationship_skipped", relationships.skipped)?;
        result.set_item("relationship_dropped_stores", relationships.dropped_stores)?;
        result.into_py_any(py)
    }

    /// Retrieve embeddings for nodes.
    ///
    /// Can be called in two ways:
    ///   - ``embeddings(node_type, text_column)`` — returns all embeddings of that type
    ///   - ``embeddings(text_column)`` — returns embeddings for the current selection
    ///
    /// Args:
    ///     text_column: Source column name (e.g. 'summary'). Resolves to '{text_column}_emb'.
    ///
    /// Returns:
    ///     Dict mapping node IDs to embedding vectors (list of floats).
    ///
    /// Raises:
    ///     ArgumentError: The one-arg form's selection spans two node types
    ///         sharing an id. Ids are unique per type only; call the two-arg
    ///         form once per type instead.
    #[pyo3(signature = (node_type_or_text_column, text_column=None))]
    fn embeddings(
        &self,
        py: Python<'_>,
        node_type_or_text_column: &str,
        text_column: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        let _arena_guard = self.inner.begin_read_pass(); // disk arena guard (no-op on memory/mapped)
        let result = PyDict::new(py);

        if let Some(col) = text_column {
            let key = kglite_core::api::embeddings::store_key(node_type_or_text_column, col);
            let store = match self.inner.embeddings.get(&key) {
                Some(s) => s,
                None => return result.into_py_any(py),
            };

            for (&node_index, &_slot) in &store.node_to_slot {
                if let Some(embedding) = store.get_embedding(node_index) {
                    if let Some(node) = self.inner.graph.node_view(NodeIndex::new(node_index)) {
                        let py_id = py_out::value_to_py(py, &node.id())?;
                        let py_vec = PyList::new(py, embedding)?;
                        result.set_item(py_id, py_vec)?;
                    }
                }
            }

            return result.into_py_any(py);
        }

        // One-arg form: embeddings(text_column) — selection-based. A selection
        // that was never narrowed means the whole graph (the never-selected
        // rule get_nodes() applies); one a query emptied stays empty.
        let col = node_type_or_text_column;

        let selection = &self.cursor.selection;
        let level = selection
            .get_level(selection.get_level_count().saturating_sub(1))
            .filter(|level| level.node_count() > 0);
        let nodes: Vec<NodeIndex> = match level {
            Some(level) => level.get_all_nodes(),
            None if selection.never_selected() => {
                GraphRead::node_indices(&self.inner.graph).collect()
            }
            None => Vec::new(),
        };

        for node_idx in &nodes {
            let node = match self.inner.graph.node_view(*node_idx) {
                Some(n) => n,
                None => continue,
            };

            let key = kglite_core::api::embeddings::store_key(
                node.node_type_str(&self.inner.interner),
                col,
            );
            let store = match self.inner.embeddings.get(&key) {
                Some(s) => s,
                None => continue,
            };

            if let Some(embedding) = store.get_embedding(node_idx.index()) {
                let py_id = py_out::value_to_py(py, &node.id())?;
                let py_vec = PyList::new(py, embedding)?;
                // The selection can span types, and ids are only unique
                // within one — two colliding nodes would silently leave one
                // vector out of the dict.
                SELECTION_ID_KEY.insert(&result, py_id.bind(py), py_vec)?;
            }
        }

        result.into_py_any(py)
    }

    /// Retrieve a single node's embedding vector.
    ///
    /// Args:
    ///     node_type: The node type (e.g. 'Article').
    ///     text_column: Source column name (e.g. 'summary').
    ///     node_id: The node ID to look up.
    ///
    /// Returns:
    ///     The embedding vector as a list of floats, or None if not found.
    fn embedding(
        &self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        node_id: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let id = py_in::py_value_to_value(node_id)?;

        let node_idx = match self.inner.lookup_by_id_readonly(node_type, &id) {
            Some(idx) => idx,
            None => return Ok(py.None()),
        };

        let key = kglite_core::api::embeddings::store_key(node_type, text_column);
        let store = match self.inner.embeddings.get(&key) {
            Some(s) => s,
            None => return Ok(py.None()),
        };

        match store.get_embedding(node_idx.index()) {
            Some(embedding) => {
                let py_vec = PyList::new(py, embedding)?;
                py_vec.into_py_any(py)
            }
            None => Ok(py.None()),
        }
    }

    /// Register or unbind an embedding model on the graph.
    ///
    /// Pass a model object to register; pass ``None`` to unbind the
    /// currently-registered embedder.
    ///
    /// The model must have:
    /// - ``dimension: int`` — the embedding vector size
    /// - ``embed(texts: list[str]) -> list[list[float]]`` — batch embedding method
    ///
    /// After registering, ``embed_texts()`` and ``search_text()`` use the
    /// registered model automatically.  The model is **not** serialized —
    /// call ``set_embedder()`` again after ``load()``.
    #[pyo3(signature = (model,))]
    fn set_embedder(&mut self, py: Python<'_>, model: Option<Py<PyAny>>) -> PyResult<()> {
        let Some(model) = model else {
            self.embedder = None;
            return Ok(());
        };
        let bound = model.bind(py);
        bound.getattr("dimension").map_err(|_| {
            PyErr::new::<pyo3::exceptions::PyAttributeError, _>(
                "model must have a 'dimension' attribute (int)",
            )
        })?;
        bound.getattr("embed").map_err(|_| {
            PyErr::new::<pyo3::exceptions::PyAttributeError, _>("model must have an 'embed' method")
        })?;
        let adapter = crate::graph::embedder::py_adapter::PyEmbedderAdapter::new(py, model)?;
        self.embedder = Some(Arc::new(adapter));
        Ok(())
    }

    /// Embed a text column for all nodes of a given type.
    ///
    /// Uses the model registered via ``set_embedder()``.  Reads each node's
    /// ``text_column`` property, calls ``model.embed()`` in batches, and stores
    /// the resulting vectors as ``{text_column}_emb``.  Nodes with missing or
    /// non-string text values are skipped.
    ///
    /// Args:
    ///     node_type: The node type to embed (e.g. ``'Article'``).
    ///     text_column: The column holding the text to embed. Resolves as
    ///         ``set_embeddings`` resolves it — a stored property, an identity
    ///         alias (a ``title_field='name'`` type embeds its titles under
    ///         ``'name'``), the canonical ``id``/``title``, or a structural
    ///         alias. A column that resolves to none of those raises.
    ///     batch_size: Number of texts per ``model.embed()`` call (default 256).
    ///     show_progress: Show a tqdm progress bar (default ``True``).
    ///         Requires ``tqdm`` to be installed; silently falls back to no
    ///         progress bar if it is not available.
    ///     mode: Which nodes to embed —
    ///         ``'missing'`` (default): only nodes without an embedding yet;
    ///         ``'changed'``: nodes missing an embedding *or* whose text changed
    ///         since the last embed (detected via a stored per-node content
    ///         hash) — the incremental re-embed;
    ///         ``'all'``: re-embed every node, rebuilding the store fresh.
    ///
    /// Returns:
    ///     Dict with ``embedded``, ``skipped``, ``skipped_existing``,
    ///     ``reembedded_changed``, and ``dimension``.
    ///
    /// Raises:
    ///     ValueError: if ``node_type`` does not exist in the graph (the same
    ///         complaint ``set_embeddings`` makes — raised before the model is
    ///         loaded), if ``text_column`` resolves to no readable column, or
    ///         if ``mode`` is not one of the three names.
    #[pyo3(signature = (node_type, text_column, batch_size=256, show_progress=true, mode=None))]
    fn embed_texts(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        batch_size: Option<usize>,
        show_progress: Option<bool>,
        mode: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        // Refuse derived durable/CDC handles before loading or invoking the
        // model. `commit_wal()` retains the same guard after mutation as
        // defense in depth, but it cannot undo an already-installed store.
        self.check_durable_owner()?;
        let model = self.get_embedder_or_error()?;
        let mode = match mode.unwrap_or("missing") {
            "missing" => EmbedMode::Missing,
            "changed" => EmbedMode::Changed,
            "all" => EmbedMode::All,
            other => {
                return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                    "embed_texts(mode={other:?}): unknown mode. Use 'missing' (default), \
                     'changed' (re-embed nodes whose text changed), or 'all'."
                )));
            }
        };
        // A type with no nodes stays the `{'embedded': 0}` no-op it has always
        // been; a type the graph has never *seen* is a mistake and gets
        // `set_embeddings`' complaint, before the model is loaded.
        if !self.inner.type_indices.contains_key(node_type) {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "Node type '{}' does not exist in the graph",
                node_type
            )));
        }

        // Resolve tqdm here, before core takes over: an optional module has to
        // be looked up from the method body, not from a hook core drives.
        let progress_factory = resolve_progress_factory(py, show_progress.unwrap_or(true));
        // The bar is opened by `on_start`, which core calls with the count
        // only when there is something to embed — so a no-op pass draws no bar.
        let progress_bar: std::cell::RefCell<Option<Bound<'_, PyAny>>> =
            std::cell::RefCell::new(None);
        let open_bar = |total: usize| {
            *progress_bar.borrow_mut() = open_progress_bar(
                progress_factory.as_ref(),
                total,
                format!("Embedding {}.{}", node_type, text_column),
            );
        };
        let tick = |done: usize| {
            if let Some(bar) = progress_bar.borrow().as_ref() {
                let _ = bar.call_method1("update", (done,));
            }
        };
        // Release the GIL while embedding — PyEmbedderAdapter reacquires
        // inside, fastembed never needs it.
        let embed_batch = |texts: &[String]| py.detach(|| model.embed(texts));
        let hooks = EmbedHooks {
            batch_size: batch_size.unwrap_or(256),
            // `embed_texts` reports a dimension in every return dict, even for
            // a pass with nothing to do, so the model is loaded for it.
            load_when_idle: true,
            embed_batch: Some(&embed_batch),
            on_start: Some(&open_bar),
            on_batch: Some(&tick),
        };
        let outcome = kglite_core::api::embeddings::embed_property(
            &mut self.inner,
            node_type,
            text_column,
            mode,
            model.as_ref(),
            &hooks,
        );
        if let Some(bar) = progress_bar.borrow().as_ref() {
            let _ = bar.call_method0("close");
        }
        let outcome = outcome.map_err(|error| embed_error(error, node_type, text_column))?;
        self.commit_wal()?;
        let result = PyDict::new(py);
        result.set_item("embedded", outcome.embedded)?;
        result.set_item("skipped", outcome.skipped)?;
        result.set_item("skipped_existing", outcome.skipped_existing)?;
        result.set_item("reembedded_changed", outcome.reembedded_changed)?;
        result.set_item("dimension", outcome.dimension)?;
        Ok(result.into())
    }

    /// Search embeddings using a text query.
    ///
    /// Uses the model registered via ``set_embedder()`` to embed the query,
    /// then performs vector search within the current selection.  The user
    /// refers to the text column name (e.g. ``"summary"``); the graph
    /// resolves it to ``"summary_emb"`` internally.
    ///
    /// Args:
    ///     text_column: Text column whose embeddings to search (e.g. ``'summary'``).
    ///     query: The text query to search for.
    ///     top_k: Number of results to return (default 10).
    ///     metric: Distance metric. Omitted uses the same selection-aware stored
    ///         metric resolution as ``vector_search``.
    ///     to_df: If True, return a pandas DataFrame.
    ///
    /// Returns:
    ///     Same format as ``vector_search()`` — list of dicts or DataFrame.
    #[pyo3(signature = (text_column, query, top_k=10, metric=None, to_df=false, returning=None, exact=false))]
    #[allow(clippy::too_many_arguments)]
    fn search_text(
        &self,
        py: Python<'_>,
        text_column: &str,
        query: &str,
        top_k: Option<usize>,
        metric: Option<&str>,
        to_df: Option<bool>,
        returning: Option<Vec<String>>,
        exact: Option<bool>,
    ) -> PyResult<Py<PyAny>> {
        let model = self.get_embedder_or_error()?;
        let model_dimension = model.dimension();

        model
            .load()
            .map_err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>)?;

        // Unload regardless of success or failure — hence the `?` after it.
        let texts = vec![query.to_string()];
        let embed_result = py.detach(|| model.embed(&texts));
        model.unload();
        let embeddings = embed_result.map_err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>)?;

        if embeddings.len() != 1 {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "search_text: model.embed() returned {} vectors for 1 texts",
                embeddings.len()
            )));
        }

        let query_vector = embeddings.into_iter().next().unwrap();
        if query_vector.len() != model_dimension {
            return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "search_text: model.embed() returned vector width {}, expected registered model dimension {model_dimension}",
                query_vector.len()
            )));
        }

        self.vector_search(
            py,
            text_column,
            query_vector,
            top_k,
            metric,
            to_df,
            returning,
            exact,
        )
    }

    /// Build an HNSW approximate-nearest-neighbour index over an embedding store
    /// so subsequent vector searches scale sub-linearly on large stores.
    ///
    /// Opt-in (like ``create_index``): without it, search is an exact brute-force
    /// scan. Once built, ``vector_search`` / ``search_text`` auto-use the index
    /// for queries covering most of a large store; pass ``exact=True`` to force
    /// an exact scan.
    ///
    /// Later vector writes (``add_embeddings`` / ``embed_texts`` /
    /// ``set_embeddings``) do **not** drop the index: they are recorded, and the
    /// next vector query folds them in — while the outstanding delta stays at or
    /// under ``auto_refresh_limit``. A larger delta is served by the exact scan,
    /// which is correct and slower, until you rebuild. Catch-up only ever
    /// indexes vectors that exist; a node with no embedding is reported by
    /// ``SHOW INDEXES`` as ``unembedded`` and is never embedded by a query.
    /// Deleting an embedded node, ``compact()`` and a rolled-back delete still
    /// drop the index outright — each of them moves the slot layout the index
    /// addresses — so rebuild after those.
    ///
    /// The selection does **not** have to be that one node type: as long as
    /// only one type carries ``text_column``, a whole-graph search (or any
    /// selection spanning other types) still uses the index. When two or more
    /// types carry the same column, only a selection of a single one of them
    /// does — a selection spanning both is ranked by exact scan so neither
    /// type's rows can be dropped.
    ///
    /// Args:
    ///     node_type: The node type (e.g. ``'Article'``).
    ///     text_column: Source column name (e.g. ``'summary'``; the store is
    ///         ``'{text_column}_emb'``).
    ///     m: Max neighbours per node on upper layers (default 16). Higher →
    ///         better recall + larger index.
    ///     ef_construction: Build-time search width (default 200). Higher →
    ///         better graph, slower build.
    ///     ef_search: Default query-time search width (default 64). Higher →
    ///         better recall, slower query.
    ///     metric: Distance metric to index for — ``'cosine'`` (default),
    ///         ``'dot_product'``, or ``'euclidean'``. ``'poincare'`` is not
    ///         supported (it stays on the exact path). If omitted, uses the
    ///         store's metric, else ``'cosine'``. An explicit metric becomes
    ///         the store's metric when the store declares none, and is refused
    ///         when it contradicts one the store already declares.
    ///     auto_refresh_limit: How many outstanding vectors a query will fold
    ///         into the index inline before it serves the exact scan instead
    ///         (default 1000). Omit on a rebuild to keep the current value.
    ///
    /// Returns:
    ///     dict: ``{'indexed': int, 'metric': str, 'm': int}`` — vectors indexed.
    ///
    /// Raises:
    ///     ValueError: if the store doesn't exist, the metric is unsupported,
    ///         or the metric contradicts the store's own.
    #[pyo3(signature = (node_type, text_column, m=None, ef_construction=None, ef_search=None, metric=None, auto_refresh_limit=None))]
    #[allow(clippy::too_many_arguments)]
    fn build_vector_index(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        m: Option<usize>,
        ef_construction: Option<usize>,
        ef_search: Option<usize>,
        metric: Option<&str>,
        auto_refresh_limit: Option<usize>,
    ) -> PyResult<Py<PyAny>> {
        self.check_durable_owner()?;
        let g = get_graph_mut(&mut self.inner);
        // Build off the GIL — pure CPU over the contiguous vector buffer.
        let report = py
            .detach(|| {
                kglite_core::api::embeddings::build_vector_index(
                    g,
                    node_type,
                    text_column,
                    m,
                    ef_construction,
                    ef_search,
                    metric,
                    auto_refresh_limit,
                )
            })
            .map_err(PyErr::new::<pyo3::exceptions::PyValueError, _>)?;

        self.commit_wal()?;

        let result = PyDict::new(py);
        result.set_item("indexed", report.indexed)?;
        result.set_item("metric", report.metric)?;
        result.set_item("m", report.m)?;
        Ok(result.into())
    }

    /// Drop the HNSW index for an embedding store (search reverts to exact
    /// brute-force). The vectors are untouched. No-op if no index exists.
    /// Returns ``True`` if one was dropped.
    #[pyo3(signature = (node_type, text_column))]
    fn drop_vector_index(&mut self, node_type: &str, text_column: &str) -> PyResult<bool> {
        self.check_durable_owner()?;
        let dropped = kglite_core::api::embeddings::drop_vector_index(
            get_graph_mut(&mut self.inner),
            node_type,
            text_column,
        );
        self.commit_wal()?;
        Ok(dropped)
    }

    /// Whether an HNSW index is currently built over an embedding store.
    #[pyo3(signature = (node_type, text_column))]
    fn has_vector_index(&self, node_type: &str, text_column: &str) -> PyResult<bool> {
        Ok(kglite_core::api::embeddings::has_vector_index(
            &self.inner,
            node_type,
            text_column,
        ))
    }

    /// Fold every outstanding vector into the HNSW index now, instead of
    /// waiting for a query to do it.
    ///
    /// Returns the number of vectors folded in — ``0`` when the index is
    /// already current, when none is built (catch-up never builds one), or on
    /// a read-only graph. Queries do this on their own while the delta stays
    /// under ``auto_refresh_limit``; call it explicitly to pay the cost at a
    /// moment of your choosing, or to bring an over-limit delta back in one
    /// step without a rebuild.
    #[pyo3(signature = (node_type, text_column))]
    fn refresh_vector_index(&self, node_type: &str, text_column: &str) -> PyResult<usize> {
        Ok(
            kglite_core::api::embeddings::refresh_vector_index(&self.inner, node_type, text_column)
                .unwrap_or(0),
        )
    }
}

/// Marshal a `{id: [floats]}` dict into the `(id, vector)` pairs the engine
/// primitive consumes. Purely a boundary conversion — every validation rule
/// (node type, source column, id resolution, dimension) lives in
/// `kglite::api::embeddings`.
fn marshal_embedding_batch(
    embeddings: &Bound<'_, PyDict>,
) -> PyResult<Vec<(kglite_core::api::Value, Vec<f32>)>> {
    let mut entries = Vec::with_capacity(embeddings.len());
    for (key, value) in embeddings.iter() {
        entries.push((
            py_in::py_value_to_value(&key)?,
            value.extract::<Vec<f32>>()?,
        ));
    }
    Ok(entries)
}

/// One core [`EmbedError`] as the exception class `embed_texts` has always
/// raised for it, with the Python-side remedy spelled out where there is one.
/// Core states the fact; naming `mode='all'` and `remove_embeddings()` is this
/// binding's job, because they are this binding's spellings.
fn embed_error(error: EmbedError, node_type: &str, text_column: &str) -> PyErr {
    match error {
        EmbedError::Dimension { store, model } => {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "embed_texts(): the model produces {model}-d vectors but the existing \
                 '{node_type}.{text_column}_emb' store is {store}-d — embedding the rest would mix \
                 dimensions and corrupt search. Re-embed the whole column with mode='all' to \
                 rebuild at the new dimension, or remove_embeddings('{node_type}', \
                 '{text_column}') first."
            ))
        }
        EmbedError::Column(message) | EmbedError::Output(message) => {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(message)
        }
        EmbedError::Model(message) => PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(message),
    }
}

/// The `tqdm` callable `embed_texts` opens its bar with, resolved once before
/// the pass starts. `None` when the caller opted out, and `None` rather than
/// an error when tqdm is not installed — the documented silent fallback.
///
/// Resolved through `importlib.import_module`, not `py.import`: the latter is
/// CPython's `PyImport_Import`, which calls `__import__` with an *empty*
/// fromlist, and that path discards the resolved submodule and imports the
/// top-level package instead. So `py.import("tqdm.auto")` fails with "No
/// module named 'tqdm'" whenever the parent is unimportable, even though
/// `sys.modules["tqdm.auto"]` already holds the target. `import_module`
/// honours that `sys.modules` entry for the full dotted name.
fn resolve_progress_factory<'py>(
    py: Python<'py>,
    show_progress: bool,
) -> Option<Bound<'py, PyAny>> {
    if !show_progress {
        return None;
    }
    let import_module = py.import("importlib").ok()?.getattr("import_module").ok()?;
    ["tqdm.auto", "tqdm"]
        .iter()
        .find_map(|name| import_module.call1((*name,)).ok())
        .and_then(|tqdm_mod| tqdm_mod.getattr("tqdm").ok())
}

/// One tqdm bar from the factory `resolve_progress_factory` returned, sized to
/// the texts this pass will embed. `None` keeps the silent fallback: no
/// factory, or a factory that refuses the call, simply draws no bar.
fn open_progress_bar<'py>(
    factory: Option<&Bound<'py, PyAny>>,
    total: usize,
    desc: String,
) -> Option<Bound<'py, PyAny>> {
    let factory = factory?;
    let kwargs = PyDict::new(factory.py());
    kwargs.set_item("total", total).ok()?;
    kwargs.set_item("desc", desc).ok()?;
    kwargs.set_item("unit", "text").ok()?;
    factory.call((), Some(&kwargs)).ok()
}

/// The row key that names a store's type: `node_type` or `relationship_type`.
fn type_key(entity: kglite_core::api::embeddings::EmbeddingEntity) -> &'static str {
    match entity {
        kglite_core::api::embeddings::EmbeddingEntity::Node => "node_type",
        kglite_core::api::embeddings::EmbeddingEntity::Relationship => "relationship_type",
    }
}

/// An ambiguous relationship carry is the caller's to resolve (it names a
/// key), so it is an `ArgumentError`; anything else about the file stays an
/// `IOError`.
fn embedding_file_error(error: std::io::Error) -> PyErr {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        crate::error_py::ArgumentError::new_err(error.to_string())
    } else {
        PyErr::new::<pyo3::exceptions::PyIOError, _>(error.to_string())
    }
}
