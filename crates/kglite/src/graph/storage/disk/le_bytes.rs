//! Little-endian reads over the raw byte regions of the mapped index files
//! (`id_indices.bin`, `type_indices.bin`): fixed-width arrays that are never
//! assumed aligned, so each is read through `[u8; N]` chunks.

/// The `index`-th little-endian `u32` of `bytes`, `None` past the end.
pub(super) fn read_le_u32(bytes: &[u8], index: usize) -> Option<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .get(index)
        .map(|chunk| u32::from_le_bytes(*chunk))
}

/// The `index`-th little-endian `i64` of `bytes`, `None` past the end.
pub(super) fn read_le_i64(bytes: &[u8], index: usize) -> Option<i64> {
    bytes
        .as_chunks::<8>()
        .0
        .get(index)
        .map(|chunk| i64::from_le_bytes(*chunk))
}

/// Every little-endian `u32` of `bytes`.
pub(super) fn le_u32_iter(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_le_bytes(*chunk))
}

/// Position of `wanted` in an ascending run of little-endian `u32`s.
pub(super) fn le_u32_binary_search(bytes: &[u8], wanted: u32) -> Option<usize> {
    bytes
        .as_chunks::<4>()
        .0
        .binary_search_by(|chunk| u32::from_le_bytes(*chunk).cmp(&wanted))
        .ok()
}

/// Position of `wanted` in an ascending run of little-endian `i64`s.
pub(super) fn le_i64_binary_search(bytes: &[u8], wanted: i64) -> Option<usize> {
    bytes
        .as_chunks::<8>()
        .0
        .binary_search_by(|chunk| i64::from_le_bytes(*chunk).cmp(&wanted))
        .ok()
}
