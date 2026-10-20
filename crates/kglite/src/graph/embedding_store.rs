//! [`EmbeddingStore`]: contiguous per-(type, property) f32 vectors with cached
//! norms, an optional HNSW index, and the slot maps the undo journal and the
//! `.kgl` loader work against. Split from `schema.rs` (source-quality ceiling);
//! `schema` re-exports every item, so `crate::graph::schema::EmbeddingStore`
//! and friends keep their paths.

use crate::graph::index_freshness::{IndexFreshness, SlotCoverage};
use crate::graph::quantized::{Precision, QuantScan, QuantStore};
use crate::graph::schema::serialize_sorted_map;
use crate::graph::vector_buf::VectorBuf;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{RwLock, RwLockReadGuard};

/// Contiguous columnar storage for f32 embeddings associated with a (node_type, property_name).
/// All vectors in one store share the same dimensionality.
/// The flat Vec<f32> layout enables SIMD-friendly linear scans during vector search.
#[derive(Debug, Serialize, Deserialize)]
pub struct EmbeddingStore {
    pub dimension: usize,
    /// Contiguous f32 buffer: embedding i occupies data[i*dimension..(i+1)*dimension].
    /// Heap, or a read-only view of a memory-mapped `.kgl` until the first write.
    pub data: VectorBuf,
    /// Maps NodeIndex.index() -> slot position in the contiguous buffer.
    /// Key-sorted on write so equivalent stores serialize byte-identically.
    #[serde(serialize_with = "serialize_sorted_map")]
    pub node_to_slot: HashMap<usize, usize>,
    /// Reverse map: slot -> NodeIndex.index(), needed for returning results
    pub slot_to_node: Vec<usize>,
    /// Default distance metric for this embedding store (e.g. "cosine", "poincare").
    /// Used when no explicit metric is provided at query time.
    #[serde(default)]
    pub metric: Option<String>,
    /// Identity of the model that produced these vectors (e.g. "BAAI/bge-m3").
    /// Set from the embedder's `model_id()` during `embed_texts`; `None` for
    /// vectors supplied directly via `add_embeddings`/`set_embeddings` or by a
    /// backend that doesn't name its model. Surfaced via `embedding_info()`.
    #[serde(default)]
    pub model_id: Option<String>,
    /// Per-node content hash of the embedded text (`NodeIndex.index()` → hash),
    /// stamped during `embed_texts`. Lets `embed_texts(mode='changed')`
    /// re-embed exactly the nodes whose text changed since the last pass,
    /// instead of every node or only the missing ones. Empty for stores built
    /// from raw vectors (no source text to hash).
    /// Key-sorted on write so equivalent stores serialize byte-identically.
    #[serde(default, serialize_with = "serialize_sorted_map")]
    pub text_hashes: HashMap<usize, u64>,
    /// Cached L2 norm (‖v‖) per slot, parallel to `slot_to_node` — `norms[slot]`
    /// is the Euclidean norm of the vector at `data[slot*dimension..]`. A derived
    /// acceleration structure: cosine search reads it instead of recomputing the
    /// stored vector's norm on every query (collapsing cosine's inner loop to a
    /// single dot product). Never part of the serde payload (`#[serde(skip)]`):
    /// a v8 file rebuilds it from `data` after load via `rebuild_norms`, while a
    /// v9 file stores it as its own mappable section next to a mapped `data`, so
    /// a mapped load skips the pass over every vector.
    #[serde(skip)]
    pub norms: VectorBuf,
    /// Optional HNSW approximate-nearest-neighbour index over this store's
    /// vectors, built on demand via `build_vector_index`. When present (and the
    /// query isn't `exact`), top-k vector search dispatches through it instead of
    /// a full linear scan.
    ///
    /// The index addresses **store slots**, not node indices, so it survives
    /// exactly the writes that leave the slot layout alone: appending a vector
    /// (a new slot above its coverage) and replacing one in place (same slot,
    /// new content) are tracked as freshness deltas and folded in
    /// incrementally, while anything that *remaps* slots — a node deletion's
    /// `remove_embedding` tail swap, its rollback, compaction — drops the index
    /// outright through [`Self::invalidate_index`].
    ///
    /// **Behind a lock** for the same reason the text index is (see
    /// [`crate::graph::text_indexes`]): catch-up happens at query entry, where
    /// the caller holds `&DirGraph` and cannot reach a `&mut` store. Lock order
    /// is this lock first, [`IndexFreshness`] second, never the reverse.
    ///
    /// NOT serialized here (`#[serde(skip)]`): it rides in a dedicated
    /// versioned, skippable sub-section of the `.kgl` so the on-disk format can
    /// evolve independently; absent that section it's simply rebuilt.
    #[serde(skip)]
    index: RwLock<Option<crate::graph::algorithms::hnsw::HnswIndex>>,
    /// Which store slots the index has yet to cover — the vector lane's half of
    /// the shared catch-up framework.
    ///
    /// Tracked in **store-slot space**, not the graph's node-slot space the
    /// text lane uses: an embedding store's documents *are* its slots, and a
    /// node that has never been embedded is not a delta at all (catch-up never
    /// embeds — decision 11c). That is also why no write hook reaches this
    /// field: vectors arrive only through the embedding APIs, so bulk node
    /// ingest cannot make a vector index stale and pays nothing for its
    /// existence.
    #[serde(skip, default = "empty_vector_freshness")]
    freshness: IndexFreshness,
    /// Optional quantized scan copy of `data`, kept slot-for-slot in step with
    /// it by every mutator below. Derived data: never part of the serde
    /// payload, persisted as its own v9 sections (and the precision in the
    /// file metadata), and rebuilt from `data` whenever it is absent.
    #[serde(skip)]
    quant: Option<QuantStore>,
}

/// Freshness state for a deserialized store, which carries no index yet — the
/// vector-index section attaches one (with its own state) afterwards, and a
/// file without that section rebuilds.
fn empty_vector_freshness() -> IndexFreshness {
    IndexFreshness::covering(0, None)
}

impl Clone for EmbeddingStore {
    /// Independent of the original — including the HNSW index and its freshness
    /// state. `Clone` backs snapshots, transactions and `independent_copy`, and
    /// each of those must be able to catch its index up without touching the
    /// other's. The one thing shared is a *mapped* `data`/`norms` buffer (an
    /// `Arc` bump on an immutable file view): a clone that writes promotes its
    /// own heap copy and leaves the original mapped.
    fn clone(&self) -> Self {
        EmbeddingStore {
            dimension: self.dimension,
            data: self.data.clone(),
            node_to_slot: self.node_to_slot.clone(),
            slot_to_node: self.slot_to_node.clone(),
            metric: self.metric.clone(),
            model_id: self.model_id.clone(),
            text_hashes: self.text_hashes.clone(),
            norms: self.norms.clone(),
            index: RwLock::new(self.index_guard().clone()),
            freshness: self.freshness.clone(),
            quant: self.quant.clone(),
        }
    }
}

/// A borrowed HNSW index, held for the duration of one query's use of it.
///
/// Holding the read guard is what keeps a concurrent catch-up from renumbering
/// the topology under a search that is walking it — the same reason the text
/// lane hands out a guard rather than a reference.
pub struct HnswRead<'a>(RwLockReadGuard<'a, Option<crate::graph::algorithms::hnsw::HnswIndex>>);

/// Moved rebuildable vector-index state used by statement rollback.
#[derive(Debug)]
pub(crate) struct VectorIndexState {
    pub(crate) index: Option<crate::graph::algorithms::hnsw::HnswIndex>,
    pub(crate) freshness: IndexFreshness,
}

impl std::ops::Deref for HnswRead<'_> {
    type Target = crate::graph::algorithms::hnsw::HnswIndex;

    fn deref(&self) -> &Self::Target {
        self.0
            .as_ref()
            .expect("HnswRead is only constructed over a present index")
    }
}

/// One vector lifted out of an [`EmbeddingStore`] by
/// [`EmbeddingStore::remove_embedding`] — everything
/// [`EmbeddingStore::restore_embedding`] needs to put it back on the slot it
/// vacated. Carried by the undo journal so a rolled-back `DELETE` leaves
/// vector search returning exactly what it returned before.
#[derive(Debug, Clone, PartialEq)]
pub struct RemovedEmbedding {
    /// The slot the vector occupied. The restore reverses the tail swap that
    /// freed it, so slot identity survives a rollback.
    pub slot: usize,
    pub vector: Vec<f32>,
    /// The node's `text_hashes` entry, if it had one (`embed_texts` stamps
    /// it; raw `add_embeddings` does not). Restoring it keeps
    /// `embed_texts(mode='changed')` from re-embedding a node whose delete
    /// was rolled back.
    pub text_hash: Option<u64>,
}

/// Squared L2 norm of a vector, computed with the same 4-accumulator
/// 8-lane chunk pattern the cosine kernel uses for the stored vector, so a
/// cached norm matches the value cosine would have computed inline (within fp
/// rounding). Auto-vectorizes (SSE2/AVX2/NEON) on the hot rebuild path.
#[inline]
fn l2_norm_sq(v: &[f32]) -> f32 {
    let (mut s0, mut s1, mut s2, mut s3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (chunks, rem) = v.as_chunks::<8>();
    for c in chunks {
        s0 += c[0] * c[0];
        s1 += c[1] * c[1];
        s2 += c[2] * c[2];
        s3 += c[3] * c[3];
        s0 += c[4] * c[4];
        s1 += c[5] * c[5];
        s2 += c[6] * c[6];
        s3 += c[7] * c[7];
    }
    for &x in rem {
        s0 += x * x;
    }
    (s0 + s1) + (s2 + s3)
}

impl EmbeddingStore {
    pub fn new(dimension: usize) -> Self {
        EmbeddingStore {
            dimension,
            data: VectorBuf::new(),
            node_to_slot: HashMap::new(),
            slot_to_node: Vec::new(),
            metric: None,
            model_id: None,
            text_hashes: HashMap::new(),
            norms: VectorBuf::new(),
            index: RwLock::new(None),
            freshness: empty_vector_freshness(),
            quant: None,
        }
    }

    pub fn with_metric(dimension: usize, metric: &str) -> Self {
        EmbeddingStore {
            metric: Some(metric.to_string()),
            ..EmbeddingStore::new(dimension)
        }
    }

    /// FNV-1a 64-bit hash of an embedded text — the content fingerprint
    /// stored in `text_hashes` for change detection. Deterministic across
    /// processes (same rationale as `InternedKey::from_str`), so a hash
    /// persisted in one process matches a re-hash in another after reload.
    pub fn text_hash(text: &str) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;
        let mut h = FNV_OFFSET;
        for &byte in text.as_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        h
    }

    /// Record the source-text hash for a node (by `NodeIndex.index()`).
    pub fn set_text_hash(&mut self, node_index: usize, hash: u64) {
        self.text_hashes.insert(node_index, hash);
    }

    /// `true` if the node has no embedding yet, or its stored text hash
    /// differs from `current_hash` (the text changed since it was embedded),
    /// or it was embedded without a recorded hash. Drives `mode='changed'`.
    pub fn is_stale(&self, node_index: usize, current_hash: u64) -> bool {
        if !self.node_to_slot.contains_key(&node_index) {
            return true;
        }
        match self.text_hashes.get(&node_index) {
            Some(&stored) => stored != current_hash,
            None => true,
        }
    }

    /// A copy of everything this store serializes except the vectors — the
    /// compressed-section record of a store whose `data` travels as a mappable
    /// section. No index, no norms, no data.
    pub(crate) fn persist_shell(&self) -> EmbeddingStore {
        EmbeddingStore {
            dimension: self.dimension,
            data: VectorBuf::new(),
            node_to_slot: self.node_to_slot.clone(),
            slot_to_node: self.slot_to_node.clone(),
            metric: self.metric.clone(),
            model_id: self.model_id.clone(),
            text_hashes: self.text_hashes.clone(),
            norms: VectorBuf::new(),
            index: RwLock::new(None),
            freshness: empty_vector_freshness(),
            quant: None,
        }
    }

    /// Where the vectors live, as the inventory and diagnostics report it.
    pub fn storage_name(&self) -> &'static str {
        if self.is_mapped() {
            "mapped"
        } else {
            "heap"
        }
    }

    /// Whether `data` is served from a memory-mapped file rather than the heap.
    pub fn is_mapped(&self) -> bool {
        self.data.is_mapped()
    }

    /// Check mapped `data` and `norms` against their recorded CRC32 (the first
    /// call per mapping; later calls read the cached result) and that their
    /// file is intact. Heap stores are always `Ok`. Search, export and save
    /// call this before trusting a mapped store.
    pub fn verify_storage(&self) -> std::io::Result<()> {
        self.data.verify()?;
        self.norms.verify()?;
        match &self.quant {
            Some(quant) => quant.verify(),
            None => Ok(()),
        }
    }

    /// Copy mapped `data` and `norms` into the heap now, releasing this
    /// store's hold on the file.
    pub fn promote_to_heap(&mut self) {
        self.data.make_heap();
        self.norms.make_heap();
        if let Some(quant) = &mut self.quant {
            quant.promote_to_heap();
        }
    }

    /// Precision, oversample, placement and size of the quantized copy.
    pub fn quantization_info(&self) -> Option<crate::graph::quantized::QuantizationInfo> {
        self.quant.as_ref().map(QuantStore::info)
    }

    /// Precision of the quantized scan copy, if one is held.
    pub fn precision(&self) -> Option<Precision> {
        self.quant.as_ref().map(QuantStore::precision)
    }

    /// The quantized copy, when it is present and in step with the store.
    /// A copy whose length disagrees with the slot count (a mutation path
    /// that bypassed the maintenance hooks) is ignored, so a miss costs
    /// speed, never a wrong candidate set.
    pub(crate) fn quantized(&self) -> Option<&QuantStore> {
        self.quant.as_ref().filter(|quant| {
            quant.slot_count() == self.slot_to_node.len() && quant.dimension() == self.dimension
        })
    }

    /// A prepared scan of the quantized copy for `query` under `metric`;
    /// `None` when there is no usable copy or the metric cannot be ranked
    /// by it (Poincaré).
    pub(crate) fn quantized_scan<'a>(
        &'a self,
        metric: crate::graph::algorithms::vector::DistanceMetric,
        query: &'a [f32],
    ) -> Option<(QuantScan<'a>, u32)> {
        let quant = self.quantized()?;
        quant
            .scan(metric, query)
            .map(|scan| (scan, quant.oversample()))
    }

    /// Raw access for persistence and diagnostics.
    pub(crate) fn quant_store(&self) -> Option<&QuantStore> {
        self.quant.as_ref()
    }

    /// Install a quantized copy decoded from a file.
    pub(crate) fn attach_quant(&mut self, quant: QuantStore) {
        self.quant = Some(quant);
    }

    /// Build (or rebuild) the quantized scan copy from the `f32` vectors.
    /// Refuses a store whose declared metric is Poincaré, which a quantized
    /// dot product cannot rank. `oversample` defaults per precision.
    pub fn quantize(
        &mut self,
        precision: Precision,
        oversample: Option<u32>,
    ) -> Result<(), String> {
        if self.metric.as_deref() == Some("poincare") {
            return Err(
                "the poincare metric is non-linear in the vector norms, so a quantized copy cannot rank it; the store stays exact"
                    .to_string(),
            );
        }
        self.validate_shape().map_err(str::to_string)?;
        self.quant = Some(QuantStore::build(
            precision,
            &self.data,
            self.dimension,
            oversample,
        )?);
        Ok(())
    }

    /// Re-derive the quantized copy at its current precision after a bulk
    /// replacement of `data` that bypassed the per-slot hooks (compaction,
    /// carry). A no-op without a copy.
    pub(crate) fn requantize(&mut self) {
        if let Some(quant) = self.quant.take() {
            let rebuilt = QuantStore::build(
                quant.precision(),
                &self.data,
                self.dimension,
                Some(quant.oversample()),
            );
            self.quant = rebuilt.ok();
        }
    }

    /// Drop the quantized copy; `true` if one was held.
    pub fn drop_quantization(&mut self) -> bool {
        self.quant.take().is_some()
    }

    /// Change how many candidates a quantized scan fetches per result.
    pub fn set_oversample(&mut self, oversample: u32) -> Result<(), String> {
        match &mut self.quant {
            Some(quant) if oversample >= 1 => {
                quant.set_oversample(oversample);
                Ok(())
            }
            Some(_) => Err("oversample must be at least 1".to_string()),
            None => Err("the store has no quantized copy".to_string()),
        }
    }

    /// Add or replace an embedding for a node. Returns the slot index.
    /// Keeps the cached L2 norm (`norms[slot]`) in sync with `data`.
    ///
    /// **Does not drop the HNSW index.** Neither arm moves an existing slot:
    /// an append lands above the index's coverage and a replacement rewrites
    /// one slot's content, so both are recorded as freshness deltas and folded
    /// in by [`Self::refresh_index`] at query entry. Until they are, the index
    /// is *incomplete*, never wrong — it stores no vectors, so every distance
    /// it reports is computed from the buffer written here.
    pub fn set_embedding(&mut self, node_index: usize, embedding: &[f32]) -> usize {
        let norm = l2_norm_sq(embedding).sqrt();
        if let Some(&slot) = self.node_to_slot.get(&node_index) {
            let start = slot * self.dimension;
            self.data[start..start + self.dimension].copy_from_slice(embedding);
            self.norms[slot] = norm;
            if let Some(quant) = &mut self.quant {
                quant.set(slot, embedding);
            }
            self.freshness.note_changed(slot as u32);
            slot
        } else {
            let slot = self.slot_to_node.len();
            self.node_to_slot.insert(node_index, slot);
            self.slot_to_node.push(node_index);
            self.data.extend_from_slice(embedding);
            self.norms.push(norm);
            if let Some(quant) = &mut self.quant {
                quant.push(embedding);
            }
            self.freshness.note_created(slot as u32, true);
            slot
        }
    }

    /// Drop a node's embedding, returning what it held so a statement
    /// rollback can put it back exactly (see [`RemovedEmbedding`]).
    /// `None` when the node has no vector in this store.
    ///
    /// **Deleting a node must call this.** `StableDiGraph` reuses a freed
    /// `NodeIndex`, and the store is keyed by that index, so a vector left
    /// behind is inherited by the next node to land on the slot — a
    /// full-similarity hit for a node that was never embedded, of any type.
    ///
    /// Compacts by swapping the tail slot into the vacated one, so the store
    /// keeps the dense, hole-free layout every reader (and the `.kgl`
    /// serializer) already assumes; the slot layout changes, so the HNSW
    /// index is dropped.
    pub fn remove_embedding(&mut self, node_index: usize) -> Option<RemovedEmbedding> {
        let slot = self.node_to_slot.remove(&node_index)?;
        self.invalidate_index();
        let text_hash = self.text_hashes.remove(&node_index);
        let dim = self.dimension;
        let vector = self.data[slot * dim..(slot + 1) * dim].to_vec();
        let last = self.slot_to_node.len() - 1;
        if slot != last {
            let moved = self.slot_to_node[last];
            self.slot_to_node[slot] = moved;
            self.node_to_slot.insert(moved, slot);
            self.data
                .copy_within(last * dim..(last + 1) * dim, slot * dim);
            self.norms[slot] = self.norms[last];
        }
        self.slot_to_node.pop();
        self.data.truncate(last * dim);
        self.norms.pop();
        if let Some(quant) = &mut self.quant {
            quant.swap_remove(slot);
        }
        Some(RemovedEmbedding {
            slot,
            vector,
            text_hash,
        })
    }

    /// Exact inverse of [`Self::remove_embedding`] — the undo journal's
    /// restore half.
    ///
    /// Reverses the tail swap as well as the removal: whatever now occupies
    /// `removed.slot` is pushed back to the end and the restored vector takes
    /// its original slot. Replayed in reverse capture order (as the journal
    /// always replays), a statement's removals therefore rebuild the exact
    /// pre-statement slot layout, not merely the same set of vectors — which
    /// matters because slot order is scan order, and scan order decides
    /// score ties.
    pub fn restore_embedding(&mut self, node_index: usize, removed: &RemovedEmbedding) {
        self.invalidate_index();
        let dim = self.dimension;
        let slot = removed.slot;
        let len = self.slot_to_node.len();
        debug_assert!(slot <= len, "a restore may only refill the slot it vacated");
        let norm = l2_norm_sq(&removed.vector).sqrt();
        if slot < len {
            // Evict the current occupant to the tail the removal vacated.
            let moved = self.slot_to_node[slot];
            let moved_vector: Vec<f32> = self.data[slot * dim..(slot + 1) * dim].to_vec();
            self.slot_to_node.push(moved);
            self.node_to_slot.insert(moved, len);
            self.data.extend_from_slice(&moved_vector);
            self.norms.push(self.norms[slot]);
            self.data[slot * dim..(slot + 1) * dim].copy_from_slice(&removed.vector);
            self.norms[slot] = norm;
            self.slot_to_node[slot] = node_index;
            if let Some(quant) = &mut self.quant {
                quant.push_copy_of(slot);
                quant.set(slot, &removed.vector);
            }
        } else {
            self.slot_to_node.push(node_index);
            self.data.extend_from_slice(&removed.vector);
            self.norms.push(norm);
            if let Some(quant) = &mut self.quant {
                quant.push(&removed.vector);
            }
        }
        self.node_to_slot.insert(node_index, slot);
        if let Some(hash) = removed.text_hash {
            self.text_hashes.insert(node_index, hash);
        }
    }

    /// Everything an undo needs to reverse one in-place [`Self::set_embedding`]:
    /// the vector it is about to overwrite, that vector's slot and text hash,
    /// and the slot's index coverage. `None` when the store holds no vector
    /// for `node_index` — the write will append, and
    /// [`Self::pop_appended_embedding`] is that write's inverse.
    ///
    /// O(dimension), never O(store): this is what lets a manual relationship
    /// vector write be journalled per changed cell rather than by cloning the
    /// store.
    pub(crate) fn cell_pre_image(
        &self,
        node_index: usize,
    ) -> Option<(RemovedEmbedding, SlotCoverage)> {
        let &slot = self.node_to_slot.get(&node_index)?;
        let start = slot * self.dimension;
        Some((
            RemovedEmbedding {
                slot,
                vector: self.data[start..start + self.dimension].to_vec(),
                text_hash: self.text_hashes.get(&node_index).copied(),
            },
            self.freshness.capture_slot(slot as u32),
        ))
    }

    /// Reverse an in-place [`Self::set_embedding`] with the pre-image
    /// [`Self::cell_pre_image`] took.
    ///
    /// Distinct from [`Self::restore_embedding`], which refills a slot a
    /// removal *vacated*: an overwrite never vacated one, so reversing it
    /// through that path would evict this node's own row to the tail and
    /// leave the store holding it twice.
    ///
    /// The HNSW index is deliberately not invalidated. `set_embedding` did not
    /// drop it either, and the index stores no vectors — its links address the
    /// buffer this writes back — so the pre-statement index is exactly right
    /// again once the slot's coverage is restored.
    pub(crate) fn restore_cell(
        &mut self,
        node_index: usize,
        prior: &RemovedEmbedding,
        coverage: SlotCoverage,
    ) {
        debug_assert_eq!(
            self.node_to_slot.get(&node_index),
            Some(&prior.slot),
            "an overwrite undo may only write back into the slot it overwrote"
        );
        self.set_embedding(node_index, &prior.vector);
        match prior.text_hash {
            Some(hash) => {
                self.text_hashes.insert(node_index, hash);
            }
            None => {
                self.text_hashes.remove(&node_index);
            }
        }
        self.freshness.restore_slot(prior.slot as u32, coverage);
    }

    /// Reverse an appending [`Self::set_embedding`]: pop the tail slot the
    /// append pushed.
    ///
    /// An append always lands at or above the watermark (the watermark is only
    /// raised to the store's length, and every shrink invalidates the index and
    /// resets it to zero), so the popped row is one no index covers and the
    /// index survives. The guard exists for the one way that can stop being
    /// true — a refresh *inside the same statement* folding the appended slot
    /// in — where the index would otherwise address a row that is gone.
    pub(crate) fn pop_appended_embedding(&mut self, node_index: usize) {
        let Some(&slot) = self.node_to_slot.get(&node_index) else {
            return;
        };
        debug_assert_eq!(
            slot + 1,
            self.slot_to_node.len(),
            "appended cells are undone in reverse capture order, tail first"
        );
        if slot + 1 != self.slot_to_node.len() {
            // Unreachable while replay stays LIFO, but truncating the buffer
            // under a live row is not a failure mode worth leaving open: take
            // the tail swap (and the index drop that comes with it) instead.
            self.remove_embedding(node_index);
            return;
        }
        self.node_to_slot.remove(&node_index);
        self.text_hashes.remove(&node_index);
        self.slot_to_node.pop();
        self.data.truncate(slot * self.dimension);
        self.norms.pop();
        if let Some(quant) = &mut self.quant {
            quant.pop();
        }
        if self.freshness.watermark() as usize > slot {
            self.invalidate_index();
        }
    }

    /// Restore the aggregate generated-model stamp a manual write cleared.
    pub(crate) fn set_model_id(&mut self, model_id: Option<String>) {
        self.model_id = model_id;
    }

    /// Get the embedding slice for a node (by NodeIndex.index()).
    #[inline]
    pub fn get_embedding(&self, node_index: usize) -> Option<&[f32]> {
        self.node_to_slot.get(&node_index).map(|&slot| {
            let start = slot * self.dimension;
            &self.data[start..start + self.dimension]
        })
    }

    /// Get the embedding slice and its cached L2 norm for a node, in one lookup.
    /// The norm lets cosine scoring skip recomputing the stored vector's norm.
    #[inline]
    pub fn get_embedding_with_norm(&self, node_index: usize) -> Option<(&[f32], f32)> {
        self.node_to_slot.get(&node_index).map(|&slot| {
            let start = slot * self.dimension;
            (&self.data[start..start + self.dimension], self.norms[slot])
        })
    }

    /// Drop any HNSW index. The whole store becomes the outstanding delta —
    /// which is what it is: nothing is accelerated until a build covers it.
    #[inline]
    pub fn invalidate_index(&mut self) {
        *self.index.get_mut().unwrap_or_else(|e| e.into_inner()) = None;
        self.freshness = IndexFreshness::covering(0, Some(self.freshness.limit()));
    }

    pub(crate) fn take_index_state(&mut self) -> VectorIndexState {
        let limit = self.freshness.limit();
        let index = self
            .index
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let freshness = std::mem::replace(
            &mut self.freshness,
            IndexFreshness::covering(0, Some(limit)),
        );
        VectorIndexState { index, freshness }
    }

    pub(crate) fn restore_index_state(&mut self, state: VectorIndexState) {
        *self
            .index
            .get_mut()
            .unwrap_or_else(|error| error.into_inner()) = state.index;
        self.freshness = state.freshness;
    }

    #[inline]
    pub fn has_index(&self) -> bool {
        self.index_guard().is_some()
    }

    fn index_guard(
        &self,
    ) -> RwLockReadGuard<'_, Option<crate::graph::algorithms::hnsw::HnswIndex>> {
        // The state behind this lock is a rebuildable cache, so a poisoned
        // lock is recovered rather than propagated into a query.
        self.index.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Store slots the HNSW index currently covers; `0` when none is built.
    pub fn indexed_slots(&self) -> usize {
        self.index_guard().as_ref().map_or(0, |idx| idx.len())
    }

    /// Vectors the next catch-up would (re-)read: the slots appended since the
    /// last build plus the ones replaced in place. An upper bound, O(1) — see
    /// [`crate::graph::index_freshness`]. Equals the store's size when no index
    /// is built, since none of it is covered.
    pub fn delta_size(&self) -> usize {
        self.freshness.delta_size(self.len() as u32)
    }

    /// Whether the vectors have moved past what the index covers.
    pub fn index_is_stale(&self) -> bool {
        self.freshness.is_stale(self.len() as u32)
    }

    /// The inline-refresh ceiling in vectors.
    pub fn auto_refresh_limit(&self) -> usize {
        self.freshness.limit()
    }

    /// Whether the outstanding delta is small enough to fold in at query entry.
    /// `false` for a current index — there is nothing to fold in.
    pub fn can_auto_refresh(&self) -> bool {
        self.freshness.within_limit(self.len() as u32)
    }

    /// Fold every outstanding vector into the HNSW index, returning how many
    /// slots it touched. `0` when nothing is outstanding — or when no index is
    /// built, because **catch-up never builds one**: the index is opt-in, and a
    /// query that quietly constructed one would spend a corpus-sized build the
    /// caller never asked for.
    ///
    /// O(delta): appended slots go in through the incremental insert, replaced
    /// ones through [`HnswIndex::relink`]. A failure at any point drops the
    /// index rather than leaving topology whose coverage no longer matches the
    /// watermark — the store still answers by exact scan, and `build_vector_index`
    /// restores acceleration.
    pub fn refresh_index(&self) -> usize {
        // The index lock is taken *before* the delta is claimed: that ordering
        // is what makes a second reader either wait for this refresh or find
        // nothing left to do (`index_freshness`'s lock-order note).
        let mut guard = self.index.write().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            return 0;
        }
        let Some(delta) = self.freshness.take_delta(self.len() as u32) else {
            return 0;
        };
        let index = guard.as_mut().expect("presence checked above");
        let mut seen = 0usize;
        let mut failed = false;
        // Ascending, and appends after replacements: `insert` only accepts the
        // next contiguous slot, so the walk order is a correctness condition,
        // not a preference. `FreshnessDelta::slots` yields the replaced slots
        // (all below the old watermark) before the appended run.
        let mut slots: Vec<u32> = delta.slots().collect();
        slots.sort_unstable();
        for slot in slots {
            seen += 1;
            let outcome = if (slot as usize) < index.len() {
                index.relink(slot, &self.data, &self.norms, self.dimension)
            } else {
                index.insert(slot, &self.data, &self.norms, self.dimension)
            };
            if outcome.is_err() {
                failed = true;
                break;
            }
        }
        if failed {
            *guard = None;
            return seen;
        }
        debug_assert!(
            index
                .validate_for_store(&self.data, &self.norms, self.dimension)
                .is_ok(),
            "a caught-up HNSW index must still validate against its store: {:?}",
            index.validate_for_store(&self.data, &self.norms, self.dimension)
        );
        seen
    }

    /// The index to serve a query with, catching it up first when the
    /// outstanding delta is small enough to fold in inline.
    ///
    /// `None` means "score by exact scan": no index, a read-only graph that may
    /// not write one, or a delta over the ceiling. All three are *correct*
    /// answers rather than degraded ones — the exact path is the oracle the
    /// approximate one is measured against — so a stale vector index costs
    /// speed, never accuracy. That is why this lane serves stale-but-right
    /// where the text lane has to serve nulls.
    pub fn index_for_query(&self, read_only: bool) -> Option<HnswRead<'_>> {
        if self.index_is_stale() {
            if read_only || !self.can_auto_refresh() {
                return None;
            }
            self.refresh_index();
            if self.index_is_stale() {
                return None;
            }
        }
        let guard = self.index_guard();
        guard.is_some().then(|| HnswRead(guard))
    }

    /// The index as it stands, without catching it up — what persistence
    /// writes. Distinct from [`Self::index_for_query`], which exists to catch
    /// up: saving must record what is actually there, not perform a refresh as
    /// a side effect of `save_graph`.
    pub(crate) fn index_read(&self) -> Option<HnswRead<'_>> {
        let guard = self.index_guard();
        guard.is_some().then(|| HnswRead(guard))
    }

    /// Mutable access to the index for tests that need to corrupt it — the
    /// persistence validators can only be shown to work against topology that
    /// is actually broken.
    #[cfg(test)]
    pub(crate) fn index_mut_for_test(
        &mut self,
    ) -> Option<&mut crate::graph::algorithms::hnsw::HnswIndex> {
        self.index
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
    }

    /// Move the inline-refresh ceiling.
    pub fn set_auto_refresh_limit(&mut self, limit: usize) {
        self.freshness = IndexFreshness::covering(self.freshness.watermark(), Some(limit));
    }

    /// Attach a persisted index that already covers `covered` slots, with the
    /// freshness state it was saved with. Persistence-only: an index arriving
    /// any other way is built here and covers everything by construction.
    pub(crate) fn attach_persisted_index(
        &mut self,
        index: crate::graph::algorithms::hnsw::HnswIndex,
        freshness: IndexFreshness,
    ) {
        *self.index.get_mut().unwrap_or_else(|e| e.into_inner()) = Some(index);
        self.freshness = freshness;
    }

    /// The freshness state to persist alongside the index.
    pub(crate) fn freshness_state(&self) -> &IndexFreshness {
        &self.freshness
    }

    /// Build (or rebuild) an HNSW index over this store's vectors for `metric`.
    /// Errors for Poincaré (unsupported by HNSW — stays brute-force). Ensures
    /// the norm cache is populated first (cosine navigation needs it).
    pub fn build_index(
        &mut self,
        metric: crate::graph::algorithms::vector::DistanceMetric,
        params: crate::graph::algorithms::hnsw::HnswParams,
        seed: u64,
    ) -> Result<(), String> {
        let hm = crate::graph::algorithms::hnsw::HnswMetric::from_distance(metric).ok_or_else(
            || "HNSW does not support the Poincaré metric; it stays on the exact (brute-force) path.".to_string(),
        )?;
        if self.dimension == 0 {
            return Err(
                "HNSW index construction requires a non-zero embedding dimension".to_string(),
            );
        }
        params.validate().map_err(str::to_string)?;
        self.validate_shape().map_err(str::to_string)?;
        if self.norms.len() != self.slot_to_node.len() {
            self.rebuild_norms();
        }
        let built = crate::graph::algorithms::hnsw::HnswIndex::build(
            &self.data,
            &self.norms,
            self.dimension,
            hm,
            params,
            seed,
        );
        let limit = self.freshness.limit();
        *self.index.get_mut().unwrap_or_else(|e| e.into_inner()) = Some(built);
        self.freshness = IndexFreshness::covering(self.len() as u32, Some(limit));
        Ok(())
    }

    /// Validate the serialized, parallel embedding-store columns before any
    /// derived cache indexes into them. Persistence callers must run this
    /// before [`Self::rebuild_norms`] or
    /// [`Self::rebuild_norms_checked`] so malformed cardinalities become a
    /// load error rather than a slice panic.
    pub(crate) fn validate_shape(&self) -> Result<(), &'static str> {
        let expected_data_len = self
            .slot_to_node
            .len()
            .checked_mul(self.dimension)
            .ok_or("embedding data cardinality overflows usize")?;
        if self.data.len() != expected_data_len {
            return Err("embedding data cardinality does not match its slot count");
        }
        if self.node_to_slot.len() != self.slot_to_node.len() {
            return Err("embedding node/slot maps have different cardinalities");
        }
        for (slot, &node) in self.slot_to_node.iter().enumerate() {
            if self.node_to_slot.get(&node) != Some(&slot) {
                return Err("embedding node/slot maps are not a bijection");
            }
        }
        Ok(())
    }

    /// Recompute every cached norm from `data`. Call after any wholesale
    /// replacement of `data` that bypasses `set_embedding` — deserialization
    /// (`norms` is `#[serde(skip)]`, so it loads empty) and slot remapping
    /// during compaction. Idempotent.
    pub fn rebuild_norms(&mut self) {
        let n = self.slot_to_node.len();
        self.norms.clear();
        self.norms.reserve(n);
        for slot in 0..n {
            let start = slot * self.dimension;
            self.norms
                .push(l2_norm_sq(&self.data[start..start + self.dimension]).sqrt());
        }
    }

    /// [`Self::rebuild_norms`] that also refuses a non-finite coordinate, in
    /// the same pass over `data`: a vector's squared norm is finite whenever
    /// all its coordinates are, so only a vector whose norm is not — a
    /// non-finite coordinate, or finite ones that overflow — is scanned
    /// again. The error names the coordinate's index in `data`, as
    /// [`validate_finite_vector`](crate::graph::embedding_validation::validate_finite_vector)
    /// over the whole of `data` does. Loaders call this instead of the two
    /// separate passes; `norms` is left partial on error.
    pub(crate) fn rebuild_norms_checked(
        &mut self,
    ) -> Result<(), crate::graph::embedding_validation::NonFiniteVector> {
        let n = self.slot_to_node.len();
        self.norms.clear();
        self.norms.reserve(n);
        for slot in 0..n {
            let start = slot * self.dimension;
            let vector = &self.data[start..start + self.dimension];
            let norm_sq = l2_norm_sq(vector);
            if !norm_sq.is_finite() {
                crate::graph::embedding_validation::validate_finite_vector(vector).map_err(
                    |error| crate::graph::embedding_validation::NonFiniteVector {
                        index: start + error.index,
                        value: error.value,
                    },
                )?;
            }
            self.norms.push(norm_sq.sqrt());
        }
        Ok(())
    }

    #[inline]
    // An `is_empty()` companion would add public API the curated surface does not want.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.slot_to_node.len()
    }
}
