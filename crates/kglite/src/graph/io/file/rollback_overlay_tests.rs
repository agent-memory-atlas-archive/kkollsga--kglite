//! A rolled-back statement leaves the write overlay of a frozen graph as it
//! found it: the entries it added are gone, so a refused or failed statement
//! costs no memory and a write budget reads what it read before.

use super::frozen_tests::{dump, saved_topo};
use super::index_sections_tests::{load, load_owned, opts, run};
use super::*;
use crate::graph::session::{execute_mut, Session};

/// Statements that write, then fail, in every way a journal has to undo: a
/// create, a delete with its edges, a property write, an edge create.
const FAILING: [&str; 4] = [
    "UNWIND range(1, 5) AS i CREATE (:A {id: 5000 + i, n: 10 / (3 - i)})",
    "MATCH (a:A) WHERE a.id IN [2, 6, 7] DETACH DELETE a WITH count(*) AS c UNWIND [1, 0] AS d CREATE (:A {id: 7000 + d, n: c / d})",
    "MATCH (a:A {id: 8}) SET a.n = 9 WITH a UNWIND [1, 0] AS d CREATE (:C {id: 7100 + d, n: 1 / d})",
    "MATCH (a:A {id: 9}), (b:B {id: 3}) CREATE (a)-[:K1]->(b), (a)-[:NEW {w: 1}]->(b) WITH a UNWIND [1, 0] AS d CREATE (:A {id: 7200 + d, n: 1 / d})",
];

fn entries(graph: &DirGraph) -> usize {
    graph.graph.overlay_entries().expect("an overlay")
}

fn heap(graph: &DirGraph) -> usize {
    graph.graph_info().columnar_heap_bytes
}

#[test]
fn rolled_back_statements_leave_the_overlay_flat() {
    let (_tmp, path) = saved_topo();
    let mut graph = load_owned(&path, StorageMode::Mapped);
    run(&mut graph, "CREATE (:A {id: 9000, n: 1, s: 'kept'})");
    run(&mut graph, "MATCH (a:A {id: 3}) SET a.n = 33");
    let (before, before_entries, before_heap) = (dump(&graph), entries(&graph), heap(&graph));
    for round in 0..50 {
        for statement in FAILING {
            assert!(
                execute_mut(&mut graph, statement, &opts()).is_err(),
                "{statement} must fail so it rolls back"
            );
        }
        assert_eq!(entries(&graph), before_entries, "round {round}: entries");
        assert_eq!(heap(&graph), before_heap, "round {round}: column heap");
    }
    assert_eq!(dump(&graph), before);
    // The graph carries on from the state it had: the same next writes land
    // where a graph that never failed puts them.
    let mut reference = load_owned(&path, StorageMode::Memory);
    run(&mut reference, "CREATE (:A {id: 9000, n: 1, s: 'kept'})");
    run(&mut reference, "MATCH (a:A {id: 3}) SET a.n = 33");
    for step in [
        "CREATE (:A {id: 9100, n: 2, s: 'next'})",
        "MATCH (a:A {id: 4}) DETACH DELETE a",
        "CREATE (:B {id: 9101, n: 3, s: 'later'})",
    ] {
        run(&mut graph, step);
        run(&mut reference, step);
    }
    assert_eq!(dump(&graph), dump(&reference));
}

#[test]
fn rolled_back_statements_in_a_transaction_leave_its_overlay_flat() {
    let (_tmp, path) = saved_topo();
    let session = Session::from_arc(load(&path, StorageMode::Mapped));
    let mut tx = session.begin();
    let working = tx.working_mut().expect("working copy");
    run(working, "CREATE (:A {id: 9000, n: 1, s: 'kept'})");
    let (before, before_entries) = (dump(working), entries(working));
    for _ in 0..30 {
        for statement in FAILING {
            assert!(execute_mut(working, statement, &opts()).is_err());
        }
    }
    assert_eq!(entries(working), before_entries);
    assert_eq!(dump(working), before);
}
