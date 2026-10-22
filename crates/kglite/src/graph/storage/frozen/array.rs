//! A read-only typed array over a `.kgl` section or a heap copy, with the
//! slice resolved once so a read is two loads rather than an enum dispatch.

use std::sync::Arc;

use crate::graph::io::file::mappable::{MappedSection, SharedSection};
use crate::graph::storage::mapped::mmap_vec::{MmapOrVec, MmapPod};

/// Little-endian element types a frozen section holds.
///
/// The decoder is used when the section cannot be reinterpreted in place (a
/// heap copy has byte alignment only, a big-endian host reads differently).
pub(crate) trait FrozenElement: MmapPod {
    const WIDTH: usize;
    fn decode(bytes: &[u8]) -> Self;
}

macro_rules! scalar_element {
    ($($ty:ty),+) => {$(
        impl FrozenElement for $ty {
            const WIDTH: usize = std::mem::size_of::<$ty>();
            #[inline]
            fn decode(bytes: &[u8]) -> Self {
                <$ty>::from_le_bytes(bytes.try_into().expect("element width"))
            }
        }
    )+};
}

scalar_element!(u16, u32, u64);

/// An immutable array of `T`, mapped from a section or owned.
///
/// `ptr`/`len` mirror the slice `store` serves. `store` is never mutated and a
/// `Vec`'s buffer does not move when the `Vec` does, so they stay valid for
/// the array's life and for every clone (a clone shares the mapping, or copies
/// the heap buffer and re-resolves).
pub(crate) struct FrozenArray<T: FrozenElement> {
    store: MmapOrVec<T>,
    ptr: *const T,
    len: usize,
}

// SAFETY: the array is read-only; `ptr` addresses the immutable buffer `store`
// owns (a heap `Vec` or a file mapping an `Arc` keeps alive).
unsafe impl<T: FrozenElement> Send for FrozenArray<T> {}
// SAFETY: as above; nothing mutates the pointee.
unsafe impl<T: FrozenElement> Sync for FrozenArray<T> {}

impl<T: FrozenElement> FrozenArray<T> {
    fn resolve(store: MmapOrVec<T>) -> Self {
        let slice = store.as_slice();
        let (ptr, len) = (slice.as_ptr(), slice.len());
        FrozenArray { store, ptr, len }
    }

    pub(crate) fn from_vec(data: Vec<T>) -> Self {
        Self::resolve(MmapOrVec::from_vec(data))
    }

    /// The section's elements: in place when it is mapped and aligned for `T`,
    /// otherwise decoded into an owned buffer. `None` when the section's bytes
    /// are not a whole number of `T`.
    pub(crate) fn from_section(section: &MappedSection) -> Option<Self> {
        let bytes = section.as_bytes();
        if !bytes.len().is_multiple_of(T::WIDTH) {
            return None;
        }
        if cfg!(target_endian = "little") {
            if let Some(shared) = SharedSection::new(section.clone()) {
                if let Some(store) = MmapOrVec::shared(shared) {
                    return Some(Self::resolve(store));
                }
            }
        }
        let data: Vec<T> = bytes.chunks_exact(T::WIDTH).map(T::decode).collect();
        Some(Self::from_vec(data))
    }

    #[inline(always)]
    pub(crate) fn as_slice(&self) -> &[T] {
        // SAFETY: `ptr`/`len` describe `store`'s slice, which lives as long
        // as `self` and is never written (see the type docs).
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The `.kgl` section this array reads in place, if it is one.
    pub(crate) fn shared_section(&self) -> Option<&Arc<SharedSection>> {
        self.store.shared_section()
    }

    /// Bytes this array holds on the heap (0 when mapped).
    pub(crate) fn heap_bytes(&self) -> usize {
        self.store.heap_bytes()
    }
}

impl<T: FrozenElement> Clone for FrozenArray<T> {
    fn clone(&self) -> Self {
        Self::resolve(self.store.clone())
    }
}

impl<T: FrozenElement> std::fmt::Debug for FrozenArray<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FrozenArray<{}>({} elements, {})",
            std::any::type_name::<T>(),
            self.len,
            if self.shared_section().is_some() {
                "mapped"
            } else {
                "heap"
            }
        )
    }
}
