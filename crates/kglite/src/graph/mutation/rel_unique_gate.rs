//! Relationship uniqueness for the bulk relationship loaders.
//!
//! The sibling of the gate in [`super::rel_constraint_gate`]: that one judges
//! each pair's post-merge state against stateless per-property rules, and this
//! one judges the *frame as a whole* against a `UNIQUE` constraint, which is a
//! rule about two relationships. It runs in the same pre-pass — before any row
//! is written — so a refusal leaves the graph and the change log untouched.
//!
//! **What it models.** The frame becomes a set of resulting relationships, one
//! per entry:
//!
//! - [`RowFolding::Independent`]: one new relationship per row.
//! - [`RowFolding::Merging`]: a row folds onto the relationship its endpoint
//!   pair already holds (stored, when `read_stored`) or an earlier row created,
//!   per the conflict mode, and the entry carries that stored relationship as
//!   its `own`, so it never conflicts with itself. Which stored relationship a
//!   pair resolves to is the loader's own rule — the last one the source's
//!   adjacency walk meets — because the walk below is the loader's.
//! - A `replace_connections` frame deletes every stored relationship of the type
//!   leaving the frame's sources first, so those no longer occupy a value.
//!
//! **What it refuses.** Two entries sharing a value (two relationships in the
//! frame), or an entry whose value a stored relationship outside the frame's own
//! entries still holds. A stored relationship the frame moves off the value, or
//! deletes, does not hold it, so a frame that swaps two values is legal.
//!
//! The index is only *read* here; filing happens when the loader writes, as for
//! every other edge write.

use std::collections::{HashMap, HashSet};

use petgraph::graph::EdgeIndex;
use petgraph::Direction;

use super::rel_constraint_gate::{ConnectionBatchGate, RowFolding};
use crate::datatypes::values::Value;
use crate::graph::constraints::{ConstraintKind, ConstraintViolation, EntityKind};
use crate::graph::dir_graph::DirGraph;
use crate::graph::storage::interner::InternedKey;
use crate::graph::storage::GraphRead;

/// The values the groups of one call have claimed so far, by `(type,
/// property)`. A call that writes several groups (`add_edges_from_specs`,
/// `extend_graph`) judges them one after another before writing any, so a
/// value claimed by an earlier group must still refuse a later one.
pub(crate) type UniqueClaims = HashMap<(String, String), HashSet<Value>>;

/// The unique properties' values for one resulting relationship, positional
/// against the frame's unique property list. `None` is absent or null.
type Values = Vec<Option<Value>>;

/// One relationship the frame leaves behind.
struct Entry {
    /// The stored relationship this entry rewrites, if it is not a new one.
    own: Option<EdgeIndex>,
    values: Values,
}

impl ConnectionBatchGate<'_> {
    /// Refuse the frame if it would leave two relationships of the type holding
    /// one value of a declared `UNIQUE` property. A no-op for a type that
    /// declares none.
    pub(crate) fn judge_unique(
        &self,
        graph: &mut DirGraph,
        claims: &mut UniqueClaims,
    ) -> Result<(), String> {
        if !graph.type_has_rel_unique(self.connection_type) {
            return Ok(());
        }
        let properties: Vec<String> = graph
            .rel_unique_properties(self.connection_type)
            .map(str::to_string)
            .collect();
        let columns: Vec<Option<usize>> = properties
            .iter()
            .map(|name| {
                self.property_columns
                    .iter()
                    .find(|(column, _, _)| column == name)
                    .map(|(_, _, index)| *index)
            })
            .collect();
        let entries = self.resulting_entries(graph, &properties, &columns);
        let deleted = self.deleted_edges(graph);
        for (at, property) in properties.iter().enumerate() {
            self.judge_property(graph, property, at, (&entries, &deleted), claims)?;
        }
        Ok(())
    }

    fn resulting_entries(
        &self,
        graph: &DirGraph,
        properties: &[String],
        columns: &[Option<usize>],
    ) -> Vec<Entry> {
        let fresh = |row: usize| Entry {
            own: None,
            values: self.row_values(row, columns),
        };
        if self.folding == RowFolding::Independent || self.start_key.is_some() {
            return self
                .matched
                .iter()
                .map(|(row, ..)| fresh(*row))
                .chain(self.deferred.iter().map(|(row, ..)| fresh(*row)))
                .collect();
        }
        let stored = self.stored_edges(graph, properties);
        let mut matched: HashMap<(usize, usize), Entry> = HashMap::new();
        let mut order: Vec<(usize, usize)> = Vec::new();
        for (row, source, target) in self.matched {
            let key = (source.index(), target.index());
            let already_there = stored.contains_key(&key) || matched.contains_key(&key);
            let state = match matched.get(&key) {
                Some(entry) => entry.values.clone(),
                None => stored
                    .get(&key)
                    .map(|(_, values)| values.clone())
                    .unwrap_or_else(|| vec![None; properties.len()]),
            };
            let values = self.merge_row(*row, columns, state, already_there);
            let own = matched
                .get(&key)
                .map(|entry| entry.own)
                .unwrap_or_else(|| stored.get(&key).map(|(edge, _)| *edge));
            if !matched.contains_key(&key) {
                order.push(key);
            }
            matched.insert(key, Entry { own, values });
        }
        let mut entries: Vec<Entry> = order
            .into_iter()
            .filter_map(|key| matched.remove(&key))
            .collect();
        let mut deferred: HashMap<(Value, Value), Values> = HashMap::new();
        let mut deferred_order: Vec<(Value, Value)> = Vec::new();
        for (row, source_id, target_id) in self.deferred {
            let key = (source_id.clone(), target_id.clone());
            let already_there = deferred.contains_key(&key);
            let state = deferred
                .get(&key)
                .cloned()
                .unwrap_or_else(|| vec![None; properties.len()]);
            if !already_there {
                deferred_order.push(key.clone());
            }
            deferred.insert(key, self.merge_row(*row, columns, state, already_there));
        }
        entries.extend(deferred_order.into_iter().filter_map(|key| {
            deferred
                .remove(&key)
                .map(|values| Entry { own: None, values })
        }));
        entries
    }

    /// The stored relationship each `(source, target)` pair of the frame
    /// resolves to, with the unique values it holds. Read only when the loader
    /// reads stored edges, by the same adjacency walk the loader uses.
    fn stored_edges(
        &self,
        graph: &DirGraph,
        properties: &[String],
    ) -> HashMap<(usize, usize), (EdgeIndex, Values)> {
        let mut stored = HashMap::new();
        if self.folding != (RowFolding::Merging { read_stored: true }) {
            return stored;
        }
        let type_key = InternedKey::from_str(self.connection_type);
        let sources: HashSet<_> = self.matched.iter().map(|(_, source, _)| *source).collect();
        for source in sources {
            for edge in graph.graph.edges_directed(source, Direction::Outgoing) {
                if edge.connection_type() != type_key {
                    continue;
                }
                let values = properties
                    .iter()
                    .map(|property| graph.read_edge_value(edge.id(), property))
                    .collect();
                stored.insert((source.index(), edge.target().index()), (edge.id(), values));
            }
        }
        stored
    }

    /// The stored relationships a `replace_connections` frame deletes before it
    /// writes: every one of the type leaving the frame's sources.
    fn deleted_edges(&self, graph: &DirGraph) -> HashSet<EdgeIndex> {
        let mut deleted = HashSet::new();
        if self.folding != (RowFolding::Merging { read_stored: false }) {
            return deleted;
        }
        let type_key = InternedKey::from_str(self.connection_type);
        let sources: HashSet<_> = self.matched.iter().map(|(_, source, _)| *source).collect();
        for source in sources {
            for edge in graph.graph.edges_directed(source, Direction::Outgoing) {
                if edge.connection_type() == type_key {
                    deleted.insert(edge.id());
                }
            }
        }
        deleted
    }

    fn judge_property(
        &self,
        graph: &mut DirGraph,
        property: &str,
        at: usize,
        (entries, deleted): (&[Entry], &HashSet<EdgeIndex>),
        claims: &mut UniqueClaims,
    ) -> Result<(), String> {
        let rewritten: HashMap<EdgeIndex, Option<&Value>> = entries
            .iter()
            .filter_map(|entry| entry.own.map(|own| (own, entry.values[at].as_ref())))
            .collect();
        let claimed = claims
            .entry((self.connection_type.to_string(), property.to_string()))
            .or_default();
        for entry in entries {
            let Some(value) = entry.values[at].as_ref() else {
                continue;
            };
            let repeated = !claimed.insert(value.clone());
            let held = graph
                .rel_unique_occupant(self.connection_type, property, value, entry.own)
                .filter(|occupant| !deleted.contains(occupant))
                .filter(|occupant| match rewritten.get(occupant) {
                    // The frame moves the occupant off this value.
                    Some(after) => *after == Some(value),
                    None => true,
                })
                .is_some();
            if repeated || held {
                let violation = ConstraintViolation::duplicate(
                    ConstraintKind::Unique,
                    self.connection_type,
                    vec![property.to_string()],
                    vec![value.clone()],
                )
                .on_entity(EntityKind::Relationship);
                return Err(graph.record_constraint_violation(violation));
            }
        }
        Ok(())
    }
}
