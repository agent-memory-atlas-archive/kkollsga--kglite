//! Vector top-k over the vectors a valid-time filter admits, shared by the
//! Cypher `vector_score` retrieval route and the embedding-query procedures.
//!
//! Below [`MASKED_EXACT_MAX`] admitted vectors a caller scores every admitted
//! slot exactly — no row is materialised, and a non-admitted slot costs one
//! test. At or above it, with an HNSW index serving the metric,
//! [`hnsw_admitted`] searches the index and keeps the admitted candidates,
//! over-fetching until it holds `k` of them or the index has nothing more to
//! give; short of `k` it declines, and the caller scores exactly. The
//! crossover follows the measured one: an exact pass over the admitted
//! vectors beats a per-instant index until the admitted set is large.

use crate::graph::schema::EmbeddingStore;
use crate::graph::schema::HnswRead;

/// Admitted vectors at or above which a masked query tries the store's HNSW
/// index before an exact pass. [`MASKED_EXACT_MAX_ENV`] overrides it.
pub const MASKED_EXACT_MAX: usize = 200_000;

/// Environment variable that replaces [`MASKED_EXACT_MAX`], read per query.
pub(crate) const MASKED_EXACT_MAX_ENV: &str = "KGLITE_TEMPORAL_VECTOR_EXACT_MAX";

/// The first over-fetch is this many times the admitted share of `k`.
const OVERFETCH: usize = 2;
/// Each retry fetches this many times more candidates.
const OVERFETCH_GROWTH: usize = 4;

/// Whether a masked query over `admitted` vectors should try the index.
pub(crate) fn prefers_index(admitted: usize) -> bool {
    let threshold = std::env::var(MASKED_EXACT_MAX_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(MASKED_EXACT_MAX);
    admitted >= threshold
}

/// How many of `store`'s slots hold a node or relationship `admits` keeps.
pub(crate) fn admitted_slots(store: &EmbeddingStore, admits: &dyn Fn(usize) -> bool) -> usize {
    store
        .slot_to_node
        .iter()
        .filter(|&&target| admits(target))
        .count()
}

/// The admitted slots among `index`'s nearest candidates to `query`, at least
/// `k` of them (or every admitted slot, when fewer than `k` exist), in the
/// index's order; `None` when the index gave up short of that, and an exact
/// pass must answer. `admitted` sizes the first over-fetch.
pub(crate) fn hnsw_admitted(
    store: &EmbeddingStore,
    index: &HnswRead<'_>,
    query: &[f32],
    k: usize,
    admitted: usize,
    admits: &dyn Fn(usize) -> bool,
) -> Option<Vec<u32>> {
    let len = store.len();
    let want = k.min(admitted);
    if want == 0 {
        return Some(Vec::new());
    }
    let query_norm = crate::graph::algorithms::vector::dot_product(query, query).sqrt();
    let share = k.saturating_mul(len).div_ceil(admitted.max(1));
    let mut fetch = share.saturating_mul(OVERFETCH).clamp(k, len);
    loop {
        let ef = fetch.max(index.params().ef_search);
        let raw = index.search(
            query,
            query_norm,
            fetch,
            Some(ef),
            &store.data,
            &store.norms,
        );
        let kept: Vec<u32> = raw
            .into_iter()
            .map(|(slot, _)| slot)
            .filter(|&slot| admits(store.slot_to_node[slot as usize]))
            .collect();
        if kept.len() >= want {
            return Some(kept);
        }
        if fetch >= len {
            return None;
        }
        fetch = fetch.saturating_mul(OVERFETCH_GROWTH).min(len);
    }
}
