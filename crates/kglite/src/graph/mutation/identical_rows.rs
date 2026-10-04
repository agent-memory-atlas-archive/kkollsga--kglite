//! Identical relationship rows within one load.
//!
//! A load that owns its edges (`maintain::InitialLoad`) writes one
//! relationship per row, so a source with repeated rows — a history table fed
//! to a relationship loader with no property columns, say — stores the same
//! relationship many times and every count over it multiplies. The tracker
//! sees each row's key (resolved source and target node, non-null property
//! values) in row order across all the calls of one load and either reports the
//! repeats ([`IdenticalRows::Keep`], the default: data and counts unchanged) or
//! drops every row after the first ([`IdenticalRows::Collapse`]).

use crate::datatypes::{DataFrame, Value};
use crate::graph::storage::interner::InternedKey;
use petgraph::graph::NodeIndex;
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::{Hash, Hasher};

/// What a load does with rows identical to an earlier row of the same load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdenticalRows {
    /// Store one relationship per row and warn once per relationship type.
    #[default]
    Keep,
    /// Store one relationship per distinct row; no warning.
    Collapse,
}

impl IdenticalRows {
    /// [`Collapse`](Self::Collapse) when a `distinct` flag is set.
    pub fn from_distinct(distinct: bool) -> Self {
        if distinct {
            Self::Collapse
        } else {
            Self::Keep
        }
    }
}

/// Distinct row keys remembered in [`IdenticalRows::Keep`] mode, bounding the
/// check's memory (about 100 MB) on very large loads. Rows past the bound go
/// unchecked and the warning says so; `Collapse` has no bound because a
/// partial collapse would be a wrong answer, not a lower bound.
const KEEP_MODE_TRACK_LIMIT: usize = 4_000_000;

/// A row's non-null property cells followed by its title cells, as
/// `(column, value)`.
pub(super) type RowCells = [(usize, Value)];

/// A row's identity: its endpoint pair, exactly, plus a 128-bit hash of its
/// cells (0 when it has none, which makes a property-less row's key exact).
pub(super) type RowKey = (u64, u128);

#[cfg(test)]
thread_local! {
    /// Replaces [`cells_hash`] in tests, to force collisions.
    pub(super) static FORCED_CELLS_HASH: std::cell::Cell<Option<u128>> =
        const { std::cell::Cell::new(None) };
}

fn cells_hash(cells: &RowCells) -> u128 {
    #[cfg(test)]
    if let Some(forced) = FORCED_CELLS_HASH.with(|f| f.get()) {
        return forced;
    }
    let mut hashes = (FxHasher::default(), FxHasher::default());
    hashes.1.write_u64(0x9e37_79b9_7f4a_7c15);
    for (column, value) in cells {
        column.hash(&mut hashes.0);
        value.hash(&mut hashes.0);
        column.hash(&mut hashes.1);
        value.hash(&mut hashes.1);
    }
    (u128::from(hashes.0.finish()) << 64) | u128::from(hashes.1.finish())
}

/// Row-key state shared by the calls (chunks, target groups) of one load.
pub(crate) struct IdenticalRowTracker {
    policy: Option<IdenticalRows>,
    seen: FxHashMap<RowKey, u32>,
    /// `Collapse` only: the first row's cells per key with cells, chained when
    /// distinct rows collide, so a hash collision can never drop a distinct row.
    exact: FxHashMap<RowKey, Vec<Box<RowCells>>>,
    rows: usize,
    max_copies: u32,
    with_properties: bool,
    limit_reached: bool,
}

impl IdenticalRowTracker {
    pub(crate) fn new(policy: IdenticalRows) -> Self {
        Self {
            policy: Some(policy),
            seen: FxHashMap::default(),
            exact: FxHashMap::default(),
            rows: 0,
            max_copies: 0,
            with_properties: false,
            limit_reached: false,
        }
    }

    /// A tracker that admits every row and never warns.
    pub(crate) fn off() -> Self {
        Self {
            policy: None,
            ..Self::new(IdenticalRows::Keep)
        }
    }

    /// Room for `rows` more keys, so a chunk's inserts do not rehash.
    fn reserve(&mut self, rows: usize) {
        let room = KEEP_MODE_TRACK_LIMIT.saturating_sub(self.seen.len());
        self.seen
            .reserve(if self.policy == Some(IdenticalRows::Keep) {
                rows.min(room)
            } else {
                rows
            });
    }

    pub(crate) fn is_on(&self) -> bool {
        self.policy.is_some()
    }

    /// Record one row (`pair` is its endpoint pair); `false` means the row
    /// repeats an earlier one and must be dropped.
    ///
    /// `Keep` counts rows by hash alone and stores no cells: a collision there
    /// can only miscount the warning, never change data. `Collapse` compares
    /// the cells of a row whose key is already present with the stored ones, so
    /// it drops only a row that is equal, not merely hash-equal.
    pub(super) fn admit(&mut self, pair: u64, cells: &RowCells, has_properties: bool) -> bool {
        let Some(policy) = self.policy else {
            return true;
        };
        if policy == IdenticalRows::Keep && self.seen.len() >= KEEP_MODE_TRACK_LIMIT {
            self.limit_reached = true;
            return true;
        }
        self.with_properties |= has_properties;
        self.rows += 1;
        if policy == IdenticalRows::Collapse && !cells.is_empty() {
            let chain = self.exact.entry((pair, cells_hash(cells))).or_default();
            if chain.iter().any(|first| **first == *cells) {
                return false;
            }
            chain.push(cells.into());
            return true;
        }
        let key = (
            pair,
            if cells.is_empty() {
                0
            } else {
                cells_hash(cells)
            },
        );
        let copies = self.seen.entry(key).or_insert(0);
        *copies += 1;
        self.max_copies = self.max_copies.max(*copies);
        *copies == 1 || policy == IdenticalRows::Keep
    }

    /// The one warning for this load of `edge_type`, when `Keep` mode saw
    /// identical rows.
    pub(crate) fn warning(&self, edge_type: &str) -> Option<String> {
        if self.policy != Some(IdenticalRows::Keep) || self.max_copies < 2 {
            return None;
        }
        let combination = if self.with_properties {
            "(source, target, properties) combinations"
        } else {
            "(source, target) pairs"
        };
        let scope = if self.limit_reached {
            format!(" among the first {} rows", self.rows)
        } else {
            String::new()
        };
        Some(format!(
            "relationship type '{edge_type}': {} relationships were created on {} distinct {combination}{scope}, \
             with up to {} identical copies of one; identical copies are kept by default. \
             Pass distinct=True to add_relationships (blueprint junction edge: `distinct: true`) \
             to keep one relationship per combination; replace_relationships has no such \
             option, so drop the repeated rows from its input.",
            self.rows,
            self.seen.len(),
            self.max_copies,
        ))
    }
}

/// The cells of one load's rows, read through [`RowIdentity::admit`].
pub(super) struct RowColumns<'a> {
    pub(super) frame: &'a DataFrame,
    /// Title column names, when the load writes titles: a title is written to
    /// the endpoint node, so rows differing only there are not interchangeable.
    pub(super) titles: (Option<&'a str>, Option<&'a str>),
    pub(super) properties: &'a [(String, InternedKey, usize)],
}

/// One call's view of its rows for the tracker. Inert (every row admitted)
/// unless the load writes one relationship per row; a load that merges folds
/// rows on the endpoint pair, and a declared temporal type already drops a row
/// identical to an earlier or stored one.
pub(super) struct RowIdentity<'a> {
    tracker: &'a mut IdenticalRowTracker,
    frame: &'a DataFrame,
    columns: Vec<usize>,
    cells: Vec<(usize, Value)>,
    property_count: usize,
    live: bool,
}

impl<'a> RowIdentity<'a> {
    pub(super) fn new(
        tracker: &'a mut IdenticalRowTracker,
        owns_edges: bool,
        cols: RowColumns<'a>,
    ) -> Self {
        let live = owns_edges && tracker.is_on();
        if live {
            tracker.reserve(cols.frame.row_count());
        }
        let columns = if live {
            cols.properties
                .iter()
                .map(|(_, _, column)| *column)
                .chain(
                    [cols.titles.0, cols.titles.1]
                        .into_iter()
                        .flatten()
                        .filter_map(|field| cols.frame.get_column_index(field)),
                )
                .collect()
        } else {
            Vec::new()
        };
        Self {
            tracker,
            frame: cols.frame,
            columns,
            cells: Vec::new(),
            property_count: cols.properties.len(),
            live,
        }
    }

    /// The replayed deferred rows as `(row, endpoints)`, without those whose
    /// relationship repeats an earlier one of the load. A row whose endpoints
    /// did not resolve is kept; the caller counts it as skipped.
    pub(super) fn new_among_replayed(
        &mut self,
        deferred: &[(usize, Value, Value)],
        replayed: Vec<Option<(NodeIndex, NodeIndex)>>,
    ) -> Vec<(usize, Option<(NodeIndex, NodeIndex)>)> {
        let mut kept = Vec::with_capacity(replayed.len());
        for ((row, _, _), endpoints) in deferred.iter().zip(replayed) {
            let fresh = endpoints.is_none_or(|(source, target)| self.admit(*row, source, target));
            if fresh {
                kept.push((*row, endpoints));
            }
        }
        kept
    }

    /// Whether the relationship `row` creates between `source` and `target`
    /// is new to this load; `false` means drop it.
    pub(super) fn admit(&mut self, row: usize, source: NodeIndex, target: NodeIndex) -> bool {
        if !self.live {
            return true;
        }
        let pair = (u64::from(source.index() as u32) << 32) | u64::from(target.index() as u32);
        self.cells.clear();
        let mut properties = 0;
        for (position, column) in self.columns.iter().enumerate() {
            match self.frame.get_value_by_index(row, *column) {
                None | Some(Value::Null) => {}
                Some(value) => {
                    if position < self.property_count {
                        properties += 1;
                    }
                    self.cells.push((*column, value));
                }
            }
        }
        self.tracker.admit(pair, &self.cells, properties > 0)
    }
}
