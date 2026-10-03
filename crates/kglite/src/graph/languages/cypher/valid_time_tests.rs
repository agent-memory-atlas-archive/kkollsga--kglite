//! Lowering a `FOR VALID_TIME AS OF` context: the per-scope templates, the
//! refusals, the guarded pass allow-list, and execution under the filter
//! each path resolves.

use super::*;
use crate::graph::languages::cypher::parser::parse_cypher;
use crate::graph::languages::cypher::planner::optimize_with_disabled;
use crate::graph::languages::cypher::result::CypherResult;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashSet;

fn write(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

/// Wells and fields both hold `vf`/`vt`; only `Well` and `LICENSED` are
/// declared, so `Field` and `NEAR` are the undeclared twins.
fn graph() -> DirGraph {
    let mut graph = DirGraph::new();
    for query in [
        "CREATE (:Well {id: 1, vf: '2000-01-01', vt: '2010-01-01'}), \
         (:Well {id: 2, vf: '2005-01-01', vt: null}), \
         (:Field {id: 10, vf: '2000-01-01', vt: null})",
        "MATCH (w:Well {id: 1}), (f:Field) \
         CREATE (w)-[:LICENSED {vf: '2000-01-01', vt: '2020-01-01'}]->(f), (w)-[:NEAR]->(f)",
        "CALL db.temporal.declare({node: 'Well', from: 'vf', to: 'vt', convention: 'closed'}) YIELD declared \
         RETURN declared",
        "CALL db.temporal.declare({relationship: 'LICENSED', from: 'vf', to: 'vt', \
         convention: 'half_open'}) YIELD declared RETURN declared",
    ] {
        write(&mut graph, query);
    }
    graph
}

fn lowered(graph: &DirGraph, query: &str, disabled: &[&str]) -> CypherQuery {
    let mut parsed = parse_cypher(query).unwrap_or_else(|e| panic!("{query}: {e}"));
    let disabled: HashSet<String> = disabled.iter().map(|s| s.to_string()).collect();
    optimize_with_disabled(&mut parsed, graph, &HashMap::new(), &disabled);
    parsed
}

fn template(query: &CypherQuery) -> String {
    query
        .guard
        .as_deref()
        .expect("a guard template")
        .to_string()
}

fn refusal(query: &CypherQuery) -> Option<String> {
    query.context.as_ref().and_then(|c| c.refusal.clone())
}

const AS_OF: &str = "FOR VALID_TIME AS OF date('2006-01-01') ";

#[test]
fn without_a_prefix_nothing_is_lowered() {
    let graph = graph();
    let query = lowered(
        &graph,
        "MATCH (w:Well) CALL { MATCH (m) RETURN m } RETURN w, m",
        &[],
    );
    assert!(query.context.is_none() && query.guard.is_none());
    let Some(Clause::CallSubquery { body, .. }) = query.clauses.get(1) else {
        panic!("{:?}", query.clauses)
    };
    assert!(body.guard.is_none());
}

/// The same query names one declared and one undeclared type: the template
/// lists the declared target only.
#[test]
fn template_lists_declared_targets_the_scope_reaches() {
    let graph = graph();
    let both = lowered(
        &graph,
        &format!("{AS_OF}MATCH (w:Well), (f:Field) RETURN w, f"),
        &[],
    );
    assert_eq!(template(&both), "(:Well [vf, vt] closed)");
    let undeclared = lowered(&graph, &format!("{AS_OF}MATCH (f:Field) RETURN f"), &[]);
    assert_eq!(template(&undeclared), "no declared targets");
    let typed = lowered(
        &graph,
        &format!("{AS_OF}MATCH (:Field)<-[r:LICENSED]-(:Field) RETURN r"),
        &[],
    );
    assert_eq!(template(&typed), "[:LICENSED [vf, vt] half_open]");
    let near = lowered(
        &graph,
        &format!("{AS_OF}MATCH (:Field)<-[r:NEAR]-(:Field) RETURN r"),
        &[],
    );
    assert_eq!(template(&near), "no declared targets");
}

/// An unlabelled node or untyped relationship can reach every declared
/// target, and so can a labelled node once secondary labels exist.
#[test]
fn unconstrained_patterns_reach_every_declared_target() {
    let mut graph = graph();
    let all = "(:Well [vf, vt] closed), [:LICENSED [vf, vt] half_open]";
    let q = lowered(&graph, &format!("{AS_OF}MATCH (a)-[r]->(b) RETURN a"), &[]);
    assert_eq!(template(&q), all);
    let q = lowered(&graph, &format!("{AS_OF}MATCH (f:Field) RETURN f"), &[]);
    assert_eq!(template(&q), "no declared targets");
    // A pattern inside an expression counts too.
    let q = lowered(
        &graph,
        &format!("{AS_OF}MATCH (f:Field) RETURN COUNT {{ (f)<-[:LICENSED]-() }} AS c"),
        &[],
    );
    assert_eq!(template(&q), all);
    write(&mut graph, "MATCH (f:Field) SET f:Tagged");
    let q = lowered(&graph, &format!("{AS_OF}MATCH (f:Field) RETURN f"), &[]);
    assert_eq!(template(&q), "(:Well [vf, vt] closed)");
}

/// Every nested scope gets its own template, even with the pass that
/// optimizes nested scopes disabled.
#[test]
fn every_scope_gets_a_template_whatever_passes_are_disabled() {
    let graph = graph();
    let text = format!(
        "{AS_OF}MATCH (f:Field) CALL {{ MATCH (w:Well) RETURN w }} RETURN f, w \
         UNION MATCH (a)-[r:LICENSED]->(b) RETURN a AS f, b AS w"
    );
    for disabled in [&[][..], &["optimize_nested_queries"][..]] {
        let query = lowered(&graph, &text, disabled);
        assert_eq!(template(&query), "no declared targets", "{disabled:?}");
        let body = query.clauses.iter().find_map(|c| match c {
            Clause::CallSubquery { body, .. } => Some(body),
            _ => None,
        });
        assert_eq!(
            template(body.expect("CALL body")),
            "(:Well [vf, vt] closed)"
        );
        let arm = query.clauses.iter().find_map(|c| match c {
            Clause::Union(u) => Some(&u.query),
            _ => None,
        });
        assert_eq!(
            template(arm.expect("UNION arm")),
            "(:Well [vf, vt] closed), [:LICENSED [vf, vt] half_open]"
        );
    }
}

#[test]
fn lowering_refusals() {
    let graph = graph();
    let axis = lowered(&graph, "FOR SYSTEM_TIME AS OF $t MATCH (n) RETURN n", &[]);
    let message = refusal(&axis).expect("refused");
    assert!(
        message.contains("axis SYSTEM_TIME is not supported"),
        "{message}"
    );
    assert!(message.contains("VALID_TIME"), "{message}");
    assert!(axis.guard.is_none());

    let write = lowered(&graph, &format!("{AS_OF}MATCH (w:Well) SET w.x = 1"), &[]);
    assert!(refusal(&write).unwrap().contains("cannot write"));

    let rule = lowered(
        &graph,
        &format!("{AS_OF}CALL orphan_node() YIELD node RETURN node"),
        &[],
    );
    assert!(refusal(&rule).unwrap().contains("procedure orphan_node"));
    // Inside a CALL body too, and no scope keeps a template once refused.
    let nested = lowered(
        &graph,
        &format!(
            "{AS_OF}MATCH (w:Well) CALL {{ CALL orphan_node() YIELD node RETURN node }} RETURN w"
        ),
        &[],
    );
    assert!(refusal(&nested).is_some() && nested.guard.is_none());

    // An algorithm routes to the valid slice and an embedding query tests
    // each candidate: both lower, reaching every declared target.
    for routed in [
        "CALL pagerank() YIELD node RETURN node",
        "CALL kglite.connected_components() YIELD node RETURN node",
        "CALL db.node_embeddings.query({text_column: 'x', vector: [1.0]}) YIELD node RETURN node",
    ] {
        let query = lowered(&graph, &format!("{AS_OF}{routed}"), &[]);
        assert_eq!(refusal(&query), None, "{routed}");
        let guard = query.guard.as_ref().expect("a template");
        assert_eq!(**guard, declared_template(&graph).unwrap(), "{routed}");
    }

    for metadata in ["CALL db.labels()", "CALL db.temporal.declarations()"] {
        let query = lowered(&graph, &format!("{AS_OF}{metadata}"), &[]);
        assert_eq!(refusal(&query), None, "{metadata}");
    }

    let empty = DirGraph::new();
    let none = lowered(&empty, &format!("{AS_OF}MATCH (n) RETURN n"), &[]);
    assert!(refusal(&none).unwrap().contains("has none"));
}

fn has_fused(clauses: &[Clause]) -> bool {
    clauses.iter().any(|c| {
        matches!(
            c,
            Clause::FusedCountTypedNode { .. }
                | Clause::FusedCountAll { .. }
                | Clause::FusedMatchReturnAggregate { .. }
                | Clause::FusedMatchWithAggregate { .. }
                | Clause::FusedNodeScanAggregate { .. }
                | Clause::FusedNodeScanTopK { .. }
                | Clause::FusedOrderByTopK { .. }
        )
    })
}

/// Default-deny: a denied pass does not run on a guarded scope, and the
/// allow-listed ones — the re-admitted fusions included — still do.
#[test]
fn a_guarded_scope_runs_only_allow_listed_passes() {
    let graph = graph();
    for body in ["MATCH (w:Well)-[:LICENSED]->(f) WITH w, count(f) AS c RETURN w.id, c"] {
        let plain = lowered(&graph, &format!("EXPLAIN {body}"), &[]);
        assert!(has_fused(&plain.clauses), "{body} should fuse unguarded");
        let guarded = lowered(&graph, &format!("EXPLAIN {AS_OF}{body}"), &[]);
        assert!(
            !has_fused(&guarded.clauses),
            "{body}: {:?}",
            guarded.clauses
        );
    }
    for (body, pass) in [
        (
            "MATCH (w:Well)-[:LICENSED]->(f) RETURN w.id AS id, count(f) AS c",
            "fuse_match_return_aggregate",
        ),
        (
            "MATCH (w:Well) RETURN count(w) AS c",
            "fuse_count_short_circuits",
        ),
        (
            "MATCH (w:Well) RETURN w.id AS id ORDER BY id LIMIT 1",
            "fuse_node_scan_top_k",
        ),
        (
            "MATCH (w:Well) RETURN w.id AS id, count(*) AS c",
            "fuse_node_scan_aggregate",
        ),
    ] {
        let guarded = lowered(&graph, &format!("EXPLAIN {AS_OF}{body}"), &[]);
        assert!(
            guarded.optimizer_tags.iter().any(|t| t == pass),
            "{body}: {:?}",
            guarded.optimizer_tags
        );
    }
    for body in [
        "MATCH (w:Well)-[:LICENSED]->(f) RETURN w.id AS id, count(f) AS c",
        "MATCH (w:Well) RETURN count(w) AS c",
    ] {
        let guarded = lowered(&graph, &format!("EXPLAIN {AS_OF}{body}"), &[]);
        for tag in &guarded.optimizer_tags {
            assert!(super::super::planner::is_known_pass(tag));
            assert!(
                super::super::planner::guard_is_safe(tag),
                "{body}: denied pass {tag} ran"
            );
        }
    }
    // An allow-listed pass still rewrites the guarded plan.
    let guarded = lowered(
        &graph,
        &format!("EXPLAIN {AS_OF}MATCH (w:Well) WHERE w.id = 1 RETURN w"),
        &[],
    );
    assert!(guarded
        .optimizer_tags
        .iter()
        .any(|t| t.starts_with("push_where_into_match")));
    // A guarded plan is never marked lazy.
    let mut lazy = lowered(&graph, &format!("{AS_OF}MATCH (w:Well) RETURN w"), &[]);
    crate::graph::languages::cypher::mark_lazy_eligibility(&mut lazy);
    let Some(Clause::Return(r)) = lazy.clauses.last() else {
        panic!()
    };
    assert!(!r.lazy_eligible);
}

fn read(
    graph: &DirGraph,
    query: &str,
    params: &HashMap<String, Value>,
) -> Result<CypherResult, String> {
    execute_read(graph, query, &ExecuteOptions::eager(params))
        .map(|o| o.result)
        .map_err(|e| e.to_string())
}

fn well_ids(result: &CypherResult) -> Vec<Value> {
    let mut ids: Vec<Value> = result.rows.iter().map(|row| row[0].clone()).collect();
    ids.sort_by_key(|v| format!("{v:?}"));
    ids
}

#[test]
fn execution_runs_under_the_filter_and_explain_renders_the_template() {
    let graph = graph();
    let none = HashMap::new();
    let at_2015 = "FOR VALID_TIME AS OF date('2015-01-01') ";
    for query in [
        format!("{at_2015}MATCH (w:Well) RETURN w.id"),
        format!("PROFILE {at_2015}MATCH (w:Well) RETURN w.id"),
        format!("{at_2015}PROFILE MATCH (w:Well) RETURN w.id"),
    ] {
        let result = read(&graph, &query, &none).unwrap_or_else(|e| panic!("{query}: {e}"));
        assert_eq!(well_ids(&result), vec![Value::Int64(2)], "{query}");
    }
    read(&graph, &format!("{AS_OF}CALL db.labels()"), &none).unwrap();
    for query in [
        format!("EXPLAIN {AS_OF}MATCH (w:Well) RETURN w.id"),
        format!("{AS_OF}EXPLAIN MATCH (w:Well) RETURN w.id"),
    ] {
        let result = read(&graph, &query, &none).unwrap();
        let first = format!("{:?}", result.rows[0][1]);
        assert!(
            first.contains(
                "ValidTimeContext axis=VALID_TIME targets=(:Well [vf, vt] closed) \
                            instant: per execution"
            ),
            "{first}"
        );
    }
    // Both EXPLAIN orders render the same plan.
    let a = read(
        &graph,
        &format!("EXPLAIN {AS_OF}MATCH (w:Well) RETURN w.id"),
        &none,
    )
    .unwrap();
    let b = read(
        &graph,
        &format!("{AS_OF}EXPLAIN MATCH (w:Well) RETURN w.id"),
        &none,
    )
    .unwrap();
    assert_eq!(format!("{:?}", a.rows), format!("{:?}", b.rows));
    // A lowering refusal stops EXPLAIN too.
    let err = read(
        &graph,
        "EXPLAIN FOR SYSTEM_TIME AS OF date() RETURN 1 AS x",
        &none,
    )
    .unwrap_err();
    assert!(err.contains("axis SYSTEM_TIME"), "{err}");
    // Without a prefix EXPLAIN has no context row.
    let plain = read(&graph, "EXPLAIN MATCH (w:Well) RETURN w.id", &none).unwrap();
    assert!(!format!("{:?}", plain.rows).contains("ValidTimeContext"));
}

#[test]
fn the_instant_is_resolved_per_execution() {
    let graph = graph();
    let query = "FOR VALID_TIME AS OF $t MATCH (w:Well) RETURN w.id";
    let missing = read(&graph, query, &HashMap::new()).unwrap_err();
    assert!(missing.contains("Missing parameter: $t"), "{missing}");
    let bad = HashMap::from([("t".to_string(), Value::Int64(42))]);
    let err = read(&graph, query, &bad).unwrap_err();
    assert!(err.contains("FOR VALID_TIME AS OF"), "{err}");
    let good = HashMap::from([("t".to_string(), Value::String("2003-01-01".into()))]);
    assert_eq!(
        well_ids(&read(&graph, query, &good).unwrap()),
        vec![Value::Int64(1)]
    );
    // EXPLAIN evaluates no parameter.
    read(&graph, &format!("EXPLAIN {query}"), &bad).unwrap();
}

/// The executor resolves the filter on its own, for callers that bypass
/// the session.
#[test]
fn the_executor_resolves_a_context_directly() {
    let graph = graph();
    let params = HashMap::new();
    let query = lowered(
        &graph,
        "FOR VALID_TIME AS OF date('2003-01-01') MATCH (w:Well) RETURN w.id",
        &[],
    );
    let result = CypherExecutor::with_params(&graph, &params, None)
        .execute(&query)
        .unwrap();
    assert_eq!(well_ids(&result), vec![Value::Int64(1)]);
}

#[test]
fn prepend_writes_a_literal_and_refuses_a_second_context() {
    let date = Value::DateTime(chrono::NaiveDate::from_ymd_opt(2020, 1, 2).unwrap());
    assert_eq!(
        prepend_valid_time("MATCH (n) RETURN n", &date).unwrap(),
        "FOR VALID_TIME AS OF date('2020-01-02') MATCH (n) RETURN n"
    );
    let ts = Value::Timestamp(
        chrono::NaiveDate::from_ymd_opt(2020, 1, 2)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap(),
    );
    assert_eq!(
        prepend_valid_time("RETURN 1", &ts).unwrap(),
        "FOR VALID_TIME AS OF datetime('2020-01-02T10:30:00') RETURN 1"
    );
    let text = Value::String("2020-01-02".into());
    assert!(prepend_valid_time("EXPLAIN RETURN 1", &text)
        .unwrap()
        .starts_with("FOR VALID_TIME AS OF date('2020-01-02') EXPLAIN"));
    let text_ts = Value::String("2020-01-02T10:30:00".into());
    assert!(prepend_valid_time("RETURN 1", &text_ts)
        .unwrap()
        .contains("datetime('2020-01-02T10:30:00')"));
    for bad in [
        Value::String("yesterday".into()),
        Value::Int64(3),
        Value::Null,
    ] {
        let err = prepend_valid_time("RETURN 1", &bad).unwrap_err();
        assert!(matches!(err, PrependError::BadInstant(_)), "{err}");
        assert!(err.to_string().starts_with("valid_at:"), "{err}");
    }
    for doubled in [
        "FOR VALID_TIME AS OF $t RETURN 1",
        "EXPLAIN for valid_time AS OF $t RETURN 1",
    ] {
        let err = prepend_valid_time(doubled, &text).unwrap_err();
        assert_eq!(
            err,
            PrependError::DoubledContext {
                literal: "date('2020-01-02')".into()
            }
        );
        assert!(carries_valid_time_context(doubled));
        let err = err.to_string();
        assert!(err.contains("already has a FOR"), "{err}");
        assert!(err.contains("date('2020-01-02')"), "{err}");
    }
    // A comment or string mentioning FOR is not a context.
    prepend_valid_time("// FOR later\nRETURN 'FOR' AS x", &text).unwrap();
    assert!(!carries_valid_time_context(
        "// FOR later\nRETURN 'FOR' AS x"
    ));
    // The prepended text parses to the same statement as the hand-written one.
    let prefixed = prepend_valid_time("EXPLAIN MATCH (n) RETURN n", &text).unwrap();
    assert!(parse_cypher(&prefixed).unwrap().context.is_some());
}

/// `date()` is today (UTC) at execution, EXPLAIN renders it per execution,
/// and planning keeps the full counts.
#[test]
fn no_argument_date_is_today_per_execution() {
    let graph = graph();
    let none = HashMap::new();
    let query = "FOR VALID_TIME AS OF date() MATCH (w:Well) RETURN w.id";
    // Well 1 closed in 2010; Well 2 is open-ended.
    assert_eq!(
        well_ids(&read(&graph, query, &none).unwrap()),
        vec![Value::Int64(2)]
    );
    let explained = read(&graph, &format!("EXPLAIN {query}"), &none).unwrap();
    let first = format!("{:?}", explained.rows[0][1]);
    assert!(first.contains("instant: per execution"), "{first}");
    assert!(plan_instant(&lowered(&graph, query, &[]), &graph).is_none());
    // The function agrees with the prefix: today's UTC date.
    let today = read(&graph, "RETURN date() AS d", &none).unwrap();
    let utc = chrono::Utc::now().date_naive();
    match today.rows[0][0] {
        Value::DateTime(d) => assert!(d == utc || d.succ_opt() == Some(utc), "{d} vs {utc}"),
        ref other => panic!("{other:?}"),
    }
}

fn explained_targets(graph: &DirGraph, query: &str) -> String {
    let result = read(graph, &format!("EXPLAIN {AS_OF}{query}"), &HashMap::new())
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    format!("{:?}", result.rows[0][1])
}

/// The anonymous nodes a variable-length or shortest-path segment passes
/// through are untyped, so they reach every declared label — as the
/// hand-expanded spelling does.
#[test]
fn multi_hop_segments_reach_every_declared_label() {
    let graph = graph();
    let well = "targets=(:Well [vf, vt] closed)";
    let expanded = "MATCH (a:Field)-[:NEAR]-()-[:NEAR]-(b:Field) RETURN a";
    assert!(explained_targets(&graph, expanded).contains(well));
    for query in [
        "MATCH (a:Field)-[:NEAR*2]-(b:Field) RETURN a",
        "MATCH (a:Field)-[:NEAR*1..3]-(b:Field) RETURN a",
        "MATCH (a:Field)-[:NEAR*]-(b:Field) RETURN a",
        "MATCH p = shortestPath((a:Field)-[:NEAR*]-(b:Field)) RETURN p",
        "MATCH p = allShortestPaths((a:Field)-[:NEAR*..4]-(b:Field)) RETURN p",
    ] {
        let rendered = explained_targets(&graph, query);
        assert!(rendered.contains(well), "{query}: {rendered}");
    }
    // A segment of at most one hop has no intermediate node.
    for query in [
        "MATCH (a:Field)-[:NEAR*1]-(b:Field) RETURN a",
        "MATCH (a:Field)-[:NEAR*0..1]-(b:Field) RETURN a",
        "MATCH p = shortestPath((a:Field)-[:NEAR]-(b:Field)) RETURN p",
    ] {
        let rendered = explained_targets(&graph, query);
        assert!(
            rendered.contains("no declared targets"),
            "{query}: {rendered}"
        );
    }
}

/// Scalar functions that read a node's relationships outside the matcher
/// cannot see a guard, so a context refuses them — EXPLAIN included.
#[test]
fn topology_scalar_functions_are_refused_under_a_context() {
    let graph = graph();
    let none = HashMap::new();
    for call in [
        "degree(w)",
        "inDegree(w)",
        "OUTDEGREE(w)",
        "shortest_path_length(w, w)",
    ] {
        let query = format!("{AS_OF}MATCH (w:Well) RETURN {call} AS x");
        for text in [query.clone(), format!("EXPLAIN {query}")] {
            let err = read(&graph, &text, &none).unwrap_err();
            assert!(
                err.contains("is not available under a valid-time context;"),
                "{text}: {err}"
            );
            assert!(err.contains("COUNT { (n)--() }"), "{text}: {err}");
        }
        // Without the context the function runs.
        read(&graph, &format!("MATCH (w:Well) RETURN {call} AS x"), &none).unwrap();
    }
    let nested = lowered(
        &graph,
        &format!("{AS_OF}MATCH (w:Well) CALL {{ WITH w RETURN degree(w) AS d }} RETURN d"),
        &[],
    );
    assert!(refusal(&nested).unwrap().contains("degree"));
}

/// The transient equality index binds nodes without the matcher, so under
/// a graph filter the subsequent MATCH goes through the matcher instead (a
/// debug assertion guards the index's binding site): the join sees only the
/// wells valid at the instant.
#[test]
fn the_transient_equality_index_declines_under_a_graph_filter() {
    let graph = graph();
    let none = HashMap::new();
    let text = "UNWIND range(1, 80) AS i MATCH (f:Field) MATCH (w:Well {vf: f.vf}) \
                RETURN i, w.id";
    assert_eq!(read(&graph, text, &none).unwrap().rows.len(), 80);
    assert_eq!(
        read(&graph, &format!("{AS_OF}{text}"), &none)
            .unwrap()
            .rows
            .len(),
        80
    );
    let closed = format!("FOR VALID_TIME AS OF date('2015-01-01') {text}");
    assert!(read(&graph, &closed, &none).unwrap().rows.is_empty());
}

/// Every instant `prepend_valid_time` accepts is written as text its own
/// parser reads back to the same instant; one it cannot write is refused.
#[test]
fn prepend_round_trips_or_refuses_every_instant() {
    let graph = graph();
    let none = HashMap::new();
    let date = |y| chrono::NaiveDate::from_ymd_opt(y, 3, 4).unwrap();
    for year in [-5, 0, 1, 2020, 9999, 10000] {
        for value in [
            Value::DateTime(date(year)),
            Value::Timestamp(date(year).and_hms_opt(10, 30, 0).unwrap()),
        ] {
            match prepend_valid_time("RETURN 1", &value) {
                Ok(text) => {
                    let parsed = parse_cypher(&text).unwrap();
                    let instant = &parsed.context.as_ref().unwrap().instant;
                    let back = CypherExecutor::with_params(&graph, &none, None)
                        .evaluate_expression(instant, &ResultRow::new())
                        .unwrap();
                    assert_eq!(back, value, "{text}");
                }
                Err(err) => {
                    assert!(year < 0, "{value:?}: {err}");
                    assert!(matches!(err, PrependError::BadInstant(_)), "{err}");
                    assert!(err.to_string().starts_with("valid_at:"), "{err}");
                }
            }
        }
    }
}
