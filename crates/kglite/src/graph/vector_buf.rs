//! [`VectorBuf`] — the f32 buffer behind an embedding store: a heap `Vec<f32>`
//! or a read-only slice of a memory-mapped `.kgl`.
//!
//! Readers see a plain `&[f32]` through `Deref`, so brute-force scans, HNSW and
//! the scorers are unchanged. Anything that needs `&mut` (the `DerefMut` impl
//! and the `Vec`-shaped mutators) first **promotes** the whole buffer to the
//! heap: a mapped buffer is never written, because it is a shared view of a
//! file other graphs may be reading. `Clone` of a mapped buffer is an `Arc`
//! bump; a clone that then writes promotes its own copy and leaves the
//! original mapped.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

use crate::graph::io::file::mappable::MappedSection;

/// A mapped `f32` section plus the shared result of its first CRC check.
///
/// Clones share both the mapping and the check, so a verified section is
/// verified once however many snapshots hold it.
#[derive(Clone)]
pub struct MappedVectors {
    section: MappedSection,
    verified: Arc<OnceLock<Result<(), String>>>,
}

impl MappedVectors {
    /// Wrap a mapped f32 section. `None` unless the section is mapped, whole
    /// `f32`s long and 4-byte aligned, so `as_slice` never reinterprets
    /// misaligned bytes.
    pub(crate) fn new(section: MappedSection) -> Option<Self> {
        let bytes = section.as_bytes();
        let fits = section.is_mapped()
            && bytes.len().is_multiple_of(4)
            && (bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<f32>());
        fits.then(|| MappedVectors {
            section,
            verified: Arc::new(OnceLock::new()),
        })
    }

    fn as_slice(&self) -> &[f32] {
        let bytes = self.section.as_bytes();
        // SAFETY: `new` checked that the pointer is f32-aligned and the length
        // a whole number of f32s; every bit pattern is a valid f32; the
        // mapping is read-only and held alive by the section's `Arc<MappedFile>`
        // for as long as `self` exists; and the format is little-endian, which
        // only little-endian hosts map (`resolve_mapping` refuses big-endian).
        unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), bytes.len() / 4) }
    }

    fn verify(&self) -> io::Result<()> {
        self.verified
            .get_or_init(|| self.section.verify().map_err(|e| e.to_string()))
            .clone()
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))
    }

    /// The path of the file this buffer is mapped from.
    pub(crate) fn source_path(&self) -> &std::path::Path {
        self.section
            .source_path()
            .unwrap_or(std::path::Path::new(""))
    }
}

/// The f32 payload (or L2 norms) of an embedding store.
#[derive(Clone)]
pub enum VectorBuf {
    Heap(Vec<f32>),
    Mapped(MappedVectors),
}

impl Default for VectorBuf {
    fn default() -> Self {
        VectorBuf::Heap(Vec::new())
    }
}

impl From<Vec<f32>> for VectorBuf {
    fn from(vec: Vec<f32>) -> Self {
        VectorBuf::Heap(vec)
    }
}

impl VectorBuf {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn mapped(section: MappedSection) -> Option<Self> {
        MappedVectors::new(section).map(VectorBuf::Mapped)
    }

    pub fn is_mapped(&self) -> bool {
        matches!(self, VectorBuf::Mapped(_))
    }

    /// Bytes the buffer holds on the heap: its capacity when it is a `Vec`,
    /// nothing for a mapped view of a file.
    pub(crate) fn heap_bytes(&self) -> usize {
        match self {
            VectorBuf::Heap(vec) => vec.capacity() * std::mem::size_of::<f32>(),
            VectorBuf::Mapped(_) => 0,
        }
    }

    pub fn as_slice(&self) -> &[f32] {
        match self {
            VectorBuf::Heap(vec) => vec,
            VectorBuf::Mapped(mapped) => mapped.as_slice(),
        }
    }

    /// Check a mapped buffer against its recorded CRC32 (once; later calls
    /// read the cached result) and that its file is still intact. A heap
    /// buffer is always `Ok`.
    pub fn verify(&self) -> io::Result<()> {
        match self {
            VectorBuf::Heap(_) => Ok(()),
            VectorBuf::Mapped(mapped) => mapped.verify(),
        }
    }

    /// Whether the mapped file's mtime changed since it was mapped; `None`
    /// for a heap buffer.
    pub(crate) fn modified_since_open(&self) -> Option<io::Result<bool>> {
        match self {
            VectorBuf::Heap(_) => None,
            VectorBuf::Mapped(mapped) => mapped.section.modified_since_open(),
        }
    }

    /// The file a mapped buffer reads from.
    pub(crate) fn mapped_source(&self) -> Option<&std::path::Path> {
        match self {
            VectorBuf::Heap(_) => None,
            VectorBuf::Mapped(mapped) => Some(mapped.source_path()),
        }
    }

    /// Copy a mapped buffer into the heap (whole buffer); a heap buffer is
    /// untouched. Panics if the backing file was truncated since it was
    /// mapped (a SIGBUS otherwise) or fails its CRC32; callers that can return
    /// an error call [`Self::verify`] first.
    pub fn make_heap(&mut self) -> &mut Vec<f32> {
        if let VectorBuf::Mapped(mapped) = self {
            // Verify before copying: a heap buffer is never re-checked, so a
            // corrupt copy would pass for good data at the next save.
            if let Err(error) = mapped.section.checked_bytes().and(mapped.verify()) {
                panic!("cannot copy a memory-mapped embedding buffer to the heap: {error}");
            }
            *self = VectorBuf::Heap(mapped.as_slice().to_vec());
        }
        match self {
            VectorBuf::Heap(vec) => vec,
            VectorBuf::Mapped(_) => unreachable!("promoted to the heap above"),
        }
    }

    pub fn push(&mut self, value: f32) {
        self.make_heap().push(value);
    }

    pub fn pop(&mut self) -> Option<f32> {
        self.make_heap().pop()
    }

    pub fn extend_from_slice(&mut self, values: &[f32]) {
        self.make_heap().extend_from_slice(values);
    }

    pub fn truncate(&mut self, len: usize) {
        self.make_heap().truncate(len);
    }

    pub fn clear(&mut self) {
        // Nothing to preserve: drop the mapping instead of copying it first.
        *self = VectorBuf::Heap(Vec::new());
    }

    pub fn reserve(&mut self, additional: usize) {
        self.make_heap().reserve(additional);
    }
}

impl Deref for VectorBuf {
    type Target = [f32];

    #[inline]
    fn deref(&self) -> &[f32] {
        self.as_slice()
    }
}

impl DerefMut for VectorBuf {
    /// Writing through a slice promotes a mapped buffer to the heap first.
    #[inline]
    fn deref_mut(&mut self) -> &mut [f32] {
        self.make_heap().as_mut_slice()
    }
}

impl PartialEq for VectorBuf {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl PartialEq<Vec<f32>> for VectorBuf {
    fn eq(&self, other: &Vec<f32>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl fmt::Debug for VectorBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VectorBuf::Heap(vec) => f.debug_tuple("Heap").field(vec).finish(),
            VectorBuf::Mapped(mapped) => f
                .debug_struct("Mapped")
                .field("len", &mapped.as_slice().len())
                .finish(),
        }
    }
}

impl fmt::Debug for MappedVectors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedVectors")
            .field("len", &self.as_slice().len())
            .finish()
    }
}

/// Wire-identical to `Vec<f32>`, so a v8 section decodes into a heap buffer
/// and a buffer serializes to the bytes a `Vec<f32>` field always produced.
impl Serialize for VectorBuf {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for VectorBuf {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<f32>::deserialize(deserializer).map(VectorBuf::Heap)
    }
}
