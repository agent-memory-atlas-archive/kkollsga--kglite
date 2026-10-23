//! User-chosen index names.
//!
//! An index is addressed by its canonical name (`Label.property`,
//! `Label.(a,b)`, `relationship:Type.property`), derived from what it indexes.
//! `CREATE INDEX <name> …` and the BM25 / vector builders can also give it a
//! name of the author's choosing, which `SHOW INDEXES` then lists, `DROP INDEX`
//! accepts and the `db.index.*` search procedures resolve — the same lookup aid
//! `constraint_names` is for constraints, never the source of truth: the index
//! lives in its own structure, and a name whose structure is gone is ignored by
//! every reader and pruned on save.
//!
//! One namespace holds index names and constraint names, as in Neo4j.
//!
//! The registry is an additive `FileMetadata` field: an older file loads with
//! none (every index answers to its canonical name) and an older binary that
//! meets the field ignores it. It is not carried in the write-ahead log, so a
//! name given after the last checkpoint does not survive crash recovery; the
//! index itself does, under its canonical name.

use serde::{Deserialize, Serialize};

use super::DirGraph;
use crate::graph::constraints::{normalize_properties, EntityKind};
use crate::graph::introspection::schema_overview::IndexKind;

/// Which index structure a registered name points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexClass {
    /// A hash equality index (`CREATE INDEX`).
    Equality,
    /// What `CREATE RANGE INDEX` installs: the hash equality index and the
    /// B-tree range index, which Neo4j's one RANGE index stands for.
    Range,
    /// A multi-property equality index.
    Composite,
    /// A BM25 text index, listed as `FULLTEXT`.
    Fulltext,
    /// An HNSW vector index.
    Vector,
}

impl IndexClass {
    /// Whether a registered name of this class names the listed row of `kind`.
    pub(crate) fn covers(self, kind: IndexKind) -> bool {
        matches!(
            (self, kind),
            (IndexClass::Equality, IndexKind::Equality)
                | (IndexClass::Range, IndexKind::Equality | IndexKind::Range)
                | (IndexClass::Composite, IndexKind::Composite)
                | (IndexClass::Fulltext, IndexKind::Text)
                | (IndexClass::Vector, IndexKind::Vector)
        )
    }
}

/// The index a registered name points at: its structure family and what it
/// indexes. `target` is a node label, or a relationship type when `entity` is
/// [`EntityKind::Relationship`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedIndex {
    pub class: IndexClass,
    #[serde(default, skip_serializing_if = "EntityKind::is_node")]
    pub entity: EntityKind,
    pub target: String,
    pub properties: Vec<String>,
}

impl NamedIndex {
    /// A name for a node index on `label`.
    pub fn node(class: IndexClass, label: &str, properties: &[String]) -> Self {
        NamedIndex {
            class,
            entity: EntityKind::Node,
            target: label.to_string(),
            properties: properties.to_vec(),
        }
    }

    /// A name for a relationship index on `rel_type.property`.
    pub fn relationship(class: IndexClass, rel_type: &str, property: &str) -> Self {
        NamedIndex {
            class,
            entity: EntityKind::Relationship,
            target: rel_type.to_string(),
            properties: vec![property.to_string()],
        }
    }

    fn describe(&self) -> String {
        let owner = &self.target;
        match self.properties.as_slice() {
            [property] if self.entity == EntityKind::Relationship => {
                format!("relationship:{owner}.{property}")
            }
            [property] => format!("{owner}.{property}"),
            many => format!("{owner}.({})", many.join(",")),
        }
    }

    /// Whether this entry indexes `(entity, target, properties)`, property
    /// order aside.
    pub(crate) fn indexes(&self, entity: EntityKind, target: &str, properties: &[String]) -> bool {
        self.entity == entity
            && self.target == target
            && normalize_properties(&self.properties) == normalize_properties(properties)
    }
}

impl DirGraph {
    /// Register `name` for an index, replacing a stale entry of that name.
    pub(crate) fn register_index_name(&mut self, name: &str, named: NamedIndex) {
        self.index_names.insert(name.to_string(), named);
    }

    /// The entry a registered name points at.
    pub(crate) fn index_name_entry(&self, name: &str) -> Option<&NamedIndex> {
        self.index_names.get(name)
    }

    /// Forget one name.
    pub(crate) fn forget_index_name(&mut self, name: &str) {
        self.index_names.remove(name);
    }

    /// Forget every name registered for `(entity, target, properties)` whose
    /// class is in `classes`. Called when an index structure goes away, so a
    /// later index built on the same property does not inherit the name.
    pub(crate) fn forget_index_names_for(
        &mut self,
        entity: EntityKind,
        target: &str,
        properties: &[String],
        classes: &[IndexClass],
    ) {
        if self.index_names.is_empty() {
            return;
        }
        self.index_names.retain(|_, named| {
            !(classes.contains(&named.class) && named.indexes(entity, target, properties))
        });
    }

    /// The name registered for the listed row `(entity, target, properties,
    /// kind)`. The lowest name wins when several classes cover the row, so the
    /// listing is stable across runs and reloads.
    pub(crate) fn registered_index_name(
        &self,
        entity: EntityKind,
        target: &str,
        properties: &[String],
        kind: IndexKind,
    ) -> Option<&str> {
        self.index_names
            .iter()
            .filter(|(_, named)| {
                named.class.covers(kind) && named.indexes(entity, target, properties)
            })
            .map(|(name, _)| name.as_str())
            .min()
    }
}

/// Refuse `name` for the index `named` describes when it is already taken.
///
/// Index names and constraint names share one namespace, as in Neo4j, so a name
/// a constraint holds is refused; so is a name another live index answers to,
/// registered or canonical. Naming the *same* index again (a rebuild, a
/// repeated statement) is not a conflict. Call it before building: nothing is
/// written here, so a refusal leaves the graph untouched.
pub fn check_index_name(graph: &DirGraph, name: &str, named: &NamedIndex) -> Result<(), String> {
    use crate::graph::introspection::schema_overview::{
        collect_indexes_unnamed, index_name_in_use,
    };
    if name.is_empty() {
        return Err("an index name must not be empty".to_string());
    }
    if graph.constraint_by_name(name).is_some() {
        return Err(format!(
            "the name '{name}' is already used by a constraint. Index and constraint names \
             share one namespace: choose another name."
        ));
    }
    if let Some(existing) = graph.index_name_entry(name) {
        let same = existing.class == named.class
            && existing.indexes(named.entity, &named.target, &named.properties);
        if !same && index_name_in_use(graph, name) {
            return Err(format!(
                "an index named '{name}' already exists on {}. Drop it first, or choose \
                 another name.",
                existing.describe()
            ));
        }
    }
    for row in collect_indexes_unnamed(graph) {
        let canonical = row.canonical_name();
        let this_index = named.indexes(
            row.entity_kind(),
            row.labels_or_types.first().map_or("", String::as_str),
            &row.properties,
        );
        if canonical == name && !this_index {
            return Err(format!(
                "'{name}' is the canonical name of the existing index {canonical}. Choose \
                 another name."
            ));
        }
    }
    Ok(())
}

/// Register `name` for the index `named` describes. Pair with
/// [`check_index_name`] before the build.
pub fn register_index_name(graph: &mut DirGraph, name: &str, named: NamedIndex) {
    graph.register_index_name(name, named);
}
