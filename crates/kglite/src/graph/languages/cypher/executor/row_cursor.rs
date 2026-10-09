//! A read query delivered as batches of finished rows, never all at once.
//!
//! The only shape driven this way is the one whose rows are independent of each
//! other: `MATCH <one pattern> [WHERE …] RETURN <plain expressions>`. Each batch
//! is a run of consecutive matches (the matcher walks start nodes in slices, see
//! `match_stream.rs`) projected through the ordinary RETURN projection, so the
//! cells are exactly the ones the materialized route produces and the memory in
//! flight is one batch plus the matcher's widest slice.
//!
//! Everything else (ORDER BY, DISTINCT, aggregation, UNION, a second clause,
//! mutations) needs the whole input before its first output row and stays on
//! the materialized route; [`CypherExecutor::run_row_cursor`] runs that route and
//! hands the finished rows over in batches, flagged as not streamed.

use super::helpers::return_item_column_name;
use super::match_stream;
use super::*;
use crate::graph::languages::cypher::ast::{is_aggregate_expression, is_window_expression};

/// What a query must look like for [`CypherExecutor::run_row_cursor`] to stream it.
pub(crate) struct RowCursorShape<'q> {
    clause: &'q MatchClause,
    where_clause: Option<&'q WhereClause>,
    ret: &'q ReturnClause,
}

/// One step of a cursor run, in order: a single `Open`, then zero or more `Rows`.
pub(crate) enum CursorEvent {
    /// The result columns, and whether the rows that follow are produced
    /// incrementally (`true`) or were materialized before the first batch.
    Open {
        columns: Vec<String>,
        streamed: bool,
    },
    Rows(Vec<Vec<Value>>),
}

/// The shape of `query` when it can stream, else `None`.
pub(crate) fn row_cursor_shape(query: &CypherQuery) -> Option<RowCursorShape<'_>> {
    if query.explain || query.profile || query.guard.is_some() {
        return None;
    }
    let (clause, where_clause, ret) = match query.clauses.as_slice() {
        [Clause::Match(m), Clause::Return(r)] => (m, None, r),
        [Clause::Match(m), Clause::Where(w), Clause::Return(r)] => (m, Some(w), r),
        _ => return None,
    };
    let plain = !ret.distinct
        && ret.having.is_none()
        && !ret.items.is_empty()
        && ret.items.iter().all(|item| {
            !matches!(item.expression, Expression::Star)
                && !is_aggregate_expression(&item.expression)
                && !is_window_expression(&item.expression)
        });
    // A single pattern joins nothing; `WHERE` is only foldable into the MATCH
    // then (`clause_pipeline.rs`), and an unfolded WHERE is a materialized clause.
    (plain && clause.patterns.len() == 1).then_some(RowCursorShape {
        clause,
        where_clause,
        ret,
    })
}

impl<'q> CypherExecutor<'q> {
    /// Run `query` (whose shape is `shape`) and deliver it through `emit` in
    /// batches of up to `batch` rows. `emit` returns `false` to stop early.
    pub(crate) fn run_row_cursor(
        &'q self,
        query: &'q CypherQuery,
        shape: &RowCursorShape<'q>,
        batch: usize,
        emit: &mut dyn FnMut(CursorEvent) -> bool,
    ) -> Result<(), String> {
        let batch = batch.max(1);
        crate::graph::languages::cypher::valid_time::check_executable(query)?;
        if query.context.is_some() {
            self.resolve_graph_filter(query)?;
        }
        let folded_where = shape
            .where_clause
            .map(|w| self.fold_constants_pred(&w.predicate));
        let anchors = match_stream::first_match_anchors(shape.clause);
        let source = if self.first_match_streams(shape.clause) {
            match_stream::open_first_match_rows(
                self,
                shape.clause,
                folded_where.as_ref(),
                anchors.as_ref(),
            )?
        } else {
            None
        };
        let Some(mut rows) = source else {
            return self.run_materialized_cursor(query, batch, emit);
        };
        let mut columns: Vec<String> = shape
            .ret
            .items
            .iter()
            .map(return_item_column_name)
            .collect();
        for (name, shown) in &query.column_display {
            if !columns.contains(shown) {
                if let Some(column) = columns.iter_mut().find(|c| *c == name) {
                    *column = shown.clone();
                }
            }
        }
        if !emit(CursorEvent::Open {
            columns,
            streamed: true,
        }) {
            return Ok(());
        }
        loop {
            self.check_deadline()?;
            let mut chunk = Vec::with_capacity(batch.min(4096));
            while chunk.len() < batch {
                match rows.next() {
                    Some(row) => chunk.push(row?),
                    None => break,
                }
            }
            if chunk.is_empty() {
                break;
            }
            let last = chunk.len() < batch;
            let set = ResultSet {
                rows: chunk,
                columns: Vec::new(),
                lazy_return_items: None,
            };
            let projected = self.execute_return_projection(shape.ret, set, &[])?;
            let mut finished = self.finalize_result(projected)?;
            crate::graph::languages::cypher::result::clear_published_relationship_incarnations(
                &mut finished,
            );
            if !emit(CursorEvent::Rows(finished.rows)) || last {
                break;
            }
        }
        self.raise_graph_filter_error()
    }

    /// The materialized route behind the cursor surface: run `query` whole and
    /// slice the finished rows into batches.
    pub(crate) fn run_materialized_cursor(
        &self,
        query: &CypherQuery,
        batch: usize,
        emit: &mut dyn FnMut(CursorEvent) -> bool,
    ) -> Result<(), String> {
        let result = self.execute(query)?;
        let mut columns = result.columns;
        for (name, shown) in &query.column_display {
            if !columns.contains(shown) {
                if let Some(column) = columns.iter_mut().find(|c| *c == name) {
                    *column = shown.clone();
                }
            }
        }
        if !emit(CursorEvent::Open {
            columns,
            streamed: false,
        }) {
            return Ok(());
        }
        let mut rows = result.rows.into_iter();
        loop {
            let chunk: Vec<Vec<Value>> = rows.by_ref().take(batch.max(1)).collect();
            if chunk.is_empty() || !emit(CursorEvent::Rows(chunk)) {
                return Ok(());
            }
        }
    }
}
