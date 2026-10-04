//! `Clause::FusedValidAtJoin`: `MATCH (x:T …) WHERE valid_at(x, e) …` after an
//! `UNWIND`, answered by scanning the pattern once.
//!
//! The pattern reads nothing from the driving rows, so the unfused plan's
//! per-row scan returns the same matches every time and only `valid_at()`
//! tells the rows apart. Here the scan runs once (through the ordinary
//! matcher, so every inline matcher, anchor and index behaves as before) and
//! each driving row keeps the matches whose `x` the endpoint index's mask for
//! that row's instant admits. A count-mode join over a bare node pattern
//! needs no scan at all: `node_count_at` is two binary searches.
//!
//! The mask is the one `FOR VALID_TIME AS OF` filters with, built for the
//! declaration of `T` alone, so it hides exactly what `valid_at(x, e)` rejects
//! and nothing else. Whatever the executor cannot decide that way — a graph
//! with secondary labels (a node is then judged by every label it carries), a
//! type without an endpoint index (Disk mode, an unreadable bound, the byte
//! cap), an `e` that does not evaluate to an instant, bound names that are not
//! the declared pair — runs the `MATCH` and `WHERE` the rewrite replaced, so
//! the answer and its errors are the unfused plan's.

use std::sync::Arc;

use super::*;
use crate::graph::core::graph_filter::{
    ElementFilter, GraphFilter, GuardBounds, GuardTemplate, NodeGuard, ValidTimeSelector,
};
use crate::graph::features::temporal::endpoint_index;
use crate::graph::features::temporal::eval::{parse_instant, Instant};
use crate::graph::features::temporal::node_type_has_property;

/// The mask-backed filter for one instant: `None` admits every match (the
/// type is timeless then), `Some` tests each node.
type InstantFilter = Option<ElementFilter>;

impl CypherExecutor<'_> {
    pub(super) fn execute_valid_at_join(
        &self,
        join: &ValidAtJoin,
        result_set: ResultSet,
    ) -> Result<ResultSet, String> {
        // An empty pipeline cannot be extended by a MATCH.
        if result_set.rows.is_empty() {
            return Ok(result_set);
        }
        match self.valid_at_join_rows(join, &result_set)? {
            Some(rows) => Ok(ResultSet {
                rows,
                columns: result_set.columns,
                lazy_return_items: None,
            }),
            None => self.valid_at_join_unfused(join, result_set),
        }
    }

    /// The replaced `MATCH` and `WHERE`. In count mode each row stands for
    /// one match, which the `RETURN`'s `sum()` adds up.
    fn valid_at_join_unfused(
        &self,
        join: &ValidAtJoin,
        result_set: ResultSet,
    ) -> Result<ResultSet, String> {
        let matched = self.execute_match(&join.match_clause, result_set, None)?;
        let mut filtered = self.execute_where(&join.where_clause, matched)?;
        if let Some(alias) = &join.count_alias {
            for row in &mut filtered.rows {
                row.projected.insert(alias.clone(), Value::Int64(1));
            }
        }
        Ok(filtered)
    }

    /// The fused answer, or `None` when this graph, this declaration or one
    /// of the instants needs the unfused plan.
    fn valid_at_join_rows(
        &self,
        join: &ValidAtJoin,
        input: &ResultSet,
    ) -> Result<Option<Vec<ResultRow>>, String> {
        let graph = self.graph;
        if self.graph_filter().is_some() || graph.has_secondary_labels {
            return Ok(None);
        }
        let Some(config) = graph.temporal.node(&join.label) else {
            return Ok(None);
        };
        let declared_pair = join
            .named_bounds
            .as_ref()
            .is_none_or(|(from, to)| *from == config.valid_from && *to == config.valid_to);
        if !declared_pair
            || !node_type_has_property(graph, &join.label, &config.valid_from)
            || !node_type_has_property(graph, &join.label, &config.valid_to)
        {
            return Ok(None);
        }
        let template = Arc::new(GuardTemplate {
            nodes: vec![NodeGuard {
                label: join.label.clone(),
                bounds: GuardBounds::of(config),
            }],
            edges: Vec::new(),
        });
        if join.count_alias.is_some()
            && join.residual.is_none()
            && is_bare_node_pattern(&join.match_clause)
        {
            return self.count_valid_nodes(join, &input.rows);
        }

        let scan = self.execute_match(&join.match_clause, ResultSet::new(), None)?;
        let mut matched = Vec::with_capacity(scan.rows.len());
        for row in &scan.rows {
            match row.node_bindings.get(&join.var) {
                Some(&idx) => matched.push(idx),
                None => return Ok(None),
            }
        }
        let residual = join
            .residual
            .clone()
            .map(|predicate| WhereClause { predicate });
        let mut out: Vec<ResultRow> = Vec::new();
        let mut current: Option<(Instant, InstantFilter)> = None;
        let mut work = 0usize;
        for row in &input.rows {
            self.check_interrupt_periodic(work)?;
            work = work.saturating_add(1);
            let Some(instant) = self.row_instant(join, row) else {
                return Ok(None);
            };
            if current.as_ref().is_none_or(|(held, _)| *held != instant) {
                let Some(filter) = self.instant_filter(&template, instant) else {
                    return Ok(None);
                };
                current = Some((instant, filter));
            }
            let filter = &current.as_ref().expect("set above").1;
            let admits = |idx| filter.as_ref().is_none_or(|f| f.admits_node(graph, idx));
            if let Some(alias) = &join.count_alias {
                let n = matched.iter().filter(|&&idx| admits(idx)).count();
                if n > 0 {
                    out.push(counted_row(row, alias, n));
                }
                continue;
            }
            let mut batch = Vec::new();
            for (scan_row, &idx) in scan.rows.iter().zip(&matched) {
                self.check_interrupt_periodic(work)?;
                work = work.saturating_add(1);
                if admits(idx) {
                    self.budget
                        .reserve_rows(out.len() + batch.len(), 1, "MATCH join")?;
                    let mut joined = row.clone();
                    merge_scan_row(&mut joined, scan_row);
                    batch.push(joined);
                }
            }
            match &residual {
                Some(where_clause) => {
                    let kept = self.execute_where(
                        where_clause,
                        ResultSet {
                            rows: batch,
                            columns: input.columns.clone(),
                            lazy_return_items: None,
                        },
                    )?;
                    out.extend(kept.rows);
                }
                None => out.extend(batch),
            }
        }
        Ok(Some(out))
    }

    /// Count mode over `MATCH (x:T)`: one row per driving row whose instant
    /// has a valid node, carrying the count from the endpoint index.
    fn count_valid_nodes(
        &self,
        join: &ValidAtJoin,
        input: &[ResultRow],
    ) -> Result<Option<Vec<ResultRow>>, String> {
        let alias = join.count_alias.as_deref().expect("count mode");
        let mut out = Vec::new();
        for (work, row) in input.iter().enumerate() {
            self.check_interrupt_periodic(work)?;
            let Some(instant) = self.row_instant(join, row) else {
                return Ok(None);
            };
            let Some(n) = endpoint_index::node_count_at(self.graph, &join.label, instant) else {
                return Ok(None);
            };
            if n > 0 {
                out.push(counted_row(row, alias, n));
            }
        }
        Ok(Some(out))
    }

    /// The instant `e` evaluates to on `row`; `None` for an error or a value
    /// that is not an instant, which the unfused plan reports (or never
    /// evaluates, when no row reaches it).
    fn row_instant(&self, join: &ValidAtJoin, row: &ResultRow) -> Option<Instant> {
        let value = self.evaluate_expression(&join.instant, row).ok()?;
        parse_instant(&value).ok()
    }

    /// `None` when `T` has no mask at `instant`.
    fn instant_filter(
        &self,
        template: &Arc<GuardTemplate>,
        instant: Instant,
    ) -> Option<InstantFilter> {
        let filter = GraphFilter {
            template: Arc::clone(template),
            selector: ValidTimeSelector::AsOf(instant),
        };
        let resolved = filter.resolve(self.graph);
        if !resolved.guarded.is_empty() {
            return None;
        }
        Some(ElementFilter::new(&filter, resolved))
    }
}

/// `MATCH (x:T)` with nothing else: the shape whose matches are exactly the
/// nodes the index counts.
fn is_bare_node_pattern(clause: &MatchClause) -> bool {
    clause.node_anchors.is_empty()
        && matches!(
            clause.patterns.as_slice(),
            [pattern] if matches!(
                pattern.elements.as_slice(),
                [PatternElement::Node(node)] if node.properties.is_none()
            )
        )
}

fn counted_row(driving: &ResultRow, alias: &str, n: usize) -> ResultRow {
    let mut row = driving.clone();
    row.projected
        .insert(alias.to_string(), Value::Int64(n as i64));
    row
}

/// The bindings the one scan produced, added to a clone of the driving row.
fn merge_scan_row(row: &mut ResultRow, scan_row: &ResultRow) {
    for (name, idx) in scan_row.node_bindings.iter() {
        row.node_bindings.insert(name.clone(), *idx);
    }
    for (name, edge) in scan_row.edge_bindings.iter() {
        row.edge_bindings.insert(name.clone(), *edge);
    }
    for (name, path) in scan_row.path_bindings.iter() {
        row.path_bindings.insert(name.clone(), path.clone());
    }
    for (name, value) in scan_row.projected.iter() {
        row.projected.insert(name.clone(), value.clone());
    }
}
