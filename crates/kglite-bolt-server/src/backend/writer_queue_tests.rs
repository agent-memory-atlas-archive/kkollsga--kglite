//! Writer admission at the backend: `--write-concurrency queue` against
//! `optimistic`, driven through the same `BoltBackend` calls boltr makes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use boltr::error::BoltError;
use boltr::server::{BoltBackend, SessionHandle, TransactionHandle};
use boltr::types::{BoltDict, BoltValue};
use kglite::api::session::CsvImportPolicy;
use kglite::api::storage::{new_dir_graph_in_mode, StorageMode};
use kglite::api::Value;

use super::*;

const OUTDATED: &str = "Neo.TransientError.Transaction.Outdated";

fn backend(mode: WriteConcurrency, wait_ms: u64, idle_ms: u64) -> Arc<KgliteBackend> {
    let graph = new_dir_graph_in_mode(StorageMode::Memory, None).expect("memory graph");
    Arc::new(
        KgliteBackend::new(
            kglite::api::session::Session::new(graph),
            std::env::temp_dir().join("writer-queue-unused.kgl"),
            false,
            "127.0.0.1:0".into(),
            CsvImportPolicy::Denied,
            ServerIdentity::default(),
            None,
        )
        .with_writer_config(WriterConfig {
            mode,
            wait_timeout: (wait_ms > 0).then(|| Duration::from_millis(wait_ms)),
            idle_timeout: (idle_ms > 0).then(|| Duration::from_millis(idle_ms)),
        }),
    )
}

fn session(n: usize) -> SessionHandle {
    SessionHandle(format!("s{n}"))
}

fn read_mode() -> BoltDict {
    BoltDict::from([("mode".to_string(), BoltValue::String("r".into()))])
}

fn run(b: &KgliteBackend, tx: &TransactionHandle, q: &str) -> Result<(), BoltError> {
    b.execute_in_tx(&tx.0, q, HashMap::new()).map(|_| ())
}

fn scalar(b: &KgliteBackend, q: &str) -> i64 {
    let snap = b.session.snapshot();
    let params = HashMap::new();
    let opts = kglite::api::session::ExecuteOptions::new(&params);
    let out = kglite::api::session::execute_read(&snap, q, &opts)
        .expect("read")
        .result;
    match out.rows.first().and_then(|r| r.first()) {
        Some(Value::Int64(n)) => *n,
        other => panic!("expected Int64, got {other:?}"),
    }
}

fn code_of(e: &BoltError) -> String {
    match e {
        BoltError::Query { code, .. } => code.clone(),
        other => format!("{other:?}"),
    }
}

/// One BEGIN/write/COMMIT. `Err` carries the failure's status code.
async fn write_once(b: &KgliteBackend, s: &SessionHandle, q: &str) -> Result<(), String> {
    let tx = b
        .begin_transaction(s, &BoltDict::new())
        .await
        .map_err(|e| code_of(&e))?;
    run(b, &tx, q).map_err(|e| code_of(&e))?;
    b.commit(s, &tx).await.map(|_| ()).map_err(|e| code_of(&e))
}

/// `writers` tasks x `rounds`, each round one transaction built by
/// `query(w, r)` and redone on conflict, as a driver would. Returns the
/// conflict count; any other failure panics.
async fn hammer(
    b: &Arc<KgliteBackend>,
    writers: usize,
    rounds: usize,
    query: impl Fn(usize, usize) -> String + Send + Sync + 'static,
) -> usize {
    let query = Arc::new(query);
    let mut tasks = Vec::new();
    for w in 0..writers {
        let (b, query) = (Arc::clone(b), Arc::clone(&query));
        tasks.push(tokio::spawn(async move {
            let s = session(w);
            let mut conflicts = 0;
            for r in 0..rounds {
                while let Err(code) = write_once(&b, &s, &query(w, r)).await {
                    assert_eq!(code, OUTDATED, "unexpected failure: {code}");
                    conflicts += 1;
                }
            }
            conflicts
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.expect("writer task");
    }
    total
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_serialises_overlapping_writers_without_conflicts() {
    let b = backend(WriteConcurrency::Queue, 0, 0);
    write_once(&b, &session(99), "CREATE (:Counter {id: 1, n: 0})")
        .await
        .expect("seed");
    let conflicts = hammer(&b, 4, 25, |_, _| {
        "MATCH (c:Counter {id: 1}) SET c.n = c.n + 1".to_string()
    })
    .await;
    assert_eq!(conflicts, 0, "queue mode must not conflict at COMMIT");
    assert_eq!(
        scalar(&b, "MATCH (c:Counter {id: 1}) RETURN c.n"),
        100,
        "every acknowledged increment must be applied"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_serialises_disjoint_writers_without_conflicts() {
    let b = backend(WriteConcurrency::Queue, 0, 0);
    let conflicts = hammer(&b, 4, 25, |w, r| {
        format!("CREATE (:Item {{id: {}}})", w * 1000 + r)
    })
    .await;
    assert_eq!(conflicts, 0);
    assert_eq!(scalar(&b, "MATCH (i:Item) RETURN count(i)"), 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn optimistic_mode_conflicts_even_on_disjoint_keys() {
    // The control that keeps the two tests above from being vacuous: with
    // admission off, two overlapping transactions on disjoint keys conflict.
    let b = backend(WriteConcurrency::Optimistic, 0, 0);
    let (sa, sb) = (session(1), session(2));
    let a = b.begin_transaction(&sa, &BoltDict::new()).await.unwrap();
    let c = b.begin_transaction(&sb, &BoltDict::new()).await.unwrap();
    run(&b, &a, "CREATE (:Item {id: 1})").unwrap();
    run(&b, &c, "CREATE (:Item {id: 2})").unwrap();
    b.commit(&sa, &a).await.unwrap();
    let err = b
        .commit(&sb, &c)
        .await
        .expect_err("stale snapshot must conflict");
    assert_eq!(code_of(&err), OUTDATED);
    assert_eq!(scalar(&b, "MATCH (i:Item) RETURN count(i)"), 1);
}

#[tokio::test]
async fn second_write_begin_waits_then_proceeds_after_commit() {
    let b = backend(WriteConcurrency::Queue, 0, 0);
    let sa = session(1);
    let a = b.begin_transaction(&sa, &BoltDict::new()).await.unwrap();
    let b2 = Arc::clone(&b);
    let waiter = tokio::spawn(async move {
        let tx = b2
            .begin_transaction(&session(2), &BoltDict::new())
            .await
            .unwrap();
        // It began after A's commit, so it sees A's write.
        run(&b2, &tx, "MATCH (i:Item {id: 1}) SET i.seen = 1").unwrap();
        b2.commit(&session(2), &tx).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !waiter.is_finished(),
        "the second writer must wait for the slot"
    );
    run(&b, &a, "CREATE (:Item {id: 1})").unwrap();
    b.commit(&sa, &a).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("waiter proceeds once the slot frees")
        .unwrap();
    assert_eq!(scalar(&b, "MATCH (i:Item {id: 1}) RETURN i.seen"), 1);
}

#[tokio::test]
async fn readers_never_wait_for_a_writer() {
    let b = backend(WriteConcurrency::Queue, 0, 0);
    let sw = session(1);
    let w = b.begin_transaction(&sw, &BoltDict::new()).await.unwrap();
    run(&b, &w, "CREATE (:Item {id: 1})").unwrap();

    let sr = session(2);
    let r = tokio::time::timeout(
        Duration::from_millis(500),
        b.begin_transaction(&sr, &read_mode()),
    )
    .await
    .expect("a read BEGIN must not wait for the writer slot")
    .unwrap();
    run(&b, &r, "MATCH (i:Item) RETURN count(i)").unwrap();
    b.commit(&sr, &r).await.unwrap();

    tokio::time::timeout(
        Duration::from_millis(500),
        b.execute(
            &sr,
            "MATCH (i:Item) RETURN count(i)",
            &HashMap::new(),
            &BoltDict::new(),
            None,
        ),
    )
    .await
    .expect("an auto-commit read must not wait")
    .unwrap();
    b.rollback(&sw, &w).await.unwrap();
}

#[tokio::test]
async fn wait_timeout_is_a_retriable_transient_failure() {
    let b = backend(WriteConcurrency::Queue, 200, 0);
    let held = b
        .begin_transaction(&session(1), &BoltDict::new())
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let err = b
        .begin_transaction(&session(2), &BoltDict::new())
        .await
        .expect_err("slot is held");
    let code = code_of(&err);
    assert_eq!(
        code,
        "Neo.TransientError.Transaction.LockAcquisitionTimeout"
    );
    assert!(
        code.starts_with("Neo.TransientError."),
        "must be driver-retriable"
    );
    assert!(started.elapsed() >= Duration::from_millis(200));
    // The failed BEGIN left no transaction behind, and the slot still works.
    b.rollback(&session(1), &held).await.unwrap();
    b.begin_transaction(&session(2), &BoltDict::new())
        .await
        .expect("slot is free");
}

#[tokio::test]
async fn an_idle_holder_is_rolled_back_when_a_writer_waits() {
    let b = backend(WriteConcurrency::Queue, 5_000, 200);
    let sa = session(1);
    let a = b.begin_transaction(&sa, &BoltDict::new()).await.unwrap();
    run(&b, &a, "CREATE (:Item {id: 1})").unwrap();

    // No waiter: an idle holder is left alone.
    tokio::time::sleep(Duration::from_millis(400)).await;
    run(&b, &a, "CREATE (:Item {id: 2})").expect("idle holder with no waiter is undisturbed");

    let sb = session(2);
    let started = std::time::Instant::now();
    let tx = tokio::time::timeout(
        Duration::from_secs(3),
        b.begin_transaction(&sb, &BoltDict::new()),
    )
    .await
    .expect("the waiter must reclaim the idle slot")
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150));
    b.commit(&sb, &tx).await.unwrap();

    // The holder's next messages say why, and nothing it wrote survives.
    let err = run(&b, &a, "CREATE (:Item {id: 3})").expect_err("reclaimed");
    assert_eq!(
        code_of(&err),
        "Neo.ClientError.Transaction.TransactionTimedOut"
    );
    let err = b.commit(&sa, &a).await.expect_err("reclaimed");
    assert_eq!(
        code_of(&err),
        "Neo.ClientError.Transaction.TransactionTimedOut"
    );
    b.rollback(&sa, &a)
        .await
        .expect("ROLLBACK of a reclaimed tx is idempotent");
    assert_eq!(scalar(&b, "MATCH (i:Item) RETURN count(i)"), 0);
}

#[tokio::test]
async fn a_running_query_is_never_reclaimed() {
    let b = backend(WriteConcurrency::Queue, 400, 100);
    let a = b
        .begin_transaction(&session(1), &BoltDict::new())
        .await
        .unwrap();
    // A query in flight for longer than the idle timeout.
    let guard = {
        let txs = b.transactions.lock().unwrap();
        let st = txs.get(&a.0).unwrap().lock().unwrap();
        st.writer.as_ref().unwrap().activity().begin_query()
    };
    let err = b
        .begin_transaction(&session(2), &BoltDict::new())
        .await
        .expect_err("holder is busy, waiter times out");
    assert_eq!(
        code_of(&err),
        "Neo.TransientError.Transaction.LockAcquisitionTimeout"
    );
    drop(guard);
    run(&b, &a, "CREATE (:Item {id: 1})").expect("holder survived");
}

#[tokio::test]
async fn closing_or_resetting_the_session_frees_the_slot() {
    for reset in [false, true] {
        let b = backend(WriteConcurrency::Queue, 300, 0);
        let sa = session(1);
        let a = b.begin_transaction(&sa, &BoltDict::new()).await.unwrap();
        run(&b, &a, "CREATE (:Item {id: 1})").unwrap();
        if reset {
            b.reset_session(&sa).await.unwrap();
        } else {
            b.close_session(&sa).await.unwrap();
        }
        let tx = tokio::time::timeout(
            Duration::from_millis(200),
            b.begin_transaction(&session(2), &BoltDict::new()),
        )
        .await
        .expect("slot must be free immediately")
        .unwrap();
        b.commit(&session(2), &tx).await.unwrap();
        assert_eq!(scalar(&b, "MATCH (i:Item) RETURN count(i)"), 0);
    }
}

#[tokio::test]
async fn the_slot_is_held_until_the_transaction_ends() {
    // A COMMIT refused for the wrong session leaves the transaction (and the
    // slot) in place; finishing it releases the slot.
    let b = backend(WriteConcurrency::Queue, 300, 0);
    let a = b
        .begin_transaction(&session(1), &BoltDict::new())
        .await
        .unwrap();
    assert!(b.commit(&session(7), &a).await.is_err());
    assert!(b
        .begin_transaction(&session(2), &BoltDict::new())
        .await
        .is_err());
    b.commit(&session(1), &a).await.unwrap();
    b.begin_transaction(&session(2), &BoltDict::new())
        .await
        .expect("slot free");
}

#[tokio::test]
async fn read_mode_transactions_cannot_write_in_queue_mode_only() {
    let q = backend(WriteConcurrency::Queue, 0, 0);
    let r = q
        .begin_transaction(&session(1), &read_mode())
        .await
        .unwrap();
    let err = run(&q, &r, "CREATE (:Item {id: 1})").expect_err("refused");
    assert_eq!(code_of(&err), "Neo.ClientError.Statement.AccessMode");

    let o = backend(WriteConcurrency::Optimistic, 0, 0);
    let r = o
        .begin_transaction(&session(1), &read_mode())
        .await
        .unwrap();
    run(&o, &r, "CREATE (:Item {id: 1})").expect("optimistic mode keeps today's behaviour");
}
