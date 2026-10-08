//! Auto-commit execution for [`KgliteBackend`]: reads run on a snapshot, and
//! every mutation (data or schema) runs as a one-shot transaction that commits
//! before `execute()` returns.
//!
//! Neo4j commits an auto-commit transaction when its result stream is fully
//! consumed. boltr hands the backend no hook after streaming, and the engine
//! materialises a statement's rows before the commit anyway, so the commit
//! happens here, at RUN. A write that fails, conflicts or cannot be logged
//! sends no rows and applies nothing. The one observable difference: a RESET
//! or disconnect between RUN and PULL does not undo the write.

use super::*;

/// Internal attempts at an auto-commit write that loses an optimistic commit
/// race. Drivers never retry `session.run`, so the server does.
pub(super) const OPTIMISTIC_ATTEMPTS: u32 = 3;

/// Whether `query` writes data or schema, as the executor would run it.
/// `EXPLAIN` of a write only describes a plan, so it is a read: running it
/// against a working copy would publish an unchanged graph under a new version.
pub(super) fn is_write_statement(query: &str) -> Result<bool, BoltError> {
    let (parsed, is_mutation) = cypher::parse_with_mutation_check(query).map_err(kg_to_bolt)?;
    Ok(is_mutation && !parsed.explain)
}

/// The refusal of a write in a read-mode session or transaction, with the
/// code and wording Neo4j uses.
pub(super) fn access_mode_error(scope: &str) -> BoltError {
    BoltError::Query {
        code: "Neo.ClientError.Statement.AccessMode".into(),
        message: format!(
            "Writing in read access mode not allowed: this {scope} was opened with mode \"r\" \
             (read access). Use a write session or transaction."
        ),
    }
}

fn is_conflict(error: &BoltError) -> bool {
    matches!(error, BoltError::Query { code, .. }
        if code == kglite::api::KgErrorCode::TransactionConflict.neo4j_status_code())
}

impl KgliteBackend {
    /// Run one auto-commit statement. `read_mode` is the RUN's `mode: "r"`.
    pub(super) async fn execute_auto_commit(
        &self,
        query: &str,
        parameters: &HashMap<String, BoltValue>,
        meta: &TxMeta,
        read_mode: bool,
    ) -> Result<ResultStream, BoltError> {
        // The executor's parse cache makes the engine's own parse free.
        if !is_write_statement(query)? {
            return off_async_worker(|| {
                let kg_params = decode_params(parameters)?;
                let started = Instant::now();
                let snapshot = self.session.snapshot();
                let opts = self.execute_opts(&kg_params, meta);
                let outcome = kglite::api::session::execute_read(&snapshot, query, &opts)
                    .map_err(kg_to_bolt)?;
                finish_stream(outcome.result, "r", outcome.explain, started)
            });
        }
        if self.readonly {
            return Err(BoltError::Forbidden(
                "server is read-only — mutations rejected (--readonly flag)".into(),
            ));
        }
        if read_mode {
            return Err(access_mode_error("session"));
        }

        let kg_params = decode_params(parameters)?;
        let started = Instant::now();
        // Queue mode holds the slot to the end of the function, past the
        // publish; optimistic mode takes none and retries a lost race.
        let (_slot, attempts) = if self.writer.config().mode == WriteConcurrency::Queue {
            (Some(self.auto_commit_slot().await?), 1)
        } else {
            (None, OPTIMISTIC_ATTEMPTS)
        };
        let opts = self.execute_opts(&kg_params, meta);
        let result = off_async_worker(|| self.one_shot_write(query, &opts, attempts, &mut |_| {}))?;
        // Neo4j's summary types: `s` schema, `rw` a write that returns rows,
        // `w` a write that does not.
        let type_str = if is_schema_ddl(query) {
            "s"
        } else if result.columns.is_empty() {
            "w"
        } else {
            "rw"
        };
        finish_stream(result, type_str, false, started)
    }

    /// Wait for the writer slot as an auto-commit statement. The returned guard
    /// keeps the slot's idle reclaim from reaping a running statement.
    async fn auto_commit_slot(&self) -> Result<(WriterPermit, impl Sized), BoltError> {
        let id = self.tx_counter.fetch_add(1, Ordering::Relaxed);
        let permit = self
            .acquire_writer_slot(&format!("auto-commit-{id}"))
            .await?;
        let running = permit.activity().begin_query();
        Ok((permit, running))
    }

    /// Begin, execute and publish `query` as one transaction, making up to
    /// `attempts` tries while the commit loses an OCC race. A lost race
    /// published nothing, so a re-run on a fresh begin cannot double-apply.
    /// `between` runs after each execution and before its commit, with the
    /// attempt number (1-based); tests use it to land a competing commit.
    pub(super) fn one_shot_write(
        &self,
        query: &str,
        opts: &kglite::api::session::ExecuteOptions<'_>,
        attempts: u32,
        between: &mut dyn FnMut(u32),
    ) -> Result<cypher::CypherResult, BoltError> {
        let mut attempt = 1;
        loop {
            let mut tx = self.session.begin();
            let working = tx.working_mut().map_err(kg_to_bolt)?;
            let outcome =
                kglite::api::session::execute_mut(working, query, opts).map_err(kg_to_bolt)?;
            between(attempt);
            match self.publish(tx, "auto-commit", "auto-commit") {
                Ok(()) => return Ok(outcome.result),
                Err(e) if is_conflict(&e) && attempt < attempts => {
                    tracing::debug!(attempt, "auto-commit write lost a commit race; retrying");
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
