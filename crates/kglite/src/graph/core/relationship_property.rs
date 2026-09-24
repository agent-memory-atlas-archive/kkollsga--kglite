//! `r.<key>` on a relationship: the one rule every read path shares.
//!
//! A relationship's own property always wins — `r.type` reads a stored `type`
//! property when there is one, as `n.type` does on a node. Only a relationship
//! without that property (or with it NULL) answers from its envelope:
//! `type` / `connection_type` is the relationship type (`type(r)`), `id` the
//! relationship id (`id(r)`), `start` / `start_id` and `end` / `end_id` the
//! endpoint node ids (`id(startNode(r))`, `id(endNode(r))`). Any other key
//! without a stored value is NULL.
//!
//! A MATCH binding, a relationship value (`collect`, `UNWIND`,
//! `relationships(p)`, `YIELD relationship`) and a WHERE the planner pushed
//! into the matcher all resolve through [`relationship_property`], so a
//! filter and the projection of the same expression cannot disagree.

use crate::datatypes::values::{RelValue, Value};
use crate::graph::core::iterators::GraphEdgeRef;
use crate::graph::schema::{DirGraph, EdgeData};
use crate::graph::storage::GraphRead;
use petgraph::graph::NodeIndex;

/// The envelope a relationship read falls back to.
pub struct RelationshipEnvelope<'a> {
    pub id: usize,
    pub rel_type: &'a str,
    pub source: NodeIndex,
    pub target: NodeIndex,
}

/// `r.<key>`: `stored` when present and not NULL, else the envelope field for
/// `key`, else NULL.
pub fn relationship_property(
    graph: &DirGraph,
    stored: Option<Value>,
    key: &str,
    envelope: RelationshipEnvelope<'_>,
) -> Value {
    if let Some(value) = stored {
        if !matches!(value, Value::Null) {
            return value;
        }
    }
    envelope_property(graph, key, envelope)
}

/// The envelope field `key` names, or NULL. Split out so a reader with the
/// stored value in hand can skip building the envelope on the hit path: the
/// matcher's pushed-down WHERE reads one property per relationship per row,
/// and the envelope's type-name resolve is the part that costs.
fn envelope_property(graph: &DirGraph, key: &str, envelope: RelationshipEnvelope<'_>) -> Value {
    match key {
        "type" | "connection_type" => Value::String(envelope.rel_type.to_string()),
        "id" => Value::Int64(envelope.id as i64),
        "start" | "start_id" => node_id(graph, envelope.source),
        "end" | "end_id" => node_id(graph, envelope.target),
        _ => Value::Null,
    }
}

/// `x.<key>` on a relationship value, under the same rule.
pub fn relationship_value_property(graph: &DirGraph, rel: &RelValue, key: &str) -> Value {
    relationship_property(
        graph,
        rel.properties.get(key).cloned(),
        key,
        RelationshipEnvelope {
            id: rel.id as usize,
            rel_type: &rel.rel_type,
            source: NodeIndex::new(rel.start_id as usize),
            target: NodeIndex::new(rel.end_id as usize),
        },
    )
}

/// `r.<key>` for an edge the matcher is expanding — the reader a pushed-down
/// WHERE filter evaluates against, so it agrees with the projection. `data` is
/// `edge.weight()`, fetched once by the caller: on disk every `weight()` call
/// materialises the edge, and a filter may read several properties.
pub fn edge_ref_property(
    graph: &DirGraph,
    edge: &GraphEdgeRef<'_>,
    data: &EdgeData,
    key: &str,
) -> Option<Value> {
    if let Some(value) = data.get_property(key) {
        if !matches!(value, Value::Null) {
            return Some(value.clone());
        }
    }
    Some(envelope_property(
        graph,
        key,
        RelationshipEnvelope {
            id: edge.id().index(),
            rel_type: graph.interner.resolve(edge.connection_type()),
            source: edge.source(),
            target: edge.target(),
        },
    ))
}

/// `id(n)` for an endpoint: the node's id, NULL when the slot is empty.
fn node_id(graph: &DirGraph, node: NodeIndex) -> Value {
    graph
        .graph
        .node_view(node)
        .map_or(Value::Null, |view| view.id().into_owned())
}
