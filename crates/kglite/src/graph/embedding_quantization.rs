//! Declaring a quantized scan copy on an embedding store, and measuring how
//! well it ranks.
//!
//! [`quantize_embeddings`] is the one entry the bindings call to build, replace
//! or drop a copy on a node or relationship store; [`quantization_recall`]
//! answers "what does this precision cost me on *my* vectors" by replaying
//! stored vectors as queries against the exact `f32` ranking.

use rayon::prelude::*;

use crate::graph::algorithms::vector::{DistanceMetric, Scorer};
use crate::graph::embedding_hints::{missing_store_error, Surface};
use crate::graph::embedding_inventory::EmbeddingEntity;
use crate::graph::embeddings::store_key;
use crate::graph::quantized::{Precision, QuantizationInfo};
use crate::graph::schema::{DirGraph, EmbeddingStore};

fn store_mut<'g>(
    graph: &'g mut DirGraph,
    entity: EmbeddingEntity,
    type_name: &str,
    text_column: &str,
) -> Result<&'g mut EmbeddingStore, String> {
    // Resolved first, shared, so the refusal can read the whole graph.
    store_ref(graph, entity, type_name, text_column)?;
    let key = store_key(type_name, text_column);
    Ok(match entity {
        EmbeddingEntity::Node => graph.embeddings.get_mut(&key),
        EmbeddingEntity::Relationship => graph
            .edge_embeddings
            .get_mut(&key)
            .map(|store| store.numeric_mut()),
    }
    .expect("the store was found above"))
}

fn store_ref<'g>(
    graph: &'g DirGraph,
    entity: EmbeddingEntity,
    type_name: &str,
    text_column: &str,
) -> Result<&'g EmbeddingStore, String> {
    let key = store_key(type_name, text_column);
    let found = match entity {
        EmbeddingEntity::Node => graph.embeddings.get(&key),
        EmbeddingEntity::Relationship => graph.edge_embeddings.get(&key).map(|s| s.numeric()),
    };
    found.ok_or_else(|| missing_store_error(graph, entity, type_name, text_column, Surface::Method))
}

/// Whether the `entity` store `(type_name, "{text_column}_emb")` exists.
pub fn embedding_store_exists(
    graph: &DirGraph,
    entity: EmbeddingEntity,
    type_name: &str,
    text_column: &str,
) -> bool {
    store_ref(graph, entity, type_name, text_column).is_ok()
}

/// What [`quantize_embeddings`] did to a store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct QuantizationReport {
    /// The copy the store holds now; `None` after a drop.
    pub quantization: Option<QuantizationInfo>,
    /// The precision the store held before the call.
    pub previous: Option<&'static str>,
    /// Vectors in the store.
    pub count: usize,
    pub dimension: usize,
}

/// Build, replace or drop the quantized scan copy of the `entity` store
/// `(type_name, "{text_column}_emb")`.
///
/// `Some(precision)` derives the copy from the `f32` vectors (replacing any
/// existing one); `oversample` is how many candidates a scan keeps per
/// requested result and defaults per precision. `None` drops the copy and
/// reports `quantization: None`; dropping a store with no copy is a no-op, and
/// an `oversample` without a precision is refused. The `f32` vectors are never
/// touched. Refuses a missing store, a Poincare-metric store and a zero or
/// over-wide dimension.
pub fn quantize_embeddings(
    graph: &mut DirGraph,
    entity: EmbeddingEntity,
    type_name: &str,
    text_column: &str,
    precision: Option<Precision>,
    oversample: Option<u32>,
) -> Result<QuantizationReport, String> {
    let store = store_mut(graph, entity, type_name, text_column)?;
    let previous = store.precision().map(Precision::name);
    match precision {
        Some(precision) => store.quantize(precision, oversample)?,
        None if oversample.is_some() => {
            return Err(
                "oversample needs a precision; pass precision='f16', 'int8' or 'binary'"
                    .to_string(),
            )
        }
        None => {
            store.drop_quantization();
        }
    }
    let report = QuantizationReport {
        quantization: store.quantization_info(),
        previous,
        count: store.len(),
        dimension: store.dimension,
    };
    graph.bump_version();
    Ok(report)
}

/// How a quantized copy ranks against the exact `f32` order, measured on the
/// store's own vectors.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct QuantizationRecall {
    pub precision: &'static str,
    /// Candidates kept per result (the store's, or the override).
    pub oversample: u32,
    /// Stored vectors replayed as queries.
    pub sampled: usize,
    pub top_k: usize,
    /// Mean overlap of the quantized top-`top_k` with the exact top-`top_k`,
    /// before any re-scoring.
    pub recall: f64,
    /// Mean overlap after the search's second stage: the exact re-score of the
    /// `top_k × oversample` candidates. This is the recall a search returns.
    pub recall_rescored: f64,
}

/// How [`quantization_recall`] samples and measures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecallOptions {
    /// Stored vectors to replay as queries (capped at the store size).
    pub sample: usize,
    /// Results per query (capped at the store size minus one).
    pub top_k: usize,
    /// Measure with this oversample instead of the store's; the copy is not
    /// changed.
    pub oversample: Option<u32>,
    /// Fixes which vectors are sampled.
    pub seed: u64,
}

impl Default for RecallOptions {
    fn default() -> Self {
        Self {
            sample: 100,
            top_k: 10,
            oversample: None,
            seed: 0,
        }
    }
}

impl RecallOptions {
    pub fn with_sample(mut self, sample: usize) -> Self {
        self.sample = sample;
        self
    }
    pub fn with_top_k(mut self, top_k: usize) -> Self {
        self.top_k = top_k;
        self
    }
    pub fn with_oversample(mut self, oversample: Option<u32>) -> Self {
        self.oversample = oversample;
        self
    }
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

/// Replay up to `options.sample` stored vectors as queries (each excluded
/// from its own results) and report the overlap of the quantized ranking with
/// the exact one, with and without the exact re-score. Costs
/// `O(sample × store × dimension)`.
pub fn quantization_recall(
    graph: &DirGraph,
    entity: EmbeddingEntity,
    type_name: &str,
    text_column: &str,
    options: &RecallOptions,
) -> Result<QuantizationRecall, String> {
    let RecallOptions {
        sample,
        top_k,
        oversample,
        seed,
    } = *options;
    let store = store_ref(graph, entity, type_name, text_column)?;
    let quant = store.quantized().ok_or_else(|| {
        format!(
            "the {} embedding store '{type_name}.{text_column}' has no quantized copy; \
             call quantize_embeddings() first",
            entity.as_str()
        )
    })?;
    if sample == 0 || top_k == 0 {
        return Err("sample and top_k must be at least 1".to_string());
    }
    if oversample == Some(0) {
        return Err("oversample must be at least 1".to_string());
    }
    let len = store.len();
    if len < 2 {
        return Err("recall needs at least two stored vectors".to_string());
    }
    let metric = DistanceMetric::from_name(store.metric.as_deref().unwrap_or("cosine"))
        .ok_or_else(|| "the store's metric is not recognised".to_string())?;
    let k = top_k.min(len - 1);
    let oversample = oversample.unwrap_or(quant.oversample());
    let pool = k.saturating_mul(oversample as usize).min(len - 1);
    let slots = sample_slots(len, sample.min(len), seed);
    let dim = store.dimension;

    let per_query: Vec<Option<(f64, f64)>> = slots
        .par_iter()
        .map(|&query_slot| {
            let query = &store.data[query_slot * dim..(query_slot + 1) * dim];
            let scan = quant.scan(metric, query)?;
            let scorer = Scorer::new(metric, query);
            let exact_of = |slot: usize| {
                scorer.score(
                    query,
                    &store.data[slot * dim..(slot + 1) * dim],
                    store.norms[slot],
                )
            };
            let exact = top_slots(
                (0..len)
                    .filter(|&s| s != query_slot)
                    .map(|s| (s, exact_of(s))),
                k,
            );
            let approx = top_slots(
                (0..len)
                    .filter(|&s| s != query_slot)
                    .map(|s| (s, scan.score(s, store.norms[s]))),
                pool.max(k),
            );
            let plain = overlap(&exact, &approx[..k.min(approx.len())]);
            let rescored_pool = top_slots(approx.iter().map(|&s| (s, exact_of(s))), k);
            Some((plain, overlap(&exact, &rescored_pool)))
        })
        .collect();
    let measured: Vec<(f64, f64)> = per_query.into_iter().flatten().collect();
    if measured.is_empty() {
        return Err(
            "the poincare metric cannot be ranked by a quantized copy, so there is no recall to measure"
                .to_string(),
        );
    }
    let n = measured.len() as f64;
    Ok(QuantizationRecall {
        precision: quant.precision().name(),
        oversample,
        sampled: measured.len(),
        top_k: k,
        recall: measured.iter().map(|m| m.0).sum::<f64>() / n,
        recall_rescored: measured.iter().map(|m| m.1).sum::<f64>() / n,
    })
}

/// `count` distinct slots of `0..len`, a seeded partial Fisher-Yates shuffle.
fn sample_slots(len: usize, count: usize, seed: u64) -> Vec<usize> {
    let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
    let mut slots: Vec<usize> = (0..len).collect();
    for i in 0..count {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = i + ((state >> 33) as usize) % (len - i);
        slots.swap(i, j);
    }
    slots.truncate(count);
    slots
}

/// The `k` best `(slot, score)` items, score descending then slot ascending.
fn top_slots(items: impl Iterator<Item = (usize, f32)>, k: usize) -> Vec<usize> {
    let mut all: Vec<(usize, f32)> = items.collect();
    let order = |a: &(usize, f32), b: &(usize, f32)| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    };
    if all.len() > k {
        all.select_nth_unstable_by(k, order);
        all.truncate(k);
    }
    all.sort_by(order);
    all.into_iter().map(|(slot, _)| slot).collect()
}

fn overlap(exact: &[usize], got: &[usize]) -> f64 {
    if exact.is_empty() {
        return 1.0;
    }
    exact.iter().filter(|slot| got.contains(slot)).count() as f64 / exact.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::quantized::tests::gaussian_vectors;

    fn graph_with_store(count: usize, dim: usize, metric: &str) -> DirGraph {
        let mut graph = DirGraph::new();
        let mut store = EmbeddingStore::with_metric(dim, metric);
        for (node, vector) in gaussian_vectors(count, dim, 11).iter().enumerate() {
            store.set_embedding(node, vector);
        }
        graph
            .embeddings
            .insert(("Doc".to_string(), "body_emb".to_string()), store);
        graph
    }

    #[test]
    fn quantize_builds_replaces_and_drops_without_touching_the_vectors() {
        let mut graph = graph_with_store(300, 32, "cosine");
        let before = graph.embeddings.values().next().unwrap().data.to_vec();
        let report = quantize_embeddings(
            &mut graph,
            EmbeddingEntity::Node,
            "Doc",
            "body",
            Some(Precision::Int8),
            None,
        )
        .unwrap();
        assert_eq!(report.previous, None);
        assert_eq!(report.count, 300);
        let info = report.quantization.unwrap();
        assert_eq!((info.precision, info.oversample), ("int8", 4));
        let replaced = quantize_embeddings(
            &mut graph,
            EmbeddingEntity::Node,
            "Doc",
            "body",
            Some(Precision::Binary),
            Some(12),
        )
        .unwrap();
        assert_eq!(replaced.previous, Some("int8"));
        assert_eq!(replaced.quantization.unwrap().oversample, 12);
        let dropped =
            quantize_embeddings(&mut graph, EmbeddingEntity::Node, "Doc", "body", None, None)
                .unwrap();
        assert!(dropped.quantization.is_none());
        assert_eq!(dropped.previous, Some("binary"));
        assert_eq!(
            graph.embeddings.values().next().unwrap().data.to_vec(),
            before
        );
    }

    #[test]
    fn quantize_refuses_what_it_cannot_serve() {
        let mut graph = graph_with_store(10, 4, "poincare");
        assert!(quantize_embeddings(
            &mut graph,
            EmbeddingEntity::Node,
            "Doc",
            "body",
            Some(Precision::F16),
            None
        )
        .unwrap_err()
        .contains("poincare"));
        let missing = quantize_embeddings(
            &mut graph,
            EmbeddingEntity::Node,
            "Doc",
            "nope",
            Some(Precision::F16),
            None,
        )
        .unwrap_err();
        assert!(
            missing.contains("No node embedding store 'Doc.nope'"),
            "{missing}"
        );
        let orphan = quantize_embeddings(
            &mut graph,
            EmbeddingEntity::Node,
            "Doc",
            "body",
            None,
            Some(3),
        )
        .unwrap_err();
        assert!(orphan.contains("needs a precision"));
    }

    #[test]
    fn recall_matches_the_core_measurements_and_is_seeded() {
        let mut graph = graph_with_store(1500, 256, "cosine");
        let recall = |graph: &DirGraph, precision_seed: u64, oversample| {
            quantization_recall(
                graph,
                EmbeddingEntity::Node,
                "Doc",
                "body",
                &RecallOptions::default()
                    .with_sample(40)
                    .with_oversample(oversample)
                    .with_seed(precision_seed),
            )
            .unwrap()
        };
        assert!(quantization_recall(
            &graph,
            EmbeddingEntity::Node,
            "Doc",
            "body",
            &RecallOptions::default().with_sample(5),
        )
        .unwrap_err()
        .contains("no quantized copy"));
        for (precision, floor, rescored_floor) in [
            (Precision::F16, 0.99, 0.99),
            (Precision::Int8, 0.80, 0.97),
            (Precision::Binary, 0.05, 0.30),
        ] {
            quantize_embeddings(
                &mut graph,
                EmbeddingEntity::Node,
                "Doc",
                "body",
                Some(precision),
                None,
            )
            .unwrap();
            let a = recall(&graph, 3, None);
            println!(
                "{precision:?}: recall {:.3}, rescored {:.3} (oversample {})",
                a.recall, a.recall_rescored, a.oversample
            );
            assert_eq!((a.sampled, a.top_k), (40, 10));
            assert!(a.recall >= floor && a.recall_rescored >= rescored_floor);
            assert!(a.recall_rescored >= a.recall - 1e-9);
            assert_eq!(a, recall(&graph, 3, None), "same seed, same sample");
            // A deeper pool never hurts the rescored figure.
            let deeper = recall(&graph, 3, Some(a.oversample * 4));
            assert!(deeper.recall_rescored >= a.recall_rescored - 1e-9);
        }
    }
}
