//! A multi-target `DELETE` frees its slots in the same order every run.
//!
//! The order deleted slots join petgraph's free lists decides which slot each
//! later `CREATE` reuses, and so the scan order of an un-ordered `MATCH`. The
//! targets used to be collected in a `std` `HashSet`, whose per-instance random
//! seed made two graphs given identical statements end up with different slots.

use super::*;

const STATEMENTS: [&str; 8] = [
    "UNWIND ['k1', 'k2', 'k3'] AS k CREATE (:T {k: k})",
    "UNWIND ['k4'] AS k CREATE (:T {k: k})",
    "MATCH (a:T {k: 'k1'}), (b:T {k: 'k3'}) CREATE (a)-[:R {i: 1}]->(b), (b)-[:R {i: 2}]->(a)",
    "MATCH (n:T {k: 'k2'}) DETACH DELETE n",
    "UNWIND ['k5'] AS k CREATE (:T {k: k})",
    "MATCH (n) DETACH DELETE n",
    "UNWIND ['k6', 'k7', 'k8'] AS k CREATE (:T {k: k})",
    "MATCH (a:T {k: 'k6'}), (b:T {k: 'k8'}) CREATE (a)-[:R {i: 3}]->(b), (b)-[:R {i: 4}]->(a)",
];

#[test]
fn identical_statements_on_two_graphs_leave_identical_slots() {
    for trial in 0..16 {
        let mut first = DirGraph::new();
        let mut second = DirGraph::new();
        for statement in STATEMENTS {
            run(&mut first, statement);
            run(&mut second, statement);
            assert_eq!(
                fingerprint(&mut first),
                fingerprint(&mut second),
                "trial {trial}: slots diverged after {statement}"
            );
        }
    }
}
