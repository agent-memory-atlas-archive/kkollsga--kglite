//! The generic embedding methods — `set_embeddings`, `add_embeddings`,
//! `embed_texts`, `embeddings`, `embedding`, `embedding_dim`,
//! `remove_embeddings`, `vector_search`, `search_text` and the vector-index
//! lifecycle — routed by
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

/// The route-specific keywords of `vector_search` / `search_text`: `returning`
/// belongs to the node twin, `types` and `relationship_keys` to the
/// relationship twin.
struct Search<'a, 'py> {
    method: &'static str,
    types: Option<&'a Bound<'py, PyAny>>,
    relationship_keys: Option<HashMap<String, String>>,
    returning: Option<Vec<String>>,
}

impl Search<'_, '_> {
    fn route(&self, entity: &str) -> PyResult<Entity> {
        let entity = Entity::parse(self.method, entity)?;
        match entity {
            Entity::Node => {
                let twin = format!("node_{}", self.method);
                let types = self.types.is_some_and(|types| !types.is_none());
                refuse_foreign_keyword(self.method, &twin, "types", "relationship", types)?;
                refuse_foreign_keyword(
                    self.method,
                    &twin,
                    "relationship_keys",
                    "relationship",
                    self.relationship_keys.is_some(),
                )?;
            }
            Entity::Relationship => refuse_foreign_keyword(
                self.method,
                &format!("relationship_{}", self.method),
                "returning",
                "node",
                self.returning.is_some(),
            )?,
        }
        Ok(entity)
    }
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
            Entity::Node => self.embed_node_texts(
                py,
                node_type,
                text_column,
                batch_size,
                show_progress,
                mode,
                metric,
            ),
            Entity::Relationship => self.embed_relationship_texts(
                py,
                node_type,
                text_column,
                batch_size.unwrap_or(256),
                show_progress.unwrap_or(true),
                mode,
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

    /// Read one stored vector: a node's by id (default) or, with entity='relationship', a relationship's by endpoint address.
    #[pyo3(signature = (node_type, text_column, node_id, *, entity="node", relationship_keys=None))]
    fn embedding(
        &self,
        py: Python<'_>,
        node_type: &str,
        text_column: &str,
        node_id: &Bound<'_, PyAny>,
        entity: &str,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        match Entity::parse("embedding", entity)? {
            Entity::Node => {
                refuse_foreign_keyword(
                    "embedding",
                    "node_embedding",
                    "relationship_keys",
                    "relationship",
                    relationship_keys.is_some(),
                )?;
                self.node_embedding(py, node_type, text_column, node_id)
            }
            Entity::Relationship => {
                self.relationship_embedding(py, node_type, text_column, node_id, relationship_keys)
            }
        }
    }

    /// The vector dimension of a node embedding store (default) or, with entity='relationship', a relationship one; None when absent.
    #[pyo3(signature = (node_type, text_column, *, entity="node"))]
    fn embedding_dim(
        &self,
        node_type: &str,
        text_column: &str,
        entity: &str,
    ) -> PyResult<Option<usize>> {
        Ok(match Entity::parse("embedding_dim", entity)? {
            Entity::Node => self.node_embedding_dim(node_type, text_column),
            Entity::Relationship => self.relationship_embedding_dim(node_type, text_column),
        })
    }

    /// Remove a node embedding store (default) or, with entity='relationship', a relationship one; refuses a store that does not exist.
    #[pyo3(signature = (node_type, text_column, *, entity="node"))]
    fn remove_embeddings(
        &mut self,
        node_type: &str,
        text_column: &str,
        entity: &str,
    ) -> PyResult<()> {
        match Entity::parse("remove_embeddings", entity)? {
            Entity::Node => self.remove_node_embeddings(node_type, text_column),
            Entity::Relationship => self.remove_relationship_embeddings(node_type, text_column),
        }
    }

    /// Rank stored vectors against a query vector: the selection's nodes (default) or, with entity='relationship', relationships.
    #[pyo3(signature = (text_column, query_vector, top_k=10, metric=None, to_df=false, returning=None, exact=false, *, entity="node", types=None, relationship_keys=None))]
    // One Rust argument per Python keyword across both routes.
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
        entity: &str,
        types: Option<&Bound<'_, PyAny>>,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        let search = Search {
            method: "vector_search",
            types,
            relationship_keys,
            returning,
        };
        match search.route(entity)? {
            Entity::Node => self.node_vector_search(
                py,
                text_column,
                query_vector,
                top_k,
                metric,
                to_df,
                search.returning,
                exact,
            ),
            Entity::Relationship => self.relationship_vector_search(
                py,
                text_column,
                query_vector,
                top_k.unwrap_or(10),
                metric,
                to_df.unwrap_or(false),
                search.types,
                exact.unwrap_or(false),
                search.relationship_keys,
            ),
        }
    }

    /// Embed a query with the registered model and rank stored vectors: the selection's nodes (default) or, with entity='relationship', relationships.
    #[pyo3(signature = (text_column, query, top_k=10, metric=None, to_df=false, returning=None, exact=false, *, entity="node", types=None, relationship_keys=None))]
    // One Rust argument per Python keyword across both routes.
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
        entity: &str,
        types: Option<&Bound<'_, PyAny>>,
        relationship_keys: Option<HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        let search = Search {
            method: "search_text",
            types,
            relationship_keys,
            returning,
        };
        match search.route(entity)? {
            Entity::Node => self.node_search_text(
                py,
                text_column,
                query,
                top_k,
                metric,
                to_df,
                search.returning,
                exact,
            ),
            Entity::Relationship => self.relationship_search_text(
                py,
                text_column,
                query,
                top_k.unwrap_or(10),
                metric,
                to_df.unwrap_or(false),
                search.types,
                exact.unwrap_or(false),
                search.relationship_keys,
            ),
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
