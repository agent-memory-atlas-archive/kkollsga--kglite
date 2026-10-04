//! Per-node counts of the relationships a fused grouped count would walk.
//!
//! A grouped count such as `MATCH (a:A)-[:T]->(b:B) RETURN b.p, count(a)`
//! counts, for every node of the grouped side, the admitted `(relationship,
//! peer)` pairs. The in-memory store keeps no per-type adjacency, so each
//! group's count walks every relationship incident to the node. One pass
//! over the relationships of the type yields the same count for every node
//! at once: a [`PeerHist`].
//!
//! A histogram is a function of the graph's elements, the relationship type,
//! the direction, the peer's label constraint and, under a valid-time
//! context, the masks that decide which relationships and endpoints are
//! visible. It is therefore kept where that state is already kept:
//!
//! * under a context, on the [`super::endpoint_index::ElementMasks`] the
//!   context resolved to, so it dies with them (a new version or instant
//!   resolves to new masks);
//! * with no filter, in the endpoint index cache beside the masks, which is
//!   emptied whenever the graph's version moves or a writing clause runs.
//!
//! Building costs one pass, so a histogram is built only once the walks it
//! would replace have cost as much as that pass (see [`HistSlot::due`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use petgraph::graph::NodeIndex;

use crate::graph::schema::InternedKey;

/// The relationship direction a histogram counts from the grouped node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistDir {
    /// Relationships leaving the node.
    Out,
    /// Relationships entering the node.
    In,
    /// Either way; a self-loop counts once.
    Both,
}

/// What a histogram is a function of, besides the graph and its masks. The
/// peer's label constraint is its alternation branches and AND-chain labels,
/// sorted, so equal constraints share one histogram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HistKey {
    pub(crate) conn: InternedKey,
    pub(crate) dir: HistDir,
    pub(crate) alternatives: Vec<String>,
    pub(crate) extras: Vec<String>,
}

impl HistKey {
    pub(crate) fn new(
        conn: InternedKey,
        dir: HistDir,
        alternatives: &[String],
        extras: &[String],
    ) -> Self {
        let sorted = |labels: &[String]| {
            let mut labels = labels.to_vec();
            labels.sort();
            labels.dedup();
            labels
        };
        HistKey {
            conn,
            dir,
            alternatives: sorted(alternatives),
            extras: sorted(extras),
        }
    }
}

/// For every node index, the admitted relationships and peers it has.
#[derive(Debug)]
pub(crate) struct PeerHist {
    counts: Vec<u32>,
}

impl PeerHist {
    pub(crate) fn new(counts: Vec<u32>) -> Self {
        PeerHist { counts }
    }

    #[inline]
    pub(crate) fn get(&self, node: NodeIndex) -> i64 {
        self.counts.get(node.index()).map_or(0, |&n| i64::from(n))
    }

    pub(crate) fn bytes(&self) -> usize {
        self.counts.len() * size_of::<u32>()
    }
}

/// One histogram's lifecycle: what walking in its place has cost so far, and
/// the histogram once built.
#[derive(Debug, Default)]
pub(crate) struct HistSlot {
    walked_ns: AtomicU64,
    hist: OnceLock<Arc<PeerHist>>,
    /// Serialises the builders so a racing pair builds once.
    building: Mutex<()>,
}

impl HistSlot {
    pub(crate) fn built(&self) -> Option<&Arc<PeerHist>> {
        self.hist.get()
    }

    /// Charge `ns` of walking the histogram would have saved.
    pub(crate) fn charge(&self, ns: u64) {
        self.walked_ns.fetch_add(ns, Ordering::Relaxed);
    }

    /// Whether the walks charged so far have cost `threshold_ns`, the price
    /// of building: the standard rent-or-buy rule, so the total spent is at
    /// most twice what the best choice in hindsight would have cost. It
    /// accumulates across statements, so a statement repeated until its walks
    /// add up buys the histogram, and one run once never does.
    pub(crate) fn due(&self, threshold_ns: u64) -> bool {
        self.walked_ns.load(Ordering::Relaxed) >= threshold_ns
    }

    /// The histogram, building it with `build` if no one has yet.
    pub(crate) fn build_once(
        &self,
        build: impl FnOnce() -> Result<PeerHist, String>,
    ) -> Result<Arc<PeerHist>, String> {
        let _guard = self.building.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hist) = self.hist.get() {
            return Ok(Arc::clone(hist));
        }
        let hist = Arc::new(build()?);
        Ok(Arc::clone(self.hist.get_or_init(|| hist)))
    }
}

/// The most histograms one mask set (or the unfiltered cache) holds.
const MAX_SLOTS: usize = 6;

/// The most bytes one histogram may take; a graph whose node bound would pass
/// it never builds one and its counts keep walking. With [`MAX_SLOTS`] it
/// bounds a set, and a mask set's histograms count toward the masks' own
/// byte cap, so the cache evicts them with the masks.
pub(crate) const HIST_BYTE_CAP: usize = 32 << 20;

/// The histograms of one mask set, or of the graph with no filter.
#[derive(Debug, Default)]
pub(crate) struct PeerHistSet {
    slots: Mutex<Vec<(HistKey, Arc<HistSlot>)>>,
}

impl PeerHistSet {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(HistKey, Arc<HistSlot>)>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `key`'s slot, opened on first ask; past [`MAX_SLOTS`] the oldest goes.
    pub(crate) fn slot(&self, key: &HistKey) -> Arc<HistSlot> {
        let mut slots = self.lock();
        if let Some((_, slot)) = slots.iter().find(|(k, _)| k == key) {
            return Arc::clone(slot);
        }
        if slots.len() >= MAX_SLOTS {
            slots.remove(0);
        }
        let slot = Arc::new(HistSlot::default());
        slots.push((key.clone(), Arc::clone(&slot)));
        slot
    }

    /// The bytes the built histograms hold.
    pub(crate) fn bytes(&self) -> usize {
        self.lock()
            .iter()
            .filter_map(|(_, slot)| slot.built())
            .map(|hist| hist.bytes())
            .sum()
    }
}

/// The nanoseconds of walking after which a histogram over `edges`
/// relationships is built: about the price of its one pass, with a floor so
/// a tiny graph never builds for a few microseconds of saved work. The
/// `KGLITE_PEER_HIST_BUILD_AFTER_NS` variable replaces it (tests).
pub(crate) fn build_threshold_ns(edges: usize) -> u64 {
    #[cfg(test)]
    if let Some(ns) = TEST_BUILD_AFTER_NS.with(std::cell::Cell::get) {
        return ns;
    }
    if let Some(ns) = std::env::var(BUILD_AFTER_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return ns;
    }
    (edges as u64).saturating_mul(4).max(50_000)
}

#[cfg(test)]
thread_local! {
    static TEST_BUILD_AFTER_NS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Replace the build threshold on this thread (tests).
#[cfg(test)]
pub(crate) fn set_test_build_after_ns(ns: Option<u64>) {
    TEST_BUILD_AFTER_NS.with(|c| c.set(ns));
}

pub(crate) const BUILD_AFTER_ENV: &str = "KGLITE_PEER_HIST_BUILD_AFTER_NS";

#[cfg(test)]
mod tests {
    use super::*;

    fn key(conn: u64) -> HistKey {
        HistKey::new(
            InternedKey::from_str(&format!("T{conn}")),
            HistDir::Out,
            &[],
            &[],
        )
    }

    #[test]
    fn equal_label_sets_share_a_key_whatever_their_order() {
        let a = HistKey::new(
            InternedKey::from_str("T"),
            HistDir::In,
            &["B".into(), "A".into(), "B".into()],
            &["X".into()],
        );
        let b = HistKey::new(
            InternedKey::from_str("T"),
            HistDir::In,
            &["A".into(), "B".into()],
            &["X".into()],
        );
        assert_eq!(a, b);
    }

    #[test]
    fn a_slot_is_due_once_the_charged_walks_reach_the_price() {
        let slot = HistSlot::default();
        assert!(!slot.due(100));
        slot.charge(60);
        assert!(!slot.due(100));
        slot.charge(40);
        assert!(slot.due(100));
        assert!(slot.due(0));
    }

    #[test]
    fn the_set_keeps_a_bounded_number_of_slots_oldest_out() {
        let set = PeerHistSet::default();
        let first = set.slot(&key(0));
        assert!(Arc::ptr_eq(&first, &set.slot(&key(0))));
        for conn in 1..=MAX_SLOTS as u64 {
            set.slot(&key(conn));
        }
        assert!(!Arc::ptr_eq(&first, &set.slot(&key(0))));
    }

    #[test]
    fn racing_builders_build_once() {
        let slot = HistSlot::default();
        let mut builds = 0;
        for _ in 0..3 {
            slot.build_once(|| {
                builds += 1;
                Ok(PeerHist::new(vec![1, 2]))
            })
            .unwrap();
        }
        assert_eq!(builds, 1);
        assert_eq!(slot.built().unwrap().get(NodeIndex::new(1)), 2);
        assert_eq!(slot.built().unwrap().get(NodeIndex::new(9)), 0);
    }
}
