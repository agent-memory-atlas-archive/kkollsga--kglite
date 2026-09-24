//! The generic `db.embeddings.*` and `db.text_index.*` routers.
//!
//! A router is dispatch only: `entity: 'node' | 'relationship'` (default
//! `'node'`) picks `db.node_*` or `db.relationship_*`, the `entity` key is
//! dropped, and the call continues as if the specific procedure had been
//! written. It runs when the parser builds the `CALL` clause, so everything
//! downstream — mutation routing, YIELD validation, the `text:` rewrite, the
//! embedder classifier — only ever sees a specific name. That is also why
//! `entity` must be a string literal: the route decides the YIELD columns
//! (`node` vs `relationship`), which are fixed before any parameter is bound.
//!
//! The router accepts the union of both entities' parameters; a key only the
//! other entity takes is refused naming that entity. A key neither takes is
//! left for the specific procedure's own unknown-key refusal, which lists what
//! it accepts.

use crate::datatypes::values::Value;
use crate::graph::languages::cypher::ast::Expression;

const EMBEDDING_OPERATIONS: [&str; 9] = [
    "set",
    "embed",
    "remove",
    "drop",
    "list",
    "build_index",
    "refresh_index",
    "drop_index",
    "query",
];
const TEXT_INDEX_OPERATIONS: [&str; 4] = ["build", "refresh", "drop", "list"];

struct Router {
    prefix: &'static str,
    node: &'static str,
    relationship: &'static str,
    operations: &'static [&'static str],
    node_keys: fn(&str) -> &'static [&'static str],
    relationship_keys: fn(&str) -> &'static [&'static str],
}

const ROUTERS: [Router; 2] = [
    Router {
        prefix: "db.embeddings.",
        node: "db.node_embeddings.",
        relationship: "db.relationship_embeddings.",
        operations: &EMBEDDING_OPERATIONS,
        node_keys: super::node_embedding_procedures::accepted_keys,
        relationship_keys: super::edge_embedding_procedures::accepted_keys,
    },
    Router {
        prefix: "db.text_index.",
        node: "db.node_text_index.",
        relationship: "db.relationship_text_index.",
        operations: &TEXT_INDEX_OPERATIONS,
        node_keys: super::node_text_index_procedures::accepted_keys,
        relationship_keys: super::edge_text_index_procedures::accepted_keys,
    },
];

/// Rewrite a router call into its specific procedure in place. Any other
/// procedure name — including an unknown router operation, which the
/// unknown-procedure error then reports — passes through untouched.
pub(crate) fn route(
    procedure_name: &mut String,
    parameters: &mut Vec<(String, Expression)>,
) -> Result<(), String> {
    let lowered = procedure_name.to_ascii_lowercase();
    let Some((router, operation)) = ROUTERS.iter().find_map(|router| {
        let operation = lowered.strip_prefix(router.prefix)?;
        router
            .operations
            .iter()
            .find(|known| **known == operation)
            .map(|known| (router, *known))
    }) else {
        return Ok(());
    };
    let display = format!("{}{operation}", router.prefix);
    let entity = match parameters.iter().position(|(key, _)| key == "entity") {
        None => "node",
        Some(at) => {
            let entity = match &parameters[at].1 {
                Expression::Literal(Value::String(entity)) if entity == "node" => "node",
                Expression::Literal(Value::String(entity)) if entity == "relationship" => {
                    "relationship"
                }
                _ => {
                    return Err(format!(
                        "CALL {display}: 'entity' must be the string literal 'node' or \
                         'relationship' (omit it for 'node'); the route is chosen before \
                         any parameter is bound"
                    ))
                }
            };
            parameters.remove(at);
            entity
        }
    };
    let (target, chosen, other, other_keys) = if entity == "node" {
        (
            router.node,
            (router.node_keys)(&format!("{}{operation}", router.node)),
            "relationship",
            (router.relationship_keys)(&format!("{}{operation}", router.relationship)),
        )
    } else {
        (
            router.relationship,
            (router.relationship_keys)(&format!("{}{operation}", router.relationship)),
            "node",
            (router.node_keys)(&format!("{}{operation}", router.node)),
        )
    };
    if let Some((key, _)) = parameters
        .iter()
        .find(|(key, _)| !chosen.contains(&key.as_str()) && other_keys.contains(&key.as_str()))
    {
        return Err(format!(
            "CALL {display}: `{key}` belongs to entity:'{other}'; this call routes to \
             entity:'{entity}' ({target}{operation})"
        ));
    }
    *procedure_name = format!("{target}{operation}");
    Ok(())
}
