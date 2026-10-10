//! Scope carry for a row-preserving `WITH`'s trailing clauses.
//!
//! `MATCH (a) WITH a.name AS n ORDER BY a.age` sorts on a variable the `WITH`
//! dropped. The projection is 1:1 (the schema check admits the read only for a
//! non-`DISTINCT`, non-aggregating, window-free `WITH`), so the dropped
//! variable's binding is put back on its own row for the `WITH`'s `WHERE` and
//! the `ORDER BY` / `SKIP` / `LIMIT` / `WHERE` group that follows, and removed
//! when the group ends so a later clause never sees it.

use super::return_clause::is_wildcard_item;
use super::*;
use crate::graph::languages::cypher::planner::simplification::{
    collect_expression_refs, collect_predicate_refs,
};
use std::collections::HashSet;

/// The bindings one row held, under the names a `WITH` dropped.
pub(super) struct SavedBindings {
    nodes: Vec<(String, petgraph::graph::NodeIndex)>,
    edges: Vec<(String, EdgeBinding)>,
    paths: Vec<(String, PathBinding)>,
}

pub(super) fn save_bindings(result_set: &ResultSet, dropped: &[String]) -> Vec<SavedBindings> {
    if dropped.is_empty() {
        return Vec::new();
    }
    result_set
        .rows
        .iter()
        .map(|row| {
            let mut saved = SavedBindings {
                nodes: Vec::new(),
                edges: Vec::new(),
                paths: Vec::new(),
            };
            for name in dropped {
                if let Some(v) = row.node_bindings.get(name) {
                    saved.nodes.push((name.clone(), *v));
                }
                if let Some(v) = row.edge_bindings.get(name) {
                    saved.edges.push((name.clone(), *v));
                }
                if let Some(v) = row.path_bindings.get(name) {
                    saved.paths.push((name.clone(), v.clone()));
                }
            }
            saved
        })
        .collect()
}

/// Put `saved` back, row for row. A name the projection itself produced wins.
pub(super) fn restore_bindings(result_set: &mut ResultSet, saved: Vec<SavedBindings>) {
    debug_assert!(saved.is_empty() || saved.len() == result_set.rows.len());
    for (row, saved) in result_set.rows.iter_mut().zip(saved) {
        for (name, v) in saved.nodes {
            if !row.node_bindings.contains_key(&name) {
                row.node_bindings.insert(name, v);
            }
        }
        for (name, v) in saved.edges {
            if !row.edge_bindings.contains_key(&name) {
                row.edge_bindings.insert(name, v);
            }
        }
        for (name, v) in saved.paths {
            if !row.path_bindings.contains_key(&name) {
                row.path_bindings.insert(name, v);
            }
        }
    }
}

fn continues_group(clause: &Clause) -> bool {
    matches!(
        clause,
        Clause::OrderBy(_) | Clause::Skip(_) | Clause::Limit(_) | Clause::Where(_)
    )
}

/// The variables the `WITH` at `clauses[i]` drops but its own `WHERE` or the
/// `ORDER BY` / `SKIP` / `LIMIT` / `WHERE` clauses right behind it read.
/// Empty for any other clause, and for a `DISTINCT`, aggregating, window or
/// `*` projection (nothing is retained there; the schema check refuses the
/// reads).
pub(super) fn dropped_reads(clauses: &[Clause], i: usize) -> Vec<String> {
    let Some(Clause::With(with)) = clauses.get(i) else {
        return Vec::new();
    };
    if with.distinct
        || with.items.iter().any(|item| {
            is_wildcard_item(item)
                || is_aggregate_expression(&item.expression)
                || is_window_expression(&item.expression)
        })
    {
        return Vec::new();
    }
    let mut reads = HashSet::new();
    if let Some(where_clause) = &with.where_clause {
        collect_predicate_refs(&where_clause.predicate, &mut reads);
    }
    for clause in clauses[i + 1..].iter().take_while(|c| continues_group(c)) {
        match clause {
            Clause::OrderBy(order) => {
                for item in &order.items {
                    collect_expression_refs(&item.expression, &mut reads);
                }
            }
            Clause::Where(where_clause) => {
                collect_predicate_refs(&where_clause.predicate, &mut reads);
            }
            _ => {}
        }
    }
    for item in &with.items {
        reads.remove(&return_item_column_name(item));
    }
    let mut names: Vec<String> = reads.into_iter().collect();
    names.sort();
    names
}

/// Run after `clauses[i]` when it ends a retaining group: remove what the
/// group kept alive.
pub(super) fn strip_retained(clauses: &[Clause], i: usize, result_set: &mut ResultSet) {
    if clauses.get(i + 1).is_some_and(continues_group) {
        return;
    }
    let mut start = i;
    while start > 0 && continues_group(&clauses[start]) {
        start -= 1;
    }
    let dropped = dropped_reads(clauses, start);
    if dropped.is_empty() {
        return;
    }
    for row in &mut result_set.rows {
        for name in &dropped {
            row.node_bindings.remove(name);
            row.edge_bindings.remove(name);
            row.path_bindings.remove(name);
            row.projected.remove(name);
        }
    }
}

impl<'a> CypherExecutor<'a> {
    /// Dispatch one read clause with the scope carry above applied.
    pub(super) fn execute_scoped_read_clause(
        &self,
        clauses: &[Clause],
        i: usize,
        clause: &Clause,
        result_set: ResultSet,
    ) -> Result<ResultSet, String> {
        let mut result = if let Clause::With(with) = clause {
            let dropped = dropped_reads(clauses, i);
            self.execute_with_retaining(with, result_set, &dropped)?
        } else {
            self.execute_single_clause(clause, result_set)?
        };
        if matches!(clause, Clause::With(_)) || continues_group(clause) {
            strip_retained(clauses, i, &mut result);
        }
        Ok(result)
    }
}
