//! Relationship-index anchoring: move `WHERE r.p = <value>` into the edge
//! pattern so the matcher can seed the pattern from a relationship index.
//!
//! The matcher's edge-anchored start (`core::pattern_matching::matcher_edge_anchor`)
//! reads the edge pattern's inline properties, so `MATCH ()-[r:T {p: $x}]->()`
//! is served from the index as written. This pass makes the `WHERE` spelling
//! reach the same shape:
//!
//! ```text
//! MATCH ()-[c:T]->() WHERE c.rid = r.rid     -- r bound before the clause
//!   =>  MATCH ()-[c:T {rid: <r.rid per row>}]->() WHERE c.rid = r.rid
//! ```
//!
//! **Precondition:** the graph declares a relationship index on `(T, p)`, the
//! relationship is a single fixed-length hop of exactly one type, and the
//! conjunct is a top-level equality (or `IN` over a literal / bound list) of
//! `c.p` against a literal, a bound parameter, or an expression that reads only
//! names bound before the clause.
//!
//! **Rewrite:** inserts an inline property matcher on the edge. The `WHERE`
//! stays in place as the safety net every pushdown pass keeps, so a matcher
//! that is looser than the predicate removes no row and one that is not
//! consulted changes nothing.
//!
//! **Why bail:** a property the pattern already constrains (the inline form
//! wins), a null literal (never equal; the predicate stays and decides), a
//! right-hand side that reads a name this clause binds (it has no value before
//! the match runs), a variable-length or multi-type hop, a disjunction (only
//! the conjunctive spine is read).

use super::super::ast::*;
use crate::datatypes::values::Value;
use crate::graph::core::membership::MembershipSet;
use crate::graph::core::pattern_matching::{Pattern, PatternElement, PropertyMatcher};
use crate::graph::schema::DirGraph;
use std::collections::HashMap;

/// `anchor_rel_property_index` pass entry.
pub(super) fn pass_anchor_rel_property_index(query: &mut CypherQuery, ctx: &super::PassCtx) {
    if !ctx.graph.has_rel_property_indices() {
        return;
    }
    for i in 0..query.clauses.len() {
        let mut predicates: Vec<Predicate> = Vec::new();
        match &query.clauses[i] {
            Clause::Match(m) | Clause::OptionalMatch(m) => {
                if let Some(w) = &m.where_clause {
                    predicates.push(w.predicate.clone());
                }
            }
            _ => continue,
        }
        if let Some(Clause::Where(w)) = query.clauses.get(i + 1) {
            predicates.push(w.predicate.clone());
        }
        if predicates.is_empty() {
            continue;
        }
        let (Clause::Match(clause) | Clause::OptionalMatch(clause)) = &mut query.clauses[i] else {
            continue;
        };
        let bound_here = clause_variables(clause);
        for predicate in &predicates {
            push_conjuncts(predicate, &mut clause.patterns, &bound_here, ctx);
        }
    }
}

/// Every variable this clause's own patterns and path assignments bind.
fn clause_variables(clause: &MatchClause) -> Vec<String> {
    let mut vars: Vec<String> = Vec::new();
    for pattern in &clause.patterns {
        for element in &pattern.elements {
            let var = match element {
                PatternElement::Node(n) => n.variable.as_ref(),
                PatternElement::Edge(e) => e.variable.as_ref(),
            };
            if let Some(var) = var {
                vars.push(var.clone());
            }
        }
    }
    vars.extend(clause.path_assignments.iter().map(|p| p.variable.clone()));
    vars
}

fn push_conjuncts(
    predicate: &Predicate,
    patterns: &mut [Pattern],
    bound_here: &[String],
    ctx: &super::PassCtx,
) {
    match predicate {
        Predicate::And(left, right) => {
            push_conjuncts(left, patterns, bound_here, ctx);
            push_conjuncts(right, patterns, bound_here, ctx);
        }
        Predicate::Comparison {
            left,
            operator: ComparisonOp::Equals,
            right,
        } => {
            for (access, value) in [(left, right), (right, left)] {
                let Expression::PropertyAccess { variable, property } = access else {
                    continue;
                };
                if let Some(matcher) = equality_matcher(value, bound_here, ctx.params) {
                    if install(patterns, variable, property, matcher, ctx.graph) {
                        break;
                    }
                }
            }
        }
        Predicate::In {
            expr: Expression::PropertyAccess { variable, property },
            list,
        } => {
            let values: Option<Vec<Value>> = list
                .iter()
                .map(|item| match item {
                    Expression::Literal(v) => Some(v.clone()),
                    _ => None,
                })
                .collect();
            if let Some(values) = values {
                install(
                    patterns,
                    variable,
                    property,
                    PropertyMatcher::In(MembershipSet::new(values)),
                    ctx.graph,
                );
            }
        }
        Predicate::InLiteralSet {
            expr: Expression::PropertyAccess { variable, property },
            values,
        } => {
            install(
                patterns,
                variable,
                property,
                PropertyMatcher::In(values.clone()),
                ctx.graph,
            );
        }
        Predicate::InExpression {
            expr: Expression::PropertyAccess { variable, property },
            list_expr,
        } => {
            let list = match list_expr {
                Expression::Parameter(name) => ctx.params.get(name.as_str()),
                Expression::Literal(v) => Some(v),
                _ => None,
            };
            if let Some(items) = list.and_then(|v| {
                super::super::executor::helpers::list_operand(v, "IN")
                    .ok()
                    .map(|items| items.map(std::borrow::Cow::into_owned).unwrap_or_default())
            }) {
                install(
                    patterns,
                    variable,
                    property,
                    PropertyMatcher::In(MembershipSet::new(items)),
                    ctx.graph,
                );
            }
        }
        _ => {}
    }
}

/// The matcher an equality's value side becomes, or `None` when it cannot be
/// evaluated before the match runs.
fn equality_matcher(
    value: &Expression,
    bound_here: &[String],
    params: &HashMap<String, Value>,
) -> Option<PropertyMatcher> {
    match value {
        Expression::Literal(Value::Null) => None,
        Expression::Literal(v) => Some(PropertyMatcher::Equals(v.clone())),
        Expression::Parameter(name) => match params.get(name.as_str())? {
            Value::Null => None,
            v => Some(PropertyMatcher::Equals(v.clone())),
        },
        Expression::PropertyAccess { variable, .. } | Expression::Variable(variable)
            if !bound_here.iter().any(|n| n == variable) =>
        {
            Some(PropertyMatcher::EqualsExpr(Box::new(value.clone())))
        }
        _ => None,
    }
}

/// Add `property: matcher` to the single-hop, single-type edge bound to
/// `variable`, when `(type, property)` is indexed and the pattern does not
/// already constrain it. Reports whether it did.
fn install(
    patterns: &mut [Pattern],
    variable: &str,
    property: &str,
    matcher: PropertyMatcher,
    graph: &DirGraph,
) -> bool {
    for pattern in patterns.iter_mut() {
        for element in pattern.elements.iter_mut() {
            let PatternElement::Edge(edge) = element else {
                continue;
            };
            if edge.variable.as_deref() != Some(variable) || edge.var_length.is_some() {
                continue;
            }
            let rel_type = match (&edge.connection_type, &edge.connection_types) {
                (Some(only), None) => only.as_str(),
                (Some(first), Some(all)) if all.len() == 1 => first.as_str(),
                _ => return false,
            };
            if !graph.has_rel_property_index(rel_type, property) {
                return false;
            }
            let props = edge.properties.get_or_insert_with(HashMap::new);
            if props.contains_key(property) {
                return false;
            }
            props.insert(property.to_string(), matcher);
            return true;
        }
    }
    false
}
