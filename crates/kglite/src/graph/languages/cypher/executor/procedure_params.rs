//! Shared validation for a `CALL` procedure's config map.
//!
//! A procedure that reads its arguments out of a map cannot tell a typo from
//! an option it does not implement — both are simply absent — so the check has
//! to be a positive one: every key the caller wrote must be a key this
//! procedure knows.

/// Reject unknown keys in a procedure's config map.
///
/// A silently-ignored key is the failure mode this project has been bitten by
/// before: `{capacity_: 10}` would leave the default in place and report
/// success, and `db.relationship_embeddings.build_index({metric_: 'euclidean'})` would
/// build a cosine index and answer "indexed".
///
/// `keys` is an iterator rather than one map type because the same rule has to
/// cover a procedure's own `HashMap` parameters and a `PropMap` nested inside
/// one of them (`db.relationship_embeddings.set`'s per-entry map).
pub(super) fn reject_unknown_keys<'k>(
    proc_name: &str,
    keys: impl IntoIterator<Item = &'k str>,
    accepted: &[&str],
) -> Result<(), String> {
    for key in keys {
        if !accepted.contains(&key) {
            return Err(format!(
                "{proc_name}: unknown parameter '{key}'. Accepted: {}.",
                if accepted.is_empty() {
                    "(none — this procedure takes no parameters)".to_string()
                } else {
                    accepted.join(", ")
                }
            ));
        }
    }
    Ok(())
}

/// The optional `name` of an index-building procedure, claimed before the
/// build so a name that is taken refuses the call with nothing built. Pair with
/// [`register_built_index_name`] once the build has succeeded.
pub(super) fn claim_index_name(
    graph: &crate::graph::dir_graph::DirGraph,
    params: &std::collections::HashMap<String, crate::datatypes::values::Value>,
    proc_name: &str,
    named: &crate::graph::dir_graph::index_names::NamedIndex,
) -> Result<Option<String>, String> {
    let Some(name) = super::edge_embedding_procedures::optional_string(params, "name", proc_name)?
    else {
        return Ok(None);
    };
    crate::graph::dir_graph::index_names::check_index_name(graph, &name, named)
        .map_err(|error| format!("CALL {proc_name}: {error}"))?;
    Ok(Some(name))
}

/// Register the name [`claim_index_name`] returned for the index just built.
pub(super) fn register_built_index_name(
    graph: &mut crate::graph::dir_graph::DirGraph,
    name: Option<String>,
    named: crate::graph::dir_graph::index_names::NamedIndex,
) {
    if let Some(name) = name {
        crate::graph::dir_graph::index_names::register_index_name(graph, &name, named);
    }
}
