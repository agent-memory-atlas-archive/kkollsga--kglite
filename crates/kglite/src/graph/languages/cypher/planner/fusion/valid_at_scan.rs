//! `UNWIND <instants> AS d MATCH (x:T …) WHERE valid_at(x, e) …`: one scan of
//! the pattern, filtered per instant, in place of one scan per instant.
//!
//! The unfused plan joins the pattern to every driving row and tests
//! `valid_at()` on each of the `rows × matches` joined rows, so `k` instants
//! over `N` versions build `k × N` rows before the filter drops most of them.
//! `executor/valid_at_join.rs` holds the operator this module mints.

use std::collections::HashSet;

use super::is_count_of_var_or_star;
use crate::datatypes::values::Value;
use crate::graph::core::pattern_matching::{PatternElement, PropertyMatcher};
use crate::graph::languages::cypher::ast::*;
use crate::graph::languages::cypher::executor::{is_mutation_query, return_item_column_name};
use crate::graph::languages::cypher::planner::simplification::{
    collect_expression_refs, visible_variables,
};
use crate::graph::languages::cypher::planner::PassCtx;

/// The variable the count rewrite binds per driving row. It is not a legal
/// identifier, so no query variable can collide with it.
const COUNT_VARIABLE: &str = "(valid_at_count)";

/// Functions whose value changes between calls, which one evaluation per
/// driving row would not reproduce.
const VOLATILE_FUNCTIONS: &[&str] = &["rand", "random", "randomuuid", "timestamp"];

/// **Pass:** `fuse_unwind_valid_at` — **Precondition:** a top-level
/// (non-imported) scope without write clauses and without a valid-time guard,
/// holding `UNWIND …` immediately followed by `MATCH <one pattern>` and
/// `WHERE <conjuncts>`.
///
/// **Pattern matched:** the pattern names a node `x` with exactly one label
/// `T`; a top-level `AND` conjunct is `valid_at(x, e)` (or the four-argument
/// form with literal bound names), `e` reading only variables bound before the
/// `MATCH` and calling no volatile function; no pattern variable is bound
/// before the `MATCH`, no matcher reads a row, there is no variable-length
/// hop, path assignment, hint or `OPTIONAL`. Either the `valid_at()` conjunct
/// is the first one, or every other conjunct is a total comparison over the
/// pattern's own nodes (so skipping rows invalid at the instant cannot skip a
/// predicate error the unfused plan raises).
///
/// **Rewrite:** the `MATCH` and `WHERE` become one `FusedValidAtJoin` holding
/// both clauses (the fallback), `x`, `T`, `e` and the `WHERE` without the
/// conjunct. When a `RETURN` follows whose items are `count(*)` (or `count(x)`)
/// and expressions of the driving variables, with no residual `WHERE`, the
/// join also emits one counted row per driving row and the `RETURN`'s counts
/// become `sum()` of that column under the original column names.
///
/// **Why-bail:** a guarded scope (the operator reads the graph without the
/// statement's filter — `GUARD_DENIED_PASSES`); a `CALL { }` body with
/// imports (an imported name may already bind `x`); a secondary label or label
/// alternation on `x`; a conjunct inside `OR`/`NOT`/`XOR`; a relationship
/// variable as `valid_at()`'s subject; anything the executor cannot decide at
/// run time (no endpoint index, an `e` that is not an instant) runs the
/// replaced clauses unchanged. The pass never reads the graph or parameters,
/// so a cached plan stays valid across writes and `$param` lists.
pub(crate) fn pass_fuse_unwind_valid_at(query: &mut CypherQuery, ctx: &PassCtx) {
    if !ctx.initial_scope.is_empty() || !ctx.global_scope.is_empty() || is_mutation_query(query) {
        return;
    }
    for i in 1..query.clauses.len().saturating_sub(1) {
        let Some(mut join) = analyse(&query.clauses, i) else {
            continue;
        };
        apply_count_rewrite(&mut join, &mut query.clauses[i + 2..]);
        query.clauses[i] = Clause::FusedValidAtJoin(Box::new(join));
        query.clauses.remove(i + 1);
        return;
    }
}

/// The join `clauses[i..i + 2]` (`MATCH`, `WHERE`) can become, if any.
fn analyse(clauses: &[Clause], i: usize) -> Option<ValidAtJoin> {
    if !matches!(clauses[i - 1], Clause::Unwind(_)) {
        return None;
    }
    let (Clause::Match(m), Clause::Where(w)) = (&clauses[i], &clauses[i + 1]) else {
        return None;
    };
    if !clauses[..i].iter().all(is_enumerable)
        || !m.path_assignments.is_empty()
        || m.where_clause.is_some()
        || m.limit_hint.is_some()
        || m.distinct_node_hint.is_some()
    {
        return None;
    }
    let [pattern] = m.patterns.as_slice() else {
        return None;
    };
    let pattern_vars = pattern_variables(pattern)?;
    let upstream = visible_variables(&clauses[..i]);
    if pattern_vars.iter().any(|v| upstream.contains(v)) {
        return None;
    }

    let mut conjuncts = Vec::new();
    flatten_and(&w.predicate, &mut conjuncts);
    let (position, call) = conjuncts
        .iter()
        .enumerate()
        .find_map(|(p, c)| valid_at_call(c).map(|call| (p, call)))?;
    let label = single_label(pattern, call.var)?;
    let mut refs = HashSet::new();
    collect_expression_refs(call.instant, &mut refs);
    if !instant_is_reproducible(call.instant)
        || !refs.iter().all(|r| upstream.contains(r))
        || refs.iter().any(|r| pattern_vars.contains(r))
    {
        return None;
    }
    let rest: Vec<&Predicate> = conjuncts
        .iter()
        .enumerate()
        .filter(|(p, _)| *p != position)
        .map(|(_, c)| *c)
        .collect();
    let node_vars = node_variables(pattern);
    if position != 0 && !rest.iter().all(|c| is_total(c, &node_vars)) {
        return None;
    }
    let residual = rest
        .into_iter()
        .cloned()
        .reduce(|left, right| Predicate::And(Box::new(left), Box::new(right)));
    Some(ValidAtJoin {
        match_clause: m.clone(),
        where_clause: w.clone(),
        var: call.var.to_string(),
        label,
        instant: call.instant.clone(),
        named_bounds: call.named,
        residual,
        count_alias: None,
    })
}

/// Clause kinds whose bindings `visible_variables` models completely, so
/// "bound before the MATCH" is decided on complete information.
fn is_enumerable(clause: &Clause) -> bool {
    matches!(
        clause,
        Clause::Match(_)
            | Clause::OptionalMatch(_)
            | Clause::Where(_)
            | Clause::With(_)
            | Clause::Unwind(_)
    )
}

/// Every variable the pattern names, or `None` when a matcher reads a row, a
/// hop is variable-length, or a label/type slot is a parameter.
fn pattern_variables(
    pattern: &crate::graph::core::pattern_matching::Pattern,
) -> Option<Vec<String>> {
    let mut vars = Vec::new();
    for element in &pattern.elements {
        let (variable, properties, slots_are_literal) = match element {
            PatternElement::Node(n) => (&n.variable, &n.properties, n.label_params.is_empty()),
            PatternElement::Edge(e) => (
                &e.variable,
                &e.properties,
                e.type_params.is_empty() && e.var_length.is_none(),
            ),
        };
        let reads_row = properties.as_ref().is_some_and(|props| {
            props.values().any(|m| {
                matches!(
                    m,
                    PropertyMatcher::EqualsVar(_)
                        | PropertyMatcher::EqualsNodeProp { .. }
                        | PropertyMatcher::EqualsExpr(_)
                )
            })
        });
        if !slots_are_literal || reads_row {
            return None;
        }
        vars.extend(variable.iter().cloned());
    }
    Some(vars)
}

fn node_variables(pattern: &crate::graph::core::pattern_matching::Pattern) -> Vec<String> {
    pattern
        .elements
        .iter()
        .filter_map(|element| match element {
            PatternElement::Node(n) => n.variable.clone(),
            PatternElement::Edge(_) => None,
        })
        .collect()
}

/// `x`'s one label, when the pattern has a node `x` carrying exactly that.
fn single_label(
    pattern: &crate::graph::core::pattern_matching::Pattern,
    var: &str,
) -> Option<String> {
    pattern.elements.iter().find_map(|element| match element {
        PatternElement::Node(n)
            if n.variable.as_deref() == Some(var)
                && n.extra_labels.is_empty()
                && n.alt_labels.is_none() =>
        {
            n.node_type.clone()
        }
        _ => None,
    })
}

fn flatten_and<'p>(predicate: &'p Predicate, out: &mut Vec<&'p Predicate>) {
    match predicate {
        Predicate::And(left, right) => {
            flatten_and(left, out);
            flatten_and(right, out);
        }
        other => out.push(other),
    }
}

struct ValidAtCall<'p> {
    var: &'p str,
    instant: &'p Expression,
    named: Option<(String, String)>,
}

/// `valid_at(x, e)` / `valid_at(x, e, 'from', 'to')` written as a bare
/// predicate (the parser's `<> false`, or an explicit `= true`).
fn valid_at_call(predicate: &Predicate) -> Option<ValidAtCall<'_>> {
    let Predicate::Comparison {
        left:
            Expression::FunctionCall {
                name,
                args,
                distinct: false,
            },
        operator,
        right: Expression::Literal(Value::Boolean(expected)),
    } = predicate
    else {
        return None;
    };
    let asks_true = match operator {
        ComparisonOp::NotEquals => !expected,
        ComparisonOp::Equals => *expected,
        _ => return None,
    };
    if !asks_true || !name.eq_ignore_ascii_case("valid_at") {
        return None;
    }
    let (Expression::Variable(var), instant) = (args.first()?, args.get(1)?) else {
        return None;
    };
    let named = match args.as_slice() {
        [_, _] => None,
        [_, _, Expression::Literal(Value::String(from)), Expression::Literal(Value::String(to))] => {
            Some((from.clone(), to.clone()))
        }
        _ => return None,
    };
    Some(ValidAtCall {
        var,
        instant,
        named,
    })
}

/// Whether evaluating `expr` once per driving row gives what the unfused plan
/// computes at each joined row: variables, literals, parameters, arithmetic
/// and non-volatile, non-aggregate function calls over them.
fn instant_is_reproducible(expr: &Expression) -> bool {
    match expr {
        Expression::Literal(_)
        | Expression::Parameter(_)
        | Expression::Variable(_)
        | Expression::PropertyAccess { .. } => true,
        Expression::FunctionCall { name, args, .. } => {
            !VOLATILE_FUNCTIONS
                .iter()
                .any(|volatile| name.eq_ignore_ascii_case(volatile))
                && !is_aggregate_expression(expr)
                && args.iter().all(instant_is_reproducible)
        }
        Expression::Add(l, r)
        | Expression::Subtract(l, r)
        | Expression::Multiply(l, r)
        | Expression::Divide(l, r)
        | Expression::Modulo(l, r)
        | Expression::Concat(l, r) => instant_is_reproducible(l) && instant_is_reproducible(r),
        Expression::Negate(inner) | Expression::ExprPropertyAccess { expr: inner, .. } => {
            instant_is_reproducible(inner)
        }
        Expression::IndexAccess { expr, index } => {
            instant_is_reproducible(expr) && instant_is_reproducible(index)
        }
        Expression::ListLiteral(items) => items.iter().all(instant_is_reproducible),
        _ => false,
    }
}

/// A predicate that cannot raise: comparisons (never `=~`, whose pattern may
/// not compile) between literals and properties of the pattern's own nodes,
/// and `IS [NOT] NULL` / boolean combinations of those.
fn is_total(predicate: &Predicate, node_vars: &[String]) -> bool {
    let operand = |e: &Expression| match e {
        Expression::Literal(_) => true,
        Expression::PropertyAccess { variable, .. } => node_vars.contains(variable),
        _ => false,
    };
    match predicate {
        Predicate::Comparison {
            left,
            operator,
            right,
        } => *operator != ComparisonOp::RegexMatch && operand(left) && operand(right),
        Predicate::IsNull(e) | Predicate::IsNotNull(e) => operand(e),
        Predicate::And(a, b) | Predicate::Or(a, b) | Predicate::Xor(a, b) => {
            is_total(a, node_vars) && is_total(b, node_vars)
        }
        Predicate::Not(inner) => is_total(inner, node_vars),
        _ => false,
    }
}

/// If the clauses after the join are a grouped `RETURN` whose counts the join
/// can emit per driving row, switch the join to count mode and rewrite those
/// counts.
///
/// A driving row with no matching row emits nothing, as it contributes no
/// group to the unfused `count(*)`; a `RETURN` with no group key is left alone
/// because it answers `0` over no rows.
fn apply_count_rewrite(join: &mut ValidAtJoin, tail: &mut [Clause]) {
    if join.residual.is_some() {
        return;
    }
    let Some((Clause::Return(ret), rest)) = tail.split_first_mut() else {
        return;
    };
    let pattern_vars: Vec<String> =
        pattern_variables(&join.match_clause.patterns[0]).unwrap_or_default();
    let is_count = |e: &Expression| {
        is_count_of_var_or_star(e, None) || is_count_of_var_or_star(e, Some(&join.var))
    };
    let reads_pattern = |refs: &HashSet<String>| refs.iter().any(|r| pattern_vars.contains(r));
    let mut counts = 0;
    let mut keys = 0;
    for item in &ret.items {
        if is_count(&item.expression) {
            counts += 1;
            continue;
        }
        let mut refs = HashSet::new();
        collect_expression_refs(&item.expression, &mut refs);
        if matches!(item.expression, Expression::Star)
            || is_aggregate_expression(&item.expression)
            || reads_pattern(&refs)
        {
            return;
        }
        keys += 1;
    }
    let rest_is_plain = rest.iter().all(|clause| {
        let exprs: Vec<&Expression> = match clause {
            Clause::OrderBy(ob) => ob.items.iter().map(|item| &item.expression).collect(),
            Clause::Skip(s) => vec![&s.count],
            Clause::Limit(l) => vec![&l.count],
            _ => return false,
        };
        exprs.into_iter().all(|e| {
            let mut refs = HashSet::new();
            collect_expression_refs(e, &mut refs);
            !reads_pattern(&refs) && !has_function_call(e)
        })
    });
    if counts == 0 || keys == 0 || ret.having.is_some() || !rest_is_plain {
        return;
    }
    for item in &mut ret.items {
        if is_count(&item.expression) {
            let column = return_item_column_name(item);
            item.expression = Expression::FunctionCall {
                name: "sum".to_string(),
                args: vec![Expression::Variable(COUNT_VARIABLE.to_string())],
                distinct: false,
            };
            item.alias = Some(column);
        }
    }
    join.count_alias = Some(COUNT_VARIABLE.to_string());
}

fn has_function_call(expr: &Expression) -> bool {
    let mut found = false;
    fn walk(expr: &Expression, found: &mut bool) {
        match expr {
            Expression::FunctionCall { .. } => *found = true,
            Expression::Add(l, r)
            | Expression::Subtract(l, r)
            | Expression::Multiply(l, r)
            | Expression::Divide(l, r)
            | Expression::Modulo(l, r)
            | Expression::Concat(l, r) => {
                walk(l, found);
                walk(r, found);
            }
            Expression::Negate(inner) => walk(inner, found),
            Expression::Variable(_)
            | Expression::PropertyAccess { .. }
            | Expression::Literal(_)
            | Expression::Parameter(_) => {}
            _ => *found = true,
        }
    }
    walk(expr, &mut found);
    found
}

#[cfg(test)]
#[path = "valid_at_scan_tests.rs"]
mod tests;
