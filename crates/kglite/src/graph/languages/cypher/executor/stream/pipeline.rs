//! Streaming-pipeline recognizer + assembler.
//!
//! Called by the driver in [`super::super::CypherExecutor::execute`]
//! before each materialized-path single-clause dispatch. If the
//! recognizer matches a clause run the streaming path can absorb, it
//! consumes those clauses, runs the streaming pipeline, and returns the
//! resulting `ResultSet` plus the number of clauses absorbed. The
//! driver then advances its index by that count and continues.
//!
//! # Recognized shapes
//! - **A.** `RETURN/WITH(group, agg)` whose RETURN/WITH consists of
//!   pure variable / property-access group keys plus inline-able
//!   aggregates (`count`, `sum`, `avg`, `min`, `max`, with optional
//!   `DISTINCT`). Streaming aggregate replaces materialize-then-bucket.
//! - **B.** A streaming-aggregate clause optionally followed by
//!   `ORDER BY <expr> [ASC|DESC] LIMIT k` (no DISTINCT, no HAVING,
//!   positive literal LIMIT). Heap top-K replaces full sort + truncate.
//!
//! Anything else returns `None` and the materialized executor handles
//! the clause as before.

use super::super::super::ast::{
    is_aggregate_expression, Clause, Expression, LimitClause, OrderByClause, OrderItem,
    ReturnClause, WhereClause,
};
use super::super::super::result::ResultSet;
use super::super::CypherExecutor;
use super::{aggregate, heap_top_k, RowStream};
use crate::datatypes::values::Value;

/// Outcome of a recognition attempt.
pub(crate) struct StreamingRun {
    /// Number of clauses absorbed by the streaming path. The driver
    /// advances its loop index by this count.
    pub absorbed: usize,
    pub result: ResultSet,
}

/// What [`try_run_streaming`] returns to the driver. `Absorbed` means
/// the pipeline ran successfully; `Bailed` returns the input
/// `ResultSet` unchanged so the caller can pass it to the materialized
/// executor.
pub(crate) enum StreamingOutcome {
    Absorbed(StreamingRun),
    Bailed(ResultSet),
}

/// A recognised streaming clause run: the aggregate clause compiled for
/// [`aggregate::apply`] and the optional heap top-K tail.
pub(crate) struct StreamingPlan {
    return_clause: ReturnClause,
    is_with: bool,
    with_where: Option<WhereClause>,
    group_indices: Vec<usize>,
    agg_indices: Vec<usize>,
    specs: Vec<aggregate::AggSpec>,
    top_k: Option<(Vec<OrderItem>, usize, usize)>,
}

/// Recognize a streaming clause run at the head of `clauses` (the clauses
/// still to execute, starting at the WITH/RETURN). `None` when no shape
/// matches; nothing is consumed, so the caller keeps its source.
pub(crate) fn plan_streaming(clauses: &[Clause]) -> Option<StreamingPlan> {
    // The first absorbed clause must be either a WITH(group, agg) or
    // a RETURN(group, agg). Anything else: bail.
    let (return_clause, is_with, with_where) = match clauses.first()? {
        Clause::With(w) => {
            // WITH delegates to the same agg machinery as RETURN.
            let rc = ReturnClause {
                items: w.items.clone(),
                distinct: w.distinct,
                having: None,
                lazy_eligible: false,
                group_limit_hint: w.group_limit_hint,
            };
            (rc, true, w.where_clause.clone())
        }
        Clause::Return(rc) => (rc.clone(), false, None),
        _ => return None,
    };

    // Must contain at least one aggregate item — otherwise the
    // materialized projection path is fine.
    let has_agg = return_clause
        .items
        .iter()
        .any(|item| is_aggregate_expression(&item.expression));
    if !has_agg {
        return None;
    }

    // RETURN-side guards: streaming path bails on HAVING.
    if return_clause.having.is_some() {
        return None;
    }

    // Try to compile the aggregate specs. If anything is unsupported
    // (collect/std/etc., arithmetic on aggregates, complex group keys),
    // bail.
    let (group_indices, agg_indices, specs) = aggregate::try_compile_specs(&return_clause).ok()?;

    // Look for an optional follow-up `ORDER BY → LIMIT` we can fuse via
    // heap top-K. Only fire when *both* clauses are present; an ORDER
    // BY without LIMIT still materializes everything, so the
    // materialized sort path is fine.
    let top_k = find_top_k(&clauses[1..]);

    Some(StreamingPlan {
        return_clause,
        is_with,
        with_where,
        group_indices,
        agg_indices,
        specs,
        top_k,
    })
}

/// Run a recognised plan over `upstream`.
///
/// **This pipeline stays sequential**, and not because its input resists
/// partitioning: two reasons it is not partitioned.
///
/// 1. *It would not pay.* Partitioning a grouped aggregation's row
///    consumption is the same shape as the materialized path's grouping
///    pass, which was implemented, measured at **0.93-0.98x** across four
///    cardinalities, and removed — see the comment in
///    `aggregation/materialized.rs`. Per-partition accumulator maps cost
///    more to allocate and merge than the per-row work they parallelise.
/// 2. *It would change answers.* `AggState::merge` adds partial sums, so
///    partitioning reassociates `sum` and `avg` over `Float64` and moves
///    the last ULP. The `parallel` flag is documented as never changing a
///    result, and a float sum that depends on thread count is exactly the
///    kind of "identical except sometimes" that doctrine exists to prevent.
///
/// The win for grouped aggregation is across *groups*, not across rows, and
/// that is where `aggregation/materialized.rs` takes it.
pub(crate) fn run_streaming_plan<'q>(
    executor: &'q CypherExecutor<'q>,
    plan: StreamingPlan,
    upstream: RowStream<'q>,
) -> Result<StreamingRun, String> {
    let mut current = aggregate::apply(
        executor,
        upstream,
        &plan.return_clause,
        &plan.group_indices,
        &plan.agg_indices,
        &plan.specs,
    )?;

    let mut absorbed = 1usize; // the WITH/RETURN clause

    if let Some((items, n, top_k_clauses)) = plan.top_k {
        current = heap_top_k::apply(executor, current, &items, n)?;
        absorbed += top_k_clauses;
    }

    let mut result = current.drain()?;

    // WITH ... WHERE: apply the post-projection WHERE on the
    // materialized result, mirroring `execute_with`.
    if plan.is_with {
        if let Some(wc) = plan.with_where {
            result = executor.execute_where(&wc, result)?;
        }
    }

    absorbed_probe::note();
    Ok(StreamingRun { absorbed, result })
}

/// Try to recognize and run a streaming clause run over an already-materialized
/// prefix. `Bailed` hands `result_set` back unchanged; `Err(_)` only when a
/// recognized pipeline fails mid-execution.
pub(crate) fn try_run_streaming<'q>(
    executor: &'q CypherExecutor<'q>,
    clauses: &[Clause],
    result_set: ResultSet,
) -> Result<StreamingOutcome, String> {
    let Some(plan) = plan_streaming(clauses) else {
        return Ok(StreamingOutcome::Bailed(result_set));
    };
    let upstream = RowStream::from_result_set(result_set);
    run_streaming_plan(executor, plan, upstream).map(StreamingOutcome::Absorbed)
}

/// Pattern-match an `OrderBy → Limit` tail. Returns the order items, the
/// resolved K, and the number of clauses consumed (always 2 on success).
fn find_top_k(clauses: &[Clause]) -> Option<(Vec<OrderItem>, usize, usize)> {
    if clauses.len() < 2 {
        return None;
    }
    let order = match &clauses[0] {
        Clause::OrderBy(OrderByClause { items }) => items.clone(),
        _ => return None,
    };
    let limit_count = match &clauses[1] {
        Clause::Limit(LimitClause { count }) => count,
        _ => return None,
    };
    // Limit must be a positive literal integer for top-K. Param /
    // expression LIMITs require eval-with-row, which the streaming
    // path doesn't currently set up.
    let n = match limit_count {
        Expression::Literal(Value::Int64(n)) if *n >= 0 => *n as usize,
        _ => return None,
    };
    Some((order, n, 2))
}

/// Runs the pipeline absorbed, counted per thread in tests so a test can
/// prove a path streamed. A no-op outside tests.
pub(crate) mod absorbed_probe {
    #[cfg(test)]
    thread_local! {
        static RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[inline]
    pub(crate) fn note() {
        #[cfg(test)]
        RUNS.with(|r| r.set(r.get() + 1));
    }

    /// The runs absorbed since the last call.
    #[cfg(test)]
    pub(crate) fn take() -> usize {
        RUNS.with(|r| r.replace(0))
    }
}
