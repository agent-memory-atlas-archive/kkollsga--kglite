//! [`PatternExecutor::begin_chunks`] / [`PatternExecutor::next_chunk`]: a
//! chunked execution must return the matches of a whole-pattern
//! [`PatternExecutor::execute`] in the same order, hold only a bounded slice of
//! them at once, and decline the shapes whose whole-pattern handling a slice
//! cannot reproduce.

use super::*;
use crate::graph::core::pattern_matching::parser::parse_pattern;
use crate::graph::session::execute::{execute_mut, ExecuteOptions};

const GROUPS: i64 = 300;
const FAN_IN: i64 = 5;
const FAN_OUT: i64 = 4;
const PER_GROUP: usize = (FAN_IN * FAN_OUT) as usize;
const PATTERN: &str = "(g:Grp)<-[:IN]-(m:Mem)-[:OWNS]->(t:Item)";

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_mut(graph, query, &opts).unwrap_or_else(|e| panic!("setup query failed: {query}: {e}"));
}

fn grouped() -> DirGraph {
    let mut graph = DirGraph::new();
    run(
        &mut graph,
        &format!("UNWIND range(1, {GROUPS}) AS g CREATE (:Grp {{gid: g}})"),
    );
    run(
        &mut graph,
        &format!(
            "MATCH (g:Grp) UNWIND range(1, {FAN_IN}) AS i \
             CREATE (:Mem {{mid: g.gid * 10 + i}})-[:IN]->(g)"
        ),
    );
    run(
        &mut graph,
        &format!(
            "MATCH (m:Mem) UNWIND range(1, {FAN_OUT}) AS j CREATE (m)-[:OWNS]->(:Item {{n: j}})"
        ),
    );
    graph
}

fn shape(matches: &[PatternMatch]) -> Vec<String> {
    matches
        .iter()
        .map(|m| format!("{:?}", m.bindings))
        .collect()
}

#[test]
fn chunks_concatenate_to_the_whole_pattern_in_order_and_stay_bounded() {
    let graph = grouped();
    let params = HashMap::new();
    let pattern = parse_pattern(PATTERN).expect("pattern parses");
    let executor = PatternExecutor::new_lightweight_with_params(&graph, None, &params);
    let whole = executor.execute(&pattern).expect("whole pattern");
    assert_eq!(whole.len(), GROUPS as usize * PER_GROUP);

    let target = 500;
    let mut chunker = executor
        .begin_chunks_sized(&pattern, target)
        .expect("seeds")
        .expect("a plain pattern chunks");
    let mut joined = Vec::new();
    let mut widest = 0;
    let mut chunks = 0;
    while let Some(chunk) = executor.next_chunk(&pattern, &mut chunker).expect("chunk") {
        widest = widest.max(chunk.len());
        chunks += 1;
        joined.extend(chunk);
    }
    assert_eq!(shape(&joined), shape(&whole));
    // The widest hop of a chunk is its last (a group yields PER_GROUP matches),
    // so a chunk sized from the previous one's width stays at the target.
    assert!(widest <= target + PER_GROUP, "widest chunk {widest}");
    assert!(chunks >= 10, "{chunks} chunks of {} matches", whole.len());
}

#[test]
fn a_pattern_that_must_run_whole_is_not_chunked() {
    let graph = grouped();
    let params = HashMap::new();
    let pattern = parse_pattern(PATTERN).expect("pattern parses");

    let capped = PatternExecutor::new_lightweight_with_params(&graph, Some(10), &params);
    assert!(capped.begin_chunks(&pattern).expect("seeds").is_none());

    let deduped = PatternExecutor::new_lightweight_with_params(&graph, None, &params)
        .set_distinct_target(Some("t".to_string()));
    assert!(deduped.begin_chunks(&pattern).expect("seeds").is_none());
}

#[test]
fn a_single_node_pattern_and_an_empty_seed_set_chunk_too() {
    let graph = grouped();
    let params = HashMap::new();
    let executor = PatternExecutor::new_lightweight_with_params(&graph, None, &params);

    let nodes = parse_pattern("(t:Item)").expect("pattern parses");
    let whole = executor.execute(&nodes).expect("whole");
    let mut chunker = executor
        .begin_chunks_sized(&nodes, 100)
        .expect("seeds")
        .expect("chunks");
    let mut joined = Vec::new();
    while let Some(chunk) = executor.next_chunk(&nodes, &mut chunker).expect("chunk") {
        joined.extend(chunk);
    }
    assert_eq!(shape(&joined), shape(&whole));

    let none = parse_pattern("(g:Nothing)<-[:IN]-(m:Mem)").expect("pattern parses");
    let mut chunker = executor
        .begin_chunks(&none)
        .expect("seeds")
        .expect("chunks");
    assert!(executor
        .next_chunk(&none, &mut chunker)
        .expect("chunk")
        .is_none());
}
