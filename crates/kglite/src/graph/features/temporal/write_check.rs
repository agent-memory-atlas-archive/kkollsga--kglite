//! The row check a write onto a declared type runs: the rule a declaration
//! validates its stored rows by ([`validate::check_row`]) — every bound NULL,
//! a date, a datetime or an ISO string, and no interval inverted, or empty
//! under `half_open` — applied to the rows a load or a Cypher write leaves,
//! and worded as the declaration words it. A type with no declaration costs
//! one map lookup.
//!
//! A loader row is judged before anything is written, by the values it will
//! leave under its conflict mode; a Cypher `CREATE` before its insert; a
//! Cypher `SET` once its clause has applied every item, so `SET n.vf = …,
//! n.vt = …` is judged as the pair it writes, and the statement's rollback
//! undoes a refused one.

use petgraph::graph::{EdgeIndex, NodeIndex};

use super::validate::{self, check_row};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::core::value_operations::format_value_compact;
use crate::graph::dir_graph::DirGraph;
use crate::graph::mutation::batch::ConflictHandling;
use crate::graph::schema::{InternedKey, TemporalConfig};
use crate::graph::storage::GraphRead;

/// [`check_row`] for a write, completed with the element's name.
fn judge(
    from: &Value,
    to: &Value,
    config: &TemporalConfig,
    name: impl FnOnce() -> String,
) -> Result<(), String> {
    if suspended() {
        return Ok(());
    }
    check_row(from, to, config)
        .map(|_| ())
        .map_err(|reason| format!("{}, {reason}", name()))
}

#[cfg(test)]
thread_local! {
    static SUSPENDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn suspended() -> bool {
    SUSPENDED.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn suspended() -> bool {
    false
}

/// Run `f` with the check off on this thread — the state a graph saved by
/// an earlier version, which accepted any write onto a declared type, can
/// hold. The tests of how the readers treat such rows build them here.
#[cfg(test)]
pub(crate) fn unchecked<T>(f: impl FnOnce() -> T) -> T {
    SUSPENDED.with(|flag| flag.set(true));
    let out = f();
    SUSPENDED.with(|flag| flag.set(false));
    out
}

/// The declarations governing a `rel_type` relationship from a node of
/// `source_type`: the source's keyed declaration, else every unkeyed one —
/// the declarations a declare-time walk validates that relationship under.
fn edge_configs_for<'g>(
    graph: &'g DirGraph,
    rel_type: &str,
    source_type: Option<&str>,
) -> Vec<&'g TemporalConfig> {
    let configs = graph.temporal.edges(rel_type);
    if let Some(keyed) = source_type.and_then(|source| {
        configs
            .iter()
            .find(|c| c.source_type.as_deref() == Some(source))
    }) {
        return vec![keyed];
    }
    configs.iter().filter(|c| c.source_type.is_none()).collect()
}

/// The declarations on the labels node `idx` carries: its primary type's,
/// then its secondary labels' — built only when the graph has any, so the
/// common node allocates nothing.
fn node_configs(graph: &DirGraph, idx: NodeIndex) -> impl Iterator<Item = &TemporalConfig> {
    let declared =
        |key: InternedKey| resolve(graph, key).and_then(|label| graph.temporal.node(label));
    let (primary, secondary) = if graph.temporal.has_node_declarations() {
        let primary = graph.graph.node_type_of(idx).and_then(declared);
        let secondary: Vec<&TemporalConfig> = if graph.has_secondary_labels {
            graph
                .secondary_labels(idx)
                .into_iter()
                .filter_map(declared)
                .collect()
        } else {
            Vec::new()
        };
        (primary, secondary)
    } else {
        (None, Vec::new())
    };
    primary.into_iter().chain(secondary)
}

fn resolve(graph: &DirGraph, key: InternedKey) -> Option<&str> {
    graph.interner.try_resolve(key)
}

fn node_name(graph: &DirGraph, idx: NodeIndex) -> String {
    graph
        .graph
        .get_node_id(idx)
        .map_or_else(|| "?".to_string(), |id| format_value_compact(&id))
}

fn edge_name(graph: &DirGraph, rel_type: &str, source: NodeIndex, target: NodeIndex) -> String {
    format!(
        "{rel_type} relationship from node '{}' to node '{}'",
        node_name(graph, source),
        node_name(graph, target)
    )
}

/// Whether writing `property` onto relationship type `rel_type` can move a
/// declared bound.
pub(crate) fn edge_property_is_bound(graph: &DirGraph, rel_type: &str, property: &str) -> bool {
    graph
        .temporal
        .edges(rel_type)
        .iter()
        .any(|c| c.valid_from == property || c.valid_to == property)
}

/// Refuse the stored state of node `idx` when it breaks a declaration on one
/// of its labels, naming the node as a declaration does.
pub(crate) fn check_stored_node(graph: &DirGraph, idx: NodeIndex) -> Result<(), String> {
    for config in node_configs(graph, idx) {
        let from = validate::node_bound(graph, idx, &config.valid_from);
        let to = validate::node_bound(graph, idx, &config.valid_to);
        judge(&from, &to, config, || {
            format!("node '{}'", node_name(graph, idx))
        })?;
    }
    Ok(())
}

/// Refuse the stored state of relationship `edge` when it breaks the
/// declaration governing it, naming its endpoints as a declaration does.
pub(crate) fn check_stored_edge(graph: &DirGraph, edge: EdgeIndex) -> Result<(), String> {
    let Some(rel_key) = graph.graph.edge_weight(edge).map(|e| e.connection_type) else {
        return Ok(());
    };
    let Some(rel_type) = resolve(graph, rel_key) else {
        return Ok(());
    };
    if graph.temporal.edges(rel_type).is_empty() {
        return Ok(());
    }
    let Some((source, target)) = graph.graph.edge_endpoints(edge) else {
        return Ok(());
    };
    let source_type = graph
        .graph
        .node_type_of(source)
        .and_then(|key| resolve(graph, key));
    for config in edge_configs_for(graph, rel_type, source_type) {
        let from = validate::edge_bound(graph, edge, InternedKey::from_str(&config.valid_from));
        let to = validate::edge_bound(graph, edge, InternedKey::from_str(&config.valid_to));
        judge(&from, &to, config, || {
            edge_name(graph, rel_type, source, target)
        })?;
    }
    Ok(())
}

/// Refuse a node a Cypher `CREATE` is about to insert with `labels`, when the
/// bounds `read` gives break a declaration on one of them. `id` names it.
pub(crate) fn check_new_node<'l>(
    graph: &DirGraph,
    labels: impl IntoIterator<Item = &'l str>,
    id: &Value,
    read: impl Fn(&str) -> Option<Value>,
) -> Result<(), String> {
    if !graph.temporal.has_node_declarations() {
        return Ok(());
    }
    for config in labels.into_iter().filter_map(|l| graph.temporal.node(l)) {
        let from = read(&config.valid_from).unwrap_or(Value::Null);
        let to = read(&config.valid_to).unwrap_or(Value::Null);
        judge(&from, &to, config, || {
            format!("node '{}'", format_value_compact(id))
        })?;
    }
    Ok(())
}

/// Refuse a `rel_type` relationship a Cypher `CREATE` is about to insert from
/// `source` to `target`, when the bounds `read` gives break its declaration.
pub(crate) fn check_new_edge(
    graph: &DirGraph,
    rel_type: &str,
    (source, target): (NodeIndex, NodeIndex),
    read: impl Fn(&str) -> Option<Value>,
) -> Result<(), String> {
    if graph.temporal.edges(rel_type).is_empty() {
        return Ok(());
    }
    let source_type = graph
        .graph
        .node_type_of(source)
        .and_then(|key| resolve(graph, key));
    for config in edge_configs_for(graph, rel_type, source_type) {
        let from = read(&config.valid_from).unwrap_or(Value::Null);
        let to = read(&config.valid_to).unwrap_or(Value::Null);
        judge(&from, &to, config, || {
            edge_name(graph, rel_type, source, target)
        })?;
    }
    Ok(())
}

/// The value a loaded row leaves in one bound of a node under `mode`: a new
/// node takes the row's; an existing one merges it with what it holds, as
/// the batch applies it (a NULL cell writes nothing).
fn loaded_bound(
    graph: &DirGraph,
    existing: Option<NodeIndex>,
    mode: ConflictHandling,
    property: &str,
    cell: Value,
) -> Value {
    let Some(idx) = existing else {
        return cell;
    };
    let stored = || validate::node_bound(graph, idx, property);
    match mode {
        ConflictHandling::Replace => cell,
        ConflictHandling::Update | ConflictHandling::Sum | ConflictHandling::Skip => {
            if matches!(cell, Value::Null) {
                stored()
            } else {
                cell
            }
        }
        ConflictHandling::Preserve => {
            let held = stored();
            if matches!(held, Value::Null) {
                cell
            } else {
                held
            }
        }
    }
}

/// Refuse an `add_nodes` load onto `node_type` whose rows would leave a node
/// breaking a declaration on its labels, naming the first by its position in
/// the load. Runs before the load writes; a load that carries neither bound
/// of a declaration cannot move one and is not read.
pub(crate) fn check_node_load(
    graph: &DirGraph,
    node_type: &str,
    frame: &DataFrame,
    id_column: usize,
    mode: ConflictHandling,
) -> Result<(), String> {
    if !graph.temporal.has_node_declarations() || suspended() {
        return Ok(());
    }
    let columns = |config: &TemporalConfig| {
        (
            frame.get_column_index(&config.valid_from),
            frame.get_column_index(&config.valid_to),
        )
    };
    let own = graph
        .temporal
        .node(node_type)
        .map(|config| (config, columns(config)))
        .filter(|(_, (from, to))| from.is_some() || to.is_some());
    // A secondary label an existing node carries can be declared although
    // the load's type is not.
    let secondary = graph.has_secondary_labels;
    if own.is_none() && !secondary {
        return Ok(());
    }
    let cell = |row: usize, column: Option<usize>| {
        column
            .and_then(|column| frame.get_value_by_index(row, column))
            .unwrap_or(Value::Null)
    };
    let row_name = |row: usize| move || format!("row {row} (0-based) of the load");
    for row in 0..frame.row_count() {
        let id = match frame.get_value_by_index(row, id_column) {
            Some(Value::Null) | None => continue,
            Some(id) => id,
        };
        // The row's own bounds are what it leaves unless a stored one can
        // show through — a NULL cell under a merging mode, or `preserve` —
        // so the common row costs no id lookup.
        if let (Some((config, (from, to))), false) = (own, secondary) {
            let (from, to) = (cell(row, from), cell(row, to));
            let own_bounds = match mode {
                ConflictHandling::Replace => true,
                ConflictHandling::Update | ConflictHandling::Sum => {
                    !matches!(from, Value::Null) && !matches!(to, Value::Null)
                }
                ConflictHandling::Skip | ConflictHandling::Preserve => false,
            };
            if own_bounds {
                judge(&from, &to, config, row_name(row))?;
                continue;
            }
        }
        let existing = graph.id_indices.lookup(node_type, &id);
        // `Skip` leaves an existing node as it is.
        if existing.is_some() && mode == ConflictHandling::Skip {
            continue;
        }
        let configs: Vec<&TemporalConfig> = match existing {
            Some(idx) if secondary => node_configs(graph, idx).collect(),
            _ => own.map(|(config, _)| config).into_iter().collect(),
        };
        for config in configs {
            let (from_column, to_column) = columns(config);
            if from_column.is_none() && to_column.is_none() {
                continue;
            }
            let from = loaded_bound(
                graph,
                existing,
                mode,
                &config.valid_from,
                cell(row, from_column),
            );
            let to = loaded_bound(
                graph,
                existing,
                mode,
                &config.valid_to,
                cell(row, to_column),
            );
            judge(&from, &to, config, row_name(row))?;
        }
    }
    Ok(())
}

/// Refuse an `add_relationships` load of `rel_type` from `source_type` whose
/// rows break the declaration governing them, naming the first by its
/// position in the load. A declared type's rows are versions — each is
/// stored as it is or matches an identical stored one — so the row is what
/// the relationship will hold.
pub(crate) fn check_edge_load(
    graph: &DirGraph,
    rel_type: &str,
    source_type: &str,
    frame: &DataFrame,
) -> Result<(), String> {
    if graph.temporal.edges(rel_type).is_empty() {
        return Ok(());
    }
    if suspended() {
        return Ok(());
    }
    for config in edge_configs_for(graph, rel_type, Some(source_type)) {
        validate::check_frame(frame, config)?;
    }
    Ok(())
}

/// Refuse edge rows already resolved to their endpoints — the shape
/// `add_edges_from_specs` and `create_connections` hold, row `i` of
/// `endpoints` reading `properties[i]` — when a row breaks the declaration
/// governing it, naming its endpoints.
pub(crate) fn check_edge_rows(
    graph: &DirGraph,
    rel_type: &str,
    endpoints: &[(usize, NodeIndex, NodeIndex)],
    properties: &[Vec<(InternedKey, Value)>],
) -> Result<(), String> {
    if graph.temporal.edges(rel_type).is_empty() {
        return Ok(());
    }
    for &(row, source, target) in endpoints {
        let Some(row) = properties.get(row) else {
            continue;
        };
        check_new_edge(graph, rel_type, (source, target), |property| {
            let key = InternedKey::from_str(property);
            row.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
        })?;
    }
    Ok(())
}
