//! Sorted string keys of a `.kgl` id section, read in place.
//!
//! A string-keyed entry is two sections of one type: the `u32` node of each id
//! (ascending key order) and a key section laid out as
//! `[u64 offset; count + 1][key bytes]`. Offsets are little-endian, start at 0,
//! never decrease, end at the byte length, and are relative to the start of the
//! key bytes; key `i` is `bytes[offset[i]..offset[i + 1]]`. Keys are valid UTF-8,
//! unique, and ascending in byte order (the order `str` compares in), so a
//! lookup is a binary search over the mapping and nothing is copied onto the
//! heap.

use std::io;

use super::invalid_index;

const WORD: usize = 8;

fn offset(keys: &[u8], index: usize) -> Option<usize> {
    let chunk = keys.as_chunks::<WORD>().0.get(index)?;
    usize::try_from(u64::from_le_bytes(*chunk)).ok()
}

/// Key `index` of an entry of `count` keys, or `None` if the section is shorter
/// than its offsets claim.
pub(super) fn key_at(keys: &[u8], count: usize, index: usize) -> Option<&[u8]> {
    let bytes = keys.get(count.checked_add(1)?.checked_mul(WORD)?..)?;
    bytes.get(offset(keys, index)?..offset(keys, index + 1)?)
}

/// Position of `wanted` among the ascending keys.
pub(super) fn search(keys: &[u8], count: usize, wanted: &[u8]) -> Option<usize> {
    let (mut low, mut high) = (0usize, count);
    while low < high {
        let mid = low + (high - low) / 2;
        match key_at(keys, count, mid)?.cmp(wanted) {
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
            std::cmp::Ordering::Equal => return Some(mid),
        }
    }
    None
}

/// Check a key/node section pair and return the key count: parallel runs,
/// well-formed offsets, valid UTF-8 keys in strictly ascending order, every
/// node below `node_bound`. The pass reads every key byte once.
pub(super) fn validate(keys: &[u8], nodes: &[u8], node_bound: usize) -> io::Result<usize> {
    if !nodes.len().is_multiple_of(4) {
        return Err(invalid_index("a string id node section is not whole u32s"));
    }
    let count = nodes.len() / 4;
    let header = (count + 1)
        .checked_mul(WORD)
        .filter(|header| *header <= keys.len())
        .ok_or_else(|| invalid_index("a string id section is shorter than its offsets"))?;
    let bytes_len = keys.len() - header;
    if offset(keys, 0) != Some(0) || offset(keys, count) != Some(bytes_len) {
        return Err(invalid_index(
            "string id offsets do not span the key bytes exactly",
        ));
    }
    let mut previous: Option<&[u8]> = None;
    for index in 0..count {
        let (start, end) = (
            offset(keys, index).unwrap_or(usize::MAX),
            offset(keys, index + 1).unwrap_or(0),
        );
        if start > end || end > bytes_len {
            return Err(invalid_index("string id offsets are not monotone"));
        }
        let key = &keys[header + start..header + end];
        if std::str::from_utf8(key).is_err() {
            return Err(invalid_index("a string id is not valid UTF-8"));
        }
        if previous.is_some_and(|prior| key <= prior) {
            return Err(invalid_index("keys are not strictly increasing"));
        }
        previous = Some(key);
        let node = nodes.as_chunks::<4>().0[index];
        if u32::from_le_bytes(node) as usize >= node_bound {
            return Err(invalid_index("an id names a node outside the graph"));
        }
    }
    Ok(count)
}

/// The two sections a save writes for ids already sorted by key: the key
/// section and the node section.
pub(crate) fn encode<'a>(
    sorted: impl ExactSizeIterator<Item = (&'a [u8], u32)>,
) -> (Vec<u8>, Vec<u8>) {
    let count = sorted.len();
    let mut keys = Vec::with_capacity((count + 1) * WORD);
    let mut bytes = Vec::new();
    let mut nodes = Vec::with_capacity(count * 4);
    keys.extend_from_slice(&0u64.to_le_bytes());
    for (key, node) in sorted {
        bytes.extend_from_slice(key);
        keys.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        nodes.extend_from_slice(&node.to_le_bytes());
    }
    keys.extend_from_slice(&bytes);
    (keys, nodes)
}
