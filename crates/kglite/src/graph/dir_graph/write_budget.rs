//! The unsaved-write budget of a read-only mapped load.
//!
//! A graph loaded from a v9 `.kgl` whose topology is served in place keeps its
//! writes in a heap overlay over the file until a save writes them out and
//! rebases the graph onto the result (`io::file::rebase`). Nothing else bounds
//! that heap, so a read-only load that keeps writing grows until the machine
//! runs out of memory. The budget is that bound: past a limit the graph refuses
//! further writes and asks for a save, and at half the limit it warns once.
//!
//! ## What is metered
//!
//! Only a graph a load [enrolled](DirGraph::enroll_write_budget): a
//! [`load_file_with`](crate::graph::io::file::load_file_with) result with a
//! frozen base. Memory storage, a graph that decoded its topology (`topology`
//! `memory`, a v8 file, a file below the section floors) and a durable open
//! (it owns a write-ahead log, whose checkpoints release the overlay) carry no
//! budget.
//!
//! The reading is an **estimate** of the heap the unsaved writes hold, taken in
//! O(column stores) per statement: the overlay entries at
//! [`OVERLAY_ENTRY_BYTES`] each, plus the column stores' heap above what the
//! load left. It is deliberately not an allocator measurement.
//!
//! ## Where it is checked
//!
//! Every write route reaches one of two seams. A Cypher statement
//! (`session::execute`, so transactions, sessions and every binding) is refused
//! before it starts when the overlay alone is already at the limit, and rolled back when
//! it crosses the limit, so a refused statement leaves the graph exactly as it
//! was. A statement that runs without a rollback checkpoint (a lone node
//! `CREATE`) only gets the first check. A fluent bulk loader of the Python
//! binding gets the first check before it starts, and a call may overshoot the
//! limit by its own size.
//!
//! The warning state lives behind an `Arc` shared by every clone of the graph,
//! so a transaction's working copy and the published graph agree on whether the
//! warning was given.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::KgError;
use crate::graph::storage::GraphRead;

use super::DirGraph;

const MIB: u64 = 1 << 20;

/// Smallest default limit: a graph whose file is small still gets room for
/// ordinary interactive writes.
pub const WRITE_BUDGET_FLOOR_BYTES: u64 = 512 * MIB;

/// Heap one overlay entry (an operation, a node weight or an edge weight)
/// stands for, with the column rows and index entries a write adds beside it.
/// Calibrated against macOS `phys_footprint` over 2M mixed writes (half node
/// creates, half relationship creates) of a 1M-node, 10M-relationship graph:
/// the footprint grew by 0.75 to 1.0 times the estimate at this value, so it
/// leans high. An estimate, not a measurement.
pub const OVERLAY_ENTRY_BYTES: u64 = 128;

/// What a load was asked to do about the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteBudgetSetting {
    /// `max(512 MiB, half the size of the loaded file)`.
    #[default]
    Default,
    /// A limit in bytes. `0` refuses every write.
    Bytes(u64),
    /// No limit and no warning.
    Unlimited,
}

impl WriteBudgetSetting {
    /// Parse an operator's spelling: `default`, `unlimited` (or `none`), a byte
    /// count, or a count with a `K`, `M` or `G` suffix (binary multiples, with
    /// an optional `iB`/`B`), for example `512M` or `2GiB`. The flag and
    /// manifest spelling of the command-line wrappers.
    pub fn parse(text: &str) -> Result<Self, String> {
        let lowered = text.trim().to_ascii_lowercase();
        match lowered.as_str() {
            "default" => return Ok(Self::Default),
            "unlimited" | "none" => return Ok(Self::Unlimited),
            _ => {}
        }
        let digits = lowered.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let unit = &lowered[digits.len()..];
        let scale: u64 = match unit {
            "" | "b" => 1,
            "k" | "kb" | "kib" => 1 << 10,
            "m" | "mb" | "mib" => 1 << 20,
            "g" | "gb" | "gib" => 1 << 30,
            _ => return Err(Self::parse_refusal(text)),
        };
        let count: u64 = digits
            .trim()
            .parse()
            .map_err(|_| Self::parse_refusal(text))?;
        count
            .checked_mul(scale)
            .map(Self::Bytes)
            .ok_or_else(|| Self::parse_refusal(text))
    }

    fn parse_refusal(text: &str) -> String {
        format!(
            "'{text}' is not a write budget: use 'default', 'unlimited', or a size such as 268435456, 512M or 2GiB"
        )
    }
}

/// Where a graph stands against its budget, for `graph_info()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteBudgetState {
    /// No budget applies: memory storage, a durable open, or a graph with no
    /// overlay over a frozen base.
    NotApplicable,
    /// A budget applies and the load asked for none.
    Unlimited,
    /// Under half the limit.
    Ok,
    /// At or over half the limit; writes are still accepted.
    Warning,
    /// At or over the limit; writes are refused until a save.
    Exceeded,
}

impl WriteBudgetState {
    /// The spelling `graph_info()` reports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotApplicable => "not_applicable",
            Self::Unlimited => "unlimited",
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Exceeded => "exceeded",
        }
    }
}

/// The three `graph_info()` readings of the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WriteBudgetReport {
    pub(crate) limit: Option<u64>,
    pub(crate) used: u64,
    pub(crate) state: WriteBudgetState,
}

#[derive(Default)]
struct Shared {
    warned: AtomicBool,
    pending: Mutex<Option<String>>,
    /// Why the last save could not release the overlay, when it could not.
    retained: Mutex<Option<&'static str>>,
}

/// The budget a load enrolled a graph in.
#[derive(Clone)]
pub(crate) struct WriteBudget {
    limit: Option<u64>,
    setting: WriteBudgetSetting,
    /// Column-store heap the load left, which is not an unsaved write.
    baseline_heap: usize,
    shared: Arc<Shared>,
}

pub(crate) fn limit_for(setting: WriteBudgetSetting, file_bytes: u64) -> Option<u64> {
    match setting {
        WriteBudgetSetting::Default => Some(WRITE_BUDGET_FLOOR_BYTES.max(file_bytes / 2)),
        WriteBudgetSetting::Bytes(bytes) => Some(bytes),
        WriteBudgetSetting::Unlimited => None,
    }
}

fn mib(bytes: u64) -> String {
    if bytes < MIB {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    }
}

impl DirGraph {
    fn column_heap_bytes(&self) -> usize {
        self.graph
            .column_stores_iter()
            .map(|(_, store)| store.heap_bytes())
            .sum()
    }

    /// Enroll a graph a load just produced. A graph with no frozen base keeps
    /// no budget: it has no overlay to meter and a save has no rebase to reset.
    pub(crate) fn enroll_write_budget(&mut self, setting: WriteBudgetSetting, file_bytes: u64) {
        self.write_budget = self.graph.has_frozen_base().then(|| WriteBudget {
            limit: limit_for(setting, file_bytes),
            setting,
            baseline_heap: self.column_heap_bytes(),
            shared: Arc::default(),
        });
    }

    /// The budget, while it applies to the graph as it stands: not once the
    /// graph owns a write-ahead log, whose checkpoints release the overlay.
    fn active_write_budget(&self) -> Option<&WriteBudget> {
        self.write_budget
            .as_ref()
            .filter(|_| self.graph.has_frozen_base() && !self.graph.is_wal_owner())
    }

    /// The overlay's share of the estimate: its entries at
    /// [`OVERLAY_ENTRY_BYTES`] each.
    fn overlay_budget_bytes(&self) -> u64 {
        self.graph.overlay_entries().unwrap_or(0) as u64 * OVERLAY_ENTRY_BYTES
    }

    /// Estimated heap, in bytes, of the writes since the load or the last
    /// rebase.
    fn write_budget_used_with(&self, budget: &WriteBudget) -> u64 {
        let overlay = self.overlay_budget_bytes();
        let columns = self
            .column_heap_bytes()
            .saturating_sub(budget.baseline_heap) as u64;
        overlay + columns
    }

    /// What `graph_info()` reports.
    pub(crate) fn write_budget_report(&self) -> WriteBudgetReport {
        let Some(budget) = self.active_write_budget() else {
            return WriteBudgetReport {
                limit: None,
                used: 0,
                state: WriteBudgetState::NotApplicable,
            };
        };
        let used = self.write_budget_used_with(budget);
        let state = match budget.limit {
            None => WriteBudgetState::Unlimited,
            Some(limit) if used >= limit => WriteBudgetState::Exceeded,
            Some(limit) if used >= limit / 2 => WriteBudgetState::Warning,
            Some(_) => WriteBudgetState::Ok,
        };
        WriteBudgetReport {
            limit: budget.limit,
            used,
            state,
        }
    }

    fn write_budget_refusal(&self, budget: &WriteBudget, limit: u64) -> KgError {
        let used = self.write_budget_used_with(budget);
        let retained = budget
            .shared
            .retained
            .lock()
            .map(|reason| *reason)
            .unwrap_or_default();
        let way_out = match retained {
            Some(reason) => format!(
                "A save already wrote these changes, but this graph could not release their memory ({reason}); \
                 reload the saved file, open it durable for continuous writes, or raise the write budget."
            ),
            None => "Save the graph to continue (a save writes the unsaved changes out and releases \
                 their memory), open it durable for continuous writes, or raise the write budget."
                .to_string(),
        };
        KgError::WriteBudgetExceeded(format!(
            "the graph holds about {} of unsaved writes over its mapped file, at the write budget of {}. \
             Reads still work and nothing from the refused write was applied. {way_out}",
            mib(used),
            mib(limit),
        ))
    }

    /// Refuse a write while the graph is at its limit. Nothing is applied by
    /// the caller yet.
    // KgError deliberately carries structured context; boxing it would change the public result type.
    #[allow(clippy::result_large_err)]
    pub fn check_write_budget(&self) -> Result<(), KgError> {
        let Some(budget) = self.active_write_budget() else {
            return Ok(());
        };
        let Some(limit) = budget.limit else {
            return Ok(());
        };
        // The overlay alone: the full reading costs a pass over the column
        // stores, which the check after the statement pays instead.
        if self.overlay_budget_bytes() >= limit {
            return Err(self.write_budget_refusal(budget, limit));
        }
        Ok(())
    }

    /// Judge the state a write left. Past the limit the write is refused when
    /// `can_refuse` (the caller holds a checkpoint it can restore); at half the
    /// limit a warning is parked for the binding, once per upward crossing.
    // KgError deliberately carries structured context; boxing it would change the public result type.
    #[allow(clippy::result_large_err)]
    pub fn settle_write_budget(&self, can_refuse: bool) -> Result<(), KgError> {
        let Some(budget) = self.active_write_budget() else {
            return Ok(());
        };
        let Some(limit) = budget.limit else {
            return Ok(());
        };
        let used = self.write_budget_used_with(budget);
        if can_refuse && used > limit {
            return Err(self.write_budget_refusal(budget, limit));
        }
        if used < limit / 2 {
            budget.shared.warned.store(false, Ordering::Relaxed);
        } else if !budget.shared.warned.swap(true, Ordering::Relaxed) {
            if let Ok(mut pending) = budget.shared.pending.lock() {
                *pending = Some(format!(
                    "unsaved writes hold about {} of the {} write budget; save the graph to release \
                     them. At the budget further writes are refused.",
                    mib(used),
                    mib(limit),
                ));
            }
        }
        Ok(())
    }

    /// The warning [`Self::settle_write_budget`] parked, if any. A binding
    /// calls it after a write and announces the text its own way.
    pub fn take_write_budget_warning(&self) -> Option<String> {
        self.write_budget
            .as_ref()?
            .shared
            .pending
            .lock()
            .ok()?
            .take()
    }

    /// A save just rebased the graph onto the file it wrote: the overlay is
    /// empty, so the budget starts over against the new file.
    pub(crate) fn reset_write_budget_after_rebase(&mut self, file_bytes: u64) {
        let baseline_heap = self.column_heap_bytes();
        if let Some(budget) = self.write_budget.as_mut() {
            budget.limit = limit_for(budget.setting, file_bytes);
            budget.baseline_heap = baseline_heap;
            budget.shared = Arc::default();
        }
    }

    /// A save wrote the graph but could not rebase it, so the memory the
    /// writes hold stays. Recorded so the refusal does not tell the user a save
    /// will help.
    pub(crate) fn note_write_budget_retained(&self, reason: &'static str) {
        if let Some(budget) = &self.write_budget {
            if let Ok(mut retained) = budget.shared.retained.lock() {
                *retained = Some(reason);
            }
        }
    }
}
