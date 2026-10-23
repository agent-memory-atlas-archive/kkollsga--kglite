//! `KnowledgeGraph.save` — the body, callable while the caller already holds
//! the graph's borrow (`close()`, `__exit__`, the loaders' durable checkpoint).

use super::kg_core::file_io_err;
use crate::graph::write_gate::lifecycle_graph_mut;
use crate::graph::KnowledgeGraph;
use kglite_core::api::io;
use pyo3::prelude::*;

/// Serialise the graph to disk, atomically and durably.
pub(crate) fn save_graph(
    this: &mut KnowledgeGraph,
    py: Python<'_>,
    path: Option<&str>,
    fsync: bool,
    layout: &str,
) -> PyResult<()> {
    let layout = io::SaveLayout::from_name(layout).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
            "layout must be 'auto' or 'compact', got '{layout}'"
        ))
    })?;
    // A graph whose log refused an append holds a statement the caller was
    // told had failed. `save()` is the one call that would commit it — it
    // serializes whatever is in memory, and cannot tell the acknowledged
    // writes from the one that was not. Refuse before anything is touched.
    this.check_wal_not_diverged()?;
    // A handle derived from a durable graph inherits `source_path` but not
    // the log, so a bare `save()` here would write *its* fork over the
    // durable file without stamping the checkpoint LSN or truncating the
    // log — leaving frames the owner still holds to replay over a
    // checkpoint that never contained them. Refuse the whole call rather
    // than only the same-path case: a derived handle serializes the entire
    // shared graph anyway, so `copy()`/`to_subgraph()` is the honest route
    // to a separate file and the message names it.
    this.check_durable_owner()?;
    // Durable (WAL) graphs: `save()` is the checkpoint that TRUNCATES
    // the fsync'd WAL below. Honouring `fsync=False` here would pair a
    // maybe-not-on-disk checkpoint with a destroyed log — a crash then
    // loses both. Force the flush and tell the caller (documented
    // contract in the `save()` docstring).
    let fsync = if this.lifecycle.durable.is_some() && !fsync {
        let msg = "save(fsync=False) is ignored for durable graphs: the checkpoint \
                   truncates the write-ahead log, so it must itself be flushed to disk \
                   (fsync forced to True) or a crash would lose both the checkpoint and \
                   the log.";
        let cmsg = std::ffi::CString::new(msg).unwrap_or_default();
        let _ = PyErr::warn(
            py,
            py.get_type::<pyo3::exceptions::PyUserWarning>().as_any(),
            cmsg.as_c_str(),
            1,
        );
        true
    } else {
        fsync
    };
    // Resolve the target: explicit path wins; otherwise fall back to the
    // origin file this graph was opened from (`kglite.open`/`load`). A
    // graph built in memory with no origin and no explicit path has
    // nowhere to go — guide the user rather than panicking.
    let effective: String = match path {
        Some(p) => p.to_string(),
        None => this
            .lifecycle
            .source_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .ok_or_else(|| {
                PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    "save() needs a path: this graph was not opened from a file. \
                     Pass an explicit path (g.save('graph.kgl')) or open it with \
                     kglite.open('graph.kgl') so save() remembers the target.",
                )
            })?,
    };
    // A contended save-as target keeps its typed refusal here; the
    // `SaveError` channel below carries only a message.
    let mut lease_contention: Option<crate::error::KgError> = None;
    let mut checkpoint_warning: Option<String> = None;
    let saved = py.detach(|| -> Result<(), io::SaveError> {
        use kglite_core::api::durable::{self, DurabilityLevel};
        use std::path::Path;
        // A background checkpoint still writing the stamped file must land
        // (and have its log trim applied) before this save touches either:
        // its older stamp would otherwise be renamed over this checkpoint.
        checkpoint_warning = this
            .lifecycle
            .durable
            .as_mut()
            .and_then(|state| state.settle_checkpoint(true));
        let io_error = |error: std::io::Error| io::SaveError::Io(error.to_string());
        let same_target = this
            .lifecycle
            .source_path
            .as_ref()
            .map_or(Ok(false), |source| {
                io::same_checkpoint_path(source, Path::new(&effective)).map_err(io_error)
            })?;
        // Keep the original sidecar spelling for equivalent publication paths.
        let target = if same_target {
            this.lifecycle.source_path.as_ref().unwrap().clone()
        } else {
            effective.into()
        };
        // A process that holds the target's lease owns it; any other holder
        // refuses the save before the write-ahead-log guard below, which
        // would otherwise answer a foreign holder's sidecar with a recovery
        // instruction. `open(..., lock=False)` opted out of lease checks.
        if !this.lifecycle.lease_opt_out {
            if let Err(refusal) = io::refuse_foreign_writer(&target) {
                let error = crate::error::KgError::from(refusal);
                let message = error.to_string();
                if matches!(error, crate::error::KgError::WriterLeaseHeld { .. }) {
                    lease_contention = Some(error);
                }
                return Err(io::SaveError::Io(message));
            }
        }
        let target_lease = if !same_target && this.lifecycle.writer_lease.is_some() {
            match io::GraphWriterLease::acquire_ex(&target, std::time::Duration::ZERO) {
                Ok(lease) => Some(lease),
                Err(refusal) => {
                    let error = crate::error::KgError::from(refusal);
                    let message = error.to_string();
                    if matches!(error, crate::error::KgError::WriterLeaseHeld { .. }) {
                        lease_contention = Some(error);
                    }
                    return Err(io::SaveError::Io(message));
                }
            }
        } else {
            None
        };
        let level = this
            .lifecycle
            .durable
            .as_ref()
            .map_or(DurabilityLevel::Off, |state| state.level);
        let target_wal = if same_target {
            None
        } else {
            durable::prepare_save_as_target(&target, level)?
        };
        let (graph_arc, lifecycle) = this.lifecycle_parts();
        let previous_stamp = graph_arc.checkpoint_lsn;
        if let Some(state) = lifecycle.durable.as_mut() {
            let graph = lifecycle_graph_mut(graph_arc);
            graph.bump_version();
            durable::checkpoint_prologue(&mut state.wal, state.next_lsn, graph)
                .map_err(io_error)?;
        }
        if let Err(error) =
            io::save_graph_with_layout(graph_arc, &target.to_string_lossy(), fsync, layout)
        {
            lifecycle_graph_mut(graph_arc).checkpoint_lsn = previous_stamp;
            return Err(error);
        }
        if same_target {
            if let Some(state) = lifecycle.durable.as_mut() {
                durable::checkpoint_epilogue(&mut state.wal, lifecycle_graph_mut(graph_arc))
                    .map_err(io_error)?;
            }
        } else {
            // The original WAL still describes its original checkpoint. Truncating
            // it here would destroy source recovery. The new WAL was emptied before
            // publication, so the handoff has no fallible work after the save.
            if let Some(state) = lifecycle.durable.as_mut() {
                state.wal = target_wal.expect("logging destination prepared above");
                kglite_core::api::cdc::drain_at_commit(lifecycle_graph_mut(graph_arc));
            }
            lifecycle.writer_lease = target_lease;
            lifecycle.source_path = Some(target);
        }
        Ok(())
    });
    if let Some(text) = checkpoint_warning {
        crate::graph::bg_checkpoint::warn_user(text);
    }
    saved.map_err(|error| match (error, lease_contention.take()) {
        (_, Some(contended)) => crate::error_py::kg_to_pyerr(contended),
        (io::SaveError::Refused(message), None) => {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(message)
        }
        (io::SaveError::Io(message), None) => file_io_err(std::io::Error::other(message)),
    })?;
    crate::mapped_fallback::announce(py, this.graph());
    crate::allocator::release_after_seam(py);
    Ok(())
}
