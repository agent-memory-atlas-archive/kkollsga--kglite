//! Index-seek candidates for the null-safe predicate shapes a query generator
//! (GraphBridge) renders around a property filter.
//!
//! Under WHERE only a TRUE result keeps a row, so for a non-NULL plan-time
//! value `c` the null-safe forms select exactly the rows the plain `x.p = c`
//! / `x.p IN L` select:
//!
//! * `coalesce(x.p = c, (x.p IS NULL AND c IS NULL))` — for a NULL `x.p` the
//!   first operand is NULL and the second is `TRUE AND FALSE`, so the row is
//!   dropped, as plain `=` drops it.
//! * `coalesce(x.p IN L, false) OR (x.p IS NULL AND any(v IN L WHERE v IS
//!   NULL))` — with no NULL element in `L` the second disjunct is FALSE.
//!
//! A NULL `c`, or a NULL element in `L`, makes the NULL-property rows match;
//! those shapes are not matched here and stay a residual filter. The matchers
//! only produce *candidates*: the caller keeps the original predicate in
//! WHERE, so a wrong match cannot widen the result. Both read parameter
//! values at plan time, which is sound because the plan-cache key carries the
//! values (including NULL and list contents) the plan was built from.

use super::super::ast::*;
use crate::datatypes::values::Value;
use std::collections::HashMap;

/// `(variable, property, value)` for `coalesce(x.p = $c, (x.p IS NULL AND
/// $c IS NULL)) <> false` with `$c` bound to a non-NULL scalar.
pub(super) fn null_safe_equality(
    pred: &Predicate,
    match_vars: &[(String, Option<String>)],
    params: &HashMap<String, Value>,
) -> Option<(String, String, Value)> {
    let [eq, default] = coalesce_truthy_args(pred)?.as_slice() else {
        return None;
    };
    let Expression::PredicateExpr(eq) = eq else {
        return None;
    };
    let Predicate::Comparison {
        left,
        operator: ComparisonOp::Equals,
        right,
    } = &**eq
    else {
        return None;
    };
    let ((Expression::PropertyAccess { variable, property }, Expression::Parameter(name))
    | (Expression::Parameter(name), Expression::PropertyAccess { variable, property })) =
        (left, right)
    else {
        return None;
    };
    let Expression::PredicateExpr(default) = default else {
        return None;
    };
    let Predicate::And(a, b) = &**default else {
        return None;
    };
    let null_property = |p: &Predicate| {
        matches!(p, Predicate::IsNull(Expression::PropertyAccess { variable: v, property: pr })
            if v == variable && pr == property)
    };
    let null_param =
        |p: &Predicate| matches!(p, Predicate::IsNull(Expression::Parameter(n)) if n == name);
    if !((null_property(a) && null_param(b)) || (null_param(a) && null_property(b))) {
        return None;
    }
    if !match_vars.iter().any(|(v, _)| v == variable) {
        return None;
    }
    let value = params
        .get(name.as_str())
        .filter(|v| !matches!(v, Value::Null | Value::List(_)))?;
    Some((variable.clone(), property.clone(), value.clone()))
}

/// `(variable, property, values)` for the null-safe IN shape with `$L` bound
/// to a list holding no NULL element.
pub(super) fn null_safe_in(
    left: &Predicate,
    right: &Predicate,
    match_vars: &[(String, Option<String>)],
    params: &HashMap<String, Value>,
) -> Option<(String, String, Vec<Value>)> {
    let (in_arm, null_arm) = match (coalesce_truthy_args(left), coalesce_truthy_args(right)) {
        (Some(_), None) => (left, right),
        (None, Some(_)) => (right, left),
        _ => return None,
    };
    let [contained, Expression::Literal(Value::Boolean(false))] =
        coalesce_truthy_args(in_arm)?.as_slice()
    else {
        return None;
    };
    let Expression::PredicateExpr(contained) = contained else {
        return None;
    };
    let Predicate::InExpression {
        expr: Expression::PropertyAccess { variable, property },
        list_expr: Expression::Parameter(name),
    } = &**contained
    else {
        return None;
    };
    let Predicate::And(a, b) = null_arm else {
        return None;
    };
    let is_null_property = |p: &Predicate| {
        matches!(p, Predicate::IsNull(Expression::PropertyAccess { variable: v, property: pr })
            if v == variable && pr == property)
    };
    let is_any_null = |p: &Predicate| {
        let Predicate::Comparison {
            left:
                Expression::QuantifiedList {
                    quantifier: ListQuantifier::Any,
                    variable: element,
                    list_expr,
                    filter,
                },
            operator: ComparisonOp::NotEquals,
            right: Expression::Literal(Value::Boolean(false)),
        } = p
        else {
            return false;
        };
        matches!(&**list_expr, Expression::Parameter(n) if n == name)
            && matches!(&**filter, Predicate::IsNull(Expression::Variable(v)) if v == element)
    };
    if !((is_null_property(a) && is_any_null(b)) || (is_any_null(a) && is_null_property(b))) {
        return None;
    }
    if !match_vars.iter().any(|(v, _)| v == variable) {
        return None;
    }
    let values =
        super::index_selection::resolve_value_list(&Expression::Parameter(name.clone()), params)?;
    if values.iter().any(|v| matches!(v, Value::Null))
        || matches!(params.get(name.as_str()), Some(Value::Null))
    {
        return None;
    }
    Some((variable.clone(), property.clone(), values))
}

/// The arguments of `coalesce(a, b) <> false`, the WHERE-position form of a
/// bare `coalesce(...)` operand.
fn coalesce_truthy_args(pred: &Predicate) -> Option<&Vec<Expression>> {
    let Predicate::Comparison {
        left:
            Expression::FunctionCall {
                name,
                args,
                distinct: false,
            },
        operator: ComparisonOp::NotEquals,
        right: Expression::Literal(Value::Boolean(false)),
    } = pred
    else {
        return None;
    };
    (name.eq_ignore_ascii_case("coalesce") && args.len() == 2).then_some(args)
}
