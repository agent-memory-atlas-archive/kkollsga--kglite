//! The write choke points of the Python `KnowledgeGraph`.
//!
//! Every mutation of a graph's content or metadata goes through one of the
//! methods here: [`logged_write`](KnowledgeGraph::logged_write),
//! [`logged_publish`](KnowledgeGraph::logged_publish) or
//! [`unlogged_write`](KnowledgeGraph::unlogged_write). The raw
//! `&mut DirGraph` accessor and the log-drain (`commit_wal`, `flush_wal`) are
//! private to this module, so a mutator written without going through a choke
//! point does not compile; `tests/test_write_choke_points.py` guards the one
//! bypass the type system cannot see (`Arc::make_mut(&mut self.inner)`).
//!
//! The bodies below are the in-binding engine: owner check, mutate, drain the
//! capture buffer into the write-ahead log. They are the only thing a change of
//! engine has to replace.

use super::{embedder, wal_diverged_io_error, CursorState, GraphLifecycle, WAL_DIVERGED_MSG};
// `Arc::make_mut` hands back a mutable reference in place when this is the only
// reference; with another (a retained `ResultView`, transaction snapshot or
// cloned `KnowledgeGraph`) the clone mechanism depends on storage. See
// `kglite_core::api::make_dir_graph_mut` for the full contract.
use kglite_core::api::make_dir_graph_mut as raw_graph_mut;
use kglite_core::api::DirGraph;
use pyo3::prelude::*;
use std::sync::Arc;

/// The checkpoint-stamp and capture-drain steps of `save()` write to the graph
/// while holding disjoint borrows of the lifecycle state, which a `&mut self`
/// method cannot express. Lifecycle only: every content or metadata mutation
/// goes through the methods below, and the scan test allows this name in
/// `kg_core.rs` alone.
///
/// Does not bump the version, unlike the other accessors here: a `save()` that
/// fails, or restores the stamp it replaced, changed no data and must not
/// invalidate an open transaction. The checkpoint prologue, which the durable
/// path counts as advancing the data version, bumps it explicitly.
pub(crate) fn lifecycle_graph_mut(inner: &mut Arc<DirGraph>) -> &mut DirGraph {
    Arc::make_mut(inner)
}

/// Main knowledge graph type exposed to Python via PyO3.
///
/// Wraps a `DirGraph` behind an `Arc` for cheap cloning (read-heavy workloads).
/// All read methods take `&self`; mutations use `Arc::make_mut` for copy-on-write.
/// Supports Cypher queries, property filtering, traversals, graph algorithms,
/// and code entity exploration methods (`find`, `source`, `context`, `toc`).
#[pyclass(module = "kglite")]
pub struct KnowledgeGraph {
    /// The published graph. Private to this module: reads go through
    /// [`graph`](Self::graph), writes through the choke points below, and the
    /// `Arc` is never cloned into a long-lived field, so a single-owner commit
    /// can still fork-free mutate it in place.
    inner: Arc<DirGraph>,
    /// Per-caller fluent cursor state — see [`CursorState`].
    pub(crate) cursor: CursorState,
    /// Registered embedding model (not serialized — re-set after load).
    /// Backend-agnostic via [`embedder::Embedder`] trait: Python
    /// embedders flow through [`embedder::py_adapter::PyEmbedderAdapter`];
    /// Rust-native embedders (e.g. fastembed-rs) implement the trait
    /// directly. Held as a trait object rather than `Option<Py<PyAny>>`
    /// so downstream Rust binaries (kglite-mcp-server) don't inherit a
    /// libpython dep transitively.
    pub(crate) embedder: Option<Arc<dyn embedder::Embedder>>,
    /// Default per-query timeout in milliseconds. Applied to cypher() when
    /// timeout_ms is not explicitly passed. None uses the Python 180s default.
    pub(crate) default_timeout_ms: Option<u64>,
    /// Default per-query work budget — intermediate rows, retained collection
    /// items and scan work, not a result-row cap. Applied to cypher() when
    /// max_work_units is not explicitly passed; a query that exceeds it errors
    /// rather than truncating. None = no explicit budget (default), leaving
    /// the engine's own backstop in charge.
    pub(crate) default_max_work_units: Option<usize>,
    /// Default cap on the result rows a query **retains**. Applied to cypher()
    /// when `row_limit` is not explicitly passed. The opposite number to
    /// `default_max_work_units`: that one fails a query that does too much
    /// work, this one truncates a result that is too big and says so.
    /// None = retain everything (default).
    pub(crate) default_row_limit: Option<usize>,
    /// Graph lifecycle / identity — save target + durability session. See
    /// [`GraphLifecycle`].
    pub(crate) lifecycle: GraphLifecycle,
}

impl KnowledgeGraph {
    /// Assemble a handle from its parts; the one constructor, so `inner` can
    /// stay private to this module.
    pub(crate) fn from_parts(
        graph: Arc<DirGraph>,
        cursor: CursorState,
        embedder: Option<Arc<dyn embedder::Embedder>>,
        lifecycle: GraphLifecycle,
    ) -> Self {
        KnowledgeGraph {
            inner: graph,
            cursor,
            embedder,
            default_timeout_ms: None,
            default_max_work_units: None,
            default_row_limit: None,
            lifecycle,
        }
    }

    /// The current graph, borrowed for the duration of one call. Returns a
    /// reference rather than a clone so nothing outlives the call and keeps
    /// the `Arc` shared; a method that must hold the graph past a GIL release
    /// takes `Arc::clone(self.graph())` locally.
    #[inline]
    pub(crate) fn graph(&self) -> &Arc<DirGraph> {
        &self.inner
    }

    /// Replace the published graph wholesale (ownership hand-off, orphaning);
    /// not a content mutation, so it neither logs nor bumps the version.
    pub(crate) fn replace_graph(&mut self, graph: Arc<DirGraph>) {
        self.inner = graph;
    }

    /// Split borrow for the lifecycle code that opens the log or checkpoints
    /// while holding the graph and the lifecycle state together. The scan test
    /// allows its callers in `lib.rs` and `kg_core.rs` only.
    pub(crate) fn lifecycle_parts(&mut self) -> (&mut Arc<DirGraph>, &mut GraphLifecycle) {
        (&mut self.inner, &mut self.lifecycle)
    }

    /// A mutation the write-ahead log describes: refuse a derived handle,
    /// mutate through `f`, then make the captured ops durable before returning.
    ///
    /// **Every method that commits a change to graph content goes through
    /// here.** A mutator that bypassed the commit would leave its ops buffered,
    /// invisible to the log and discarded by the next `save()` — silently not
    /// crash-safe — which is why no other path to a mutable `DirGraph` exists.
    /// An `Err` from `f` skips the commit, exactly as an early `?` did before:
    /// ops the failed call buffered ride into the next frame.
    pub(crate) fn logged_write<T>(
        &mut self,
        f: impl FnOnce(&mut DirGraph) -> PyResult<T>,
    ) -> PyResult<T> {
        self.logged_write_with_cursor(|g, _| f(g))
    }

    /// [`logged_write`](Self::logged_write) for a mutation that must also
    /// settle the cursor (reset or remap the selection) before the commit, so
    /// a failed commit still leaves a selection that matches the graph.
    pub(crate) fn logged_write_with_cursor<T>(
        &mut self,
        f: impl FnOnce(&mut DirGraph, &mut CursorState) -> PyResult<T>,
    ) -> PyResult<T> {
        self.check_durable_owner()?;
        // A graph at its unsaved-write budget refuses the call before it
        // starts; a call may overshoot by its own size (the Cypher route rolls
        // its statement back instead, see `dir_graph::write_budget`).
        self.inner
            .check_write_budget()
            .map_err(crate::error_py::kg_to_pyerr)?;
        let out = f(raw_graph_mut(&mut self.inner), &mut self.cursor)?;
        self.commit_wal()?;
        let _ = self.inner.settle_write_budget(false);
        Python::attach(|py| crate::write_budget::announce(py, &self.inner));
        Ok(out)
    }

    /// A renumbering mutation (vacuum): the pending ops are logged first,
    /// because captured ops are keyed by `NodeIndex` and `f` remaps every
    /// index, so ops resolved afterwards would describe the wrong nodes. `f`
    /// itself changes no logical content and is not logged.
    pub(crate) fn renumbering_write<T>(
        &mut self,
        f: impl FnOnce(&mut DirGraph, &mut CursorState) -> T,
    ) -> PyResult<T> {
        self.commit_wal()?;
        Ok(f(raw_graph_mut(&mut self.inner), &mut self.cursor))
    }

    /// [`logged_write`](Self::logged_write) for a callee that takes the
    /// `Arc` itself and forks it only once it has something to store — the
    /// embedding pass, which runs the model for as long as it takes and must
    /// not copy the graph before it knows the result.
    pub(crate) fn logged_write_lazy<T>(
        &mut self,
        f: impl FnOnce(&mut Arc<DirGraph>) -> PyResult<T>,
    ) -> PyResult<T> {
        self.check_durable_owner()?;
        self.inner
            .check_write_budget()
            .map_err(crate::error_py::kg_to_pyerr)?;
        let out = f(&mut self.inner)?;
        self.commit_wal()?;
        let _ = self.inner.settle_write_budget(false);
        Python::attach(|py| crate::write_budget::announce(py, &self.inner));
        Ok(out)
    }

    /// Replace the published graph with a finished working copy and log it as
    /// one frame — the transaction commit. Refuses with `TransactionConflict`
    /// when the published graph moved since `base_version`, before anything
    /// changes.
    pub(crate) fn logged_publish(
        &mut self,
        mut working: DirGraph,
        base_version: u64,
    ) -> PyResult<()> {
        self.check_durable_owner()?;
        let current_version = self.inner.version;
        if current_version != base_version {
            return Err(crate::error_py::kg_to_pyerr(
                crate::error::KgError::TransactionConflict {
                    base_version,
                    current_version,
                },
            ));
        }
        working.set_version(current_version + 1);
        self.inner = Arc::new(working);
        self.cursor.selection = kglite_core::api::CowSelection::new();
        self.commit_wal()?;
        Python::attach(|py| crate::write_budget::announce(py, &self.inner));
        Ok(())
    }

    /// Log whatever the capture layer still holds, without mutating anything:
    /// the lifecycle calls (`backup`, `sync`) that must not read an LSN or
    /// barrier the log while ops are buffered.
    pub(crate) fn flush_pending_ops(&mut self) -> PyResult<()> {
        self.commit_wal()
    }

    /// A mutation of state the log does not describe: schema and config
    /// metadata that `save()` persists, derived caches a load rebuilds, a
    /// vacuum that renumbers without changing logical content. No owner check
    /// and no commit, matching how these behaved before the choke points.
    pub(crate) fn unlogged_write<T>(&mut self, f: impl FnOnce(&mut DirGraph) -> T) -> T {
        f(raw_graph_mut(&mut self.inner))
    }

    /// [`unlogged_write`](Self::unlogged_write) for a mutation that adds heap
    /// the write budget meters (the quantized copy of an embedding store):
    /// refused before it starts when the graph is at its limit, and settled
    /// after it.
    pub(crate) fn unlogged_metered_write<T>(
        &mut self,
        f: impl FnOnce(&mut DirGraph) -> PyResult<T>,
    ) -> PyResult<T> {
        self.inner
            .check_write_budget()
            .map_err(crate::error_py::kg_to_pyerr)?;
        let out = f(raw_graph_mut(&mut self.inner))?;
        let _ = self.inner.settle_write_budget(false);
        Python::attach(|py| crate::write_budget::announce(py, &self.inner));
        Ok(out)
    }

    /// [`unlogged_write`](Self::unlogged_write) for a callee that takes the
    /// `Arc` itself (the disk materialisation, which swaps the backend and
    /// forks only when it must).
    pub(crate) fn unlogged_write_lazy<T>(&mut self, f: impl FnOnce(&mut Arc<DirGraph>) -> T) -> T {
        f(&mut self.inner)
    }

    /// Housekeeping after a Cypher write's frame is durable: auto-vacuum, then
    /// the selection remap that follows the compaction.
    ///
    /// **The order against the commit is a correctness requirement.** Captured
    /// WAL ops are keyed by `NodeIndex` and resolved against current graph
    /// state, while a vacuum rebuilds the graph with contiguous indices — ops
    /// resolved after a remap describe the wrong nodes, or none at all. The
    /// commit therefore has to come first.
    pub(crate) fn after_logged_cypher(
        &mut self,
        stats: Option<&kglite_core::api::cypher::MutationStats>,
    ) {
        let Some(stats) = stats else { return };
        if stats.nodes_deleted > 0 || stats.relationships_deleted > 0 {
            // Follow the compaction, do not throw the selection away. A reset
            // `CurrentSelection` reads as "no filter applied", so the same held
            // handle would answer `ids()` with `[]` and `len()` with the whole
            // graph. `remap_indices` is a no-op when the mapping describes no
            // rebuild, which is what the disk backend returns.
            if let Some(remap) = self.unlogged_write(|g| g.check_auto_vacuum()) {
                self.cursor.selection.remap_indices(&remap);
            }
        }
        self.cursor.last_mutation_stats = Some(stats.clone());
    }

    /// [`flush_wal`](Self::flush_wal) mapped into `PyResult`; private, so the
    /// only callers are [`logged_write`](Self::logged_write) and its siblings.
    ///
    /// Not every mutation is logged. Ops exist for nodes, edges,
    /// labels, the declarations captured at their own choke points — identity-
    /// field spellings, parent types, the ontology, the schema stamp, spatial
    /// configs, validity-interval declarations, user indexes and constraints —
    /// and the bulk payloads:
    /// timeseries channels and embedding stores with their vector-index
    /// declaration. What is left checkpoint-only is derived state a load
    /// rebuilds anyway (text indexes, cached norms), for which calling this is
    /// a no-op.
    #[inline]
    fn commit_wal(&mut self) -> PyResult<()> {
        // Mutation entries reject missing capture ownership before changing data.
        // Keep the commit guard as well: no caller may drain a source stream
        // or append a source WAL for an independently mutated derived handle.
        self.check_durable_owner()?;
        // The fsync inside `flush_wal` is the slow part of a durable write;
        // other Python threads run while it waits on the disk.
        let flushed = if self.lifecycle.durable.is_some() {
            Python::attach(|py| py.detach(|| self.flush_wal()))
        } else {
            self.flush_wal()
        };
        flushed.map_err(|e| crate::error_py::durability_failed_pyerr(e.to_string()))?;
        self.auto_checkpoint_if_needed();
        Ok(())
    }

    /// Drain the capture buffer at this commit boundary: publish its changes
    /// to the change stream, and — for a durable graph — resolve them to
    /// logical ops and append a durably-`fsync`'d WAL frame. No-op for a graph
    /// that is neither durable nor capturing, and when no ops are pending. The
    /// `fsync` inside makes a committed mutation on a durable graph crash-safe
    /// before control returns to the caller.
    ///
    /// **Two owners want this drain, and either alone is reason to run it.**
    /// The write-ahead log is the older one; change data capture is the other,
    /// and a CDC-enabled *non-durable* in-memory graph is the modal CDC
    /// deployment, so gating the drain on durability would have left the
    /// stream permanently empty for it. The publish itself lives in
    /// `api::cdc::drain_at_commit` — the wheel only says *where* its commits
    /// are, which is the one thing core cannot know.
    fn flush_wal(&mut self) -> std::io::Result<()> {
        let durable = self.lifecycle.durable.is_some();
        if !durable && !self.inner.cdc_enabled() {
            return Ok(());
        }
        // Refuse *before* the drain, not after: draining publishes this
        // commit to the change stream and empties the capture buffer, and on a
        // graph whose log has already diverged neither can be undone — the
        // events would describe changes no checkpoint will ever hold. Leaving
        // the buffer intact costs nothing (this handle can no longer save, so
        // it is on its way to being dropped) and keeps the refusal total.
        if self
            .lifecycle
            .durable
            .as_ref()
            .is_some_and(|ds| ds.diverged)
        {
            return Err(wal_diverged_io_error());
        }
        let dir = raw_graph_mut(&mut self.inner);
        // Publishes this commit's CDC events and hands the raw ops back;
        // yields nothing for a graph carrying no capture layer (durable
        // state without a recording backend shouldn't happen, but reads
        // here as nothing to flush).
        let raw = kglite_core::api::cdc::drain_at_commit(dir);
        if raw.is_empty() || !durable {
            return Ok(());
        }
        let ds = self
            .lifecycle
            .durable
            .as_mut()
            .expect("durable checked Some above; not cleared in between");
        let lsn = ds.next_lsn;
        let appended = if ds.fail_append {
            Err(std::io::Error::other(
                "injected write-ahead log append failure (kglite._fail_wal_append)",
            ))
        } else {
            // The whole `DirGraph`, not its backend: secondary labels and the
            // relationship embedding stores both live above the backend, and a
            // frame resolved without them logs vector state as absent — see
            // `resolve_ops`.
            ds.wal.append_resolved(lsn, &raw, dir)
        };
        match appended {
            Ok(()) => {
                // Only a frame that reached the log consumes its LSN — the same
                // rule `Session::log_working_commit` follows. Consuming it up
                // front would leave a hole the checkpoint stamp counts as a
                // committed frame, so `checkpoint_lsn = next_lsn - 1` would
                // claim to have folded in a commit that never existed and the
                // replay gate would silently skip the *next* real one.
                ds.next_lsn = lsn + 1;
                Ok(())
            }
            Err(e) => {
                // The mutation is already applied to `self.inner` and its ops
                // are drained, so this handle now holds state the log does not
                // describe and cannot be made to describe. Latch, so the write
                // the caller was told had FAILED can never be committed by a
                // later save() (which would neither know about it nor be able
                // to separate it from the writes that succeeded).
                ds.diverged = true;
                // Carry the latch into the *first* error too. This is the one
                // the caller actually sees at ENOSPC, and a bare "No space left
                // on device" says nothing about the graph now being unsaveable
                // — which is the part that decides what they do next.
                Err(std::io::Error::new(
                    e.kind(),
                    format!("write-ahead log append failed: {e}. {WAL_DIVERGED_MSG}"),
                ))
            }
        }
    }
}
