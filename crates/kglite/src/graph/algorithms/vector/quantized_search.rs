//! Two-stage search over a store's quantized scan copy.
//!
//! Stage one ranks the candidates by the copy's approximate score and keeps
//! `top_k × oversample`; stage two re-scores that pool against the canonical
//! `f32` vectors with the requested metric. Every returned score is therefore
//! exact, and the only approximation is *which* candidates reach stage two —
//! the same contract as the HNSW route, which this route yields to when an
//! index serves the query.

use super::{MinHeap, Scorer, VectorSearchResult, PARALLEL_THRESHOLD};
use crate::graph::quantized::QuantScan;
use crate::graph::schema::EmbeddingStore;
use petgraph::graph::NodeIndex;
use rayon::prelude::*;

use super::DistanceMetric;

/// Top-k of `candidates` through the store's quantized copy, or `None` when
/// the exact scan should answer: no usable copy, a metric the copy cannot
/// rank, or an oversampled pool at least as large as the candidate set (the
/// scan would touch every vector anyway, so there is nothing to save).
pub(super) fn search(
    candidates: &[NodeIndex],
    store: &EmbeddingStore,
    metric: DistanceMetric,
    query: &[f32],
    top_k: usize,
    scorer: &Scorer,
) -> Option<Vec<VectorSearchResult>> {
    let (scan, oversample) = store.quantized_scan(metric, query)?;
    let pool = top_k.saturating_mul(oversample as usize);
    if pool >= candidates.len() {
        return None;
    }
    let coarse = coarse_nodes(candidates, store, &scan, pool);
    let mut heap = MinHeap::with_capacity(top_k);
    for node in coarse {
        if let Some((embedding, norm)) = store.get_embedding_with_norm(node.index()) {
            heap.push_if_better(node, scorer.score(query, embedding, norm), top_k);
        }
    }
    Some(heap.into_sorted_results())
}

fn coarse_chunk(
    chunk: &[NodeIndex],
    store: &EmbeddingStore,
    scan: &QuantScan<'_>,
    pool: usize,
) -> Vec<VectorSearchResult> {
    let mut heap = MinHeap::with_capacity(pool);
    for &node in chunk {
        if let Some(&slot) = store.node_to_slot.get(&node.index()) {
            heap.push_if_better(node, scan.score(slot, store.norms[slot]), pool);
        }
    }
    heap.into_sorted_results()
}

fn coarse_nodes(
    candidates: &[NodeIndex],
    store: &EmbeddingStore,
    scan: &QuantScan<'_>,
    pool: usize,
) -> Vec<NodeIndex> {
    if candidates.len() <= PARALLEL_THRESHOLD {
        return coarse_chunk(candidates, store, scan, pool)
            .into_iter()
            .map(|r| r.node_idx)
            .collect();
    }
    let chunk = (candidates.len() / rayon::current_num_threads()).max(1024);
    let partial: Vec<Vec<VectorSearchResult>> = candidates
        .par_chunks(chunk)
        .map(|part| coarse_chunk(part, store, scan, pool))
        .collect();
    let mut heap = MinHeap::with_capacity(pool);
    for result in partial.into_iter().flatten() {
        heap.push_if_better(result.node_idx, result.score, pool);
    }
    heap.into_sorted_results()
        .into_iter()
        .map(|r| r.node_idx)
        .collect()
}

/// The best `pool` slots of the whole store by approximate score — the
/// relationship search's stage one, which scans every slot.
pub(crate) fn coarse_slots(
    store: &EmbeddingStore,
    scan: &QuantScan<'_>,
    pool: usize,
) -> Vec<usize> {
    let slots: Vec<NodeIndex> = (0..store.len()).map(NodeIndex::new).collect();
    let rank = |chunk: &[NodeIndex]| {
        let mut heap = MinHeap::with_capacity(pool);
        for &slot in chunk {
            let slot = slot.index();
            heap.push_if_better(
                NodeIndex::new(slot),
                scan.score(slot, store.norms[slot]),
                pool,
            );
        }
        heap.into_sorted_results()
    };
    let merged: Vec<VectorSearchResult> = if slots.len() <= PARALLEL_THRESHOLD {
        rank(&slots)
    } else {
        let chunk = (slots.len() / rayon::current_num_threads()).max(1024);
        let partial: Vec<Vec<VectorSearchResult>> = slots.par_chunks(chunk).map(rank).collect();
        let mut heap = MinHeap::with_capacity(pool);
        for result in partial.into_iter().flatten() {
            heap.push_if_better(result.node_idx, result.score, pool);
        }
        heap.into_sorted_results()
    };
    merged.into_iter().map(|r| r.node_idx.index()).collect()
}
