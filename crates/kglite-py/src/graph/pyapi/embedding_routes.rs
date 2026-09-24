//! The generic embedding methods — `set_embeddings`, `add_embeddings`,
//! `embed_texts`, `embeddings` and the vector-index lifecycle — routed by
//! `entity=` to their node twin (the default) or their relationship twin. A
//! router holds no logic of its own: it accepts the union of both twins'
//! keywords and refuses, by name, one the chosen twin does not take.

use std::collections::HashMap;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::graph::KnowledgeGraph;

#[derive(Clone, Copy)]
enum Entity {
    Node,
    Relationship,
}

impl Entity {
    fn parse(method: &str, entity: &str) -> PyResult<Self> {
        match entity {
            "node" => Ok(Self::Node),
            "relationship" => Ok(Self::Relationship),
            other => Err(PyValueError::new_err(format!(
                "{method}(entity='{other}'): entity must be 'node' (the default) or \
                 'relationship'"
            ))),
        }
    }
}

/// Refuse a keyword that only the relationship twin takes on a node-routed
/// call, or the reverse, naming the entity it belongs to.
fn refuse_foreign_keyword(
    method: &str,
    route: &str,
    keyword: &str,
    owner: &str,
    passed: bool,
) -> PyResult<()> {
    if !passed {
        return Ok(());
    }
    Err(PyTypeError::new_err(format!(
        "{method}(): `{keyword}` belongs to entity='{owner}'; this call routes to {route}()"
    )))
}

/// The node writers take `{id: vector}` only. The cast error is the one the
/// node twin's own argument extraction raises, so a node call through the
/// router fails exactly as it did before the router existed.
fn node_rows<'a, 'py>(embeddings: &'a Bound<'py, PyAny>) -> PyResult<&'a Bound<'py, PyDict>> {
    embeddings.cast::<PyDict>().map_err(PyErr::from)
}

#[pymethods]
impl KnowledgeGraph {
    /// Replace an embedding store: a node type's (default) or, with entity='relationship', a relationship type's.
    #[pyo3(signature = (node_type, text_column, embeddings, metric=None, *, entity="node", relationship_keys=None))]
    // One Rust argument per Python keyword across both routes.
    #[allow(clippy::too_many_arguments)]
    fn set_embeddings(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        embeddings: &Bound<'_, PyAny>,
        metric: Option<&str>,
        entity: &str,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        match Entity::parse("set_embeddings", entity)? {
            Entity::Node => {
                refuse_foreign_keyword(
                    "set_embeddings",
                    "set_node_embeddings",
                    "relationship_keys",
                    "relationship",
                    relationship_keys.is_some(),
                )?;
                self.set_node_embeddings(py, node_type, text_column, node_rows(embeddings)?, metric)
            }
            Entity::Relationship => self.set_relationship_embeddings(
                py,
                node_type,
                text_column,
                embeddings,
                relationship_keys,
                metric,
            ),
        }
    }

    /// Upsert into an embedding store: a node type's (default) or, with entity='relationship', a relationship type's.
    #[pyo3(signature = (node_type, text_column, embeddings, metric=None, *, entity="node", relationship_keys=None))]
    // One Rust argument per Python keyword across both routes.
    #[allow(clippy::too_many_arguments)]
    fn add_embeddings(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        embeddings: &Bound<'_, PyAny>,
        metric: Option<&str>,
        entity: &str,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        match Entity::parse("add_embeddings", entity)? {
            Entity::Node => {
                refuse_foreign_keyword(
                    "add_embeddings",
                    "add_node_embeddings",
                    "relationship_keys",
                    "relationship",
                    relationship_keys.is_some(),
                )?;
                self.add_node_embeddings(py, node_type, text_column, node_rows(embeddings)?, metric)
            }
            Entity::Relationship => self.add_relationship_embeddings(
                py,
                node_type,
                text_column,
                embeddings,
                relationship_keys,
                metric,
            ),
        }
    }

    /// Embed a text column with the registered model, for a node type (default) or, with entity='relationship', a relationship type.
    #[pyo3(signature = (node_type, text_column, batch_size=256, show_progress=true, mode=None, *, entity="node", metric=None))]
    // One Rust argument per Python keyword across both routes.
    #[allow(clippy::too_many_arguments)]
    fn embed_texts(
        &mut self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        batch_size: Option<usize>,
        show_progress: Option<bool>,
        mode: Option<&str>,
        entity: &str,
        metric: Option<&str>,
    ) -> PyResult<Py<PyAny>> {
        match Entity::parse("embed_texts", entity)? {
            Entity::Node => {
                refuse_foreign_keyword(
                    "embed_texts",
                    "embed_node_texts",
                    "metric",
                    "relationship",
                    metric.is_some(),
                )?;
                self.embed_node_texts(py, node_type, text_column, batch_size, show_progress, mode)
            }
            Entity::Relationship => self.embed_relationship_texts(
                py,
                node_type,
                text_column,
                mode,
                batch_size.unwrap_or(256),
                show_progress.unwrap_or(true),
                metric,
            ),
        }
    }

    /// Read stored vectors: node vectors by id (default) or, with entity='relationship', relationship rows by endpoints.
    #[pyo3(signature = (node_type_or_text_column, text_column=None, *, entity="node", relationship_keys=None))]
    fn embeddings(
        &self,
        py: Python<'_>,
        node_type_or_text_column: &str,
        text_column: Option<&str>,
        entity: &str,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        match Entity::parse("embeddings", entity)? {
            Entity::Node => {
                refuse_foreign_keyword(
                    "embeddings",
                    "node_embeddings",
                    "relationship_keys",
                    "relationship",
                    relationship_keys.is_some(),
                )?;
                self.node_embeddings(py, node_type_or_text_column, text_column)
            }
            Entity::Relationship => {
                let text_column = text_column.ok_or_else(|| {
                    PyTypeError::new_err(
                        "embeddings(entity='relationship') takes (relationship_type, \
                         text_column); text_column is missing",
                    )
                })?;
                self.relationship_embeddings(
                    py,
                    node_type_or_text_column,
                    text_column,
                    relationship_keys,
                )
            }
        }
    }

    /// Build an HNSW index over a node embedding store (default) or, with entity='relationship', a relationship one.
    #[pyo3(signature = (node_type, text_column, m=None, ef_construction=None, ef_search=None, metric=None, auto_refresh_limit=None, *, entity="node"))]
    // One Rust argument per HNSW knob plus the route.
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
        entity: &str,
    ) -> PyResult<Py<PyAny>> {
        match Entity::parse("build_vector_index", entity)? {
            Entity::Node => self.build_node_vector_index(
                py,
                node_type,
                text_column,
                m,
                ef_construction,
                ef_search,
                metric,
                auto_refresh_limit,
            ),
            Entity::Relationship => self.build_relationship_vector_index(
                py,
                node_type,
                text_column,
                m,
                ef_construction,
                ef_search,
                metric,
                auto_refresh_limit,
            ),
        }
    }

    /// Drop the HNSW index over a node embedding store (default) or, with entity='relationship', a relationship one.
    #[pyo3(signature = (node_type, text_column, *, entity="node"))]
    fn drop_vector_index(
        &mut self,
        node_type: &str,
        text_column: &str,
        entity: &str,
    ) -> PyResult<bool> {
        match Entity::parse("drop_vector_index", entity)? {
            Entity::Node => self.drop_node_vector_index(node_type, text_column),
            Entity::Relationship => self.drop_relationship_vector_index(node_type, text_column),
        }
    }

    /// Whether an HNSW index is built over a node embedding store (default) or, with entity='relationship', a relationship one.
    #[pyo3(signature = (node_type, text_column, *, entity="node"))]
    fn has_vector_index(&self, node_type: &str, text_column: &str, entity: &str) -> PyResult<bool> {
        match Entity::parse("has_vector_index", entity)? {
            Entity::Node => self.has_node_vector_index(node_type, text_column),
            Entity::Relationship => Ok(self.has_relationship_vector_index(node_type, text_column)),
        }
    }

    /// Fold outstanding vectors into a node store's HNSW index (default) or, with entity='relationship', a relationship store's.
    #[pyo3(signature = (node_type, text_column, *, entity="node"))]
    fn refresh_vector_index(
        &self,
        node_type: &str,
        text_column: &str,
        entity: &str,
    ) -> PyResult<usize> {
        match Entity::parse("refresh_vector_index", entity)? {
            Entity::Node => self.refresh_node_vector_index(node_type, text_column),
            Entity::Relationship => self.refresh_relationship_vector_index(node_type, text_column),
        }
    }
}
