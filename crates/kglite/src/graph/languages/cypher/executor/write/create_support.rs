//! Helpers `create_node` calls around the insert: the abstract-class refusal
//! and the id-index upkeep for the node just made.

use crate::datatypes::values::Value;
use crate::graph::schema::DirGraph;
use petgraph::graph::NodeIndex;

/// Refuse creating a node whose primary type is a declared **abstract**
/// class, naming the concrete declared subtypes (built only on the cold
/// path).
pub(crate) fn reject_abstract_create(graph: &DirGraph, label: &str) -> Result<(), String> {
    let Some(decl) = graph.ontology.classes.get(label) else {
        return Ok(());
    };
    if !decl.is_abstract {
        return Ok(());
    }
    let mut concrete: Vec<&str> = graph
        .ontology
        .classes
        .iter()
        .filter(|(name, d)| {
            !d.is_abstract && graph.ontology.ancestors(name).iter().any(|a| a == label)
        })
        .map(|(name, _)| name.as_str())
        .collect();
    concrete.sort_unstable();
    Err(if concrete.is_empty() {
        format!("'{label}' is an abstract ontology class and cannot hold nodes")
    } else {
        format!(
            "'{label}' is an abstract ontology class — create one of: {}",
            concrete.join(", ")
        )
    })
}

/// Keep `label`'s id index consistent with the node `create_node` just made.
/// Whenever the index is already cached — and therefore complete — insert into
/// it, so a sequential CREATE (e.g. UNWIND … CREATE) stays O(1)/node instead of
/// paying an O(n) rebuild-per-node. When it isn't cached, invalidate for lazy rebuild: the
/// `contains_key` guard is what stops us building a *partial* entry that
/// `build_id_index` would later short-circuit on and trust as complete.
///
/// Deliberately independent of whether a primary key is declared — the
/// declaration governs *uniqueness enforcement*, not index freshness, and
/// gating maintenance on it dropped an undeclared type's whole cached id
/// index on every single CREATE. Nothing about duplicate ids is protected by
/// invalidating: a rebuild and an incremental insert collapse a duplicate
/// identically — and both warn, so the write that forks an id says so then,
/// not at the next rebuild (a reload).
pub(super) fn note_created_id(
    graph: &mut DirGraph,
    label: &str,
    pk_id: Option<Value>,
    node_idx: NodeIndex,
) {
    match pk_id {
        Some(idv) if graph.id_indices.contains_key(label) => {
            let entry = graph.id_indices.entry_or_default(label.to_string());
            let duplicate = entry.get(&idv).is_some();
            entry.insert(idv, node_idx);
            if duplicate {
                crate::graph::dir_graph::warn_on_duplicate_ids(label, 1, 0);
            }
        }
        _ => {
            graph.id_indices.remove(label);
        }
    }
}
