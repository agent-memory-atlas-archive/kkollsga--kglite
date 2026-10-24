//! `SHOW ONTOLOGY` result set — split from `schema_ddl.rs` (file ceiling),
//! the `show_indexes` sibling-module precedent.

use super::super::result::{ResultRow, ResultSet};
use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;

/// `SHOW ONTOLOGY` — one row per declared class and relationship, plus a
/// leading `store` row when store-level settings are set. A graph
/// with no ontology returns zero rows (not an error), matching the other
/// SHOW forms' empty-inventory behaviour.
pub(crate) fn show_ontology_result_set(graph: &DirGraph) -> ResultSet {
    let mut out = ResultSet::new();
    out.columns = [
        "kind",
        "name",
        "is_a",
        "abstract",
        "domain",
        "range",
        "required_properties",
        "property_types",
        "property_values",
        "distinct",
        "enforcement",
        "exempt",
        "description",
    ]
    .iter()
    .map(|c| c.to_string())
    .collect();
    let opt = |v: &Option<String>| v.clone().map(Value::String).unwrap_or(Value::Null);
    let mut push = |cells: [(&str, Value); 13]| {
        let mut row = ResultRow::new();
        for (name, value) in cells {
            row.projected.insert(name.to_string(), value);
        }
        out.rows.push(row);
    };
    // Store-level settings (closed_labels, default severity) ride on a
    // leading `store` row, present only when one is set so ontologies that
    // use neither keep their row set.
    if let Some(summary) = graph.ontology.store_summary() {
        push([
            ("kind", Value::String("store".to_string())),
            ("name", Value::Null),
            ("is_a", Value::Null),
            ("abstract", Value::Null),
            ("domain", Value::Null),
            ("range", Value::Null),
            ("enforcement", Value::String(summary)),
            ("exempt", Value::Null),
            ("required_properties", Value::List(Vec::new())),
            ("property_types", Value::Map(Default::default())),
            ("property_values", Value::Map(Default::default())),
            ("distinct", Value::Null),
            ("description", Value::Null),
        ]);
    }
    for (name, decl) in &graph.ontology.classes {
        push([
            ("kind", Value::String("class".to_string())),
            ("name", Value::String(name.clone())),
            ("is_a", opt(&decl.is_a)),
            ("abstract", Value::Boolean(decl.is_abstract)),
            ("domain", Value::Null),
            ("range", Value::Null),
            ("enforcement", Value::String(decl.enforcement_summary())),
            ("exempt", Value::Null),
            (
                "required_properties",
                Value::List(
                    decl.required_properties
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            ),
            (
                "property_types",
                Value::Map(
                    decl.property_types
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect(),
                ),
            ),
            (
                "property_values",
                Value::Map(
                    decl.property_values
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.summary())))
                        .collect(),
                ),
            ),
            ("distinct", Value::Null),
            ("description", opt(&decl.description)),
        ]);
    }
    for (name, decl) in &graph.ontology.relationships {
        push([
            ("kind", Value::String("relationship".to_string())),
            ("name", Value::String(name.clone())),
            ("is_a", Value::Null),
            ("abstract", Value::Null),
            ("domain", opt(&decl.domain)),
            ("range", opt(&decl.range)),
            ("enforcement", Value::String(decl.enforcement_summary())),
            ("exempt", opt(&decl.exempt_summary())),
            (
                "required_properties",
                Value::List(
                    decl.required_properties
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            ),
            (
                "property_types",
                Value::Map(
                    decl.property_types
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect(),
                ),
            ),
            (
                "property_values",
                Value::Map(
                    decl.property_values
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.summary())))
                        .collect(),
                ),
            ),
            (
                "distinct",
                opt(&decl.distinct.as_ref().map(|d| d.summary())),
            ),
            ("description", opt(&decl.description)),
        ]);
    }
    out
}
