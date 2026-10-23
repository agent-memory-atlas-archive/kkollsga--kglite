//! Edge-anchored start: seed a pattern from a relationship property index hit
//! instead of from the first node pattern.
//!
//! Every other anchor in the matcher is node-based. `MATCH ()-[r:T {p: $x}]->()`
//! has no node to anchor on, so without this it enumerates every node and walks
//! every adjacency list. With a relationship index on `(T, p)` the hit edges name
//! their endpoint nodes directly, and those become the start nodes.
//!
//! **What the index decides and what it does not.** It chooses the *start
//! nodes*; the ordinary first hop then expands from them and re-applies the
//! edge pattern, so a stale or over-wide hit costs a wasted expansion and never
//! a wrong row. What the index must be is complete — a missing entry would drop
//! a row — which every edge write path maintains (`dir_graph::rel_index`).
//!
//! **Cost.** O(hits) to seed, then O(degree) per distinct start node for the
//! first hop. A hit on a very high-degree endpoint therefore still pays that
//! endpoint's degree; it is bounded by the degree of the hit's endpoints, never
//! by the size of the graph.

use super::*;

/// The relationship-index probe a pattern's first hop can be seeded from.
pub(crate) struct EdgeAnchor<'p> {
    pub rel_type: &'p str,
    pub property: &'p str,
    pub matcher: &'p PropertyMatcher,
    pub direction: EdgeDirection,
}

/// Whether a matcher is one the index can serve once its value is known.
fn serves_equality(matcher: &PropertyMatcher) -> bool {
    matches!(
        matcher,
        PropertyMatcher::Equals(_)
            | PropertyMatcher::EqualsParam(_)
            | PropertyMatcher::EqualsVar(_)
            | PropertyMatcher::EqualsNodeProp { .. }
            | PropertyMatcher::EqualsExpr(_)
            | PropertyMatcher::In(_)
    )
}

/// The relationship index `pattern`'s first hop can be seeded from, if any.
///
/// Eligible when the first node carries no property filter (a node anchor is
/// at least as selective then), is not pre-bound, and the first hop is one
/// fixed-length segment of a single relationship type whose pattern properties
/// hold an equality (or `IN`) on an indexed property. Shared by the matcher and
/// by `EXPLAIN`, so a plan row never claims an anchor the runtime declines.
pub(crate) fn edge_anchor<'p>(
    graph: &DirGraph,
    pattern: &'p Pattern,
    first_is_prebound: bool,
) -> Option<EdgeAnchor<'p>> {
    if first_is_prebound || !graph.has_rel_property_indices() {
        return None;
    }
    let (PatternElement::Node(first), Some(PatternElement::Edge(edge))) =
        (pattern.elements.first()?, pattern.elements.get(1))
    else {
        return None;
    };
    if first.properties.is_some() || edge.var_length.is_some() {
        return None;
    }
    let rel_type = match (&edge.connection_type, &edge.connection_types) {
        (Some(only), None) => only.as_str(),
        (Some(first_type), Some(all)) if all.len() == 1 => first_type.as_str(),
        _ => return None,
    };
    let props = edge.properties.as_ref()?;
    // Deterministic choice when several properties are indexed.
    let mut names: Vec<&String> = props.keys().collect();
    names.sort_unstable();
    names.into_iter().find_map(|name| {
        let matcher = &props[name];
        (serves_equality(matcher) && graph.has_rel_property_index(rel_type, name)).then_some(
            EdgeAnchor {
                rel_type,
                property: name.as_str(),
                matcher,
                direction: edge.direction,
            },
        )
    })
}

impl<'a> PatternExecutor<'a> {
    /// Start nodes from the relationship index, or `None` to seed the ordinary
    /// way (no eligible index, an unresolved value, or a value the index cannot
    /// prove the complete answer for).
    pub(super) fn edge_anchor_seeds(
        &self,
        pattern: &Pattern,
        first_node: &NodePattern,
        first_is_prebound: bool,
    ) -> Option<Vec<NodeIndex>> {
        let anchor = edge_anchor(self.graph, pattern, first_is_prebound)?;
        let mut hits: Vec<petgraph::graph::EdgeIndex> = Vec::new();
        let mut probe = |value: &Value| -> Option<()> {
            hits.extend(
                self.graph
                    .lookup_rel_index(anchor.rel_type, anchor.property, value)?,
            );
            Some(())
        };
        match anchor.matcher {
            PropertyMatcher::Equals(value) => probe(value)?,
            PropertyMatcher::EqualsParam(name) => probe(self.params.get(name.as_str())?)?,
            PropertyMatcher::In(values) => {
                for value in values.iter() {
                    probe(value)?;
                }
            }
            // A deferred reference reaches the matcher already resolved to
            // `Equals`; one that did not has no value to probe.
            _ => return None,
        }
        let mut seeds: Vec<NodeIndex> = Vec::with_capacity(hits.len());
        for edge in hits {
            let (source, target) = self.graph.graph.edge_endpoints(edge)?;
            match anchor.direction {
                EdgeDirection::Outgoing => seeds.push(source),
                EdgeDirection::Incoming => seeds.push(target),
                EdgeDirection::Both => {
                    seeds.push(source);
                    seeds.push(target);
                }
            }
        }
        seeds.sort_unstable();
        seeds.dedup();
        seeds.retain(|&node| self.node_matches_pattern_labels(node, first_node));
        Some(self.guard_seeds(seeds, None))
    }
}
