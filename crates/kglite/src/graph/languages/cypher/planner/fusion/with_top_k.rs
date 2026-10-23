//! Top-K fusion at a `WITH` boundary.
//!
//! `fuse_vector_score_order_limit`, `fuse_text_bm25_order_limit` and
//! `fuse_order_by_top_k` match `RETURN, ORDER BY, LIMIT`, so a clause behind
//! the LIMIT left the `WITH … ORDER BY … LIMIT` form unfused: every row was
//! scored and sorted, and a vector top-K never reached the HNSW index.
//! Measured on 3,000 concepts (dim 128), `WITH c, vector_score(c, …) AS s
//! ORDER BY s DESC LIMIT 8 OPTIONAL MATCH …` cost 2.2 ms against 0.05 ms for
//! the top-K alone.

use super::topk::{match_scored_order_limit, resolve_fused_sort_keys};
use crate::datatypes::values::Value;
use crate::graph::languages::cypher::ast::*;
use crate::graph::languages::cypher::executor::is_mutation_query;
use crate::graph::languages::cypher::planner::PassCtx;

/// **Pass:** `fuse_with_top_k` — not applied inside a `CALL { }` body (W5).
pub(in crate::graph::languages::cypher::planner) fn pass_fuse_with_top_k(
    query: &mut CypherQuery,
    ctx: &PassCtx,
) {
    if !ctx.nested {
        fuse_with_top_k(query)
    }
}

/// The pass body. **Pass:** `fuse_with_top_k` — **Precondition:** a read-only statement with
/// `With, OrderBy, Limit` at any position after the first clause.
/// **Pattern matched:** those three adjacent clauses, under W1–W6 below.
/// **Rewrite:** the three clauses become the one fused top-K the same
/// `RETURN, ORDER BY, LIMIT` span would have become, carrying the WITH's items
/// as its projection and `with_scope: true`: `FusedVectorScoreTopK` for a
/// `vector_score` sort, `FusedTextBm25TopK` for `text_bm25`, otherwise
/// `FusedOrderByTopK` (property sorts, `text_score`, any other key). The
/// executor then drops the bindings the projection does not carry, as the
/// `WITH` did.
///
/// **Why it is an identity:** the fused operators rank rows before projecting,
/// which a non-`DISTINCT`, non-aggregating, non-window projection allows (it is
/// 1:1 and order-preserving), and the sort keys are resolved in the
/// pre-projection scope by `resolve_fused_sort_keys`, exactly as for a `RETURN`.
/// The vector case applies the existing rule unchanged: only `DESC` with
/// `NULLS FIRST` reaches the HNSW-backed operator (`requested=auto`); other
/// directions stay on the generic heap.
///
/// **Why-bail** (each leaves the clauses exactly as written):
/// - **W1** `DISTINCT`, an aggregate or window item, a `*` item, a
///   WITH-attached `WHERE` or a stamped `group_limit_hint` — not a 1:1
///   projection, or one the fused projection cannot express.
/// - **W2** `SKIP` between `ORDER BY` and `LIMIT`, or a LIMIT that is not a
///   positive integer literal.
/// - **W3** an `ORDER BY` key that reads a WITH alias it is not equal to
///   (`resolve_fused_sort_keys`).
/// - **W4** a statement with a write clause: the mutation executor is not
///   proven for a fused clause ahead of a write.
/// - **W5** a `CALL { }` body (`ctx.nested`): its WITH runs on seed rows with
///   imported names the projection restore handles, not this operator.
/// - **W6** the WITH is the first clause.
///
/// `fold_aliasing_with` and `hoist_terminal_return_over_with_top_k` run first
/// and remove the terminal-RETURN shapes; this pass takes what is left.
pub(crate) fn fuse_with_top_k(query: &mut CypherQuery) {
    if is_mutation_query(query) {
        return;
    }
    let mut i = 1;
    while i + 2 < query.clauses.len() {
        if let Some(fused) = fused_with_top_k(&query.clauses[i..i + 3]) {
            query.clauses.splice(i..i + 3, [fused]);
        }
        i += 1;
    }
}

fn fused_with_top_k(window: &[Clause]) -> Option<Clause> {
    let (Clause::With(w), Clause::OrderBy(o), Clause::Limit(l)) =
        (&window[0], &window[1], &window[2])
    else {
        return None;
    };
    if w.distinct
        || w.where_clause.is_some()
        || w.group_limit_hint.is_some()
        || w.items.iter().any(|item| {
            is_aggregate_expression(&item.expression)
                || is_window_expression(&item.expression)
                || matches!(item.expression, Expression::Star)
        })
    {
        return None;
    }
    let return_clause = ReturnClause {
        items: w.items.clone(),
        distinct: false,
        having: None,
        lazy_eligible: false,
        group_limit_hint: None,
    };
    // The same shape tests the RETURN-shape fusions run, over a RETURN
    // standing in for the WITH.
    let probe = [
        Clause::Return(return_clause.clone()),
        Clause::OrderBy(o.clone()),
        Clause::Limit(l.clone()),
    ];
    if let Some(shape) = match_scored_order_limit(&probe, 0, "vector_score") {
        if shape.descending && shape.nulls == NullsPlacement::First {
            return Some(Clause::FusedVectorScoreTopK {
                return_clause,
                score_item_index: shape.score_index,
                score_call: shape.score_call,
                descending: shape.descending,
                limit: shape.limit,
                non_null_only: false,
                with_scope: true,
            });
        }
    }
    let sort_keys = resolve_fused_sort_keys(o, &return_clause.items)?;
    if let Some(shape) = match_scored_order_limit(&probe, 0, "text_bm25") {
        return Some(Clause::FusedTextBm25TopK {
            return_clause,
            score_item_index: shape.score_index,
            score_call: shape.score_call,
            sort_keys,
            limit: shape.limit,
            with_scope: true,
        });
    }
    let limit = match &l.count {
        Expression::Literal(Value::Int64(n)) if *n > 0 => *n as usize,
        _ => return None,
    };
    Some(Clause::FusedOrderByTopK {
        return_clause,
        sort_keys,
        limit,
        with_scope: true,
    })
}
