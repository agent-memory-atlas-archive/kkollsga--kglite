//! A lazily built id index that every version of one lineage can reuse.
//!
//! ## Why
//!
//! A published version is cut from the writer's graph, never from a reader's
//! snapshot. An id index a reader builds for a type the writer never looks up
//! was therefore cached only in that snapshot, and the next version started
//! cold again: under a steady write stream every query paid the O(N) build
//! (1.5-3 ms at 160k nodes against 0.2 ms warm). The slot lives in the
//! `IdIndexStore` and is shared by `Clone`, so a build on any version is
//! visible to all of them.
//!
//! ## When an entry may be reused
//!
//! An entry is tagged with the type's member-bucket generation
//! (`TypeIndexStore::generation`) at build time and served only to a graph
//! whose generation for that type is equal. Generations are process-unique, so
//! equal means no create, delete or rollback separates the two graphs' member
//! lists, and ids are immutable: the index is the same. A graph that has
//! edited the type, or an older snapshot of it, holds a different value and
//! builds its own.
//!
//! ## Memory and concurrency
//!
//! One entry per type, the latest published; the writer drops a type's entry
//! when it edits that type (its own copy is maintained incrementally from
//! then on). No lock is held during a build: two builders racing publish equal
//! content and the later one wins, costing one extra build.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::id_index_layer::TypeEntry;
use crate::graph::schema::TypeIdIndex;

/// The shared slot; see the module doc.
#[derive(Debug, Default)]
pub(crate) struct SharedIdIndexes {
    entries: RwLock<HashMap<String, (u64, Arc<TypeEntry>)>>,
}

impl SharedIdIndexes {
    /// The published index for `name` when it was built at `generation`.
    pub(crate) fn get(&self, name: &str, generation: u64) -> Option<Arc<TypeEntry>> {
        let entries = self.entries.read().unwrap();
        entries
            .get(name)
            .filter(|(built_at, _)| *built_at == generation)
            .map(|(_, entry)| Arc::clone(entry))
    }

    /// The index for `name` at `generation`: the published one, or `build()`
    /// published in its place. The build runs outside the lock.
    pub(crate) fn get_or_publish(
        &self,
        name: &str,
        generation: u64,
        build: impl FnOnce() -> TypeIdIndex,
    ) -> Arc<TypeEntry> {
        if let Some(shared) = self.get(name, generation) {
            return shared;
        }
        let built = Arc::new(TypeEntry::from(build()));
        self.entries
            .write()
            .unwrap()
            .insert(name.to_string(), (generation, Arc::clone(&built)));
        built
    }

    /// `name` was edited by the graph that owns this call: its published entry
    /// describes a state that graph has left.
    pub(crate) fn forget(&self, name: &str) {
        if self.entries.read().unwrap().contains_key(name) {
            self.entries.write().unwrap().remove(name);
        }
    }

    pub(crate) fn forget_all(&self) {
        let mut entries = self.entries.write().unwrap();
        if !entries.is_empty() {
            entries.clear();
        }
    }
}
