//! A write clause's row-invariant value expressions, folded once per clause
//! execution the way a read projection folds its items
//! ([`CypherExecutor::fold_constants_expr`]) — so the statement's budget is
//! charged for `size(range(1, 600))` once, not on every row the clause
//! writes. A non-deterministic function (`rand()`, `randomUUID()`) and any
//! expression reading the row stay per row.

use super::super::ast::{
    CreateClause, CreateElement, CreatePattern, Expression, MergeClause, SetItem, SetPathStep,
};
use super::CypherExecutor;

fn fold_properties(
    executor: &CypherExecutor<'_>,
    properties: &[(String, Expression)],
) -> Vec<(String, Expression)> {
    properties
        .iter()
        .map(|(key, expr)| (key.clone(), executor.fold_constants_expr(expr)))
        .collect()
}

pub(super) fn fold_pattern(
    executor: &CypherExecutor<'_>,
    pattern: &CreatePattern,
) -> CreatePattern {
    let elements = pattern
        .elements
        .iter()
        .map(|element| match element {
            CreateElement::Node(node) => {
                let mut node = node.clone();
                node.properties = fold_properties(executor, &node.properties);
                CreateElement::Node(node)
            }
            CreateElement::Edge(edge) => {
                let mut edge = edge.clone();
                edge.properties = fold_properties(executor, &edge.properties);
                CreateElement::Edge(edge)
            }
        })
        .collect();
    CreatePattern { elements }
}

pub(super) fn fold_create(executor: &CypherExecutor<'_>, create: &CreateClause) -> CreateClause {
    CreateClause {
        patterns: create
            .patterns
            .iter()
            .map(|pattern| fold_pattern(executor, pattern))
            .collect(),
    }
}

pub(super) fn fold_set_items(executor: &CypherExecutor<'_>, items: &[SetItem]) -> Vec<SetItem> {
    items
        .iter()
        .map(|item| match item {
            SetItem::Property {
                variable,
                property,
                path,
                expression,
            } => SetItem::Property {
                variable: variable.clone(),
                property: property.clone(),
                path: path
                    .iter()
                    .map(|step| match step {
                        SetPathStep::Index(expr) => {
                            SetPathStep::Index(executor.fold_constants_expr(expr))
                        }
                        field => field.clone(),
                    })
                    .collect(),
                expression: executor.fold_constants_expr(expression),
            },
            SetItem::Map {
                variable,
                expression,
                replace,
            } => SetItem::Map {
                variable: variable.clone(),
                expression: executor.fold_constants_expr(expression),
                replace: *replace,
            },
            label => label.clone(),
        })
        .collect()
}

pub(super) fn fold_merge(executor: &CypherExecutor<'_>, merge: &MergeClause) -> MergeClause {
    MergeClause {
        pattern: fold_pattern(executor, &merge.pattern),
        on_create: merge
            .on_create
            .as_ref()
            .map(|items| fold_set_items(executor, items)),
        on_match: merge
            .on_match
            .as_ref()
            .map(|items| fold_set_items(executor, items)),
    }
}
