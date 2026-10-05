//! The checked single-property index creation every binding and the Cypher
//! `CREATE INDEX` executor share.
//!
//! `DirGraph::create_property_index_routed` stays the raw build (WAL replay
//! and the OKF loader re-install recorded declarations through it, where a
//! refusal would be wrong). The refusals and the report a *user's* request
//! gets live here, so the Python `create_index` and Cypher `CREATE INDEX`
//! cannot answer the same request differently again — they did: on a disk
//! graph the Python route reported an integer-column index as created and
//! serving with zero entries, while Cypher refused it.

use super::DirGraph;

/// What [`DirGraph::create_property_index_checked`] built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyIndexCreated {
    /// Distinct values indexed (in memory), or nodes indexed (disk).
    pub unique_values: usize,
    /// The index is the disk backend's persistent mmap index.
    pub persistent: bool,
    /// No index on `(node_type, property)` existed before the call.
    pub created: bool,
    /// Queries will read the index.
    pub serves_lookups: bool,
    /// Why queries will not read it, when they will not.
    pub not_serving: Option<String>,
    /// The node type has nodes or a schema declaration. An index on a type
    /// that has neither is real and fills as nodes arrive, but a misspelled
    /// type name is the likelier reading.
    pub node_type_known: bool,
}

/// Why [`DirGraph::create_property_index_checked`] built nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyIndexError {
    /// The request asks for an index no query could use; nothing is left
    /// behind.
    Refused(String),
    /// The backend failed to build the index.
    Build(String),
}

impl std::fmt::Display for PropertyIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropertyIndexError::Refused(message) | PropertyIndexError::Build(message) => {
                f.write_str(message)
            }
        }
    }
}

impl DirGraph {
    /// Build an equality index on `node_type.property` for a user's request:
    /// the backend-routed build plus every refusal and the honest report.
    ///
    /// Refused: a secondary-only label (no lookup consults it), and a disk
    /// index that indexed nothing over a populated type (persistent indexes
    /// cover string columns only).
    pub fn create_property_index_checked(
        &mut self,
        node_type: &str,
        property: &str,
    ) -> Result<PropertyIndexCreated, PropertyIndexError> {
        self.reject_secondary_only_index_type(node_type)
            .map_err(PropertyIndexError::Refused)?;
        // Read before the build, which rebuilds an existing index in place.
        let created = !self.has_any_index(node_type, property);
        let (unique_values, persistent) = self
            .create_property_index_routed(node_type, property)
            .map_err(PropertyIndexError::Build)?;
        if persistent {
            self.reject_empty_disk_index(node_type, property, unique_values)
                .map_err(PropertyIndexError::Refused)?;
        }
        Ok(PropertyIndexCreated {
            unique_values,
            persistent,
            created,
            serves_lookups: self.index_serves_lookups(node_type, property),
            not_serving: self.index_not_serving_reason(node_type, property),
            node_type_known: self.type_indices.contains_key(node_type)
                || self
                    .schema_definition
                    .as_ref()
                    .is_some_and(|schema| schema.node_schemas.contains_key(node_type)),
        })
    }

    /// A disk graph's persistent property index covers **string columns
    /// only** (see `DiskGraph::build_property_index`, where a non-string or
    /// missing property is a deliberate zero-entry no-op). A zero-entry index
    /// over a populated node type therefore indexed nothing, and reporting
    /// success for that is worse than failing: the caller would go on
    /// believing their lookups are indexed. Refuse it, and name the reason.
    ///
    /// An empty node type legitimately yields zero entries, so the emptiness
    /// check gates the error rather than the count alone.
    fn reject_empty_disk_index(
        &mut self,
        label: &str,
        property: &str,
        entries: usize,
    ) -> Result<(), String> {
        if entries > 0 {
            return Ok(());
        }
        let type_is_populated = self
            .type_indices
            .get(label)
            .is_some_and(|nodes| nodes.iter().next().is_some());
        if !type_is_populated {
            return Ok(());
        }
        // Leave no half-built index behind for a request that failed.
        let _ = self.drop_index(label, property);
        Err(format!(
            "an index on a disk-backed graph indexed no values for '{label}.{property}'. \
             Persistent property indexes cover string columns; '{property}' is either absent \
             from {label} or not stored as a string. Check `describe()` for the column's type, \
             or use an in-memory / mapped graph, where every property type is indexable."
        ))
    }
}
