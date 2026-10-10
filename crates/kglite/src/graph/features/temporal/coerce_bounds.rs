//! Coerce the ISO-string cells of a declared bound column to dates or
//! datetimes before a load stores them. A declaration accepts string bounds
//! (`validate::check_row`), so a frame whose bound columns arrive as text —
//! a reload that omits `column_types` — would otherwise write `String` beside
//! the `Date` cells already stored and re-record the property as `String`.
//! Coercion, not refusal: a column with a cell that does not parse is left as
//! it is for the row check to name.
//!
//! A Cypher write of one value onto a declared bound is coerced the same way
//! (`coerce_node_bound`, `coerce_new_node_bounds`, `coerce_edge_bound`,
//! `coerce_new_edge_bounds`): text that parses becomes a date or datetime, and
//! text that does not is left for the statement's bound check to refuse.

use super::declarations::node_names_bound;
use super::eval::{parse_instant, Instant};
use super::write_check::{ancestor_labels, edge_configs_for};
use crate::datatypes::values::{ColumnData, ColumnType, DataFrame, Value};
use crate::graph::dir_graph::DirGraph;
use crate::graph::storage::GraphRead;
use chrono::NaiveDate;
use petgraph::graph::NodeIndex;
use std::collections::HashMap;

/// Coerce the declared bound columns of an `add_nodes` frame onto `node_type`
/// (and the ancestors whose declarations govern it).
pub(crate) fn coerce_node_bounds(graph: &DirGraph, node_type: &str, frame: &mut DataFrame) {
    if !graph.temporal.has_node_declarations() {
        return;
    }
    let ancestors = ancestor_labels(graph, node_type);
    let mut columns: Vec<&str> = Vec::new();
    for name in std::iter::once(node_type).chain(ancestors) {
        if let Some(config) = graph.temporal.node(name) {
            columns.push(&config.valid_from);
            columns.push(&config.valid_to);
        }
    }
    let recorded = graph.get_node_type_metadata(node_type);
    for column in columns {
        let stored_timestamps = recorded
            .and_then(|meta| meta.get(column))
            .is_some_and(|kind| kind.eq_ignore_ascii_case("timestamp"));
        coerce_column(frame, column, stored_timestamps);
    }
}

/// Coerce the declared bound columns of an `add_relationships` frame of
/// `rel_type` from `source_type`.
pub(crate) fn coerce_edge_bounds(
    graph: &DirGraph,
    rel_type: &str,
    source_type: &str,
    frame: &mut DataFrame,
) {
    for config in edge_configs_for(graph, rel_type, Some(source_type)) {
        coerce_column(frame, &config.valid_from, false);
        coerce_column(frame, &config.valid_to, false);
    }
}

/// Replace a `String` column by a date column when every non-null cell is a
/// date, else by a datetime column (a date at its midnight), when every
/// non-null cell parses. `as_timestamps` forces the datetime column, for a
/// property the type already stores as datetimes.
fn coerce_column(frame: &mut DataFrame, column: &str, as_timestamps: bool) {
    if frame.get_column_type(column) != Some(ColumnType::String) {
        return;
    }
    let Some(index) = frame.get_column_index(column) else {
        return;
    };
    let mut instants = Vec::with_capacity(frame.row_count());
    for row in 0..frame.row_count() {
        match frame.get_value_by_index(row, index) {
            Some(Value::Null) | None => instants.push(None),
            Some(text @ Value::String(_)) => match parse_instant(&text) {
                Ok(instant) => instants.push(Some(instant)),
                Err(_) => return,
            },
            Some(_) => return,
        }
    }
    let all_dates = instants
        .iter()
        .flatten()
        .all(|instant| matches!(instant, Instant::Date(_)));
    if all_dates && !as_timestamps {
        let dates: Vec<Option<NaiveDate>> = instants.iter().map(|i| i.map(Instant::date)).collect();
        frame.replace_column(column, ColumnType::DateTime, ColumnData::DateTime(dates));
    } else {
        let stamps = instants
            .iter()
            .map(|i| {
                i.map(|instant| match instant {
                    Instant::Date(d) => d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    Instant::Timestamp(ts) => ts,
                })
            })
            .collect();
        frame.replace_column(column, ColumnType::Timestamp, ColumnData::Timestamp(stamps));
    }
}

/// The date or datetime an ISO string stands for, else `value` unchanged. A
/// date becomes a datetime (at midnight) when `as_timestamp` says the
/// property already stores datetimes.
fn coerce_value(value: Value, as_timestamp: bool) -> Value {
    if !matches!(value, Value::String(_)) {
        return value;
    }
    match parse_instant(&value) {
        Ok(Instant::Date(date)) if !as_timestamp => Value::DateTime(date),
        Ok(Instant::Date(date)) => Value::Timestamp(date.and_hms_opt(0, 0, 0).unwrap_or_default()),
        Ok(Instant::Timestamp(ts)) => Value::Timestamp(ts),
        Err(_) => value,
    }
}

fn stores_timestamps(graph: &DirGraph, node_type: &str, property: &str) -> bool {
    graph
        .get_node_type_metadata(node_type)
        .and_then(|meta| meta.get(property))
        .is_some_and(|kind| kind.eq_ignore_ascii_case("timestamp"))
}

/// Coerce `value`, about to be written to `property` of existing node `idx`,
/// when a declaration on one of the node's labels names that property a bound.
pub(crate) fn coerce_node_bound(
    graph: &DirGraph,
    idx: NodeIndex,
    property: &str,
    value: Value,
) -> Value {
    if !matches!(value, Value::String(_)) || !node_names_bound(graph, idx, property) {
        return value;
    }
    let as_timestamp = graph
        .graph
        .node_type_of(idx)
        .and_then(|key| graph.interner.try_resolve(key))
        .is_some_and(|node_type| stores_timestamps(graph, node_type, property));
    coerce_value(value, as_timestamp)
}

/// Coerce the declared bounds among `properties`, the map a Cypher `CREATE`
/// is about to insert as a `node_type` node also carrying `extra_labels`.
pub(crate) fn coerce_new_node_bounds<'l>(
    graph: &'l DirGraph,
    (node_type, extra_labels): (&'l str, impl IntoIterator<Item = &'l str>),
    properties: &mut HashMap<String, Value>,
) {
    if !graph.temporal.has_node_declarations() {
        return;
    }
    let labels: Vec<&str> = std::iter::once(node_type)
        .chain(extra_labels)
        .chain(ancestor_labels(graph, node_type))
        .collect();
    for (property, value) in properties.iter_mut() {
        if !matches!(value, Value::String(_))
            || !labels
                .iter()
                .any(|label| graph.temporal.names_node_bound(label, property))
        {
            continue;
        }
        let as_timestamp = stores_timestamps(graph, node_type, property);
        *value = coerce_value(std::mem::replace(value, Value::Null), as_timestamp);
    }
}

/// Whether a declaration governing `rel_type` from `source_type` names
/// `property` a bound.
fn edge_names_bound(
    graph: &DirGraph,
    (rel_type, source_type): (&str, Option<&str>),
    property: &str,
) -> bool {
    edge_configs_for(graph, rel_type, source_type)
        .iter()
        .any(|c| c.valid_from == property || c.valid_to == property)
}

/// Coerce `value`, about to be written to `property` of a `rel_type`
/// relationship from a `source_type` node, when a declaration names it a bound.
pub(crate) fn coerce_edge_bound(
    graph: &DirGraph,
    (rel_type, source_type): (&str, Option<&str>),
    property: &str,
    value: Value,
) -> Value {
    if !matches!(value, Value::String(_))
        || !edge_names_bound(graph, (rel_type, source_type), property)
    {
        return value;
    }
    coerce_value(value, false)
}

/// Coerce the declared bounds among `properties`, the map a Cypher `CREATE`
/// is about to insert on a `rel_type` relationship from `source_type`.
pub(crate) fn coerce_new_edge_bounds(
    graph: &DirGraph,
    (rel_type, source_type): (&str, Option<&str>),
    properties: &mut HashMap<String, Value>,
) {
    if graph.temporal.edges(rel_type).is_empty() {
        return;
    }
    for (property, value) in properties.iter_mut() {
        if matches!(value, Value::String(_))
            && edge_names_bound(graph, (rel_type, source_type), property)
        {
            *value = coerce_value(std::mem::replace(value, Value::Null), false);
        }
    }
}
