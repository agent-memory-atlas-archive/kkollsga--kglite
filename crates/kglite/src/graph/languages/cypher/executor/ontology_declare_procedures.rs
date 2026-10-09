//! `db.ontology.declare` / `db.ontology.clear` — the declared semantic layer
//! written from Cypher, over `DirGraph::define_ontology` / `clear_ontology`.
//!
//! ```text
//! CALL db.ontology.declare({ontology: $ontology})   // map or JSON string
//! CALL db.ontology.declare({classes: {...}, relationships: {...}, enforcement: 'error'})
//! CALL db.ontology.clear()
//! ```
//!
//! A locked graph (`DirGraph::lock_ontology`, the Bolt server's `--ontology`)
//! refuses both. A declaration that stored data already breaks at an `error`
//! rule is refused with the per-rule report as the error text and parked
//! as a typed `OntologyViolation` (code, rule, report) like a write refusal; `warn`-level
//! findings come back in the `warnings` column and on the statement's
//! warning sink.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::datatypes::values::Value;
use crate::graph::languages::cypher::ast::YieldItem;
use crate::graph::languages::cypher::result::{QueryDiagnostics, ResultRow};
use crate::graph::ontology::ontology_from_value;
use crate::graph::ontology::violation::DefineOntologyError;
use crate::graph::schema::DirGraph;

use super::edge_embedding_procedures::yield_row;
use super::procedure_params::reject_unknown_keys;

pub(super) fn execute(
    graph: &mut DirGraph,
    proc_name: &str,
    params: &HashMap<String, Value>,
    yields: &[YieldItem],
    diagnostics: &Mutex<QueryDiagnostics>,
) -> Result<Vec<ResultRow>, String> {
    let values = match proc_name {
        "db.ontology.declare" => {
            let store = ontology_from_value(&document(params)?)
                .map_err(|e| format!("CALL {proc_name}: {e}"))?;
            let warnings = match graph.define_ontology(store) {
                Ok(warnings) => warnings,
                // The report's message goes out unwrapped: the typed refusal is
                // recovered by message identity (see `PendingViolation`).
                Err(DefineOntologyError::Refused(refusal)) => {
                    return Err(graph.record_declaration_refusal(refusal));
                }
                Err(e) => return Err(format!("CALL {proc_name}: {e}")),
            };
            {
                let mut sink = diagnostics.lock().unwrap_or_else(|e| e.into_inner());
                for warning in &warnings {
                    super::retrieval_diagnostics::record_warning(
                        &mut sink.warnings,
                        warning.clone(),
                    );
                }
            }
            HashMap::from([
                ("declared", Value::Boolean(true)),
                (
                    "warnings",
                    Value::List(warnings.into_iter().map(Value::String).collect()),
                ),
            ])
        }
        "db.ontology.clear" => {
            reject_unknown_keys(
                &format!("CALL {proc_name}"),
                params.keys().map(String::as_str),
                &[],
            )?;
            let had = !graph.ontology.is_empty();
            graph
                .clear_ontology()
                .map_err(|e| format!("CALL {proc_name}: {e}"))?;
            HashMap::from([("cleared", Value::Boolean(had))])
        }
        other => unreachable!("non-ontology procedure routed here: {other}"),
    };
    Ok(vec![yield_row(values, yields)])
}

/// The declaration document: the `ontology` parameter (a map, or a JSON
/// string) or, when absent, the call's own keys read as the document.
fn document(params: &HashMap<String, Value>) -> Result<Value, String> {
    match params.get("ontology") {
        Some(Value::String(json)) => {
            if params.len() > 1 {
                return Err(
                    "CALL db.ontology.declare: pass the declaration as 'ontology' alone, or \
                     as top-level keys — not both"
                        .to_string(),
                );
            }
            let parsed: serde_json::Value = serde_json::from_str(json)
                .map_err(|e| format!("CALL db.ontology.declare: ontology JSON parse: {e}"))?;
            Ok(crate::param::json_value_to_kglite_value(&parsed))
        }
        Some(map @ Value::Map(_)) => {
            if params.len() > 1 {
                return Err(
                    "CALL db.ontology.declare: pass the declaration as 'ontology' alone, or \
                     as top-level keys — not both"
                        .to_string(),
                );
            }
            Ok(map.clone())
        }
        Some(_) => {
            Err("CALL db.ontology.declare: 'ontology' must be a map or a JSON string".to_string())
        }
        None => Ok(Value::Map(
            params.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        )),
    }
}
