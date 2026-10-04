//! Chain path-count fusion: `MATCH` of one linear chain `RETURN count(*)`.
//!
//! The matcher materialises every path of the chain. A chain's path count is
//! a degree product, computable hop by hop over the frontier of nodes it
//! reaches — see `executor/chain_count.rs` for the executor. The same chain
//! under `count(DISTINCT x)` is a reachability question, answered by the
//! executor's forward and backward sweeps; grouped by a key on one chain
//! node, the same sweeps answer per key value.

use super::is_count_of_var_or_star;
use crate::graph::core::pattern_matching::{
    EdgeDirection, EdgePattern, NodePattern, Pattern, PatternElement, PropertyMatcher,
};
use crate::graph::languages::cypher::ast::*;
use crate::graph::languages::cypher::executor::{expression_to_string, return_item_column_name};
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

/// The position in `pattern.elements` of the chain variable `expr` counts
/// distinctly: `count(DISTINCT x)` for a node or relationship `x` of the chain.
fn distinct_count_target(expr: &Expression, pattern: &Pattern) -> Option<usize> {
    let Expression::FunctionCall {
        name,
        args,
        distinct: true,
    } = expr
    else {
        return None;
    };
    let [Expression::Variable(var)] = args.as_slice() else {
        return None;
    };
    if !name.eq_ignore_ascii_case("count") {
        return None;
    }
    pattern.elements.iter().position(|element| {
        let variable = match element {
            PatternElement::Node(node) => node.variable.as_deref(),
            PatternElement::Edge(edge) => edge.variable.as_deref(),
        };
        variable == Some(var.as_str())
    })
}

/// Rewrite `MATCH <chain> RETURN count(*)` into `Clause::FusedChainPathCount`,
/// or `... RETURN count(DISTINCT x)` into `Clause::FusedChainDistinctCount`.
///
/// **Precondition:** the statement is exactly one `MATCH` and one `RETURN`.
/// **Pattern matched:** one linear pattern of at least [`MIN_HOPS`]
/// relationships ([`MIN_HOPS_GUARDED`] under a valid-time guard), no path
/// assignment, no residual predicate or hint, `RETURN count(*)` (or `count(v)`
/// of a variable the chain binds) without DISTINCT or HAVING. The distinct
/// form is the single item `count(DISTINCT x)` for a node or relationship
/// variable `x` of a chain of two or more relationships, whose only hint may
/// be the aggregate-only DISTINCT hint naming `x`. **Rewrite:** the pair
/// becomes one clause holding the pattern and the column name (and `x`'s
/// position for the distinct form).
/// **Why-bail:** a repeated variable (identity constraint), a var-length or
/// parameterised hop, a comma pattern, an `OPTIONAL MATCH`, a property
/// matcher that reads a row, `count(DISTINCT x.prop)`, `RETURN DISTINCT`, and
/// hop types that are untyped or shared with another hop unless the count is
/// a two-directed-hop path count. The matcher enforces relationship
/// uniqueness for such chains; the path count corrects for it only in that
/// two-hop case (see `executor/chain_count.rs`), the distinct form not at all,
/// so the rest stay on the matcher. The grouped distinct form is
/// [`fuse_grouped_chain_distinct`], tried first.
pub(crate) fn fuse_chain_path_count(query: &mut CypherQuery, guarded: bool) {
    if fuse_grouped_chain_distinct(query, guarded) {
        return;
    }
    let [Clause::Match(m), Clause::Return(ret)] = query.clauses.as_slice() else {
        return;
    };
    let [pattern] = m.patterns.as_slice() else {
        return;
    };
    let [item] = ret.items.as_slice() else {
        return;
    };
    if !m.path_assignments.is_empty()
        || m.where_clause.is_some()
        || m.limit_hint.is_some()
        || !m.node_anchors.is_empty()
        || ret.distinct
        || ret.having.is_some()
    {
        return;
    }
    let alias = return_item_column_name(item);
    if let Some(target) = distinct_count_target(&item.expression, pattern) {
        // Only the aggregate-only hint on the counted variable is harmless: it
        // describes this very aggregate.
        let hint_ok = m.distinct_node_hint.as_ref().is_none_or(|hint| {
            hint.aggregate_only
                && matches!(&pattern.elements[target],
                    PatternElement::Node(node) if node.variable.as_deref() == Some(hint.var.as_str()))
        });
        if !hint_ok
            || !is_countable_chain(pattern, MIN_HOPS_GUARDED)
            || !fixed_edge_types_are_pairwise_disjoint(pattern)
        {
            return;
        }
        query.clauses = vec![Clause::FusedChainDistinctCount {
            pattern: pattern.clone(),
            target,
            alias,
            group: None,
        }];
        return;
    }
    let min_hops = if guarded { MIN_HOPS_GUARDED } else { MIN_HOPS };
    if m.distinct_node_hint.is_some()
        || !is_chain_count(&item.expression, pattern)
        || !is_countable_chain(pattern, min_hops)
    {
        return;
    }
    let overlapping_types = !fixed_edge_types_are_pairwise_disjoint(pattern);
    if overlapping_types && !has_correctable_overlap(pattern) {
        return;
    }
    query.clauses = vec![Clause::FusedChainPathCount {
        pattern: pattern.clone(),
        overlapping_types,
        alias,
    }];
}

/// The chain node variable a grouping key reads, as the pattern position of
/// that node: `RETURN g` or `RETURN g.prop`.
fn group_key_position(expr: &Expression, pattern: &Pattern) -> Option<usize> {
    let var = match expr {
        Expression::Variable(var) => var,
        Expression::PropertyAccess { variable, .. } => variable,
        _ => return None,
    };
    pattern.elements.iter().position(|element| {
        matches!(element, PatternElement::Node(node) if node.variable.as_deref() == Some(var.as_str()))
    })
}

/// Whether every trailing clause is an ORDER BY over returned columns, a SKIP
/// or a LIMIT, which the fused clause's rows feed unchanged.
fn trailing_clauses_read_columns(rest: &[Clause], columns: &[String]) -> bool {
    rest.iter().all(|clause| match clause {
        Clause::Skip(_) | Clause::Limit(_) => true,
        Clause::OrderBy(order) => order.items.iter().all(|item| {
            matches!(
                &item.expression,
                Expression::Variable(_) | Expression::PropertyAccess { .. }
            ) && columns.contains(&expression_to_string(&item.expression))
        }),
        _ => false,
    })
}

/// The aggregate of a grouped chain RETURN.
enum GroupedCount {
    /// `count(DISTINCT x)`, `x` at this pattern position.
    Distinct(usize),
    /// `count(*)` or `count(v)` of a chain variable: the number of paths.
    Paths,
}

fn grouped_count_kind(expr: &Expression, pattern: &Pattern) -> Option<GroupedCount> {
    if let Some(target) = distinct_count_target(expr, pattern) {
        Some(GroupedCount::Distinct(target))
    } else {
        is_chain_count(expr, pattern).then_some(GroupedCount::Paths)
    }
}

/// Rewrite `MATCH <chain> RETURN <key>, <count>` (the two items in either
/// order, optionally followed by ORDER BY / SKIP / LIMIT over the returned
/// columns) into a grouped `Clause::FusedChainDistinctCount` for
/// `count(DISTINCT x)` or a `Clause::FusedChainGroupedPathCount` for
/// `count(*)` / `count(v)`; true when it did.
///
/// **Precondition:** one `MATCH` of one linear pattern, then a `RETURN` of
/// exactly two items. **Pattern matched:** the same chain as the ungrouped
/// forms (pairwise-disjoint types; two or more relationships for the distinct
/// count, [`MIN_HOPS`] outside a guard and [`MIN_HOPS_GUARDED`] under one for
/// the path count, whose shorter shapes belong to the aggregate fusions; no
/// predicate, hint, path or DISTINCT/HAVING); the other item `g` or `g.prop`
/// for a node variable `g` of the chain. **Rewrite:** the pair becomes one
/// clause carrying the key; the trailing clauses stay and read its columns.
/// **Why-bail:** a second aggregate, `count(DISTINCT x.prop)`, a key over a
/// relationship or two variables, an ORDER BY over anything but a returned
/// column, overlapping hop types.
fn fuse_grouped_chain_distinct(query: &mut CypherQuery, guarded: bool) -> bool {
    let [Clause::Match(m), Clause::Return(ret), rest @ ..] = query.clauses.as_slice() else {
        return false;
    };
    let ([pattern], [first, second]) = (m.patterns.as_slice(), ret.items.as_slice()) else {
        return false;
    };
    if !m.path_assignments.is_empty()
        || m.where_clause.is_some()
        || m.limit_hint.is_some()
        || m.distinct_node_hint.is_some()
        || !m.node_anchors.is_empty()
        || ret.distinct
        || ret.having.is_some()
    {
        return false;
    }
    let (key_first, key_item, count_item, kind) =
        if let Some(kind) = grouped_count_kind(&second.expression, pattern) {
            (true, first, second, kind)
        } else if let Some(kind) = grouped_count_kind(&first.expression, pattern) {
            (false, second, first, kind)
        } else {
            return false;
        };
    let Some(position) = group_key_position(&key_item.expression, pattern) else {
        return false;
    };
    let columns = [
        return_item_column_name(key_item),
        return_item_column_name(count_item),
    ];
    let min_hops = match kind {
        GroupedCount::Distinct(_) => MIN_HOPS_GUARDED,
        GroupedCount::Paths if guarded => MIN_HOPS_GUARDED,
        GroupedCount::Paths => MIN_HOPS,
    };
    if !is_countable_chain(pattern, min_hops)
        || !fixed_edge_types_are_pairwise_disjoint(pattern)
        || !trailing_clauses_read_columns(rest, &columns)
    {
        return false;
    }
    let group = ChainGroupKey {
        position,
        key: key_item.expression.clone(),
        key_alias: columns[0].clone(),
        key_first,
    };
    let fused = match kind {
        GroupedCount::Distinct(target) => Clause::FusedChainDistinctCount {
            pattern: pattern.clone(),
            target,
            alias: columns[1].clone(),
            group: Some(group),
        },
        GroupedCount::Paths => Clause::FusedChainGroupedPathCount {
            pattern: pattern.clone(),
            alias: columns[1].clone(),
            group,
        },
    };
    query.clauses.splice(0..2, [fused]);
    true
}
