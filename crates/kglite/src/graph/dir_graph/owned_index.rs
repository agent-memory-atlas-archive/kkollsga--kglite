//! The index a UNIQUE / NODE KEY constraint owns.
//!
//! A constraint has no structure of its own. Its backing index is an ordinary
//! entry in `property_indices` (one property) or `composite_indices` (several),
//! so everything those families already provide applies unchanged: a fork
//! shares the entry (`LayeredIndex`), the incremental maintenance every write
//! path runs files nodes into it, the undo journal reverses it, and
//! `MATCH (n:L {p: $v})` reads it like any other index. What the constraint
//! adds is enforcement ([`DirGraph::unique_occupant`]) and ownership.
//!
//! # Ownership
//!
//! An index entry is in one of three states:
//!
//! - **user-only** — no constraint names it; `DROP INDEX` removes it.
//! - **owned-only** — a constraint names it and the user never declared it.
//!   It is hidden from the persisted index lists (the file derives it from
//!   `unique_constraint_keys`), listed by `SHOW INDEXES` with its owning
//!   constraint, refuses `DROP INDEX`, and goes with `DROP CONSTRAINT`.
//! - **adopted** — a user declared it and a constraint names it too (the index
//!   came first, or the user asked for it by name afterwards). It is persisted
//!   with the user lists and survives `DROP CONSTRAINT`.
//!
//! Only the adopted set is stored ([`DirGraph::adopted_index_slots`]); the
//! other two follow from the declaration list. A file therefore needs no new
//! field: an old binary reads the constraint keys and builds its own
//! enforcement, and this one rebuilds the owned entry at load.
//!
//! # Probing
//!
//! Enforcement probes the bucket for the claimed tuple under the exact `Value`
//! equality the retired single-occupant map used (`CompositeValue` derives
//! `Eq`/`Hash` over `Value`, as does the single-value index key), so `1` and
//! `1.0` stay two tuples and the equality-key union `MATCH` applies
//! (`predicate_queries::equality_keys`) does not leak into enforcement.
//!
//! The index files a node with no stored `name` under its title, because
//! `MATCH {name: ..}` matches it that way. A constraint has always meant the
//! *stored* property, so a bucket member only occupies a tuple when it stores
//! every property of it ([`DirGraph::unique_occupant`]).

use std::collections::HashSet;

use petgraph::graph::NodeIndex;

use super::DirGraph;
use crate::datatypes::values::Value;
use crate::graph::constraints::{
    descriptor, normalize_properties, ConstraintKind, EntityKind, UniqueConstraintKey,
};
use crate::graph::schema::{
    soft_alias_fallback, CompositeIndexKey, CompositeValue, IndexKey, SoftAliasFallback,
};

/// The index slot a constraint owns: its type and its property names,
/// normalized (sorted, deduplicated) so declaration order does not matter and a
/// composite constraint lands on the composite index's own key.
pub(crate) fn owned_slot(key: &UniqueConstraintKey) -> CompositeIndexKey {
    (key.0.clone(), normalize_properties(&key.1))
}

impl DirGraph {
    /// Whether the slot `key` owns already holds a built index.
    pub(super) fn owned_slot_is_built(&self, key: &UniqueConstraintKey) -> bool {
        let slot = owned_slot(key);
        self.slot_is_built(&slot)
    }

    fn slot_is_built(&self, slot: &CompositeIndexKey) -> bool {
        match slot.1.as_slice() {
            [property] => self
                .property_indices
                .contains_key(&(slot.0.clone(), property.clone())),
            _ => self.composite_indices.contains_key(slot),
        }
    }

    /// Make sure the index `key` owns exists, building it from live data when
    /// it does not. `user_declared` records that a user declared the entry
    /// independently, which keeps it past `DROP CONSTRAINT`.
    ///
    /// Not routed by backend and not logged as a user declaration: the owned
    /// entry is derived state, rebuilt from `unique_constraint_keys`.
    pub(crate) fn install_owned_index(&mut self, key: &UniqueConstraintKey, user_declared: bool) {
        let slot = owned_slot(key);
        let built = self.slot_is_built(&slot);
        if user_declared {
            self.adopted_index_slots.insert(slot.clone());
        }
        if built {
            return;
        }
        match slot.1.as_slice() {
            [property] => {
                self.create_index(&slot.0, property);
            }
            properties => {
                let refs: Vec<&str> = properties.iter().map(String::as_str).collect();
                self.create_composite_index(&slot.0, &refs);
            }
        }
    }

    /// Release the index `key` owned: an owned-only entry is dropped, an
    /// adopted one stays as the plain user index it also was.
    pub(super) fn release_owned_index(&mut self, key: &UniqueConstraintKey) {
        let slot = owned_slot(key);
        if self.adopted_index_slots.remove(&slot) {
            return;
        }
        match slot.1.as_slice() {
            [property] => {
                self.property_indices.remove(&(slot.0, property.clone()));
            }
            _ => {
                self.composite_indices.remove(&slot);
            }
        }
    }

    /// Adopt the user's own declaration of the index on `slot`, when a
    /// constraint owns it. A no-op otherwise.
    pub(crate) fn adopt_owned_slot(&mut self, node_type: &str, properties: &[String]) -> bool {
        let slot = (node_type.to_string(), normalize_properties(properties));
        if self.constraint_for_index(node_type, properties).is_none() {
            return false;
        }
        self.adopted_index_slots.insert(slot);
        true
    }

    /// Whether a constraint owns the index on `(node_type, properties)` and the
    /// user never declared it: the state `DROP INDEX` refuses and a `CREATE
    /// INDEX` of the same equality structure duplicates.
    pub(crate) fn index_is_owned_only(&self, node_type: &str, properties: &[String]) -> bool {
        self.constraint_for_index(node_type, properties).is_some()
            && !self
                .adopted_index_slots
                .contains(&(node_type.to_string(), normalize_properties(properties)))
    }

    /// The node currently occupying `value` under the constraint `key`, if any.
    ///
    /// `value` is in `key`'s declared property order; the composite index is
    /// keyed in sorted order, so the tuple is rearranged to match. The first
    /// bucket member that stores every property of the tuple counts — more than
    /// one member only happens in a file whose data already violated the
    /// constraint, where the first holder keeps the claim.
    pub(crate) fn unique_occupant(
        &self,
        key: &UniqueConstraintKey,
        value: &CompositeValue,
    ) -> Option<NodeIndex> {
        // The single-property case is the hot one (every CREATE and SET on a
        // constrained type probes it), so it skips the normalized slot.
        let single = match key.1.as_slice() {
            [property] => Some(property),
            _ => None,
        };
        let members = if let Some(property) = single {
            let probe = value.0.first()?;
            self.property_indices
                .get(&(key.0.clone(), property.clone()))?
                .get(probe)?
        } else {
            let slot = owned_slot(key);
            let in_declared_order = |property: &String| {
                key.1
                    .iter()
                    .position(|p| p == property)
                    .map(|at| &value.0[at])
            };
            match slot.1.as_slice() {
                [property] => {
                    let probe = in_declared_order(property)?;
                    self.property_indices
                        .get(&(slot.0.clone(), property.clone()))?
                        .get(probe)?
                }
                properties => {
                    let tuple: Vec<Value> = properties
                        .iter()
                        .map(|property| in_declared_order(property).cloned())
                        .collect::<Option<_>>()?;
                    self.composite_indices
                        .get(&slot)?
                        .get(&CompositeValue(tuple))?
                }
            }
        };
        let needs_stored_check = key.1.iter().any(|property| {
            matches!(
                soft_alias_fallback(self.resolve_alias(&key.0, property)),
                Some(SoftAliasFallback::Title)
            )
        });
        if !needs_stored_check {
            return members.first().copied();
        }
        let slot = owned_slot(key);
        members
            .iter()
            .copied()
            .find(|member| self.stores_every_property(&slot, *member))
    }

    /// Whether `node` stores a non-null value for every property of `slot`
    /// (a title-filed `name` is not stored).
    fn stores_every_property(&self, slot: &CompositeIndexKey, node: NodeIndex) -> bool {
        slot.1.iter().all(|property| {
            let reader = self.property_reader_readonly(&slot.0, property);
            !matches!(self.read_indexed(&reader, node), Some(Value::Null) | None)
        })
    }

    /// The constraint whose owned index sits on `(node_type, properties)`, up
    /// to property order. Reads the declaration list directly, so it answers
    /// the same for a deferred load as for a built graph — a listing and a
    /// refusal rule, never a lookup predicate.
    pub(crate) fn constraint_for_index(
        &self,
        node_type: &str,
        properties: &[String],
    ) -> Option<&UniqueConstraintKey> {
        let wanted = normalize_properties(properties);
        self.unique_constraint_keys
            .iter()
            .find(|(nt, props)| nt == node_type && normalize_properties(props) == wanted)
    }

    /// The name `SHOW CONSTRAINTS` gives the constraint `key`: the one its
    /// author registered, else the canonical descriptor.
    pub(crate) fn unique_constraint_display_name(&self, key: &UniqueConstraintKey) -> String {
        self.name_for_constraint(
            EntityKind::Node,
            &key.0,
            &key.1,
            &[ConstraintKind::Unique, ConstraintKind::NodeKey],
        )
        .map(str::to_string)
        .unwrap_or_else(|| descriptor(&key.0, &key.1))
    }

    /// Refuse to drop an index a constraint owns. The error names the
    /// constraint, whose `DROP CONSTRAINT` is the way to remove the index.
    ///
    /// A persistent disk bundle on the same property is the user's own
    /// structure even while a constraint owns the heap entry beside it, so it
    /// is not refused.
    pub fn reject_owned_index_drop(
        &self,
        node_type: &str,
        properties: &[String],
    ) -> Result<(), String> {
        if let [property] = properties {
            if self.has_persistent_property_index(node_type, property) {
                return Ok(());
            }
        }
        let Some(key) = self.constraint_for_index(node_type, properties) else {
            return Ok(());
        };
        let index = match properties {
            [property] => format!("{node_type}.{property}"),
            many => format!("{node_type}.({})", normalize_properties(many).join(",")),
        };
        let constraint = self.unique_constraint_display_name(key);
        Err(format!(
            "index '{index}' backs the constraint '{constraint}' and cannot be dropped on its \
             own. DROP CONSTRAINT {constraint} removes the constraint; an index you declared \
             yourself stays, one the constraint created goes with it."
        ))
    }

    /// Whether the persisted index lists name the slot `key` owns — the load
    /// path's test for "a user declared this index" (see the module doc).
    pub(super) fn persisted_lists_name_slot(&self, key: &UniqueConstraintKey) -> bool {
        let slot = owned_slot(key);
        match slot.1.as_slice() {
            [property] => self
                .property_index_keys
                .contains(&(slot.0.clone(), property.clone())),
            _ => self.composite_index_keys.contains(&slot),
        }
    }

    /// Slots a constraint owns that the user never declared.
    fn owned_only_slots(&self) -> HashSet<CompositeIndexKey> {
        self.unique_constraint_keys
            .iter()
            .map(owned_slot)
            .filter(|slot| !self.adopted_index_slots.contains(slot))
            .collect()
    }

    /// The single-property index keys a save persists: every index except the
    /// owned-only ones, sorted.
    pub(super) fn user_property_index_keys(&self) -> Vec<IndexKey> {
        let owned = self.owned_only_slots();
        let mut keys: Vec<IndexKey> = self
            .property_indices
            .keys()
            .filter(|(nt, property)| {
                owned.is_empty() || !owned.contains(&(nt.clone(), vec![property.clone()]))
            })
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    }

    /// The composite index keys a save persists, owned-only ones excluded.
    pub(super) fn user_composite_index_keys(&self) -> Vec<CompositeIndexKey> {
        let owned = self.owned_only_slots();
        let mut keys: Vec<CompositeIndexKey> = self
            .composite_indices
            .keys()
            .filter(|slot| !owned.contains(*slot))
            .cloned()
            .collect();
        keys.sort_unstable();
        keys
    }

    /// Owned-only slots a deferred load has declared but not built, as
    /// `(node_type, properties)`. The deferred listing shows them beside the
    /// persisted user indexes so `SHOW INDEXES` reports the same rows before
    /// and after materialization.
    pub(super) fn deferred_owned_slots(&self) -> Vec<CompositeIndexKey> {
        let mut slots: Vec<CompositeIndexKey> = self
            .unique_constraint_keys
            .iter()
            .filter(|key| !self.persisted_lists_name_slot(key))
            .map(owned_slot)
            .collect();
        slots.sort_unstable();
        slots.dedup();
        slots
    }
}

#[cfg(test)]
impl DirGraph {
    /// Every `(constraint, tuple, node slot)` the owned indexes file, sorted:
    /// the oracle for what the retired single-occupant map held. Reads the
    /// buckets directly, so a title-filed member shows up here even though
    /// [`Self::unique_occupant`] would skip it.
    pub(crate) fn unique_occupancy(&self) -> Vec<(UniqueConstraintKey, String, usize)> {
        let mut out = Vec::new();
        for key in &self.unique_constraint_keys {
            let slot = owned_slot(key);
            match slot.1.as_slice() {
                [property] => {
                    if let Some(index) = self
                        .property_indices
                        .get(&(slot.0.clone(), property.clone()))
                    {
                        for (value, members) in index.iter() {
                            for member in members {
                                out.push((
                                    key.clone(),
                                    format!("{:?}", CompositeValue(vec![value.clone()])),
                                    member.index(),
                                ));
                            }
                        }
                    }
                }
                _ => {
                    if let Some(index) = self.composite_indices.get(&slot) {
                        for (value, members) in index.iter() {
                            for member in members {
                                out.push((key.clone(), format!("{value:?}"), member.index()));
                            }
                        }
                    }
                }
            }
        }
        out.sort();
        out
    }
}
