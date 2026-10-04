//! Chain path-count fusion: `MATCH` of one linear chain `RETURN count(*)`.
//!
//! The matcher materialises every path of the chain. A chain's path count is
//! a degree product, computable hop by hop over the frontier of nodes it
//! reaches — see `executor/chain_count.rs` for the executor.

use super::is_count_of_var_or_star;
use crate::graph::core::pattern_matching::{
    EdgeDirection, EdgePattern, NodePattern, Pattern, PatternElement, PropertyMatcher,
};
use crate::graph::languages::cypher::ast::*;
use crate::graph::languages::cypher::executor::return_item_column_name;
use crate::graph::languages::cypher::planner::annotations::fixed_edge_types_are_pairwise_disjoint;

/// Fewest relationships a chain needs outside a valid-time guard. Below it
/// the existing hop fusions already answer in adjacency counts, and the
/// two-hop shape is left to them so their unguarded bench cells do not move.
const MIN_HOPS: usize = 3;

/// Fewest relationships under a guard, where the aggregate fusions decline
/// the two-hop shape.
const MIN_HOPS_GUARDED: usize = 2;

/// Matchers that resolve against a row of an earlier clause, which a
/// first-clause chain does not have.
fn matcher_needs_row(matcher: &PropertyMatcher) -> bool {
    matches!(
        matcher,
        PropertyMatcher::EqualsVar(_)
            | PropertyMatcher::EqualsNodeProp { .. }
            | PropertyMatcher::EqualsExpr(_)
    )
}

fn node_is_countable(node: &NodePattern) -> bool {
    node.label_params.is_empty()
        && node
            .properties
            .as_ref()
            .is_none_or(|props| !props.values().any(matcher_needs_row))
}

fn edge_is_countable(edge: &EdgePattern) -> bool {
    edge.var_length.is_none()
        && edge.type_params.is_empty()
        && edge
            .properties
            .as_ref()
            .is_none_or(|props| !props.values().any(matcher_needs_row))
}

/// Whether `pattern` is a linear chain of `min_hops` or more relationships
/// whose every element is one the executor can test per node and per
/// relationship, with no variable named twice (a repeat is an identity
/// constraint the DP does not track).
fn is_countable_chain(pattern: &Pattern, min_hops: usize) -> bool {
    let hops = pattern.elements.len() / 2;
    if hops < min_hops || pattern.elements.len().is_multiple_of(2) {
        return false;
    }
    let mut names = std::collections::HashSet::new();
    pattern.elements.iter().enumerate().all(|(i, element)| {
        let (variable, countable) = match (element, i % 2) {
            (PatternElement::Node(node), 0) => (node.variable.as_deref(), node_is_countable(node)),
            (PatternElement::Edge(edge), 1) => (edge.variable.as_deref(), edge_is_countable(edge)),
            _ => return false,
        };
        countable && variable.is_none_or(|name| names.insert(name))
    })
}

/// Whether a chain with overlapping hop types is one the executor can
/// correct for the matcher's relationship-uniqueness rule: exactly two
/// directed hops. A path then breaks the rule only by using one relationship
/// twice, which is countable from the first hop alone; three or more hops, or
/// an undirected hop (a relationship can be crossed in either orientation),
/// have no such closed form.
fn has_correctable_overlap(pattern: &Pattern) -> bool {
    pattern.elements.len() == 5
        && pattern.elements.iter().all(|element| {
            !matches!(element, PatternElement::Edge(edge) if edge.direction == EdgeDirection::Both)
        })
}

/// Whether `expr` is `count(*)` or `count(v)` for a variable the chain binds.
fn is_chain_count(expr: &Expression, pattern: &Pattern) -> bool {
    is_count_of_var_or_star(expr, None)
        || pattern.elements.iter().any(|element| {
            let variable = match element {
                PatternElement::Node(node) => node.variable.as_deref(),
                PatternElement::Edge(edge) => edge.variable.as_deref(),
            };
            variable.is_some_and(|name| is_count_of_var_or_star(expr, Some(name)))
        })
}

/// Rewrite `MATCH <chain> RETURN count(*)` into `Clause::FusedChainPathCount`.
///
/// **Precondition:** the statement is exactly one `MATCH` and one `RETURN`.
/// **Pattern matched:** one linear pattern of at least [`MIN_HOPS`]
/// relationships ([`MIN_HOPS_GUARDED`] under a valid-time guard), no path
/// assignment, no residual predicate or hint, `RETURN count(*)` (or `count(v)`
/// of a variable the chain binds) without DISTINCT or HAVING. **Rewrite:** the
/// pair becomes one clause holding the pattern and the column name.
/// **Why-bail:** a repeated variable (identity constraint), a var-length or
/// parameterised hop, a comma pattern, an `OPTIONAL MATCH`, a property
/// matcher that reads a row, and hop types that are untyped or shared with
/// another hop unless the chain is two directed hops. The matcher enforces
/// relationship uniqueness for such chains; the executor corrects for it only
/// in that two-hop case (see `executor/chain_count.rs`), so the rest stay on
/// the matcher.
pub(crate) fn fuse_chain_path_count(query: &mut CypherQuery, guarded: bool) {
    let [Clause::Match(m), Clause::Return(ret)] = query.clauses.as_slice() else {
        return;
    };
    let [pattern] = m.patterns.as_slice() else {
        return;
    };
    let [item] = ret.items.as_slice() else {
        return;
    };
    let min_hops = if guarded { MIN_HOPS_GUARDED } else { MIN_HOPS };
    if !m.path_assignments.is_empty()
        || m.where_clause.is_some()
        || m.limit_hint.is_some()
        || m.distinct_node_hint.is_some()
        || !m.node_anchors.is_empty()
        || ret.distinct
        || ret.having.is_some()
        || !is_chain_count(&item.expression, pattern)
        || !is_countable_chain(pattern, min_hops)
    {
        return;
    }
    let overlapping_types = !fixed_edge_types_are_pairwise_disjoint(pattern);
    if overlapping_types && !has_correctable_overlap(pattern) {
        return;
    }
    let fused = Clause::FusedChainPathCount {
        pattern: pattern.clone(),
        overlapping_types,
        alias: return_item_column_name(item),
    };
    query.clauses = vec![fused];
}
