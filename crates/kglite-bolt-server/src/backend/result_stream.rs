//! From engine rows to the Bolt result stream, and where that work runs.

use std::collections::HashMap;
use std::time::Instant;

use boltr::error::BoltError;
use boltr::server::{BoltRecord, ResultMetadata, ResultStream};
use boltr::types::{BoltDict, BoltValue};

use kglite::api::{cypher, Value};

use super::intercepts::plan_from_explain_rows;
use crate::value_adapter;

/// Decode Bolt parameters into engine values. A failure is a genuine client
/// error (bad parameter type).
pub(super) fn decode_params(
    parameters: &HashMap<String, BoltValue>,
) -> Result<HashMap<String, Value>, BoltError> {
    parameters
        .iter()
        .map(|(k, v)| value_adapter::from_bolt(v).map(|kv| (k.clone(), kv)))
        .collect()
}

/// Run CPU-bound query work without stalling the async runtime.
///
/// A query runs for as long as it needs, and the pipeline is synchronous. On
/// a worker thread it would hold up every other connection that worker serves
/// (a `RETURN 1` waited behind a 30 ms aggregate, and a new connection waited
/// for the accept task to be scheduled). `block_in_place` hands the worker's
/// queued tasks to another thread for the duration. A current-thread runtime
/// (the unit tests) has no other thread to hand them to, so the work runs in
/// place.
pub(super) fn off_async_worker<R>(work: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(work),
        _ => work(),
    }
}

/// Convert engine rows to Bolt records, consuming each row as it is converted
/// so the engine's copy shrinks while the Bolt copy grows.
fn rows_to_records(rows: Vec<Vec<Value>>) -> Result<Vec<BoltRecord>, BoltError> {
    rows.into_iter()
        .map(|row| {
            row.into_iter()
                .map(value_adapter::to_bolt_owned)
                .collect::<Result<Vec<_>, _>>()
                .map(|values| BoltRecord { values })
        })
        .collect()
}

/// Turn an engine result into the Bolt result stream: records, columns and
/// the summary (type, timing, plan, counters).
pub(super) fn finish_stream(
    result: cypher::CypherResult,
    type_str: &'static str,
    explain: bool,
    started: Instant,
) -> Result<ResultStream, BoltError> {
    let elapsed_ms = started.elapsed().as_millis() as i64;

    let mut summary = BoltDict::from([
        ("type".to_string(), BoltValue::String(type_str.to_string())),
        ("t_last".to_string(), BoltValue::Integer(elapsed_ms)),
    ]);

    // EXPLAIN follows the Bolt contract: ZERO records, and the plan in
    // the SUCCESS metadata's `plan` key. The engine answers EXPLAIN as
    // step rows (step/operation/estimated_rows); forwarding those as
    // records left every plan-tab consumer (Neo4j Browser, G.V()) blank
    // and handed drivers records where the contract promises none.
    // Retrieval execution evidence uses namespaced summary metadata;
    // no synthetic Neo4j dbHits counters are invented.
    if let Some(d) = &result.diagnostics {
        if !d.retrieval.is_empty() {
            summary.insert("kglite.retrieval".into(), retrieval_metadata(&d.retrieval));
        }
        if let Some(temporal) = &d.temporal {
            summary.insert("kglite.temporal".into(), temporal_metadata(temporal));
        }
    }
    let mut columns = result.columns;
    let rows = result.rows;
    let records = match explain
        .then(|| plan_from_explain_rows(&columns, &rows))
        .flatten()
    {
        Some(plan) => {
            summary.insert("plan".to_string(), plan);
            columns = Vec::new();
            Vec::new()
        }
        None => rows_to_records(rows)?,
    };
    if let Some(stats) = &result.stats {
        let stats_dict = BoltDict::from([
            (
                "nodes-created".to_string(),
                BoltValue::Integer(stats.nodes_created as i64),
            ),
            (
                "nodes-deleted".to_string(),
                BoltValue::Integer(stats.nodes_deleted as i64),
            ),
            (
                "relationships-created".to_string(),
                BoltValue::Integer(stats.relationships_created as i64),
            ),
            (
                "relationships-deleted".to_string(),
                BoltValue::Integer(stats.relationships_deleted as i64),
            ),
            (
                "properties-set".to_string(),
                BoltValue::Integer(stats.properties_set as i64),
            ),
        ]);
        summary.insert("stats".to_string(), BoltValue::Dict(stats_dict));
    }

    Ok(ResultStream {
        metadata: ResultMetadata {
            columns,
            extra: BoltDict::new(),
        },
        records,
        summary,
    })
}

fn temporal_metadata(echo: &kglite::api::cypher::TemporalDiagnostics) -> BoltValue {
    let text = |value: &str| BoltValue::String(value.to_string());
    BoltValue::Dict(BoltDict::from([
        ("axis".into(), text(&echo.axis)),
        ("source".into(), text(&echo.source)),
        ("instant".into(), text(&echo.instant)),
        (
            "targets".into(),
            BoltValue::List(echo.targets.iter().map(|t| text(t)).collect()),
        ),
        (
            "hidden".into(),
            BoltValue::Dict(
                echo.hidden
                    .iter()
                    .map(|(target, count)| (target.clone(), BoltValue::Integer(*count as i64)))
                    .collect(),
            ),
        ),
        (
            "endpoint_invalid".into(),
            echo.endpoint_invalid
                .map_or(BoltValue::Null, |n| BoltValue::Integer(n as i64)),
        ),
        ("route".into(), text(&echo.route)),
        (
            "retrieval".into(),
            echo.retrieval.as_deref().map_or(BoltValue::Null, text),
        ),
        ("slice".into(), BoltValue::Boolean(echo.slice)),
        (
            "session_version".into(),
            BoltValue::Integer(echo.session_version as i64),
        ),
    ]))
}

fn retrieval_metadata(records: &[kglite::api::cypher::RetrievalDiagnostics]) -> BoltValue {
    BoltValue::List(
        records
            .iter()
            .map(|r| {
                BoltValue::Dict(BoltDict::from([
                    (
                        "requested_policy".into(),
                        BoltValue::String(r.requested_policy.clone()),
                    ),
                    (
                        "actual_mode".into(),
                        BoltValue::String(r.actual_mode.clone()),
                    ),
                    (
                        "fallback_reason".into(),
                        r.fallback_reason
                            .clone()
                            .map(BoltValue::String)
                            .unwrap_or(BoltValue::Null),
                    ),
                    (
                        "store".into(),
                        r.store
                            .clone()
                            .map(BoltValue::String)
                            .unwrap_or(BoltValue::Null),
                    ),
                ]))
            })
            .collect(),
    )
}
