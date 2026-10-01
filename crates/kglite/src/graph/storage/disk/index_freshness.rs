//! Freshness state for a disk graph's persistent index bundles.
//!
//! A `property_index_*` / `global_index_*` bundle is an mmap snapshot of the
//! graph at build time, and nothing maintains it afterwards: rewriting the
//! `keys`/`offsets`/`ids` files under a live mapping on every `SET` is exactly
//! the unbounded per-write cost the disk backend exists to avoid. So the
//! bundle's *answer contract* carries the maintenance instead — `Some(v)` may
//! only be returned by a bundle that provably covers every live slot, and a
//! bundle that does not declines with `None`, which the matcher already reads
//! as "scan the type".
//!
//! This is the mapped backend's invalidate-and-rebuild-lazily
//! (`MappedGraph::invalidate_property_index`) with the rebuild moved off the
//! read path: a disk rebuild writes files and needs `&mut`, so a reader can
//! only decline. `reindex()` and `save()` are where the rebuild happens.
//!
//! # Why one [`IndexFreshness`] per bundle, plus a baseline
//!
//! [`IndexFreshness`] reads node creation out of the graph's slot bound rather
//! than out of notifications, so per-bundle state is what makes rebuilding one
//! bundle stop lying about the others. But a bundle is opened **lazily**, on
//! the first lookup that wants it, which can be long after the writes it
//! missed. `baseline` is the freshness a bundle would have had if it had been
//! registered at open time — it is notified like a registered bundle and
//! cloned into every lazily discovered one, so a bundle carried in from a
//! published generation inherits every write since the graph was opened
//! instead of being born fresh.
//!
//! # The tracked gate
//!
//! Every write funnel asks [`DiskIndexFreshness::tracks_anything`] first. It is
//! a latch over "does this graph's published generation hold any bundle?" —
//! self-initialising rather than set by each of `DiskGraph`'s constructors,
//! because the answer has to be right *before* the first mutation and a
//! constructor this module does not know about would silently answer `false`.
//! A graph that has never been saved and has no index pays one relaxed load per
//! written row and nothing else. The latch only ever moves towards "yes": a
//! graph built in this process answers "no" at its first write (no generation
//! yet), and the first build must still open the gate for every write after it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

use crate::graph::index_freshness::IndexFreshness;

use super::property_index;

/// Freshness for every persistent bundle a `DiskGraph` may consult.
///
/// Keys mirror the two caches on `DiskGraph`: typed bundles by
/// `(node_type, property)`, global bundles by property.
#[derive(Debug)]
pub(crate) struct DiskIndexFreshness {
    /// Freshness a not-yet-registered bundle inherits — see the module docs.
    baseline: Arc<IndexFreshness>,
    typed: RwLock<HashMap<(String, String), Arc<IndexFreshness>>>,
    global: RwLock<HashMap<String, Arc<IndexFreshness>>>,
    /// Whether any bundle is known to exist: [`UNDECIDED`] until the first
    /// question, then [`NO_BUNDLE`] or [`HAS_BUNDLE`]. Never moves back to
    /// "no": a graph that has held a bundle keeps paying the (single-load)
    /// gate, and clearing it would open the window a lazily-opened legacy
    /// bundle needs.
    tracked: AtomicU8,
}

/// The global bundles a disk save always carries.
const SAVED_GLOBALS: [&str; 2] = ["title", "nid"];

/// Whether a write to `field` can change what the global bundle on `property`
/// holds for a node: the bundle's own column, and the identity fields it falls
/// back to (`title`, its Cypher spelling `name`, `id`).
///
/// `field` is the alias-resolved spelling the write landed in (a declared title
/// or id spelling arrives as `title` / `id`), so a write to any other property
/// leaves the bundle exactly as it was.
fn global_field_matters(property: &str, field: &str) -> bool {
    field == property || matches!(field, "title" | "name" | "id")
}

const UNDECIDED: u8 = 0;
const NO_BUNDLE: u8 = 1;
const HAS_BUNDLE: u8 = 2;

impl DiskIndexFreshness {
    /// Freshness for a graph whose bundles all cover slots below `node_bound`.
    ///
    /// The two global bundles every save carries are registered here rather
    /// than discovered later: a lazily discovered bundle inherits the baseline,
    /// which marks every write because it stands for bundles of unknown
    /// identity, so it could never stay fresh across a write to an unrelated
    /// property (see [`global_field_matters`]).
    pub(crate) fn covering(node_bound: u32) -> Self {
        let baseline = Arc::new(IndexFreshness::covering(node_bound, None));
        let global = SAVED_GLOBALS
            .iter()
            .map(|property| (property.to_string(), Arc::new((*baseline).clone())))
            .collect();
        Self {
            baseline,
            typed: RwLock::new(HashMap::new()),
            global: RwLock::new(global),
            tracked: AtomicU8::new(UNDECIDED),
        }
    }

    /// Whether this graph has any persistent bundle to keep honest — the write
    /// path's whole cost when it does not.
    ///
    /// `data_dir` is the published generation the graph was opened on; a
    /// bundle built later latches the answer through [`Self::mark_tracked`].
    #[inline]
    pub(crate) fn tracks_anything(&self, data_dir: &Path) -> bool {
        match self.tracked.load(Ordering::Relaxed) {
            HAS_BUNDLE => true,
            NO_BUNDLE => false,
            _ => {
                let held = directory_holds_a_bundle(data_dir);
                // A builder may have latched "yes" while the scan ran; it wins.
                let _ = self.tracked.compare_exchange(
                    UNDECIDED,
                    if held { HAS_BUNDLE } else { NO_BUNDLE },
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
                self.tracked.load(Ordering::Relaxed) == HAS_BUNDLE
            }
        }
    }

    /// Latch "this graph has a bundle" without a directory scan — used by the
    /// builders, which have just written one.
    ///
    /// Unconditional: a graph built in this process has already answered "no"
    /// at its first write, and a latch that kept that answer left every write
    /// after the first build unannounced, so a title `SET` after the graph's
    /// first save was served the stale bundle as fresh.
    pub(crate) fn mark_tracked(&self) {
        self.tracked.store(HAS_BUNDLE, Ordering::Relaxed);
    }

    /// A node of `node_type` was created at `slot`.
    pub(crate) fn note_created(&self, slot: u32, node_type: &str) {
        // The baseline stands in for bundles of unknown identity, so every
        // creation is a covered one there — the safe direction.
        self.baseline.note_created(slot, true);
        for (key, freshness) in self.typed.read().unwrap_or_else(|e| e.into_inner()).iter() {
            freshness.note_created(slot, key.0 == node_type);
        }
        for freshness in self
            .global
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            freshness.note_created(slot, true);
        }
    }

    /// A property of the node at `slot` was written. `node_type` is `None` from
    /// a caller that did not resolve it, which marks every typed bundle.
    ///
    /// `field` is the alias-resolved field the write landed in, or `None` from
    /// a caller that wrote a set of fields it did not decompose. It is used for
    /// the global bundles only, whose identity is known: one of them stays
    /// fresh across a write to a field it does not read
    /// ([`global_field_matters`]). A typed bundle records `(node_type,
    /// property)` in the user's spelling, and the alias-resolved `field` cannot
    /// be compared against it without re-resolving per row, so a typed bundle
    /// is marked whatever the field: one extra bundle costs one declined lookup
    /// until the next rebuild; missing one is a wrong answer.
    pub(crate) fn note_property_written(
        &self,
        slot: u32,
        node_type: Option<&str>,
        field: Option<&str>,
    ) {
        self.baseline.note_changed(slot);
        for (key, freshness) in self.typed.read().unwrap_or_else(|e| e.into_inner()).iter() {
            if node_type.is_none_or(|written| written == key.0) {
                freshness.note_changed(slot);
            }
        }
        for (property, freshness) in self.global.read().unwrap_or_else(|e| e.into_inner()).iter() {
            if field.is_none_or(|field| global_field_matters(property, field)) {
                freshness.note_changed(slot);
            }
        }
    }

    /// The node at `slot` was removed.
    ///
    /// Deletion has to mark on its own rather than lean on the tombstone
    /// filter downstream: the filter drops a dead slot, but a slot the graph
    /// hands back out to a node with a *different* indexed value is still
    /// answered from the stale bundle, and one of the matcher's index arms
    /// returns its hits unfiltered.
    pub(crate) fn note_removed(&self, slot: u32) {
        self.note_property_written(slot, None, None);
    }

    /// Whether the typed bundle for `key` covers the graph up to `node_bound`.
    pub(crate) fn typed_is_fresh(&self, key: &(String, String), node_bound: u32) -> bool {
        let registered = {
            let read = self.typed.read().unwrap_or_else(|e| e.into_inner());
            read.get(key).cloned()
        };
        let freshness = match registered {
            Some(freshness) => freshness,
            None => {
                let inherited = Arc::new((*self.baseline).clone());
                let mut write = self.typed.write().unwrap_or_else(|e| e.into_inner());
                Arc::clone(write.entry(key.clone()).or_insert(inherited))
            }
        };
        !freshness.is_stale(node_bound)
    }

    /// Whether the global bundle for `property` covers the graph up to
    /// `node_bound`.
    pub(crate) fn global_is_fresh(&self, property: &str, node_bound: u32) -> bool {
        let registered = {
            let read = self.global.read().unwrap_or_else(|e| e.into_inner());
            read.get(property).cloned()
        };
        let freshness = match registered {
            Some(freshness) => freshness,
            None => {
                let inherited = Arc::new((*self.baseline).clone());
                let mut write = self.global.write().unwrap_or_else(|e| e.into_inner());
                Arc::clone(write.entry(property.to_string()).or_insert(inherited))
            }
        };
        !freshness.is_stale(node_bound)
    }

    /// Record that the typed bundle for `key` was just built over every slot
    /// below `node_bound`.
    pub(crate) fn mark_typed_built(&self, key: (String, String), node_bound: u32) {
        self.mark_tracked();
        self.typed
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Arc::new(IndexFreshness::covering(node_bound, None)));
    }

    /// Global counterpart of [`Self::mark_typed_built`].
    pub(crate) fn mark_global_built(&self, property: &str, node_bound: u32) {
        self.mark_tracked();
        self.global
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                property.to_string(),
                Arc::new(IndexFreshness::covering(node_bound, None)),
            );
    }

    /// Drop a typed bundle's state — the bundle itself is gone.
    pub(crate) fn forget_typed(&self, key: &(String, String)) {
        self.typed
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

impl Clone for DiskIndexFreshness {
    /// Deep, never shared — a clone is another graph, and `DiskGraph::clone`
    /// empties the bundle caches, so its bundles are all re-discovered from the
    /// baseline this copy carries.
    fn clone(&self) -> Self {
        fn deep<K: Clone + Eq + std::hash::Hash>(
            map: &RwLock<HashMap<K, Arc<IndexFreshness>>>,
        ) -> RwLock<HashMap<K, Arc<IndexFreshness>>> {
            let read = map.read().unwrap_or_else(|e| e.into_inner());
            RwLock::new(
                read.iter()
                    .map(|(key, freshness)| (key.clone(), Arc::new((**freshness).clone())))
                    .collect(),
            )
        }
        Self {
            baseline: Arc::new((*self.baseline).clone()),
            typed: deep(&self.typed),
            global: deep(&self.global),
            tracked: AtomicU8::new(self.tracked.load(Ordering::Relaxed)),
        }
    }
}

/// Whether `data_dir` holds any persistent index bundle. One `read_dir` per
/// graph, taken once at the first write.
fn directory_holds_a_bundle(data_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        property_index::is_bundle_file_name(&name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> (String, String) {
        ("Doc".to_string(), "tag".to_string())
    }

    #[test]
    fn a_lazily_discovered_bundle_inherits_the_writes_it_missed() {
        let state = DiskIndexFreshness::covering(4);
        // A write below the watermark lands only in the baseline — nothing is
        // registered yet.
        state.note_property_written(1, Some("Doc"), None);

        assert!(
            !state.typed_is_fresh(&key(), 4),
            "a bundle opened after the write must inherit it"
        );
    }

    #[test]
    fn a_rebuild_clears_only_its_own_bundle() {
        let state = DiskIndexFreshness::covering(4);
        assert!(state.typed_is_fresh(&key(), 4));
        assert!(state.global_is_fresh("title", 4));

        state.note_created(4, "Doc");
        assert!(!state.typed_is_fresh(&key(), 5));
        assert!(!state.global_is_fresh("title", 5));

        state.mark_typed_built(key(), 5);
        assert!(state.typed_is_fresh(&key(), 5));
        assert!(
            !state.global_is_fresh("title", 5),
            "rebuilding one bundle says nothing about another"
        );
    }

    #[test]
    fn a_creation_of_an_uncovered_type_leaves_the_typed_bundle_fresh() {
        let state = DiskIndexFreshness::covering(4);
        assert!(state.typed_is_fresh(&key(), 4), "register the bundle");

        state.note_created(4, "Person");

        assert!(
            state.typed_is_fresh(&key(), 5),
            "bulk-loading another type must not stale this one"
        );
        assert!(
            !state.global_is_fresh("title", 5),
            "a cross-type bundle covers it"
        );
    }

    #[test]
    fn a_removal_stales_every_bundle() {
        let state = DiskIndexFreshness::covering(4);
        assert!(state.typed_is_fresh(&key(), 4));

        state.note_removed(2);

        assert!(!state.typed_is_fresh(&key(), 4));
        assert!(!state.global_is_fresh("title", 4));
    }

    /// The two bundles a save carries read the identity fields and their own
    /// column; a write to any other property leaves them as they were, while a
    /// bundle whose identity is unknown stays marked by every write.
    #[test]
    fn a_saved_global_bundle_is_marked_only_by_a_write_to_a_field_it_reads() {
        for (property, matters) in [
            ("title", ["title", "name", "id", "title"]),
            ("nid", ["nid", "title", "name", "id"]),
        ] {
            for field in matters {
                let state = DiskIndexFreshness::covering(4);
                state.note_property_written(1, Some("Doc"), Some(field));
                assert!(
                    !state.global_is_fresh(property, 4),
                    "{property} must go stale on a write to {field}"
                );
            }
            let state = DiskIndexFreshness::covering(4);
            state.note_property_written(1, Some("Doc"), Some("grade"));
            assert!(
                state.global_is_fresh(property, 4),
                "{property} reads no `grade`"
            );
            state.note_property_written(1, Some("Doc"), None);
            assert!(
                !state.global_is_fresh(property, 4),
                "a write of unknown fields marks it"
            );
        }
        let state = DiskIndexFreshness::covering(4);
        state.note_property_written(1, Some("Doc"), Some("grade"));
        assert!(
            !state.global_is_fresh("label", 4),
            "a bundle of unknown identity inherits every write"
        );
        assert!(
            !state.typed_is_fresh(&key(), 4),
            "a typed bundle is marked whatever the field"
        );
    }

    #[test]
    fn a_build_after_the_first_question_still_opens_the_write_gate() {
        let state = DiskIndexFreshness::covering(4);
        let empty = std::env::temp_dir().join("kglite-no-such-generation-dir");
        assert!(
            !state.tracks_anything(&empty),
            "a graph with no generation holds no bundle"
        );

        state.mark_global_built("title", 4);

        assert!(
            state.tracks_anything(&empty),
            "the first build must be announced to every later write"
        );
    }

    #[test]
    fn a_clone_shares_no_state_with_its_source() {
        let state = DiskIndexFreshness::covering(4);
        assert!(state.typed_is_fresh(&key(), 4));
        let copy = state.clone();

        copy.note_property_written(1, None, None);

        assert!(!copy.typed_is_fresh(&key(), 4));
        assert!(state.typed_is_fresh(&key(), 4), "the source must not move");
    }
}
