//! The temporal declaration store: which two properties bound each node
//! label's or relationship type's validity interval, and under which
//! convention.
//!
//! Relationship configs are kept as an ordered list per type: a legacy type
//! holding several unkeyed configs (which every valid-time filter refuses as
//! ambiguous) is saved back in the order it was read, and a bulk load reads a
//! row's start from the first config it carries ([`merge_start_key`]). How the
//! store is saved and loaded is `persist.rs`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::endpoint_index::{self, IndexCache};
use super::eval::{EmptyWhen, IntervalConvention};
use super::merge_key::StartKey;
use super::validate::{self, Walk};
use crate::graph::diagnostics::{Diagnostic, DiagnosticGroup};
use crate::graph::dir_graph::caches::ForkPrivateCache;
use crate::graph::dir_graph::DirGraph;
use crate::graph::schema::TemporalConfig;

/// What a declaration is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TemporalTarget {
    /// A node label: a primary type or a secondary label.
    Node(String),
    /// A relationship type, optionally only the relationships leaving nodes of
    /// `source_type`. One type may carry several source-keyed declarations
    /// when its sources store their bounds under different properties. A
    /// relationship takes its source's keyed declaration first, and the
    /// unkeyed one only when its source has none.
    Relationship {
        rel_type: String,
        source_type: Option<String>,
    },
}

impl TemporalTarget {
    /// `node label 'X'`, `relationship type 'R'`, or `relationship type 'R'
    /// from source type 'S'` — the phrase every message names a target by.
    pub(crate) fn describe(&self) -> String {
        match self {
            TemporalTarget::Node(label) => format!("node label '{label}'"),
            TemporalTarget::Relationship {
                rel_type,
                source_type: None,
            } => format!("relationship type '{rel_type}'"),
            TemporalTarget::Relationship {
                rel_type,
                source_type: Some(source),
            } => format!("relationship type '{rel_type}' from source type '{source}'"),
        }
    }

    fn source_type(&self) -> Option<&str> {
        match self {
            TemporalTarget::Node(_) => None,
            TemporalTarget::Relationship { source_type, .. } => source_type.as_deref(),
        }
    }
}

/// The outcome of [`declare`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclareReport {
    /// `false` when an identical declaration was already in place for the same
    /// key (the node label, or the relationship type plus its source type).
    pub changed: bool,
    /// Rows validated. 0 for a no-op, which validates nothing.
    pub rows: usize,
    /// Rows whose `to` bound equals another row's `from` bound within the same
    /// entity — the node rows sharing an `id`, or one source node's
    /// relationships of the type — counted at declare time. Rows of different
    /// entities that share a boundary day are not counted: no reader sees an
    /// entity twice because of them. `None` when not counted: a no-op, or a disk-mode label
    /// above [`DISK_NODE_ABUTMENT_CAP`] rows.
    pub abutting_rows: Option<usize>,
    /// The advisory a declaration earns, at most one applying: a half-open
    /// one with rows whose interval is empty (valid at no instant, kept and
    /// counted), a closed one with abutting rows, or one whose `to` property
    /// no row carries yet.
    pub warning: Option<String>,
    /// `warning` with its classification, for a caller that reports by group.
    pub diagnostic: Option<Diagnostic>,
}

/// One entry of [`list`].
#[derive(Clone, Debug, PartialEq)]
pub struct DeclarationInfo {
    pub target: TemporalTarget,
    pub config: TemporalConfig,
    /// The declare-time count [`DeclareReport::abutting_rows`] reported,
    /// kept across save and load; `None` for a config a loader or
    /// `set_temporal` wrote, and for one read from a file written before
    /// declarations were saved.
    pub abutting_rows: Option<usize>,
    /// The relationship type holds several different configs none of which
    /// names a source type — possible only for configs written before source
    /// types existed, or by `set_temporal` — so which one applies to an edge
    /// depends on the order they were added in. Re-declare them per
    /// `source_type` to resolve it. Always `false` for a node label.
    pub ambiguous: bool,
    /// Rows whose interval is empty now, valid at no instant: `from == to`
    /// under half-open, which a declaration and every load or Cypher write
    /// accept with a warning, or `from` after `to`, which they refuse and
    /// only a writer the check does not judge can leave (a fluent
    /// `update()`, an undeclare that hands a source's relationships to the
    /// unkeyed declaration, or a graph saved by an earlier version). Counted
    /// at the graph's current version; `None` where not counted.
    pub empty_rows: Option<usize>,
    /// Rows holding a bound that is now unreadable (not NULL, a date, a
    /// datetime or an ISO string) — left as [`Self::empty_rows`] are.
    /// A query that filters on such a row raises. Counted like
    /// [`Self::empty_rows`].
    pub unreadable_rows: Option<usize>,
}

/// The largest disk-mode node label whose abutting rows are counted. Counting
/// holds every row's bounds at once, and disk mode keeps no heap structure
/// that grows with the graph beyond a fixed ceiling; above it the declaration
/// is still validated in full and the count is reported as not computed.
pub const DISK_NODE_ABUTMENT_CAP: usize = 250_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct TemporalDeclarations {
    pub(super) nodes: HashMap<String, TemporalConfig>,
    pub(super) edges: HashMap<String, Vec<TemporalConfig>>,
    /// Declare-time counts. Saved with the declarations by `persist.rs`, not
    /// through this derive.
    #[serde(skip)]
    pub(super) abutting: HashMap<TemporalTarget, usize>,
    /// Endpoint indexes and counts at one graph version; a fork starts
    /// empty.
    #[serde(skip)]
    pub(super) index: ForkPrivateCache<IndexCache>,
}

/// What a declaration does to the store once validated.
pub(super) enum Change {
    Unchanged,
    Insert,
}

/// Nodes first, then by name; within a relationship type the source-keyed
/// declarations (by source) before the unkeyed one — the order a lookup
/// tries them in.
fn lookup_order(target: &TemporalTarget) -> (u8, &str, bool, Option<&str>) {
    match target {
        TemporalTarget::Node(label) => (0, label, false, None),
        TemporalTarget::Relationship {
            rel_type,
            source_type,
        } => (1, rel_type, source_type.is_none(), source_type.as_deref()),
    }
}

fn same_properties(a: &TemporalConfig, b: &TemporalConfig) -> bool {
    a.valid_from == b.valid_from && a.valid_to == b.valid_to
}

fn same_interval(a: &TemporalConfig, b: &TemporalConfig) -> bool {
    same_properties(a, b) && a.convention == b.convention && a.empty_when == b.empty_when
}

impl TemporalDeclarations {
    /// Whether nothing is declared — every temporal filter is then empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.values().all(Vec::is_empty)
    }

    pub(crate) fn has_node_declarations(&self) -> bool {
        !self.nodes.is_empty()
    }

    pub(crate) fn node(&self, label: &str) -> Option<&TemporalConfig> {
        self.nodes.get(label)
    }

    /// Every declared node label.
    pub(crate) fn node_labels(&self) -> impl Iterator<Item = &str> {
        self.nodes.keys().map(String::as_str)
    }

    /// Whether `label`'s declaration names `property` as its `from` or `to`
    /// bound. The unknown-property guards count such a name as known: a `to`
    /// no row carries yet is absent from the observed schema, and the
    /// declaration was accepted on the promise that a write may set it.
    pub(crate) fn names_node_bound(&self, label: &str, property: &str) -> bool {
        self.nodes
            .get(label)
            .is_some_and(|c| c.valid_from == property || c.valid_to == property)
    }

    /// Whether any node declaration names `property` as a bound — the check a
    /// write settles before resolving the written node's labels.
    pub(crate) fn names_any_node_bound(&self, property: &str) -> bool {
        self.nodes
            .values()
            .any(|c| c.valid_from == property || c.valid_to == property)
    }

    /// Every config of `rel_type`, in declaration order; empty when none.
    pub(crate) fn edges(&self, rel_type: &str) -> &[TemporalConfig] {
        self.edges.get(rel_type).map_or(&[], Vec::as_slice)
    }

    /// Whether `rel_type` holds more than one unkeyed config (see
    /// [`DeclarationInfo::ambiguous`]).
    pub(crate) fn is_ambiguous(&self, rel_type: &str) -> bool {
        self.edges(rel_type)
            .iter()
            .filter(|c| c.source_type.is_none())
            .nth(1)
            .is_some()
    }

    pub(crate) fn abutting(&self, target: &TemporalTarget) -> Option<usize> {
        self.abutting.get(target).copied()
    }

    /// The key a load of `target`'s rows declares under (see
    /// `loader::declare_from_column_types`): a source-keyed relationship
    /// target becomes unkeyed unless its source has a declaration of its own
    /// or the unkeyed one names other properties.
    pub(super) fn load_target(
        &self,
        target: TemporalTarget,
        valid_from: &str,
        valid_to: &str,
    ) -> TemporalTarget {
        let TemporalTarget::Relationship {
            rel_type,
            source_type: Some(source),
        } = target
        else {
            return target;
        };
        let configs = self.edges(&rel_type);
        let keyed = configs
            .iter()
            .any(|c| c.source_type.as_deref() == Some(source.as_str()));
        let fallbacks = || configs.iter().filter(|c| c.source_type.is_none());
        let other_fallback = fallbacks().next().is_some()
            && !fallbacks().any(|c| c.valid_from == valid_from && c.valid_to == valid_to);
        TemporalTarget::Relationship {
            rel_type,
            source_type: (keyed || other_fallback).then_some(source),
        }
    }

    /// The convention and `empty_when` a declaration of `valid_from`/`valid_to`
    /// for `target` takes when its caller names neither: those of a
    /// declaration of the same key already naming the same properties, else
    /// closed and unset.
    pub(super) fn default_form(
        &self,
        target: &TemporalTarget,
        valid_from: &str,
        valid_to: &str,
    ) -> (IntervalConvention, Option<EmptyWhen>) {
        let same = |c: &&TemporalConfig| c.valid_from == valid_from && c.valid_to == valid_to;
        let existing = match target {
            TemporalTarget::Node(label) => self.nodes.get(label).filter(same),
            TemporalTarget::Relationship {
                rel_type,
                source_type,
            } => self
                .edges(rel_type)
                .iter()
                .filter(|c| c.source_type == *source_type)
                .find(same),
        };
        existing.map_or((IntervalConvention::Closed, None), |c| {
            (c.convention, c.empty_when)
        })
    }

    /// Decide what declaring `config` for `target` does: nothing (an identical
    /// declaration holds the same key), an insert, or a conflict error. The
    /// key is the node label, or the relationship type plus its source type —
    /// an unkeyed relationship declaration is its own key, the fallback for
    /// sources without one, so it never conflicts with a keyed one.
    pub(super) fn change_for(
        &self,
        target: &TemporalTarget,
        config: &TemporalConfig,
    ) -> Result<Change, String> {
        let same_key: Vec<&TemporalConfig> = match target {
            TemporalTarget::Node(label) => self.nodes.get(label).into_iter().collect(),
            TemporalTarget::Relationship {
                rel_type,
                source_type,
            } => self
                .edges(rel_type)
                .iter()
                .filter(|c| c.source_type == *source_type)
                .collect(),
        };
        // A legacy list can hold several distinct unkeyed configs; one
        // identical to the declaration makes it a no-op.
        match same_key.first() {
            None => Ok(Change::Insert),
            Some(_) if same_key.iter().any(|c| same_interval(c, config)) => Ok(Change::Unchanged),
            Some(existing) => Err(format!(
                "{} is already declared with from '{}', to '{}', convention '{}'{}. \
                 Undeclare it first to change it.",
                target.describe(),
                existing.valid_from,
                existing.valid_to,
                existing.convention.as_str(),
                existing
                    .empty_when
                    .map_or(String::new(), |e| format!(", empty_when '{}'", e.as_str()))
            )),
        }
    }

    /// Source types with a keyed declaration of `rel_type` — the sources an
    /// unkeyed declaration does not apply to.
    pub(crate) fn keyed_sources(&self, rel_type: &str) -> Vec<&str> {
        self.edges(rel_type)
            .iter()
            .filter_map(|c| c.source_type.as_deref())
            .collect()
    }

    pub(super) fn insert(
        &mut self,
        target: &TemporalTarget,
        config: TemporalConfig,
        abutting: Option<usize>,
    ) {
        match target {
            TemporalTarget::Node(label) => {
                self.nodes.insert(label.clone(), config);
            }
            TemporalTarget::Relationship { rel_type, .. } => {
                self.edges.entry(rel_type.clone()).or_default().push(config);
            }
        }
        match abutting {
            Some(count) => self.abutting.insert(target.clone(), count),
            None => self.abutting.remove(target),
        };
        self.forget_templates();
    }

    /// The cached templates and resolutions are built from the declarations,
    /// which just changed.
    fn forget_templates(&self) {
        if let Some(cache) = self
            .index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            cache.forget_declared();
        }
    }

    /// Drop every cached endpoint index. For a version set directly rather
    /// than bumped, which could repeat a version the cache was filled at.
    pub(crate) fn forget_endpoint_indexes(&self) {
        *self
            .index
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    pub(super) fn remove(&mut self, target: &TemporalTarget) -> bool {
        self.abutting.remove(target);
        self.forget_templates();
        match target {
            TemporalTarget::Node(label) => self.nodes.remove(label).is_some(),
            TemporalTarget::Relationship {
                rel_type,
                source_type,
            } => {
                let Some(configs) = self.edges.get_mut(rel_type) else {
                    return false;
                };
                let before = configs.len();
                configs.retain(|c| c.source_type != *source_type);
                let removed = configs.len() != before;
                if configs.is_empty() {
                    self.edges.remove(rel_type);
                }
                removed
            }
        }
    }

    pub(super) fn entries(&self) -> Vec<DeclarationInfo> {
        let nodes = self
            .nodes
            .iter()
            .map(|(label, config)| (TemporalTarget::Node(label.clone()), config));
        let edges = self.edges.iter().flat_map(|(rel_type, configs)| {
            configs.iter().map(move |config| {
                (
                    TemporalTarget::Relationship {
                        rel_type: rel_type.clone(),
                        source_type: config.source_type.clone(),
                    },
                    config,
                )
            })
        });
        let mut out: Vec<DeclarationInfo> = nodes
            .chain(edges)
            .map(|(target, config)| DeclarationInfo {
                abutting_rows: self.abutting(&target),
                ambiguous: match &target {
                    TemporalTarget::Relationship {
                        rel_type,
                        source_type: None,
                    } => self.is_ambiguous(rel_type),
                    _ => false,
                },
                target,
                config: config.clone(),
                empty_rows: None,
                unreadable_rows: None,
            })
            .collect();
        out.sort_by(|a, b| lookup_order(&a.target).cmp(&lookup_order(&b.target)));
        out
    }
}

/// Declare which two properties bound `target`'s validity interval, and
/// whether the `to` day belongs to it.
///
/// The `from` property must exist on the target. A `to` property no row carries
/// yet is accepted with a warning — every period is still open — unless it is a
/// near miss of a property the target has, which is refused as a typo. Every
/// stored bound is read under
/// the rule `valid_at` uses; the first one that is not NULL, a date, a
/// datetime or an ISO date string is refused, naming its element, and so is a
/// row whose interval is inverted (`from > to`), except the one empty shape
/// a closed declaration's `empty_when` names ([`declare_loaded_with`]). A row
/// whose interval is empty under half-open (`from == to`) is accepted,
/// counted and warned about. Re-declaring an identical interval is a no-op; a different one for
/// the same target is refused. A real change bumps the graph version.
pub fn declare(
    graph: &mut DirGraph,
    target: &TemporalTarget,
    valid_from: &str,
    valid_to: &str,
    convention: IntervalConvention,
) -> Result<DeclareReport, String> {
    declare_loaded(graph, target, valid_from, valid_to, convention, &[])
}

/// [`declare`] for a loader that has just written the bound columns. The
/// schema records a property only once some row holds a value for it, so a
/// column the load wrote entirely NULL (every period still open) is absent
/// from it; naming it in `written` lets it count as existing, without the
/// open-ended warning a manual declaration of such a `to` column earns.
pub fn declare_loaded(
    graph: &mut DirGraph,
    target: &TemporalTarget,
    valid_from: &str,
    valid_to: &str,
    convention: IntervalConvention,
    written: &[&str],
) -> Result<DeclareReport, String> {
    declare_loaded_with(
        graph,
        target,
        (valid_from, valid_to, convention, None),
        written,
    )
}

/// [`declare_loaded`] with `empty_when`: under `closed`, the empty interval
/// it names is accepted instead of refused (and refused outright under
/// `half_open`, which needs no such option).
pub fn declare_loaded_with(
    graph: &mut DirGraph,
    target: &TemporalTarget,
    (valid_from, valid_to, convention, empty_when): (
        &str,
        &str,
        IntervalConvention,
        Option<EmptyWhen>,
    ),
    written: &[&str],
) -> Result<DeclareReport, String> {
    declare_walked(
        graph,
        target,
        interval_config(valid_from, valid_to, convention, empty_when),
        written,
        (true, EntityGrouping::OwnId),
    )
}

/// A config with no source type, which [`declare_walked`] takes from the
/// target.
pub(super) fn interval_config(
    valid_from: &str,
    valid_to: &str,
    convention: IntervalConvention,
    empty_when: Option<EmptyWhen>,
) -> TemporalConfig {
    TemporalConfig {
        valid_from: valid_from.to_string(),
        valid_to: valid_to.to_string(),
        convention,
        source_type: None,
        empty_when,
    }
}

/// Refuse `empty_when` under `half_open`: a half-open interval already holds
/// `from == to` as empty, so the option names a shape that convention has no
/// use for.
pub(super) fn check_options(config: &TemporalConfig) -> Result<(), String> {
    match (config.empty_when, config.convention) {
        (Some(empty_when), IntervalConvention::HalfOpen) => Err(format!(
            "empty_when '{}' applies to convention 'closed', where a to bound before the from \
             bound is otherwise refused; convention 'half_open' already holds an empty interval \
             (from equal to to) and needs no option",
            empty_when.as_str()
        )),
        _ => Ok(()),
    }
}

/// Which node rows are versions of one entity when a declaration counts
/// abutting rows: only versions of one entity can be valid twice on a shared
/// boundary day. Relationship targets always group by source node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EntityGrouping<'a> {
    /// Rows sharing a node `id`.
    #[default]
    OwnId,
    /// Rows with the same target over this outgoing relationship type — a
    /// blueprint sub-node's parent edge, whose parent column is not a stored
    /// property. A row without such an edge groups by its own `id`.
    ParentEdge(&'a str),
}

/// [`declare_loaded`] with the node rows grouped by `grouping` when abutting
/// rows are counted (`grouping` is ignored for a relationship target).
pub(crate) fn declare_loaded_grouped(
    graph: &mut DirGraph,
    target: &TemporalTarget,
    bounds: TemporalConfig,
    written: &[&str],
    grouping: EntityGrouping<'_>,
) -> Result<DeclareReport, String> {
    declare_walked(graph, target, bounds, written, (true, grouping))
}

/// [`declare_loaded`]; `warn_empty` false leaves the empty-row warning to a
/// load that has already reported the same rows.
pub(super) fn declare_walked(
    graph: &mut DirGraph,
    target: &TemporalTarget,
    bounds: TemporalConfig,
    written: &[&str],
    (warn_empty, grouping): (bool, EntityGrouping<'_>),
) -> Result<DeclareReport, String> {
    let config = TemporalConfig {
        source_type: target.source_type().map(str::to_string),
        ..bounds
    };
    check_options(&config)?;
    validate::check_target(graph, target)?;
    let change = graph.temporal.change_for(target, &config)?;
    if matches!(change, Change::Unchanged) {
        return Ok(DeclareReport {
            changed: false,
            rows: 0,
            abutting_rows: None,
            warning: None,
            diagnostic: None,
        });
    }
    let Walk {
        rows,
        abutting,
        abutting_other,
        empty,
        open_ended,
    } = validate::walk(graph, target, &config, written, grouping)?;
    // Empty rows exist under half-open and under closed with `empty_when`;
    // abutment warns only under closed; an open-ended `to` leaves no row
    // with two bounds.
    let empty_warning = warn_empty
        .then(|| empty.declaration_warning(target))
        .flatten()
        .map(|m| Diagnostic::new(DiagnosticGroup::DataQuality, "empty_interval_rows", m));
    let abutment = match abutting {
        Some(same) if config.convention == IntervalConvention::Closed => {
            validate::abutment_warning(target, grouping, (same, abutting_other), rows)
        }
        _ => None,
    }
    .map(|m| Diagnostic::new(DiagnosticGroup::DataQuality, "abutting_intervals", m));
    let open_ended =
        open_ended.map(|m| Diagnostic::new(DiagnosticGroup::DataShape, "open_ended_interval", m));
    let diagnostic = abutment.or(empty_warning).or(open_ended);
    record_insert(graph, target, config, abutting);
    graph.bump_version();
    Ok(DeclareReport {
        changed: true,
        rows,
        abutting_rows: abutting,
        warning: diagnostic.as_ref().map(|d| d.message.clone()),
        diagnostic,
    })
}

/// Remove `target`'s declaration. `false`, and no version bump, when there
/// was none. An unkeyed relationship target removes only the unkeyed
/// declaration, never a source-keyed one.
pub fn undeclare(graph: &mut DirGraph, target: &TemporalTarget) -> bool {
    let removed = record_remove(graph, target);
    if removed {
        graph.bump_version();
    }
    removed
}

/// Install `config` for `target`, journaling the change. Every route that
/// writes the store goes through this or [`record_remove`], so a durable graph
/// recovers what a crash before the next checkpoint would otherwise lose.
pub(super) fn record_insert(
    graph: &mut DirGraph,
    target: &TemporalTarget,
    config: TemporalConfig,
    abutting: Option<usize>,
) {
    graph.note_temporal_declaration(target, Some((&config, abutting)));
    graph.temporal.insert(target, config, abutting);
}

/// Remove `target`'s declaration, journaling the withdrawal when there was
/// one to remove.
pub(super) fn record_remove(graph: &mut DirGraph, target: &TemporalTarget) -> bool {
    let removed = graph.temporal.remove(target);
    if removed {
        graph.note_temporal_declaration(target, None);
    }
    removed
}

/// Every declaration: nodes by label, then relationship types by name, each
/// type's source-keyed declarations (by source) before its unkeyed one — the
/// order a relationship's lookup tries them in. The empty and unreadable
/// row counts are taken at the graph's current version (one walk per
/// declaration, cached until the next write).
pub fn list(graph: &DirGraph) -> Vec<DeclarationInfo> {
    let mut entries = graph.temporal.entries();
    for info in &mut entries {
        let counts = endpoint_index::target_counts(graph, &info.target, &info.config);
        info.empty_rows = Some(counts.empty_rows);
        info.unreadable_rows = Some(counts.unreadable_rows);
    }
    entries
}

/// [`list`] without the row counts, which walk the graph: for callers that
/// read only the declarations themselves.
pub(crate) fn declared(graph: &DirGraph) -> Vec<DeclarationInfo> {
    graph.temporal.entries()
}

/// Whether a declaration on any label node `idx` carries — its primary type or
/// a secondary label — names `property` as a bound. The locked `SET` check
/// reads this: the executor holds the node, not the pattern's labels.
pub(crate) fn node_names_bound(
    graph: &DirGraph,
    idx: petgraph::graph::NodeIndex,
    property: &str,
) -> bool {
    graph.temporal.names_any_node_bound(property)
        && graph.node_labels(idx).into_iter().any(|key| {
            graph
                .interner
                .try_resolve(key)
                .is_some_and(|label| graph.temporal.names_node_bound(label, property))
        })
}

/// The config declared for node label `label`, if any.
pub fn node_config<'g>(graph: &'g DirGraph, label: &str) -> Option<&'g TemporalConfig> {
    graph.temporal.node(label)
}

/// Every config declared for relationship type `rel_type`, in declaration
/// order — the order the fluent `traverse()` filter tries them in.
pub fn edge_configs<'g>(graph: &'g DirGraph, rel_type: &str) -> &'g [TemporalConfig] {
    graph.temporal.edges(rel_type)
}

/// The bounds a bulk load of `rel_type` relationships from `source_type` reads
/// its rows' versions by (`ConnectionBatchProcessor::configure`), so that a row
/// that is not an identical copy of a stored relationship between the same
/// endpoints becomes a parallel one: the source's keyed declaration's, else
/// every unkeyed declaration's in declaration order (a legacy type can hold
/// several; each row keys on the first it carries). `None` as the source takes
/// only the unkeyed declarations, and `None` is returned when none applies.
pub(crate) fn merge_start_key(
    graph: &DirGraph,
    rel_type: &str,
    source_type: Option<&str>,
) -> Option<StartKey> {
    let configs = graph.temporal.edges(rel_type);
    let keyed = source_type.and_then(|source| {
        configs
            .iter()
            .find(|c| c.source_type.as_deref() == Some(source))
    });
    match keyed {
        Some(config) => StartKey::of([config]),
        None => StartKey::of(configs.iter().filter(|c| c.source_type.is_none())),
    }
}
