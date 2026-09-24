//! `db.edge_text_index.*` — lifecycle of relationship BM25 indexes, in Cypher.
//!
//! The node text index is built from a binding (`build_text_index`); the
//! relationship lane follows the relationship *vector* lane instead and puts
//! its lifecycle in Cypher, so every binding reaches it through
//! `cypher_query` with no binding-side method. Querying needs no procedure:
//! `text_bm25(r, 'property', 'query')` scores a relationship binding or value.
//!
//! Conventions shared with `db.edge_embeddings.*`: one accepted-key table read
//! by the shared unknown-key refusal, `type` names the relationship type, a
//! drop of an absent index yields `dropped: false` rather than erroring.

use std::collections::HashMap;

use crate::datatypes::values::Value;
use crate::graph::languages::cypher::ast::YieldItem;
use crate::graph::languages::cypher::result::ResultRow;
use crate::graph::schema::DirGraph;
use crate::graph::text_indexes::edge_text::{
    build_edge_text_index, drop_edge_text_index, list_edge_text_indexes, refresh_edge_text_index,
};

use super::edge_embedding_procedures::{
    optional_nonnegative_usize, optional_string, require_string, yield_row,
};
use super::procedure_params::reject_unknown_keys;

/// The mutating procedures: build, refresh, drop.
pub(super) fn execute(
    graph: &mut DirGraph,
    proc_name: &str,
    params: &HashMap<String, Value>,
    yields: &[YieldItem],
) -> Result<Vec<ResultRow>, String> {
    reject_unknown_keys(
        &format!("CALL {proc_name}"),
        params.keys().map(String::as_str),
        accepted_keys(proc_name),
    )?;
    let rel_type = require_string(params, "type", proc_name)?;
    let property = require_string(params, "property", proc_name)?;
    let values = match proc_name {
        "db.edge_text_index.build" => {
            let limit = optional_nonnegative_usize(params, "auto_refresh_limit", proc_name)?;
            let report = build_edge_text_index(graph, &rel_type, &property, limit)
                .map_err(|error| format!("CALL {proc_name}: {error}"))?;
            HashMap::from([
                ("indexed", Value::Int64(report.indexed as i64)),
                ("skipped", Value::Int64(report.skipped as i64)),
                ("terms", Value::Int64(report.terms as i64)),
            ])
        }
        "db.edge_text_index.refresh" => {
            let refreshed =
                refresh_edge_text_index(graph, &rel_type, &property).ok_or_else(|| {
                    format!(
                        "CALL {proc_name}: no relationship text index on \
                         '{rel_type}.{property}'. Build one with CALL \
                         db.edge_text_index.build({{type: '{rel_type}', property: \
                         '{property}'}})."
                    )
                })?;
            HashMap::from([("refreshed", Value::Int64(refreshed as i64))])
        }
        "db.edge_text_index.drop" => {
            let dropped = drop_edge_text_index(graph, &rel_type, &property);
            HashMap::from([("dropped", Value::Boolean(dropped))])
        }
        other => unreachable!("non-edge-text-index procedure routed here: {other}"),
    };
    Ok(vec![yield_row(values, yields)])
}

/// `db.edge_text_index.list({type?, property?})` — one row per index, sorted.
pub(super) fn list(
    graph: &DirGraph,
    params: &HashMap<String, Value>,
    yields: &[YieldItem],
) -> Result<Vec<ResultRow>, String> {
    let proc_name = "db.edge_text_index.list";
    reject_unknown_keys(
        &format!("CALL {proc_name}"),
        params.keys().map(String::as_str),
        accepted_keys(proc_name),
    )?;
    let type_filter = optional_string(params, "type", proc_name)?;
    let property_filter = optional_string(params, "property", proc_name)?;
    Ok(list_edge_text_indexes(graph)
        .into_iter()
        .filter(|(rel_type, property, _)| {
            type_filter
                .as_deref()
                .is_none_or(|wanted| wanted == *rel_type)
                && property_filter
                    .as_deref()
                    .is_none_or(|wanted| wanted == *property)
        })
        .map(|(rel_type, property, store)| {
            let stale = store.edge_is_stale(graph);
            yield_row(
                HashMap::from([
                    ("entity", Value::String("relationship".into())),
                    ("type", Value::String(rel_type.to_string())),
                    ("property", Value::String(property.to_string())),
                    ("documents", Value::Int64(store.documents() as i64)),
                    ("terms", Value::Int64(store.terms() as i64)),
                    ("skipped", Value::Int64(store.skipped() as i64)),
                    (
                        "index_state",
                        Value::String(if stale { "stale" } else { "online" }.into()),
                    ),
                    ("delta", Value::Int64(store.edge_delta_size(graph) as i64)),
                    (
                        "auto_refresh_limit",
                        Value::Int64(store.auto_refresh_limit() as i64),
                    ),
                ]),
                yields,
            )
        })
        .collect())
}

/// Every parameter each `db.edge_text_index.*` procedure reads — also the
/// "Accepted:" line of the unknown-key refusal.
fn accepted_keys(proc_name: &str) -> &'static [&'static str] {
    match proc_name {
        "db.edge_text_index.build" => &["type", "property", "auto_refresh_limit"],
        "db.edge_text_index.refresh" | "db.edge_text_index.drop" | "db.edge_text_index.list" => {
            &["type", "property"]
        }
        other => unreachable!("non-edge-text-index procedure routed here: {other}"),
    }
}
