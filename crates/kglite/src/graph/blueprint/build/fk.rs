//! Phase 4: foreign-key edges declared on node CSVs, plus the implicit
//! `parent` → `OF_{PARENT}` edges, buffered or streamed.

use super::super::filter::apply_filter;
use super::super::input::InputRegistry;
use super::super::schema::OnMissingEndpoint;
use super::super::table::{MisparseTally, RawCsv};
use super::super::timeseries as ts;
use super::super::typing::map_blueprint_type;
use super::cache::{CsvCache, IdTypeCache};
use super::missing_endpoints::{
    apply_policy, is_policy_error, DroppedEndpoints, EdgeEnds, EndpointPolicy,
};
use super::nodes::{fk_id_columns, node_chunk_size, should_stream_spec};
use super::parent_link::{drop_unresolved_parents, parent_link, ParentLink, UnresolvedParents};
use super::prepass;
use super::specs::FlatSpec;
use super::table_ops::subset_rows;
use super::BuildReport;
use crate::datatypes::values::DataFrame;
use crate::graph::diagnostics::{Diagnostic, DiagnosticGroup};
use crate::graph::mutation::identical_rows::IdenticalRowTracker;
use crate::graph::mutation::maintain;
use crate::graph::schema::DirGraph;
use indexmap::IndexMap;
use std::collections::{HashMap, HashSet};

struct PreppedFkEdges {
    source_type: String,
    /// Source PK column name (which may be a synthesised `_type_id` for `pk: "auto"`).
    pk: String,
    /// Pre-built edge DataFrames, one per declared FK edge, in blueprint
    /// insertion order (critical for `skip_existence_check` parity with the
    /// old Python loader).
    edges: Vec<PreppedFkEdge>,
    /// Spec-level errors (e.g. missing FK column); surfaced after the serial
    /// consumer runs.
    errors: Vec<String>,
    /// Spec-level warnings (list cells that were probably meant as several
    /// values), surfaced alongside the errors.
    warnings: Vec<Diagnostic>,
}

/// The spec's declared FK edges plus its generated parent edge, with that
/// edge's type when one is generated (see [`parent_link`]).
fn spec_fk_edges(
    spec: &FlatSpec,
) -> (
    IndexMap<String, super::super::schema::FkEdge>,
    Option<String>,
) {
    let mut edges: IndexMap<String, super::super::schema::FkEdge> = spec
        .spec
        .connections
        .fk_edges
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut implicit = None;
    if let Some(ParentLink::Implicit { edge_type, edge }) = parent_link(spec) {
        if !edges.contains_key(&edge_type) {
            implicit = Some(edge_type.clone());
            edges.insert(edge_type, *edge);
        }
    }
    (edges, implicit)
}

/// The error for an edge whose FK column the source lacks. A generated parent
/// edge names `parent_fk`, the key the blueprint actually declares.
fn missing_fk_column_error(
    spec: &FlatSpec,
    edge_type: &str,
    fk: &str,
    implicit: &Option<String>,
) -> String {
    if implicit.as_deref() == Some(edge_type) {
        format!(
            "[{}] parent_fk column '{fk}' not found in the source CSV",
            spec.node_type
        )
    } else {
        format!(
            "[{}] FK column '{fk}' not found for edge {edge_type}",
            spec.node_type
        )
    }
}

/// One warning per spec that declares `parent_fk` under a parent with
/// `pk: "auto"`, which no column value can reference.
fn auto_parent_warnings(specs: &[&FlatSpec]) -> Vec<Diagnostic> {
    specs
        .iter()
        .filter_map(|spec| match parent_link(spec)? {
            ParentLink::AutoPkParent {
                parent_type,
                parent_fk,
            } => Some(Diagnostic::new(
                DiagnosticGroup::DataShape,
                "parent_fk_auto_pk",
                format!(
                    "[{}] parent_fk '{parent_fk}' writes no edge: '{parent_type}' has pk \
                     \"auto\", so its ids are row numbers no column value names. Declare an \
                     fk_edges entry to '{parent_type}' on a column holding those row numbers, \
                     or drop parent_fk.",
                    spec.node_type
                ),
            )),
            _ => None,
        })
        .collect()
}

struct PreppedFkEdge {
    edge_type: String,
    target_type: String,
    target_col: String,
    df: DataFrame,
    /// The generated parent edge, whose unresolvable rows are dropped.
    implicit: bool,
    fk: String,
    on_missing_endpoint: Option<OnMissingEndpoint>,
}

fn prep_fk_edges(
    spec: &FlatSpec,
    registry: &InputRegistry,
    cache: &CsvCache,
    endpoints: &EndpointIdTypes,
) -> Option<PreppedFkEdges> {
    let input = spec.input.as_deref()?;

    let (fk_edges, implicit) = spec_fk_edges(spec);
    if fk_edges.is_empty() {
        return None;
    }

    let known_types = registry
        .get(input)
        .map(|s| s.known_column_types())
        .unwrap_or_default();
    let raw_rc = cache.get(registry, input).ok()?;
    let mut raw: RawCsv = (*raw_rc).clone_raw();
    if !spec.spec.filter.is_empty() {
        apply_filter(&mut raw, &spec.spec.filter);
    }
    if let Some(tspec) = &spec.spec.timeseries {
        // The node phase already reported the drop; this pass sees the same rows.
        let _ = ts::drop_zero_time_components(&mut raw, tspec);
    }
    let raw_pk = spec.spec.pk.clone().unwrap_or_else(|| "id".to_string());
    let pk = if raw_pk == "auto" {
        let synth = format!("_{}_id", spec.node_type);
        let n = raw.row_count();
        let values: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
        raw.headers.push(synth.clone());
        for (r, row) in raw.rows.iter_mut().enumerate() {
            row.push(values[r].clone());
            raw.nulls[r].push(false);
        }
        synth
    } else {
        raw_pk
    };

    let mut built = Vec::new();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    for (edge_type, edge) in &fk_edges {
        let Some(fk_idx) = raw.col_index(&edge.fk) else {
            errors.push(missing_fk_column_error(
                spec, edge_type, &edge.fk, &implicit,
            ));
            continue;
        };
        let Some(pk_idx) = raw.col_index(&pk) else {
            errors.push(format!(
                "[{}] pk column '{}' not found for edge {}",
                spec.node_type, pk, edge_type
            ));
            continue;
        };
        let props = match fk_edge_properties(edge_type, &spec.node_type, edge, &pk) {
            Ok(mut p) => {
                super::super::typing::overlay_known_types(&mut p.declared, &known_types);
                p.endpoint_ids = endpoints.for_edge(&spec.node_type, &edge.target);
                p
            }
            Err(e) => {
                errors.push(e);
                continue;
            }
        };

        // A timeseries spec has one CSV row per time step but one node per
        // pk, so the FK repeats on every row; the edge is declared once per
        // distinct (pk, fk, property values). A changing FK keeps both targets.
        let distinct;
        let edge_raw: &RawCsv = if spec.spec.timeseries.is_some() {
            distinct = distinct_edge_rows(&raw, pk_idx, fk_idx, &props.columns);
            &distinct
        } else {
            &raw
        };

        let mut misparses = MisparseTally::default();
        let frame = match fk_edge_frame(
            edge_raw,
            &pk,
            edge,
            IdColumnIdx {
                pk: pk_idx,
                fk: fk_idx,
            },
            &props,
            &IdTypes(None),
            &mut misparses,
        ) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(e) => {
                errors.push(format!(
                    "[{}] failed to build edge DataFrame for {}: {}",
                    spec.node_type, edge_type, e
                ));
                continue;
            }
        };
        warnings.extend(misparses.into_diagnostics(&format!(
            "fk_edge '{edge_type}' (node '{}')",
            spec.node_type
        )));
        for col in &frame.missing_properties {
            errors.push(missing_fk_property_error(&spec.node_type, edge_type, col));
        }
        built.push(PreppedFkEdge {
            edge_type: edge_type.clone(),
            target_type: edge.target.clone(),
            target_col: frame.target_col,
            df: frame.df,
            implicit: implicit.as_deref() == Some(edge_type.as_str()),
            fk: edge.fk.clone(),
            on_missing_endpoint: edge.on_missing_endpoint,
        });
    }

    Some(PreppedFkEdges {
        source_type: spec.node_type.clone(),
        pk,
        edges: built,
        errors,
        warnings,
    })
}

/// `raw` reduced to its first row per distinct (pk, fk, declared property
/// values) combination, in source order. Properties absent from the CSV are
/// ignored here; `fk_edge_frame` reports them.
fn distinct_edge_rows(raw: &RawCsv, pk_idx: usize, fk_idx: usize, props: &[String]) -> RawCsv {
    let key_cols: Vec<usize> = [pk_idx, fk_idx]
        .into_iter()
        .chain(props.iter().filter_map(|c| raw.col_index(c)))
        .collect();
    let mut seen: HashSet<Vec<&str>> = HashSet::new();
    let keep: Vec<usize> = (0..raw.row_count())
        .filter(|&r| {
            let key = key_cols.iter().map(|&c| raw.rows[r][c].as_str()).collect();
            seen.insert(key)
        })
        .collect();
    subset_rows(raw, &keep)
}

fn missing_fk_property_error(node_type: &str, edge_type: &str, column: &str) -> String {
    format!(
        "[{node_type}] fk_edge {edge_type}: property column '{column}' not found in the \
         source CSV — the edge is built without it"
    )
}

/// What one FK edge attaches to each edge besides the two ids: the source
/// columns, their declared types and the name each lands under. Validated
/// once per edge, then reused for every table (chunk) the edge is built from.
struct FkEdgeProperties {
    columns: Vec<String>,
    /// Keyed by CSV column name, like a junction's `property_types` — the
    /// rename applies to the output name only.
    declared: HashMap<String, String>,
    rename: HashMap<String, String>,
    /// The source and target id columns' types where the referenced node
    /// type fixes them — see [`EndpointIdTypes`]. Wins over inference.
    endpoint_ids: (
        Option<crate::datatypes::values::ColumnType>,
        Option<crate::datatypes::values::ColumnType>,
    ),
}

/// Validate one FK edge's `properties` / `property_types` / `rename` against
/// the id columns the frame already carries. Same rules as a junction's:
/// a rename key must be a declared property, an id column is not renamable,
/// and no two columns may land under one name.
fn fk_edge_properties(
    edge_type: &str,
    node_type: &str,
    edge: &super::super::schema::FkEdge,
    pk: &str,
) -> Result<FkEdgeProperties, String> {
    let target_col = fk_target_col(pk, &edge.fk);
    let mut columns: Vec<String> = Vec::new();
    for col in &edge.properties {
        if col == pk || col == &edge.fk {
            return Err(format!(
                "[{node_type}] fk_edge {edge_type}: property '{col}' is an id column \
                 (pk '{pk}', fk '{}'); the edge already carries it",
                edge.fk
            ));
        }
        if !columns.contains(col) {
            columns.push(col.clone());
        }
    }

    let mut rename: HashMap<String, String> = HashMap::new();
    for (col, new_name) in &edge.rename {
        if col == pk || col == &edge.fk {
            return Err(format!(
                "[{node_type}] fk_edge {edge_type}: rename of fk column '{col}' is not \
                 supported — 'fk' and 'pk' name the CSV columns"
            ));
        }
        if !columns.contains(col) {
            return Err(format!(
                "[{node_type}] fk_edge {edge_type}: rename key '{col}' is not in 'properties'"
            ));
        }
        let collides = new_name == pk
            || new_name == &target_col
            || columns.iter().any(|c| c == new_name && c != col)
            || rename.values().any(|v| v == new_name);
        if collides {
            return Err(format!(
                "[{node_type}] fk_edge {edge_type}: rename target '{new_name}' collides with \
                 another column"
            ));
        }
        rename.insert(col.clone(), new_name.clone());
    }

    // An unrecognized type keyword falls through to inference, and
    // `validation::unknown_property_type_warnings` already names it.
    let declared = edge
        .property_types
        .iter()
        .filter(|(_, ty)| map_blueprint_type(ty).is_some())
        .map(|(col, ty)| (col.clone(), ty.clone()))
        .collect();
    Ok(FkEdgeProperties {
        columns,
        declared,
        rename,
        endpoint_ids: (None, None),
    })
}

/// Where this edge's two id columns sit in the table it is built from. They
/// are resolved once per (table, edge) and always travel together.
#[derive(Clone, Copy)]
struct IdColumnIdx {
    pk: usize,
    fk: usize,
}

struct FkEdgeFrame {
    target_col: String,
    df: DataFrame,
    /// Declared property columns this table does not have.
    missing_properties: Vec<String>,
}

/// One FK edge's frame, built from one raw table: the target column's name
/// plus the DataFrame `connect` consumes (source id, target id, and any
/// declared edge properties). `Ok(None)` when the table contributes no edge —
/// every FK cell in it was null.
///
/// Shared by the buffered and the streaming loader so the two cannot drift:
/// a chunk is just a shorter table, and both paths must derive an edge from
/// one the same way.
fn fk_edge_frame(
    raw: &RawCsv,
    pk: &str,
    edge: &super::super::schema::FkEdge,
    idx: IdColumnIdx,
    props: &FkEdgeProperties,
    id_types: &IdTypes<'_>,
    misparses: &mut MisparseTally,
) -> Result<Option<FkEdgeFrame>, String> {
    let cols = build_fk_columns(raw, pk, &edge.fk, idx.pk, idx.fk);
    if cols.src.is_empty() {
        return Ok(None);
    }
    let mut df = build_edge_df(pk, &cols.target_col, cols.src, cols.tgt, {
        let (src, tgt) = id_types.for_columns(pk, &edge.fk);
        let (src_fixed, tgt_fixed) = props.endpoint_ids.clone();
        (src_fixed.or(src), tgt_fixed.or(tgt))
    })?;

    let mut missing_properties = Vec::new();
    let mut present = Vec::new();
    for col in &props.columns {
        if raw.col_index(col).is_some() {
            present.push(col.clone());
        } else {
            missing_properties.push(col.clone());
        }
    }
    if !present.is_empty() {
        // Property values must follow the rows the ids came from: a row whose
        // FK was null produced no edge, and its properties must not slide onto
        // the next row's.
        let subset;
        let source: &RawCsv = if cols.rows.len() == raw.row_count() {
            raw
        } else {
            subset = subset_rows(raw, &cols.rows);
            &subset
        };
        super::super::typing::append_typed_columns(
            &mut df,
            source,
            &present,
            &props.declared,
            &props.rename,
            misparses,
        )?;
    }

    Ok(Some(FkEdgeFrame {
        target_col: cols.target_col,
        df,
        missing_properties,
    }))
}

/// The edge frame's target column name. A self-reference (`fk == pk`) needs a
/// synthesised one so the source and target columns differ.
fn fk_target_col(pk: &str, fk: &str) -> String {
    if pk == fk {
        format!("_target_{}", fk)
    } else {
        fk.to_string()
    }
}

struct FkColumns {
    target_col: String,
    src: Vec<Option<String>>,
    tgt: Vec<Option<String>>,
    /// Indices into `raw.rows` of the rows behind `src`/`tgt`, so property
    /// columns can be built from exactly the rows that produced an edge.
    rows: Vec<usize>,
}

fn build_fk_columns(raw: &RawCsv, pk: &str, fk: &str, pk_idx: usize, fk_idx: usize) -> FkColumns {
    let target_col = fk_target_col(pk, fk);
    let mut src = Vec::new();
    let mut tgt = Vec::new();
    let mut rows = Vec::new();
    // Keep only rows with a non-null target id.
    if pk == fk {
        for (r, row) in raw.rows.iter().enumerate() {
            if raw.nulls[r][pk_idx] {
                continue;
            }
            src.push(Some(row[pk_idx].clone()));
            tgt.push(Some(row[pk_idx].clone()));
            rows.push(r);
        }
    } else {
        for (r, row) in raw.rows.iter().enumerate() {
            if raw.nulls[r][fk_idx] {
                continue;
            }
            let src_val = if raw.nulls[r][pk_idx] {
                None
            } else {
                Some(row[pk_idx].clone())
            };
            src.push(src_val);
            tgt.push(Some(row[fk_idx].clone()));
            rows.push(r);
        }
    }
    FkColumns {
        target_col,
        src,
        tgt,
        rows,
    }
}

pub(super) fn load_fk_edges(
    graph: &mut DirGraph,
    specs: &[&FlatSpec],
    registry: &InputRegistry,
    cache: &CsvCache,
    id_types: &IdTypeCache,
    policy: &EndpointPolicy,
    report: &mut BuildReport,
) -> Result<(), String> {
    use rayon::prelude::*;
    let profile = std::env::var("KGLITE_BLUEPRINT_PROFILE").is_ok();
    report.add_all(auto_parent_warnings(specs));

    // Same predicate as node streaming, so a spec's nodes and FK edges
    // either both stream or both buffer. Mixing the two for one spec would
    // re-introduce the cache requirement streaming exists to drop.
    let (streamable, buffered): (Vec<&FlatSpec>, Vec<&FlatSpec>) = specs
        .iter()
        .copied()
        .partition(|s| should_stream_spec(s, registry));

    // Buffered path: parallel prep, serial connect.
    let t_par = std::time::Instant::now();
    let endpoints = EndpointIdTypes::of(graph);
    let prepped: Vec<Option<PreppedFkEdges>> = buffered
        .par_iter()
        .map(|spec| prep_fk_edges(spec, registry, cache, &endpoints))
        .collect();
    let t_par_ms = t_par.elapsed().as_millis();

    let t_serial = std::time::Instant::now();
    let mut t_connect = std::time::Duration::ZERO;
    for result in prepped {
        let Some(pfx) = result else { continue };
        for err in pfx.errors {
            report.errors.push(err);
        }
        report.add_all(pfx.warnings);
        for edge in pfx.edges {
            let mut df = edge.df;
            if edge.implicit {
                let mut tally = UnresolvedParents::default();
                df = drop_unresolved_parents(
                    graph,
                    df,
                    (&pfx.source_type, &edge.target_type),
                    (&pfx.pk, &edge.target_col),
                    &mut tally,
                )?;
                report.add_all(tally.diagnostic(
                    &pfx.source_type,
                    &edge.target_type,
                    (&edge.edge_type, &edge.fk),
                ));
            } else {
                let mut dropped = DroppedEndpoints::default();
                let ends = EdgeEnds {
                    edge_type: &edge.edge_type,
                    source: (&pfx.source_type, &pfx.pk),
                    target: (&edge.target_type, &edge.target_col),
                };
                df = apply_policy(
                    graph,
                    policy,
                    edge.on_missing_endpoint,
                    df,
                    &ends,
                    &mut dropped,
                )?;
                report.add_all(dropped.into_diagnostics(&edge.edge_type, &pfx.source_type));
            }
            let t_c = std::time::Instant::now();
            let count = connect(
                graph,
                df,
                &edge.edge_type,
                &pfx.source_type,
                &pfx.pk,
                &edge.target_type,
                &edge.target_col,
                report,
                maintain::InitialLoad::Detect,
            )?;
            t_connect += t_c.elapsed();
            *report
                .edges_by_type
                .entry(edge.edge_type.clone())
                .or_insert(0) += count;
        }
    }

    // Streaming path: same chain, one chunk at a time.
    let t_stream = std::time::Instant::now();
    for spec in &streamable {
        match load_streamed_fk_edges(graph, spec, registry, id_types, policy, report) {
            Err(e) if is_policy_error(&e) => return Err(e),
            Err(e) => report.errors.push(e),
            Ok(()) => {}
        }
    }
    let t_stream_ms = t_stream.elapsed().as_millis();

    if profile {
        eprintln!(
            "    fk parallel prep: {} ms | serial connect: {} ms | streaming ({} specs): {} ms | serial total: {} ms",
            t_par_ms,
            t_connect.as_millis(),
            streamable.len(),
            t_stream_ms,
            t_serial.elapsed().as_millis(),
        );
    }
    Ok(())
}

/// Type the edge property columns the blueprint left untyped, over the whole
/// input, and return the chunk stream to load from.
///
/// What the pre-pass leaves the streamed FK loader: the chunk stream to load
/// from, and the id type resolved for each endpoint column over the whole
/// input.
type PreparedFkChunks<'a> = (
    Box<dyn Iterator<Item = Result<RawCsv, String>> + 'a>,
    IndexMap<String, crate::datatypes::values::ColumnType>,
);

/// Inferring them per chunk would make an edge property's type depend on the
/// chunk size. One pass covers every edge — the inferred type of a column is a
/// property of the data, not of the edge that reads it — and an edge that
/// declared the column keeps its declaration.
fn resolve_fk_property_types<'a>(
    source: &'a dyn super::super::input::Source,
    chunk_size: usize,
    spec: &FlatSpec,
    id_columns: &[String],
    edge_props: &mut IndexMap<String, FkEdgeProperties>,
    report: &mut BuildReport,
) -> Result<PreparedFkChunks<'a>, String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut wanted: Vec<String> = Vec::new();
    for props in edge_props.values() {
        for col in &props.columns {
            if !props.declared.contains_key(col) && seen.insert(col.as_str()) {
                wanted.push(col.clone());
            }
        }
    }

    let filtered = !spec.spec.filter.is_empty();
    let prepared = prepass::prepare_chunks(
        source,
        chunk_size,
        &HashMap::new(),
        id_columns,
        !filtered,
        |raw| {
            if filtered {
                apply_filter(raw, &spec.spec.filter);
            }
            wanted.clone()
        },
    )
    .map_err(|e| format!("[{}] {}", spec.node_type, e))?;
    if let Some(w) = prepass::prepass_warning(
        &format!("fk_edge properties (node '{}')", spec.node_type),
        &prepared,
    ) {
        report.add(w);
    }
    for props in edge_props.values_mut() {
        for (col, keyword) in &prepared.resolved {
            if props.columns.contains(col) && !props.declared.contains_key(col) {
                props.declared.insert(col.clone(), keyword.clone());
            }
        }
    }
    Ok((prepared.chunks, prepared.resolved_ids))
}

/// Streaming FK-edge loader. Mirrors `load_streamed_node_spec`
/// row-handling, but each chunk emits one `connect()` call per declared
/// FK edge, built with the same `build_fk_columns` + `build_edge_df`
/// primitives the buffered path uses.
///
/// The auto-pk counter advances in lock-step with
/// `load_streamed_node_spec`'s counter so source ids match across
/// the node + FK phases (both apply the same filter to the same CSV
/// in the same chunk order).
fn load_streamed_fk_edges(
    graph: &mut DirGraph,
    spec: &FlatSpec,
    registry: &InputRegistry,
    id_types: &IdTypeCache,
    policy: &EndpointPolicy,
    report: &mut BuildReport,
) -> Result<(), String> {
    let Some(input) = spec.input.as_deref() else {
        return Ok(());
    };

    let (fk_edges, implicit) = spec_fk_edges(spec);
    if fk_edges.is_empty() {
        return Ok(());
    }

    let chunk_size = node_chunk_size();
    // Decided before the first chunk and reused for all of them: chunking this
    // input bounds peak RAM, so it must not decide which rows become their own
    // edge. See `maintain::InitialLoad`.
    let initial_load: HashMap<String, maintain::InitialLoad> = fk_edges
        .keys()
        .map(|edge_type| {
            let owned = maintain::source_owns_its_edges(graph, edge_type, &spec.node_type);
            (edge_type.clone(), maintain::InitialLoad::Preset(owned))
        })
        .collect();
    let source = registry
        .get(input)
        .map_err(|e| format!("[{}] {}", spec.node_type, e))?;

    let raw_pk = spec.spec.pk.clone().unwrap_or_else(|| "id".to_string());
    let (pk, is_auto_pk) = if raw_pk == "auto" {
        (format!("_{}_id", spec.node_type), true)
    } else {
        (raw_pk, false)
    };
    let mut auto_pk_counter: u64 = 1;

    // Validated once per edge, not once per chunk: a bad `rename` is a
    // property of the spec, and an edge carrying one is skipped whole rather
    // than half-built. An edge missing from this map is one such.
    let known_types = source.known_column_types();
    let endpoints = EndpointIdTypes::of(graph);
    let mut edge_props: IndexMap<String, FkEdgeProperties> = IndexMap::new();
    for (edge_type, edge) in &fk_edges {
        match fk_edge_properties(edge_type, &spec.node_type, edge, &pk) {
            Ok(mut props) => {
                super::super::typing::overlay_known_types(&mut props.declared, &known_types);
                props.endpoint_ids = endpoints.for_edge(&spec.node_type, &edge.target);
                edge_props.insert(edge_type.clone(), props);
            }
            Err(e) => report.errors.push(e),
        }
    }

    // The endpoint columns of every edge, plus the source pk: their id type is
    // resolved over the whole input, because deciding it per chunk splits one
    // logical id space into an `Int64` half and a `String` half, and the half
    // that does not match the target type's ids vivifies a duplicate stub node
    // per value instead of finding the node that is already there.
    //
    // The node phase streamed this input first and published the answer, so
    // the common case costs no read at all here.
    let id_columns = fk_id_columns(spec, &pk);
    let known = id_types.get(input, &id_columns);
    let (chunks, resolved_ids) = resolve_fk_property_types(
        source,
        chunk_size,
        spec,
        if known.is_some() { &[] } else { &id_columns },
        &mut edge_props,
        report,
    )?;
    let resolved_ids = known.unwrap_or(resolved_ids);

    // Track per-edge missing-column errors so we report each at most
    // once instead of once per chunk.
    let mut reported_missing_fk: HashSet<String> = HashSet::new();
    let mut reported_missing_pk: HashSet<String> = HashSet::new();
    let mut reported_missing_prop: HashSet<(String, String)> = HashSet::new();
    // One tally per edge across every chunk of this CSV — the junction
    // loader's reason applies here too.
    let mut misparses: IndexMap<String, MisparseTally> = IndexMap::new();
    let mut unresolved = UnresolvedParents::default();
    let mut dropped: IndexMap<String, DroppedEndpoints> = IndexMap::new();

    for chunk_result in chunks {
        let mut raw = chunk_result.map_err(|e| format!("[{}] {}", spec.node_type, e))?;
        if !spec.spec.filter.is_empty() {
            apply_filter(&mut raw, &spec.spec.filter);
        }
        if raw.row_count() == 0 {
            continue;
        }
        if is_auto_pk {
            raw.headers.push(pk.clone());
            for r in 0..raw.row_count() {
                raw.rows[r].push(auto_pk_counter.to_string());
                raw.nulls[r].push(false);
                auto_pk_counter += 1;
            }
        }

        let Some(pk_idx) = raw.col_index(&pk) else {
            for edge_type in fk_edges.keys() {
                if reported_missing_pk.insert(edge_type.clone()) {
                    report.errors.push(format!(
                        "[{}] pk column '{}' not found for edge {}",
                        spec.node_type, pk, edge_type
                    ));
                }
            }
            continue;
        };

        for (edge_type, edge) in &fk_edges {
            let Some(props) = edge_props.get(edge_type) else {
                continue;
            };
            let Some(fk_idx) = raw.col_index(&edge.fk) else {
                if reported_missing_fk.insert(edge_type.clone()) {
                    report.errors.push(missing_fk_column_error(
                        spec, edge_type, &edge.fk, &implicit,
                    ));
                }
                continue;
            };
            let tally = misparses.entry(edge_type.clone()).or_default();
            let frame = match fk_edge_frame(
                &raw,
                &pk,
                edge,
                IdColumnIdx {
                    pk: pk_idx,
                    fk: fk_idx,
                },
                props,
                &IdTypes(Some(&resolved_ids)),
                tally,
            ) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(e) => {
                    report.errors.push(format!(
                        "[{}] failed to build edge DataFrame for {}: {}",
                        spec.node_type, edge_type, e
                    ));
                    continue;
                }
            };
            for col in &frame.missing_properties {
                if reported_missing_prop.insert((edge_type.clone(), col.clone())) {
                    report
                        .errors
                        .push(missing_fk_property_error(&spec.node_type, edge_type, col));
                }
            }
            let (target_col, mut df) = (frame.target_col, frame.df);
            if implicit.as_deref() == Some(edge_type.as_str()) {
                df = drop_unresolved_parents(
                    graph,
                    df,
                    (&spec.node_type, &edge.target),
                    (&pk, &target_col),
                    &mut unresolved,
                )?;
            } else {
                let ends = EdgeEnds {
                    edge_type,
                    source: (&spec.node_type, &pk),
                    target: (&edge.target, &target_col),
                };
                let tally = dropped.entry(edge_type.clone()).or_default();
                df = apply_policy(graph, policy, edge.on_missing_endpoint, df, &ends, tally)?;
            }
            let count = connect(
                graph,
                df,
                edge_type,
                &spec.node_type,
                &pk,
                &edge.target,
                &target_col,
                report,
                initial_load[edge_type],
            )?;
            *report.edges_by_type.entry(edge_type.clone()).or_insert(0) += count;
        }
    }
    report_streamed_findings(
        report,
        spec,
        (&fk_edges, &implicit),
        (misparses, unresolved, dropped),
    );
    Ok(())
}

/// The advisories a streamed spec accumulated across its chunks, one per
/// edge: unparsed cells, rows dropped for a missing endpoint, and rows whose
/// `parent_fk` matched no parent.
fn report_streamed_findings(
    report: &mut BuildReport,
    spec: &FlatSpec,
    (fk_edges, implicit): (
        &IndexMap<String, super::super::schema::FkEdge>,
        &Option<String>,
    ),
    (misparses, unresolved, dropped): (
        IndexMap<String, MisparseTally>,
        UnresolvedParents,
        IndexMap<String, DroppedEndpoints>,
    ),
) {
    for (edge_type, tally) in misparses {
        report.add_all(tally.into_diagnostics(&format!(
            "fk_edge '{edge_type}' (node '{}')",
            spec.node_type
        )));
    }
    for (edge_type, tally) in dropped {
        report.add_all(tally.into_diagnostics(&edge_type, &spec.node_type));
    }
    if let Some(edge_type) = implicit {
        if let Some(edge) = fk_edges.get(edge_type) {
            report.add_all(unresolved.diagnostic(
                &spec.node_type,
                &edge.target,
                (edge_type, &edge.fk),
            ));
        }
    }
}

/// The node types whose nodes the graph keys by string ids, read after the
/// node phase. An id column referring to one of them must be typed string: a
/// blueprint that declares `pk` as `"string"` keeps `"0001"` on the node, and
/// an edge column left to inference read the same cell as the integer 1, found
/// no node, and vivified a stub for every row.
pub(super) struct EndpointIdTypes(HashSet<String>);

impl EndpointIdTypes {
    pub(super) fn of(graph: &DirGraph) -> Self {
        use crate::graph::storage::GraphRead;
        Self(
            graph
                .type_indices
                .iter()
                .filter(|(_, nodes)| {
                    nodes
                        .iter()
                        .next()
                        .and_then(|idx| graph.graph.get_node_id(idx))
                        .is_some_and(|id| matches!(id, crate::datatypes::values::Value::String(_)))
                })
                .map(|(node_type, _)| node_type.to_string())
                .collect(),
        )
    }

    /// The type an id column referring to `node_type` must take, if fixed.
    pub(super) fn for_type(&self, node_type: &str) -> Option<crate::datatypes::values::ColumnType> {
        self.0
            .contains(node_type)
            .then_some(crate::datatypes::values::ColumnType::String)
    }

    fn for_edge(
        &self,
        source_type: &str,
        target_type: &str,
    ) -> (
        Option<crate::datatypes::values::ColumnType>,
        Option<crate::datatypes::values::ColumnType>,
    ) {
        (self.for_type(source_type), self.for_type(target_type))
    }
}

/// Id types resolved ahead of the frame, when the caller reads its input in
/// chunks. `None` means "infer from the values in hand", which is what the
/// buffered path does — there the values in hand are the whole column.
struct IdTypes<'a>(Option<&'a IndexMap<String, crate::datatypes::values::ColumnType>>);

impl IdTypes<'_> {
    fn for_columns(
        &self,
        src: &str,
        tgt: &str,
    ) -> (
        Option<crate::datatypes::values::ColumnType>,
        Option<crate::datatypes::values::ColumnType>,
    ) {
        match self.0 {
            Some(map) => (map.get(src).cloned(), map.get(tgt).cloned()),
            None => (None, None),
        }
    }
}

fn build_edge_df(
    src_name: &str,
    tgt_name: &str,
    src: Vec<Option<String>>,
    tgt: Vec<Option<String>>,
    resolved: (
        Option<crate::datatypes::values::ColumnType>,
        Option<crate::datatypes::values::ColumnType>,
    ),
) -> Result<DataFrame, String> {
    // Decide column types: try i64, fall back to string.
    let src_type = resolved.0.unwrap_or_else(|| infer_id_type(&src));
    let tgt_type = resolved.1.unwrap_or_else(|| infer_id_type(&tgt));
    let mut df = DataFrame::new(Vec::new());
    add_id_column(&mut df, src_name, src, src_type)?;
    add_id_column(&mut df, tgt_name, tgt, tgt_type)?;
    Ok(df)
}

pub(super) fn infer_id_type(vals: &[Option<String>]) -> crate::datatypes::values::ColumnType {
    let mut inference = super::super::typing::IdInference::default();
    for v in vals {
        if inference.is_settled() {
            break;
        }
        if let Some(s) = v {
            inference.observe(s);
        }
    }
    inference.resolve()
}

pub(super) fn add_id_column(
    df: &mut DataFrame,
    name: &str,
    vals: Vec<Option<String>>,
    col_type: crate::datatypes::values::ColumnType,
) -> Result<(), String> {
    use crate::datatypes::values::{ColumnData, ColumnType};
    let data = match col_type {
        ColumnType::Int64 => {
            let ints: Vec<Option<i64>> = vals
                .iter()
                .map(|v| {
                    v.as_ref().and_then(|s| {
                        let t = s.trim();
                        if t.is_empty() {
                            None
                        } else if let Ok(i) = t.parse::<i64>() {
                            Some(i)
                        } else if let Ok(f) = t.parse::<f64>() {
                            if f.is_finite()
                                && f.fract() == 0.0
                                && f >= i64::MIN as f64
                                && f <= i64::MAX as f64
                            {
                                Some(f as i64)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                })
                .collect();
            ColumnData::Int64(ints)
        }
        _ => ColumnData::String(
            vals.into_iter()
                .map(|v| v.filter(|s| !s.is_empty()))
                .collect(),
        ),
    };
    df.add_column(name.to_string(), col_type, data)
}

// Thin adapter: the parameter list mirrors `add_connections_with_initial_load`.
#[allow(clippy::too_many_arguments)]
pub(super) fn connect(
    graph: &mut DirGraph,
    df: DataFrame,
    connection_type: &str,
    source_type: &str,
    source_id_field: &str,
    target_type: &str,
    target_id_field: &str,
    report: &mut BuildReport,
    initial_load: maintain::InitialLoad,
) -> Result<usize, String> {
    connect_tracked(
        graph,
        df,
        (connection_type, source_type, target_type),
        (source_id_field, target_id_field),
        report,
        initial_load,
        &mut IdenticalRowTracker::off(),
    )
}

/// [`connect`] with the identical-row state of a chunked load, which the
/// caller reports once its last chunk has landed.
pub(super) fn connect_tracked(
    graph: &mut DirGraph,
    df: DataFrame,
    (connection_type, source_type, target_type): (&str, &str, &str),
    (source_id_field, target_id_field): (&str, &str),
    report: &mut BuildReport,
    initial_load: maintain::InitialLoad,
    tracker: &mut IdenticalRowTracker,
) -> Result<usize, String> {
    match maintain::add_connections_tracked(
        graph,
        df,
        connection_type.to_string(),
        source_type.to_string(),
        source_id_field.to_string(),
        target_type.to_string(),
        target_id_field.to_string(),
        None,
        None,
        None,
        initial_load,
        tracker,
    ) {
        Ok(r) => {
            if r.connections_skipped > 0 {
                let detail = r.errors.join("; ");
                report.add(Diagnostic::new(
                    DiagnosticGroup::DataQuality,
                    "edge_rows_skipped",
                    format!(
                        "[{}] -[{}]-> {}: {} skipped ({})",
                        source_type, connection_type, target_type, r.connections_skipped, detail
                    ),
                ));
            }
            // The loader's own advisories — stub vivification, empty
            // intervals — under this edge's heading.
            let heading = format!("[{source_type}] -[{connection_type}]-> {target_type}: ");
            report.add_all(r.diagnostics.into_iter().map(|d| d.prefixed(&heading)));
            // Rows that landed, merged ones included: the summary compares
            // this input count with the stored edges to report dedupes.
            Ok(r.connections_created + r.connections_updated)
        }
        Err(e) => {
            report
                .errors
                .push(format!("[{}] edge {}: {}", source_type, connection_type, e));
            Ok(0)
        }
    }
}
