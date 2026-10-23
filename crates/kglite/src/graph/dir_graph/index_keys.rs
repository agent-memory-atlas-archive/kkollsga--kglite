//! Persisting and restoring `DirGraph`'s declared indexes.
//!
//! The live index stores are `#[serde(skip)]`, so a save snapshots their keys
//! into the declaration lists and a load replays them. Indexes a constraint
//! owns are not in the user lists: the file derives them from
//! `unique_constraint_keys` (`dir_graph::owned_index`). Split out of
//! `dir_graph/mod.rs` for the production-source line ceiling; the stores
//! themselves live in `dir_graph::indexes` and `dir_graph::constraints`.
//!
//! Three of the functions here are the load path's fork: `rebuild_*` builds the
//! declared indexes, `defer_index_rebuild_from_keys` records them without
//! building, and `materialize_indexes` closes the gap. See
//! [`DirGraph::indexes_deferred`] for the invariant the deferred state rests on.

use std::collections::HashMap;

use super::DirGraph;
use crate::graph::schema::{CompositeIndexKey, IndexKey};

/// The keys of a hash map, sorted — the shape every persisted index-key
/// snapshot takes (see [`DirGraph::populate_index_keys`]).
pub(super) fn sorted_keys<K: Ord + Clone, V>(map: &HashMap<K, V>) -> Vec<K> {
    let mut keys: Vec<K> = map.keys().cloned().collect();
    keys.sort_unstable();
    keys
}

impl DirGraph {
    /// Snapshot which property/composite/range indexes exist so they survive
    /// serialization. Called automatically before save.
    ///
    /// Every list is sorted. Each source is a `HashMap` whose iteration order is
    /// reseeded per process, and these lists are read back as sets
    /// (`rebuild_indices_from_keys`, `rebuild_unique_indices_from_keys`), so the
    /// order carries no meaning — imposing one is what makes two saves of the
    /// same graph byte-identical.
    pub fn populate_index_keys(&mut self) {
        // A deferred graph's four maps are empty *by construction*, so
        // snapshotting them would erase every declaration the file carries —
        // and `prune_constraint_names` would drop every constraint name with
        // them. The lists it would rebuild are already exactly the loaded
        // declarations, canonicalized by `defer_index_rebuild_from_keys`, so
        // the save writes the same bytes an eager load would have produced.
        if self.indexes_deferred {
            return;
        }
        self.property_index_keys = self.user_property_index_keys();
        self.composite_index_keys = self.user_composite_index_keys();
        self.range_index_keys = sorted_keys(&self.range_indices);
        self.rel_property_index_keys = self.user_rel_index_keys();
        // The declared UNIQUE constraints are the live list; only its order
        // is imposed here.
        self.unique_constraint_keys.sort_unstable();
        self.unique_constraint_keys.dedup();
        // Constraint *names* cannot be re-derived from the enforcement
        // structures, so unlike the lists above they are maintained live. Prune
        // instead: a name whose declaration is gone must not be saved, or
        // `DROP CONSTRAINT <name>` would resurrect it after a reload.
        self.prune_constraint_names();
        // Index names likewise: a name whose index is gone must not be saved,
        // or the next index built on that property would inherit it.
        crate::graph::introspection::schema_overview::prune_index_names(self);
    }

    /// Whether [`Self::populate_index_keys`] would change anything: its
    /// read-only test, so a writer holding a shared snapshot can skip the copy
    /// a mutation would force.
    pub(crate) fn index_keys_stale(&self) -> bool {
        !self.indexes_deferred
            && (self.property_index_keys != self.user_property_index_keys()
                || self.composite_index_keys != self.user_composite_index_keys()
                || self.range_index_keys != sorted_keys(&self.range_indices)
                || self.rel_property_index_keys != self.user_rel_index_keys()
                || !self
                    .unique_constraint_keys
                    .windows(2)
                    .all(|pair| pair[0] < pair[1])
                || self.has_undeclared_constraint_names()
                || crate::graph::introspection::schema_overview::has_undeclared_index_names(self))
    }

    /// Rebuild property, composite and range indexes from the persisted key
    /// lists. Called automatically after load.
    ///
    /// The indexes unique constraints own are installed last, adopting any the
    /// lists already named. A violation the loaded data already contains does
    /// not fail the load — see [`Self::rebuild_unique_indices_from_keys`] for
    /// why a `.kgl` must always open, and use [`Self::verify_unique_constraints`]
    /// to audit on demand.
    pub fn rebuild_indices_from_keys(&mut self) {
        let prop_keys: Vec<IndexKey> = std::mem::take(&mut self.property_index_keys);
        for (node_type, property) in &prop_keys {
            self.create_index(node_type, property);
        }
        self.property_index_keys = prop_keys;

        // A `.kgl` written before composite keys were canonicalized carries the
        // declaration order; `create_composite_index` sorts it, so re-deriving
        // the list from the rebuilt map keeps the snapshot and the live index
        // agreeing on one spelling instead of persisting the old one again.
        let comp_keys: Vec<CompositeIndexKey> = std::mem::take(&mut self.composite_index_keys);
        for (node_type, properties) in &comp_keys {
            let prop_refs: Vec<&str> = properties.iter().map(|s| s.as_str()).collect();
            self.create_composite_index(node_type, &prop_refs);
        }
        self.composite_index_keys = sorted_keys(&self.composite_indices);

        let range_keys: Vec<IndexKey> = std::mem::take(&mut self.range_index_keys);
        for (node_type, property) in &range_keys {
            self.create_range_index(node_type, property);
        }
        self.range_index_keys = range_keys;

        let rel_keys: Vec<IndexKey> = std::mem::take(&mut self.rel_property_index_keys);
        for (rel_type, property) in &rel_keys {
            self.build_rel_property_index(rel_type, property);
        }
        self.rel_property_index_keys = rel_keys;

        self.rebuild_unique_indices_from_keys();
    }

    /// Record the declared indexes without building them: the deferred
    /// counterpart of [`Self::rebuild_indices_from_keys`], and the only place
    /// that sets [`Self::indexes_deferred`].
    ///
    /// Canonicalizes the four lists to the spelling the eager rebuild would
    /// have persisted — `create_composite_index` sorts a composite key's
    /// property names, and every list is stored sorted — so a deferred load
    /// followed by a save writes the same bytes as an eager one.
    ///
    /// The load path never reported a constraint violation in the loaded data,
    /// so deferring the rebuild drops nothing observable. Enforcement is
    /// unaffected — the first write materializes.
    pub(crate) fn defer_index_rebuild_from_keys(&mut self) {
        self.property_index_keys.sort_unstable();
        self.property_index_keys.dedup();
        for (_, properties) in self.composite_index_keys.iter_mut() {
            properties.sort_unstable();
        }
        self.composite_index_keys.sort_unstable();
        self.composite_index_keys.dedup();
        self.range_index_keys.sort_unstable();
        self.range_index_keys.dedup();
        self.rel_property_index_keys.sort_unstable();
        self.rel_property_index_keys.dedup();
        self.unique_constraint_keys.sort_unstable();
        self.unique_constraint_keys.dedup();
        self.indexes_deferred = true;
    }

    /// Whether this graph's declared indexes are still unbuilt.
    pub fn indexes_deferred(&self) -> bool {
        self.indexes_deferred
    }

    /// Build the indexes a deferred load left declared-but-unbuilt. Returns
    /// `true` when it did work, `false` when there was nothing deferred.
    ///
    /// Idempotent, and safe to call from inside the DDL entry points that
    /// [`Self::rebuild_indices_from_keys`] itself calls: the flag is cleared
    /// *before* the rebuild, so the nested calls see a non-deferred graph.
    ///
    /// Does not bump the version, so a plan cached while the graph was still
    /// deferred can outlive the build. That plan scans where it could now
    /// probe — slower, never wrong, and the callers that matter (a write, a
    /// DDL statement) bump the version themselves.
    pub fn materialize_indexes(&mut self) -> bool {
        if !self.indexes_deferred {
            return false;
        }
        self.indexes_deferred = false;
        self.rebuild_indices_from_keys();
        true
    }
}
