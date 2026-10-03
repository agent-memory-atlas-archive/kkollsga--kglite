//! The endpoint index: per declared target and graph version, the `from`
//! and `to` bounds of the rows the declaration governs as two sorted arrays
//! of `i64` keys, each key beside its row's slot (`NodeIndex` /
//! `EdgeIndex` index). Two binary searches give how many rows are valid at
//! an instant, which *segment* of the time line the instant falls in, and —
//! from those two prefix lengths, with no property read — the rows a mask
//! must clear.
//!
//! ## Keys
//!
//! A key is microseconds since 1970-01-01 on one axis for dates and
//! timestamps alike. The evaluator ([`super::eval`]) compares at date grain
//! whenever either side is a date, so each side maps its grain to the edge of
//! the day that keeps both tests a prefix of a sorted array:
//!
//! - `from`: a date is its midnight, a timestamp itself, NULL `i64::MIN`.
//!   The row has started by `t` iff `key <= start_cutoff(t)`, where the
//!   cutoff of a date `d` is the last microsecond of `d` and that of a
//!   timestamp is itself.
//! - `to` under `closed`: a date is the last microsecond of its day, a
//!   timestamp itself, NULL `i64::MAX`. The row has ended by `t` iff
//!   `key < end_cutoff(t)`, where the cutoff of a date is its midnight and
//!   that of a timestamp is itself.
//! - `to` under `half_open`: a date is its midnight, a timestamp itself,
//!   NULL `i64::MAX`. Ended iff `key <= end_cutoff(t)` — for a timestamp
//!   `to` against a date `t` that is exactly the evaluator's "ends after
//!   `t`'s midnight" exception.
//!
//! A row whose interval is empty (`from > to`, or `from == to` under
//! `half_open`: [`eval::end_admits`] refuses its own start) is valid on no
//! instant; it is kept apart in `empty_slots`, out of both arrays. For every
//! other row `from key <= to key` (`<` under `half_open`), so a row that has
//! ended has also started, and
//!
//! ```text
//! count(t) = started(t) - ended(t)
//! ```
//!
//! with `started`/`ended` the two prefix lengths. The pair is the
//! [`Segment`]: two instants with the same pair see the same valid rows,
//! because both sets are fixed prefixes of fixed arrays.
//!
//! A timestamp with sub-microsecond precision has no exact key. A bound like
//! that makes its target's build fail (the target keeps property guards); an
//! instant like that resolves no segment.
//!
//! ## Cache, cap, modes
//!
//! Indexes are built lazily, one walk over a target's rows through
//! `GraphRead` (so journalled writes are seen), and cached inside the
//! declaration store in a fork-private cache stamped with the graph version.
//! Every write bumps the version, so the next lookup rebuilds; a fork starts
//! cold, and a version set directly empties the cache. Inside a mutating
//! statement the version moves only at commit, so the write engine empties
//! the cache after each writing clause ([`invalidate`]). A target keeps
//! property guards instead of an index when a row holds a bound the
//! evaluator cannot read (a mask cannot raise that error only when the row is
//! visited), or when its arrays would pass [`ENDPOINT_INDEX_BYTE_CAP`], which
//! bounds one graph's arrays and cached masks together. A build is refused
//! before its peak working set ([`PEAK_BYTES_PER_PAIR`]) would pass what the
//! cap leaves beside the kept arrays; cached masks are evicted oldest first
//! to make room for a new mask, and one that cannot fit beside the arrays at
//! all is not built (its targets keep property guards). Disk mode
//! never builds an index (its heap must not grow with the graph); the walk
//! still counts the rows for `db.temporal.declarations()`. The same cache,
//! stamp and cap hold each node type's duplicate-id map
//! ([`super::duplicate_ids`]), which every mode builds.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, PoisonError, RwLockReadGuard, RwLockWriteGuard, Weak};

use chrono::{Datelike, NaiveDate};
use fixedbitset::FixedBitSet;
use petgraph::graph::EdgeIndex;

use super::declarations::TemporalTarget;
use super::duplicate_ids::DuplicateIds;
use super::eval::{self, Instant, IntervalConvention};
use super::slice::{SliceKey, ValidSlice};
use super::validate::{edge_bound, for_each_edge_row, for_each_node_row, node_bound, EdgeRow};
use crate::datatypes::values::Value;
use crate::graph::algorithms::text_index::bm25::MaskedStats;
use crate::graph::core::graph_filter::{GuardBounds, GuardTemplate, ValidTimeSelector};
use crate::graph::dir_graph::DirGraph;
use crate::graph::schema::{InternedKey, TemporalConfig};
use crate::graph::storage::column_store::{exact_micros, DAY_US, EPOCH_DAYS_FROM_CE};
use crate::graph::storage::GraphRead;

/// The bytes one graph's endpoint arrays and cached masks may hold together.
/// A per-graph stand-in for a shared cache budget; [`BYTE_CAP_ENV`]
/// overrides it.
pub(crate) const ENDPOINT_INDEX_BYTE_CAP: usize = 128 << 20;

/// Environment variable that replaces [`ENDPOINT_INDEX_BYTE_CAP`] (a byte
/// count), read at each build.
pub(crate) const BYTE_CAP_ENV: &str = "KGLITE_TEMPORAL_INDEX_MAX_BYTES";

/// The most the build holds per reserved row slot: the `from` and `to`
/// `(key, slot)` pair buffers (16 bytes each), plus at most one more buffer
/// of that size alive at once — the old one while a pair buffer grows (both
/// grow to the same capacity, one after the other), or the finished 12-byte
/// `(key, slot)` arrays of the side being split. The build reserves its own
/// capacity and checks this bound before every reservation, so the walk's
/// peak stays within the budget; the finished arrays keep 24 bytes a row.
const PEAK_BYTES_PER_PAIR: usize = 3 * size_of::<(i64, u32)>();
/// An empty row's slot, and its buffer's old copy while that grows.
const PEAK_BYTES_PER_EMPTY: usize = 2 * size_of::<u32>();
/// The first capacity a build reserves.
const MIN_RESERVE: usize = 16;
/// The most masks cached at once; the byte cap may hold fewer.
const MAX_CACHED_MASKS: usize = 8;
/// The most endpoint-invalid counts cached at once.
const MAX_CACHED_INVALID: usize = 8;

fn day_start(date: NaiveDate) -> i64 {
    (i64::from(date.num_days_from_ce()) - EPOCH_DAYS_FROM_CE) * DAY_US
}

fn from_key(from: Option<Instant>) -> Option<i64> {
    match from {
        None => Some(i64::MIN),
        Some(Instant::Date(d)) => Some(day_start(d)),
        Some(Instant::Timestamp(ts)) => exact_micros(ts),
    }
}

fn to_key(to: Option<Instant>, convention: IntervalConvention) -> Option<i64> {
    match to {
        None => Some(i64::MAX),
        Some(Instant::Date(d)) if convention.is_closed() => Some(day_start(d) + DAY_US - 1),
        Some(Instant::Date(d)) => Some(day_start(d)),
        Some(Instant::Timestamp(ts)) => exact_micros(ts),
    }
}

/// `(start cutoff, end cutoff)` of an instant — see the module docs.
fn cutoffs(t: Instant) -> Option<(i64, i64)> {
    match t {
        Instant::Date(d) => Some((day_start(d) + DAY_US - 1, day_start(d))),
        Instant::Timestamp(ts) => exact_micros(ts).map(|us| (us, us)),
    }
}

/// Which instants share one valid set: rows `from_slots[..started]` have
/// started and rows `to_slots[..ended]` have ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Segment {
    pub(crate) started: usize,
    pub(crate) ended: usize,
}

/// One target's sorted endpoint arrays at one graph version.
#[derive(Debug)]
pub(crate) struct EndpointIndex {
    convention: IntervalConvention,
    from_keys: Vec<i64>,
    from_slots: Vec<u32>,
    to_keys: Vec<i64>,
    to_slots: Vec<u32>,
    empty_slots: Vec<u32>,
}

impl EndpointIndex {
    /// Every governed row, the empty ones included.
    pub(crate) fn rows(&self) -> usize {
        self.from_keys.len() + self.empty_slots.len()
    }

    /// The segment `t` falls in; `None` for an instant finer than a
    /// microsecond.
    pub(crate) fn segment_of(&self, t: Instant) -> Option<Segment> {
        let (start_cutoff, end_cutoff) = cutoffs(t)?;
        let started = self.from_keys.partition_point(|&k| k <= start_cutoff);
        let ended = if self.convention.is_closed() {
            self.to_keys.partition_point(|&k| k < end_cutoff)
        } else {
            self.to_keys.partition_point(|&k| k <= end_cutoff)
        };
        Some(Segment { started, ended })
    }

    pub(crate) fn count(&self, segment: Segment) -> usize {
        segment.started - segment.ended
    }

    /// Rows valid at `t`.
    pub(crate) fn count_at(&self, t: Instant) -> Option<usize> {
        self.segment_of(t).map(|s| self.count(s))
    }

    /// Whether every governed row is valid at `t`, so filtering this target
    /// at `t` removes nothing. `count <= rows - empty` always, so equality
    /// with every row also rules out an empty one.
    pub(crate) fn timeless_at(&self, t: Instant) -> Option<bool> {
        self.segment_of(t).map(|s| self.count(s) == self.rows())
    }

    /// Clear the bit of every governed row not valid in `segment`: rows not
    /// yet started, rows already ended, and empty rows. Bits of rows the
    /// target does not govern are left alone.
    pub(crate) fn clear_invalid(&self, segment: Segment, mask: &mut FixedBitSet) {
        let not_started = &self.from_slots[segment.started..];
        let ended = &self.to_slots[..segment.ended];
        for &slot in not_started.iter().chain(ended).chain(&self.empty_slots) {
            mask.set(slot as usize, false);
        }
    }

    /// Slots of every governed row that is not empty: the ones with a
    /// `from` key, whatever their validity.
    fn nonempty_slots(&self) -> &[u32] {
        &self.from_slots
    }

    fn bytes(&self) -> usize {
        let keys = self.from_keys.capacity() + self.to_keys.capacity();
        let slots =
            self.from_slots.capacity() + self.to_slots.capacity() + self.empty_slots.capacity();
        keys * size_of::<i64>() + slots * size_of::<u32>()
    }
}

/// Why a target has no index; it keeps property guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unindexed {
    /// Disk mode builds none.
    Disk,
    /// A row holds a bound the evaluator cannot read.
    Unreadable,
    /// A bound is finer than a microsecond.
    Unrepresentable,
    /// The arrays would pass the graph's byte cap.
    OverBudget,
}

/// What the walk counted for one target at the current version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TargetCounts {
    pub(crate) rows: usize,
    /// Rows valid on no instant: `from > to`, or `from == to` under
    /// `half_open`.
    pub(crate) empty_rows: usize,
    /// Rows holding a bound that is not NULL, a date, a datetime or an ISO
    /// string.
    pub(crate) unreadable_rows: usize,
}

struct Builder {
    convention: IntervalConvention,
    from: Vec<(i64, u32)>,
    to: Vec<(i64, u32)>,
    empty: Vec<u32>,
    /// Bytes the build may hold at its peak — see [`PEAK_BYTES_PER_PAIR`].
    budget: usize,
}

impl Builder {
    fn new(convention: IntervalConvention, budget: usize) -> Self {
        Builder {
            convention,
            from: Vec::new(),
            to: Vec::new(),
            empty: Vec::new(),
            budget,
        }
    }

    /// The peak bytes the build can reach at `pairs` and `empties` reserved
    /// slots.
    fn peak(pairs: usize, empties: usize) -> usize {
        pairs
            .saturating_mul(PEAK_BYTES_PER_PAIR)
            .saturating_add(empties.saturating_mul(PEAK_BYTES_PER_EMPTY))
    }

    /// The capacity to grow `len` slots to: double, clamped to what the
    /// budget leaves beside `other` bytes; `None` when not one more fits.
    fn grown(&self, len: usize, per_slot: usize, other: usize) -> Option<usize> {
        let fits = self.budget.checked_sub(other)? / per_slot;
        let wanted = len.saturating_mul(2).max(MIN_RESERVE);
        (fits > len).then(|| wanted.min(fits))
    }

    /// Reserve room for one more row, checking the peak bound before any
    /// allocation; refused when the budget cannot hold it.
    fn reserve_one(&mut self, empty: bool) -> Result<(), Unindexed> {
        let pairs = self.from.capacity().max(self.to.capacity());
        if empty {
            if self.empty.len() < self.empty.capacity() {
                return Ok(());
            }
            let other = Self::peak(pairs, 0);
            let cap = self
                .grown(self.empty.len(), PEAK_BYTES_PER_EMPTY, other)
                .ok_or(Unindexed::OverBudget)?;
            self.empty.reserve_exact(cap - self.empty.len());
        } else {
            if self.from.len() < pairs {
                return Ok(());
            }
            let other = Self::peak(0, self.empty.capacity());
            let cap = self
                .grown(self.from.len(), PEAK_BYTES_PER_PAIR, other)
                .ok_or(Unindexed::OverBudget)?;
            self.from.reserve_exact(cap - self.from.len());
            self.to.reserve_exact(cap - self.to.len());
        }
        // `reserve_exact` may hand out more than asked; hold that to the
        // bound as well.
        let reserved = Self::peak(
            self.from.capacity().max(self.to.capacity()),
            self.empty.capacity(),
        );
        if reserved > self.budget {
            return Err(Unindexed::OverBudget);
        }
        Ok(())
    }

    fn push(
        &mut self,
        slot: usize,
        from: Option<Instant>,
        to: Option<Instant>,
        empty: bool,
    ) -> Result<(), Unindexed> {
        let keys = if empty {
            None
        } else {
            let from = from_key(from).ok_or(Unindexed::Unrepresentable)?;
            let to = to_key(to, self.convention).ok_or(Unindexed::Unrepresentable)?;
            Some((from, to))
        };
        self.push_keys(slot, keys)
    }

    /// Add one row by its finished keys — `None` for an empty interval.
    fn push_keys(&mut self, slot: usize, keys: Option<(i64, i64)>) -> Result<(), Unindexed> {
        let slot = u32::try_from(slot).map_err(|_| Unindexed::OverBudget)?;
        self.reserve_one(keys.is_none())?;
        match keys {
            None => self.empty.push(slot),
            Some((from, to)) => {
                self.from.push((from, slot));
                self.to.push((to, slot));
            }
        }
        Ok(())
    }

    /// Sort each side and split it into its kept arrays, one side at a
    /// time, so a side's pair buffer is freed before the next is split.
    fn finish(self) -> EndpointIndex {
        fn split(mut pairs: Vec<(i64, u32)>) -> (Vec<i64>, Vec<u32>) {
            pairs.sort_unstable();
            pairs.into_iter().unzip()
        }
        let (from_keys, from_slots) = split(self.from);
        let (to_keys, to_slots) = split(self.to);
        let mut empty_slots = self.empty;
        empty_slots.shrink_to_fit();
        EndpointIndex {
            convention: self.convention,
            from_keys,
            from_slots,
            to_keys,
            to_slots,
            empty_slots,
        }
    }
}

/// One walk over a target's rows: the counts always, the index when a
/// budget is given and every row fits it.
struct Scan {
    convention: IntervalConvention,
    counts: TargetCounts,
    builder: Result<Builder, Unindexed>,
}

impl Scan {
    fn visit(&mut self, slot: usize, from: &Value, to: &Value) {
        self.counts.rows += 1;
        let Ok((from, to)) = eval::parse_bounds(from, to) else {
            self.counts.unreadable_rows += 1;
            self.builder = Err(Unindexed::Unreadable);
            return;
        };
        let empty =
            matches!((from, to), (Some(f), Some(t)) if !eval::end_admits(t, f, self.convention));
        self.counts.empty_rows += usize::from(empty);
        if let Ok(builder) = &mut self.builder {
            if let Err(reason) = builder.push(slot, from, to, empty) {
                self.builder = Err(reason);
            }
        }
    }

    /// [`Self::visit`] for a row whose bounds are typed timestamp cells, read
    /// as epoch microseconds (`None` is an open bound). Such a bound is never
    /// unreadable, and its key is the cell itself.
    fn visit_micros(&mut self, slot: usize, from: Option<i64>, to: Option<i64>) {
        self.counts.rows += 1;
        let empty = match (from, to) {
            (Some(from), Some(to)) if self.convention.is_closed() => to < from,
            (Some(from), Some(to)) => to <= from,
            _ => false,
        };
        self.counts.empty_rows += usize::from(empty);
        if let Ok(builder) = &mut self.builder {
            let keys = (!empty).then(|| (from.unwrap_or(i64::MIN), to.unwrap_or(i64::MAX)));
            if let Err(reason) = builder.push_keys(slot, keys) {
                self.builder = Err(reason);
            }
        }
    }
}

/// Walk `target`'s rows under `config` — the rows its declaration governs,
/// read as the declaration walk reads them. `budget` is `None` to count
/// only.
fn scan(
    graph: &DirGraph,
    target: &TemporalTarget,
    config: &TemporalConfig,
    budget: Option<usize>,
) -> (TargetCounts, Result<EndpointIndex, Unindexed>) {
    let mut scan = Scan {
        convention: config.convention,
        counts: TargetCounts::default(),
        builder: budget.map_or(Err(Unindexed::Disk), |budget| {
            Ok(Builder::new(config.convention, budget))
        }),
    };
    match target {
        TemporalTarget::Node(label) => {
            let (from_key, to_key) = (
                InternedKey::from_str(&config.valid_from),
                InternedKey::from_str(&config.valid_to),
            );
            // `id`/`title` bounds and disk graphs read through the general path.
            let cells = !graph.graph.is_disk()
                && ![config.valid_from.as_str(), config.valid_to.as_str()]
                    .iter()
                    .any(|field| matches!(*field, "id" | "title"));
            // The two bound columns, resolved once per store: every row of a
            // type shares one, so a row only compares pointers.
            let mut resolved = None;
            for_each_node_row(graph, label, |idx| {
                let micros = cells
                    .then(|| graph.graph.node_view(idx))
                    .flatten()
                    .and_then(|view| {
                        let (store, row) = view.column_row()?;
                        if !resolved
                            .as_ref()
                            .is_some_and(|(seen, _, _)| std::ptr::eq(*seen, store))
                        {
                            resolved = Some((
                                store as *const _,
                                store.timestamp_cells(from_key)?,
                                store.timestamp_cells(to_key)?,
                            ));
                        }
                        let (_, from, to) = resolved.as_ref()?;
                        Some((from.value(row), to.value(row)))
                    });
                if let Some((from, to)) = micros {
                    scan.visit_micros(idx.index(), from, to);
                    return Ok::<(), Infallible>(());
                }
                let from = node_bound(graph, idx, &config.valid_from);
                let to = node_bound(graph, idx, &config.valid_to);
                scan.visit(idx.index(), &from, &to);
                Ok::<(), Infallible>(())
            })
            .unwrap_or_else(|never| match never {});
        }
        TemporalTarget::Relationship {
            rel_type,
            source_type,
        } => {
            let from_key = InternedKey::from_str(&config.valid_from);
            let to_key = InternedKey::from_str(&config.valid_to);
            for_each_edge_row(graph, rel_type, source_type.as_deref(), |row| {
                if let EdgeRow::Edge { id, .. } = row {
                    let from = edge_bound(graph, id, from_key);
                    let to = edge_bound(graph, id, to_key);
                    scan.visit(id.index(), &from, &to);
                }
                Ok::<(), Infallible>(())
            })
            .unwrap_or_else(|never| match never {});
        }
    }
    (scan.counts, scan.builder.map(Builder::finish))
}

/// Node and relationship masks over the graph's index bounds
/// (`node_bound` / `edge_bound`). A bit is clear when an element a resolved
/// target governs is not valid; every other element keeps its bit set, so a
/// node with several declared labels passes only when valid under each.
#[derive(Debug)]
pub(crate) struct ElementMasks {
    pub(crate) nodes: FixedBitSet,
    pub(crate) edges: FixedBitSet,
}

impl ElementMasks {
    fn bytes(&self) -> usize {
        Self::bytes_for(self.nodes.len(), self.edges.len())
    }

    pub(crate) fn bytes_for(nodes: usize, edges: usize) -> usize {
        (nodes.div_ceil(64) + edges.div_ceil(64)) * size_of::<u64>()
    }
}

/// Per resolved target the segment its instant falls in, in template order.
/// Equal keys mean equal masks.
pub(crate) type SegmentKey = Vec<(TemporalTarget, Segment)>;

#[derive(Debug)]
struct CachedTarget {
    target: TemporalTarget,
    config: TemporalConfig,
    counts: TargetCounts,
    index: Result<Arc<EndpointIndex>, Unindexed>,
}

/// The per-graph cache the declaration store carries, fork-private.
/// Everything but `cap` belongs to `version`.
#[derive(Debug, Default)]
pub(crate) struct IndexCache {
    version: u64,
    targets: Vec<CachedTarget>,
    /// Oldest first.
    masks: VecDeque<(SegmentKey, Arc<ElementMasks>)>,
    /// Masks a live valid-time view holds ([`pin_masks`]). The view's `Arc`
    /// keeps a mask alive after the LRU drops it; the weak entry lets a query
    /// whose key the view's covers reuse it ([`is_covered_by`]).
    pinned: Vec<(SegmentKey, Weak<ElementMasks>)>,
    /// [`FilteredCounts::endpoint_invalid`] per segment key, oldest first;
    /// a few `usize`s, so outside the byte cap.
    endpoint_invalid: VecDeque<(SegmentKey, usize)>,
    /// Materialised valid slices, oldest first, under their own byte cap
    /// (see [`super::slice`]).
    slices: VecDeque<(SliceKey, Arc<ValidSlice>)>,
    /// Per node type, its duplicate-id map; `None` when it would pass the
    /// cap.
    duplicates: Vec<(String, Option<Arc<DuplicateIds>>)>,
    /// Disk-mode masks per instant, oldest first, under their own cap (see
    /// [`super::instant`]).
    disk_masks: VecDeque<(Instant, Arc<ElementMasks>)>,
    /// A text index's corpus statistics over the documents visible at an
    /// instant, oldest first (see [`super::instant::masked_text_stats`]).
    text_stats: VecDeque<(TextStatsKey, MaskedStats)>,
    /// Replaces the byte cap for this graph (tests).
    cap: Option<usize>,
}

type Lookup = (TargetCounts, Result<Arc<EndpointIndex>, Unindexed>);

impl IndexCache {
    fn at(&mut self, version: u64) -> &mut Self {
        if self.version != version {
            self.version = version;
            self.clear();
        }
        self
    }

    fn clear(&mut self) {
        self.targets.clear();
        self.masks.clear();
        self.endpoint_invalid.clear();
        self.pinned.clear();
        self.slices.clear();
        self.duplicates.clear();
        self.disk_masks.clear();
        self.text_stats.clear();
    }

    fn find(&self, target: &TemporalTarget, config: &TemporalConfig) -> Option<Lookup> {
        self.targets
            .iter()
            .find(|c| c.target == *target && c.config == *config)
            .map(|c| (c.counts, c.index.clone()))
    }

    /// What the cache keeps beside its masks: the endpoint arrays and the
    /// duplicate-id maps.
    fn array_bytes(&self) -> usize {
        let arrays: usize = self
            .targets
            .iter()
            .filter_map(|c| c.index.as_ref().ok())
            .map(|index| index.bytes())
            .sum();
        let duplicates: usize = self
            .duplicates
            .iter()
            .filter_map(|(_, map)| map.as_deref())
            .map(DuplicateIds::bytes)
            .sum();
        arrays + duplicates
    }

    fn find_duplicates(&self, node_type: &str) -> Option<Option<Arc<DuplicateIds>>> {
        self.duplicates
            .iter()
            .find(|(t, _)| t == node_type)
            .map(|(_, map)| map.clone())
    }

    /// The cached masks, and each pinned mask the LRU no longer holds —
    /// counted once, alive only while its view is.
    fn mask_bytes(&self) -> usize {
        let cached: usize = self.masks.iter().map(|(_, m)| m.bytes()).sum();
        let pinned_only: usize = self
            .pinned
            .iter()
            .filter_map(|(_, weak)| weak.upgrade())
            .filter(|pin| !self.masks.iter().any(|(_, m)| Arc::ptr_eq(m, pin)))
            .map(|pin| pin.bytes())
            .sum();
        cached + pinned_only
    }

    /// A cached or pinned mask whose key covers `key`.
    fn covering_mask(&mut self, key: &SegmentKey) -> Option<Arc<ElementMasks>> {
        if let Some(pos) = self.masks.iter().position(|(k, _)| is_covered_by(key, k)) {
            let entry = self.masks.remove(pos).expect("position is in range");
            let masks = Arc::clone(&entry.1);
            self.masks.push_back(entry);
            return Some(masks);
        }
        self.pinned.retain(|(_, weak)| weak.strong_count() > 0);
        self.pinned
            .iter()
            .filter(|(k, _)| is_covered_by(key, k))
            .find_map(|(_, weak)| weak.upgrade())
    }

    /// Evict cached masks, oldest first, until `extra` more bytes fit the
    /// cap beside what is held. `false`, with nothing evicted, when they
    /// cannot fit even beside no mask.
    fn make_room(&mut self, extra: usize, cap: usize) -> bool {
        let arrays = self.array_bytes();
        if arrays.saturating_add(extra) > cap {
            return false;
        }
        while arrays + self.mask_bytes() + extra > cap {
            if self.masks.pop_front().is_none() {
                return false;
            }
        }
        true
    }

    fn cap(&self) -> usize {
        self.cap
            .or_else(|| std::env::var(BYTE_CAP_ENV).ok()?.parse().ok())
            .unwrap_or(ENDPOINT_INDEX_BYTE_CAP)
    }
}

fn read_cache(graph: &DirGraph) -> RwLockReadGuard<'_, Option<IndexCache>> {
    graph
        .temporal
        .index
        .read()
        .unwrap_or_else(PoisonError::into_inner)
}

fn write_cache(graph: &DirGraph) -> RwLockWriteGuard<'_, Option<IndexCache>> {
    graph
        .temporal
        .index
        .write()
        .unwrap_or_else(PoisonError::into_inner)
}

/// `target`'s counts and index at the graph's version, walking its rows on a
/// miss. The walk runs outside the lock; a racing builder's result wins.
fn lookup(graph: &DirGraph, target: &TemporalTarget, config: &TemporalConfig) -> Lookup {
    let version = graph.version();
    let budget = {
        let read = read_cache(graph);
        let current = read.as_ref().filter(|c| c.version == version);
        if let Some(hit) = current.and_then(|c| c.find(target, config)) {
            return hit;
        }
        let cap = read
            .as_ref()
            .map_or_else(|| IndexCache::default().cap(), IndexCache::cap);
        cap.saturating_sub(current.map_or(0, IndexCache::array_bytes))
    };
    let budget = (!graph.graph.is_disk()).then_some(budget);
    let (counts, built) = scan(graph, target, config, budget);
    let mut write = write_cache(graph);
    let cache = write.get_or_insert_with(IndexCache::default).at(version);
    if let Some(hit) = cache.find(target, config) {
        return hit;
    }
    let index = built.and_then(|index| {
        if cache.make_room(index.bytes(), cache.cap()) {
            Ok(Arc::new(index))
        } else {
            Err(Unindexed::OverBudget)
        }
    });
    cache.targets.push(CachedTarget {
        target: target.clone(),
        config: config.clone(),
        counts,
        index: index.clone(),
    });
    (counts, index)
}

/// What the walk counts for one declaration at the graph's current version
/// (cached per version). Every mode counts, Disk included.
pub(crate) fn target_counts(
    graph: &DirGraph,
    target: &TemporalTarget,
    config: &TemporalConfig,
) -> TargetCounts {
    lookup(graph, target, config).0
}

/// `target`'s index, or why it has none.
pub(crate) fn index(
    graph: &DirGraph,
    target: &TemporalTarget,
    config: &TemporalConfig,
) -> Result<Arc<EndpointIndex>, Unindexed> {
    lookup(graph, target, config).1
}

/// Nodes carrying declared label `label` that are valid at `t`; `None` when
/// the label is undeclared or has no index.
pub(crate) fn node_count_at(graph: &DirGraph, label: &str, t: Instant) -> Option<usize> {
    let config = graph.temporal.node(label)?;
    index(graph, &TemporalTarget::Node(label.to_string()), config)
        .ok()?
        .count_at(t)
}

/// The declaration a template entry was compiled from, when it is still the
/// one in force.
fn template_targets<'g>(
    graph: &'g DirGraph,
    template: &GuardTemplate,
) -> Vec<(TemporalTarget, Option<&'g TemporalConfig>)> {
    let nodes = template.nodes.iter().map(|guard| {
        let config = graph
            .temporal
            .node(&guard.label)
            .filter(|c| GuardBounds::of(c) == guard.bounds);
        (TemporalTarget::Node(guard.label.clone()), config)
    });
    let edges = template.edges.iter().map(|guard| {
        let config = graph
            .temporal
            .edges(&guard.rel_type)
            .iter()
            .find(|c| c.source_type == guard.source_type && GuardBounds::of(c) == guard.bounds);
        let target = TemporalTarget::Relationship {
            rel_type: guard.rel_type.clone(),
            source_type: guard.source_type.clone(),
        };
        (target, config)
    });
    nodes.chain(edges).collect()
}

/// Whether every target of `template` is indexed and timeless at `t` —
/// every row it governs valid then. Builds (and caches) the indexes, never a
/// mask; the session's plain-plan exit asks it over every declaration.
pub(crate) fn template_timeless_at(graph: &DirGraph, template: &GuardTemplate, t: Instant) -> bool {
    template_targets(graph, template)
        .into_iter()
        .all(|(target, config)| {
            config.is_some_and(|config| {
                index(graph, &target, config)
                    .ok()
                    .and_then(|index| index.timeless_at(t))
                    == Some(true)
            })
        })
}

/// A [`GuardTemplate`] resolved against one instant. See
/// [`crate::graph::core::graph_filter::GraphFilter::resolve`].
#[derive(Debug)]
pub(crate) struct ResolvedFilter {
    /// The targets the masks cover, each with its segment. A valid-time
    /// view pins its masks under this key; execution needs only the masks.
    pub(crate) key: SegmentKey,
    /// `None` when no target is indexed, when [`Self::timeless`] holds (the
    /// plain plan answers), or when the masks do not fit the byte cap (their
    /// targets then move to `guarded` and `key` is empty).
    pub(crate) masks: Option<Arc<ElementMasks>>,
    /// Targets the masks do not cover — Disk mode, an unreadable or
    /// sub-microsecond bound, the byte cap, a range selector or a
    /// sub-microsecond instant. These keep property guards.
    pub(crate) guarded: Vec<TemporalTarget>,
    /// Every template target is indexed and every one of its rows is valid
    /// at the instant, so filtering removes nothing.
    pub(crate) timeless: bool,
}

/// Resolve `template` at `selector`: the segment of every indexed target,
/// the masks those segments give (built once per key and cached under the
/// byte cap), the targets left to property guards, and whether the whole
/// template is timeless. Only `AsOf` resolves; a range leaves every target
/// guarded.
pub(crate) fn resolve(
    graph: &DirGraph,
    template: &GuardTemplate,
    selector: ValidTimeSelector,
) -> ResolvedFilter {
    let targets = template_targets(graph, template);
    let ValidTimeSelector::AsOf(t) = selector else {
        let guarded = targets.into_iter().map(|(target, _)| target).collect();
        return ResolvedFilter {
            key: Vec::new(),
            masks: None,
            guarded,
            timeless: false,
        };
    };
    let mut key = SegmentKey::new();
    let mut parts = Vec::new();
    let mut guarded = Vec::new();
    let mut timeless = true;
    for (target, config) in targets {
        let indexed = config.and_then(|config| {
            let index = index(graph, &target, config).ok()?;
            let segment = index.segment_of(t)?;
            Some((index, segment))
        });
        match indexed {
            Some((index, segment)) => {
                timeless &= index.timeless_at(t) == Some(true);
                let edge = matches!(target, TemporalTarget::Relationship { .. });
                key.push((target, segment));
                parts.push((index, segment, edge));
            }
            None => {
                timeless = false;
                guarded.push(target);
            }
        }
    }
    let masks = if timeless || parts.is_empty() {
        None
    } else {
        let masks = cached_masks(graph, &key, &parts);
        if masks.is_none() {
            // No room for the masks: every indexed target keeps its guards.
            guarded.extend(key.drain(..).map(|(target, _)| target));
        }
        masks
    };
    ResolvedFilter {
        key,
        masks,
        guarded,
        timeless,
    }
}

/// What a valid-time filter at one instant removes, for the echo
/// ([`crate::graph::languages::cypher::result::TemporalDiagnostics`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FilteredCounts {
    /// Per indexed target, the rows it governs that are not valid at the
    /// instant, judged by the target's own bounds alone. A target kept on
    /// property guards (Disk mode, an unreadable bound, the byte cap) has no
    /// entry.
    pub(crate) hidden: Vec<(TemporalTarget, usize)>,
    /// Relationships valid by their own bounds whose source or target node is
    /// not valid, so the filter hides them too. `None` when a target of the
    /// template is not indexed, so the nodes' validity cannot be read from
    /// masks.
    pub(crate) endpoint_invalid: Option<usize>,
}

/// The rows `template`'s targets hide at `t`: per target from two binary
/// searches, and the relationships hidden only through an endpoint from one
/// pass over the declared relationships' rows, cached per segment key.
pub(crate) fn filtered_counts(
    graph: &DirGraph,
    template: &GuardTemplate,
    t: Instant,
) -> FilteredCounts {
    let mut counts = FilteredCounts::default();
    let mut key = SegmentKey::new();
    let mut parts = Vec::new();
    let mut complete = true;
    for (target, config) in template_targets(graph, template) {
        let indexed = config.and_then(|config| {
            let index = index(graph, &target, config).ok()?;
            let segment = index.segment_of(t)?;
            Some((index, segment))
        });
        let Some((index, segment)) = indexed else {
            complete = false;
            continue;
        };
        counts
            .hidden
            .push((target.clone(), index.rows() - index.count(segment)));
        let edge = matches!(target, TemporalTarget::Relationship { .. });
        key.push((target, segment));
        parts.push((index, segment, edge));
    }
    if complete {
        counts.endpoint_invalid = Some(endpoint_invalid(graph, &key, &parts));
    }
    counts
}

/// Relationships of `parts`' edge targets valid by their own bounds with an
/// endpoint the node targets of `parts` hide.
fn endpoint_invalid(
    graph: &DirGraph,
    key: &SegmentKey,
    parts: &[(Arc<EndpointIndex>, Segment, bool)],
) -> usize {
    let hides_nodes = parts
        .iter()
        .any(|(index, segment, edge)| !edge && index.count(*segment) != index.rows());
    if !hides_nodes || !parts.iter().any(|(_, _, edge)| *edge) {
        return 0;
    }
    if let Some(hit) = write_cache(graph)
        .get_or_insert_with(IndexCache::default)
        .at(graph.version())
        .endpoint_invalid
        .iter()
        .find_map(|(k, n)| (k == key).then_some(*n))
    {
        return hit;
    }
    let masks = build_masks(graph, parts);
    let mut invalid = 0;
    for (index, _, _) in parts.iter().filter(|(_, _, edge)| *edge) {
        for &slot in index.nonempty_slots() {
            if !masks.edges.contains(slot as usize) {
                continue;
            }
            let Some((source, target)) = graph.graph.edge_endpoints(EdgeIndex::new(slot as usize))
            else {
                continue;
            };
            if !masks.nodes.contains(source.index()) || !masks.nodes.contains(target.index()) {
                invalid += 1;
            }
        }
    }
    let mut write = write_cache(graph);
    let cache = write
        .get_or_insert_with(IndexCache::default)
        .at(graph.version());
    if cache.endpoint_invalid.len() >= MAX_CACHED_INVALID {
        cache.endpoint_invalid.pop_front();
    }
    cache.endpoint_invalid.push_back((key.clone(), invalid));
    invalid
}

/// The masks for `key`, from the cache — an entry or a view's pin whose key
/// covers it — or built and cached under the byte cap; `None` when they cannot fit the cap even with every cached mask
/// evicted, and nothing is allocated or evicted then. Held under the cache
/// lock throughout, so the room made is the room used.
fn cached_masks(
    graph: &DirGraph,
    key: &SegmentKey,
    parts: &[(Arc<EndpointIndex>, Segment, bool)],
) -> Option<Arc<ElementMasks>> {
    let mut write = write_cache(graph);
    let cache = write
        .get_or_insert_with(IndexCache::default)
        .at(graph.version());
    if let Some(masks) = cache.covering_mask(key) {
        return Some(masks);
    }
    let needed = ElementMasks::bytes_for(graph.graph.node_bound(), graph.graph.edge_bound());
    if !cache.make_room(needed, cache.cap()) {
        return None;
    }
    if cache.masks.len() >= MAX_CACHED_MASKS {
        cache.masks.pop_front();
    }
    let masks = Arc::new(build_masks(graph, parts));
    cache.masks.push_back((key.clone(), Arc::clone(&masks)));
    Some(masks)
}

fn build_masks(graph: &DirGraph, parts: &[(Arc<EndpointIndex>, Segment, bool)]) -> ElementMasks {
    let all_set = |len: usize| {
        let mut bits = FixedBitSet::with_capacity(len);
        bits.insert_range(..);
        bits
    };
    let mut masks = ElementMasks {
        nodes: all_set(graph.graph.node_bound()),
        edges: all_set(graph.graph.edge_bound()),
    };
    for (index, segment, edge) in parts {
        let bits = if *edge {
            &mut masks.edges
        } else {
            &mut masks.nodes
        };
        index.clear_invalid(*segment, bits);
    }
    masks
}

/// Drop every cached index and mask. The version moves only when a
/// statement commits, so the write engine calls this after each writing
/// clause: a later read in the same statement walks the rows it wrote.
pub(crate) fn invalidate(graph: &DirGraph) {
    if let Some(cache) = write_cache(graph).as_mut() {
        cache.clear();
    }
}

/// Whether masks built for `cover` may serve a query keyed `key`: every
/// `(target, segment)` of `key` is in `cover`. The extra targets of `cover`
/// only clear elements of targets the query's template does not reach —
/// its scopes cannot bind them (the template already holds every declared
/// label a pattern can reach, secondary labels included), and a
/// relationship type outside it is never traversed. A retrieval filter's
/// template holds every declared node label and its index's relationship
/// type, whose documents are all a retrieval pass walks.
fn is_covered_by(key: &SegmentKey, cover: &SegmentKey) -> bool {
    key.iter().all(|pair| cover.contains(pair))
}

/// Register `masks`, resolved for `key`, as pinned by a live view: while the
/// view holds its `Arc`, a query whose key it covers is served these masks
/// even after the LRU evicts them, and the cap counts them once.
pub(crate) fn pin_masks(graph: &DirGraph, key: &SegmentKey, masks: &Arc<ElementMasks>) {
    let mut write = write_cache(graph);
    let cache = write
        .get_or_insert_with(IndexCache::default)
        .at(graph.version());
    cache.pinned.retain(|(_, weak)| weak.strong_count() > 0);
    if !cache
        .pinned
        .iter()
        .any(|(_, weak)| std::ptr::eq(weak.as_ptr(), Arc::as_ptr(masks)))
    {
        cache.pinned.push((key.clone(), Arc::downgrade(masks)));
    }
}

/// The slice cached for `key` at the graph's version.
pub(crate) fn cached_slice(graph: &DirGraph, key: &SliceKey) -> Option<Arc<ValidSlice>> {
    let mut write = write_cache(graph);
    let cache = write.as_mut().filter(|c| c.version == graph.version())?;
    let pos = cache.slices.iter().position(|(k, _)| k == key)?;
    let entry = cache.slices.remove(pos).expect("position is in range");
    let slice = Arc::clone(&entry.1);
    cache.slices.push_back(entry);
    Some(slice)
}

/// Cache `slice` for `key`, evicting the oldest slices until the slices fit
/// `cap` bytes together; one that alone passes `cap` is not kept.
pub(crate) fn store_slice(graph: &DirGraph, key: SliceKey, slice: &Arc<ValidSlice>, cap: usize) {
    let mut write = write_cache(graph);
    let cache = write
        .get_or_insert_with(IndexCache::default)
        .at(graph.version());
    if slice.bytes() > cap || cache.slices.iter().any(|(k, _)| *k == key) {
        return;
    }
    let held = |slices: &VecDeque<(SliceKey, Arc<ValidSlice>)>| -> usize {
        slices.iter().map(|(_, s)| s.bytes()).sum()
    };
    while held(&cache.slices) + slice.bytes() > cap {
        cache.slices.pop_front();
    }
    cache.slices.push_back((key, Arc::clone(slice)));
}

/// The most Disk-mode instant masks cached at once.
const MAX_DISK_MASKS: usize = 2;
/// The most masked text statistics cached at once (a few dozen bytes each).
const MAX_TEXT_STATS: usize = 32;

/// Which text index, at which generation, as of which instant: what one
/// [`MaskedStats`] was computed for. The graph's version stamp covers the
/// declarations and the elements; the generation covers the index.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextStatsKey {
    pub(crate) relationship: bool,
    pub(crate) owner_type: String,
    pub(crate) property: String,
    pub(crate) generation: u64,
    pub(crate) instant: Instant,
}

/// The Disk-mode masks cached for instant `t` at the graph's version.
pub(crate) fn cached_disk_masks(graph: &DirGraph, t: Instant) -> Option<Arc<ElementMasks>> {
    let read = read_cache(graph);
    let cache = read.as_ref().filter(|c| c.version == graph.version())?;
    cache
        .disk_masks
        .iter()
        .find(|(instant, _)| *instant == t)
        .map(|(_, masks)| Arc::clone(masks))
}

/// Cache Disk-mode `masks` for instant `t`, dropping the oldest past
/// [`MAX_DISK_MASKS`]; a racing builder's entry wins.
pub(crate) fn store_disk_masks(graph: &DirGraph, t: Instant, masks: &Arc<ElementMasks>) {
    let mut write = write_cache(graph);
    let cache = write
        .get_or_insert_with(IndexCache::default)
        .at(graph.version());
    if cache.disk_masks.iter().any(|(instant, _)| *instant == t) {
        return;
    }
    if cache.disk_masks.len() >= MAX_DISK_MASKS {
        cache.disk_masks.pop_front();
    }
    cache.disk_masks.push_back((t, Arc::clone(masks)));
}

/// The statistics cached for `key` at the graph's version, or `build()`'s,
/// computed outside the lock and cached. One entry serves every query at the
/// instant, so `build` must admit by a filter no statement's template shapes:
/// the caller's is the instant's filter over every declared node label and
/// the index's own relationship type (`instant::retrieval_instant_filter`),
/// under which the admitted documents are fixed by the declarations and the
/// elements (the version).
pub(crate) fn text_stats(
    graph: &DirGraph,
    key: TextStatsKey,
    build: impl FnOnce() -> MaskedStats,
) -> MaskedStats {
    let version = graph.version();
    {
        let read = read_cache(graph);
        let hit = read
            .as_ref()
            .filter(|c| c.version == version)
            .and_then(|c| c.text_stats.iter().find(|(k, _)| *k == key));
        if let Some((_, stats)) = hit {
            return *stats;
        }
    }
    let stats = build();
    let mut write = write_cache(graph);
    let cache = write.get_or_insert_with(IndexCache::default).at(version);
    if !cache.text_stats.iter().any(|(k, _)| *k == key) {
        if cache.text_stats.len() >= MAX_TEXT_STATS {
            cache.text_stats.pop_front();
        }
        cache.text_stats.push_back((key, stats));
    }
    stats
}

/// How many slices the graph's cache holds (tests).
#[cfg(test)]
pub(crate) fn cached_slice_count(graph: &DirGraph) -> usize {
    read_cache(graph).as_ref().map_or(0, |c| c.slices.len())
}

/// `node_type`'s duplicate-id map at the graph's version, built on a miss
/// outside the lock (a racing builder's map wins) under what the byte cap
/// leaves; `None` when it does not fit. Every mode builds one: it holds only
/// nodes whose id repeats (see [`super::duplicate_ids`]).
pub(crate) fn duplicate_ids(graph: &DirGraph, node_type: &str) -> Option<Arc<DuplicateIds>> {
    let version = graph.version();
    let budget = {
        let read = read_cache(graph);
        let current = read.as_ref().filter(|c| c.version == version);
        if let Some(hit) = current.and_then(|c| c.find_duplicates(node_type)) {
            return hit;
        }
        let cap = read
            .as_ref()
            .map_or_else(|| IndexCache::default().cap(), IndexCache::cap);
        cap.saturating_sub(current.map_or(0, IndexCache::array_bytes))
    };
    let built = DuplicateIds::build(graph, node_type, budget);
    let mut write = write_cache(graph);
    let cache = write.get_or_insert_with(IndexCache::default).at(version);
    if let Some(hit) = cache.find_duplicates(node_type) {
        return hit;
    }
    let map = built
        .filter(|map| cache.make_room(map.bytes(), cache.cap()))
        .map(Arc::new);
    cache.duplicates.push((node_type.to_string(), map.clone()));
    map
}

/// Replace the byte cap for this graph's endpoint indexes.
#[cfg(test)]
pub(crate) fn set_byte_cap(graph: &DirGraph, cap: usize) {
    write_cache(graph)
        .get_or_insert_with(IndexCache::default)
        .cap = Some(cap);
}

#[cfg(test)]
#[path = "endpoint_index_tests.rs"]
mod tests;
