//! What an embedding-store refusal tells its reader: the tail that points at
//! the store they meant, and the remedy spelled as a call they can make.
//!
//! The same store is reached from a binding method (`build_vector_index`,
//! `relationship_embeddings`, …) and from a Cypher procedure
//! (`db.relationship_embeddings.build_index`, …). A remedy names a call in the
//! surface the caller used — a Python caller is told the method, a Cypher
//! caller the procedure — so [`Surface`] travels with every message that names
//! one. The "did you mean" tails are surface-neutral: they name stores and
//! columns, not calls, except where the store the reader meant lives on the
//! other entity, and then the way to reach it is again a call.

use crate::graph::embedding_inventory::EmbeddingEntity;
use crate::graph::embeddings::{store_name, text_column_of};
use crate::graph::mutation::validation::did_you_mean;
use crate::graph::schema::DirGraph;

/// The surface a refusal is raised through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// A binding method, named as `kglite::api::embeddings` and the Python
    /// wheel spell it.
    Method,
    /// A `db.*_embeddings.*` procedure.
    Cypher,
}

fn procedure_family(entity: EmbeddingEntity) -> &'static str {
    match entity {
        EmbeddingEntity::Node => "db.node_embeddings",
        EmbeddingEntity::Relationship => "db.relationship_embeddings",
    }
}

fn procedure_call(entity: EmbeddingEntity, name: &str, ty: &str, col: &str) -> String {
    format!(
        "CALL {}.{name}({{type: '{ty}', text_column: '{col}'}})",
        procedure_family(entity)
    )
}

impl Surface {
    /// The call that builds an HNSW index over `(ty, col)`.
    pub(crate) fn build_index(self, entity: EmbeddingEntity, ty: &str, col: &str) -> String {
        match (self, entity) {
            (Self::Method, EmbeddingEntity::Node) => format!("build_vector_index('{ty}', '{col}')"),
            (Self::Method, EmbeddingEntity::Relationship) => {
                format!("build_relationship_vector_index('{ty}', '{col}')")
            }
            (Self::Cypher, _) => procedure_call(entity, "build_index", ty, col),
        }
    }

    /// The calls that write a `(ty, col)` store.
    pub(crate) fn write_store(self, entity: EmbeddingEntity, ty: &str, col: &str) -> String {
        match (self, entity) {
            (Self::Method, EmbeddingEntity::Node) => {
                format!("embed_texts('{ty}', '{col}') or set_embeddings()")
            }
            (Self::Method, EmbeddingEntity::Relationship) => {
                format!(
                    "embed_relationship_texts('{ty}', '{col}') or set_relationship_embeddings()"
                )
            }
            (Self::Cypher, _) => format!(
                "CALL {family}.embed or {family}.set with {{type: '{ty}', text_column: '{col}'}}",
                family = procedure_family(entity)
            ),
        }
    }

    /// How to reach the other entity's store from this surface.
    fn reach(self, entity: EmbeddingEntity) -> String {
        match self {
            Self::Method => format!(
                "pass entity='{}' (or call the {}_* method)",
                entity.as_str(),
                entity.as_str()
            ),
            Self::Cypher => format!("use {}.*", procedure_family(entity)),
        }
    }
}

fn other(entity: EmbeddingEntity) -> EmbeddingEntity {
    match entity {
        EmbeddingEntity::Node => EmbeddingEntity::Relationship,
        EmbeddingEntity::Relationship => EmbeddingEntity::Node,
    }
}

/// Every `(type, text column)` pair with a store on `entity`, sorted.
fn stores(graph: &DirGraph, entity: EmbeddingEntity) -> Vec<(&str, &str)> {
    let mut pairs: Vec<(&str, &str)> = match entity {
        EmbeddingEntity::Node => graph
            .embeddings
            .keys()
            .map(|(ty, name)| (ty.as_str(), text_column_of(name).unwrap_or(name)))
            .collect(),
        EmbeddingEntity::Relationship => graph
            .edge_embeddings
            .keys()
            .map(|(ty, name)| (ty.as_str(), text_column_of(name).unwrap_or(name)))
            .collect(),
    };
    pairs.sort_unstable();
    pairs.dedup();
    pairs
}

fn has_store(graph: &DirGraph, entity: EmbeddingEntity, ty: &str, col: &str) -> bool {
    stores(graph, entity).contains(&(ty, col))
}

/// Stores listed in a tail at most; a long-tailed type vocabulary would
/// otherwise turn one refusal into a page.
const LISTED_STORES: usize = 12;

/// The tail for a `(ty, col)` store that does not exist on `entity`, or `""`
/// when there is nothing honest to point at. In order of how specific the
/// pointer is: the store name passed where the text column belongs; a store
/// of that name on the other entity; a near-miss column on the same type; a
/// near-miss type carrying that column; the columns the type does carry; every
/// store of the entity (up to [`LISTED_STORES`]).
pub(crate) fn missing_store_hint(
    graph: &DirGraph,
    entity: EmbeddingEntity,
    ty: &str,
    col: &str,
    surface: Surface,
) -> String {
    let known = stores(graph, entity);
    if let Some(stripped) = text_column_of(col) {
        if known.contains(&(ty, stripped)) {
            return format!(
                " Did you mean '{stripped}'? The text column is '{stripped}'; '{col}' is the \
                 embedding store's own name."
            );
        }
    }
    let other_entity = other(entity);
    if has_store(graph, other_entity, ty, col) {
        return format!(
            " '{ty}.{col}' is a {} embedding store — {}.",
            other_entity.as_str(),
            surface.reach(other_entity)
        );
    }
    let columns: Vec<&str> = known
        .iter()
        .filter(|(stored, _)| *stored == ty)
        .map(|(_, column)| *column)
        .collect();
    let suggestion = did_you_mean(col, &columns);
    if !suggestion.is_empty() {
        return suggestion;
    }
    if columns.is_empty() {
        let types: Vec<&str> = known
            .iter()
            .filter(|(_, column)| *column == col)
            .map(|(stored, _)| *stored)
            .collect();
        let suggestion = did_you_mean(ty, &types);
        if !suggestion.is_empty() {
            return suggestion;
        }
    } else {
        return format!(
            " {} embedding stores of '{ty}': {}.",
            capitalised(entity),
            columns.join(", ")
        );
    }
    if known.is_empty() {
        return String::new();
    }
    let listed: Vec<String> = known
        .iter()
        .take(LISTED_STORES)
        .map(|(stored, column)| format!("{stored}.{column}"))
        .collect();
    let more = known.len().saturating_sub(LISTED_STORES);
    format!(
        " {} embedding stores: {}{}.",
        capitalised(entity),
        listed.join(", "),
        if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        }
    )
}

/// The tail for a text column no store of `entity` carries under any type
/// (a cross-type query or search that names no type).
pub(crate) fn missing_column_hint(
    graph: &DirGraph,
    entity: EmbeddingEntity,
    col: &str,
    surface: Surface,
) -> String {
    let known = stores(graph, entity);
    if let Some(stripped) = text_column_of(col) {
        if known.iter().any(|(_, column)| *column == stripped) {
            return format!(
                " Did you mean '{stripped}'? The text column is '{stripped}'; '{col}' is the \
                 embedding store's own name."
            );
        }
    }
    let other_entity = other(entity);
    let elsewhere: Vec<&str> = stores(graph, other_entity)
        .into_iter()
        .filter(|(_, column)| *column == col)
        .map(|(ty, _)| ty)
        .collect();
    if !elsewhere.is_empty() {
        return format!(
            " '{col}' is a {} embedding store (on {}) — {}.",
            other_entity.as_str(),
            elsewhere.join(", "),
            surface.reach(other_entity)
        );
    }
    let mut columns: Vec<&str> = known.iter().map(|(_, column)| *column).collect();
    columns.sort_unstable();
    columns.dedup();
    let suggestion = did_you_mean(col, &columns);
    if !suggestion.is_empty() {
        return suggestion;
    }
    if columns.is_empty() {
        String::new()
    } else {
        format!(
            " {} embedding text columns: {}.",
            capitalised(entity),
            columns.join(", ")
        )
    }
}

fn capitalised(entity: EmbeddingEntity) -> &'static str {
    match entity {
        EmbeddingEntity::Node => "Node",
        EmbeddingEntity::Relationship => "Relationship",
    }
}

/// `No {entity} embedding store 'T.col'` plus [`missing_store_hint`].
pub(crate) fn missing_store_error(
    graph: &DirGraph,
    entity: EmbeddingEntity,
    ty: &str,
    col: &str,
    surface: Surface,
) -> String {
    format!(
        "No {} embedding store '{ty}.{col}'.{}",
        entity.as_str(),
        missing_store_hint(graph, entity, ty, col, surface)
    )
}

/// The refusal for a refresh with no index to fold into.
pub(crate) fn no_index_to_refresh(
    entity: EmbeddingEntity,
    ty: &str,
    col: &str,
    surface: Surface,
) -> String {
    let (store, deleted) = match entity {
        EmbeddingEntity::Node => ("", "a delete of an embedded node"),
        EmbeddingEntity::Relationship => (
            "relationship store ",
            "a delete of an embedded relationship or an endpoint",
        ),
    };
    format!(
        "no vector index on {store}'{ty}.{}' to refresh — none was built, or {deleted} (or a \
         vacuum()) dropped it. Build one with {}.",
        store_name(col),
        surface.build_index(entity, ty, col)
    )
}

#[cfg(test)]
#[path = "embedding_hints_tests.rs"]
mod tests;
