//! A merging `WITH`'s `ORDER BY` written against the expressions it projected.
//!
//! `WITH a.n AS n, count(*) AS c ORDER BY a.n` sorts on a grouping key by the
//! expression the `WITH` projected it from, but `a` is gone once the rows are
//! grouped (or de-duplicated by `DISTINCT`). A sort key that is, in full, the
//! rendered expression of a non-aggregate item is rewritten to that item's
//! alias, so the schema check, the planner passes and every executor path see
//! the projected column the key stands for.
//!
//! Only whole-key matches against an *aliased* item are rewritten. A key that
//! merely reads the same variables (`ORDER BY a.n + 1`) or names an unaliased
//! item stays as written and keeps the scope check's verdict.

use super::super::ast::*;
use super::super::executor::helpers::expression_to_string;
use std::collections::HashMap;

pub(super) fn rewrite_merging_with_sort_keys(clauses: &mut [Clause]) {
    for i in 0..clauses.len().saturating_sub(1) {
        let (head, tail) = clauses.split_at_mut(i + 1);
        let (Clause::With(with), Clause::OrderBy(order)) = (&head[i], &mut tail[0]) else {
            continue;
        };
        let merges = with.distinct
            || with
                .items
                .iter()
                .any(|item| is_aggregate_expression(&item.expression));
        if !merges
            || with
                .items
                .iter()
                .any(|item| matches!(item.expression, Expression::Star))
        {
            continue;
        }
        let mut aliases: HashMap<String, &str> = HashMap::new();
        for item in &with.items {
            if let (Some(alias), false) = (&item.alias, is_aggregate_expression(&item.expression)) {
                aliases
                    .entry(expression_to_string(&item.expression))
                    .or_insert(alias);
            }
        }
        for key in &mut order.items {
            if matches!(key.expression, Expression::Variable(_)) {
                continue;
            }
            if let Some(alias) = aliases.get(&expression_to_string(&key.expression)) {
                key.expression = Expression::Variable((*alias).to_string());
            }
        }
    }
}
