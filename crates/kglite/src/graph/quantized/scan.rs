//! Query-bound scoring over a [`QuantStore`]'s codes.

use super::kernels::{dot_f16_kernel, dot_i8, hamming_kernel};
use super::{encode_bits, encode_int8, Codes, QuantStore};
use crate::graph::algorithms::vector::DistanceMetric;

/// How the approximate dot product becomes a ranking score.
#[derive(Clone, Copy)]
enum Ranking {
    Cosine,
    Dot,
    Euclidean,
}

enum Query<'a> {
    F16 {
        codes: &'a [u16],
        query: &'a [f32],
        dot: fn(&[u16], &[f32]) -> f32,
    },
    Int8 {
        codes: &'a [i8],
        scales: &'a [f32],
        query: Vec<i8>,
        query_scale: f32,
    },
    Bits {
        codes: &'a [u8],
        query: Vec<u8>,
        hamming: fn(&[u8], &[u8]) -> u32,
    },
}

/// A prepared scan: the codes' slices, the quantized query and the kernel
/// chosen for this CPU, so [`Self::score`] is one dot product per slot.
///
/// Scores are *approximate* and only meaningful for ranking candidates; the
/// two-stage search re-scores the survivors against the `f32` store.
pub(crate) struct QuantScan<'a> {
    query: Query<'a>,
    ranking: Ranking,
    dim: usize,
    row_bytes: usize,
    query_norm: f32,
}

impl QuantStore {
    /// Prepare a scan for `query` under `metric`. `None` for the Poincaré
    /// metric: its distance is non-linear in the norms, so a quantized dot
    /// product cannot rank it.
    pub(crate) fn scan<'a>(
        &'a self,
        metric: DistanceMetric,
        query: &'a [f32],
    ) -> Option<QuantScan<'a>> {
        let ranking = match metric {
            DistanceMetric::Cosine => Ranking::Cosine,
            DistanceMetric::DotProduct => Ranking::Dot,
            DistanceMetric::Euclidean => Ranking::Euclidean,
            DistanceMetric::Poincare => return None,
        };
        if query.len() != self.dim {
            return None;
        }
        let prepared = match self.codes() {
            Codes::F16(buf) => Query::F16 {
                codes: buf.as_slice(),
                query,
                dot: dot_f16_kernel(),
            },
            Codes::Int8(buf) => {
                let mut quantized = vec![0i8; self.dim];
                let query_scale = encode_int8(query, &mut quantized);
                Query::Int8 {
                    codes: buf.as_slice(),
                    scales: self.scales().as_slice(),
                    query: quantized,
                    query_scale,
                }
            }
            Codes::Bits(buf) => {
                let mut bits = vec![0u8; self.precision().row_bytes(self.dim)];
                encode_bits(query, &mut bits);
                Query::Bits {
                    codes: buf.as_slice(),
                    query: bits,
                    hamming: hamming_kernel(),
                }
            }
        };
        let query_norm = query.iter().map(|v| v * v).sum::<f32>().sqrt();
        Some(QuantScan {
            query: prepared,
            ranking,
            dim: self.dim,
            row_bytes: self.precision().row_bytes(self.dim),
            query_norm,
        })
    }
}

impl QuantScan<'_> {
    /// Approximate score of `slot`, higher is better. `norm` is the slot's
    /// exact cached L2 norm. Binary scans rank by negative Hamming distance
    /// whatever the metric: sign agreement tracks angular similarity and is
    /// only a candidate filter for the others.
    #[inline]
    pub(crate) fn score(&self, slot: usize, norm: f32) -> f32 {
        let dot = match &self.query {
            Query::F16 { codes, query, dot } => dot(&codes[slot * self.dim..][..self.dim], query),
            Query::Int8 {
                codes,
                scales,
                query,
                query_scale,
            } => {
                let sum = dot_i8(&codes[slot * self.dim..][..self.dim], query);
                sum as f32 * scales[slot] * query_scale
            }
            Query::Bits {
                codes,
                query,
                hamming,
            } => {
                return -(hamming(&codes[slot * self.row_bytes..][..self.row_bytes], query) as f32);
            }
        };
        match self.ranking {
            Ranking::Cosine => {
                let denominator = self.query_norm * norm;
                if denominator > 0.0 {
                    dot / denominator
                } else {
                    0.0
                }
            }
            Ranking::Dot => dot,
            Ranking::Euclidean => -(self.query_norm * self.query_norm + norm * norm - 2.0 * dot)
                .max(0.0)
                .sqrt(),
        }
    }
}
