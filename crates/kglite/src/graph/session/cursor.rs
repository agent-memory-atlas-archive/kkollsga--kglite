//! A read query as a cursor: the result is pulled in batches, and a streamable
//! query never holds more than a few batches of it.
//!
//! [`execute_read_cursor`] decides the memory class of a query:
//!
//! - **Streamed** (`Cursor::streamed() == true`): `MATCH <one pattern> [WHERE …]
//!   RETURN <plain expressions>`. The rows are independent, so a worker thread
//!   walks the matcher a slice of start nodes at a time and projects one batch
//!   at a time into a two-slot channel; the consumer's pace is the worker's
//!   pace, and memory is the channel plus the matcher's widest slice, not the
//!   result. See `executor/row_cursor.rs` for the exact shape.
//! - **Materialized**: anything that needs its whole input before its first
//!   row (ORDER BY, DISTINCT, aggregation, UNION, several clauses), a
//!   `row_limit` or codec'd columns, a disk graph, `max_work_units`. These run
//!   exactly like [`execute_read`]; the cursor only slices the finished rows.
//!
//! The cursor holds the `Arc<DirGraph>` snapshot it was opened on until it is
//! dropped, so a writer that publishes meanwhile forks rather than disturbing
//! it, and the snapshot's memory stays alive for as long as the cursor does.
//! Dropping the cursor stops the worker and releases the snapshot.
//!
//! Cancellation and deadlines are the query's own ([`ExecuteOptions::cancel`],
//! [`ExecuteOptions::deadline`]) and apply for the cursor's whole life,
//! including between `next_batch` calls. Not carried over from [`execute_read`]:
//! query diagnostics (use [`Cursor::warnings`] for the schema warnings) and the
//! integer-id coercion warnings.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use super::execute::{exec_err, prepare, read_statement, timeless_route, PreparedQuery};
use super::{CancelToken, ExecuteOptions, Session};
use crate::datatypes::Value;
use crate::error::KgError;
use crate::graph::dir_graph::DirGraph;
use crate::graph::languages::cypher;
use crate::graph::languages::cypher::executor::row_cursor::{row_cursor_shape, CursorEvent};
use crate::graph::storage::GraphRead;

/// Rows per batch the worker produces. The channel holds two, so a streamed
/// cursor keeps roughly this many rows times three alive at once.
const WORKER_BATCH_ROWS: usize = 1024;

enum Msg {
    Open {
        columns: Vec<String>,
        streamed: bool,
    },
    Rows(Vec<Vec<Value>>),
    Failed(KgError),
    Done,
}

struct Worker {
    rx: Receiver<Msg>,
    handle: Option<JoinHandle<()>>,
}

enum Source {
    /// Rows already in memory (the materialized class).
    Buffered,
    Worker(Worker),
}

/// A read result pulled in batches. See the module docs for the memory classes.
pub struct Cursor {
    columns: Vec<String>,
    streamed: bool,
    warnings: Arc<[String]>,
    source: Source,
    pending: VecDeque<Vec<Value>>,
    finished: bool,
    // Only tests read the count back; production code just shares the counter.
    #[cfg_attr(not(test), allow(dead_code))]
    produced: Arc<AtomicUsize>,
}

impl Cursor {
    #[cfg(test)]
    pub(crate) fn rows_produced(&self) -> usize {
        self.produced.load(Ordering::Relaxed)
    }

    /// Result column names, known before the first row.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Whether rows are produced as they are pulled (`true`) or the whole
    /// result was materialized before the first batch (`false`).
    pub fn streamed(&self) -> bool {
        self.streamed
    }

    /// Schema warnings the statement earned (unknown labels, absent properties).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Up to `n` further rows; an empty vector means the cursor is exhausted.
    /// A query failure, cancellation or timeout arrives here, once, and ends
    /// the cursor.
    // KgError carries query context; boxing it would only burden an error path.
    #[allow(clippy::result_large_err)]
    pub fn next_batch(&mut self, n: usize) -> Result<Vec<Vec<Value>>, KgError> {
        let n = n.max(1);
        while self.pending.len() < n && !self.finished {
            let Source::Worker(worker) = &mut self.source else {
                self.finished = true;
                break;
            };
            match worker.rx.recv() {
                Ok(Msg::Rows(rows)) => self.pending.extend(rows),
                Ok(Msg::Done) => self.finished = true,
                Ok(Msg::Failed(e)) => {
                    self.finished = true;
                    self.pending.clear();
                    return Err(e);
                }
                Ok(Msg::Open { .. }) | Err(_) => {
                    self.finished = true;
                    return Err(KgError::CypherExecution {
                        message: "the cursor's worker ended unexpectedly".to_string(),
                        position: None,
                    });
                }
            }
        }
        let take = n.min(self.pending.len());
        Ok(self.pending.drain(..take).collect())
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        if let Source::Worker(worker) = &mut self.source {
            // Closing the channel fails the worker's next send, which ends it
            // within one batch of work; joining makes "snapshot released on
            // drop" literally true.
            let (_, closed) = std::sync::mpsc::channel();
            drop(std::mem::replace(&mut worker.rx, closed));
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

/// Open a cursor over `query` on the snapshot `graph`. The snapshot is held
/// until the cursor drops. `opts.lazy_eligible` must be `false`.
// KgError carries query context; boxing it would only burden an error path.
#[allow(clippy::result_large_err)]
pub fn execute_read_cursor(
    graph: Arc<DirGraph>,
    query: &str,
    opts: &ExecuteOptions<'_>,
) -> Result<Cursor, KgError> {
    if opts.lazy_eligible {
        return Err(KgError::Argument(
            "execute_read_cursor needs lazy_eligible = false: a lazy result has no rows to batch"
                .to_string(),
        ));
    }
    let started = Instant::now();
    let (prepared, _echo) =
        timeless_route(&graph, query, prepare(&graph, query, opts, false)?, opts)?;
    let PreparedQuery {
        plan,
        params,
        encode_plan,
        warnings,
    } = prepared;
    let streamable = !plan.explain
        && !cypher::is_mutation_query(&plan)
        && opts.row_limit.is_none()
        && opts.max_work_units.is_none()
        && encode_plan.iter().all(Option::is_none)
        && !graph.graph.is_disk()
        && row_cursor_shape(&plan).is_some();
    if !streamable {
        let outcome = read_statement(&graph, query, opts)?;
        return Ok(Cursor {
            columns: outcome.result.columns,
            streamed: false,
            warnings,
            source: Source::Buffered,
            pending: outcome.result.rows.into(),
            finished: false,
            produced: Arc::new(AtomicUsize::new(0)),
        });
    }

    let (tx, rx) = sync_channel::<Msg>(2);
    let produced = Arc::new(AtomicUsize::new(0));
    let job = Job {
        graph,
        plan,
        params,
        deadline: opts.deadline,
        deadline_origin: opts.deadline_origin,
        cancel: opts.cancel.clone(),
        parallel: opts.parallel,
        started,
        produced: Arc::clone(&produced),
    };
    let handle = std::thread::Builder::new()
        .name("kglite-cursor".to_string())
        .stack_size(super::QUERY_THREAD_STACK_SIZE)
        .spawn(move || job.run(tx))
        .map_err(|e| KgError::CypherExecution {
            message: format!("could not start the cursor worker: {e}"),
            position: None,
        })?;
    let mut cursor = Cursor {
        columns: Vec::new(),
        streamed: true,
        warnings,
        source: Source::Worker(Worker {
            rx,
            handle: Some(handle),
        }),
        pending: VecDeque::new(),
        finished: false,
        produced,
    };
    // The first message is the columns, or the failure that ended the run
    // before a column was known (a parse-time or seeding error).
    let Source::Worker(worker) = &cursor.source else {
        unreachable!("just built as a worker source")
    };
    match worker.rx.recv() {
        Ok(Msg::Open { columns, streamed }) => {
            cursor.columns = columns;
            cursor.streamed = streamed;
            Ok(cursor)
        }
        Ok(Msg::Failed(e)) => Err(e),
        _ => Err(KgError::CypherExecution {
            message: "the cursor's worker ended unexpectedly".to_string(),
            position: None,
        }),
    }
}

struct Job {
    graph: Arc<DirGraph>,
    plan: Arc<cypher::ast::CypherQuery>,
    params: HashMap<String, Value>,
    deadline: Option<Instant>,
    deadline_origin: Option<Instant>,
    cancel: Option<CancelToken>,
    parallel: bool,
    started: Instant,
    produced: Arc<AtomicUsize>,
}

impl Job {
    fn run(self, tx: SyncSender<Msg>) {
        let Some(shape) = row_cursor_shape(&self.plan) else {
            let _ = tx.send(Msg::Failed(KgError::CypherExecution {
                message: "the cursor's query is not a row-cursor shape".to_string(),
                position: None,
            }));
            return;
        };
        let executor =
            cypher::CypherExecutor::with_params(&self.graph, &self.params, self.deadline)
                .with_streaming(true)
                .with_parallel(self.parallel)
                .with_cancel(self.cancel.as_ref().map(CancelToken::flag));
        let graph = &self.graph;
        let mut emit = |event: CursorEvent| -> bool {
            let msg = match event {
                CursorEvent::Open { columns, streamed } => Msg::Open { columns, streamed },
                CursorEvent::Rows(mut rows) => {
                    super::resolve_noderefs(&graph.graph, &mut rows);
                    self.produced.fetch_add(rows.len(), Ordering::Relaxed);
                    Msg::Rows(rows)
                }
            };
            tx.send(msg).is_ok()
        };
        let outcome = executor.run_row_cursor(&self.plan, &shape, WORKER_BATCH_ROWS, &mut emit);
        let msg = match outcome {
            Ok(()) => Msg::Done,
            Err(message) => {
                let empty = HashMap::new();
                let mut opts = ExecuteOptions::eager(&empty);
                opts.deadline = self.deadline;
                opts.deadline_origin = self.deadline_origin;
                opts.cancel = self.cancel.clone();
                Msg::Failed(exec_err(&opts, self.started, message))
            }
        };
        let _ = tx.send(msg);
    }
}

impl Session {
    /// Open a cursor over `query` on the session's current snapshot.
    // KgError carries query context; boxing it would only burden an error path.
    #[allow(clippy::result_large_err)]
    pub fn execute_read_cursor(
        &self,
        query: &str,
        opts: &ExecuteOptions<'_>,
    ) -> Result<Cursor, KgError> {
        execute_read_cursor(self.snapshot(), query, opts)
    }
}
