//! The heap one expanded row holds.
//!
//! A hop copies its parent row's bindings and pushes one or two more. A bare
//! `Vec::clone` is exact-fit, so that first push reallocated the copy to the
//! amortised four-slot minimum: with a 160 B `(String, MatchBinding)` slot,
//! 640 B per row where 320 B holds it. A depth-3 expansion keeps one row per
//! reached path alive until the aggregate consumes it, so the slack scaled
//! with the reached set (28 -> 63 MB at 83k reached nodes), and the peak
//! doubled when only the seed count did.

use super::*;
use crate::datatypes::Value;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};
use crate::test_alloc::peak_during;

const NODES: i64 = 20_000;

fn graph() -> DirGraph {
    let mut g = new_dir_graph_in_mode(StorageMode::Memory, None).expect("graph");
    let params = HashMap::from([("n".to_string(), Value::Int64(NODES))]);
    let opts = ExecuteOptions::eager(&params);
    execute_mut(
        &mut g,
        "UNWIND range(0, $n - 1) AS i CREATE (:Person {id: i})",
        &opts,
    )
    .expect("nodes");
    // Out-degree 3 with spread-out targets, so depth 3 reaches a large set.
    let mut state: i64 = 20_260_821;
    let mut pairs = Vec::new();
    for i in 0..NODES {
        for _ in 0..12 {
            state = (state * 1_103_515_245 + 12_345) & 0x7FFF_FFFF;
            pairs.push(Value::List(vec![
                Value::Int64(i),
                Value::Int64(state % NODES),
            ]));
        }
    }
    let params = HashMap::from([("pairs".to_string(), Value::List(pairs))]);
    execute_mut(
        &mut g,
        "UNWIND $pairs AS p MATCH (a:Person {id: p[0]}), (b:Person {id: p[1]}) \
         CREATE (a)-[:KNOWS]->(b)",
        &ExecuteOptions::eager(&params),
    )
    .expect("edges");
    g
}

#[test]
fn clone_with_spare_reserves_exactly_what_the_hop_pushes() {
    let pm = PatternMatch {
        bindings: vec![("p".to_string(), MatchBinding::NodeRef(NodeIndex::new(0)))],
        exact_path: None,
    };
    let grown = pm.clone_with_spare(2);
    assert_eq!(grown.bindings.len(), 1);
    assert_eq!(grown.bindings.capacity(), 3, "room for the hop, no more");
}

#[test]
fn a_depth_three_expansion_holds_no_slack_per_row() {
    let g = graph();
    let ids: Vec<Value> = (0..20).map(|i| Value::Int64(i * 97 % NODES)).collect();
    let params = HashMap::from([("ids".to_string(), Value::List(ids))]);
    let (out, peak) = peak_during(|| {
        execute_read(
            &g,
            "MATCH (p:Person)-[:KNOWS*1..3]->(f:Person) WHERE p.id IN $ids \
             RETURN count(DISTINCT f) AS reached",
            &ExecuteOptions::eager(&params),
        )
        .expect("query")
    });
    let Value::Int64(reached) = out.result.rows[0][0] else {
        panic!("count is an integer")
    };
    assert!(
        reached > 8_000,
        "premise: the expansion is wide ({reached})"
    );
    // Measured on this fixture in the debug profile: 807 B per reached node
    // with the exact-fit clone, 487 B with `clone_with_spare`.
    let per_reached = peak / reached as usize;
    assert!(
        per_reached <= BOUND_PER_REACHED,
        "{per_reached} B of peak heap per reached node (bound {BOUND_PER_REACHED})"
    );
}

const BOUND_PER_REACHED: usize = 600;
