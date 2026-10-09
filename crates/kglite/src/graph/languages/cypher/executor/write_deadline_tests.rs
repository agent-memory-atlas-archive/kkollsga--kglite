//! The write engine's deadline/cancel coverage.
//!
//! Write loops used to poll every 4,096 rows, so a statement of fewer rows
//! polled once, at row 0, and a slow per-row write (a disk SET clones the
//! touched column) overran its deadline by the whole clause. The poll-count
//! test is timing-free: it counts the polls one statement performs and is red
//! on a cadence that skips rows. The cancel test pins that a write clause's
//! value expression carries the interrupt — before it did, the evaluator was
//! built with no deadline and no cancel flag and ran to completion.

use super::take_write_polls;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::session::{execute_mut, execute_read, ExecuteOptions};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Rows per statement.
const ROWS: usize = 3;

fn run(graph: &mut DirGraph, query: &str) {
    let params = HashMap::new();
    execute_mut(graph, query, &ExecuteOptions::eager(&params))
        .unwrap_or_else(|e| panic!("{query}: {e}"));
}

fn count(graph: &DirGraph, query: &str) -> i64 {
    let params = HashMap::new();
    let outcome = execute_read(graph, query, &ExecuteOptions::eager(&params)).unwrap();
    match &outcome.result.rows[0][0] {
        crate::datatypes::values::Value::Int64(n) => *n,
        other => panic!("{query}: expected a count, got {other:?}"),
    }
}

/// Polls performed by `query` alone.
fn polls(graph: &mut DirGraph, query: &str) -> usize {
    take_write_polls();
    run(graph, query);
    take_write_polls()
}

#[test]
fn every_write_loop_polls_per_row_cadence() {
    let mut graph = DirGraph::new();
    let cases = [
        format!("UNWIND range(1, {ROWS}) AS i CREATE (:W {{id: i}})"),
        "MATCH (n:W) SET n.x = n.id".to_string(),
        "MATCH (n:W) REMOVE n.x".to_string(),
        // Twice: the first run creates (and so also polls in CREATE's loop);
        // the second only matches, which leaves MERGE's own row loop.
        format!("UNWIND range(1, {ROWS}) AS i MERGE (:M {{id: i}})"),
        format!("UNWIND range(1, {ROWS}) AS i MERGE (:M {{id: i}})"),
        "MATCH (n:W) DETACH DELETE n".to_string(),
    ];
    for query in cases {
        let performed = polls(&mut graph, &query);
        assert!(
            performed >= ROWS,
            "{query}: {performed} poll(s) over {ROWS} rows — a row loop skipped its poll"
        );
    }
    assert_eq!(count(&graph, "MATCH (n:W) RETURN count(n)"), 0);
}

#[test]
fn a_cancel_raised_mid_expression_aborts_a_one_row_create() {
    // A pre-set flag would be caught by the clause-boundary poll before the
    // CREATE ran; the flag has to rise while the value expression evaluates.
    static CANCEL: AtomicBool = AtomicBool::new(false);
    let params = HashMap::from([(
        "s".to_string(),
        crate::datatypes::values::Value::String("a".repeat(10_000)),
    )]);
    let mut opts = ExecuteOptions::eager(&params);
    // 20,000 range() calls, each polling the evaluator's interrupt, and ~9M
    // list items in total — under the 10M collection backstop, which the
    // uncancelled meter below proves (the cancel flag is re-read on *any*
    // error, so a budget error would otherwise pass as a cancel).
    let query = "CREATE (:T {v: reduce(s = 0, j IN range(1, 20000) | s + size(range(1, 50)) + size(replace($s, 'a', 'b')))})";

    // Non-vacuity meter: uncancelled, the statement succeeds, and takes `full`.
    let mut meter = DirGraph::new();
    let started = Instant::now();
    execute_mut(&mut meter, query, &opts).unwrap();
    let full = started.elapsed();
    assert_eq!(count(&meter, "MATCH (t:T) RETURN count(t)"), 1);

    let mut graph = DirGraph::new();
    opts.cancel = Some(crate::api::session::CancelToken::from_static(&CANCEL));
    let raise_after = full / 10;
    let raiser = std::thread::spawn(move || {
        std::thread::sleep(raise_after);
        CANCEL.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let outcome = execute_mut(&mut graph, query, &opts);
    let elapsed = started.elapsed();
    raiser.join().unwrap();
    assert!(
        matches!(outcome, Err(KgError::Cancelled)),
        "expected Cancelled, got {:?}",
        outcome.map(|o| o.result.rows.len())
    );
    assert_eq!(count(&graph, "MATCH (t:T) RETURN count(t)"), 0);
    // Without the evaluator carrying the flag the expression ran to its end;
    // with it, the abort follows the flag within one range() call.
    assert!(
        elapsed < full / 2,
        "cancel raised at {raise_after:?} was observed only after {elapsed:?} (uncancelled: {full:?})"
    );
}
