//! Auto-commit write and checkpoint-if-changed: the two `Session` verbs every
//! serving binding writes identically on top of [`Session::begin`] /
//! [`Session::commit`] / [`Session::save`].

use super::execute::{execute_mut, ExecuteOptions, ExecuteOutcome};
use super::transaction::{CommitOutcome, Session};
use crate::error::KgError;

/// What [`Session::checkpoint_if_changed`] did, and at which graph version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointOutcome {
    /// The graph was saved; the value is the version it was saved at.
    Written(u64),
    /// Unchanged since the last successful checkpoint at this version.
    Skipped(u64),
}

impl Session {
    /// Run one mutating statement as a one-shot transaction, making up to
    /// `attempts` tries (minimum one) while the commit loses an optimistic
    /// race.
    ///
    /// A lost race published nothing, so re-running on a fresh `begin()` cannot
    /// double-apply. Errors: whatever the statement raised, a
    /// [`KgError::TransactionConflict`] once the attempts are spent, and a
    /// [`KgError::DurabilityFailed`] when the write-ahead log rejected the
    /// frame (the commit was not published, and is never retried — the log is
    /// not going to answer differently).
    // KgError deliberately carries structured context; boxing it would change the public result type.
    #[allow(clippy::result_large_err)]
    pub fn execute_auto_commit(
        &self,
        query: &str,
        opts: &ExecuteOptions<'_>,
        attempts: u32,
    ) -> Result<ExecuteOutcome, KgError> {
        self.execute_auto_commit_observed(query, opts, attempts, &mut |_| {})
    }

    /// [`Self::execute_auto_commit`] with `between` called after each
    /// execution and before its commit, with the 1-based attempt number. A test
    /// seam: it lets a competing commit land inside the race window.
    // KgError deliberately carries structured context; boxing it would change the public result type.
    #[allow(clippy::result_large_err)]
    pub fn execute_auto_commit_observed(
        &self,
        query: &str,
        opts: &ExecuteOptions<'_>,
        attempts: u32,
        between: &mut dyn FnMut(u32),
    ) -> Result<ExecuteOutcome, KgError> {
        let mut attempt = 1;
        loop {
            let mut tx = self.begin();
            let working = tx.working_mut()?;
            let outcome = execute_mut(working, query, opts)?;
            between(attempt);
            match self.commit(tx, true) {
                CommitOutcome::Committed { .. } | CommitOutcome::NoWritesNoOp => {
                    return Ok(outcome)
                }
                CommitOutcome::ConflictDetected {
                    current_version,
                    base_version,
                } => {
                    if attempt >= attempts {
                        return Err(KgError::TransactionConflict {
                            base_version,
                            current_version,
                        });
                    }
                    attempt += 1;
                }
                // Exhaustive on purpose: an outcome added later must be decided
                // here, not fall into a catch-all that could read as success.
                CommitOutcome::DurabilityFailed { error } => {
                    return Err(KgError::DurabilityFailed { message: error });
                }
                CommitOutcome::OntologyViolated { error } => return Err(*error),
            }
        }
    }

    /// Save the session to `path` unless it is unchanged since the last
    /// successful checkpoint recorded in `last_version`.
    ///
    /// **First call always writes.** `last_version` starts `None` for the
    /// process, and the file on disk may predate it entirely (a stale `.kgl`,
    /// or a graph mutated and never checkpointed by a previous run), so no
    /// version comparison can be trusted until this process has written one.
    ///
    /// **Version read before the save, never after.** A commit landing between
    /// the read and the save's lock acquisition makes the recorded version one
    /// behind what reached disk, so the next call re-saves: a redundant write.
    /// Recording afterwards fails the other way: that commit would be recorded
    /// as saved when it was not, and the next call would skip it.
    ///
    /// A failed save leaves `last_version` untouched, so a retry still writes.
    /// Holding `last_version` as `&mut` is what serializes two checkpoints of
    /// one session; a caller sharing it across threads keeps it in a `Mutex`
    /// and holds the guard across this call.
    pub fn checkpoint_if_changed(
        &self,
        path: &std::path::Path,
        last_version: &mut Option<u64>,
    ) -> Result<CheckpointOutcome, String> {
        let version = self.version();
        if *last_version == Some(version) {
            return Ok(CheckpointOutcome::Skipped(version));
        }
        self.save(&path.to_string_lossy(), true)?;
        *last_version = Some(version);
        Ok(CheckpointOutcome::Written(version))
    }
}
