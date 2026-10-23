//! The `connection`-named loader and fluent methods: permanent pointers to
//! their `relationship`-named twins (user decision 2026-09-24). A pointer
//! carries no logic — it passes every argument to its twin unchanged.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::graph::KnowledgeGraph;

#[pymethods]
impl KnowledgeGraph {
    /// Pointer to add_relationships(), the primary spelling; a connection is a relationship.
    #[pyo3(signature = (data, connection_type, source_type, source_id_field, target_type, target_id_field, source_title_field=None, target_title_field=None, columns=None, skip_columns=None, conflict_handling=None, column_types=None, query=None, extra_properties=None, git_sha=None, modified_by=None, on_invalid="error", convention=None, empty_when=None, distinct=false))]
    // The loader arguments of add_relationships, passed through unchanged.
    #[allow(clippy::too_many_arguments)]
    fn add_connections(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        data: Option<&Bound<'_, PyAny>>,
        connection_type: String,
        source_type: String,
        source_id_field: String,
        target_type: String,
        target_id_field: String,
        source_title_field: Option<String>,
        target_title_field: Option<String>,
        columns: Option<&Bound<'_, PyList>>,
        skip_columns: Option<&Bound<'_, PyList>>,
        conflict_handling: Option<String>,
        column_types: Option<&Bound<'_, PyDict>>,
        query: Option<String>,
        extra_properties: Option<&Bound<'_, PyDict>>,
        git_sha: Option<String>,
        modified_by: Option<String>,
        on_invalid: &str,
        convention: Option<&str>,
        empty_when: Option<&str>,
        distinct: bool,
    ) -> PyResult<Py<PyAny>> {
        Self::add_relationships(
            slf,
            py,
            data,
            connection_type,
            source_type,
            source_id_field,
            target_type,
            target_id_field,
            source_title_field,
            target_title_field,
            columns,
            skip_columns,
            conflict_handling,
            column_types,
            query,
            extra_properties,
            git_sha,
            modified_by,
            on_invalid,
            convention,
            empty_when,
            distinct,
        )
    }

    /// Pointer to replace_relationships(), the primary spelling; a connection is a relationship.
    #[pyo3(signature = (data, connection_type, source_type, source_id_field, target_type, target_id_field, source_title_field=None, target_title_field=None, columns=None, skip_columns=None, conflict_handling=None, column_types=None, query=None, extra_properties=None, git_sha=None, modified_by=None, on_invalid="error", convention=None, empty_when=None))]
    // The loader arguments of replace_relationships, passed through unchanged.
    #[allow(clippy::too_many_arguments)]
    fn replace_connections(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        data: Option<&Bound<'_, PyAny>>,
        connection_type: String,
        source_type: String,
        source_id_field: String,
        target_type: String,
        target_id_field: String,
        source_title_field: Option<String>,
        target_title_field: Option<String>,
        columns: Option<&Bound<'_, PyList>>,
        skip_columns: Option<&Bound<'_, PyList>>,
        conflict_handling: Option<String>,
        column_types: Option<&Bound<'_, PyDict>>,
        query: Option<String>,
        extra_properties: Option<&Bound<'_, PyDict>>,
        git_sha: Option<String>,
        modified_by: Option<String>,
        on_invalid: &str,
        convention: Option<&str>,
        empty_when: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        Self::replace_relationships(
            slf,
            py,
            data,
            connection_type,
            source_type,
            source_id_field,
            target_type,
            target_id_field,
            source_title_field,
            target_title_field,
            columns,
            skip_columns,
            conflict_handling,
            column_types,
            query,
            extra_properties,
            git_sha,
            modified_by,
            on_invalid,
            convention,
            empty_when,
        )
    }

    /// Pointer to add_relationships_bulk(), the primary spelling; a connection is a relationship.
    #[pyo3(signature = (connections, *, git_sha=None, modified_by=None))]
    fn add_connections_bulk(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        connections: &Bound<'_, PyList>,
        git_sha: Option<String>,
        modified_by: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        Self::add_relationships_bulk(slf, py, connections, git_sha, modified_by)
    }

    /// Pointer to add_relationships_from_source(), the primary spelling; a connection is a relationship.
    #[pyo3(signature = (connections, *, git_sha=None, modified_by=None))]
    fn add_connections_from_source(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        connections: &Bound<'_, PyList>,
        git_sha: Option<String>,
        modified_by: Option<String>,
    ) -> PyResult<Py<PyAny>> {
        Self::add_relationships_from_source(slf, py, connections, git_sha, modified_by)
    }

    /// Pointer to relationships(), the primary spelling; a connection is a relationship.
    #[pyo3(signature = (indices=None, parent_info=None, include_node_properties=None,
                        flatten_single_parent=true))]
    fn connections(
        slf: &Bound<'_, Self>,
        indices: Option<Vec<usize>>,
        parent_info: Option<bool>,
        include_node_properties: Option<bool>,
        flatten_single_parent: Option<bool>,
    ) -> PyResult<Py<PyAny>> {
        Self::relationships(
            slf,
            indices,
            parent_info,
            include_node_properties,
            flatten_single_parent,
        )
    }

    /// Pointer to relationship_types(), the primary spelling; a connection is a relationship.
    fn connection_types(slf: &Bound<'_, Self>) -> PyResult<Py<PyAny>> {
        Self::relationship_types(slf)
    }

    /// Pointer to create_relationships(), the primary spelling; a connection is a relationship.
    #[pyo3(signature = (connection_type, keep_selection=None, conflict_handling=None, properties=None, source_type=None, target_type=None))]
    // The arguments of create_relationships, passed through unchanged.
    #[allow(clippy::too_many_arguments)]
    fn create_connections(
        slf: &Bound<'_, Self>,
        py: Python<'_>,
        connection_type: String,
        keep_selection: Option<bool>,
        conflict_handling: Option<String>,
        properties: Option<&Bound<'_, PyDict>>,
        source_type: Option<String>,
        target_type: Option<String>,
    ) -> PyResult<Self> {
        Self::create_relationships(
            slf,
            py,
            connection_type,
            keep_selection,
            conflict_handling,
            properties,
            source_type,
            target_type,
        )
    }
}
