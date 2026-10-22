//! Quantized scan copies of an embedding store.
//!
//! A [`QuantStore`] is a derived accelerator beside the canonical `f32`
//! vectors: `f16` halves the bytes a brute-force scan reads, `int8` (one
//! symmetric scale per vector) quarters them, and `binary` (sign bits)
//! divides them by 32. Search scans the copy for `top_k × oversample`
//! candidates and re-scores those against the `f32` store, so every returned
//! score is exact and only the *candidate set* is approximate (see
//! `algorithms::vector::quantized_search`).
//!
//! The copy follows the store's own mutations slot for slot
//! (`push`/`set`/`swap_remove`/`pop` mirror `EmbeddingStore`), is persisted as
//! v9 sections when the store's vectors are, and lives in the heap or as a
//! read-only slice of the mapped `.kgl` until its first write.

pub(crate) mod kernels;
mod scan;

use std::io;
use std::sync::{Arc, OnceLock};

use rayon::prelude::*;

use crate::graph::io::file::mappable::MappedSection;
use crate::graph::vector_buf::VectorBuf;

pub(crate) use scan::QuantScan;

/// Widest vector the int8 integer dot product accepts without risking i32
/// overflow (127² · dim < 2³¹).
pub const MAX_QUANTIZED_DIMENSION: usize = 100_000;

/// Scan precision of a quantized copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Precision {
    /// IEEE half precision, saturating at ±65504.
    F16,
    /// Signed 8-bit codes with one symmetric scale per vector.
    Int8,
    /// One sign bit per dimension, ranked by Hamming distance.
    Binary,
}

impl Precision {
    pub const ALL: [Precision; 3] = [Precision::F16, Precision::Int8, Precision::Binary];

    /// The spelling Python, Cypher and the manifest use.
    pub fn name(self) -> &'static str {
        match self {
            Precision::F16 => "f16",
            Precision::Int8 => "int8",
            Precision::Binary => "binary",
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "f16" => Ok(Precision::F16),
            "int8" => Ok(Precision::Int8),
            "binary" => Ok(Precision::Binary),
            other => Err(format!(
                "unknown quantization precision '{other}'; expected 'f16', 'int8' or 'binary'"
            )),
        }
    }

    /// Candidates fetched per requested result when the caller does not say.
    /// Tuned against the recall measured in `quantized::tests`: the lossy
    /// 8-bit and 1-bit scans need a deeper candidate pool than `f16`, whose
    /// ranking is already near-exact.
    pub fn default_oversample(self) -> u32 {
        match self {
            Precision::F16 | Precision::Int8 => 4,
            Precision::Binary => 10,
        }
    }

    /// Bytes of codes per vector of `dim` dimensions (scales excluded).
    pub fn row_bytes(self, dim: usize) -> usize {
        match self {
            Precision::F16 => dim * 2,
            Precision::Int8 => dim,
            Precision::Binary => dim.div_ceil(8),
        }
    }
}

/// What a store reports about its quantized copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantizationInfo {
    /// `"f16"`, `"int8"` or `"binary"`.
    pub precision: &'static str,
    /// Candidates fetched per requested result.
    pub oversample: u32,
    /// `"mapped"` or `"heap"`, as the copy is currently held.
    pub storage: &'static str,
    /// Bytes the copy occupies.
    pub bytes: usize,
    /// The scoring kernel this CPU selected (`"f16c"`, `"popcnt"`, `"scalar"`
    /// or, for int8, the auto-vectorized integer dot).
    pub kernel: &'static str,
}

/// A typed slice of a mapped section plus the shared result of its first CRC
/// check. Clones share both, as `MappedVectors` does for the `f32` payload.
#[derive(Clone)]
pub(crate) struct MappedCodes {
    section: MappedSection,
    verified: Arc<OnceLock<Result<(), String>>>,
}

/// Element types a code section may be viewed as. Sealed to the three the
/// formats define.
pub(crate) trait Code: Copy + Default + 'static {}
impl Code for u16 {}
impl Code for i8 {}
impl Code for u8 {}

/// Heap `Vec<T>` or a read-only mapped section of `T`s.
#[derive(Clone)]
pub(crate) enum CodeBuf<T: Code> {
    Heap(Vec<T>),
    Mapped(MappedCodes, std::marker::PhantomData<T>),
}

impl<T: Code> CodeBuf<T> {
    /// Wrap a mapped section; `None` unless it is mapped, a whole number of
    /// `T`s long and aligned for `T`.
    pub(crate) fn mapped(section: MappedSection) -> Option<Self> {
        let bytes = section.as_bytes();
        let fits = section.is_mapped()
            && bytes.len().is_multiple_of(std::mem::size_of::<T>())
            && (bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<T>());
        fits.then(|| {
            CodeBuf::Mapped(
                MappedCodes {
                    section,
                    verified: Arc::new(OnceLock::new()),
                },
                std::marker::PhantomData,
            )
        })
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        match self {
            CodeBuf::Heap(vec) => vec,
            CodeBuf::Mapped(mapped, _) => {
                let bytes = mapped.section.as_bytes();
                // SAFETY: `mapped` checked alignment and a whole-element length;
                // u16/i8/u8 accept every bit pattern; the mapping is read-only
                // and kept alive by the section's `Arc<MappedFile>`; the format
                // is little-endian and big-endian hosts never map.
                unsafe {
                    std::slice::from_raw_parts(
                        bytes.as_ptr().cast::<T>(),
                        bytes.len() / std::mem::size_of::<T>(),
                    )
                }
            }
        }
    }

    pub(crate) fn is_mapped(&self) -> bool {
        matches!(self, CodeBuf::Mapped(..))
    }

    /// Capacity of a heap buffer in bytes; nothing for a mapped one.
    fn heap_bytes(&self) -> usize {
        match self {
            CodeBuf::Heap(vec) => vec.capacity() * std::mem::size_of::<T>(),
            CodeBuf::Mapped(..) => 0,
        }
    }

    fn verify(&self) -> io::Result<()> {
        match self {
            CodeBuf::Heap(_) => Ok(()),
            CodeBuf::Mapped(mapped, _) => mapped
                .verified
                .get_or_init(|| mapped.section.verify().map_err(|e| e.to_string()))
                .clone()
                .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message)),
        }
    }

    /// Copy a mapped buffer into the heap (whole buffer). Panics if the
    /// backing file was truncated (a SIGBUS otherwise) or fails its CRC32.
    fn make_heap(&mut self) -> &mut Vec<T> {
        if self.is_mapped() {
            if let Err(error) = self.verify() {
                panic!("cannot copy a memory-mapped quantized buffer to the heap: {error}");
            }
        }
        if let CodeBuf::Mapped(mapped, _) = self {
            if let Err(error) = mapped.section.checked_bytes() {
                panic!("cannot copy a memory-mapped quantized buffer to the heap: {error}");
            }
            *self = CodeBuf::Heap(self.as_slice().to_vec());
        }
        match self {
            CodeBuf::Heap(vec) => vec,
            CodeBuf::Mapped(..) => unreachable!("promoted to the heap above"),
        }
    }

    fn mapped_source(&self) -> Option<&std::path::Path> {
        match self {
            CodeBuf::Heap(_) => None,
            CodeBuf::Mapped(mapped, _) => mapped.section.source_path(),
        }
    }
}

/// The codes of a quantized copy, by precision.
#[derive(Clone)]
pub(crate) enum Codes {
    F16(CodeBuf<u16>),
    Int8(CodeBuf<i8>),
    Bits(CodeBuf<u8>),
}

/// A quantized scan copy of an `EmbeddingStore`.
#[derive(Clone)]
pub struct QuantStore {
    precision: Precision,
    dim: usize,
    oversample: u32,
    codes: Codes,
    /// One scale per vector for `int8`; empty otherwise.
    scales: VectorBuf,
}

impl std::fmt::Debug for QuantStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuantStore")
            .field("precision", &self.precision)
            .field("dim", &self.dim)
            .field("len", &self.slot_count())
            .field("mapped", &self.is_mapped())
            .finish()
    }
}

fn encode_f16(vector: &[f32], out: &mut [u16]) {
    for (code, &value) in out.iter_mut().zip(vector) {
        *code = kernels::f32_to_f16(value);
    }
}

/// Symmetric int8: `value ≈ code × scale` with `scale = max|value| / 127`.
/// Returns the scale (0 for the zero vector, whose codes are all zero).
pub(crate) fn encode_int8(vector: &[f32], out: &mut [i8]) -> f32 {
    let max = vector.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
    if max == 0.0 || !max.is_finite() {
        out.fill(0);
        return 0.0;
    }
    let scale = max / 127.0;
    let inverse = 127.0 / max;
    for (code, &value) in out.iter_mut().zip(vector) {
        *code = (value * inverse).round().clamp(-127.0, 127.0) as i8;
    }
    scale
}

/// Sign bits, least-significant bit first within each byte; padding bits are
/// zero in stored rows and query rows alike, so they never contribute to a
/// Hamming distance.
pub(crate) fn encode_bits(vector: &[f32], out: &mut [u8]) {
    out.fill(0);
    for (i, &value) in vector.iter().enumerate() {
        if value > 0.0 {
            out[i / 8] |= 1 << (i % 8);
        }
    }
}

impl QuantStore {
    /// Quantize `data` (`len × dim` row-major `f32`s).
    pub(crate) fn build(
        precision: Precision,
        data: &[f32],
        dim: usize,
        oversample: Option<u32>,
    ) -> Result<Self, String> {
        if dim == 0 {
            return Err("quantization needs a non-zero embedding dimension".to_string());
        }
        if dim > MAX_QUANTIZED_DIMENSION {
            return Err(format!(
                "quantization supports at most {MAX_QUANTIZED_DIMENSION} dimensions, found {dim}"
            ));
        }
        let oversample = match oversample {
            Some(0) => return Err("oversample must be at least 1".to_string()),
            Some(n) => n,
            None => precision.default_oversample(),
        };
        let len = data.len() / dim;
        let (codes, scales) = match precision {
            Precision::F16 => {
                let mut codes = vec![0u16; len * dim];
                codes
                    .par_chunks_mut(dim)
                    .zip(data.par_chunks(dim))
                    .for_each(|(out, row)| encode_f16(row, out));
                (Codes::F16(CodeBuf::Heap(codes)), Vec::new())
            }
            Precision::Int8 => {
                let mut codes = vec![0i8; len * dim];
                let mut scales = vec![0.0f32; len];
                codes
                    .par_chunks_mut(dim)
                    .zip(data.par_chunks(dim))
                    .zip(scales.par_iter_mut())
                    .for_each(|((out, row), scale)| *scale = encode_int8(row, out));
                (Codes::Int8(CodeBuf::Heap(codes)), scales)
            }
            Precision::Binary => {
                let stride = precision.row_bytes(dim);
                let mut codes = vec![0u8; len * stride];
                codes
                    .par_chunks_mut(stride)
                    .zip(data.par_chunks(dim))
                    .for_each(|(out, row)| encode_bits(row, out));
                (Codes::Bits(CodeBuf::Heap(codes)), Vec::new())
            }
        };
        Ok(QuantStore {
            precision,
            dim,
            oversample,
            codes,
            scales: scales.into(),
        })
    }

    /// Rebuild from persisted parts, checking the shapes agree with `len`
    /// vectors of `dim` dimensions. `None` means "derive the copy again".
    pub(crate) fn from_parts(
        precision: Precision,
        oversample: u32,
        dim: usize,
        len: usize,
        codes: Codes,
        scales: VectorBuf,
    ) -> Option<Self> {
        let stride = precision.row_bytes(dim);
        let code_len = match &codes {
            Codes::F16(buf) => buf.as_slice().len() * 2,
            Codes::Int8(buf) => buf.as_slice().len(),
            Codes::Bits(buf) => buf.as_slice().len(),
        };
        let matches_precision = matches!(
            (&codes, precision),
            (Codes::F16(_), Precision::F16)
                | (Codes::Int8(_), Precision::Int8)
                | (Codes::Bits(_), Precision::Binary)
        );
        let scales_ok = match precision {
            Precision::Int8 => scales.len() == len,
            _ => scales.is_empty(),
        };
        (matches_precision
            && dim > 0
            && oversample > 0
            && code_len == len.checked_mul(stride)?
            && scales_ok)
            .then_some(QuantStore {
                precision,
                dim,
                oversample,
                codes,
                scales,
            })
    }

    pub fn precision(&self) -> Precision {
        self.precision
    }

    pub fn dimension(&self) -> usize {
        self.dim
    }

    /// Candidates fetched per requested result.
    pub fn oversample(&self) -> u32 {
        self.oversample
    }

    pub fn set_oversample(&mut self, oversample: u32) {
        self.oversample = oversample.max(1);
    }

    /// Vectors held.
    pub fn slot_count(&self) -> usize {
        match &self.codes {
            Codes::F16(buf) => buf.as_slice().len() / self.dim,
            Codes::Int8(buf) => buf.as_slice().len() / self.dim,
            Codes::Bits(buf) => buf.as_slice().len() / self.precision.row_bytes(self.dim),
        }
    }

    /// Bytes the codes (and scales) occupy.
    pub fn byte_size(&self) -> usize {
        self.slot_count() * self.precision.row_bytes(self.dim) + self.scales.len() * 4
    }

    pub fn info(&self) -> QuantizationInfo {
        QuantizationInfo {
            precision: self.precision.name(),
            oversample: self.oversample,
            storage: if self.is_mapped() { "mapped" } else { "heap" },
            bytes: self.byte_size(),
            kernel: match self.precision {
                Precision::F16 => kernels::f16_kernel_name(),
                Precision::Int8 => "int8_dot",
                Precision::Binary => kernels::hamming_kernel_name(),
            },
        }
    }

    /// Bytes the codes and scales hold on the heap; a mapped buffer counts 0.
    pub(crate) fn heap_bytes(&self) -> usize {
        let codes = match &self.codes {
            Codes::F16(buf) => buf.heap_bytes(),
            Codes::Int8(buf) => buf.heap_bytes(),
            Codes::Bits(buf) => buf.heap_bytes(),
        };
        codes + self.scales.heap_bytes()
    }

    pub fn is_mapped(&self) -> bool {
        match &self.codes {
            Codes::F16(buf) => buf.is_mapped(),
            Codes::Int8(buf) => buf.is_mapped(),
            Codes::Bits(buf) => buf.is_mapped(),
        }
    }

    pub(crate) fn codes(&self) -> &Codes {
        &self.codes
    }

    pub(crate) fn scales(&self) -> &VectorBuf {
        &self.scales
    }

    /// Check mapped parts against their recorded CRC32s (once each).
    pub(crate) fn verify(&self) -> io::Result<()> {
        match &self.codes {
            Codes::F16(buf) => buf.verify()?,
            Codes::Int8(buf) => buf.verify()?,
            Codes::Bits(buf) => buf.verify()?,
        }
        self.scales.verify()
    }

    pub(crate) fn mapped_source(&self) -> Option<&std::path::Path> {
        match &self.codes {
            Codes::F16(buf) => buf.mapped_source(),
            Codes::Int8(buf) => buf.mapped_source(),
            Codes::Bits(buf) => buf.mapped_source(),
        }
    }

    /// Copy any mapped part into the heap, releasing the file.
    pub(crate) fn promote_to_heap(&mut self) {
        match &mut self.codes {
            Codes::F16(buf) => {
                buf.make_heap();
            }
            Codes::Int8(buf) => {
                buf.make_heap();
            }
            Codes::Bits(buf) => {
                buf.make_heap();
            }
        }
        self.scales.make_heap();
    }

    /// Append the quantization of `vector` as the last slot.
    pub(crate) fn push(&mut self, vector: &[f32]) {
        debug_assert_eq!(vector.len(), self.dim);
        let dim = self.dim;
        match &mut self.codes {
            Codes::F16(buf) => {
                let codes = buf.make_heap();
                let start = codes.len();
                codes.resize(start + dim, 0);
                encode_f16(vector, &mut codes[start..]);
            }
            Codes::Int8(buf) => {
                let codes = buf.make_heap();
                let start = codes.len();
                codes.resize(start + dim, 0);
                let scale = encode_int8(vector, &mut codes[start..]);
                self.scales.push(scale);
            }
            Codes::Bits(buf) => {
                let stride = self.precision.row_bytes(dim);
                let codes = buf.make_heap();
                let start = codes.len();
                codes.resize(start + stride, 0);
                encode_bits(vector, &mut codes[start..]);
            }
        }
    }

    /// Overwrite `slot` with the quantization of `vector`.
    pub(crate) fn set(&mut self, slot: usize, vector: &[f32]) {
        debug_assert_eq!(vector.len(), self.dim);
        let dim = self.dim;
        match &mut self.codes {
            Codes::F16(buf) => {
                encode_f16(vector, &mut buf.make_heap()[slot * dim..(slot + 1) * dim]);
            }
            Codes::Int8(buf) => {
                let scale = encode_int8(vector, &mut buf.make_heap()[slot * dim..(slot + 1) * dim]);
                self.scales[slot] = scale;
            }
            Codes::Bits(buf) => {
                let stride = self.precision.row_bytes(dim);
                encode_bits(
                    vector,
                    &mut buf.make_heap()[slot * stride..(slot + 1) * stride],
                );
            }
        }
    }

    /// Remove the last slot.
    pub(crate) fn pop(&mut self) {
        let (dim, stride) = (self.dim, self.precision.row_bytes(self.dim));
        match &mut self.codes {
            Codes::F16(buf) => {
                let codes = buf.make_heap();
                codes.truncate(codes.len().saturating_sub(dim));
            }
            Codes::Int8(buf) => {
                let codes = buf.make_heap();
                codes.truncate(codes.len().saturating_sub(dim));
                self.scales.pop();
            }
            Codes::Bits(buf) => {
                let codes = buf.make_heap();
                codes.truncate(codes.len().saturating_sub(stride));
            }
        }
    }

    /// Move the last slot into `slot` and drop the tail — the quantized twin
    /// of the store's tail-swap removal.
    pub(crate) fn swap_remove(&mut self, slot: usize) {
        let last = self.slot_count() - 1;
        if slot != last {
            self.copy_slot(last, slot);
        }
        self.pop();
    }

    /// Append a copy of `slot`'s codes (and scale) as a new last slot.
    pub(crate) fn push_copy_of(&mut self, slot: usize) {
        let (dim, stride) = (self.dim, self.precision.row_bytes(self.dim));
        match &mut self.codes {
            Codes::F16(buf) => {
                let codes = buf.make_heap();
                codes.extend_from_within(slot * dim..(slot + 1) * dim);
            }
            Codes::Int8(buf) => {
                let codes = buf.make_heap();
                codes.extend_from_within(slot * dim..(slot + 1) * dim);
                let scale = self.scales[slot];
                self.scales.push(scale);
            }
            Codes::Bits(buf) => {
                let codes = buf.make_heap();
                codes.extend_from_within(slot * stride..(slot + 1) * stride);
            }
        }
    }

    fn copy_slot(&mut self, from: usize, to: usize) {
        let (dim, stride) = (self.dim, self.precision.row_bytes(self.dim));
        match &mut self.codes {
            Codes::F16(buf) => buf
                .make_heap()
                .copy_within(from * dim..(from + 1) * dim, to * dim),
            Codes::Int8(buf) => {
                buf.make_heap()
                    .copy_within(from * dim..(from + 1) * dim, to * dim);
                self.scales[to] = self.scales[from];
            }
            Codes::Bits(buf) => buf
                .make_heap()
                .copy_within(from * stride..(from + 1) * stride, to * stride),
        }
    }

    /// Decode `slot` back to `f32`s (test and diagnostics aid).
    #[cfg(test)]
    pub(crate) fn decode(&self, slot: usize) -> Vec<f32> {
        let dim = self.dim;
        match &self.codes {
            Codes::F16(buf) => buf.as_slice()[slot * dim..(slot + 1) * dim]
                .iter()
                .map(|&c| kernels::f16_to_f32(c))
                .collect(),
            Codes::Int8(buf) => buf.as_slice()[slot * dim..(slot + 1) * dim]
                .iter()
                .map(|&c| c as f32 * self.scales[slot])
                .collect(),
            Codes::Bits(buf) => {
                let stride = self.precision.row_bytes(dim);
                let row = &buf.as_slice()[slot * stride..(slot + 1) * stride];
                (0..dim)
                    .map(|i| {
                        if row[i / 8] >> (i % 8) & 1 == 1 {
                            1.0
                        } else {
                            0.0
                        }
                    })
                    .collect()
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
