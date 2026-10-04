//! What a blueprint edge load does with a row whose endpoint no node row
//! supplied.
//!
//! The default (`auto`) decides per endpoint type: a type the build declares
//! valid-time on drops the row, because a stub node of it carries no bounds and
//! is valid at every instant, so it would count in every default-today read; a
//! type the build does not declare keeps getting a provisional stub, as it
//! always did. `on_missing_endpoint` on an `fk_edges` / `junction_edges` entry,
//! or under `settings`, picks `vivify`, `drop` or `error` instead.
//!
//! Dropping happens on the edge frame before it reaches `add_connections`, so
//! the engine's own vivify-everything behaviour (and the Python
//! `add_connections` default) is untouched, and a dropped row vivifies no stub
//! for its other endpoint either.

use super::super::schema::{Blueprint, OnMissingEndpoint};
use crate::datatypes::values::{DataFrame, Value};
use crate::graph::diagnostics::{Diagnostic, DiagnosticGroup};
use crate::graph::schema::DirGraph;
use crate::graph::storage::lookups::EndpointResolver;
use std::collections::{BTreeMap, HashSet};

/// The action for one endpoint type once `auto` is resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    Vivify,
    Drop,
    Error,
}

/// Start of the message of a failure under `on_missing_endpoint: "error"`;
/// the streamed loaders swallow per-spec load errors but must not swallow it.
const POLICY_ERROR_PREFIX: &str = "on_missing_endpoint 'error': ";

/// Whether `message` is a failure raised by [`Mode::Error`].
pub(super) fn is_policy_error(message: &str) -> bool {
    message.starts_with(POLICY_ERROR_PREFIX)
}

/// The build-wide inputs to the decision: which labels the build declares
/// valid-time on, known before any row loads, and the `settings` default.
pub(super) struct EndpointPolicy {
    declared: HashSet<String>,
    default: OnMissingEndpoint,
}

impl EndpointPolicy {
    /// The labels the blueprint's specs and its manifest declare.
    pub(super) fn new(blueprint: &Blueprint, blueprint_dir: &std::path::Path) -> Self {
        let mut declared: HashSet<String> = super::temporal::declared_node_labels(blueprint)
            .into_iter()
            .collect();
        // An unreadable manifest is the build's error to report at its end;
        // here it only means it names no extra labels.
        if let Some(manifest) = &blueprint.settings.manifest {
            if let Some(parsed) = std::fs::read_to_string(blueprint_dir.join(manifest))
                .ok()
                .and_then(|text| crate::graph::io::export::ExportManifest::from_json(&text).ok())
            {
                declared.extend(
                    parsed
                        .temporal
                        .iter()
                        .filter(|entry| entry.target == "node")
                        .map(|entry| entry.name.clone()),
                );
            }
        }
        Self {
            declared,
            default: blueprint.settings.on_missing_endpoint.unwrap_or_default(),
        }
    }

    fn is_declared(&self, graph: &DirGraph, label: &str) -> bool {
        self.declared.contains(label)
            || crate::graph::features::temporal::node_config(graph, label).is_some()
    }

    /// The action for missing endpoints of `label` on an edge whose own setting
    /// is `explicit`.
    pub(super) fn mode(
        &self,
        graph: &DirGraph,
        explicit: Option<OnMissingEndpoint>,
        label: &str,
    ) -> Mode {
        match explicit.unwrap_or(self.default) {
            OnMissingEndpoint::Vivify => Mode::Vivify,
            OnMissingEndpoint::Drop => Mode::Drop,
            OnMissingEndpoint::Error => Mode::Error,
            OnMissingEndpoint::Auto if self.is_declared(graph, label) => Mode::Drop,
            OnMissingEndpoint::Auto => Mode::Vivify,
        }
    }

    /// The `(source, target)` actions of one edge.
    pub(super) fn modes(
        &self,
        graph: &DirGraph,
        explicit: Option<OnMissingEndpoint>,
        (source_type, target_type): (&str, &str),
    ) -> (Mode, Mode) {
        (
            self.mode(graph, explicit, source_type),
            self.mode(graph, explicit, target_type),
        )
    }

    /// Whether dropped endpoints of `label` are ones the build declares.
    fn declared_label(&self, graph: &DirGraph, label: &str) -> bool {
        self.is_declared(graph, label)
    }
}

#[derive(Default)]
struct TypeTally {
    declared: bool,
    rows: usize,
    ids: HashSet<Value>,
    example: Option<Value>,
}

/// The rows one edge input had dropped, per endpoint type, across all its
/// chunks.
#[derive(Default)]
pub(super) struct DroppedEndpoints {
    by_type: BTreeMap<String, TypeTally>,
}

impl DroppedEndpoints {
    /// One advisory per endpoint type that lost rows.
    pub(super) fn into_diagnostics(self, edge_type: &str, source_type: &str) -> Vec<Diagnostic> {
        self.by_type
            .into_iter()
            .map(|(endpoint_type, tally)| {
                let example = tally
                    .example
                    .map(|v| format!(" (e.g. {v})"))
                    .unwrap_or_default();
                let (kind, why) = if tally.declared {
                    (
                        "endpoints_dropped_declared",
                        format!(
                            "'{endpoint_type}' is declared valid-time and a stub of it would be \
                             valid at every instant; set on_missing_endpoint to \"vivify\" to \
                             create stubs anyway."
                        ),
                    )
                } else {
                    (
                        "endpoints_dropped",
                        "on_missing_endpoint is \"drop\".".to_string(),
                    )
                };
                Diagnostic::new(
                    DiagnosticGroup::Stubs,
                    kind,
                    format!(
                        "[{source_type}] -[{edge_type}]-> {endpoint_type}: dropped {} row(s) \
                         naming {} '{endpoint_type}' id(s) no row supplies{example} — no edge, no \
                         stub node. {why}",
                        tally.rows,
                        tally.ids.len(),
                    ),
                )
            })
            .collect()
    }
}

/// The columns and types of the edge a frame carries.
pub(super) struct EdgeEnds<'a> {
    pub(super) edge_type: &'a str,
    pub(super) source: (&'a str, &'a str),
    pub(super) target: (&'a str, &'a str),
}

/// `df` without the rows whose endpoint is missing and whose [`Mode`] (from
/// `explicit` or the policy, per endpoint type) is not `Vivify`, the dropped
/// rows tallied. `Mode::Error` fails on the first missing endpoint instead. A
/// frame whose modes are both `Vivify` is returned untouched, without a
/// lookup.
///
/// `source` / `target` pair each side's node type with its id column.
pub(super) fn apply_policy(
    graph: &DirGraph,
    policy: &EndpointPolicy,
    explicit: Option<OnMissingEndpoint>,
    df: DataFrame,
    ends: &EdgeEnds<'_>,
    tally: &mut DroppedEndpoints,
) -> Result<DataFrame, String> {
    let modes = policy.modes(graph, explicit, (ends.source.0, ends.target.0));
    if modes == (Mode::Vivify, Mode::Vivify) {
        return Ok(df);
    }
    let ((source_type, source_col), (target_type, target_col)) = (ends.source, ends.target);
    let resolver = EndpointResolver::new(
        &graph.id_indices,
        &graph.graph,
        source_type.to_string(),
        target_type.to_string(),
    )
    .ok();
    let found = |is_source: bool, id: &Value| {
        resolver.as_ref().is_some_and(|r| {
            if is_source {
                r.check_source(id).is_some()
            } else {
                r.check_target(id).is_some()
            }
        })
    };
    let rows = df.row_count();
    let cell = |col: &str, r: usize| df.get_value(r, col).filter(|v| *v != Value::Null);
    let mut keep: Vec<usize> = Vec::with_capacity(rows);
    for r in 0..rows {
        let mut missing: Vec<(&str, Mode, Value)> = Vec::new();
        for (is_source, ty, col, mode) in [
            (true, source_type, source_col, modes.0),
            (false, target_type, target_col, modes.1),
        ] {
            if mode == Mode::Vivify {
                continue;
            }
            if let Some(id) = cell(col, r) {
                if !found(is_source, &id) {
                    missing.push((ty, mode, id));
                }
            }
        }
        if missing.is_empty() {
            keep.push(r);
            continue;
        }
        for (ty, mode, id) in missing {
            if mode == Mode::Error {
                return Err(format!(
                    "{POLICY_ERROR_PREFIX}[{source_type}] -[{}]-> {target_type}: row {r} names \
                     {ty} id {id}, which no row supplies. Fix the input, or set \
                     on_missing_endpoint to \"drop\" or \"vivify\" for this edge.",
                    ends.edge_type
                ));
            }
            let entry = tally.by_type.entry(ty.to_string()).or_default();
            entry.declared = policy.declared_label(graph, ty);
            entry.rows += 1;
            entry.ids.insert(id.clone());
            entry.example.get_or_insert(id);
        }
    }
    if keep.len() == rows {
        return Ok(df);
    }
    Ok(df.select_rows(&keep))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(declared: &[&str], default: OnMissingEndpoint) -> EndpointPolicy {
        EndpointPolicy {
            declared: declared.iter().map(|s| s.to_string()).collect(),
            default,
        }
    }

    #[test]
    fn auto_drops_declared_types_and_vivifies_the_rest() {
        let graph = DirGraph::new();
        let p = policy(&["Department"], OnMissingEndpoint::Auto);
        assert_eq!(p.mode(&graph, None, "Department"), Mode::Drop);
        assert_eq!(p.mode(&graph, None, "Team"), Mode::Vivify);
        assert_eq!(
            p.modes(&graph, None, ("Team", "Department")),
            (Mode::Vivify, Mode::Drop)
        );
    }

    #[test]
    fn an_edge_setting_beats_the_settings_default_which_beats_auto() {
        let graph = DirGraph::new();
        let p = policy(&["Department"], OnMissingEndpoint::Vivify);
        assert_eq!(p.mode(&graph, None, "Department"), Mode::Vivify);
        assert_eq!(
            p.mode(&graph, Some(OnMissingEndpoint::Drop), "Team"),
            Mode::Drop
        );
        assert_eq!(
            p.mode(&graph, Some(OnMissingEndpoint::Error), "Team"),
            Mode::Error
        );
        assert_eq!(
            p.mode(&graph, Some(OnMissingEndpoint::Auto), "Department"),
            Mode::Drop
        );
    }

    #[test]
    fn a_frame_to_vivify_is_returned_untouched() {
        let graph = DirGraph::new();
        let p = policy(&[], OnMissingEndpoint::Auto);
        let df = DataFrame::from_cypher_rows(
            vec!["s".into(), "t".into()],
            vec![vec![Value::Int64(1), Value::Int64(2)]],
        )
        .unwrap();
        let ends = EdgeEnds {
            edge_type: "E",
            source: ("A", "s"),
            target: ("B", "t"),
        };
        let mut tally = DroppedEndpoints::default();
        let out = apply_policy(
            &graph,
            &p,
            Some(OnMissingEndpoint::Vivify),
            df,
            &ends,
            &mut tally,
        )
        .unwrap();
        assert_eq!(out.row_count(), 1);
        assert!(tally.into_diagnostics("E", "A").is_empty());
    }

    #[test]
    fn select_rows_keeps_the_chosen_rows_in_order_with_their_types() {
        let df = DataFrame::from_cypher_rows(
            vec!["n".into(), "s".into()],
            vec![
                vec![Value::Int64(1), Value::String("a".into())],
                vec![Value::Int64(2), Value::Null],
                vec![Value::Int64(3), Value::String("c".into())],
            ],
        )
        .unwrap();
        let out = df.select_rows(&[2, 1]);
        assert_eq!(out.row_count(), 2);
        assert_eq!(out.get_value(0, "n"), Some(Value::Int64(3)));
        assert_eq!(out.get_value(0, "s"), Some(Value::String("c".into())));
        assert_eq!(out.get_value(1, "n"), Some(Value::Int64(2)));
        assert_eq!(out.get_value(1, "s"), None);
        assert_eq!(out.get_column_type("n"), df.get_column_type("n"));
        assert_eq!(df.select_rows(&[]).row_count(), 0);
    }
}
