//! Declare-time warning for a property name nothing stored carries.
//!
//! A misspelt or renamed property in `distinct`, `required_properties`,
//! `property_types` or `property_values` is accepted (the data may arrive
//! later) but reads as a rule that can never be satisfied or never splits
//! anything, so the declaration says so. Judged from the recorded schema, with
//! no data scan, and only for a type that already has rows.

use std::collections::BTreeSet;

use super::{ClassDecl, OntologyStore, RelationshipDecl};
use crate::graph::dir_graph::DirGraph;

/// The property names a class declaration's contracts mention.
fn class_names(decl: &ClassDecl) -> BTreeSet<&str> {
    decl.required_properties
        .iter()
        .map(String::as_str)
        .chain(decl.property_types.keys().map(String::as_str))
        .chain(decl.property_values.keys().map(String::as_str))
        .collect()
}

/// The property names a relationship declaration's contracts mention.
fn relationship_names(decl: &RelationshipDecl) -> BTreeSet<&str> {
    decl.required_properties
        .iter()
        .map(String::as_str)
        .chain(decl.property_types.keys().map(String::as_str))
        .chain(decl.property_values.keys().map(String::as_str))
        .chain(
            decl.distinct
                .iter()
                .flat_map(|d| d.properties.iter().map(String::as_str)),
        )
        .collect()
}

pub(crate) fn unseen_property_warnings(graph: &DirGraph, store: &OntologyStore) -> Vec<String> {
    let mut warnings = Vec::new();
    for (class, decl) in &store.classes {
        let populated = graph
            .type_indices
            .get(class)
            .is_some_and(|nodes| !nodes.is_empty());
        let Some(carried) = graph.get_node_type_metadata(class).filter(|_| populated) else {
            continue;
        };
        for name in class_names(decl) {
            let canonical = graph.resolve_alias(class, name);
            if matches!(canonical, "id" | "title")
                || carried.contains_key(name)
                || carried.contains_key(canonical)
            {
                continue;
            }
            warnings.push(format!(
                "class '{class}': property '{name}' is carried by no stored '{class}' node; the \
                 contract on it can never be met or never matches until a node carries it \
                 (check the spelling)"
            ));
        }
    }
    let edge_counts = (!store.relationships.is_empty()).then(|| graph.get_edge_type_counts());
    for (rel, decl) in &store.relationships {
        let populated = edge_counts
            .as_ref()
            .and_then(|counts| counts.get(rel))
            .is_some_and(|n| *n > 0);
        if !populated {
            continue;
        }
        let info = graph.connection_type_metadata.get(rel);
        for name in relationship_names(decl) {
            if info.is_some_and(|i| i.property_types.contains_key(name)) {
                continue;
            }
            warnings.push(format!(
                "relationship '{rel}': property '{name}' is carried by no stored '{rel}' edge; \
                 the contract on it can never be met or never matches until an edge carries it \
                 (check the spelling)"
            ));
        }
    }
    warnings
}
