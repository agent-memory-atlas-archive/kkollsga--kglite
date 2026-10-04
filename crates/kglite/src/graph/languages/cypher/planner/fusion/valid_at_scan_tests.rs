use std::collections::HashMap;

use crate::graph::languages::cypher::ast::*;
use crate::graph::languages::cypher::parser::parse_cypher;
use crate::graph::languages::cypher::planner::optimize;
use crate::graph::schema::DirGraph;

fn optimized(source: &str) -> CypherQuery {
    let mut query = parse_cypher(source).unwrap();
    optimize(&mut query, &DirGraph::new(), &HashMap::new());
    query
}

fn join_of(source: &str) -> Option<ValidAtJoin> {
    optimized(source)
        .clauses
        .into_iter()
        .find_map(|clause| match clause {
            Clause::FusedValidAtJoin(join) => Some(*join),
            _ => None,
        })
}

const FUSED: &[&str] = &[
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(d)) RETURN d, count(*)",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND e.role = 'a' RETURN d, e.role",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE e.role = 'a' AND valid_at(e, d) RETURN d, e.role",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d, 'vf', 'vt') = true RETURN d, e",
    "UNWIND $ds AS d MATCH (e:Emp)-[:IN]->(x:Dept) WHERE valid_at(e, d) RETURN d, x.name",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND size(e.name) > 3 RETURN d, e.role",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE e.level < e.rank AND valid_at(e, d) RETURN d, e.role",
    "UNWIND $ds AS d MATCH (e:Emp {role: 'a'}) WHERE valid_at(e, d) RETURN d, count(*)",
    "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(d)) AND valid_at(e, d) RETURN d",
];

/// Each of these keeps `MATCH` and `WHERE` as written, for the reason beside it.
const BAILED: &[(&str, &str)] = &[
    (
        "MATCH (e:Emp) WHERE valid_at(e, date('2020-01-01')) RETURN e",
        "no UNWIND drives it (the WHERE already folds into the opening MATCH)",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) OR e.role = 'a' RETURN d",
        "the conjunct sits inside OR",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE NOT valid_at(e, d) RETURN d",
        "the conjunct sits inside NOT",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp:Mgr) WHERE valid_at(e, d) RETURN d",
        "a secondary label judges the node by every label it carries",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp|Mgr) WHERE valid_at(e, d) RETURN d",
        "a label alternation",
    ),
    (
        "UNWIND $ds AS d MATCH (e) WHERE valid_at(e, d) RETURN d",
        "no label to read the declaration from",
    ),
    (
        "UNWIND $ds AS d MATCH (a:Emp)-[r:IN]->(b:Dept) WHERE valid_at(r, d) RETURN d",
        "a relationship is not a node pattern variable",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp)-[:IN*1..2]->(x:Dept) WHERE valid_at(e, d) RETURN d",
        "a variable-length hop",
    ),
    (
        "UNWIND $ds AS d MATCH p = (e:Emp) WHERE valid_at(e, d) RETURN d",
        "a path assignment",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, e.vf) RETURN d",
        "the instant reads the matched node",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(rand())) RETURN d",
        "a volatile instant expression",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp {role: d}) WHERE valid_at(e, d) RETURN d",
        "a matcher reads the driving row",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE e.salary / 0 > 1 AND valid_at(e, d) RETURN d",
        "a residual that can raise runs before the valid_at conjunct",
    ),
    (
        "UNWIND $ds AS d WITH d AS day MATCH (e:Emp) WHERE valid_at(e, day) RETURN day",
        "a WITH sits between the UNWIND and the MATCH",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE size(e.name) > 3 AND valid_at(e, d) RETURN d",
        "a function call before the valid_at conjunct may raise",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE e.name =~ '(' AND valid_at(e, d) RETURN d",
        "a residual regex can raise",
    ),
    (
        "UNWIND [1, 2] AS e MATCH (e:Emp) WHERE valid_at(e, date('2020-01-01')) RETURN e",
        "the pattern variable is already bound by the UNWIND",
    ),
    (
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) CREATE (:Log {d: d})",
        "a write clause in the statement",
    ),
    (
        "UNWIND $ds AS d OPTIONAL MATCH (e:Emp) WHERE valid_at(e, d) RETURN d",
        "OPTIONAL MATCH keeps rows with no match",
    ),
];

#[test]
fn trigger_shapes_fuse() {
    for source in FUSED {
        assert!(join_of(source).is_some(), "expected a fused join: {source}");
    }
}

#[test]
fn unsafe_shapes_keep_match_and_where() {
    for (source, why) in BAILED {
        assert!(join_of(source).is_none(), "must not fuse ({why}): {source}");
    }
}

#[test]
fn a_call_body_with_imports_is_not_fused() {
    let query = optimized(
        "WITH [date('2020-01-01')] AS ds CALL (ds) { UNWIND ds AS d MATCH (e:Emp) \
         WHERE valid_at(e, d) RETURN count(*) AS n } RETURN n",
    );
    let Some(Clause::CallSubquery { body, .. }) = query
        .clauses
        .iter()
        .find(|c| matches!(c, Clause::CallSubquery { .. }))
    else {
        panic!("no CALL clause");
    };
    assert!(!body
        .clauses
        .iter()
        .any(|c| matches!(c, Clause::FusedValidAtJoin(_))));
}

#[test]
fn the_conjunct_is_split_out_and_the_rest_kept() {
    let join = join_of(
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND size(e.name) > 3 \
         AND size(e.role) < 9 RETURN d, e.role",
    )
    .unwrap();
    assert_eq!(join.var, "e");
    assert_eq!(join.label, "Emp");
    assert!(join.named_bounds.is_none());
    assert!(join.count_alias.is_none());
    let residual = format!("{:?}", join.residual.expect("residual"));
    assert!(!residual.contains("valid_at"), "{residual}");
    assert!(
        residual.contains("name") && residual.contains("role"),
        "{residual}"
    );
    let full = format!("{:?}", join.where_clause.predicate);
    assert!(
        full.contains("valid_at"),
        "the fallback keeps the whole WHERE"
    );
}

/// A comparison with a literal moves into the pattern's matcher, which the
/// one scan applies, so it leaves no residual and the join can still count.
#[test]
fn pushed_down_predicates_leave_no_residual() {
    let join = join_of(
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND e.role = 'a' RETURN d, count(*)",
    )
    .unwrap();
    assert!(join.residual.is_none());
    assert!(join.count_alias.is_some());
}

#[test]
fn named_bounds_are_carried() {
    let join =
        join_of("UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d, 'vf', 'vt') RETURN d").unwrap();
    assert_eq!(join.named_bounds, Some(("vf".into(), "vt".into())));
}

fn counted(source: &str) -> (bool, Vec<ReturnItem>) {
    let query = optimized(source);
    let counted = query
        .clauses
        .iter()
        .any(|c| matches!(c, Clause::FusedValidAtJoin(join) if join.count_alias.is_some()));
    let items = query
        .clauses
        .iter()
        .find_map(|c| match c {
            Clause::Return(r) => Some(r.items.clone()),
            _ => None,
        })
        .unwrap_or_default();
    (counted, items)
}

#[test]
fn a_grouped_count_return_switches_the_join_to_count_mode() {
    let (counted, items) =
        counted("UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, date(d)) RETURN d, count(*)");
    assert!(counted);
    // The count becomes a sum of the per-row count, under the column name the
    // unfused plan would have given it.
    let sum = &items[1];
    assert_eq!(sum.alias.as_deref(), Some("count(*)"));
    assert!(matches!(&sum.expression, Expression::FunctionCall { name, .. } if name == "sum"));
}

#[test]
fn count_mode_keeps_an_explicit_alias() {
    let (counted, items) = counted(
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d AS day, count(e) AS n",
    );
    assert!(counted);
    assert_eq!(items[1].alias.as_deref(), Some("n"));
}

#[test]
fn count_mode_declines_what_a_per_row_count_cannot_answer() {
    for source in [
        // no group key: over no rows the unfused plan answers 0
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN count(*)",
        // a residual predicate needs the rows
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) AND size(e.name) > 3 RETURN d, count(*)",
        // another aggregate, a key that reads the match, DISTINCT counting, HAVING-free ORDER BY on the aggregate
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(*), collect(e.role)",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, e.role, count(*)",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(DISTINCT e)",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN d, count(*) AS n ORDER BY count(*) DESC",
        "UNWIND $ds AS d MATCH (e:Emp) WHERE valid_at(e, d) RETURN *",
    ] {
        let (counted, items) = counted(source);
        assert!(!counted, "count mode must not engage: {source}");
        assert!(
            !items.iter().any(|item| matches!(
                &item.expression,
                Expression::FunctionCall { name, .. } if name == "sum"
            )),
            "RETURN must be untouched: {source}"
        );
    }
}
