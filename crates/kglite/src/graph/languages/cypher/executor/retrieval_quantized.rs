//! Whole-store and masked `vector_score` top-k through a quantized scan copy.
//!
//! The same two-stage contract as the API search and the relationship routes
//! (`algorithms::vector::pool_slots`): the copy ranks `limit × oversample`
//! candidates, those are re-scored against the `f32` vectors with the
//! statement's own scorer, so every returned score is exact and only the
//! candidate set is approximate. An index serves first and `exact: true`
//! bypasses the copy; the callers decide both before asking here.

use super::*;
use crate::graph::algorithms::vector::{pool_slots, Admitted};
use crate::graph::schema::EmbeddingStore;

impl CypherExecutor<'_> {
    /// The best `limit` slots as `(slot, score)`, best first with ties in slot
    /// order, and whether the quantized copy produced them. Falls back to
    /// [`Self::exact_vector_winners`] when `allow_quantized` is false, the
    /// store has no usable copy for the metric, or the pool would cover every
    /// slot in play.
    pub(super) fn vector_winners(
        &self,
        store: &EmbeddingStore,
        prepared: &VectorScoreCache,
        limit: usize,
        admitted: Option<Admitted<'_>>,
        allow_quantized: bool,
    ) -> Result<(Vec<(usize, Value)>, bool), String> {
        if allow_quantized {
            self.check_deadline()?;
            if let Some(winners) = quantized_winners(store, prepared, limit, admitted) {
                return Ok((winners, true));
            }
        }
        let winners =
            self.exact_vector_winners(store, prepared, limit, admitted.map(|(admits, _)| admits))?;
        Ok((winners, false))
    }
}

fn quantized_winners(
    store: &EmbeddingStore,
    prepared: &VectorScoreCache,
    limit: usize,
    admitted: Option<Admitted<'_>>,
) -> Option<Vec<(usize, Value)>> {
    let pool = pool_slots(store, prepared.metric, &prepared.query_vec, limit, admitted)?;
    let mut scored: Vec<(usize, f32)> = pool
        .into_iter()
        .map(|slot| {
            let start = slot * store.dimension;
            let score = prepared.scorer.score(
                &prepared.query_vec,
                &store.data[start..start + store.dimension],
                store.norms[slot],
            );
            (slot, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(limit);
    Some(
        scored
            .into_iter()
            .map(|(slot, score)| (slot, Value::Float64(score as f64)))
            .collect(),
    )
}
