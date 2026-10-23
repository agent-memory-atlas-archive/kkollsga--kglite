//! Vector indexes declared over a stored list property
//! (`CREATE VECTOR INDEX name FOR (n:Label) ON (n.prop) OPTIONS {...}`).
//!
//! A `build_vector_index` index sits over an *embedding store* that the caller
//! fills through the embedding APIs. This index is the other source: the
//! vectors are read from the node property itself, so a client that writes
//! `SET n.emb = $vector` and queries `db.index.vector.queryNodes` /
//! `SEARCH ... IN (VECTOR INDEX ...)` needs no second load.
//!
//! **A derived structure, not a second source of truth.** The store here is a
//! cache of the property: it is never serialized, and `.kgl` load, WAL replay
//! and vacuum rebuild it from the rows. Only the declaration persists, as the
//! `vector_source` of the index's registered name
//! ([`NamedIndex`](crate::graph::dir_graph::index_names::NamedIndex)).
//!
//! **Freshness follows the text-index pattern**
//! ([`crate::graph::index_freshness`]). The write hooks that already notify
//! text indexes (`index_freshness::write_hooks`) mark the slot; the next query
//! folds every marked slot, re-reading the property, so a write costs one set
//! insert and no vector work. Unlike the text lane the fold always runs to
//! completion: a vector index that served stale hits would answer a `SEARCH`
//! with a deleted or since-overwritten node.
//!
//! **What counts as indexed.** The label is the node's primary type (the same
//! meaning a text index gives it). A node holds a vector when the property is a
//! list of exactly `dimensions` finite numbers (integers widen to floats). An
//! absent property, `null`, another type, a list of the wrong length or with a
//! non-numeric or non-finite member, and a zero vector under cosine are all
//! *not indexed*, silently, as in Neo4j; `SHOW INDEXES` reports how many nodes
//! of the label are not indexed.
//!
//! **One source per column.** A property-sourced column and an embedder-managed
//! store at `(label, "<prop>_emb")` cannot coexist: the DDL refuses a column
//! that already has a store, and the embedding writers refuse a column that has
//! a property-sourced index ([`check_not_property_sourced`]).
//!
//! **Memory and mapped only**, like the text index: a heap-resident copy of a
//! disk-scale graph's vectors is the cliff that backend exists to avoid.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use petgraph::graph::NodeIndex;

use crate::datatypes::values::Value;
use crate::graph::algorithms::hnsw::HnswParams;
use crate::graph::algorithms::vector::DistanceMetric;
use crate::graph::dir_graph::index_names::{IndexClass, VectorSource};
use crate::graph::dir_graph::DirGraph;
use crate::graph::index_freshness::IndexFreshness;
use crate::graph::schema::{EmbeddingStore, InternedKey};
use crate::graph::storage::GraphRead;

/// The similarity functions the DDL accepts, as the store metric they map to.
pub(crate) fn metric_for_similarity(function: &str) -> Result<&'static str, String> {
    match function.to_ascii_lowercase().as_str() {
        "cosine" => Ok("cosine"),
        "euclidean" => Ok("euclidean"),
        other => Err(format!(
            "vector.similarity_function '{other}' is not supported: use 'cosine' or 'euclidean'."
        )),
    }
}

/// What a build counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PropertyVectorReport {
    pub indexed: usize,
    pub skipped: usize,
}

/// One label's HNSW index over one list property.
#[derive(Debug)]
pub struct PropertyVectorIndex {
    label: String,
    property: String,
    /// The alias-resolved field the build read, so a later fold cannot read a
    /// different column than the build did.
    resolved_field: String,
    dimension: usize,
    metric: &'static str,
    /// Behind a lock because the fold runs at query entry, where the caller
    /// holds `&DirGraph`. Lock order: this first, `freshness` second.
    store: RwLock<EmbeddingStore>,
    freshness: IndexFreshness,
    /// Members of the label that held no vector at the last build or fold.
    skipped: AtomicUsize,
}

impl Clone for PropertyVectorIndex {
    fn clone(&self) -> Self {
        PropertyVectorIndex {
            label: self.label.clone(),
            property: self.property.clone(),
            resolved_field: self.resolved_field.clone(),
            dimension: self.dimension,
            metric: self.metric,
            store: RwLock::new(self.read_store().clone()),
            freshness: self.freshness.clone(),
            skipped: AtomicUsize::new(self.skipped.load(Ordering::Relaxed)),
        }
    }
}

fn distance_of(metric: &str) -> DistanceMetric {
    if metric == "euclidean" {
        DistanceMetric::Euclidean
    } else {
        DistanceMetric::Cosine
    }
}

/// The vector a node holds for the indexed property, if it holds a valid one.
fn read_vector(
    graph: &DirGraph,
    node: NodeIndex,
    label_key: InternedKey,
    label: &str,
    field: &str,
    dimension: usize,
    metric: &str,
) -> Option<Vec<f32>> {
    if graph.graph.node_type_of(node) != Some(label_key) {
        return None;
    }
    let view = graph.graph.node_view(node)?;
    let value = view.resolved_field(label, field, InternedKey::from_str(field))?;
    let Value::List(items) = value.as_ref() else {
        return None;
    };
    if items.len() != dimension {
        return None;
    }
    let mut vector = Vec::with_capacity(dimension);
    for item in items {
        let component = match item {
            Value::Float64(f) => *f as f32,
            Value::Int64(i) => *i as f32,
            _ => return None,
        };
        if !component.is_finite() {
            return None;
        }
        vector.push(component);
    }
    if metric == "cosine" && vector.iter().all(|c| *c == 0.0) {
        return None;
    }
    Some(vector)
}

impl PropertyVectorIndex {
    /// Build over every current member of `label`.
    fn build(
        graph: &DirGraph,
        label: &str,
        property: &str,
        source: &VectorSource,
    ) -> Result<(Self, PropertyVectorReport), String> {
        let metric = metric_for_similarity(&source.metric)?;
        let resolved_field = graph.resolve_alias(label, property).to_string();
        let label_key = InternedKey::from_str(label);
        let members: Vec<NodeIndex> = graph
            .type_indices
            .get(label)
            .map(|nodes| nodes.to_vec())
            .unwrap_or_default();
        let mut store = EmbeddingStore::with_metric(source.dimension, metric);
        let mut skipped = 0usize;
        for node in &members {
            match read_vector(
                graph,
                *node,
                label_key,
                label,
                &resolved_field,
                source.dimension,
                metric,
            ) {
                Some(vector) => {
                    store.set_embedding(node.index(), &vector);
                }
                None => skipped += 1,
            }
        }
        rebuild_hnsw(&mut store, metric);
        let report = PropertyVectorReport {
            indexed: store.len(),
            skipped,
        };
        let index = PropertyVectorIndex {
            label: label.to_string(),
            property: property.to_string(),
            resolved_field,
            dimension: source.dimension,
            metric,
            store: RwLock::new(store),
            freshness: IndexFreshness::covering(node_bound(graph), None),
            skipped: AtomicUsize::new(skipped),
        };
        Ok((index, report))
    }

    fn read_store(&self) -> RwLockReadGuard<'_, EmbeddingStore> {
        self.store.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write_store(&self) -> RwLockWriteGuard<'_, EmbeddingStore> {
        self.store.write().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn property(&self) -> &str {
        &self.property
    }

    pub(crate) fn dimension(&self) -> usize {
        self.dimension
    }

    pub(crate) fn metric(&self) -> &'static str {
        self.metric
    }

    /// Fold every outstanding slot into the store and the HNSW index, re-reading
    /// each node's property. Returns how many slots were re-read.
    pub(crate) fn refresh(&self, graph: &DirGraph) -> usize {
        // The store lock comes before the delta claim: a second reader either
        // waits for this fold or finds nothing left (`index_freshness` lock
        // order note).
        let mut store = self.write_store();
        let Some(delta) = self.freshness.take_delta(node_bound(graph)) else {
            return 0;
        };
        let label_key = InternedKey::from_str(&self.label);
        let mut seen = 0usize;
        for slot in delta.slots() {
            seen += 1;
            let node = NodeIndex::new(slot as usize);
            let held = store.node_to_slot.contains_key(&(slot as usize));
            match read_vector(
                graph,
                node,
                label_key,
                &self.label,
                &self.resolved_field,
                self.dimension,
                self.metric,
            ) {
                Some(vector) => {
                    store.set_embedding(slot as usize, &vector);
                }
                None => {
                    if held {
                        store.remove_embedding(slot as usize);
                    }
                }
            }
        }
        // What a fold leaves unindexed cannot be counted from the delta (a
        // node that never held a vector never appears in it), so derive it from
        // the label's member count.
        let members = graph.type_indices.get(&self.label).map_or(0, |n| n.len());
        self.skipped
            .store(members.saturating_sub(store.len()), Ordering::Relaxed);
        if seen > 0 {
            catch_up_hnsw(&mut store, self.metric, seen);
        }
        seen
    }

    /// Nodes of the label that hold no vector right now. Folds first.
    pub(crate) fn unindexed(&self, graph: &DirGraph) -> usize {
        self.refresh(graph);
        self.skipped.load(Ordering::Relaxed)
    }
}

/// Bring the HNSW index in step with the store after a fold of `changes` slots.
///
/// A removal remaps store slots (tail swap) and drops the index, and a fold
/// that rewrote most of the store is cheaper to rebuild than to relink, so both
/// rebuild; otherwise the incremental refresh folds the appended and replaced
/// slots.
fn catch_up_hnsw(store: &mut EmbeddingStore, metric: &str, changes: usize) {
    if store.has_index() && changes <= store.len().max(1000) / 2 {
        store.refresh_index();
        if store.has_index() && !store.index_is_stale() {
            return;
        }
    }
    rebuild_hnsw(store, metric);
}

fn rebuild_hnsw(store: &mut EmbeddingStore, metric: &str) {
    if store.len() == 0 {
        store.invalidate_index();
        return;
    }
    let seed = 0x9E37_79B9_7F4A_7C15 ^ (store.len() as u64);
    // The failure cases (zero dimension, Poincare) are excluded by the DDL's
    // own validation; a failed build leaves the exact scan, which is correct.
    let _ = store.build_index(distance_of(metric), HnswParams::default(), seed);
}

#[inline]
fn node_bound(graph: &DirGraph) -> u32 {
    GraphRead::node_bound(&graph.graph) as u32
}

/// The index over `(label, property)`.
pub(crate) fn property_vector_index<'a>(
    graph: &'a DirGraph,
    label: &str,
    property: &str,
) -> Option<&'a PropertyVectorIndex> {
    graph
        .property_vectors
        .get(&(label.to_string(), property.to_string()))
}

/// Every property-sourced vector index, in a stable order.
pub(crate) fn list_property_vector_indexes(graph: &DirGraph) -> Vec<&PropertyVectorIndex> {
    let mut all: Vec<&PropertyVectorIndex> = graph.property_vectors.values().collect();
    all.sort_by(|a, b| (&a.label, &a.property).cmp(&(&b.label, &b.property)));
    all
}

/// Whether `(label, text_column)` is the source column of a property-sourced
/// vector index.
pub(crate) fn is_property_sourced(graph: &DirGraph, label: &str, column: &str) -> bool {
    !graph.property_vectors.is_empty()
        && graph
            .property_vectors
            .contains_key(&(label.to_string(), column.to_string()))
}

/// Refuse an embedding-store write onto a column a property-sourced vector
/// index owns. Cheap when the graph has none.
pub(crate) fn check_not_property_sourced(
    graph: &DirGraph,
    label: &str,
    column: &str,
) -> Result<(), String> {
    if !is_property_sourced(graph, label, column) {
        return Ok(());
    }
    Err(format!(
        "'{label}.{column}' is the source column of a vector index created with CREATE VECTOR \
         INDEX: its vectors are read from the node property, so they cannot also be written \
         through the embedding API. Write the property instead, or DROP INDEX first."
    ))
}

/// Create the index for `source` over `(label, property)` and install it.
///
/// Refuses a disk graph and a column that already has an embedding store.
pub(crate) fn build_property_vector_index(
    graph: &mut DirGraph,
    label: &str,
    property: &str,
    source: &VectorSource,
) -> Result<PropertyVectorReport, String> {
    if GraphRead::is_disk(&graph.graph) {
        return Err(format!(
            "CREATE VECTOR INDEX over '{label}.{property}' is not supported on a disk-backed \
             graph: the index keeps a heap copy of every vector, which is the memory cliff \
             that backend exists to avoid. Use the default (in-memory) or 'mapped' storage \
             mode, or load vectors into an embedding store and call build_vector_index."
        ));
    }
    if source.dimension == 0 {
        return Err("vector.dimensions must be at least 1".to_string());
    }
    if crate::graph::embeddings::store_exists(graph, label, property) {
        return Err(format!(
            "'{label}.{property}' already has an embedding store ('{label}.{property}_emb'). A \
             column is either written through the embedding API or read from the node property \
             by CREATE VECTOR INDEX, never both: drop the store (db.node_embeddings.drop) or \
             index a different property."
        ));
    }
    let (index, report) = PropertyVectorIndex::build(graph, label, property, source)?;
    graph
        .property_vectors
        .insert((label.to_string(), property.to_string()), index);
    Ok(report)
}

/// Remove the index over `(label, property)`; whether one existed.
pub(crate) fn drop_property_vector_index(
    graph: &mut DirGraph,
    label: &str,
    property: &str,
) -> bool {
    let had = graph
        .property_vectors
        .remove(&(label.to_string(), property.to_string()))
        .is_some();
    if had {
        graph.forget_index_names_for(
            crate::graph::constraints::EntityKind::Node,
            label,
            std::slice::from_ref(&property.to_string()),
            &[IndexClass::Vector],
        );
    }
    had
}

/// Rebuild every declared index from the registry. Load, WAL replay and a
/// vacuum remap end here: the structures are derived, only the declarations
/// persist. A declaration the recovered graph cannot serve (a disk graph, a
/// column that has since gained an embedding store) is skipped, not fatal.
pub(crate) fn rebuild_declared_property_vectors(graph: &mut DirGraph) {
    graph.property_vectors.clear();
    if graph.index_names.is_empty() {
        return;
    }
    let declared: Vec<(String, String, VectorSource)> = graph
        .index_names
        .values()
        .filter(|named| {
            named.class == IndexClass::Vector
                && named.entity == crate::graph::constraints::EntityKind::Node
        })
        .filter_map(|named| {
            let source = named.vector_source.clone()?;
            let property = named.properties.first()?.clone();
            Some((named.target.clone(), property, source))
        })
        .collect();
    for (label, property, source) in declared {
        let _ = build_property_vector_index(graph, &label, &property, &source);
    }
}

/// Rank `vector` against the index over `(label, property)` after folding its
/// outstanding writes. `None` when no such index exists.
pub(crate) fn query_property_vector_index(
    graph: &DirGraph,
    label: &str,
    property: &str,
    vector: &[f32],
    top_k: usize,
    admits: Option<&dyn Fn(usize) -> bool>,
) -> Option<Result<Vec<crate::graph::edge_embeddings::vector_index::DenseStoreHit>, String>> {
    use crate::graph::edge_embeddings::vector_index::{rank_dense_stores, EdgeVectorQueryOptions};
    let index = property_vector_index(graph, label, property)?;
    index.refresh(graph);
    let store = index.read_store();
    Some(rank_dense_stores(
        "Node",
        &[(label, &*store)],
        property,
        vector,
        &EdgeVectorQueryOptions {
            top_k,
            exact: false,
            metric: None,
        },
        graph.read_only,
        admits,
    ))
}

// ---- write hooks (reached only through `index_freshness::write_hooks`) ----

/// A node of `node_type` was created at `node`.
pub(crate) fn note_node_created(graph: &DirGraph, node: NodeIndex, node_type: &str) {
    let slot = node.index() as u32;
    for ((label, _), index) in &graph.property_vectors {
        index
            .freshness
            .note_created(slot, label.as_str() == node_type);
    }
}

/// A node's property was written. `field` is the alias-resolved field, or
/// `None` when the caller wrote several and did not decompose them.
pub(crate) fn note_property_written(
    graph: &DirGraph,
    node: NodeIndex,
    node_type: &str,
    field: Option<&str>,
) {
    let slot = node.index() as u32;
    for ((label, _), index) in &graph.property_vectors {
        if label.as_str() != node_type {
            continue;
        }
        if field.is_none_or(|written| written == index.resolved_field) {
            index.freshness.note_changed(slot);
        }
    }
}

/// A node is about to be removed: its slot must be re-read, and may be handed
/// to the next node created.
pub(crate) fn note_node_removed(graph: &DirGraph, node: NodeIndex) {
    let slot = node.index() as u32;
    let Some(node_type) = graph.graph.node_type_of(node) else {
        return;
    };
    for ((label, _), index) in &graph.property_vectors {
        if InternedKey::from_str(label) == node_type {
            index.freshness.note_changed(slot);
        }
    }
}

impl PropertyVectorIndex {
    pub(crate) fn begin_statement(&self) {
        self.freshness.begin_statement();
    }

    pub(crate) fn rollback_statement(&self) {
        self.freshness.rollback_statement();
    }

    pub(crate) fn end_statement(&self) {
        self.freshness.end_statement();
    }
}
