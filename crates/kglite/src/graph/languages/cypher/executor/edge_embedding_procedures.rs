use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use petgraph::graph::EdgeIndex;

use crate::datatypes::values::{RelValue, Value};
use crate::graph::edge_embedding_generation::{
    embed_selected_relationships, EdgeGenerationRequest, EmbeddingExecutionService,
    SelectedEdgeText,
};
use crate::graph::edge_embeddings::vector_index::{
    build_edge_vector_index, drop_edge_vector_index, query_edge_embedding_stores,
    refresh_edge_vector_index, EdgeStoreQueryHit, EdgeVectorIndexOptions, EdgeVectorQueryOptions,
};
use crate::graph::edge_embeddings::{
    describe_relationship, drop_edge_embedding_store, remove_edge_embeddings,
    require_carried_text_property, upsert_edge_embeddings, EdgeEmbeddingWriteReport,
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
            let report = execute_set(
                graph,
                params,
                &relationship_type,
                &text_property,
                identities,
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
            reject_repeated(graph, &edges, "relationships", proc_name)?;
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
            for (position, value) in relationships.iter().enumerate() {
                let Value::Relationship(relationship) = value else {
                    return Err(format!(
                        "CALL {proc_name}: 'relationships' must contain relationships"
                    ));
                };
                let at = ListPosition("relationships", position);
                let edge =
                    resolve_rel_value(relationship, &relationship_type, identities, proc_name, at)?;
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
            let edges: Vec<_> = selected.iter().map(|item| item.edge).collect();
            reject_repeated(graph, &edges, "relationships", proc_name)?;
            if !selected.is_empty() {
                require_carried_text_property(graph, &relationship_type, &text_property)
                    .map_err(|error| format!("CALL {proc_name}: {error}"))?;
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
                optional_positive_usize(params, "batch_size", proc_name)?.unwrap_or(256);
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
) -> Result<Vec<EdgeStoreQueryHit>, String> {
    let proc_name = "db.edge_embeddings.query";
    reject_unknown_keys(
        &format!("CALL {proc_name}"),
        params.keys().map(String::as_str),
        accepted_keys(proc_name),
    )?;
    if params.contains_key("text") {
        return Err(format!(
            "CALL {proc_name}: 'text' reached execution unembedded; this execution path \
             skipped query preparation, so pass the query as 'vector'"
        ));
    }
    let text_property = require_string(params, "text_property", proc_name)?;
    let types = query_types(graph, params, &text_property, proc_name)?;
    let vector = numeric_vector(params.get("vector"), proc_name)?;
    let top_k = optional_nonnegative_usize(params, "top_k", proc_name)?.unwrap_or(10);
    let exact = optional_boolean(params, "exact", proc_name)?.unwrap_or(false);
    let metric = optional_string(params, "metric", proc_name)?;
    query_edge_embedding_stores(
        graph,
        &types,
        &text_property,
        &vector,
        EdgeVectorQueryOptions {
            top_k,
            exact,
            metric,
        },
    )
}

/// The relationship types a `query` ranks: `type` alone, the `types` list
/// (sorted, duplicates dropped), or — with neither — every type that has a
/// `text_property` store. A named type without a store is refused later, by
/// name, when its store is looked up.
fn query_types(
    graph: &DirGraph,
    params: &HashMap<String, Value>,
    text_property: &str,
    proc_name: &str,
) -> Result<Vec<String>, String> {
    let listed = match params.get("types") {
        None | Some(Value::Null) => None,
        Some(Value::List(items)) => Some(items),
        Some(value) => {
            return Err(format!(
                "CALL {proc_name}: 'types' must be a list of relationship types, got {}",
                value.type_name()
            ))
        }
    };
    let single = match params.get("type") {
        None | Some(Value::Null) => None,
        Some(_) => Some(require_string(params, "type", proc_name)?),
    };
    let mut types = match (single, listed) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "CALL {proc_name}: 'type' and 'types' are mutually exclusive; name one \
                 relationship type, or a list of them"
            ))
        }
        (Some(single), None) => vec![single],
        (None, Some(items)) => {
            let mut types = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(name) if !name.is_empty() => types.push(name.clone()),
                    other => {
                        return Err(format!(
                            "CALL {proc_name}: 'types' must hold non-empty strings, got {}",
                            other.type_name()
                        ))
                    }
                }
            }
            if types.is_empty() {
                return Err(format!(
                    "CALL {proc_name}: 'types' is empty; name at least one relationship type, \
                     or omit it to rank every '{text_property}' store"
                ));
            }
            types
        }
        (None, None) => {
            let types: Vec<String> = graph
                .edge_embeddings
                .keys()
                .filter(|(_, store_name)| {
                    crate::graph::embeddings::text_column_of(store_name) == Some(text_property)
                })
                .map(|(rel_type, _)| rel_type.clone())
                .collect();
            if types.is_empty() {
                return Err(format!(
                    "CALL {proc_name}: no relationship embedding store for text_property \
                     '{text_property}'"
                ));
            }
            types
        }
    };
    types.sort();
    types.dedup();
    Ok(types)
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
        // `text` never reaches `query`: preparation rewrites it into `vector`
        // (`planner::simplification::rewrite_text_score`). It is listed so the
        // "Accepted:" line names every spelling a caller may write.
        "db.edge_embeddings.query" => &[
            "type",
            "types",
            "text_property",
            "vector",
            "text",
            "top_k",
            "exact",
            "metric",
        ],
        other => unreachable!("non-edge-embedding procedure routed here: {other}"),
    }
}

fn execute_set(
    graph: &mut DirGraph,
    params: &HashMap<String, Value>,
    relationship_type: &str,
    text_property: &str,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
) -> Result<EdgeEmbeddingWriteReport, String> {
    let proc_name = "db.edge_embeddings.set";
    let entries = require_list(params, "entries", proc_name)?;
    let mut resolved = Vec::with_capacity(entries.len());
    for (position, entry) in entries.iter().enumerate() {
        let Value::Map(pair) = entry else {
            return Err(format!("CALL {proc_name}: each entry must be a map"));
        };
        reject_unknown_keys(
            &format!("CALL {proc_name}: entry"),
            pair.keys(),
            &["relationship", "vector"],
        )?;
        let relationship = pair
            .get("relationship")
            .ok_or_else(|| format!("CALL {proc_name}: each entry requires 'relationship'"))?;
        let at = ListPosition("entries", position);
        let edge =
            resolve_relationship(relationship, relationship_type, identities, proc_name, at)?;
        let vector = numeric_vector(pair.get("vector"), proc_name)?;
        resolved.push((edge, vector));
    }
    let edges: Vec<_> = resolved.iter().map(|(edge, _)| *edge).collect();
    reject_repeated(graph, &edges, "entries", proc_name)?;
    if !resolved.is_empty() {
        require_carried_text_property(graph, relationship_type, text_property)
            .map_err(|error| format!("CALL {proc_name}: {error}"))?;
    }
    let metric = optional_string(params, "metric", proc_name)?;
    upsert_edge_embeddings(
        graph,
        relationship_type,
        text_property,
        resolved,
        metric.as_deref(),
    )
}

/// Where a relationship sat in the list a procedure was given, as the caller
/// spelled it: `entries[2]`, `relationships[0]`.
#[derive(Clone, Copy)]
struct ListPosition(&'static str, usize);

impl std::fmt::Display for ListPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}[{}]", self.0, self.1)
    }
}

/// Refuse a relationship listed twice, naming it and both positions. The
/// storage layer refuses repeats too, but it only knows the physical slot.
fn reject_repeated(
    graph: &DirGraph,
    edges: &[EdgeIndex],
    list: &'static str,
    proc_name: &str,
) -> Result<(), String> {
    let mut first_seen = HashMap::with_capacity(edges.len());
    for (position, edge) in edges.iter().enumerate() {
        if let Some(first) = first_seen.insert(edge.index(), position) {
            return Err(format!(
                "CALL {proc_name}: relationship {} appears more than once ({} and {})",
                describe_relationship(graph, *edge),
                ListPosition(list, first),
                ListPosition(list, position)
            ));
        }
    }
    Ok(())
}

fn resolve_relationships(
    values: &[Value],
    relationship_type: &str,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
    proc_name: &str,
) -> Result<Vec<EdgeIndex>, String> {
    values
        .iter()
        .enumerate()
        .map(|(position, value)| {
            let at = ListPosition("relationships", position);
            resolve_relationship(value, relationship_type, identities, proc_name, at)
        })
        .collect()
}

fn resolve_relationship(
    value: &Value,
    relationship_type: &str,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
    proc_name: &str,
    at: ListPosition,
) -> Result<EdgeIndex, String> {
    let Value::Relationship(relationship) = value else {
        return Err(format!("CALL {proc_name}: expected a relationship value"));
    };
    resolve_rel_value(relationship, relationship_type, identities, proc_name, at)
}

fn resolve_rel_value(
    relationship: &RelValue,
    relationship_type: &str,
    identities: &Arc<Mutex<StatementRelationshipIdentities>>,
    proc_name: &str,
    at: ListPosition,
) -> Result<EdgeIndex, String> {
    if relationship.rel_type != relationship_type {
        return Err(format!(
            "CALL {proc_name}: {at} has type '{}', expected '{relationship_type}'",
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
            "CALL {proc_name}: {at} is a '{}' relationship deleted or replaced earlier in \
             this statement",
            relationship.rel_type
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

pub(super) fn require_string(
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

pub(super) fn optional_string(
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

pub(super) fn optional_nonnegative_usize(
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

pub(super) fn yield_row(values: HashMap<&str, Value>, yields: &[YieldItem]) -> ResultRow {
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
