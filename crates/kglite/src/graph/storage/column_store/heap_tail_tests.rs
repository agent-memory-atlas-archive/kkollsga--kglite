//! Heap tails: a transaction's appends to a large type under a writer overlay
//! copy no column, read back on every route, roll back, save, and fold into
//! the store when the overlay folds (`tail.rs`, "Heap tails").

use super::tail::HEAP_TAIL_MIN_ROWS;
use super::{column_clones, reset_column_clones};
use crate::datatypes::Value;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_mut, execute_read, CommitOutcome, ExecuteOptions, Session};
use std::collections::HashMap;

const ROWS: u32 = HEAP_TAIL_MIN_ROWS + 1_000;

fn execute(graph: &mut DirGraph, query: &str) -> Result<(), String> {
    execute_mut(graph, query, &ExecuteOptions::eager(&HashMap::new()))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn one(graph: &DirGraph, query: &str) -> Value {
    let out = execute_read(graph, query, &ExecuteOptions::eager(&HashMap::new()))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    out.result.rows[0][0].clone()
}

fn seeded_graph() -> DirGraph {
    let mut graph = DirGraph::new();
    execute(
        &mut graph,
        &format!(
            "UNWIND range(0, {}) AS i CREATE (:Item {{id: i, title: 'n' + toString(i), \
             score: i, tag: 'base'}})",
            ROWS - 1
        ),
    )
    .unwrap();
    graph
}

fn seed() -> Session {
    Session::new(seeded_graph())
}

fn heap_tail(graph: &DirGraph) -> bool {
    graph.column_store("Item").unwrap().has_heap_tail()
}

/// Every read route a tail row must answer on: the id point lookup, a
/// filtered scan the column fast path would serve, a count and an aggregate.
fn assert_reads(graph: &DirGraph, created: &[i64]) {
    let rows = i64::from(ROWS) + created.len() as i64;
    assert_eq!(
        one(graph, "MATCH (n:Item) RETURN count(n)"),
        Value::Int64(rows)
    );
    for &id in created {
        assert_eq!(
            one(
                graph,
                &format!("MATCH (n:Item {{id: {id}}}) RETURN n.title")
            ),
            Value::String(format!("t{id}"))
        );
    }
    assert_eq!(
        one(graph, "MATCH (n:Item) WHERE n.tag = 'tx' RETURN count(n)"),
        Value::Int64(created.len() as i64)
    );
    assert_eq!(
        one(
            graph,
            &format!("MATCH (n:Item) WHERE n.score >= {ROWS} RETURN sum(n.score)")
        ),
        Value::Int64(created.iter().sum())
    );
}

fn create(id: i64) -> String {
    format!("CREATE (:Item {{id: {id}, title: 't{id}', score: {id}, tag: 'tx'}})")
}

#[test]
fn a_transaction_append_on_a_large_type_copies_no_column() {
    let session = seed();
    let mut created = Vec::new();
    for id in [i64::from(ROWS), i64::from(ROWS) + 1] {
        let mut tx = session.begin();
        let working = tx.working_mut().unwrap();
        reset_column_clones();
        execute(working, &create(id)).unwrap();
        assert_eq!(column_clones(), 0, "the append copied a shared column");
        assert!(heap_tail(working));
        created.push(id);
        assert_reads(working, &created);
        assert!(matches!(
            session.commit(tx, true),
            CommitOutcome::Committed { .. }
        ));
        let published = session.snapshot();
        assert!(!heap_tail(&published), "the commit's fold left the tail");
        assert_reads(&published, &created);
    }
}

#[test]
fn a_small_type_or_an_unshared_store_takes_no_tail() {
    let mut graph = DirGraph::new();
    execute(
        &mut graph,
        "UNWIND range(0, 99) AS i CREATE (:Item {id: i})",
    )
    .unwrap();
    let session = Session::new(graph);
    let mut tx = session.begin();
    execute(tx.working_mut().unwrap(), "CREATE (:Item {id: 100})").unwrap();
    assert!(!heap_tail(tx.working_mut().unwrap()), "below the row floor");

    let mut big = seeded_graph();
    execute(&mut big, &create(i64::from(ROWS))).unwrap();
    assert!(!heap_tail(&big), "no overlay, no tail");
}

#[test]
fn a_failed_statement_in_a_transaction_rolls_its_tail_rows_back() {
    let session = seed();
    let mut tx = session.begin();
    let working = tx.working_mut().unwrap();
    let id = i64::from(ROWS);
    execute(working, &create(id)).unwrap();
    let error = execute(
        working,
        &format!(
            "UNWIND [1, 2] AS i CREATE (:Item {{id: {} + i, tag: 'tx', \
             score: CASE WHEN i = 2 THEN duration({{months: 2147483648}}) ELSE i END}})",
            id
        ),
    );
    assert!(error.is_err());
    assert_reads(working, &[id]);
    execute(working, &create(id + 5)).unwrap();
    assert_reads(working, &[id, id + 5]);
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
    assert_reads(&session.snapshot(), &[id, id + 5]);
}

#[test]
fn a_held_reader_keeps_the_tail_until_it_drops_and_a_save_holds_every_row() {
    let session = seed();
    let holder = session.snapshot();
    let id = i64::from(ROWS);
    let mut tx = session.begin();
    execute(tx.working_mut().unwrap(), &create(id)).unwrap();
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
    let published = session.snapshot();
    assert!(published.graph.is_forked());
    assert!(heap_tail(&published));
    assert_reads(&published, &[id]);
    // The compiled scan filter still serves the base rows; the tail row
    // reaches the result through the row route.
    use crate::graph::core::pattern_matching::column_filter::{reset_rows_filtered, rows_filtered};
    reset_rows_filtered();
    assert_eq!(
        one(&published, "MATCH (n:Item) WHERE n.tag = 'tx' RETURN n.id"),
        Value::Int64(id)
    );
    assert!(
        rows_filtered() >= ROWS as usize,
        "the compiled filter declined"
    );
    assert_eq!(
        one(&holder, "MATCH (n:Item) RETURN count(n)"),
        Value::Int64(i64::from(ROWS))
    );

    let mut bytes = Vec::new();
    crate::graph::io::file::write_kgl_to(&published, &mut bytes).unwrap();
    let loaded = crate::graph::io::file::load_kgl_bytes(&bytes).unwrap();
    assert_reads(&loaded, &[id]);

    drop((holder, published));
    let mut tx = session.begin();
    execute(tx.working_mut().unwrap(), &create(id + 1)).unwrap();
    assert!(matches!(
        session.commit(tx, true),
        CommitOutcome::Committed { .. }
    ));
    let compacted = session.snapshot();
    assert!(!compacted.graph.is_forked());
    assert!(!heap_tail(&compacted));
    assert_reads(&compacted, &[id, id + 1]);
}
