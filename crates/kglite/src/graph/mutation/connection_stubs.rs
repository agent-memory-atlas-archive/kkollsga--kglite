//! Provisional stub nodes for the missing endpoints of a relationship load.

use super::maintain::add_nodes;
use crate::datatypes::{DataFrame, Value};
use crate::graph::schema::{DirGraph, PROVISIONAL_KEY};

/// Pass B of a relationship load: vivify the missing source and target ids as
/// stubs, returning how many were created and one advisory per stub type.
pub(super) fn vivify_endpoints(
    graph: &mut DirGraph,
    connection_type: &str,
    sources: (&str, &[Value]),
    targets: (&str, &[Value]),
) -> Result<(usize, Vec<String>), String> {
    let mut by_type: Vec<(&str, usize)> = Vec::new();
    for (node_type, ids) in [sources, targets] {
        if ids.is_empty() {
            continue;
        }
        let created = vivify_stubs(graph, node_type, ids)?;
        match by_type.iter_mut().find(|(seen, _)| *seen == node_type) {
            Some((_, count)) => *count += created,
            None => by_type.push((node_type, created)),
        }
    }
    let advisories = by_type
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(node_type, count)| stub_advisory(graph, connection_type, node_type, *count))
        .collect();
    Ok((by_type.iter().map(|(_, count)| count).sum(), advisories))
}

/// The advisory for `count` stubs vivified on `node_type` — the one text every
/// binding and the blueprint build report. A stub carries no validity bounds,
/// so on a label with a valid-time declaration it is valid at every instant
/// until a node row with bounds promotes it, and the advisory says so.
fn stub_advisory(graph: &DirGraph, connection_type: &str, node_type: &str, count: usize) -> String {
    if crate::graph::features::temporal::node_config(graph, node_type).is_some() {
        format!(
            "{count} stub node(s) vivified for missing '{connection_type}' endpoints on \
             declared label '{node_type}' carry no bounds and are valid at every instant until \
             promoted (call purge_provisional() to drop any left unpromoted)."
        )
    } else {
        format!(
            "{count} stub node(s) vivified for missing '{connection_type}' endpoints of type \
             '{node_type}' — call purge_provisional() to drop any left unpromoted."
        )
    }
}

/// Auto-vivify missing edge endpoints as provisional stub nodes.
///
/// Each id in `ids` becomes a node of `node_type` carrying only its id
/// (also used as the title) and a `_provisional = true` marker. Routed
/// through `add_nodes` so a stub lands in the same storage (columnar,
/// on the disk/mapped backends) as every other node; `preserve` mode
/// makes a re-vivified id (same id missing as both a source and a
/// target on a same-type edge) a no-op. Returns the count actually
/// created.
fn vivify_stubs(graph: &mut DirGraph, node_type: &str, ids: &[Value]) -> Result<usize, String> {
    let rows: Vec<Vec<Value>> = ids
        .iter()
        .map(|id| vec![id.clone(), Value::Boolean(true)])
        .collect();
    let df =
        DataFrame::from_cypher_rows(vec!["id".to_string(), PROVISIONAL_KEY.to_string()], rows)?;
    let report = add_nodes(
        graph,
        df,
        node_type.to_string(),
        "id".to_string(),
        None,
        Some("preserve".to_string()),
    )?;
    Ok(report.nodes_created)
}
