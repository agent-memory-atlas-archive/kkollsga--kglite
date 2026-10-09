//! `execute_read_cursor`: the rows equal `execute_read`'s, a streamable shape
//! stays bounded, and everything the cursor holds is released with it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::cursor::execute_read_cursor;
use super::execute::{execute_mut, execute_read, ExecuteOptions};
use super::CancelToken;
use crate::datatypes::Value;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;

fn graph(n: i64) -> Arc<DirGraph> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut g = DirGraph::new();
    execute_mut(
        &mut g,
        &format!(
            "UNWIND range(1, {n}) AS i CREATE (:Item {{id: i, seq: i, name: 'n' + toString(i)}})"
        ),
        &opts,
    )
    .expect("seed");
    if n <= 3000 {
        execute_mut(
            &mut g,
            "MATCH (a:Item), (b:Item) WHERE b.seq = a.seq + 1 CREATE (a)-[:NEXT]->(b)",
            &opts,
        )
        .expect("edges");
    }
    Arc::new(g)
}

fn drain(cursor: &mut super::Cursor, batch: usize) -> Vec<Vec<Value>> {
    let mut rows = Vec::new();
    loop {
        let got = cursor.next_batch(batch).expect("batch");
        if got.is_empty() {
            return rows;
        }
        assert!(got.len() <= batch);
        rows.extend(got);
    }
}

fn eager_rows(g: &DirGraph, q: &str) -> Vec<Vec<Value>> {
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    execute_read(g, q, &opts).expect("read").result.rows
}

const STREAMED: &[&str] = &[
    "MATCH (n:Item) RETURN n.seq, n.name",
    "MATCH (n:Item) WHERE n.seq % 3 = 0 RETURN n.seq AS s, n.seq * 2 AS d",
    "MATCH (a:Item)-[:NEXT]->(b:Item) RETURN a.seq, b.name",
    "MATCH (n:Item {seq: 7}) RETURN n.name",
    "MATCH (n:Item) RETURN n",
];

const MATERIALISED: &[&str] = &[
    "MATCH (n:Item) RETURN n.seq ORDER BY n.seq DESC",
    "MATCH (n:Item) RETURN count(n)",
    "MATCH (n:Item) RETURN DISTINCT n.seq % 5",
    "MATCH (n:Item) RETURN n.seq LIMIT 5",
    "MATCH (n:Item) RETURN n.seq AS `n.seq` UNION MATCH (m:Item) RETURN m.seq AS `n.seq`",
];

#[test]
fn cursor_rows_equal_execute_read_for_every_shape() {
    let g = graph(3000);
    for q in STREAMED.iter().chain(MATERIALISED) {
        let params = HashMap::new();
        let opts = ExecuteOptions::eager(&params);
        let mut cursor = execute_read_cursor(Arc::clone(&g), q, &opts).expect(q);
        let want_cols = execute_read(&g, q, &opts).expect(q).result.columns;
        assert_eq!(cursor.columns(), want_cols.as_slice(), "{q}");
        let got = drain(&mut cursor, 700);
        assert_eq!(got, eager_rows(&g, q), "{q}");
    }
}

#[test]
fn streamed_flag_names_the_memory_class() {
    let g = graph(50);
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    for q in STREAMED {
        let c = execute_read_cursor(Arc::clone(&g), q, &opts).expect(q);
        assert!(c.streamed(), "{q} should stream");
    }
    for q in MATERIALISED {
        let c = execute_read_cursor(Arc::clone(&g), q, &opts).expect(q);
        assert!(!c.streamed(), "{q} must materialise");
    }
}

/// The red proof: a cursor that materialised the result would have produced
/// every row before the first batch came back.
#[test]
fn streamed_worker_runs_only_a_few_batches_ahead() {
    let g = graph(30_000);
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut cursor =
        execute_read_cursor(Arc::clone(&g), "MATCH (n:Item) RETURN n.seq", &opts).expect("open");
    assert!(cursor.streamed());
    let first = cursor.next_batch(10).expect("batch");
    assert_eq!(first.len(), 10);
    std::thread::sleep(Duration::from_millis(400));
    let ahead = cursor.rows_produced();
    assert!(ahead >= 10, "the worker produced the batch it handed over");
    assert!(
        ahead <= 4 * 1024,
        "worker ran {ahead} rows ahead of a consumer that read 10"
    );
    assert_eq!(drain(&mut cursor, 5000).len() + 10, 30_000);
}

#[test]
fn dropping_the_cursor_releases_the_snapshot() {
    let g = graph(30_000);
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut cursor =
        execute_read_cursor(Arc::clone(&g), "MATCH (n:Item) RETURN n.seq", &opts).expect("open");
    cursor.next_batch(1).expect("batch");
    assert!(Arc::strong_count(&g) > 1, "the cursor holds the snapshot");
    drop(cursor);
    assert_eq!(
        Arc::strong_count(&g),
        1,
        "drop joined the worker and freed it"
    );
}

#[test]
fn cancel_applies_between_batches() {
    let g = graph(30_000);
    let params = HashMap::new();
    let token = CancelToken::new();
    let mut opts = ExecuteOptions::eager(&params);
    opts.cancel = Some(token.clone());
    let mut cursor =
        execute_read_cursor(Arc::clone(&g), "MATCH (n:Item) RETURN n.seq", &opts).expect("open");
    cursor.next_batch(10).expect("batch");
    token.cancel();
    let mut outcome = Ok(Vec::new());
    for _ in 0..40 {
        outcome = cursor.next_batch(1000);
        if outcome.is_err() {
            break;
        }
    }
    assert!(matches!(outcome, Err(KgError::Cancelled)), "{outcome:?}");
    assert!(cursor.next_batch(10).expect("ended").is_empty());
}

#[test]
fn a_writer_publishing_meanwhile_does_not_change_the_cursor() {
    let g = graph(2000);
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    let mut cursor =
        execute_read_cursor(Arc::clone(&g), "MATCH (n:Item) RETURN n.seq", &opts).expect("open");
    let mut writer = (*g).clone();
    execute_mut(&mut writer, "CREATE (:Item {id: 99999, seq: 99999})", &opts).expect("write");
    assert_eq!(drain(&mut cursor, 500).len(), 2000);
}

#[test]
fn errors_surface_from_next_batch_or_open() {
    let g = graph(10);
    let params = HashMap::new();
    let opts = ExecuteOptions::eager(&params);
    assert!(execute_read_cursor(Arc::clone(&g), "MATCH (n RETURN n", &opts).is_err());
    assert!(execute_read_cursor(Arc::clone(&g), "CREATE (:X)", &opts).is_err());
    let mut lazy = ExecuteOptions::eager(&params);
    lazy.lazy_eligible = true;
    assert!(execute_read_cursor(g, "MATCH (n:Item) RETURN n.seq", &lazy).is_err());
}

#[test]
fn row_limit_falls_back_to_the_materialised_route() {
    let g = graph(100);
    let params = HashMap::new();
    let mut opts = ExecuteOptions::eager(&params);
    opts.row_limit = Some(7);
    let mut c = execute_read_cursor(g, "MATCH (n:Item) RETURN n.seq", &opts).expect("open");
    assert!(!c.streamed());
    assert_eq!(drain(&mut c, 3).len(), 7);
}
