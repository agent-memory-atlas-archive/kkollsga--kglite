use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use petgraph::graph::EdgeIndex;

use crate::datatypes::values::{RelValue, Value};
use crate::graph::edge_embedding_generation::{
    embed_selected_relationships, EdgeGenerationRequest, EmbeddingExecutionService,
    SelectedEdgeText,
};
use crate::graph::edge_embeddings::vector_index::{
    build_edge_vector_index, drop_edge_vector_index, refresh_edge_vector_index,
    EdgeVectorIndexOptions, EdgeVectorQueryOptions, EdgeVectorQueryReport,
};
use crate::graph::edge_embeddings::{
    drop_edge_embedding_store, remove_edge_embeddings, upsert_edge_embeddings,
};
use crate::graph::embeddings::EmbedMode;
use crate::graph::languages::cypher::ast::YieldItem;
use crate::graph::languages::cypher::result::ResultRow;
use crate::graph::schema::DirGraph;
use crate::graph::storage::GraphRead;

use super::procedure_params::reject_unknown_keys;
use super::relationship_identity::StatementRelationshipIdentities;

pub(super) fn execute(
    graph: &mut DirGraph,
    proc_name: &str,
    params: &HashMap<String, Value>,
    yields: &[YieldItem],
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
    service: Option<&EmbeddingExecutionService<'_>>,
) -> Result<Vec<ResultRow>, String> {
    reject_unknown_keys(
        &format!("CALL {proc_name}"),
        params.keys().map(String::as_str),
        accepted_keys(proc_name),
    )?;
    let relationship_type = require_string(params, "type", proc_name)?;
    let text_property = require_string(params, "text_property", proc_name)?;
    let values = match proc_name {
        "db.edge_embeddings.set" => {
            let entries = require_list(params, "entries", proc_name)?;
            let mut resolved = Vec::with_capacity(entries.len());
            for entry in entries {
                let Value::Map(pair) = entry else {
                    return Err(format!("CALL {proc_name}: each entry must be a map"));
                };
                reject_unknown_keys(
                    &format!("CALL {proc_name}: entry"),
                    pair.keys(),
                    &["relationship", "vector"],
                )?;
                let relationship = pair.get("relationship").ok_or_else(|| {
                    format!("CALL {proc_name}: each entry requires 'relationship'")
                })?;
                let edge =
                    resolve_relationship(relationship, &relationship_type, identities, proc_name)?;
                let vector = numeric_vector(pair.get("vector"), proc_name)?;
                resolved.push((edge, vector));
            }
            let metric = optional_string(params, "metric", proc_name)?;
            let report = upsert_edge_embeddings(
                graph,
                &relationship_type,
                &text_property,
                resolved,
                metric.as_deref(),
            )?;
            HashMap::from([
                ("stored", Value::Int64(report.stored as i64)),
                ("dimension", Value::Int64(report.dimension as i64)),
            ])
        }
        "db.edge_embeddings.remove" => {
            let relationships = require_list(params, "relationships", proc_name)?;
            let edges =
                resolve_relationships(relationships, &relationship_type, identities, proc_name)?;
            let removed =
                remove_edge_embeddings(graph, &relationship_type, &text_property, &edges)?;
            HashMap::from([("removed", Value::Int64(removed as i64))])
        }
        "db.edge_embeddings.drop" => {
            let dropped = drop_edge_embedding_store(graph, &relationship_type, &text_property)?;
            HashMap::from([("dropped", Value::Boolean(dropped))])
        }
        "db.edge_embeddings.build_index" => {
            let report = build_edge_vector_index(
                graph,
                &relationship_type,
                &text_property,
                EdgeVectorIndexOptions {
                    m: optional_positive_usize(params, "m", proc_name)?,
                    ef_construction: optional_positive_usize(params, "ef_construction", proc_name)?,
                    ef_search: optional_positive_usize(params, "ef_search", proc_name)?,
                    metric: optional_string(params, "metric", proc_name)?,
                    auto_refresh_limit: optional_nonnegative_usize(
                        params,
                        "auto_refresh_limit",
                        proc_name,
                    )?,
                },
            )?;
            HashMap::from([
                ("indexed", Value::Int64(report.indexed as i64)),
                ("metric", Value::String(report.metric)),
                ("m", Value::Int64(report.m as i64)),
            ])
        }
        "db.edge_embeddings.refresh_index" => {
            let refreshed = refresh_edge_vector_index(graph, &relationship_type, &text_property)?;
            HashMap::from([("refreshed", Value::Int64(refreshed as i64))])
        }
        "db.edge_embeddings.drop_index" => {
            let dropped = drop_edge_vector_index(graph, &relationship_type, &text_property)?;
            HashMap::from([("dropped", Value::Boolean(dropped))])
        }
        "db.edge_embeddings.embed" => {
            let relationships = require_list(params, "relationships", proc_name)?;
            let mut selected = Vec::with_capacity(relationships.len());
            for value in relationships {
                let Value::Relationship(relationship) = value else {
                    return Err(format!(
                        "CALL {proc_name}: 'relationships' must contain relationships"
                    ));
                };
                let edge =
                    resolve_rel_value(relationship, &relationship_type, identities, proc_name)?;
                let text = graph
                    .graph
                    .edge_weight(edge)
                    .and_then(|current| current.get_property(&text_property))
                    .and_then(|value| match value {
                        Value::String(text) if !text.is_empty() => Some(text.clone()),
                        _ => None,
                    });
                selected.push(SelectedEdgeText { edge, text });
            }
            let mode = match optional_string(params, "mode", proc_name)?.as_deref() {
                None | Some("missing") => EmbedMode::Missing,
                Some("changed") => EmbedMode::Changed,
                Some("all") => EmbedMode::All,
                Some(other) => {
                    return Err(format!(
                        "CALL {proc_name}: unknown mode '{other}'; use missing, changed, or all"
                    ))
                }
            };
            let batch_size =
                optional_positive_usize(params, "batch_size", proc_name)?.unwrap_or(32);
            let metric = optional_string(params, "metric", proc_name)?;
            let report = embed_selected_relationships(
                graph,
                EdgeGenerationRequest {
                    connection_type: relationship_type,
                    text_property,
                    selected,
                    mode,
                    batch_size,
                    metric,
                },
                service,
            )?;
            HashMap::from([
                ("embedded", Value::Int64(report.embedded as i64)),
                ("skipped", Value::Int64(report.skipped as i64)),
                ("dimension", Value::Int64(report.dimension as i64)),
                ("model", report.model_id.map_or(Value::Null, Value::String)),
            ])
        }
        other => unreachable!("non-edge-embedding procedure routed here: {other}"),
    };
    Ok(vec![yield_row(values, yields)])
}

pub(super) fn list(
    graph: &DirGraph,
    params: &HashMap<String, Value>,
    yields: &[YieldItem],
) -> Result<Vec<ResultRow>, String> {
    reject_unknown_keys(
        "CALL db.edge_embeddings.list",
        params.keys().map(String::as_str),
        accepted_keys("db.edge_embeddings.list"),
    )?;
    let type_filter = optional_string(params, "type", "db.edge_embeddings.list")?;
    let property_filter = optional_string(params, "text_property", "db.edge_embeddings.list")?;
    let statuses = crate::graph::edge_embeddings::vector_index::list_edge_vector_indexes(graph)
        .into_iter()
        .map(|status| {
            (
                (status.connection_type.clone(), status.text_property.clone()),
                status,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut stores: Vec<_> = graph.edge_embeddings.iter().collect();
    stores.sort_by_key(|(key, _)| (*key).clone());
    Ok(stores
        .into_iter()
        .filter(|((relationship_type, store_name), _)| {
            type_filter
                .as_ref()
                .is_none_or(|wanted| wanted == relationship_type)
                && property_filter.as_ref().is_none_or(|wanted| {
                    crate::graph::embeddings::text_column_of(store_name) == Some(wanted.as_str())
                })
        })
        .map(|((relationship_type, store_name), store)| {
            let text_property = crate::graph::embeddings::text_column_of(store_name)
                .expect("edge embedding store names carry the _emb suffix");
            let status = statuses
                .get(&(relationship_type.clone(), text_property.to_string()))
                .expect("every edge embedding store has vector-index status");
            yield_row(
                HashMap::from([
                    ("entity", Value::String("relationship".into())),
                    ("type", Value::String(relationship_type.clone())),
                    ("text_property", Value::String(text_property.to_string())),
                    ("store", Value::String(store_name.clone())),
                    ("dimension", Value::Int64(store.dimension() as i64)),
                    ("count", Value::Int64(store.len() as i64)),
                    (
                        "metric",
                        store
                            .metric()
                            .map_or(Value::String("cosine".into()), |value| {
                                Value::String(value.into())
                            }),
                    ),
                    (
                        "model",
                        store
                            .model_id()
                            .map_or(Value::Null, |value| Value::String(value.into())),
                    ),
                    (
                        "index_state",
                        Value::String(
                            if !status.built {
                                "none"
                            } else if status.stale {
                                "stale"
                            } else {
                                "online"
                            }
                            .into(),
                        ),
                    ),
                    ("delta", Value::Int64(status.delta as i64)),
                    ("unembedded", Value::Int64(status.unembedded as i64)),
                ]),
                yields,
            )
        })
        .collect())
}

pub(super) fn query(
    graph: &DirGraph,
    params: &HashMap<String, Value>,
) -> Result<EdgeVectorQueryReport, String> {
    let proc_name = "db.edge_embeddings.query";
    reject_unknown_keys(
        &format!("CALL {proc_name}"),
        params.keys().map(String::as_str),
        accepted_keys(proc_name),
    )?;
    let relationship_type = require_string(params, "type", proc_name)?;
    let text_property = require_string(params, "text_property", proc_name)?;
    let vector = numeric_vector(params.get("vector"), proc_name)?;
    let top_k = optional_nonnegative_usize(params, "top_k", proc_name)?.unwrap_or(10);
    let exact = optional_boolean(params, "exact", proc_name)?.unwrap_or(false);
    let metric = optional_string(params, "metric", proc_name)?;
    crate::graph::edge_embeddings::vector_index::query_edge_embeddings(
        graph,
        &relationship_type,
        &text_property,
        &vector,
        EdgeVectorQueryOptions {
            top_k,
            exact,
            metric,
        },
    )
}

/// Every parameter each `db.edge_embeddings.*` procedure reads, by name.
///
/// One table rather than a literal at each call site: the nine procedures share
/// `type`/`text_property` and differ only in their tails, and a list that lived
/// beside its reader is the kind that goes stale when a parameter is added two
/// functions away. The strings are also the "Accepted:" line a caller sees, so
/// they carry `type` and `text_property` even though those are required rather
/// than optional.
fn accepted_keys(proc_name: &str) -> &'static [&'static str] {
    match proc_name {
        "db.edge_embeddings.set" => &["type", "text_property", "entries", "metric"],
        "db.edge_embeddings.remove" => &["type", "text_property", "relationships"],
        "db.edge_embeddings.drop"
        | "db.edge_embeddings.refresh_index"
        | "db.edge_embeddings.drop_index"
        | "db.edge_embeddings.list" => &["type", "text_property"],
        "db.edge_embeddings.build_index" => &[
            "type",
            "text_property",
            "m",
            "ef_construction",
            "ef_search",
            "metric",
            "auto_refresh_limit",
        ],
        "db.edge_embeddings.embed" => &[
            "type",
            "text_property",
            "relationships",
            "mode",
            "batch_size",
            "metric",
        ],
        "db.edge_embeddings.query" => &[
            "type",
            "text_property",
            "vector",
            "top_k",
            "exact",
            "metric",
        ],
        other => unreachable!("non-edge-embedding procedure routed here: {other}"),
    }
}

fn resolve_relationships(
    values: &[Value],
    relationship_type: &str,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
    proc_name: &str,
) -> Result<Vec<EdgeIndex>, String> {
    values
        .iter()
        .map(|value| resolve_relationship(value, relationship_type, identities, proc_name))
        .collect()
}

fn resolve_relationship(
    value: &Value,
    relationship_type: &str,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
    proc_name: &str,
) -> Result<EdgeIndex, String> {
    let Value::Relationship(relationship) = value else {
        return Err(format!("CALL {proc_name}: expected a relationship value"));
    };
    resolve_rel_value(relationship, relationship_type, identities, proc_name)
}

fn resolve_rel_value(
    relationship: &RelValue,
    relationship_type: &str,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
    proc_name: &str,
) -> Result<EdgeIndex, String> {
    if relationship.rel_type != relationship_type {
        return Err(format!(
            "CALL {proc_name}: relationship has type '{}', expected '{relationship_type}'",
            relationship.rel_type
        ));
    }
    let edge = EdgeIndex::new(relationship.id as usize);
    let token = relationship
        .incarnation
        .ok_or_else(|| format!("CALL {proc_name}: relationship was not bound by this statement"))?;
    if !identities
        .lock()
        .map_err(|_| "relationship identity state is unavailable".to_string())?
        .accepts(edge, token)
    {
        return Err(format!(
            "CALL {proc_name}: relationship slot {} is stale",
            edge.index()
        ));
    }
    Ok(edge)
}

fn numeric_vector(value: Option<&Value>, proc_name: &str) -> Result<Vec<f32>, String> {
    let Some(Value::List(values)) = value else {
        return Err(format!("CALL {proc_name}: 'vector' must be a numeric list"));
    };
    values
        .iter()
        .map(|value| match value {
            Value::Float64(number) => Ok(*number as f32),
            Value::Int64(number) => Ok(*number as f32),
            _ => Err(format!("CALL {proc_name}: 'vector' must be a numeric list")),
        })
        .collect()
}

fn require_list<'a>(
    params: &'a HashMap<String, Value>,
    name: &str,
    proc_name: &str,
) -> Result<&'a [Value], String> {
    match params.get(name) {
        Some(Value::List(values)) => Ok(values),
        Some(value) => Err(format!(
            "CALL {proc_name}: '{name}' must be a list, got {}",
            value.type_name()
        )),
        None => Err(format!("CALL {proc_name}: missing parameter '{name}'")),
    }
}

fn require_string(
    params: &HashMap<String, Value>,
    name: &str,
    proc_name: &str,
) -> Result<String, String> {
    match params.get(name) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(value) => Err(format!(
            "CALL {proc_name}: '{name}' must be a non-empty string, got {}",
            value.type_name()
        )),
        None => Err(format!("CALL {proc_name}: missing parameter '{name}'")),
    }
}

fn optional_string(
    params: &HashMap<String, Value>,
    name: &str,
    proc_name: &str,
) -> Result<Option<String>, String> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(value) => Err(format!(
            "CALL {proc_name}: '{name}' must be a string, got {}",
            value.type_name()
        )),
    }
}

fn optional_positive_usize(
    params: &HashMap<String, Value>,
    name: &str,
    proc_name: &str,
) -> Result<Option<usize>, String> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Int64(value)) if *value > 0 => Ok(Some(*value as usize)),
        _ => Err(format!(
            "CALL {proc_name}: '{name}' must be a positive integer"
        )),
    }
}

fn optional_nonnegative_usize(
    params: &HashMap<String, Value>,
    name: &str,
    proc_name: &str,
) -> Result<Option<usize>, String> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Int64(value)) if *value >= 0 => Ok(Some(*value as usize)),
        _ => Err(format!(
            "CALL {proc_name}: '{name}' must be a non-negative integer"
        )),
    }
}

fn optional_boolean(
    params: &HashMap<String, Value>,
    name: &str,
    proc_name: &str,
) -> Result<Option<bool>, String> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Boolean(value)) => Ok(Some(*value)),
        Some(value) => Err(format!(
            "CALL {proc_name}: '{name}' must be a boolean, got {}",
            value.type_name()
        )),
    }
}

fn yield_row(values: HashMap<&str, Value>, yields: &[YieldItem]) -> ResultRow {
    let mut row = ResultRow::new();
    for item in yields {
        if let Some(value) = values.get(item.name.as_str()) {
            row.projected.insert(
                item.alias.clone().unwrap_or_else(|| item.name.clone()),
                value.clone(),
            );
        }
    }
    row
}
