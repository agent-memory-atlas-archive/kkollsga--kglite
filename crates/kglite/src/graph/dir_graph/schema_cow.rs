//! Copy-on-write for the schema-scale maps, and the accessors that write them.
//!
//! ## Why these six maps are `Arc`
//!
//! Statement rollback takes a `schema_shell` — a `DirGraph::clone()` with the
//! ten O(V+E) fields parked — before **every** mutating statement, and that
//! clone used to deep-copy the whole property catalogue. Measured on a
//! 200-type × 50-column schema (release, 2026-08-14): 337 µs per shell, of
//! which `node_type_metadata` was 326 µs, `type_schemas` 10.0 µs and
//! `title_field_aliases` 4.6 µs — paid by every statement, including one that
//! matched nothing and wrote nothing. Node count does not enter it; schema
//! width does.
//!
//! Behind an `Arc` the shell's copy is six refcount bumps. The maps then
//! follow the same discipline the columnar cell journal follows one level
//! down:
//!
//! - The shell holds the pristine handle, so the statement's **first** write to
//!   a given map forks it once — O(that map), and only on a statement that
//!   actually changes the schema, which after warmup is rare.
//! - Every later write in the same statement sees a uniquely-owned map and
//!   mutates it in place.
//! - **Rollback** swaps the pristine handle back (`restore_schema_shell`);
//!   **commit** drops the shell and uniqueness returns.
//!
//! The correctness argument is the same one that lets the shell restore work
//! at all: these maps are restored *verbatim*, so a pointer to the
//! pre-statement value is exactly as good as a copy of it.
//!
//! ## The rule for writers
//!
//! Every mutation goes through the `*_mut` accessor below — never through
//! `Arc::make_mut` at the call site — because that is where the fork counter
//! lives, and the counter is what keeps the cost model testable.
//!
//! And a writer that can determine it changes nothing must **not** call the
//! accessor: taking `&mut` forks the map whether or not anything is written,
//! so a metadata upsert that re-declares keys the catalogue already holds pays
//! the whole copy for a no-op. `upsert_node_type_metadata`,
//! `upsert_connection_type_metadata` and `ensure_type_schema_keys` each check
//! first for exactly that reason — the Cypher `SET` path calls all three once
//! per written row.

use std::collections::HashMap;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use super::DirGraph;
use crate::graph::cow::cow_mut;
use crate::graph::schema::{ConnectionTypeInfo, TypeSchema};

impl DirGraph {
    /// Writable `node_type_metadata` — forks it if the rollback shell (or a
    /// fork, or a held view) is holding the pre-write value.
    pub fn node_type_metadata_mut(&mut self) -> &mut HashMap<String, HashMap<String, String>> {
        cow_mut(&mut self.node_type_metadata)
    }

    /// Writable `connection_type_metadata`; see
    /// [`Self::node_type_metadata_mut`].
    pub fn connection_type_metadata_mut(&mut self) -> &mut HashMap<String, ConnectionTypeInfo> {
        cow_mut(&mut self.connection_type_metadata)
    }

    /// Writable `type_schemas`; see [`Self::node_type_metadata_mut`].
    ///
    /// Doubly shared: the outer map is CoW here, and each `Arc<TypeSchema>`
    /// inside it is CoW at its own write sites, so growing one type's key list
    /// copies that type's schema and no other's.
    pub fn type_schemas_mut(&mut self) -> &mut HashMap<String, Arc<TypeSchema>> {
        cow_mut(&mut self.type_schemas)
    }

    /// Writable `id_field_aliases`; see [`Self::node_type_metadata_mut`].
    pub fn id_field_aliases_mut(&mut self) -> &mut FxHashMap<String, String> {
        cow_mut(&mut self.id_field_aliases)
    }

    /// Writable `title_field_aliases`; see [`Self::node_type_metadata_mut`].
    pub fn title_field_aliases_mut(&mut self) -> &mut FxHashMap<String, String> {
        cow_mut(&mut self.title_field_aliases)
    }

    /// Record `field` as `node_type`'s id spelling. A title spelling equal to it
    /// is dropped: see [`Self::declare_title_field_alias`].
    pub(crate) fn declare_id_field_alias(&mut self, node_type: &str, field: &str) {
        if self.id_field_aliases.get(node_type).map(String::as_str) != Some(field) {
            self.id_field_aliases_mut()
                .insert(node_type.to_string(), field.to_string());
        }
        if self.title_field_aliases.get(node_type).map(String::as_str) == Some(field) {
            self.title_field_aliases_mut().remove(node_type);
        }
    }

    /// Record `field` as `node_type`'s title spelling — unless it names the
    /// type's identity (`id`, or the declared id spelling), which records
    /// nothing.
    ///
    /// A title column that *is* the id column (`add_nodes(df, 'A', 'id', 'id')`)
    /// means what omitting the title field means: the title is filled from the
    /// id column. Registering the spelling as a title alias instead made the
    /// identity's own name resolve to the title slot wherever the maps are
    /// consulted — `MERGE (a:A {id: 'n1'})` probed titles and created a twin on
    /// every run, and the twin read back its fabricated `A_<n>` title as
    /// `a.id` — while the id index and `MATCH {id: …}` still answered by id.
    pub(crate) fn declare_title_field_alias(&mut self, node_type: &str, field: &str) {
        if !self.names_identity(node_type, field)
            && self.title_field_aliases.get(node_type).map(String::as_str) != Some(field)
        {
            self.title_field_aliases_mut()
                .insert(node_type.to_string(), field.to_string());
        }
    }

    /// Whether `field` names `node_type`'s identity: `id`, or its id spelling.
    pub(crate) fn names_identity(&self, node_type: &str, field: &str) -> bool {
        field == "id" || self.id_field_aliases.get(node_type).map(String::as_str) == Some(field)
    }

    /// Read-compat: drop every title spelling that names its type's identity,
    /// which [`Self::declare_title_field_alias`] refuses to record but graphs
    /// saved or logged before it did may carry. Their nodes' ids were stored
    /// correctly; only the spelling map misrouted reads, so dropping it is the
    /// whole repair. Returns the affected node types.
    pub(crate) fn drop_identity_shadowing_title_aliases(&mut self) -> Vec<String> {
        let shadowing: Vec<String> = self
            .title_field_aliases
            .iter()
            .filter(|(node_type, field)| self.names_identity(node_type, field))
            .map(|(node_type, _)| node_type.clone())
            .collect();
        if !shadowing.is_empty() {
            let aliases = self.title_field_aliases_mut();
            for node_type in &shadowing {
                aliases.remove(node_type);
            }
        }
        shadowing
    }

    /// Writable `parent_types`; see [`Self::node_type_metadata_mut`].
    pub fn parent_types_mut(&mut self) -> &mut HashMap<String, String> {
        cow_mut(&mut self.parent_types)
    }
}
