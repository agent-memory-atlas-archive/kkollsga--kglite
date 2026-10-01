//! The `kg:` fast path: what a `kg:manifest` changes about an RDF import.
//!
//! With a manifest the loader restores node ids and titles to the kind the
//! exporter wrote, re-attaches secondary labels and parent types, and
//! re-declares valid time through [`ExportManifest::apply_declarations`].
//! Typed literals need no manifest: their datatype IRIs already map back (see
//! `fold`). Without a manifest every function here is a no-op.

use std::collections::HashSet;

use petgraph::graph::NodeIndex;

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::io::export::kg_vocab::node_id_segment;
use crate::graph::io::export::manifest::ColumnKind;
use crate::graph::io::export::ExportManifest;

use super::fold::datatype_to_value;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Ids already given to a node, per type, so a restored id can never silently
/// replace another node's entry in the id index.
#[derive(Default)]
pub(super) struct IdentityCheck {
    seen: HashSet<(String, Value)>,
}

/// The id and title value of a node. A node of a manifest type whose IRI is
/// `<base>node/<Type>/<id>` gets back its original id; every other node
/// keeps the dense id.
pub(super) fn node_identity(
    manifest: Option<&ExportManifest>,
    node_type: &str,
    iri: &str,
    title: String,
    dense: u32,
    check: &mut IdentityCheck,
) -> Result<(Value, Value), String> {
    let Some(entry) = manifest.and_then(|m| m.node_types.get(node_type)) else {
        return Ok((Value::UniqueId(dense), Value::String(title)));
    };
    let restored = node_id_segment(iri, node_type).and_then(|segment| match entry.id_kind {
        ColumnKind::Int64 => segment.parse().ok().map(Value::Int64),
        ColumnKind::UniqueId => segment.parse().ok().map(Value::UniqueId),
        ColumnKind::String => Some(Value::String(segment)),
        _ => None,
    });
    let id = restored.unwrap_or(Value::UniqueId(dense));
    if !check.seen.insert((node_type.to_string(), id.clone())) {
        return Err(format!(
            "kg:manifest import: two {node_type} nodes resolve to id {id:?} ({iri})"
        ));
    }
    let title = match title_datatype(entry.title_kind) {
        Some(datatype) => datatype_to_value(&title, &format!("{XSD}{datatype}")),
        None => Value::String(title),
    };
    Ok((id, title))
}

/// The XSD datatype that reads a title of this kind back; `None` for text.
fn title_datatype(kind: ColumnKind) -> Option<&'static str> {
    match kind {
        ColumnKind::Int64 | ColumnKind::UniqueId => Some("integer"),
        ColumnKind::Float64 => Some("double"),
        ColumnKind::Boolean => Some("boolean"),
        ColumnKind::DateTime => Some("date"),
        ColumnKind::Timestamp => Some("dateTime"),
        _ => None,
    }
}

/// Give a freshly added node of a manifest type the secondary labels every
/// node of that type carried.
pub(super) fn add_labels(
    graph: &mut DirGraph,
    manifest: Option<&ExportManifest>,
    node_type: &str,
    idx: NodeIndex,
) {
    let Some(entry) = manifest.and_then(|m| m.node_types.get(node_type)) else {
        return;
    };
    for label in &entry.labels {
        let key = graph.interner.get_or_intern(label);
        graph.add_node_label(idx, key);
    }
}

/// After every row is loaded: parent types, then the valid-time declarations.
/// Returns warnings for what could not be restored.
pub(super) fn apply(graph: &mut DirGraph, manifest: &ExportManifest) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    for (name, entry) in &manifest.node_types {
        if let Some(parent) = &entry.parent {
            if graph.has_node_type(name) {
                graph.set_parent_type(name, Some(parent));
            }
        }
        if !entry.partial_labels.is_empty() && graph.has_node_type(name) {
            warnings.push(format!(
                "{name}: labels {:?} were carried by only some nodes and are not restored",
                entry.partial_labels
            ));
        }
    }
    warnings.extend(manifest.apply_declarations(graph)?);
    Ok(warnings)
}
