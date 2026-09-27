//! A read-only filter over a graph's elements: which nodes and relationships
//! a query may see. Valid time is its first client — a statement prefixed
//! `FOR VALID_TIME AS OF <instant>` compiles a [`GuardTemplate`] per query
//! scope at plan time, and the instant becomes a [`ValidTimeSelector`] once
//! per execution, so a cached plan never carries an instant.

use std::fmt;
use std::sync::{Arc, OnceLock};

use fixedbitset::FixedBitSet;
use petgraph::graph::{EdgeIndex, NodeIndex};

use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::features::temporal::duplicate_ids;
pub(crate) use crate::graph::features::temporal::endpoint_index::ResolvedFilter;
use crate::graph::features::temporal::endpoint_index::{self, ElementMasks};
use crate::graph::features::temporal::eval::{self, Instant, TemporalError};
use crate::graph::features::temporal::{self, IntervalConvention, TemporalTarget};
use crate::graph::schema::{InternedKey, TemporalConfig};
use crate::graph::storage::GraphRead;
use crate::graph::TemporalContext;

/// The declared validity interval of one node label a scope can reach.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NodeGuard {
    pub(crate) label: String,
    pub(crate) bounds: GuardBounds,
}

/// The declared validity interval of one relationship type (optionally only
/// from one source type) a scope can reach.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EdgeGuard {
    pub(crate) rel_type: String,
    pub(crate) rel_key: InternedKey,
    /// `None` for the unkeyed declaration, the fallback for sources that have
    /// no keyed one of their own.
    pub(crate) source_type: Option<String>,
    pub(crate) source_type_key: Option<InternedKey>,
    pub(crate) bounds: GuardBounds,
}

/// The two bound properties, pre-interned, and whether the `to` day belongs
/// to the interval.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GuardBounds {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) from_key: InternedKey,
    pub(crate) to_key: InternedKey,
    pub(crate) convention: IntervalConvention,
}

impl GuardBounds {
    pub(crate) fn of(config: &TemporalConfig) -> Self {
        GuardBounds {
            from: config.valid_from.clone(),
            to: config.valid_to.clone(),
            from_key: InternedKey::from_str(&config.valid_from),
            to_key: InternedKey::from_str(&config.valid_to),
            convention: config.convention,
        }
    }
}

/// Every declared target one query scope can reach, in the declaration
/// store's lookup order: node labels by name, then relationship types by
/// name with a type's source-keyed declarations before its unkeyed one.
///
/// The template holds no instant. It is compiled at plan time and may be
/// cached with the plan; the instant is resolved per execution into a
/// [`ValidTimeSelector`] and joined with the template in a [`GraphFilter`].
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GuardTemplate {
    pub(crate) nodes: Vec<NodeGuard>,
    pub(crate) edges: Vec<EdgeGuard>,
}

impl fmt::Display for GuardTemplate {
    /// `(:Well [vf, vt] closed), [:LICENSEE from :Field [f, t] half_open]`,
    /// or `no declared targets`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.nodes.is_empty() && self.edges.is_empty() {
            return f.write_str("no declared targets");
        }
        let bounds = |b: &GuardBounds| format!("[{}, {}] {}", b.from, b.to, b.convention.as_str());
        let nodes = self
            .nodes
            .iter()
            .map(|n| format!("(:{} {})", n.label, bounds(&n.bounds)));
        let edges = self.edges.iter().map(|e| match &e.source_type {
            Some(source) => format!("[:{} from :{} {}]", e.rel_type, source, bounds(&e.bounds)),
            None => format!("[:{} {}]", e.rel_type, bounds(&e.bounds)),
        });
        let parts: Vec<String> = nodes.chain(edges).collect();
        f.write_str(&parts.join(", "))
    }
}

/// Which instants a filter keeps: those valid at one instant, or those whose
/// interval overlaps a range (the fluent two-date form).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ValidTimeSelector {
    AsOf(Instant),
    Overlap(Instant, Instant),
}

impl TryFrom<&TemporalContext> for ValidTimeSelector {
    type Error = ();

    /// The fluent temporal context as a selector; `All` filters nothing and
    /// has none.
    fn try_from(context: &TemporalContext) -> Result<Self, ()> {
        match context {
            TemporalContext::Today => Ok(ValidTimeSelector::AsOf(Instant::Date(
                chrono::Local::now().date_naive(),
            ))),
            TemporalContext::At(d) => Ok(ValidTimeSelector::AsOf(Instant::Date(*d))),
            TemporalContext::During(a, b) => Ok(ValidTimeSelector::Overlap(
                Instant::Date(*a),
                Instant::Date(*b),
            )),
            TemporalContext::All => Err(()),
        }
    }
}

/// A compiled template joined with the selector one execution resolved.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GraphFilter {
    pub(crate) template: Arc<GuardTemplate>,
    pub(crate) selector: ValidTimeSelector,
}

impl GraphFilter {
    /// This filter at `graph`'s current version: per indexed target its
    /// segment (equal keys, equal masks), the node and relationship masks
    /// those segments give, the targets left to property guards (Disk mode,
    /// an unreadable bound, the endpoint-index byte cap, a range selector),
    /// and whether every target is timeless at the instant. See
    /// `features::temporal::endpoint_index`.
    pub(crate) fn resolve(&self, graph: &DirGraph) -> ResolvedFilter {
        endpoint_index::resolve(graph, &self.template, self.selector)
    }
}

/// A [`GraphFilter`] resolved for one execution: the test every guarded site
/// puts an element to. A bit test where the endpoint index gave a mask; the
/// validity evaluator on the declaration's bound properties for the targets it did
/// not (Disk mode, an unreadable bound, the byte cap, a range selector). NULL
/// or missing bounds are open.
///
/// A node passes only when it is valid under every declared label it carries
/// (primary or secondary); a relationship is judged by the declaration keyed
/// on its own source node's primary type, else the type's unkeyed one.
#[derive(Debug)]
pub(crate) struct ElementFilter {
    selector: ValidTimeSelector,
    masks: Option<Arc<ElementMasks>>,
    /// Declared labels the masks do not cover.
    node_residual: Box<[(InternedKey, GuardBounds)]>,
    /// Per relationship type with a declaration the masks do not cover, every
    /// declaration of that type in the template.
    edge_rules: Box<[(InternedKey, Box<[EdgeRule]>)]>,
    /// The first bound the evaluator could not read. The element is rejected
    /// and the execution raises this once it finishes.
    error: OnceLock<String>,
}

#[derive(Debug)]
struct EdgeRule {
    source: Option<InternedKey>,
    bounds: GuardBounds,
    /// Left to the evaluator: its rows have no mask.
    residual: bool,
}

impl ElementFilter {
    /// `None` when the resolved filter removes nothing: every target is
    /// timeless at the instant, or the template has no target.
    pub(crate) fn new(filter: &GraphFilter, resolved: ResolvedFilter) -> Option<Self> {
        if resolved.timeless || (resolved.masks.is_none() && resolved.guarded.is_empty()) {
            return None;
        }
        let template = &filter.template;
        let residual_node = |label: &str| {
            resolved
                .guarded
                .iter()
                .any(|t| matches!(t, TemporalTarget::Node(l) if l == label))
        };
        let node_residual = template
            .nodes
            .iter()
            .filter(|guard| residual_node(&guard.label))
            .map(|guard| (InternedKey::from_str(&guard.label), guard.bounds.clone()))
            .collect();
        let residual_edge = |guard: &EdgeGuard| {
            resolved.guarded.iter().any(|t| {
                matches!(t, TemporalTarget::Relationship { rel_type, source_type }
                    if *rel_type == guard.rel_type && *source_type == guard.source_type)
            })
        };
        let mut edge_rules: Vec<(InternedKey, Box<[EdgeRule]>)> = Vec::new();
        for guard in &template.edges {
            if !residual_edge(guard) || edge_rules.iter().any(|(k, _)| *k == guard.rel_key) {
                continue;
            }
            let rules = template
                .edges
                .iter()
                .filter(|g| g.rel_key == guard.rel_key)
                .map(|g| EdgeRule {
                    source: g.source_type_key,
                    bounds: g.bounds.clone(),
                    residual: residual_edge(g),
                })
                .collect();
            edge_rules.push((guard.rel_key, rules));
        }
        Some(ElementFilter {
            selector: filter.selector,
            masks: resolved.masks,
            node_residual,
            edge_rules: edge_rules.into_boxed_slice(),
            error: OnceLock::new(),
        })
    }

    /// Whether node `idx` is visible.
    #[inline]
    pub(crate) fn admits_node(&self, graph: &DirGraph, idx: NodeIndex) -> bool {
        if let Some(masks) = &self.masks {
            if !bit_admits(&masks.nodes, idx.index()) {
                return false;
            }
        }
        self.node_residual.is_empty() || self.residual_admits_node(graph, idx)
    }

    /// Whether relationship `edge` of type `conn`, leaving `source`, is
    /// visible — its own interval only, not its endpoints.
    #[inline]
    pub(crate) fn admits_edge(
        &self,
        graph: &DirGraph,
        edge: EdgeIndex,
        conn: InternedKey,
        source: NodeIndex,
    ) -> bool {
        if let Some(masks) = &self.masks {
            if !bit_admits(&masks.edges, edge.index()) {
                return false;
            }
        }
        self.edge_rules.is_empty() || self.residual_admits_edge(graph, edge, conn, source)
    }

    /// A matched hop: the relationship and the node it reaches.
    #[inline]
    pub(crate) fn admits_hop(
        &self,
        graph: &DirGraph,
        edge: EdgeIndex,
        conn: InternedKey,
        source: NodeIndex,
        far: NodeIndex,
    ) -> bool {
        self.admits_edge(graph, edge, conn, source) && self.admits_node(graph, far)
    }

    /// A relationship and both its endpoints.
    pub(crate) fn admits_relationship(
        &self,
        graph: &DirGraph,
        edge: EdgeIndex,
        conn: InternedKey,
        source: NodeIndex,
        target: NodeIndex,
    ) -> bool {
        self.admits_hop(graph, edge, conn, source, target) && self.admits_node(graph, source)
    }

    /// How many nodes carrying `label` the filter admits, from the endpoint
    /// index without visiting them: when no node carries a second label (so
    /// the label's own interval decides) and the label is undeclared or
    /// indexed at an instant. `None` sends the caller to a guarded walk.
    pub(crate) fn label_count(&self, graph: &DirGraph, label: &str) -> Option<usize> {
        if graph.has_secondary_labels {
            return None;
        }
        let ValidTimeSelector::AsOf(t) = self.selector else {
            return None;
        };
        let key = InternedKey::from_str(label);
        if self
            .node_residual
            .iter()
            .any(|(residual, _)| *residual == key)
        {
            return None;
        }
        match graph.temporal.node(label) {
            None => Some(graph.label_cardinality(label)),
            Some(_) => endpoint_index::node_count_at(graph, label, t),
        }
    }

    /// The error a bound the evaluator could not read raised, if any.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.get().map(String::as_str)
    }

    /// The visible node of `node_type` with the id the id index answered
    /// `hit` for. The index holds one node per (type, id), the last in the
    /// type's node order, so when version nodes share an id it may hand back
    /// one that is not visible while another is. Then the nodes it shadows —
    /// only those, from the type's duplicate-id map — are tested latest
    /// first, so every mode returns the latest-inserted visible version and
    /// no other id's bounds are read. A map over the byte cap is not built;
    /// the type is walked instead, comparing ids before any bound.
    #[cold]
    #[inline(never)]
    pub(crate) fn lookup_id(
        &self,
        graph: &DirGraph,
        node_type: &str,
        hit: Option<NodeIndex>,
    ) -> Option<NodeIndex> {
        let hit = hit?;
        if self.admits_node(graph, hit) {
            return Some(hit);
        }
        if let Some(duplicates) = endpoint_index::duplicate_ids(graph, node_type) {
            return duplicates
                .others(hit)
                .find(|&idx| self.admits_node(graph, idx));
        }
        let nodes = graph.type_indices.get(node_type)?;
        (0..nodes.len())
            .rev()
            .filter_map(|i| nodes.get(i))
            .find(|&idx| {
                duplicate_ids::shadowed_by(graph, node_type, idx) == Some(hit)
                    && self.admits_node(graph, idx)
            })
    }

    #[cold]
    #[inline(never)]
    fn residual_admits_node(&self, graph: &DirGraph, idx: NodeIndex) -> bool {
        let primary = graph.graph.node_type_of(idx);
        for (label, bounds) in self.node_residual.iter() {
            let carries = primary == Some(*label)
                || (graph.has_secondary_labels
                    && graph
                        .secondary_label_index
                        .get(label)
                        .is_some_and(|bucket| bucket.binary_search(&idx).is_ok()));
            if !carries {
                continue;
            }
            let from = temporal::node_bound(graph, idx, &bounds.from);
            let to = temporal::node_bound(graph, idx, &bounds.to);
            let valid = self.evaluate(&from, &to, bounds, || {
                let id = graph
                    .graph
                    .get_node_id(idx)
                    .map_or_else(|| "?".to_string(), |v| format_value_compact(&v));
                format!("node '{id}'")
            });
            if !valid {
                return false;
            }
        }
        true
    }

    #[cold]
    #[inline(never)]
    fn residual_admits_edge(
        &self,
        graph: &DirGraph,
        edge: EdgeIndex,
        conn: InternedKey,
        source: NodeIndex,
    ) -> bool {
        let Some((_, rules)) = self.edge_rules.iter().find(|(key, _)| *key == conn) else {
            return true;
        };
        let source_type = graph.graph.node_type_of(source);
        let keyed = rules
            .iter()
            .find(|rule| rule.source.is_some() && rule.source == source_type);
        let Some(rule) = keyed.or_else(|| rules.iter().find(|rule| rule.source.is_none())) else {
            return true;
        };
        if !rule.residual {
            return true;
        }
        let from = temporal::edge_bound(graph, edge, rule.bounds.from_key);
        let to = temporal::edge_bound(graph, edge, rule.bounds.to_key);
        self.evaluate(&from, &to, &rule.bounds, || {
            let id = graph
                .graph
                .get_node_id(source)
                .map_or_else(|| "?".to_string(), |v| format_value_compact(&v));
            format!("relationship from node '{id}'")
        })
    }

    /// Whether `[from, to]` holds the selector; an unreadable bound records
    /// the execution's error (the first one wins) and rejects the element.
    fn evaluate(
        &self,
        from: &Value,
        to: &Value,
        bounds: &GuardBounds,
        element: impl FnOnce() -> String,
    ) -> bool {
        let outcome = match self.selector {
            ValidTimeSelector::AsOf(t) => eval::interval_contains(from, to, t, bounds.convention),
            ValidTimeSelector::Overlap(a, b) => {
                eval::interval_overlaps(from, to, a, b, bounds.convention)
            }
        };
        outcome.unwrap_or_else(|err: TemporalError| {
            let message = format!(
                "FOR VALID_TIME AS OF: {}, {}",
                element(),
                temporal::describe_bound_error(err, &bounds.from, &bounds.to)
            );
            let _ = self.error.set(message);
            false
        })
    }
}

/// A slot past the mask was created after the filter resolved; no declared
/// row can be there, so it passes.
#[inline]
fn bit_admits(bits: &FixedBitSet, slot: usize) -> bool {
    slot >= bits.len() || bits.contains(slot)
}

fn format_value_compact(value: &Value) -> String {
    crate::graph::core::value_operations::format_value_compact(value)
}
