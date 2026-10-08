//! A path variable names one thing: the path a `MATCH` part binds.
//!
//! `p = (a)-->(b)` is refused when `p` is already a variable in scope (any
//! kind), when `p` is also a node or relationship variable of the same clause,
//! or when two path variables of one clause share a name. The reverse is
//! refused too: once `p` names a path, a later pattern cannot use `p` as a node
//! or relationship variable. Re-using a *node* or *relationship* variable stays
//! legal — a later segment that names it is constrained to the bound value.

use super::super::super::ast::{MatchClause, WithClause};
use super::{reject_global_redeclaration, SchemaError, SchemaErrorKind};
use crate::graph::core::pattern_matching::PatternElement;
use crate::graph::languages::cypher::ast::{Expression, ReturnItem};
use std::collections::HashSet;

/// The variables in scope that hold a path, as opposed to a node, a
/// relationship or a value.
#[derive(Default)]
pub(super) struct PathVariables(HashSet<String>);

fn conflict(message: String) -> SchemaError {
    SchemaError {
        kind: SchemaErrorKind::UndefinedVariable,
        message,
    }
}

fn pattern_variables(clause: &MatchClause) -> impl Iterator<Item = &String> {
    clause
        .patterns
        .iter()
        .flat_map(|pattern| pattern.elements.iter())
        .filter_map(|element| match element {
            PatternElement::Node(node) => node.variable.as_ref(),
            PatternElement::Edge(edge) => edge.variable.as_ref(),
        })
}

impl PathVariables {
    /// Check `clause` against the variables already in `scope`, before the
    /// clause binds its own, then record its path variables.
    pub(super) fn enter_match(
        &mut self,
        clause: &MatchClause,
        scope: &HashSet<String>,
        globals: &HashSet<String>,
    ) -> Result<(), SchemaError> {
        for assignment in &clause.path_assignments {
            reject_global_redeclaration(&assignment.variable, globals)?;
        }
        self.check_match(clause, scope)?;
        self.0
            .extend(clause.path_assignments.iter().map(|a| a.variable.clone()));
        Ok(())
    }

    fn check_match(
        &self,
        clause: &MatchClause,
        scope: &HashSet<String>,
    ) -> Result<(), SchemaError> {
        let mut seen = HashSet::new();
        for assignment in &clause.path_assignments {
            let name = &assignment.variable;
            if scope.contains(name) || !seen.insert(name.as_str()) {
                return Err(conflict(format!(
                    "Variable `{name}` is already bound; a path variable cannot reuse a name \
                     that is already in use in the query"
                )));
            }
        }
        for name in pattern_variables(clause) {
            if seen.contains(name.as_str()) || self.0.contains(name) {
                return Err(conflict(format!(
                    "Variable `{name}` names a path and cannot also be a node or relationship \
                     variable"
                )));
            }
        }
        Ok(())
    }

    /// A projection keeps a name a path only when it passes the variable
    /// through unrenamed.
    pub(super) fn project_with(&mut self, clause: &WithClause) {
        if clause
            .items
            .iter()
            .any(|item| matches!(item.expression, Expression::Star))
        {
            self.0
                .retain(|name| !clause.items.iter().any(|item| renames(item, name)));
            return;
        }
        let kept: HashSet<String> = clause
            .items
            .iter()
            .filter_map(|item| match (&item.expression, &item.alias) {
                (Expression::Variable(name), None) if self.0.contains(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        self.0 = kept;
    }
}

/// Whether `item` (alongside `*`) rebinds `name` to something else.
fn renames(item: &ReturnItem, name: &str) -> bool {
    item.alias.as_deref() == Some(name)
        && !matches!(&item.expression, Expression::Variable(source) if source == name)
}
