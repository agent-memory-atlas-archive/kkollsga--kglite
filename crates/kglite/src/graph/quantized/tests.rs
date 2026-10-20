//! Quantized copies: encoding accuracy, slot-for-slot maintenance, recall.

use super::*;
use crate::graph::algorithms::vector::{DistanceMetric, Scorer};
use crate::graph::schema::EmbeddingStore;

fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 11
}

fn uniform(state: &mut u64) -> f32 {
    (next(state) as f64 / (1u64 << 53) as f64) as f32
}

/// Standard normal by Box-Muller.
fn gaussian(state: &mut u64) -> f32 {
    let (u, v) = (uniform(state).max(1e-9), uniform(state));
    (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
}

pub(crate) fn gaussian_vectors(count: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed;
    (0..count)
        .map(|_| (0..dim).map(|_| gaussian(&mut state)).collect())
        .collect()
}

fn store_of(vectors: &[Vec<f32>], metric: &str) -> EmbeddingStore {
    let mut store = EmbeddingStore::with_metric(vectors[0].len(), metric);
    for (node, vector) in vectors.iter().enumerate() {
        store.set_embedding(node, vector);
    }
    store
}

fn same_codes(left: &QuantStore, right: &QuantStore) -> bool {
    let codes = match (left.codes(), right.codes()) {
        (Codes::F16(a), Codes::F16(b)) => a.as_slice() == b.as_slice(),
        (Codes::Int8(a), Codes::Int8(b)) => a.as_slice() == b.as_slice(),
        (Codes::Bits(a), Codes::Bits(b)) => a.as_slice() == b.as_slice(),
        _ => false,
    };
    codes && left.scales().as_slice() == right.scales().as_slice()
}

/// The copy a store holds equals the copy rebuilt from its current vectors:
/// the encoding is deterministic, so any maintenance slip shows as a diff.
fn assert_in_step(store: &EmbeddingStore, context: &str) {
    let held = store.quant_store().expect("a copy is held");
    let rebuilt = QuantStore::build(held.precision(), &store.data, store.dimension, None).unwrap();
    assert_eq!(held.slot_count(), store.len(), "{context}: length");
    assert!(
        same_codes(held, &rebuilt),
        "{context}: codes diverged from a rebuild ({:?})",
        held.precision()
    );
    assert!(
        store.quantized().is_some(),
        "{context}: guard rejected the copy"
    );
}

#[test]
fn int8_and_f16_decode_close_and_binary_keeps_signs() {
    let vectors = gaussian_vectors(50, 64, 3);
    for vector in &vectors {
        let data: Vec<f32> = vector.clone();
        let f16 = QuantStore::build(Precision::F16, &data, 64, None).unwrap();
        let int8 = QuantStore::build(Precision::Int8, &data, 64, None).unwrap();
        let bits = QuantStore::build(Precision::Binary, &data, 64, None).unwrap();
        let max = data.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        for (i, &original) in data.iter().enumerate() {
            assert!((f16.decode(0)[i] - original).abs() <= original.abs() * 1.0e-3 + 1e-7);
            assert!((int8.decode(0)[i] - original).abs() <= max / 127.0 * 0.5 + 1e-6);
            assert_eq!(bits.decode(0)[i] == 1.0, original > 0.0);
        }
    }
    let zero = QuantStore::build(Precision::Int8, &[0.0; 8], 8, None).unwrap();
    assert_eq!(zero.decode(0), vec![0.0; 8]);
}

#[test]
fn build_refuses_unusable_shapes() {
    assert!(QuantStore::build(Precision::F16, &[], 0, None).is_err());
    assert!(QuantStore::build(Precision::F16, &[1.0], 1, Some(0)).is_err());
    assert!(QuantStore::build(Precision::Int8, &[1.0], MAX_QUANTIZED_DIMENSION + 1, None).is_err());
    let mut poincare = EmbeddingStore::with_metric(2, "poincare");
    poincare.set_embedding(0, &[0.1, 0.2]);
    assert!(poincare.quantize(Precision::F16, None).is_err());
    assert!(Precision::parse("f8").is_err());
}

#[test]
fn every_store_mutation_keeps_the_copy_in_step() {
    for precision in Precision::ALL {
        let dim = 19; // not a multiple of 8, so binary rows carry padding bits
        let vectors = gaussian_vectors(40, dim, 17);
        let mut store = store_of(&vectors[..30], "cosine");
        store.quantize(precision, None).unwrap();
        assert_in_step(&store, "initial");
        let mut state = 99u64;
        let mut removed = Vec::new();
        for step in 0..400 {
            let fresh = &vectors[(next(&mut state) % 40) as usize];
            let live: Vec<usize> = store.slot_to_node.clone();
            match next(&mut state) % 6 {
                0 if !live.is_empty() => {
                    // in-place replace
                    let node = live[(next(&mut state) as usize) % live.len()];
                    store.set_embedding(node, fresh);
                }
                1 => {
                    // append a new node
                    store.set_embedding(1000 + step, fresh);
                }
                2 if live.len() > 1 => {
                    let node = live[(next(&mut state) as usize) % live.len()];
                    removed.push((node, store.remove_embedding(node).unwrap()));
                }
                3 if !removed.is_empty() => {
                    let (node, prior) = removed.pop().unwrap();
                    store.restore_embedding(node, &prior);
                }
                4 if !live.is_empty() => {
                    // journalled overwrite and its undo
                    let node = live[(next(&mut state) as usize) % live.len()];
                    let (prior, coverage) = store.cell_pre_image(node).unwrap();
                    store.set_embedding(node, fresh);
                    store.restore_cell(node, &prior, coverage);
                }
                _ => {
                    // journalled append and its undo
                    store.set_embedding(5000 + step, fresh);
                    store.pop_appended_embedding(5000 + step);
                }
            }
            assert_in_step(&store, &format!("{precision:?} step {step}"));
        }
        // Restoring everything that is still outstanding, newest first.
        while let Some((node, prior)) = removed.pop() {
            store.restore_embedding(node, &prior);
            assert_in_step(&store, "final unwinding");
        }
    }
}

#[test]
fn a_clone_diverges_independently_and_requantize_matches() {
    let vectors = gaussian_vectors(20, 12, 4);
    let mut store = store_of(&vectors, "cosine");
    store.quantize(Precision::Int8, Some(7)).unwrap();
    let mut clone = store.clone();
    clone.set_embedding(3, &vectors[10]);
    assert_in_step(&clone, "clone");
    assert_in_step(&store, "original");
    assert!(!same_codes(
        store.quant_store().unwrap(),
        clone.quant_store().unwrap()
    ));
    store.requantize();
    assert_eq!(store.quant_store().unwrap().oversample(), 7);
    assert_in_step(&store, "requantized");
    assert!(store.drop_quantization());
    assert!(!store.drop_quantization());
    assert!(store.precision().is_none());
}

#[test]
fn a_copy_that_disagrees_with_the_store_is_ignored() {
    let vectors = gaussian_vectors(10, 8, 6);
    let mut store = store_of(&vectors, "cosine");
    store.quantize(Precision::F16, None).unwrap();
    // A direct write that bypasses the hooks, as a missed mutation path would.
    store.slot_to_node.push(99);
    store.node_to_slot.insert(99, 10);
    store.data.extend_from_slice(&[0.0; 8]);
    store.norms.push(0.0);
    assert!(
        store.quantized().is_none(),
        "stale copy must not serve a scan"
    );
}

/// Fraction of the exact top-`k` found in `got`.
fn overlap(exact: &[usize], got: &[usize]) -> f64 {
    let hits = exact.iter().filter(|slot| got.contains(slot)).count();
    hits as f64 / exact.len() as f64
}

fn exact_top(store: &EmbeddingStore, query: &[f32], k: usize) -> Vec<usize> {
    let scorer = Scorer::new(DistanceMetric::Cosine, query);
    let mut scored: Vec<(usize, f32)> = (0..store.len())
        .map(|slot| {
            let start = slot * store.dimension;
            (
                slot,
                scorer.score(
                    query,
                    &store.data[start..start + store.dimension],
                    store.norms[slot],
                ),
            )
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    scored.into_iter().take(k).map(|(slot, _)| slot).collect()
}

fn approx_top(store: &EmbeddingStore, query: &[f32], pool: usize) -> Vec<usize> {
    let (scan, _) = store
        .quantized_scan(DistanceMetric::Cosine, query)
        .expect("copy present");
    crate::graph::algorithms::vector::coarse_slots(store, &scan, pool, None)
}

/// Recall of the quantized stage on random Gaussian 1024-d vectors — the
/// adversarial case, since nothing in them is low-rank. Pre-rescore is the
/// top-10 of the approximate ranking alone; post-rescore is whether the
/// default oversampled pool contains the exact top-10 (the re-score then
/// makes the returned scores exact).
#[test]
fn measured_recall_on_gaussian_1024() {
    let (count, dim, k) = (3000, 1024, 10);
    let vectors = gaussian_vectors(count, dim, 1234);
    let queries = gaussian_vectors(30, dim, 777);
    let mut base = store_of(&vectors, "cosine");
    let truths: Vec<Vec<usize>> = queries.iter().map(|q| exact_top(&base, q, k)).collect();
    let floors = [
        (Precision::F16, 1.0, 1.0),
        (Precision::Int8, 0.90, 0.99),
        // Sign bits of iid Gaussian coordinates carry little ranking signal;
        // real embeddings are far more structured (see the deeper pools below).
        (Precision::Binary, 0.10, 0.45),
    ];
    for (precision, pre_floor, pool_floor) in floors {
        base.quantize(precision, None).unwrap();
        let oversample = base.quant_store().unwrap().oversample() as usize;
        let (mut pre, mut pooled) = (0.0, 0.0);
        for (query, truth) in queries.iter().zip(&truths) {
            pre += overlap(truth, &approx_top(&base, query, k));
            pooled += overlap(truth, &approx_top(&base, query, k * oversample));
        }
        let (pre, pooled) = (pre / queries.len() as f64, pooled / queries.len() as f64);
        println!(
            "recall@{k} {precision:?}: pre-rescore {pre:.4}, pool x{oversample} {pooled:.4} (N={count}, d={dim})"
        );
        if precision == Precision::Binary {
            for deeper in [30, 100] {
                let recall: f64 = queries
                    .iter()
                    .zip(&truths)
                    .map(|(q, t)| overlap(t, &approx_top(&base, q, k * deeper)))
                    .sum::<f64>()
                    / queries.len() as f64;
                println!("recall@{k} Binary: pool x{deeper} {recall:.4}");
            }
        }
        assert!(pre >= pre_floor, "{precision:?} pre-rescore recall {pre}");
        assert!(pooled >= pool_floor, "{precision:?} pooled recall {pooled}");
    }
}
