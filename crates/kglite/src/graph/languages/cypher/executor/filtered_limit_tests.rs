//! A filtered `MATCH … WHERE … RETURN … LIMIT k` holds about `k` first-pattern
//! matches, not every match of the type, in every storage mode. Pinned by the
//! widest match buffer the executor held ([`super::first_rows_probe`]), which
//! is deterministic, rather than by the process footprint, which other work in
//! the process moves.

use std::collections::HashMap;

use super::first_rows_probe;
use crate::datatypes::values::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::execute::{execute_mut, execute_read, ExecuteOptions};
use crate::graph::storage::mode::{new_dir_graph_in_mode, StorageMode};

const NODES: i64 = 20_000;
const LIMIT: usize = 50;

fn graph(mode: StorageMode, dir: &tempfile::TempDir) -> DirGraph {
    let path = matches!(mode, StorageMode::Disk).then(|| dir.path());
    let mut graph = new_dir_graph_in_mode(mode, path).expect("graph");
    let params = HashMap::from([("n".to_string(), Value::Int64(NODES))]);
    execute_mut(
        &mut graph,
        "UNWIND range(1, $n) AS i CREATE (:P {id: i, status: CASE WHEN i % 2 = 0 THEN 'a' ELSE 'b' END, \
         closed: CASE WHEN i % 3 = 0 THEN 1 ELSE null END})",
        &ExecuteOptions::eager(&params),
    )
    .expect("fixture");
    graph
}

/// Rows returned, and the widest match buffer the statement held.
fn run(graph: &DirGraph, query: &str) -> (usize, usize) {
    let params = HashMap::new();
    first_rows_probe::take();
    let rows = execute_read(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .result
        .rows
        .len();
    (rows, first_rows_probe::take())
}

#[test]
fn a_filtered_limit_holds_about_the_limit_in_every_mode() {
    for mode in [StorageMode::Memory, StorageMode::Mapped, StorageMode::Disk] {
        let dir = tempfile::tempdir().unwrap();
        let graph = graph(mode, &dir);
        for query in [
            // Subsumed: the scan stops at the limit.
            "MATCH (p:P) WHERE p.status = 'a' RETURN p.id LIMIT 50",
            "MATCH (p:P) WHERE p.id > 5 RETURN p.id LIMIT 50",
            // Residual: drained a slice of start nodes at a time.
            "MATCH (p:P) WHERE p.closed IS NULL RETURN p.id LIMIT 50",
            "MATCH (p:P) WHERE p.id % 7 = 0 RETURN p.id LIMIT 50",
        ] {
            let (rows, widest) = run(&graph, query);
            assert_eq!(rows, LIMIT, "{mode:?}: {query}");
            // The chunks grow 4x from one start node, so the slice that fills
            // the limit is at most 4x the matches it needed (here < 400).
            assert!(widest <= 2_000, "{mode:?}: {query} held {widest} matches");
        }
        // The control: with no LIMIT every match is held.
        let (rows, widest) = run(&graph, "MATCH (p:P) WHERE p.status = 'a' RETURN p.id");
        assert_eq!(
            (rows, widest),
            (NODES as usize / 2, NODES as usize / 2),
            "{mode:?}"
        );
    }
}
