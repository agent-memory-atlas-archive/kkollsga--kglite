//! Filter hoisting across a `CALL { }` subquery.
//!
//! `Clause::CallSubquery` runs its body once per incoming row, so a filter
//! written after it pays for the body on every row the filter then discards
//! (`MATCH (n:Review) CALL (n) { … } WITH n, … WHERE n.pid = $p`: the body ran
//! for every `Review` to keep one). This pass moves the conjuncts of such a
//! filter that read only variables bound before the CALL to just ahead of it,
//! where `push_where_into_match` can turn them into an index seek.

use super::super::ast::*;
use super::fusion::VOLATILE_FUNCTIONS;
use super::simplification::{collect_expression_refs, collect_predicate_refs, visible_variables};
use super::with_boundary::predicate_has_aggregate;
use super::PassCtx;
use std::collections::HashSet;

/// **Pass:** `hoist_where_over_call_subquery` — **Precondition:** a
/// `Clause::CallSubquery` at index `c >= 1` with no `UNION` in the scope, whose
/// body is *row-preserving, read-only and deterministic* (below), followed by
/// zero or more `WITH`, standalone `WHERE` or further qualifying CALLs and a
/// filter (`Clause::Where`, or the `where_clause` of a `WITH`).
///
/// **Pattern matched:** a top-level `AND` conjunct of that filter that
/// (H1) reads at least one variable and only variables visible before the
/// CALL, (H2) reads each of them through every intervening `WITH` as a bare
/// pass-through (`n` or `n AS n`, or `WITH *`), so it names the same binding
/// on both sides, and (H3) contains no aggregate and no volatile function
/// (`rand`, `random`, `randomUuid`, `timestamp`).
///
/// **Rewrite:** the conjunct leaves the filter (the filter is removed when it
/// held nothing else) and joins a `Clause::Where` placed immediately before the
/// CALL, merged into an existing `Where` there. `push_where_into_match.1` then
/// sees `Match, Where, CallSubquery`.
///
/// **Body qualification.** The CALL runs its body once per outer row and
/// inner-joins the result to that row, so a filter over outer variables alone
/// commutes with it *except* where the body has a side effect, a
/// row-dependent result, or an error that only some rows raise. The body must
/// therefore (B1) end in a `RETURN` whose every item is a top-level aggregate
/// call (`count`, `collect`, …) and has no `HAVING`: with no grouping key an
/// aggregation yields exactly one row per input row even over zero matches, so
/// the CALL neither drops nor multiplies rows; (B2) contain only `MATCH`,
/// `OPTIONAL MATCH`, `WHERE`, `WITH`, `UNWIND`, `ORDER BY`, `SKIP`, `LIMIT` and
/// nested CALLs of the same kind (no write, no procedure call, no `UNION`,
/// `FOREACH`); (B3) mention no volatile function. A `LIMIT` without `ORDER BY`
/// is allowed: each row's body result depends on that row alone, so which
/// rows are skipped cannot change it.
///
/// **Why-bail:** any (H*) or (B*) failure; an intervening `WITH` that is
/// `DISTINCT`, aggregating or capped; any other intervening clause (including
/// `ORDER BY`/`LIMIT`, which a filter must not cross). Hoisting skips the
/// body for filtered rows, so a body that would raise for a row the filter
/// removes no longer raises; that is the only observable difference.
///
/// **Top-K.** When the clauses after the CALL are exactly `WITH`s that keep
/// every row (non-`DISTINCT`, no aggregate, no filter), a non-aggregating
/// `RETURN`, `ORDER BY <keys>` and `LIMIT k` (no `SKIP`), and every key reads
/// only names the `WITH`s and the `RETURN` pass through unchanged, the same
/// `ORDER BY`/`LIMIT` pair is also placed ahead of the CALL. Each input row
/// yields exactly one output row, so the `k` rows the final sort keeps are
/// among the `k` the early sort keeps (both sorts see one stable row order);
/// the final pair stays and re-sorts them. Rows tied at the cut may differ
/// from the unhoisted plan's, which Cypher leaves unspecified. A filter still
/// standing after the CALL (a conjunct that reads a CALL output) blocks it:
/// it would remove rows after the early cut.
///
/// The pass returns after an `O(clauses)` scan on a query with no CALL.
pub(super) fn pass_hoist_where_over_call_subquery(query: &mut CypherQuery, _ctx: &PassCtx) {
    hoist_where_over_call_subquery(query)
}

fn hoist_where_over_call_subquery(query: &mut CypherQuery) {
    if !query
        .clauses
        .iter()
        .any(|c| matches!(c, Clause::CallSubquery { .. }))
        || query.clauses.iter().any(|c| matches!(c, Clause::Union(_)))
    {
        return;
    }
    let mut c = 1;
    while c < query.clauses.len() {
        if call_qualifies(&query.clauses[c]) {
            if let Some(hoisted) = take_hoistable(&mut query.clauses, c) {
                if let Clause::Where(existing) = &mut query.clauses[c - 1] {
                    let merged =
                        Predicate::And(Box::new(existing.predicate.clone()), Box::new(hoisted));
                    existing.predicate = merged;
                } else {
                    query
                        .clauses
                        .insert(c, Clause::Where(WhereClause { predicate: hoisted }));
                    c += 1;
                }
            }
            if hoist_top_k(&mut query.clauses, c) {
                c += 2;
            }
        }
        c += 1;
    }
}

/// Remove and return (AND-joined) every hoistable conjunct from the filters
/// following the CALL at `c`.
fn take_hoistable(clauses: &mut Vec<Clause>, c: usize) -> Option<Predicate> {
    let mut live = visible_variables(&clauses[..c]);
    if live.is_empty() {
        return None;
    }
    let mut hoisted: Vec<Predicate> = Vec::new();
    let mut emptied: Vec<usize> = Vec::new();
    let mut j = c + 1;
    while j < clauses.len() {
        match &mut clauses[j] {
            Clause::With(w) => {
                if w.distinct
                    || w.group_limit_hint.is_some()
                    || w.items
                        .iter()
                        .any(|i| is_aggregate_expression(&i.expression))
                {
                    break;
                }
                live = narrow_to_pass_through(&live, w);
                if let Some(wc) = w.where_clause.take() {
                    w.where_clause = split(wc.predicate, &live, &mut hoisted)
                        .map(|predicate| WhereClause { predicate });
                }
            }
            Clause::Where(wc) => match split(wc.predicate.clone(), &live, &mut hoisted) {
                Some(rest) => wc.predicate = rest,
                None => emptied.push(j),
            },
            clause @ Clause::CallSubquery { .. } if call_qualifies(clause) => {}
            _ => break,
        }
        j += 1;
    }
    for j in emptied.into_iter().rev() {
        clauses.remove(j);
    }
    hoisted
        .into_iter()
        .reduce(|a, b| Predicate::And(Box::new(a), Box::new(b)))
}

/// The names of `live` that `w` keeps bound to the same value.
fn narrow_to_pass_through(live: &HashSet<String>, w: &WithClause) -> HashSet<String> {
    if w.items
        .iter()
        .any(|i| matches!(i.expression, Expression::Star))
    {
        return live.clone();
    }
    w.items
        .iter()
        .filter_map(|i| match (&i.expression, &i.alias) {
            (Expression::Variable(v), None) => Some(v),
            (Expression::Variable(v), Some(a)) if v == a => Some(v),
            _ => None,
        })
        .filter(|v| live.contains(*v))
        .cloned()
        .collect()
}

/// Move the hoistable top-level conjuncts of `predicate` into `hoisted`;
/// the rest, AND-joined, is returned.
fn split(
    predicate: Predicate,
    live: &HashSet<String>,
    hoisted: &mut Vec<Predicate>,
) -> Option<Predicate> {
    let mut conjuncts = Vec::new();
    flatten_and(predicate, &mut conjuncts);
    let mut rest = Vec::new();
    for conjunct in conjuncts {
        if conjunct_hoistable(&conjunct, live) {
            hoisted.push(conjunct);
        } else {
            rest.push(conjunct);
        }
    }
    rest.into_iter()
        .reduce(|a, b| Predicate::And(Box::new(a), Box::new(b)))
}

fn flatten_and(predicate: Predicate, out: &mut Vec<Predicate>) {
    match predicate {
        Predicate::And(l, r) => {
            flatten_and(*l, out);
            flatten_and(*r, out);
        }
        other => out.push(other),
    }
}

fn conjunct_hoistable(conjunct: &Predicate, live: &HashSet<String>) -> bool {
    let mut refs = HashSet::new();
    collect_predicate_refs(conjunct, &mut refs);
    !refs.is_empty()
        && refs.iter().all(|r| live.contains(r))
        && !is_volatile(conjunct)
        && !predicate_has_aggregate(conjunct)
}

/// Whether the debug rendering of `node` calls a volatile function. A string
/// literal spelled like one only makes this bail, never hoist wrongly.
fn is_volatile(node: &impl std::fmt::Debug) -> bool {
    let text = format!("{node:?}").to_lowercase();
    VOLATILE_FUNCTIONS
        .iter()
        .any(|name| text.contains(&format!("name: \"{name}\"")))
}

fn call_qualifies(clause: &Clause) -> bool {
    let Clause::CallSubquery { body, .. } = clause else {
        return false;
    };
    returns_one_row_per_input(body) && body_is_pure(body)
}

/// B1: the body's last clause is a `RETURN` of aggregate calls alone.
fn returns_one_row_per_input(body: &CypherQuery) -> bool {
    let Some(Clause::Return(r)) = body.clauses.last() else {
        return false;
    };
    r.having.is_none()
        && !r.items.is_empty()
        && r.items.iter().all(|item| {
            matches!(&item.expression,
                Expression::FunctionCall { name, .. } if is_aggregate_function_name(name))
        })
}

/// B2 + B3: only read clauses, no volatile function.
fn body_is_pure(body: &CypherQuery) -> bool {
    let last = body.clauses.len().saturating_sub(1);
    body.clauses
        .iter()
        .enumerate()
        .all(|(i, clause)| match clause {
            Clause::Match(_)
            | Clause::OptionalMatch(_)
            | Clause::Where(_)
            | Clause::With(_)
            | Clause::Unwind(_)
            | Clause::OrderBy(_)
            | Clause::Skip(_)
            | Clause::Limit(_) => !is_volatile(clause),
            Clause::Return(_) => i == last && !is_volatile(clause),
            Clause::CallSubquery { body, .. } => body_is_pure(body),
            _ => false,
        })
}

/// Place `ORDER BY <keys> LIMIT k` ahead of the CALL at `ci` when the tail
/// after it has the shape described on the pass. Returns whether it did.
fn hoist_top_k(clauses: &mut Vec<Clause>, ci: usize) -> bool {
    let mut live = visible_variables(&clauses[..ci]);
    let mut j = ci + 1;
    while let Some(Clause::With(w)) = clauses.get(j) {
        if w.distinct
            || w.where_clause.is_some()
            || w.group_limit_hint.is_some()
            || w.items
                .iter()
                .any(|i| is_aggregate_expression(&i.expression))
        {
            return false;
        }
        live = narrow_to_pass_through(&live, w);
        j += 1;
    }
    let Some(Clause::Return(r)) = clauses.get(j) else {
        return false;
    };
    if r.distinct
        || r.having.is_some()
        || r.group_limit_hint.is_some()
        || r.items
            .iter()
            .any(|i| is_aggregate_expression(&i.expression))
    {
        return false;
    }
    // An alias that is not the bare variable of the same name rebinds the
    // name for the ORDER BY that follows.
    for item in &r.items {
        if let Some(alias) = &item.alias {
            if !matches!(&item.expression, Expression::Variable(v) if v == alias) {
                live.remove(alias);
            }
        }
    }
    let (Some(Clause::OrderBy(order)), Some(Clause::Limit(limit)), None) =
        (clauses.get(j + 1), clauses.get(j + 2), clauses.get(j + 3))
    else {
        return false;
    };
    let keys_ok = order.items.iter().all(|key| {
        let mut refs = HashSet::new();
        collect_expression_refs(&key.expression, &mut refs);
        refs.iter().all(|v| live.contains(v))
            && !is_aggregate_expression(&key.expression)
            && !is_volatile(&key.expression)
    });
    if !keys_ok || order.items.is_empty() || is_volatile(&limit.count) {
        return false;
    }
    let (order, limit) = (order.clone(), limit.clone());
    clauses.insert(ci, Clause::Limit(limit));
    clauses.insert(ci, Clause::OrderBy(order));
    true
}
