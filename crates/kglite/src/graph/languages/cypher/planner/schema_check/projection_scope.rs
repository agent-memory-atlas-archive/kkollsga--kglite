//! What a projection's trailing `ORDER BY` / `SKIP` / `LIMIT` / `WHERE` may read.
//!
//! A non-aggregating, non-`DISTINCT`, window-free `WITH` or `RETURN` emits one
//! row per input row, so the variables it dropped still have a value on every
//! row and the clauses attached to it may read them (`MATCH (a) WITH a.name AS
//! n ORDER BY a.age`). A `DISTINCT` or aggregating projection merges rows: a
//! dropped variable then has no single value per output row, and reading it is
//! refused. Nothing here widens the scope the *next* clause sees; the executor
//! strips the retained names when the clause group ends.

use super::super::super::ast::*;
use super::super::super::executor::helpers::expression_to_string;
use super::super::simplification::collect_expression_refs;
use super::{AggregateOrderScope, SchemaError, SchemaErrorKind};
use std::collections::HashSet;

/// Whether the projection keeps one row per input row.
pub(super) fn is_row_preserving(items: &[ReturnItem], distinct: bool) -> bool {
    !distinct
        && !items.iter().any(|item| {
            is_aggregate_expression(&item.expression) || is_window_expression(&item.expression)
        })
}

/// Reject an `ORDER BY` key that reads a variable a `RETURN DISTINCT`
/// dropped. A key that is itself a projected expression (`RETURN DISTINCT
/// a.name ORDER BY a.name`) or only reads projected columns is fine.
pub(super) fn validate_distinct_order(
    items: &[ReturnItem],
    order: &OrderByClause,
) -> Result<(), SchemaError> {
    let mut columns: HashSet<String> = HashSet::new();
    let mut rendered: HashSet<String> = HashSet::new();
    for item in items {
        if let Some(alias) = &item.alias {
            columns.insert(alias.clone());
        } else if let Expression::Variable(name) = &item.expression {
            columns.insert(name.clone());
        }
        rendered.insert(expression_to_string(&item.expression));
    }
    for key in &order.items {
        if rendered.contains(&expression_to_string(&key.expression)) {
            continue;
        }
        let mut refs = HashSet::new();
        collect_expression_refs(&key.expression, &mut refs);
        if let Some(name) = refs.iter().find(|name| !columns.contains(*name)) {
            return Err(SchemaError {
                kind: SchemaErrorKind::UndefinedVariable,
                message: format!(
                    "Undefined variable '{name}': ORDER BY after RETURN DISTINCT can only read \
                     projected columns, because DISTINCT merges the rows that carried '{name}'."
                ),
            });
        }
    }
    Ok(())
}

/// What the clauses after a projection (its `ORDER BY` / `SKIP` / `LIMIT` and,
/// for a `WITH`, a standalone `WHERE`) may read, carried across the clause
/// loop. Every field is cleared by the first clause that is not one of those.
#[derive(Default)]
pub(super) struct ProjectionTail<'q> {
    /// Set by an aggregating `RETURN`.
    aggregate: Option<AggregateOrderScope>,
    /// Set by a `RETURN DISTINCT` that does not aggregate.
    distinct_items: Option<&'q [ReturnItem]>,
    /// Set by a `WITH`: projected names plus, for a row-preserving one, the
    /// incoming scope.
    with_scope: Option<HashSet<String>>,
    /// The projection just seen has no aggregate item, so an aggregate in its
    /// `ORDER BY` has no grouping to attach to.
    non_aggregating: bool,
}

impl<'q> ProjectionTail<'q> {
    pub(super) fn enter_clause(&mut self, clause: &Clause) {
        if !matches!(
            clause,
            Clause::OrderBy(_) | Clause::Skip(_) | Clause::Limit(_)
        ) {
            self.aggregate = None;
            self.distinct_items = None;
            self.non_aggregating = false;
        }
        if !matches!(
            clause,
            Clause::OrderBy(_) | Clause::Skip(_) | Clause::Limit(_) | Clause::Where(_)
        ) {
            self.with_scope = None;
        }
    }

    pub(super) fn after_return(&mut self, clause: &'q ReturnClause) {
        self.non_aggregating = !clause
            .items
            .iter()
            .any(|item| is_aggregate_expression(&item.expression));
        self.aggregate = AggregateOrderScope::for_projection(&clause.items);
        let star = clause
            .items
            .iter()
            .any(|item| matches!(item.expression, Expression::Star));
        self.distinct_items =
            (clause.distinct && self.aggregate.is_none() && !star).then_some(&clause.items[..]);
    }

    pub(super) fn after_with(&mut self, items: &[ReturnItem], trailing: HashSet<String>) {
        self.non_aggregating = !items
            .iter()
            .any(|item| is_aggregate_expression(&item.expression));
        self.with_scope = Some(trailing);
    }

    /// The scope a trailing expression is checked against.
    pub(super) fn visible<'s>(&'s self, scope: &'s HashSet<String>) -> &'s HashSet<String> {
        self.with_scope.as_ref().unwrap_or(scope)
    }

    pub(super) fn validate_order(&self, order: &OrderByClause) -> Result<(), SchemaError> {
        if self.non_aggregating {
            if let Some(key) = order
                .items
                .iter()
                .find(|key| is_aggregate_expression(&key.expression))
            {
                return Err(SchemaError {
                    kind: SchemaErrorKind::UndefinedVariable,
                    message: format!(
                        "ORDER BY cannot use the aggregate '{}' after a projection that does \
                         not aggregate. Project the aggregate first and order by its alias.",
                        expression_to_string(&key.expression)
                    ),
                });
            }
        }
        if let Some(restriction) = &self.aggregate {
            restriction.validate(order)?;
        }
        if let Some(items) = self.distinct_items {
            validate_distinct_order(items, order)?;
        }
        Ok(())
    }
}
