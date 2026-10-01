//! Streaming writer of `id_indices.bin`.
//!
//! The file is built one directory entry at a time and streamed through a
//! buffered writer; no step holds a copy of the whole store. An untouched base
//! entry is copied from its mapping byte for byte, an [`TypeEntry::OverBase`]
//! entry is merged (sorted base with sorted delta) as it is written, and an
//! owned index is read in place. What is held in memory is a sorted pair list
//! for one owned all-`Int64` index at a time (16 B per id, against the 60-90 B
//! per id the heap map already occupies) and a General index's encoded blob.

use super::{
    IdIndexBase, IdIndexStore, DIR_ENTRY_BYTES, HEADER_BYTES, INT64_ENTRY_BYTES, MAGIC,
    MAX_GENERAL_INDEX_DECODE_BYTES, VARIANT_GENERAL, VARIANT_INT64, VARIANT_INTEGER, VERSION,
};
use crate::datatypes::Value;
use crate::graph::schema::{canonical_id, mixed_numeric_kinds, StringInterner, TypeIdIndex};
use crate::graph::storage::disk::id_index_layer::TypeEntry;
use crate::graph::storage::disk::le_bytes::{le_i64_binary_search, read_le_i64, read_le_u32};
use crate::graph::storage::disk::type_index::TypeIndexStore;
use crate::serde_codec;
use petgraph::graph::NodeIndex;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// Where one directory entry's payload bytes come from.
enum Payload<'a> {
    /// Bytes already in memory: an Integer payload or a General blob.
    Bytes(Vec<u8>),
    /// A base entry's payload, copied from the mapping untouched.
    Slice(&'a [u8]),
    /// The sorted `(id, node)` pairs of an owned all-`Int64` index.
    Pairs(Vec<(i64, u32)>),
    /// A sorted delta merged into a mapped Int64Sorted entry while streaming.
    /// A delta value of `None` deletes the id.
    Merge {
        keys: &'a [u8],
        nodes: &'a [u8],
        delta: Vec<(i64, Option<u32>)>,
    },
}

struct Plan<'a> {
    type_key: u64,
    variant: u8,
    num_entries: u64,
    payload: Payload<'a>,
}

impl Plan<'_> {
    fn payload_len(&self) -> u64 {
        match &self.payload {
            Payload::Bytes(bytes) => bytes.len() as u64,
            Payload::Slice(bytes) => bytes.len() as u64,
            Payload::Pairs(_) | Payload::Merge { .. } => {
                self.num_entries * INT64_ENTRY_BYTES as u64
            }
        }
    }
}

/// The number of distinct ids `index` holds, counting every numeric spelling
/// of one id once. Only a `General` map mixing numeric kinds pays the count.
fn distinct_ids(index: &TypeIdIndex) -> usize {
    match index {
        TypeIdIndex::General(map) if mixed_numeric_kinds(map.keys()) => map
            .keys()
            .map(|id| canonical_id(id).into_owned())
            .collect::<rustc_hash::FxHashSet<Value>>()
            .len(),
        _ => index.len(),
    }
}

fn node_u32(idx: NodeIndex) -> Result<u32, String> {
    u32::try_from(idx.index())
        .map_err(|_| format!("node index {} does not fit an id index entry", idx.index()))
}

/// Plan for an owned index, or `None` when its ids repeat.
fn plan_owned(
    type_key: u64,
    index: &TypeIdIndex,
    members: Option<usize>,
) -> Result<Option<Plan<'static>>, String> {
    if members.is_some_and(|members| distinct_ids(index) < members) {
        return Ok(None);
    }
    match index {
        TypeIdIndex::Integer(map) => {
            let mut pairs = map
                .iter()
                .map(|(key, node)| Ok((*key, node_u32(*node)?)))
                .collect::<Result<Vec<(u32, u32)>, String>>()?;
            pairs.sort_unstable_by_key(|(key, _)| *key);
            let mut data = Vec::with_capacity(pairs.len() * 8);
            data.extend(pairs.iter().flat_map(|(key, _)| key.to_le_bytes()));
            data.extend(pairs.iter().flat_map(|(_, node)| node.to_le_bytes()));
            Ok(Some(Plan {
                type_key,
                variant: VARIANT_INTEGER,
                num_entries: pairs.len() as u64,
                payload: Payload::Bytes(data),
            }))
        }
        TypeIdIndex::General(map) => {
            if !map.is_empty() && map.keys().all(|id| matches!(id, Value::Int64(_))) {
                let mut pairs = map
                    .iter()
                    .map(|(id, node)| match id {
                        Value::Int64(key) => Ok((*key, node_u32(*node)?)),
                        _ => unreachable!("every key was checked to be an Int64"),
                    })
                    .collect::<Result<Vec<(i64, u32)>, String>>()?;
                pairs.sort_unstable_by_key(|(key, _)| *key);
                return Ok(Some(Plan {
                    type_key,
                    variant: VARIANT_INT64,
                    num_entries: pairs.len() as u64,
                    payload: Payload::Pairs(pairs),
                }));
            }
            let blob = serde_codec::encode_versioned(
                serde_codec::CURRENT_CODEC,
                map,
                MAX_GENERAL_INDEX_DECODE_BYTES,
            )
            .map_err(|e| format!("id_indices General-variant codec failed: {e}"))?;
            Ok(Some(Plan {
                type_key,
                variant: VARIANT_GENERAL,
                num_entries: map.len() as u64,
                payload: Payload::Bytes(blob),
            }))
        }
    }
}

/// Plan for a delta over a mapped Int64Sorted entry, or `None` when the merged
/// ids repeat. A delta holding an id that is not an `Int64` cannot be merged
/// into the sorted layout and is reported as `Err(())` so the caller falls back
/// to the owned path.
fn plan_over_base<'a>(
    type_key: u64,
    base: &'a IdIndexBase,
    name: &str,
    delta: &TypeIdIndex,
    members: Option<usize>,
) -> Result<Result<Option<Plan<'a>>, ()>, String> {
    let (keys, nodes) = base
        .int64_parts(name)
        .ok_or_else(|| format!("id index for type '{name}' layers over a non-Int64 entry"))?;
    let tombstone = NodeIndex::end();
    let mut changes: Vec<(i64, Option<u32>)> = Vec::with_capacity(delta.len());
    for (id, node) in delta.iter() {
        let Value::Int64(key) = id else {
            return Ok(Err(()));
        };
        let value = if node == tombstone {
            None
        } else {
            Some(node_u32(node)?)
        };
        changes.push((key, value));
    }
    changes.sort_unstable_by_key(|(key, _)| *key);

    // The merged count without a pass over the base: only the changed ids can
    // move it.
    let mut count = (keys.len() / 8) as i64;
    for (key, value) in &changes {
        match (value.is_some(), le_i64_binary_search(keys, *key).is_some()) {
            (true, false) => count += 1,
            (false, true) => count -= 1,
            _ => {}
        }
    }
    let count = usize::try_from(count)
        .map_err(|_| format!("id index for type '{name}' merged to a negative size"))?;
    if members.is_some_and(|members| count < members) {
        return Ok(Ok(None));
    }
    Ok(Ok(Some(Plan {
        type_key,
        variant: VARIANT_INT64,
        num_entries: count as u64,
        payload: Payload::Merge {
            keys,
            nodes,
            delta: changes,
        },
    })))
}

/// The base entry for `name`, copied verbatim, unless its ids repeat.
fn plan_untouched<'a>(
    type_key: u64,
    base: &'a IdIndexBase,
    name: &str,
    members: Option<usize>,
) -> Option<Plan<'a>> {
    let entry = base.dir.get(name)?;
    if members.is_some_and(|members| base.entry_len(name).unwrap_or(0) < members) {
        return None;
    }
    let start = usize::try_from(entry.payload_off).ok()?;
    let end = start.checked_add(usize::try_from(entry.payload_len).ok()?)?;
    Some(Plan {
        type_key,
        variant: entry.variant,
        num_entries: u64::from(entry.num_entries),
        payload: Payload::Slice(base.mmap.get(start..end)?),
    })
}

/// A sorted base merged with a sorted delta: the base's entries, with each
/// delta id replacing the base's entry for it (or deleting it, for `None`) and
/// the delta's new ids interleaved in key order.
fn merged<'b>(
    keys: &'b [u8],
    nodes: &'b [u8],
    delta: &'b [(i64, Option<u32>)],
) -> impl Iterator<Item = (i64, u32)> + 'b {
    let base_len = keys.len() / 8;
    let mut base_at = 0usize;
    let mut delta_at = 0usize;
    std::iter::from_fn(move || loop {
        let base = if base_at < base_len {
            match (read_le_i64(keys, base_at), read_le_u32(nodes, base_at)) {
                (Some(key), Some(node)) => Some((key, node)),
                // A mapping cut short ends the merge; the caller counts what it
                // wrote and refuses the file.
                _ => return None,
            }
        } else {
            None
        };
        match (base, delta.get(delta_at).copied()) {
            (None, None) => return None,
            (Some(base), None) => {
                base_at += 1;
                return Some(base);
            }
            (None, Some((key, value))) => {
                delta_at += 1;
                if let Some(node) = value {
                    return Some((key, node));
                }
            }
            (Some(base), Some((key, value))) => {
                if base.0 < key {
                    base_at += 1;
                    return Some(base);
                }
                delta_at += 1;
                if base.0 == key {
                    base_at += 1;
                }
                if let Some(node) = value {
                    return Some((key, node));
                }
            }
        }
    })
}

/// The type names to write: the overlay's, then the base's that nothing masks.
/// `overlay` is the store's overlay, already read-locked by the caller: a second
/// `read` on the same lock may deadlock behind a queued writer.
fn live_names(store: &IdIndexStore, overlay: &HashMap<String, TypeEntry>) -> Vec<String> {
    let mut names: Vec<String> = overlay.keys().cloned().collect();
    if let Some(base) = store.base.as_deref() {
        names.extend(
            base.dir
                .keys()
                .filter(|name| {
                    !overlay.contains_key(name.as_str()) && !store.removed.contains(*name)
                })
                .cloned(),
        );
    }
    names
}

/// Write `id_indices.bin` (raw mmap layout) from the store's live view, overlay
/// over base, without ever holding the whole store in memory.
///
/// Directory keys are `InternedKey` hashes and the loader turns each one back
/// into a type name through the interner sidecar that ships with the same
/// snapshot. Names are therefore *resolved*, never interned here: deriving a
/// key from the name alone — the old `interner.clone().try_get_or_intern`,
/// which registered the name in a throwaway clone — manufactured keys for
/// names the persisted interner never carried, and a single such entry made
/// the whole directory unloadable ("directory contains an unresolved type
/// key").
///
/// Only an empty index can carry an unregistered name: creating a node interns
/// its type (`mutation/batch.rs`), so any index holding ids names an interned
/// type. Empty entries do reach the store — the read path caches a
/// build-on-miss index for a label a query merely mentioned
/// ([`IdIndexStore::lookup_or_build`]) — and they are pure cache, so dropping
/// them loses nothing. A non-empty unregistered entry would be a broken
/// invariant, so it fails the save rather than shipping a directory that
/// cannot be read back.
///
/// A type whose ids repeat (fewer distinct ids than `type_indices` members) is
/// not written either: its index names the last node per id in the bucket's
/// order, which the type-index writer sorts by `NodeIndex`, so a reused slot
/// would leave the persisted choice disagreeing with the reloaded order. The
/// reload rebuilds such an index from the persisted bucket on first use. Ids
/// are counted by [`canonical_id`], not by entries: an index holding one id in
/// two spellings (as one built through 0.18.1 did) repeats although its
/// length matches the type's.
///
/// An all-`Int64` index is written as the Int64Sorted variant, whether it is an
/// owned map or a delta merged over a mapped entry of the same variant.
pub fn write_id_indices_bin(
    dir: &Path,
    store: &IdIndexStore,
    type_indices: &TypeIndexStore,
    interner: &StringInterner,
) -> Result<(), String> {
    let overlay = store.overlay.read().unwrap();
    let mut plans: Vec<Plan<'_>> = Vec::new();
    for name in live_names(store, &overlay) {
        let members = type_indices.get(&name).map(|members| members.len());
        let entry = overlay.get(&name);
        let Some(key) = interner.try_resolve_to_key(&name) else {
            let empty = match entry {
                Some(entry) => entry.len() == 0,
                None => store
                    .base
                    .as_deref()
                    .is_none_or(|base| base.entry_len(&name) == Some(0)),
            };
            if empty {
                continue;
            }
            return Err(format!(
                "id index for type '{name}' holds ids but the type name is not in the \
                 graph's interner; refusing to write an id_indices.bin that cannot be \
                 read back"
            ));
        };
        let type_key = key.as_u64();
        let plan = match entry {
            None => store
                .base
                .as_deref()
                .and_then(|base| plan_untouched(type_key, base, &name, members)),
            Some(TypeEntry::Owned(index)) => plan_owned(type_key, index, members)?,
            Some(entry) => match entry.delta_over_mapping() {
                Some((base, base_name, delta)) => {
                    match plan_over_base(type_key, base, base_name, &delta, members)? {
                        Ok(plan) => plan,
                        // A non-Int64 id joined the type: it no longer fits the
                        // sorted layout, so write the merged index the general
                        // way.
                        Err(()) => plan_owned(type_key, &entry.materialize(), members)?,
                    }
                }
                None => plan_owned(type_key, &entry.materialize(), members)?,
            },
        };
        plans.extend(plan);
    }
    plans.sort_unstable_by_key(|plan| plan.type_key);

    let data_offset = HEADER_BYTES + DIR_ENTRY_BYTES * plans.len();
    let path = dir.join("id_indices.bin");
    let io = |e: std::io::Error| format!("Failed to write id_indices.bin: {e}");
    let file = std::fs::File::create(&path).map_err(io)?;
    let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
    out.write_all(MAGIC).map_err(io)?;
    out.write_all(&VERSION.to_le_bytes()).map_err(io)?;
    out.write_all(&(plans.len() as u32).to_le_bytes())
        .map_err(io)?;
    out.write_all(&(HEADER_BYTES as u64).to_le_bytes())
        .map_err(io)?;
    out.write_all(&(data_offset as u64).to_le_bytes())
        .map_err(io)?;
    let mut cursor = data_offset as u64;
    for plan in &plans {
        let len = plan.payload_len();
        out.write_all(&plan.type_key.to_le_bytes()).map_err(io)?;
        out.write_all(&[plan.variant]).map_err(io)?;
        out.write_all(&[0u8; 7]).map_err(io)?;
        out.write_all(&plan.num_entries.to_le_bytes()).map_err(io)?;
        out.write_all(&cursor.to_le_bytes()).map_err(io)?;
        out.write_all(&len.to_le_bytes()).map_err(io)?;
        out.write_all(&[0u8; 8]).map_err(io)?;
        cursor += len;
    }
    for plan in &plans {
        match &plan.payload {
            Payload::Bytes(bytes) => out.write_all(bytes).map_err(io)?,
            Payload::Slice(bytes) => out.write_all(bytes).map_err(io)?,
            Payload::Pairs(pairs) => {
                for (key, _) in pairs {
                    out.write_all(&key.to_le_bytes()).map_err(io)?;
                }
                for (_, node) in pairs {
                    out.write_all(&node.to_le_bytes()).map_err(io)?;
                }
            }
            Payload::Merge { keys, nodes, delta } => {
                // Two streaming passes: every key, then every node. Each pass
                // must produce exactly the count the directory declared.
                let mut written = 0u64;
                for (key, _) in merged(keys, nodes, delta) {
                    out.write_all(&key.to_le_bytes()).map_err(io)?;
                    written += 1;
                }
                let mut nodes_written = 0u64;
                for (_, node) in merged(keys, nodes, delta) {
                    out.write_all(&node.to_le_bytes()).map_err(io)?;
                    nodes_written += 1;
                }
                if written != plan.num_entries || nodes_written != plan.num_entries {
                    return Err(format!(
                        "id index merge wrote {written} keys and {nodes_written} nodes where \
                         {} were expected; the mapped index is inconsistent",
                        plan.num_entries
                    ));
                }
            }
        }
    }
    out.flush().map_err(io)?;
    Ok(())
}
