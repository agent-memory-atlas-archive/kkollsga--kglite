//! Turn a pure mmap-backed store into an owned, appendable store whose columns
//! are **file-backed** in a workspace spill directory, instead of
//! `flattened_owned`'s per-row decode onto the heap.
//!
//! Each region of the source column file is copied once into its own spill file
//! (via the loader's `load_typed_vec` / `load_bytes`), which is then mapped
//! growable (`MmapOrVec::Mapped`), so later appends push into the file and the
//! store's resident heap stays O(small columns). The copy is O(rows) — a
//! deliberate simplification: the source file belongs to a published generation
//! and is never written through.

use super::{ColumnStore, TypedColumn};
use crate::graph::schema::TypeSchema;
use crate::graph::storage::mapped::column_store::{
    ColRef, FixedColumnMeta, MmapColumnStore, Region, StrColumnMeta,
};
use crate::graph::storage::mapped::mmap_vec::MmapOrVec;
use crate::graph::storage::type_build_meta::ColType;
use rustc_hash::FxHashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

fn region<'a>(ms: &'a MmapColumnStore, r: &Region) -> &'a [u8] {
    &ms.mmap[r.offset..r.offset + r.len]
}

fn nulls_or_zero(
    ms: &MmapColumnStore,
    r: &Region,
    rc: usize,
    dir: &Path,
    name: &str,
) -> io::Result<MmapOrVec<u8>> {
    if r.len == 0 {
        // An absent null region reads as "all null" in the mmap store.
        return ColumnStore::load_typed_vec::<u8>(&vec![1u8; rc], rc, Some(dir), name, "null");
    }
    ColumnStore::load_typed_vec::<u8>(region(ms, r), rc, Some(dir), name, "null")
}

fn fixed_column(
    ms: &MmapColumnStore,
    fc: &FixedColumnMeta,
    rc: usize,
    dir: &Path,
    name: &str,
) -> io::Result<TypedColumn> {
    let nulls = nulls_or_zero(ms, &fc.nulls, rc, dir, name)?;
    let bytes = region(ms, &fc.data);
    Ok(match fc.col_type {
        ColType::Int64 => TypedColumn::Int64 {
            data: ColumnStore::load_typed_vec::<i64>(bytes, rc, Some(dir), name, "i64")?,
            nulls,
        },
        ColType::Float64 => TypedColumn::Float64 {
            data: ColumnStore::load_typed_vec::<f64>(bytes, rc, Some(dir), name, "f64")?,
            nulls,
        },
        ColType::UniqueId => TypedColumn::UniqueId {
            data: ColumnStore::load_typed_vec::<u32>(bytes, rc, Some(dir), name, "u32")?,
            nulls,
        },
        ColType::Bool => TypedColumn::Bool {
            data: ColumnStore::load_typed_vec::<u8>(bytes, rc, Some(dir), name, "bool")?,
            nulls,
        },
        ColType::Date => TypedColumn::Date {
            data: ColumnStore::load_typed_vec::<i32>(bytes, rc, Some(dir), name, "i32")?,
            nulls,
        },
        ColType::Timestamp => TypedColumn::Timestamp {
            data: ColumnStore::load_typed_vec::<i64>(bytes, rc, Some(dir), name, "ts")?,
            nulls,
        },
        // A fixed slot never holds a string column; a sidecar naming one is
        // corrupt (an unknown type tag maps to `Str`), and refusing it beats a panic.
        ColType::Str => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("column '{name}' has a fixed-width slot with a string type tag"),
            ))
        }
    })
}

fn str_column(
    ms: &MmapColumnStore,
    sc: &StrColumnMeta,
    rc: usize,
    dir: &Path,
    name: &str,
) -> io::Result<TypedColumn> {
    // The mega-file stores `rc` cumulative end offsets; the in-memory column
    // wants `rc + 1` with a leading zero.
    let mut offsets = Vec::with_capacity((rc + 1) * 8);
    offsets.extend_from_slice(&0u64.to_le_bytes());
    offsets.extend_from_slice(region(ms, &sc.offsets));
    Ok(TypedColumn::Str {
        offsets: ColumnStore::load_typed_vec::<u64>(&offsets, rc + 1, Some(dir), name, "off")?,
        data: ColumnStore::load_bytes(region(ms, &sc.data), Some(dir), name, "str")?,
        nulls: nulls_or_zero(ms, &sc.nulls, rc, dir, name)?,
        relocated: FxHashMap::default(),
    })
}

impl ColumnStore {
    /// `Some(store)` when this store is a pure mmap base that could be moved
    /// into file-backed columns under `dir`; `None` leaves the caller on the
    /// heap `materialize_for_append` path (overflow bags are not handled). An
    /// `Err` means a spill file could not be written or a region disagrees with
    /// the row count; `self` is untouched either way.
    pub(crate) fn mapped_owned_for_append(&self, dir: &Path) -> io::Result<Option<ColumnStore>> {
        let Some(ms) = self.pure_mmap_store() else {
            return Ok(None);
        };
        if ms.has_overflow {
            return Ok(None);
        }
        let rc = ms.row_count as usize;
        let mut keys: Vec<_> = ms.col_map.keys().copied().collect();
        keys.sort_by_key(|k| k.as_u64());
        let schema = Arc::new(TypeSchema::from_keys(keys.clone()));
        let mut columns = Vec::with_capacity(keys.len());
        for key in &keys {
            let name = format!("c{}", key.as_u64());
            let col = match ms.col_map[key] {
                ColRef::Fixed(i) => fixed_column(ms, &ms.fixed_cols[i], rc, dir, &name)?,
                ColRef::Str(i) => str_column(ms, &ms.str_cols[i], rc, dir, &name)?,
            };
            columns.push(Arc::new(col));
        }
        let id_column = if ms.id_is_string {
            ms.id_str
                .as_ref()
                .filter(|sc| sc.nulls.len != 0)
                .map(|sc| str_column(ms, sc, rc, dir, "__id__"))
                .transpose()?
        } else {
            ms.id_fixed
                .as_ref()
                .filter(|fc| fc.data.len != 0)
                .map(|fc| fixed_column(ms, fc, rc, dir, "__id__"))
                .transpose()?
        };
        let title_column = if ms.title.nulls.len == 0 && ms.title.data.len == 0 {
            None
        } else if ms.title_is_int() {
            let fc = FixedColumnMeta {
                col_type: ColType::Int64,
                data: ms.title.data,
                nulls: ms.title.nulls,
            };
            Some(fixed_column(ms, &fc, rc, dir, "__title__")?)
        } else {
            Some(str_column(ms, &ms.title, rc, dir, "__title__")?)
        };
        let mut owned = ColumnStore::from_mmap_store(Arc::clone(ms));
        owned.mmap_store = None;
        owned.schema = schema;
        owned.columns = columns;
        owned.row_count = ms.row_count;
        owned.tombstones = vec![false; rc];
        owned.id_column = id_column.map(Arc::new);
        owned.title_column = title_column.map(Arc::new);
        Ok(Some(owned))
    }
}
