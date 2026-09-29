//! Shared Cypher execution helpers for interactive and one-shot CLI modes.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;
use kglite::api::session::{
    execute_mut, execute_read, CsvImportPolicy, ExecuteOptions, ExecuteOutcome,
};
use kglite::api::{make_dir_graph_mut, DirGraph, Value};

use crate::format::{render, CellCap, Mode};

/// Per-query knobs shared by the REPL and one-shot commands.
#[derive(Debug, Default)]
pub struct QueryOptions {
    pub cancel: Option<&'static AtomicBool>,
    pub write_scope: Option<HashSet<String>>,
    pub git_sha: Option<String>,
    pub modified_by: Option<String>,
    /// Opt in to the parallel Cypher runtime (`--parallel`). Off by default;
    /// a hint the engine's own runtime gate may decline.
    pub parallel: bool,
    /// Deadline for this statement, in milliseconds (`--timeout-ms`).
    ///
    /// `None` is **no deadline**, and that is the CLI's declared default — not
    /// an oversight, and deliberately unlike Python and the MCP server, which
    /// adopt `kglite::api::session::DEFAULT_TIMEOUT_MS`. A human at a terminal
    /// has Ctrl-C (reads and `CALL` are interruptible), and a batch query over
    /// a Wikidata-scale graph legitimately runs for hours, so a silent
    /// three-minute kill would be a regression. `Some(0)` is the same as
    /// `None`.
    pub timeout_ms: Option<u64>,
}

/// Execute one Cypher statement through the mutable session path.
///
/// `execute_mut` internally keeps read queries read-only, so this single seam
/// supports both the single-user REPL and write-enabled one-shot commands.
pub fn execute(
    graph: &mut Arc<DirGraph>,
    query: &str,
    params: &HashMap<String, Value>,
    options: &QueryOptions,
) -> Result<ExecuteOutcome> {
    let mut opts = ExecuteOptions::new(params).with_csv_import(CsvImportPolicy::LocalFilesystem);
    opts.set_timeout_ms(options.timeout_ms);
    opts.cancel = options.cancel;
    opts.write_scope = options.write_scope.as_ref();
    opts.git_sha = options.git_sha.as_deref();
    opts.modified_by = options.modified_by.as_deref();
    opts.parallel = options.parallel;
    opts.streaming = true;

    let g = make_dir_graph_mut(graph);
    let outcome = execute_mut(g, query, &opts)?;
    // The CLI's commit boundary is the statement, so this is where a change
    // stream learns about it — a no-op unless `CALL db.cdc.enable()` has been
    // run. A statement that *failed* returns above, having already rolled its
    // captured ops back, so nothing uncommitted can be published from here.
    kglite::api::cdc::drain_at_commit(g);
    Ok(outcome)
}

/// Execute one read-only Cypher statement.
pub fn execute_readonly(
    graph: &Arc<DirGraph>,
    query: &str,
    params: &HashMap<String, Value>,
    options: &QueryOptions,
) -> Result<ExecuteOutcome> {
    let mut opts = ExecuteOptions::new(params)
        .with_csv_import(CsvImportPolicy::LocalFilesystem)
        .with_parallel(options.parallel);
    opts.set_timeout_ms(options.timeout_ms);
    opts.streaming = true;
    Ok(execute_read(graph, query, &opts)?)
}

/// Render a Cypher outcome in the requested CLI mode. `cap` is the table
/// renderer's per-cell width ceiling — `format::stdout_cell_cap()` for output
/// a human reads, `None` for output a program parses (the JSONL session).
pub fn render_outcome(mode: Mode, outcome: &ExecuteOutcome, cap: CellCap) -> String {
    let r = &outcome.result;
    render(mode, &r.columns, &r.rows, cap)
}

/// A Cypher outcome's rows for the JSONL session, serialised through
/// [`crate::format::json_rows`] so each row object lists its columns in the
/// query's order. Built as a `serde_json::Value` the rows would come out
/// alphabetised (`Value::Object` is a `BTreeMap`; the `preserve_order` feature
/// is off by design — see `JsonRow`).
pub struct OrderedJsonRows {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

impl OrderedJsonRows {
    pub fn from_outcome(outcome: ExecuteOutcome) -> Self {
        OrderedJsonRows {
            columns: outcome.result.columns,
            rows: outcome.result.rows,
        }
    }

    /// The rows as a `Value`, for a consumer that edits the response as one
    /// (the agent budget pass). Row keys sort again on this path.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

impl serde::Serialize for OrderedJsonRows {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        crate::format::json_rows(&self.columns, &self.rows).serialize(serializer)
    }
}

/// Write CLI output, treating a closed downstream pipe as successful exit.
pub fn write_stdout(text: &str) -> io::Result<()> {
    write_stdout_raw(text)?;
    write_stdout_raw("\n")
}

/// Write CLI output verbatim, with no trailing newline added.
///
/// For output whose bytes are the contract — a skill body is stored markdown a
/// caller may redirect into a file, so [`write_stdout`]'s convenience newline
/// would be an edit to the content.
pub fn write_stdout_raw(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout.write_all(text.as_bytes()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn parse_write_scope(raw: Option<&str>) -> Option<HashSet<String>> {
    raw.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_write_scope_splits_commas_and_ignores_blanks() {
        let scope = parse_write_scope(Some("Plan, Task,,Artifact ")).unwrap();
        assert!(scope.contains("Plan"));
        assert!(scope.contains("Task"));
        assert!(scope.contains("Artifact"));
        assert_eq!(scope.len(), 3);
    }

    #[test]
    fn parse_write_scope_none_is_unrestricted() {
        assert!(parse_write_scope(None).is_none());
    }
}
