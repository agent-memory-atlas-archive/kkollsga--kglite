//! Typed relationship embedding storage and validated private mutation primitives.
//!
//! Prepared for P1. This module deliberately exposes `EdgeIndex`, never a raw
//! integer or `NodeIndex`, at its mutation/query boundary. The shared numeric
//! store remains an implementation detail.

use std::collections::HashSet;

use petgraph::graph::EdgeIndex;
use serde::{Deserialize, Serialize};

use crate::graph::algorithms::vector::DistanceMetric;
use crate::graph::embedding_validation::validate_finite_vector;
use crate::graph::embeddings::store_name;
use crate::graph::schema::{DirGraph, EdgeData, EmbeddingStore, RemovedEmbedding};
use crate::graph::storage::{GraphRead, GraphWrite};

type EdgeEmbeddingKey = (String, String);

const VACANT_EDGE: u32 = u32::MAX;

/// Temporary physical-slot remap produced by heap vacuum or disk compaction.
/// It exists only while stores are rewritten; no durable edge id is added.
pub(crate) struct EdgeRemap {
    slots: Vec<u32>,
}

impl EdgeRemap {
    pub(crate) fn with_bound(bound: usize) -> Self {
        Self {
            slots: vec![VACANT_EDGE; bound],
        }
    }

    pub(crate) fn set(&mut self, old: EdgeIndex, new: EdgeIndex) {
        self.slots[old.index()] = new.index() as u32;
    }

    pub(crate) fn from_raw(slots: Vec<u32>) -> Self {
        Self { slots }
    }

    pub(crate) fn get(&self, old: EdgeIndex) -> Option<EdgeIndex> {
        let raw = *self.slots.get(old.index())?;
        (raw != VACANT_EDGE).then(|| EdgeIndex::new(raw as usize))
    }
}

// P4 activates the canonical typed store-key constructor.
#[cfg_attr(not(test), allow(dead_code))]
fn edge_store_key(connection_type: &str, text_property: &str) -> EdgeEmbeddingKey {
    (connection_type.to_string(), store_name(text_property))
}

/// Sparse vectors owned by physical relationship slots.
///
/// `EmbeddingStore` supplies the dense numeric layout, cached norms and later
/// HNSW machinery. Keeping it private prevents a node slot from crossing this
/// typed boundary accidentally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EdgeEmbeddingStore {
    numeric: EmbeddingStore,
}

// P2/P4 consume these typed construction and lookup methods; P1 owns their tested foundation.
#[cfg_attr(not(test), allow(dead_code))]
impl EdgeEmbeddingStore {
    pub(crate) fn new(dimension: usize, metric: Option<&str>) -> Self {
        let numeric = match metric {
            Some(metric) => EmbeddingStore::with_metric(dimension, metric),
            None => EmbeddingStore::new(dimension),
        };
        Self { numeric }
    }

    pub(crate) fn dimension(&self) -> usize {
        self.numeric.dimension
    }

    pub(crate) fn len(&self) -> usize {
        self.numeric.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.numeric.len() == 0
    }

    pub(crate) fn metric(&self) -> Option<&str> {
        self.numeric.metric.as_deref()
    }

    pub(crate) fn get(&self, edge: EdgeIndex) -> Option<&[f32]> {
        self.numeric.get_embedding(edge.index())
    }

    pub(crate) fn edges(&self) -> impl Iterator<Item = EdgeIndex> + '_ {
        self.numeric
            .slot_to_node
            .iter()
            .copied()
            .map(EdgeIndex::new)
    }

    fn set_manual(&mut self, edge: EdgeIndex, vector: &[f32]) {
        self.numeric.set_embedding(edge.index(), vector);
        self.numeric.text_hashes.remove(&edge.index());
    }

    pub(crate) fn remove(&mut self, edge: EdgeIndex) -> Option<RemovedEmbedding> {
        self.numeric.remove_embedding(edge.index())
    }

    pub(crate) fn restore(&mut self, edge: EdgeIndex, removed: &RemovedEmbedding) {
        self.numeric.restore_embedding(edge.index(), removed);
    }

    /// Rewrite physical relationship keys after a topology compaction.
    /// Iterating the old dense slot order preserves exact-search tie order.
    pub(crate) fn remap(&mut self, remap: &EdgeRemap) {
        let dimension = self.numeric.dimension;
        let metric = self.numeric.metric.clone();
        let model_id = self.numeric.model_id.clone();
        let old = std::mem::replace(&mut self.numeric, EmbeddingStore::new(dimension));
        let mut rebuilt = EmbeddingStore::new(dimension);
        rebuilt.metric = metric;
        rebuilt.model_id = model_id;
        rebuilt.data.reserve(old.data.len());
        for &old_raw in &old.slot_to_node {
            let old_edge = EdgeIndex::new(old_raw);
            let Some(new_edge) = remap.get(old_edge) else {
                continue;
            };
            let vector = old
                .get_embedding(old_raw)
                .expect("slot_to_node and node_to_slot remain a bijection");
            rebuilt.set_embedding(new_edge.index(), vector);
            if let Some(hash) = old.text_hashes.get(&old_raw) {
                rebuilt.set_text_hash(new_edge.index(), *hash);
            }
        }
        self.numeric = rebuilt;
    }
}

/// Remove one relationship's vectors before its physical slot becomes reusable.
/// The empty-map guard is the common no-vector fast path.
pub(crate) fn prune_edge_embeddings(graph: &mut DirGraph, edge: EdgeIndex) {
    if graph.edge_embeddings.is_empty() {
        return;
    }
    let removed: Vec<_> = graph
        .edge_embeddings
        .iter_mut()
        .filter_map(|(key, store)| Some((key.clone(), store.remove(edge)?)))
        .collect();
    if let Some(journal) = graph.graph.undo_journal_mut() {
        for (store_key, prior) in removed {
            journal.note_edge_embedding_removed(store_key, edge, prior);
        }
    }
}

/// The graph-level edge deletion choke point. Pruning precedes the backend
/// delete, so reverse journal replay restores the edge before its vector.
pub(crate) fn remove_edge_with_embeddings(
    graph: &mut DirGraph,
    edge: EdgeIndex,
) -> Option<EdgeData> {
    let is_live = {
        let _arena_guard = graph.graph.begin_query();
        graph.graph.edge_weight(edge).is_some()
    };
    if !is_live {
        return None;
    }
    prune_edge_embeddings(graph, edge);
    GraphWrite::remove_edge(&mut graph.graph, edge)
}

pub(crate) fn remap_edge_embeddings(graph: &mut DirGraph, remap: &EdgeRemap) {
    for store in graph.edge_embeddings.values_mut() {
        store.remap(remap);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
// P4 exposes this report through the validated relationship-value boundary.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct EdgeEmbeddingWriteReport {
    /// Vectors held after the call.
    pub(crate) stored: usize,
    pub(crate) dimension: usize,
    /// Entries whose vector changed or was newly installed.
    pub(crate) changed: usize,
    pub(crate) store_created: bool,
}

// P4 activates metric validation through the private batch primitive.
#[cfg_attr(not(test), allow(dead_code))]
fn validate_metric(metric: Option<&str>) -> Result<(), String> {
    if let Some(name) = metric {
        if DistanceMetric::from_name(name).is_none() {
            return Err(format!(
                "Unknown distance metric '{name}'. Use cosine, dot_product, euclidean, or poincare."
            ));
        }
    }
    Ok(())
}

// P4 activates relationship identity validation before selected writes.
#[cfg_attr(not(test), allow(dead_code))]
fn validate_live_edge_type(
    graph: &DirGraph,
    edge: EdgeIndex,
    connection_type: &str,
) -> Result<(), String> {
    let weight = graph
        .graph
        .edge_weight(edge)
        .ok_or_else(|| format!("Relationship slot {} is not live", edge.index()))?;
    let actual = weight.connection_type_str(&graph.interner);
    if actual != connection_type {
        return Err(format!(
            "Relationship slot {} has type '{actual}', not declared type '{connection_type}'",
            edge.index()
        ));
    }
    Ok(())
}

/// Atomically upsert explicitly selected relationships.
///
/// Kept crate-private until P3 supplies the durable grouped WAL record and P4
/// resolves Cypher `Relationship` values into the typed indices accepted here.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn upsert_edge_embeddings(
    graph: &mut DirGraph,
    connection_type: &str,
    text_property: &str,
    entries: Vec<(EdgeIndex, Vec<f32>)>,
    metric: Option<&str>,
) -> Result<EdgeEmbeddingWriteReport, String> {
    if entries.is_empty() {
        return Ok(EdgeEmbeddingWriteReport::default());
    }
    if graph.records_payloads() {
        return Err(
            "Relationship embedding writes require grouped WAL support; use a non-durable graph"
                .to_string(),
        );
    }
    validate_metric(metric)?;

    let key = edge_store_key(connection_type, text_property);
    let existing = graph.edge_embeddings.get(&key);
    let expected_dimension = existing
        .map(EdgeEmbeddingStore::dimension)
        .unwrap_or(entries[0].1.len());
    if expected_dimension == 0 {
        return Err("Embedding vectors must not be empty".to_string());
    }
    if let (Some(store), Some(requested)) = (existing, metric) {
        let stored = store.metric().unwrap_or("cosine");
        if requested != stored {
            return Err(format!(
                "Store metric is '{stored}', but this batch requested '{requested}'"
            ));
        }
    }

    let mut seen = HashSet::with_capacity(entries.len());
    let _arena_guard = graph.graph.begin_query();
    for (edge, vector) in &entries {
        if !seen.insert(edge.index()) {
            return Err(format!(
                "Relationship slot {} appears more than once in the batch",
                edge.index()
            ));
        }
        validate_live_edge_type(graph, *edge, connection_type)?;
        if vector.len() != expected_dimension {
            return Err(format!(
                "Embedding for relationship slot {} has dimension {}, expected {}",
                edge.index(),
                vector.len(),
                expected_dimension
            ));
        }
        validate_finite_vector(vector).map_err(|error| {
            format!(
                "Invalid embedding for relationship slot {}: {error}",
                edge.index()
            )
        })?;
    }
    drop(_arena_guard);

    let changed_edges: HashSet<_> = entries
        .iter()
        .filter(|(edge, vector)| {
            existing.and_then(|store| store.get(*edge)) != Some(vector.as_slice())
        })
        .map(|(edge, _)| edge.index())
        .collect();
    if changed_edges.is_empty() {
        return Ok(EdgeEmbeddingWriteReport {
            stored: existing.map_or(0, EdgeEmbeddingStore::len),
            dimension: expected_dimension,
            changed: 0,
            store_created: false,
        });
    }

    let store_created = existing.is_none();
    let store = graph
        .edge_embeddings
        .entry(key)
        .or_insert_with(|| EdgeEmbeddingStore::new(expected_dimension, metric));
    for (edge, vector) in &entries {
        if changed_edges.contains(&edge.index()) {
            store.set_manual(*edge, vector);
        }
    }
    // A manual write makes aggregate generated-model provenance unknown.
    store.numeric.model_id = None;
    let stored = store.len();
    graph.bump_version();

    Ok(EdgeEmbeddingWriteReport {
        stored,
        dimension: expected_dimension,
        changed: changed_edges.len(),
        store_created,
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn remove_edge_embeddings(
    graph: &mut DirGraph,
    connection_type: &str,
    text_property: &str,
    edges: &[EdgeIndex],
) -> Result<usize, String> {
    if edges.is_empty() {
        return Ok(0);
    }
    if graph.records_payloads() {
        return Err(
            "Relationship embedding writes require grouped WAL support; use a non-durable graph"
                .to_string(),
        );
    }

    let mut seen = HashSet::with_capacity(edges.len());
    let _arena_guard = graph.graph.begin_query();
    for &edge in edges {
        if !seen.insert(edge.index()) {
            return Err(format!(
                "Relationship slot {} appears more than once in the batch",
                edge.index()
            ));
        }
        validate_live_edge_type(graph, edge, connection_type)?;
    }
    drop(_arena_guard);

    let key = edge_store_key(connection_type, text_property);
    let Some(store) = graph.edge_embeddings.get_mut(&key) else {
        return Ok(0);
    };
    let mut removed = 0;
    for &edge in edges {
        removed += usize::from(store.remove(edge).is_some());
    }
    if removed > 0 {
        graph.bump_version();
    }
    Ok(removed)
}

#[cfg(test)]
#[path = "edge_embeddings_tests.rs"]
mod tests;
