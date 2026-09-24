//! Relationship-store readers — rank (`relationship_vector_search`,
//! `relationship_search_text`), read one vector (`relationship_embedding`),
//! report a dimension, remove a store — over `kglite::api::embeddings`.

use std::collections::HashMap;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::IntoPyObjectExt;

use super::relationship_vectors::tuple_row;
use crate::datatypes::py_out;
use crate::graph::{get_graph_mut, KnowledgeGraph};
use kglite_core::api::embeddings::{RelationshipSearchHit, RelationshipSearchOptions};

/// `types=` as a list of names: a single string, a list of strings, or None.
fn type_list(types: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Vec<String>>> {
    let Some(types) = types.filter(|value| !value.is_none()) else {
        return Ok(None);
    };
    if let Ok(single) = types.extract::<String>() {
        return Ok(Some(vec![single]));
    }
    types.extract::<Vec<String>>().map(Some).map_err(|_| {
        PyTypeError::new_err("types must be a relationship type name or a list of them")
    })
}

#[pymethods]
impl KnowledgeGraph {
    /// Rank relationship vectors in a text column's stores against a query vector, merged into one top-k.
    #[pyo3(signature = (text_column, query_vector, top_k=10, metric=None, to_df=false, *, types=None, exact=false, relationship_keys=None))]
    // One Rust argument per Python keyword, as node_vector_search has.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn relationship_vector_search(
        &self,
        py: Python<'_>,
        text_column: &str,
        query_vector: Vec<f32>,
        top_k: usize,
        metric: Option<&str>,
        to_df: bool,
        types: Option<&Bound<'_, PyAny>>,
        exact: bool,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        let types = type_list(types)?;
        let keys = relationship_keys.unwrap_or_default();
        let options = RelationshipSearchOptions::new(top_k)
            .with_exact(exact)
            .with_metric(metric);
        let inner = self.inner.clone();
        let hits = py
            .detach(|| {
                kglite_core::api::embeddings::search_relationship_embeddings(
                    &inner,
                    types.as_deref(),
                    text_column,
                    &query_vector,
                    &options,
                    &keys,
                )
            })
            .map_err(PyValueError::new_err)?;
        let rows = PyList::empty(py);
        for hit in &hits {
            rows.append(hit_row(py, hit)?)?;
        }
        if to_df {
            return crate::datatypes::pandas_out::dataframe(py, rows.as_any(), None, None, None);
        }
        rows.into_py_any(py)
    }

    /// Embed a query with the registered model and rank relationship vectors in a text column's stores against it.
    #[pyo3(signature = (text_column, query, top_k=10, metric=None, to_df=false, *, types=None, exact=false, relationship_keys=None))]
    // One Rust argument per Python keyword, as node_search_text has.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn relationship_search_text(
        &self,
        py: Python<'_>,
        text_column: &str,
        query: &str,
        top_k: usize,
        metric: Option<&str>,
        to_df: bool,
        types: Option<&Bound<'_, PyAny>>,
        exact: bool,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        let query_vector = self.embed_query(py, query)?;
        self.relationship_vector_search(
            py,
            text_column,
            query_vector,
            top_k,
            metric,
            to_df,
            types,
            exact,
            relationship_keys,
        )
    }

    /// One relationship's stored vector by endpoint address, or None when it has none; refuses a store that does not exist.
    #[pyo3(signature = (relationship_type, text_column, address, *, relationship_keys=None))]
    pub(super) fn relationship_embedding(
        &self,
        py: Python<'_>,
        relationship_type: &str,
        text_column: &str,
        address: &Bound<'_, PyAny>,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        let address = tuple_row(address, Vec::new(), "relationship_embedding", "address")?;
        let keys = relationship_keys.unwrap_or_default();
        let vector = kglite_core::api::embeddings::relationship_embedding(
            &self.inner,
            relationship_type,
            text_column,
            &address,
            &keys,
        )
        .map_err(PyValueError::new_err)?;
        match vector {
            Some(vector) => PyList::new(py, vector)?.into_py_any(py),
            None => Ok(py.None()),
        }
    }

    /// The vector dimension of a relationship type's embedding store for a text column, or None when there is no such store.
    pub(super) fn relationship_embedding_dim(
        &self,
        relationship_type: &str,
        text_column: &str,
    ) -> Option<usize> {
        kglite_core::api::embeddings::relationship_embedding_dim(
            &self.inner,
            relationship_type,
            text_column,
        )
    }

    /// Remove a relationship type's embedding store for a text column; refuses a store that does not exist.
    pub(super) fn remove_relationship_embeddings(
        &mut self,
        relationship_type: &str,
        text_column: &str,
    ) -> PyResult<()> {
        self.check_durable_owner()?;
        kglite_core::api::embeddings::remove_relationship_embeddings(
            get_graph_mut(&mut self.inner),
            relationship_type,
            text_column,
        )
        .map_err(PyValueError::new_err)?;
        self.commit_wal()
    }
}

/// A hit as a `relationship_embeddings()` row without the vector, plus the
/// relationship type it came from and its score.
fn hit_row<'py>(py: Python<'py>, hit: &RelationshipSearchHit) -> PyResult<Bound<'py, PyDict>> {
    let row = PyDict::new(py);
    row.set_item("source", py_out::value_to_py(py, &hit.source_id)?)?;
    row.set_item("target", py_out::value_to_py(py, &hit.target_id)?)?;
    row.set_item("source_type", &hit.source_type)?;
    row.set_item("target_type", &hit.target_type)?;
    match &hit.key {
        Some(key) => row.set_item("key", py_out::value_to_py(py, key)?)?,
        None => row.set_item("key", py.None())?,
    }
    row.set_item("relationship_type", &hit.relationship_type)?;
    row.set_item("score", hit.score)?;
    Ok(row)
}
