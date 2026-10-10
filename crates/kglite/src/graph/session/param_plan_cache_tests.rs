//! A statement with bound parameters is plan-cached per exact parameter
//! values: the planner bakes the values it reads (index pushdown, `LIMIT $n`,
//! anchors) into the plan, so a plan may serve only the values it was built
//! from. These cases pin both halves — repeated values hit, and different
//! values never see another call's plan — plus the no-churn rule for
//! a workload that never repeats a value.

use std::collections::HashMap;

use super::execute::{execute_mut, execute_read, ExecuteOptions};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::languages::cypher::plan_cache;
use crate::graph::languages::cypher::plan_cache::instrumentation::{self, CacheStats};

fn events(stats: CacheStats) -> (u64, u64, u64) {
    (stats.lookups, stats.hits, stats.insertions)
}

fn seeded() -> DirGraph {
    let mut graph = DirGraph::new();
    let none = HashMap::new();
    for q in [
        "UNWIND range(1, 50) AS i CREATE (:Item {id: i, name: 'item' + toString(i), n: i * 10})",
        "MATCH (a:Item {id: 1}), (b:Item {id: 2}) CREATE (a)-[:NEXT]->(b)",
    ] {
        execute_mut(&mut graph, q, &ExecuteOptions::eager(&none)).expect("seed");
    }
    graph
}

fn run(graph: &DirGraph, q: &str, params: &[(&str, Value)]) -> Vec<Vec<Value>> {
    let params: HashMap<String, Value> = params
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect();
    execute_read(graph, q, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .result
        .rows
}

const LOOKUP: &str = "MATCH (n:Item) WHERE n.id = $x RETURN n.name AS v";
const INLINE: &str = "MATCH (n:Item {id: $x}) RETURN n.name AS v";
const LIMITED: &str = "MATCH (n:Item) RETURN n.id AS v ORDER BY v LIMIT $k";

fn s(text: &str) -> Value {
    Value::String(text.into())
}

#[test]
fn the_third_identical_parameterised_run_hits_and_the_first_two_do_not() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    instrumentation::reset();
    for _ in 0..3 {
        assert_eq!(
            run(&graph, LOOKUP, &[("x", Value::Int64(7))]),
            vec![vec![s("item7")]]
        );
    }
    assert_eq!(
        events(instrumentation::totals().read),
        (2, 1, 1),
        "first sighting plans only, second looks up and stores, third hits"
    );
}

#[test]
fn different_values_never_share_a_plan() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    for query in [LOOKUP, INLINE] {
        // Prime a plan for x = 7 until it is stored and hitting.
        for _ in 0..3 {
            run(&graph, query, &[("x", Value::Int64(7))]);
        }
        for (x, expect) in [(8, "item8"), (7, "item7"), (49, "item49")] {
            for _ in 0..3 {
                assert_eq!(
                    run(&graph, query, &[("x", Value::Int64(x))]),
                    vec![vec![s(expect)]],
                    "{query} with x = {x}"
                );
            }
        }
        assert!(run(&graph, query, &[("x", Value::Int64(999))]).is_empty());
        assert!(run(&graph, query, &[("x", Value::Null)]).is_empty());
    }
}

#[test]
fn a_limit_taken_from_a_parameter_follows_each_value() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    for round in 0..3 {
        for k in [1, 3, 5, 3] {
            let rows = run(&graph, LIMITED, &[("k", Value::Int64(k))]);
            assert_eq!(rows.len() as i64, k, "round {round}, k = {k}");
        }
    }
}

#[test]
fn a_changed_parameter_name_or_type_is_a_different_key() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    for _ in 0..3 {
        run(&graph, LOOKUP, &[("x", Value::Int64(7))]);
    }
    // Same text, same spelling of the value, different type: the string
    // matches no integer id.
    for _ in 0..3 {
        assert!(run(&graph, LOOKUP, &[("x", s("7"))]).is_empty());
    }
    // An extra bound name is part of the identity too.
    for _ in 0..3 {
        assert_eq!(
            run(
                &graph,
                LOOKUP,
                &[("x", Value::Int64(7)), ("unused", Value::Int64(1))]
            ),
            vec![vec![s("item7")]]
        );
    }
}

#[test]
fn a_missing_parameter_still_errors_after_a_bound_run_was_cached() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    for _ in 0..3 {
        run(&graph, LOOKUP, &[("x", Value::Int64(7))]);
    }
    let none = HashMap::new();
    for _ in 0..2 {
        let err = execute_read(&graph, LOOKUP, &ExecuteOptions::eager(&none))
            .err()
            .expect("an unbound $x must raise");
        assert!(err.to_string().contains("Missing parameter: $x"), "{err}");
    }
}

#[test]
fn values_that_never_repeat_store_no_plan() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    instrumentation::reset();
    for x in 1..=40 {
        assert_eq!(
            run(&graph, LOOKUP, &[("x", Value::Int64(x))]).len(),
            1,
            "x = {x}"
        );
    }
    assert_eq!(
        events(instrumentation::totals().read),
        (0, 0, 0),
        "a workload that never repeats a value must not look up, take the write lock or evict parameter-free plans"
    );
}

#[test]
fn a_map_parameter_is_never_cached() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    let map = Value::Map(
        [("a".to_string(), Value::Int64(1))]
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
            .into(),
    );
    instrumentation::reset();
    for _ in 0..4 {
        run(&graph, "RETURN $m AS m", &[("m", map.clone())]);
    }
    assert_eq!(events(instrumentation::totals().read), (0, 0, 0));
}

#[test]
fn a_parameterised_workload_past_its_capacity_evicts_no_parameter_free_plan() {
    let _guard = plan_cache::TEST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let graph = seeded();
    const PLAIN: &str = "MATCH (n:Item) RETURN count(n) AS c";
    run(&graph, PLAIN, &[]);
    run(&graph, PLAIN, &[]);
    // Two laps over more distinct values than the parameter cache holds: the
    // second lap stores every one of them, evicting along the way.
    let laps = plan_cache::PARAM_CACHE_CAPACITY as i64 + 100;
    instrumentation::reset();
    for _ in 0..2 {
        for x in 0..laps {
            run(&graph, LOOKUP, &[("x", Value::Int64(x))]);
        }
    }
    let churn = instrumentation::totals().read;
    assert!(
        churn.evictions > 0,
        "the lap must overflow the cache: {churn:?}"
    );
    instrumentation::reset();
    run(&graph, PLAIN, &[]);
    assert_eq!(
        events(instrumentation::totals().read),
        (1, 1, 0),
        "the parameter-free plan survived the parameterised churn"
    );
}
