//! Vector top-k over the vectors a valid-time filter admits, shared by the
//! Cypher `vector_score` retrieval route and the embedding-query procedures.
//!
//! Two routes, one rule. The exact pass scores every admitted slot — no row
//! is materialised, and a non-admitted slot costs one admit test. The
//! filtered search ([`hnsw_admitted`]) walks the store's HNSW index through
//! every slot but returns only admitted ones, so its cost grows as the
//! admitted share shrinks (about `1 / share` more steps than an unfiltered
//! search), while the exact pass grows with the admitted count and, more
//! slowly, the store. The rule compares [`weighted_admitted`] — the admitted
//! count times the admitted share — with [`MASKED_EXACT_MAX`]: at or above
//! it, and with a fresh index serving the metric, the search answers;
//! otherwise, or when the search passes its distance budget, the exact pass.
//!
//! Measured 2026-09 (release, 64-d random vectors, uniformly scattered
//! masks, top-10, whole-query medians): the search overtook the exact pass
//! at a weighted count of about 350 on a 5k store, 1,400–1,500 on 20k–50k,
//! and 2,500–2,800 on 130k–300k.

use crate::graph::schema::EmbeddingStore;
use crate::graph::schema::HnswRead;

/// The [`weighted_admitted`] count at or above which a masked query searches
/// the store's HNSW index instead of scoring every admitted vector; the
/// middle of the measured crossovers (module docs). [`MASKED_EXACT_MAX_ENV`]
/// overrides it (`1` sends every masked query with an index to the search).
pub const MASKED_EXACT_MAX: usize = 1_500;

/// Environment variable that replaces [`MASKED_EXACT_MAX`], read per query.
pub(crate) const MASKED_EXACT_MAX_ENV: &str = "KGLITE_TEMPORAL_VECTOR_EXACT_MAX";

/// A filtered search's budget is `admitted + store_len / VISIT_STORE_DIVISOR`
/// search steps, never below [`VISIT_FLOOR`] — about twice the exact pass's
/// cost counted in steps (measured 2026-09, release, 64-d vectors: an exact
/// score ~13 ns, an admit test ~2 ns, a search step ~27 ns). Near the
/// crossover a search needs about the exact pass's cost, so the budget leaves
/// it room; an admitted set it cannot reach cheaply — valid vectors clustered
/// away from the query — gives way and costs at most about three exact passes.
const VISIT_STORE_DIVISOR: usize = 8;

/// The smallest budget: a search this short costs microseconds whatever the
/// store.
const VISIT_FLOOR: usize = 1_024;

/// The route reason when a filtered search passed its visit budget and the
/// exact pass answered.
pub(crate) const VISIT_LIMIT_REASON: &str = "exact_mask_visit_limit";

/// Whether a masked query over `admitted` of a store's `store_len` vectors
/// should search the index: when [`weighted_admitted`] reaches the threshold.
pub(crate) fn prefers_index(admitted: usize, store_len: usize) -> bool {
    let threshold = std::env::var(MASKED_EXACT_MAX_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(MASKED_EXACT_MAX);
    weighted_admitted(admitted, store_len) >= threshold
}

/// `admitted` weighted by the admitted share of the store,
/// `admitted² / store_len` rounded up — so it is at least 1 whenever a vector
/// is admitted.
pub(crate) fn weighted_admitted(admitted: usize, store_len: usize) -> usize {
    let weighted = (admitted as u128 * admitted as u128).div_ceil(store_len.max(1) as u128);
    usize::try_from(weighted).unwrap_or(usize::MAX)
}

/// How many of `store`'s slots hold a node or relationship `admits` keeps.
pub(crate) fn admitted_slots(store: &EmbeddingStore, admits: &dyn Fn(usize) -> bool) -> usize {
    store
        .slot_to_node
        .iter()
        .filter(|&&target| admits(target))
        .count()
}

/// The admitted slots nearest `query`, at least `k` of them (or every
/// admitted slot, when fewer than `k` exist), nearest first, from one
/// filtered HNSW search ([`HnswIndex::search_filtered`]) at the index's
/// `ef_search`; `None` when the search spends its budget
/// ([`VISIT_STORE_DIVISOR`]) or comes back short, and the exact pass must
/// answer. `admitted` is the admitted slot count.
///
/// [`HnswIndex::search_filtered`]: crate::graph::algorithms::hnsw::HnswIndex::search_filtered
pub(crate) fn hnsw_admitted(
    store: &EmbeddingStore,
    index: &HnswRead<'_>,
    query: &[f32],
    k: usize,
    admitted: usize,
    admits: &dyn Fn(usize) -> bool,
) -> Option<Vec<u32>> {
    // A `k` past the store (a LIMIT or `top_k` larger than it) asks for every
    // admitted slot.
    let want = k.min(admitted).min(store.len());
    if want == 0 {
        return Some(Vec::new());
    }
    let query_norm = crate::graph::algorithms::vector::dot_product(query, query).sqrt();
    let accept = |slot: u32| admits(store.slot_to_node[slot as usize]);
    let budget = (admitted + store.len() / VISIT_STORE_DIVISOR).max(VISIT_FLOOR);
    let found = index.search_filtered(
        (query, query_norm),
        (want, None),
        (&store.data, &store.norms),
        &accept,
        budget,
    )?;
    (found.len() >= want).then(|| found.into_iter().map(|(slot, _)| slot).collect())
}

#[cfg(test)]
mod tests {
    use super::weighted_admitted;

    #[test]
    fn the_weighted_count_is_the_admitted_count_times_the_admitted_share() {
        assert_eq!(weighted_admitted(26_000, 130_000), 5_200);
        assert_eq!(weighted_admitted(2_600, 13_000), 520);
        assert_eq!(weighted_admitted(4_000, 4_000), 4_000);
        // Rounded up: one admitted vector weighs at least 1, so a threshold
        // of 1 sends every masked query with an admitted vector to the index.
        assert_eq!(weighted_admitted(1, 1_000_000), 1);
        assert_eq!(weighted_admitted(0, 1_000), 0);
        assert_eq!(weighted_admitted(usize::MAX, 1), usize::MAX);
    }
}
