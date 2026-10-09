//! Automatic checkpoints that run off the committing thread.
//!
//! The commit that pushes the log past `auto_checkpoint_wal_mib` fixes a
//! snapshot point (an `Arc` clone of the graph, the newest logged LSN, the log
//! offset and epoch) and hands the expensive part, writing the stamped `.kgl`,
//! to one short-lived thread. The committing call returns at once. The log is
//! owned by the committing thread, so the second half, trimming the frames the
//! checkpoint now holds, happens there too: at the first commit after the write
//! finished, or at the `save()` that waits for it. Between the rename and the
//! trim the new checkpoint carries `checkpoint_lsn = L` and the log still holds
//! every frame, which replay already resolves by skipping frames at or below
//! `L`; a crash anywhere in the sequence loses nothing.
//!
//! **One in flight.** Triggers that arrive while a checkpoint runs coalesce
//! into it. `save()` (and so `close()` / `__exit__`) joins the thread before it
//! touches the log or the checkpoint file, because the older stamped file
//! renamed over a newer `save()` would roll the checkpoint back. Dropping the
//! graph joins it for the same reason (a reopened handle must not race a
//! rename from the handle it replaced).
//!
//! **Errors.** The commit that triggered the checkpoint was already durable in
//! the log, so a failed background checkpoint is a `UserWarning` raised from the
//! next call that settles it, and the policy backs off by one bound of log
//! growth; it is never silent.

use super::{DurableState, KnowledgeGraph};
use kglite_core::api::durable::{self, DurabilityLevel, OnlineCheckpointPoint};
use pyo3::prelude::*;
use std::sync::Arc;
use std::thread::JoinHandle;

type Finished = (OnlineCheckpointPoint, Result<u64, String>);

/// A checkpoint write running on its own thread.
pub(crate) struct InFlightCheckpoint {
    handle: Option<JoinHandle<Finished>>,
}

impl InFlightCheckpoint {
    fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn join(&mut self) -> Result<Finished, String> {
        match self.handle.take() {
            Some(handle) => handle
                .join()
                .map_err(|_| "the checkpoint thread panicked".to_string()),
            None => Err("the checkpoint thread was already joined".to_string()),
        }
    }
}

impl Drop for InFlightCheckpoint {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

impl DurableState {
    /// Fold a finished (or, with `wait`, a still-running) background checkpoint
    /// into the log: trim on success, back off on failure. Returns the warning
    /// text for a failure; `None` when nothing was in flight or it is still
    /// running and `wait` is false.
    pub(crate) fn settle_checkpoint(&mut self, wait: bool) -> Option<String> {
        let running = self.inflight.as_ref()?;
        if !wait && !running.is_finished() {
            return None;
        }
        let mut running = self.inflight.take()?;
        let limit = self.auto_checkpoint_wal_bytes.unwrap_or(0);
        let backoff = |ds: &mut DurableState| {
            ds.checkpoint_floor = None;
            ds.retry_after_wal_bytes = ds.wal.frame_bytes().saturating_add(limit);
        };
        match running.join() {
            Ok((point, Ok(bytes))) => {
                match durable::finish_online_checkpoint(&mut self.wal, &point) {
                    Ok(()) => {
                        self.checkpoint_floor = Some(bytes);
                        self.retry_after_wal_bytes = 0;
                        None
                    }
                    Err(error) => {
                        backoff(self);
                        Some(failure_text(&format!("trimming the log failed: {error}")))
                    }
                }
            }
            Ok((_, Err(message))) => {
                backoff(self);
                Some(failure_text(&message))
            }
            Err(message) => {
                backoff(self);
                Some(failure_text(&message))
            }
        }
    }
}

fn failure_text(message: &str) -> String {
    format!(
        "automatic checkpoint of the write-ahead log failed ({message}); the commit is in \
         the log, which keeps growing, and the next attempt waits for one more \
         auto_checkpoint_wal_mib of growth"
    )
}

/// Raise `text` as a `UserWarning` at the caller's frame.
pub(crate) fn warn_user(text: String) {
    Python::attach(|py| {
        let cmsg = std::ffi::CString::new(text).unwrap_or_default();
        let _ = PyErr::warn(
            py,
            py.get_type::<pyo3::exceptions::PyUserWarning>().as_any(),
            cmsg.as_c_str(),
            1,
        );
    });
}

impl KnowledgeGraph {
    /// Start a background checkpoint when the log has outgrown
    /// `auto_checkpoint_wal_mib` (and is at least as large as the file it
    /// extends), after folding any finished one into the log. Never waits.
    pub(super) fn auto_checkpoint_if_needed(&mut self) {
        let Some(ds) = self.lifecycle.durable.as_mut() else {
            return;
        };
        if let Some(text) = ds.settle_checkpoint(false) {
            warn_user(text);
        }
        let Some(limit) = ds.auto_checkpoint_wal_bytes else {
            return;
        };
        let size = ds.wal.frame_bytes();
        if ds.inflight.is_some() || ds.diverged || size < limit || size < ds.retry_after_wal_bytes {
            return;
        }
        let Some(source) = self.lifecycle.source_path.clone() else {
            return;
        };
        let floor = *ds
            .checkpoint_floor
            .get_or_insert_with(|| std::fs::metadata(&source).map_or(0, |m| m.len()));
        if size < floor {
            return;
        }
        let barrier = ds.level != DurabilityLevel::Full;
        let point = match durable::begin_online_checkpoint(&ds.wal, ds.next_lsn, &source, barrier) {
            Ok(point) => point,
            Err(error) => {
                ds.retry_after_wal_bytes = size.saturating_add(limit);
                warn_user(failure_text(&error.to_string()));
                return;
            }
        };
        let snapshot = Arc::clone(&self.inner);
        let spawned = std::thread::Builder::new()
            .name("kglite-checkpoint".to_string())
            .spawn(move || {
                let written = durable::write_online_checkpoint(&snapshot, &point);
                drop(snapshot);
                (point, written)
            });
        match spawned {
            Ok(handle) => {
                ds.inflight = Some(InFlightCheckpoint {
                    handle: Some(handle),
                });
            }
            Err(error) => {
                ds.retry_after_wal_bytes = size.saturating_add(limit);
                warn_user(failure_text(&format!(
                    "could not start the thread: {error}"
                )));
            }
        }
    }
}
