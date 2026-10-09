//! The public bulk loaders, each run as one transaction with respect to the
//! ontology's "must exist" rules.
//!
//! The raw loaders in [`maintain`](super::maintain), [`edge_specs`](super::edge_specs)
//! and [`extend`](super::extend) are also the building blocks of the
//! blueprint and OKF builders, which assemble a graph from many calls and
//! verify the finished graph instead. A caller that writes through the public
//! surface gets one call = one transaction: the call is written, the stored
//! end state is judged against the rules that demand something be present, and
//! a refusal restores the graph as it was. On a transaction working copy, and
//! on a graph with no enforced must-exist rule, each wrapper is one branch.

use std::collections::HashMap;

use crate::datatypes::DataFrame;
use crate::graph::introspection::reporting::{ConnectionOperationReport, NodeOperationReport};
use crate::graph::mutation::edge_specs::{self, EdgeSpec, EdgeSpecReport};
use crate::graph::mutation::extend::{self, ExtendReport};
use crate::graph::mutation::identical_rows::IdenticalRows;
use crate::graph::mutation::maintain;
use crate::graph::mutation::ontology_frame_gate::as_diagnostics;
use crate::graph::schema::{CurrentSelection, DirGraph};

/// [`maintain::add_nodes`] as one transaction.
pub fn add_nodes(
    graph: &mut DirGraph,
    df_data: DataFrame,
    node_type: String,
    unique_id_field: String,
    node_title_field: Option<String>,
    conflict_handling: Option<String>,
) -> Result<NodeOperationReport, String> {
    let (mut report, warnings) = graph.checked_bulk_write(|graph| {
        maintain::add_nodes(
            graph,
            df_data,
            node_type,
            unique_id_field,
            node_title_field,
            conflict_handling,
        )
    })?;
    report.warn_all(as_diagnostics(warnings));
    Ok(report)
}

/// [`maintain::add_connections`] as one transaction.
#[allow(clippy::too_many_arguments)]
pub fn add_connections(
    graph: &mut DirGraph,
    df_data: DataFrame,
    connection_type: String,
    source_type: String,
    source_id_field: String,
    target_type: String,
    target_id_field: String,
    source_title_field: Option<String>,
    target_title_field: Option<String>,
    conflict_handling: Option<String>,
) -> Result<ConnectionOperationReport, String> {
    add_connections_with_identical_rows(
        graph,
        df_data,
        connection_type,
        source_type,
        source_id_field,
        target_type,
        target_id_field,
        source_title_field,
        target_title_field,
        conflict_handling,
        IdenticalRows::Keep,
    )
}

/// [`maintain::add_connections_with_identical_rows`] as one transaction.
// Same argument list as add_connections plus its load options; a params struct would only re-spell it.
#[allow(clippy::too_many_arguments)]
pub fn add_connections_with_identical_rows(
    graph: &mut DirGraph,
    df_data: DataFrame,
    connection_type: String,
    source_type: String,
    source_id_field: String,
    target_type: String,
    target_id_field: String,
    source_title_field: Option<String>,
    target_title_field: Option<String>,
    conflict_handling: Option<String>,
    identical_rows: IdenticalRows,
) -> Result<ConnectionOperationReport, String> {
    let (mut report, warnings) = graph.checked_bulk_write(|graph| {
        maintain::add_connections_with_identical_rows(
            graph,
            df_data,
            connection_type,
            source_type,
            source_id_field,
            target_type,
            target_id_field,
            source_title_field,
            target_title_field,
            conflict_handling,
            identical_rows,
        )
    })?;
    report.warn_all(as_diagnostics(warnings));
    Ok(report)
}

/// [`maintain::replace_connections`] as one transaction.
#[allow(clippy::too_many_arguments)]
pub fn replace_connections(
    graph: &mut DirGraph,
    df_data: DataFrame,
    connection_type: String,
    source_type: String,
    source_id_field: String,
    target_type: String,
    target_id_field: String,
    source_title_field: Option<String>,
    target_title_field: Option<String>,
    conflict_handling: Option<String>,
) -> Result<ConnectionOperationReport, String> {
    let (mut report, warnings) = graph.checked_bulk_write(|graph| {
        maintain::replace_connections(
            graph,
            df_data,
            connection_type,
            source_type,
            source_id_field,
            target_type,
            target_id_field,
            source_title_field,
            target_title_field,
            conflict_handling,
        )
    })?;
    report.warn_all(as_diagnostics(warnings));
    Ok(report)
}

/// [`maintain::create_connections`] as one transaction.
pub fn create_connections(
    graph: &mut DirGraph,
    selection: &CurrentSelection,
    connection_type: String,
    conflict_handling: Option<String>,
    copy_properties: Option<HashMap<String, Vec<String>>>,
    source_type_filter: Option<String>,
    target_type_filter: Option<String>,
) -> Result<ConnectionOperationReport, String> {
    let (mut report, warnings) = graph.checked_bulk_write(|graph| {
        maintain::create_connections(
            graph,
            selection,
            connection_type,
            conflict_handling,
            copy_properties,
            source_type_filter,
            target_type_filter,
        )
    })?;
    report.warn_all(as_diagnostics(warnings));
    Ok(report)
}

/// [`maintain::purge_provisional_nodes`] as one transaction: a purge that
/// would leave a neighbour short of a required relationship is refused and
/// changes nothing.
pub fn purge_provisional_nodes(graph: &mut DirGraph) -> Result<(usize, usize), String> {
    graph
        .checked_bulk_write(|graph| Ok(maintain::purge_provisional_nodes(graph)))
        .map(|(counts, _warnings)| counts)
}

/// [`edge_specs::add_edges_from_specs`] as one transaction.
pub fn add_edges_from_specs(
    graph: &mut DirGraph,
    specs: Vec<EdgeSpec>,
) -> Result<EdgeSpecReport, String> {
    let (mut report, warnings) =
        graph.checked_bulk_write(|graph| edge_specs::add_edges_from_specs(graph, specs))?;
    report.diagnostics.extend(as_diagnostics(warnings.clone()));
    report.warnings.extend(warnings);
    Ok(report)
}

/// [`extend::extend_graph`] as one transaction.
pub fn extend_graph(
    target: &mut DirGraph,
    source: &DirGraph,
    conflict_handling: Option<String>,
) -> Result<ExtendReport, String> {
    let (mut report, warnings) = target
        .checked_bulk_write(|target| extend::extend_graph(target, source, conflict_handling))?;
    report.warnings.extend(warnings);
    Ok(report)
}
