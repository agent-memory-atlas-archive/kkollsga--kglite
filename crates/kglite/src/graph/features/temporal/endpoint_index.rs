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
//! cold, and a version set directly empties the cache. A target keeps
//! property guards instead of an index when a row holds a bound the
//! evaluator cannot read (a mask cannot raise that error only when the row is
//! visited), or when its arrays would pass [`ENDPOINT_INDEX_BYTE_CAP`], which
//! bounds one graph's arrays and cached masks together: a build over it is
//! refused, and cached masks are evicted oldest first to make room. Disk mode
//! never builds an index (its heap must not grow with the graph); the walk
//! still counts the rows for `db.temporal.declarations()`.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, PoisonError, RwLockReadGuard, RwLockWriteGuard};

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use fixedbitset::FixedBitSet;

use super::declarations::TemporalTarget;
use super::eval::{self, Instant, IntervalConvention};
use super::validate::{edge_bound, for_each_edge_row, for_each_node_row, node_bound, EdgeRow};
use crate::datatypes::values::Value;
use crate::graph::core::graph_filter::{GuardBounds, GuardTemplate, ValidTimeSelector};
use crate::graph::dir_graph::DirGraph;
use crate::graph::schema::{InternedKey, TemporalConfig};
use crate::graph::storage::GraphRead;

/// The bytes one graph's endpoint arrays and cached masks may hold together.
/// A per-graph stand-in for a shared cache budget; [`BYTE_CAP_ENV`]
/// overrides it.
pub(crate) const ENDPOINT_INDEX_BYTE_CAP: usize = 128 << 20;

/// Environment variable that replaces [`ENDPOINT_INDEX_BYTE_CAP`] (a byte
/// count), read at each build.
pub(crate) const BYTE_CAP_ENV: &str = "KGLITE_TEMPORAL_INDEX_MAX_BYTES";

const DAY_US: i64 = 86_400_000_000;
/// `NaiveDate::num_days_from_ce` of 1970-01-01.
const EPOCH_DAYS_FROM_CE: i64 = 719_163;
/// A collected row's `(key, slot)` pair on each side, as the build holds it
/// while walking — more than the 24 bytes the finished arrays keep, so the
/// cap bounds the walk's working set too (splitting the sorted pairs into
/// the kept arrays briefly holds both).
const BYTES_PER_ROW: usize = 2 * size_of::<(i64, u32)>();
/// The most masks cached at once; the byte cap may hold fewer.
const MAX_CACHED_MASKS: usize = 8;

fn day_start(date: NaiveDate) -> i64 {
    (i64::from(date.num_days_from_ce()) - EPOCH_DAYS_FROM_CE) * DAY_US
}

/// A timestamp in microseconds; `None` when it is finer than that (or a
/// leap second), which no key can hold exactly.
fn micros(ts: NaiveDateTime) -> Option<i64> {
    let nanos = ts.nanosecond();
    if !nanos.is_multiple_of(1_000) || nanos >= 1_000_000_000 {
        return None;
    }
    let of_day = i64::from(ts.num_seconds_from_midnight()) * 1_000_000 + i64::from(nanos / 1_000);
    Some(day_start(ts.date()) + of_day)
}

fn from_key(from: Option<Instant>) -> Option<i64> {
    match from {
        None => Some(i64::MIN),
        Some(Instant::Date(d)) => Some(day_start(d)),
        Some(Instant::Timestamp(ts)) => micros(ts),
    }
}

fn to_key(to: Option<Instant>, convention: IntervalConvention) -> Option<i64> {
    match to {
        None => Some(i64::MAX),
        Some(Instant::Date(d)) if convention.is_closed() => Some(day_start(d) + DAY_US - 1),
        Some(Instant::Date(d)) => Some(day_start(d)),
        Some(Instant::Timestamp(ts)) => micros(ts),
    }
}

/// `(start cutoff, end cutoff)` of an instant — see the module docs.
fn cutoffs(t: Instant) -> Option<(i64, i64)> {
    match t {
        Instant::Date(d) => Some((day_start(d) + DAY_US - 1, day_start(d))),
        Instant::Timestamp(ts) => micros(ts).map(|us| (us, us)),
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
    /// Bytes the collected rows may reach.
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

    fn push(
        &mut self,
        slot: usize,
        from: Option<Instant>,
        to: Option<Instant>,
        empty: bool,
    ) -> Result<(), Unindexed> {
        let slot = u32::try_from(slot).map_err(|_| Unindexed::OverBudget)?;
        let bytes =
            (self.from.len() + 1) * BYTES_PER_ROW + (self.empty.len() + 1) * size_of::<u32>();
        if bytes > self.budget {
            return Err(Unindexed::OverBudget);
        }
        if empty {
            self.empty.push(slot);
            return Ok(());
        }
        let from = from_key(from).ok_or(Unindexed::Unrepresentable)?;
        let to = to_key(to, self.convention).ok_or(Unindexed::Unrepresentable)?;
        self.from.push((from, slot));
        self.to.push((to, slot));
        Ok(())
    }

    fn finish(mut self) -> EndpointIndex {
        self.from.sort_unstable();
        self.to.sort_unstable();
        let (from_keys, from_slots) = self.from.into_iter().unzip();
        let (to_keys, to_slots) = self.to.into_iter().unzip();
        EndpointIndex {
            convention: self.convention,
            from_keys,
            from_slots,
            to_keys,
            to_slots,
            empty_slots: self.empty,
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
            for_each_node_row(graph, label, |idx| {
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
        (self.nodes.len().div_ceil(64) + self.edges.len().div_ceil(64)) * size_of::<u64>()
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
    /// Replaces the byte cap for this graph (tests).
    cap: Option<usize>,
}

type Lookup = (TargetCounts, Result<Arc<EndpointIndex>, Unindexed>);

impl IndexCache {
    fn at(&mut self, version: u64) -> &mut Self {
        if self.version != version {
            self.version = version;
            self.targets.clear();
            self.masks.clear();
        }
        self
    }

    fn find(&self, target: &TemporalTarget, config: &TemporalConfig) -> Option<Lookup> {
        self.targets
            .iter()
            .find(|c| c.target == *target && c.config == *config)
            .map(|c| (c.counts, c.index.clone()))
    }

    fn array_bytes(&self) -> usize {
        self.targets
            .iter()
            .filter_map(|c| c.index.as_ref().ok())
            .map(|index| index.bytes())
            .sum()
    }

    fn mask_bytes(&self) -> usize {
        self.masks.iter().map(|(_, m)| m.bytes()).sum()
    }

    /// Evict cached masks, oldest first, until `extra` more bytes fit the
    /// cap beside what is held. `false` when they cannot fit.
    fn make_room(&mut self, extra: usize, cap: usize) -> bool {
        let arrays = self.array_bytes();
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

/// A [`GuardTemplate`] resolved against one instant. See
/// [`crate::graph::core::graph_filter::GraphFilter::resolve`].
// Read only by tests until guarded execution consumes a resolved filter;
// the first production reader removes this allowance.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ResolvedFilter {
    /// The targets the masks cover, each with its segment.
    pub(crate) key: SegmentKey,
    /// `None` when no target is indexed, or when [`Self::timeless`] holds
    /// (the plain plan answers).
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
    let masks = (!timeless && !parts.is_empty()).then(|| cached_masks(graph, &key, &parts));
    ResolvedFilter {
        key,
        masks,
        guarded,
        timeless,
    }
}

fn cached_masks(
    graph: &DirGraph,
    key: &SegmentKey,
    parts: &[(Arc<EndpointIndex>, Segment, bool)],
) -> Arc<ElementMasks> {
    let version = graph.version();
    {
        let mut write = write_cache(graph);
        let cache = write.get_or_insert_with(IndexCache::default).at(version);
        if let Some(pos) = cache.masks.iter().position(|(k, _)| k == key) {
            let entry = cache.masks.remove(pos).expect("position is in range");
            let masks = Arc::clone(&entry.1);
            cache.masks.push_back(entry);
            return masks;
        }
    }
    let masks = Arc::new(build_masks(graph, parts));
    let mut write = write_cache(graph);
    let cache = write.get_or_insert_with(IndexCache::default).at(version);
    if cache.masks.len() >= MAX_CACHED_MASKS {
        cache.masks.pop_front();
    }
    if cache.make_room(masks.bytes(), cache.cap()) {
        cache.masks.push_back((key.clone(), Arc::clone(&masks)));
    }
    masks
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
