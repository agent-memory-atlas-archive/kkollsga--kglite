//! Query-local equality hash index for cross-MATCH joins on non-id properties.
//!
//! Built once per `execute_match` call when the heuristic detects a single
//! typed-node pattern carrying exactly one `EqualsVar` / `EqualsNodeProp`
//! matcher. Probed per outer row, replacing N×M property scans with O(N+M)
//! work. Mirrors the per-query R-tree pattern in [`super::spatial_join`].
//!
//! The index is dropped when the executor goes out of scope; it never
//! mutates [`DirGraph::property_indices`].
//!
//! A probe must match exactly the nodes the per-row matcher would, which
//! compares with `values_equal` — across kinds. Per kind:
//!
//! * **Numbers** (`Int64`, `Float64`, `UniqueId`) are keyed on one spelling
//!   ([`canonical_id`]): two numbers are equal exactly when they denote the
//!   same value, so `5`, `5.0` and a `UniqueId(5)` share a key and `2^53+1`
//!   does not share one with the float it rounds to. `NaN` equals nothing
//!   and is neither indexed nor matched.
//! * **Dates and datetimes** are keyed with a datetime at midnight folded
//!   onto its date, the pair `values_equal` calls equal.
//! * **Text against a date or datetime** compares by parsing the text, and a
//!   **one-element JSON list** (`["Oslo"]`) equals its string. Neither is an
//!   equivalence a key can carry (two texts can parse to one date and still
//!   differ, and the JSON rule is not transitive), so a probe that could
//!   meet one — text when the index holds a date or datetime, a date or
//!   datetime when it holds text, text beginning `[` or any text against an
//!   index holding such text — declines, and the per-row matcher answers
//!   that row.
//! * A stored **list or map** compares element-wise with coercion; the index
//!   is not built over one.

use super::helpers::resolve_node_property;
use super::ResultRow;
use crate::datatypes::values::Value;
use crate::graph::core::filtering::parse_datetime_string;
use crate::graph::core::pattern_matching::{NodePattern, Pattern, PatternElement, PropertyMatcher};
use crate::graph::schema::canonical_id;
use crate::graph::schema::DirGraph;
use crate::graph::storage::GraphRead;
use petgraph::graph::NodeIndex;
use std::collections::HashMap;

/// Activation threshold: only build the index when there are at least this
/// many outer rows. Below it, the per-row pattern execution is already
/// cheap enough that the build cost doesn't pay back.
pub(super) const TRANSIENT_INDEX_THRESHOLD: usize = 64;

/// Per-query equality index over a single typed property.
pub(super) struct TransientEqIndex {
    /// Pattern variable bound by an index probe (e.g. `"pg"`).
    pub(super) bind_var: String,
    /// How to resolve the per-row probe value.
    pub(super) resolution: ProbeResolution,
    /// Built index: canonical property value ([`canonical_key`]) →
    /// matching `NodeIndex`(es).
    pub(super) by_value: HashMap<Value, Vec<NodeIndex>>,
    /// The kinds among the indexed values that a probe of another kind can
    /// equal without sharing its key.
    kinds: TextKinds,
}

/// Which cross-kind text rules the indexed values can take part in.
#[derive(Default)]
struct TextKinds {
    text: bool,
    /// Text beginning `[`: a one-element JSON list equals its string.
    bracketed: bool,
    temporal: bool,
}

/// How to read the probe value from a row.
pub(super) enum ProbeResolution {
    /// Resolved from `row.projected[var]` — pushed by the planner from
    /// `WITH x AS pnum MATCH (n {prop: pnum})` style joins.
    Projected(String),
    /// Resolved by reading `row.node_bindings[var]`'s property `prop`.
    /// Pushed from correlated `MATCH (a) MATCH (b) WHERE b.x = a.y`.
    NodeProp { var: String, prop: String },
}

impl TransientEqIndex {
    /// Try to build a transient index for `pattern`. Returns `None` when:
    /// - the pattern shape doesn't qualify (not a single typed node, more
    ///   than one matcher, etc.),
    /// - the existing-row count is below the threshold,
    /// - a persistent index already covers `(node_type, property)`,
    /// - or the type has no live nodes.
    pub(super) fn try_build(
        graph: &DirGraph,
        pattern: &Pattern,
        existing_row_count: usize,
    ) -> Option<TransientEqIndex> {
        if existing_row_count < TRANSIENT_INDEX_THRESHOLD {
            return None;
        }
        let np = extract_single_node_pattern(pattern)?;
        // Multi-label patterns (`MATCH (n:A:B)`) need a label intersection
        // the single-property eq-index can't express — fall to the matcher.
        if np.multi_label_constrained() {
            return None;
        }
        let node_type = np.node_type.as_deref()?.to_string();
        let bind_var = np.variable.as_deref()?.to_string();
        let props = np.properties.as_ref()?;
        if props.len() != 1 {
            return None;
        }
        let (property, matcher) = props.iter().next()?;
        // The `id` / `title` virtuals are node *identity*, not stored
        // properties: `resolve_node_property` maps them to `node.id()` /
        // `node.title()`, and when the id-field column was consumed as
        // identity at load the stored value is `Null`. Either way, building an
        // equality hash-index over them is wrong — it yields an empty/partial
        // map, so every probe misses and the MATCH returns nothing (the bug
        // that surfaced as `UNWIND $ids MATCH (n {id:i})` dropping all rows
        // once the list crossed the 64-row activation threshold). Identity
        // lookups already have their own fast seek path, so bail and let the
        // per-row matcher handle them.
        let resolved_prop = graph.resolve_alias(np.node_type.as_deref()?, property);
        if resolved_prop == "id" || resolved_prop == "title" {
            return None;
        }
        let resolution = match matcher {
            PropertyMatcher::EqualsVar(name) => ProbeResolution::Projected(name.clone()),
            PropertyMatcher::EqualsNodeProp { var, prop } => ProbeResolution::NodeProp {
                var: var.clone(),
                prop: prop.clone(),
            },
            _ => return None,
        };
        // Don't double-build when a persistent index already exists.
        if graph.has_any_index(&node_type, property) {
            return None;
        }
        // Union primary + secondary candidates (identical to a
        // `type_indices` clone on single-label graphs).
        let nodes = graph.nodes_with_label(&node_type);
        if nodes.is_empty() {
            return None;
        }
        let mut by_value: HashMap<Value, Vec<NodeIndex>> = HashMap::with_capacity(nodes.len());
        let mut kinds = TextKinds::default();
        for idx in nodes {
            if let Some(node) = graph.graph.node_view(idx) {
                let val = resolve_node_property(node, property, graph);
                match &val {
                    Value::Null => continue,
                    Value::Float64(f) if f.is_nan() => continue,
                    Value::List(_) | Value::Map(_) => return None,
                    Value::String(text) => {
                        kinds.text = true;
                        kinds.bracketed |= text.starts_with('[');
                    }
                    Value::DateTime(_) | Value::Timestamp(_) => kinds.temporal = true,
                    _ => {}
                }
                by_value.entry(canonical_key(val)).or_default().push(idx);
            }
        }
        Some(TransientEqIndex {
            bind_var,
            resolution,
            by_value,
            kinds,
        })
    }

    /// Resolve the probe value for this row. `None` means "no candidates":
    /// either the variable is missing or the value is null (Cypher
    /// equality with null never matches).
    pub(super) fn probe_value(&self, row: &ResultRow, graph: &DirGraph) -> Option<Value> {
        let value = match &self.resolution {
            ProbeResolution::Projected(var) => row.projected.get(var.as_str()).cloned()?,
            ProbeResolution::NodeProp { var, prop } => node_prop_reference(graph, row, var, prop)?,
        };
        if matches!(value, Value::Null) {
            None
        } else {
            Some(value)
        }
    }

    /// The nodes whose value equals `value` (empty when none do), or `None`
    /// when a key cannot answer for it and the per-row matcher must: see the
    /// module docs, per kind.
    pub(super) fn lookup(&self, value: &Value) -> Option<&[NodeIndex]> {
        let declines = match value {
            Value::String(text) => {
                self.kinds.bracketed
                    || text.starts_with('[')
                    || (self.kinds.temporal && parse_datetime_string(text).is_some())
            }
            Value::DateTime(_) | Value::Timestamp(_) => self.kinds.text,
            Value::Float64(f) if f.is_nan() => return Some(&[]),
            _ => false,
        };
        if declines {
            return None;
        }
        Some(
            self.by_value
                .get(&canonical_key(value.clone()))
                .map_or(&[], Vec::as_slice),
        )
    }
}

/// The key two values `values_equal` calls equal share, for the kinds whose
/// equality is an equivalence: one spelling per number, and a datetime at
/// midnight as its date. Every other value is its own key.
fn canonical_key(value: Value) -> Value {
    match value {
        Value::Int64(_) | Value::Float64(_) | Value::UniqueId(_) => {
            canonical_id(&value).into_owned()
        }
        Value::Timestamp(ts) if ts.time() == chrono::NaiveTime::MIN => Value::DateTime(ts.date()),
        other => other,
    }
}

/// The value `var.prop` names in an inline-map matcher (`{id: var.prop}`),
/// for this row: a bound node's property, else a projected node value's, else
/// a projected map's member — a row of `UNWIND $rows AS var`. `None` when
/// `var` is none of these. The per-row matcher and the probe of an index
/// built for it both read it here, so they cannot disagree about what the
/// reference names.
pub(super) fn node_prop_reference(
    graph: &DirGraph,
    row: &ResultRow,
    var: &str,
    prop: &str,
) -> Option<Value> {
    if let Some(node) = row
        .node_bindings
        .get(var)
        .and_then(|idx| graph.graph.node_view(*idx))
    {
        return Some(resolve_node_property(node, prop, graph));
    }
    match row.projected.get(var)? {
        Value::NodeRef(i) => graph
            .graph
            .node_view(NodeIndex::new(*i as usize))
            .map(|node| resolve_node_property(node, prop, graph)),
        Value::Node(node) => node.properties.get(prop).cloned(),
        Value::Map(members) => members.get(prop).cloned(),
        _ => None,
    }
}

fn extract_single_node_pattern(pattern: &Pattern) -> Option<&NodePattern> {
    if pattern.elements.len() != 1 {
        return None;
    }
    match &pattern.elements[0] {
        PatternElement::Node(np) => Some(np),
        _ => None,
    }
}
