//! Per-process cache for **optimized** CypherQuery plans.
//!
//! The sibling [`super::parse_cache`] caches the parsed AST; this caches the
//! post-optimizer plan, which its own comment flagged as the bigger win:
//! "parse + plan accounts for ~80% of small-query cost." The optimizer
//! (`planner::optimize_with_disabled`) re-runs on every call because its
//! output depends on graph state (schema + cardinality). This cache lets a
//! repeated query on an **unchanged graph** skip parse + validate + optimize
//! entirely — the common pattern for a served, read-heavy graph (bolt/mcp)
//! and for any hot read loop.
//!
//! ## Soundness — the key is `(graph_id, version, schema_locked, query)`
//!
//! - `version` changes on **every** mutation (see `DirGraph::bump_version`,
//!   wired into `execute_mut`, the bulk-ingest fns, and `make_dir_graph_mut`),
//!   so a cache hit means the graph is byte-for-byte the same state it was
//!   when the plan was computed → the cached plan is *identical* to
//!   re-optimizing. A mutation bumps `version` → the old key never hits again.
//! - `schema_locked` changes the disposition of unknown node labels and strict
//!   read findings, so unlocked and locked plans are distinct even when a core
//!   caller toggles the public flag without changing `version`.
//! - `graph_id` is process-unique and never reused, so two different graphs
//!   that happen to share a `version` (e.g. both freshly loaded at version 0)
//!   can never collide on each other's plans.
//!
//! **`version` alone is not enough, and that is what `graph_id` is really
//! for.** Two transactions forked from one base bump in lockstep, so they hold
//! *different* graphs at the same version — the key is only unambiguous
//! because `fork_transaction` mints a fresh `graph_id` for a working copy.
//! Before it did, a sibling could be served a plan carrying the other fork's
//! resolved anchor `NodeIndex` and return a wrong count; the case is pinned in
//! `session::plan_cache_cost_tests::a_sibling_fork_is_never_served_another_forks_plan`.
//! Any future clone that becomes an independently mutable lineage owes itself
//! a new id for the same reason.
//!
//! ## Reads only
//!
//! `session::execute::prepare` inserts a plan only for a **non-mutating**
//! statement. A mutation bumps `version` right after its plan would be stored,
//! so the entry is unreachable to that writer forever — a serial writer
//! measured 0 hits in 600 identical writes while filling this whole cache with
//! its own dead entries and evicting other graphs' live read plans. The
//! *lookup* is not skipped (classification does not exist yet at that point in
//! `prepare`; see the comment there), it is simply a guaranteed miss.
//!
//! ## What a hit carries besides the plan
//!
//! The non-fatal schema warnings (`schema_check::collect_unknown_pattern_warnings`
//! — unknown label / relationship type, with a "did you mean?") ride on the
//! entry. They are a pure function of `(query, graph schema)`, and the key
//! already pins the graph state, so a hit hands back exactly what a miss would
//! have computed. Re-deriving them at lookup time is not an option: it needs
//! the parsed AST, and skipping the parse is the whole point of this cache.
//! Not carrying them at all is worse — the *second* run of a typo'd query
//! would silently lose its warning, which is the shape this cache had before
//! `QueryDiagnostics.warnings` was populated.
//!
//! One warning family is the exception, and it is excluded by *not caching the
//! plan at all*: a statement that earned an **absent-property** finding is
//! never inserted, because `lock_schema()` promotes that family to a
//! `SchemaError` and a hit returns before the schema pass could re-decide it.
//! The exclusion is what makes an entry's existence a proof that there was
//! nothing to promote, so locking a graph mid-session cannot be outrun by a
//! plan primed while it was open. See `session::strict_reads_tests`.
//!
//! Only **codec-free, no-disabled-passes, non-`text_score`** queries are cached
//! (see `session::execute::prepare`): with those excluded, the optimized plan
//! is a pure function of `(query, bound parameters, graph state)`. The planner
//! reads parameter values and bakes them into the plan, so the key carries
//! [`param_fingerprint`] and a parameterised statement hits only for the exact
//! values it was planned with; it is looked up and inserted from its second
//! sighting ([`first_sighting`]) and lives in a cache of its own, so churn in
//! parameter values never evicts a parameter-free plan. text_score queries inject per-call embedding params, so
//! they're never inserted (the insert is gated on the post-prepare param map
//! being the caller's own) and therefore never hit.

use super::CypherQuery;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock, RwLock};

/// Maximum cached plans. A served graph cycles through a small working set of
/// queries at a stable version; 512 comfortably covers it. (Larger than the
/// parse cache because each graph-version generation gets its own entries; old
/// generations age out via FIFO as the working set re-populates post-mutation.)
pub(crate) const CACHE_CAPACITY: usize = 512;

/// `(graph_id, version, schema_locked, lazy_eligible, suppress_default,
/// valid_time_default, query_hash, params)`. `params` is [`param_fingerprint`]
/// of the call's bound parameters, `0` for none. `valid_time_default` is the graph's
/// default-instant setting (`ValidTimeDefault::cache_code`): the same text
/// lowers to a different plan under each. `suppress_default` separates the plan of a text prepared
/// with the default valid-time context from the plain plan the session
/// re-prepares of the same text when that context would filter nothing.
/// `lazy_eligible` is part of the key because the cached plan is stored **post lazy-marking** (so a hit is
/// a pure `Arc` clone with no per-call mutation); the wheel runs
/// `lazy_eligible=true`, the bolt/mcp servers `false`, so each gets its own
/// variant. `schema_locked` partitions the two validation dispositions. See the
/// module docs for the complete key contract.
pub(crate) type PlanKey = (u64, u64, bool, bool, bool, i64, u64, u64);

/// Everything but the query text that selects a plan-cache entry. Field
/// meanings are in [`PlanKey`].
#[derive(Clone, Copy)]
pub struct PlanScope {
    pub graph_id: u64,
    pub version: u64,
    pub schema_locked: bool,
    pub lazy: bool,
    pub suppress_default: bool,
    pub valid_time_default: i64,
    pub params: u64,
}

impl PlanScope {
    /// The key of `query` (as its [`hash_query`]) under this scope.
    pub(crate) fn key_with(self, query_hash: u64) -> PlanKey {
        (
            self.graph_id,
            self.version,
            self.schema_locked,
            self.lazy,
            self.suppress_default,
            self.valid_time_default,
            query_hash,
            self.params,
        )
    }
}

/// What a lookup hands back: the ready-to-execute plan plus the schema
/// warnings computed for it (see the module docs). Both are behind `Arc`, so a
/// hit is two refcount bumps and no clone of either payload.
#[derive(Clone)]
pub struct CachedPlan {
    pub plan: Arc<CypherQuery>,
    pub warnings: Arc<[String]>,
}

struct PlanCache {
    capacity: usize,
    /// Plans are stored behind `Arc` so a cache hit is a refcount bump, not a
    /// deep AST clone — execute borrows the plan read-only, so sharing is safe.
    map: HashMap<PlanKey, CachedPlan>,
    /// Insertion order — front = oldest, for FIFO eviction at capacity.
    order: VecDeque<PlanKey>,
}

impl PlanCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            map: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
        }
    }
}

/// Maximum cached plans for parameterised statements, which live in a cache of
/// their own: one entry per distinct set of values, so an application cycling
/// through more values than this evicts only its own entries and never a
/// parameter-free plan.
pub(crate) const PARAM_CACHE_CAPACITY: usize = 256;

static CACHE: OnceLock<RwLock<PlanCache>> = OnceLock::new();
static PARAM_CACHE: OnceLock<RwLock<PlanCache>> = OnceLock::new();

fn cache() -> &'static RwLock<PlanCache> {
    CACHE.get_or_init(|| RwLock::new(PlanCache::new(CACHE_CAPACITY)))
}

fn param_cache() -> &'static RwLock<PlanCache> {
    PARAM_CACHE.get_or_init(|| RwLock::new(PlanCache::new(PARAM_CACHE_CAPACITY)))
}

/// The cache a key belongs to: the key's last field is the parameter
/// fingerprint, `0` for none.
fn cache_for(key: &PlanKey) -> &'static RwLock<PlanCache> {
    if key.7 == 0 {
        cache()
    } else {
        param_cache()
    }
}

pub(crate) fn hash_query(query: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    query.hash(&mut hasher);
    hasher.finish()
}

/// Identity of a call's bound parameters for the cache key: `Some(0)` for none,
/// `Some(non-zero)` for a set built only from scalar and list values, `None`
/// for anything else (maps, graph entities, points), which is never cached.
///
/// The planner reads parameter *values* (index pushdown, anchors, `LIMIT $n`,
/// constant folding) and bakes them into the plan, so a plan is valid only for
/// the exact values it was built from — the fingerprint is part of the key, and
/// two calls share a plan only when every name and value agrees. Floats hash
/// by raw bits, so `0.0` and `-0.0` stay apart.
pub fn param_fingerprint(params: &HashMap<String, crate::datatypes::Value>) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    if params.is_empty() {
        return Some(0);
    }
    fn feed(value: &crate::datatypes::Value, h: &mut impl Hasher) -> bool {
        use crate::datatypes::Value as V;
        match value {
            V::Null => 0u8.hash(h),
            V::Boolean(b) => (1u8, b).hash(h),
            V::UniqueId(n) => (2u8, n).hash(h),
            V::Int64(n) => (3u8, n).hash(h),
            V::Float64(f) => (4u8, f.to_bits()).hash(h),
            V::String(t) => (5u8, t).hash(h),
            V::DateTime(d) => (6u8, d).hash(h),
            V::Duration {
                months,
                days,
                seconds,
                nanos,
            } => (7u8, months, days, seconds, nanos).hash(h),
            V::List(items) => {
                (8u8, items.len()).hash(h);
                return items.iter().all(|item| feed(item, h));
            }
            _ => return false,
        }
        true
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    params.len().hash(&mut hasher);
    if params.len() == 1 {
        let (name, value) = params.iter().next()?;
        name.hash(&mut hasher);
        if !feed(value, &mut hasher) {
            return None;
        }
    } else {
        let mut names: Vec<&String> = params.keys().collect();
        names.sort_unstable();
        for name in names {
            name.hash(&mut hasher);
            if !feed(&params[name], &mut hasher) {
                return None;
            }
        }
    }
    Some(hasher.finish() | 1)
}

#[inline]
fn mix(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29)
}

fn mix_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    let (chunks, rest) = bytes.as_chunks::<8>();
    for chunk in chunks {
        h = mix(h, u64::from_le_bytes(*chunk));
    }
    let mut tail = [0u8; 8];
    tail[..rest.len()].copy_from_slice(rest);
    mix(mix(h, u64::from_le_bytes(tail)), bytes.len() as u64)
}

/// A cheap, non-cryptographic hash of one parameter value, with the same
/// supported set as [`param_fingerprint`]; `None` for an unsupported value.
fn cheap_value(h: u64, value: &crate::datatypes::Value) -> Option<u64> {
    use crate::datatypes::Value as V;
    Some(match value {
        V::Null => mix(h, 1),
        V::Boolean(b) => mix(mix(h, 2), *b as u64),
        V::UniqueId(n) => mix(mix(h, 3), u64::from(*n)),
        V::Int64(n) => mix(mix(h, 4), *n as u64),
        V::Float64(f) => mix(mix(h, 5), f.to_bits()),
        V::String(t) => mix_bytes(mix(h, 6), t.as_bytes()),
        V::List(items) => {
            let mut acc = mix(mix(h, 7), items.len() as u64);
            for item in items {
                acc = cheap_value(acc, item)?;
            }
            acc
        }
        V::DateTime(_) | V::Duration { .. } => {
            mix_bytes(mix(h, 8), format!("{value:?}").as_bytes())
        }
        _ => return None,
    })
}

const SIGHTINGS: usize = 4096;

thread_local! {
    /// Per-thread, so recording a sighting writes no cache line another thread
    /// reads: a shared table measured 10% slower at four threads on a workload
    /// that never repeats a value.
    static SEEN: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// For a call that bound parameters: `Some(true)` the first time this thread prepares this exact
/// `(scope, query, parameter values)` (and records it),
/// `Some(false)` on any later time, `None` when the parameters cannot be part
/// of a key at all.
///
/// A parameterised statement is looked up and cached only from its second
/// sighting: an application using a different id each call would otherwise
/// take the cache's write lock on every call and push the parameter-free plans
/// out, for entries nothing ever reads again — and it skips even the lookup,
/// which cannot hit before the first insertion. The table is direct-mapped,
/// per thread, and keyed by a cheap hash of the parts; the strong hashes of the
/// real key are computed only for a repeat. A repeat is therefore a repeat on
/// the same thread. A collision therefore only delays
/// or hastens one lookup or insertion and never serves a wrong plan, because a
/// hit is decided by the full key.
pub(crate) fn first_sighting(
    scope: PlanScope,
    query: &str,
    params: &HashMap<String, crate::datatypes::Value>,
) -> Option<bool> {
    let mut id = mix(scope.graph_id, scope.version);
    id = mix_bytes(id, query.as_bytes());
    id = mix(
        id,
        (scope.valid_time_default as u64)
            ^ ((scope.schema_locked as u64) << 1)
            ^ ((scope.lazy as u64) << 2)
            ^ ((scope.suppress_default as u64) << 3),
    );
    // Commutative over the map, so no sort and no allocation.
    let mut bound = 0u64;
    for (name, value) in params {
        bound = bound.wrapping_add(cheap_value(mix_bytes(0, name.as_bytes()), value)?);
    }
    let id = mix(id, bound) | 1;
    Some(SEEN.with_borrow_mut(|table| {
        if table.is_empty() {
            table.resize(SIGHTINGS, 0);
        }
        let slot = &mut table[(id >> 20) as usize % SIGHTINGS];
        let first = *slot != id;
        *slot = id;
        first
    }))
}

/// Look up a cached, ready-to-execute plan for `query` against the graph
/// identified by `(graph_id, version, schema_locked)` at the given
/// `lazy_eligible` mode.
/// Returns an `Arc` clone on hit (no AST copy), `None` on miss.
#[cfg(test)]
pub fn get(scope: PlanScope, query: &str) -> Option<CachedPlan> {
    get_by_key(&scope.key_with(hash_query(query)))
}

/// [`get`] for a caller that already built the key, so a miss followed by an
/// insert hashes the query text once.
pub(crate) fn get_by_key(key: &PlanKey) -> Option<CachedPlan> {
    let guard = cache_for(key).read().expect("plan_cache RwLock poisoned");
    let hit = guard.map.get(key).cloned();
    #[cfg(test)]
    instrumentation::record_lookup(hit.is_some());
    hit
}

/// Cache `plan` (the optimized AST, already lazy-marked for `lazy`) plus the
/// schema `warnings` computed for it, for `query` against `(graph_id,
/// version)`. FIFO-evicts the oldest entry at capacity.
#[cfg(test)]
pub fn insert(scope: PlanScope, query: &str, plan: Arc<CypherQuery>, warnings: Arc<[String]>) {
    insert_by_key(scope.key_with(hash_query(query)), plan, warnings);
}

/// [`insert`] for a caller that already built the key.
pub(crate) fn insert_by_key(key: PlanKey, plan: Arc<CypherQuery>, warnings: Arc<[String]>) {
    let mut guard = cache_for(&key).write().expect("plan_cache RwLock poisoned");
    if guard.map.contains_key(&key) {
        return; // benign race: another thread inserted the same key.
    }
    if guard.map.len() >= guard.capacity {
        if let Some(oldest) = guard.order.pop_front() {
            guard.map.remove(&oldest);
            #[cfg(test)]
            instrumentation::record_eviction();
        }
    }
    guard.order.push_back(key);
    guard.map.insert(key, CachedPlan { plan, warnings });
    #[cfg(test)]
    instrumentation::record_insertion();
}

/// Test-only event counters, split by the kind of statement whose `prepare()`
/// caused the event.
///
/// **Why the caller kind cannot simply be passed in.** `prepare()` looks the
/// plan up *before* anything has parsed the query, so at lookup time nobody
/// knows whether the statement mutates; `is_mutation_query` runs on the
/// prepared plan, one line later in `execute_read` / `execute_mut`. So events
/// are buffered per in-flight `prepare()` and attributed retroactively by
/// [`classify_pending`] once the caller knows. A `prepare()` that ends in an
/// error never classifies, and its buffered events are folded into
/// [`CallerStats::unclassified`] by the next [`begin_prepare`] rather than
/// silently landing in the next statement's bucket.
///
/// **Why thread-local and not global.** The cache is process-wide, but `cargo
/// test` runs cases on separate threads: a global counter would interleave
/// unrelated tests and force every counter-reading case onto one lock. Every
/// event is recorded on the thread that caused it, so per-thread totals are
/// exactly "what this test did". Cross-thread interference in the *map* is a
/// non-issue for the same reason the cache is sound at all — `graph_id` is
/// process-unique, so no other test's entries share a key.
#[cfg(test)]
pub mod instrumentation {
    use std::cell::Cell;

    #[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CacheStats {
        /// Calls to [`super::get`].
        pub lookups: u64,
        /// Subset of `lookups` that returned a plan.
        pub hits: u64,
        /// Entries actually added by [`super::insert`] (a benign-race duplicate
        /// key returns early and is not counted).
        pub insertions: u64,
        /// FIFO evictions forced by those insertions.
        pub evictions: u64,
    }

    const EMPTY: CacheStats = CacheStats {
        lookups: 0,
        hits: 0,
        insertions: 0,
        evictions: 0,
    };

    impl CacheStats {
        fn add(self, other: CacheStats) -> CacheStats {
            CacheStats {
                lookups: self.lookups + other.lookups,
                hits: self.hits + other.hits,
                insertions: self.insertions + other.insertions,
                evictions: self.evictions + other.evictions,
            }
        }
    }

    #[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CallerStats {
        /// Events caused by a statement that turned out to be a read.
        pub read: CacheStats,
        /// Events caused by a statement that turned out to be a mutation.
        pub mutation: CacheStats,
        /// Events from a `prepare()` that never reached classification — an
        /// error before `is_mutation_query`, or a direct `get`/`insert` call
        /// from a `plan_cache` unit test.
        pub unclassified: CacheStats,
    }

    thread_local! {
        /// Events of the `prepare()` currently in flight on this thread.
        static PENDING: Cell<CacheStats> = const { Cell::new(EMPTY) };
        static TOTALS: Cell<CallerStats> = const {
            Cell::new(CallerStats { read: EMPTY, mutation: EMPTY, unclassified: EMPTY })
        };
    }

    fn bump(f: impl FnOnce(&mut CacheStats)) {
        PENDING.with(|pending| {
            let mut stats = pending.get();
            f(&mut stats);
            pending.set(stats);
        });
    }

    pub(super) fn record_lookup(hit: bool) {
        bump(|stats| {
            stats.lookups += 1;
            stats.hits += u64::from(hit);
        });
    }

    pub(super) fn record_insertion() {
        bump(|stats| stats.insertions += 1);
    }

    pub(super) fn record_eviction() {
        bump(|stats| stats.evictions += 1);
    }

    fn take_pending() -> CacheStats {
        PENDING.with(|pending| pending.replace(EMPTY))
    }

    /// Open a fresh attribution window. Any events still buffered belong to a
    /// `prepare()` that errored out before classifying, so they are banked as
    /// `unclassified` instead of contaminating this statement.
    pub fn begin_prepare() {
        let leftover = take_pending();
        if leftover != EMPTY {
            TOTALS.with(|totals| {
                let mut all = totals.get();
                all.unclassified = all.unclassified.add(leftover);
                totals.set(all);
            });
        }
    }

    /// Attribute the in-flight `prepare()`'s events now that the caller has
    /// classified the statement.
    pub fn classify_pending(is_mutation: bool) {
        let pending = take_pending();
        TOTALS.with(|totals| {
            let mut all = totals.get();
            if is_mutation {
                all.mutation = all.mutation.add(pending);
            } else {
                all.read = all.read.add(pending);
            }
            totals.set(all);
        });
    }

    /// Zero this thread's counters, including any unclassified in-flight events.
    pub fn reset() {
        take_pending();
        TOTALS.with(|totals| totals.set(CallerStats::default()));
    }

    /// This thread's classified totals since the last [`reset`].
    pub fn totals() -> CallerStats {
        TOTALS.with(|totals| totals.get())
    }
}

/// Serializes every test whose assertion depends on cache *contents*. The
/// cache is a process-wide singleton, so one test's [`clear_for_tests`] (or a
/// capacity test's 600 inserts) would otherwise evict another's entry between
/// its insert and its assert. Shared with `session::plan_cache_cost_tests`.
#[cfg(test)]
pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub fn clear_for_tests() {
    for cache in [cache(), param_cache()] {
        let mut guard = cache.write().expect("plan_cache RwLock poisoned");
        guard.map.clear();
        guard.order.clear();
    }
}

#[cfg(test)]
pub fn entry_count_for_tests() -> usize {
    cache()
        .read()
        .expect("plan_cache RwLock poisoned")
        .map
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::languages::cypher::parser::parse_cypher;

    fn plan(q: &str) -> Arc<CypherQuery> {
        Arc::new(parse_cypher(q).expect("parse"))
    }

    fn scope(
        graph_id: u64,
        version: u64,
        schema_locked: bool,
        lazy: bool,
        suppress_default: bool,
    ) -> PlanScope {
        PlanScope {
            graph_id,
            version,
            schema_locked,
            lazy,
            suppress_default,
            valid_time_default: 0,
            params: 0,
        }
    }

    fn no_warnings() -> Arc<[String]> {
        Vec::new().into()
    }

    #[test]
    fn miss_then_hit_same_key() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_for_tests();
        let q = "MATCH (n:T) RETURN n";
        assert!(
            get(scope(1, 0, false, false, false), q).is_none(),
            "cold miss"
        );
        insert(
            scope(1, 0, false, false, false),
            q,
            plan(q),
            vec!["typo'd label".to_string()].into(),
        );
        let hit = get(scope(1, 0, false, false, false), q).expect("warm hit");
        assert_eq!(
            &*hit.warnings,
            ["typo'd label".to_string()],
            "a hit carries the warnings its miss computed"
        );
    }

    #[test]
    fn version_graph_id_lock_and_lazy_partition_the_key() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_for_tests();
        let q = "MATCH (n:T) RETURN n";
        insert(scope(7, 3, false, false, false), q, plan(q), no_warnings());
        // Same query, different version / graph / lock / lazy-mode → must miss.
        assert!(
            get(scope(7, 4, false, false, false), q).is_none(),
            "version change invalidates"
        );
        assert!(
            get(scope(8, 3, false, false, false), q).is_none(),
            "different graph never collides"
        );
        assert!(
            get(scope(7, 3, true, false, false), q).is_none(),
            "lock is part of the key"
        );
        assert!(
            get(scope(7, 3, false, true, false), q).is_none(),
            "lazy mode is part of the key"
        );
        assert!(
            get(scope(7, 3, false, false, true), q).is_none(),
            "the default-suppressed plan of a text is a different plan"
        );
        assert!(
            get(scope(7, 3, false, false, false), q).is_some(),
            "exact key hits"
        );
    }

    #[test]
    fn evicts_at_capacity() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        clear_for_tests();
        for i in 0..(CACHE_CAPACITY as u64 + 5) {
            insert(
                scope(1, i, false, false, false),
                "MATCH (n:T) RETURN n",
                plan("MATCH (n:T) RETURN n"),
                no_warnings(),
            );
        }
        assert_eq!(entry_count_for_tests(), CACHE_CAPACITY);
    }
}
