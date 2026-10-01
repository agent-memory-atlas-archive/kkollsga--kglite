//! Row reads of a typed fixed-width temporal column — a date as epoch days, a
//! timestamp as epoch microseconds — for the validity filter's per-node bound
//! checks and the endpoint-index build, which compare bounds as integers and
//! would otherwise decode every cell to a calendar value and encode it back.

use super::{ColumnStore, TypedColumn};
use crate::graph::schema::InternedKey;
use crate::graph::storage::mapped::mmap_vec::{MmapOrVec, MmapPod};

/// A typed fixed-width column that alone answers for a key: every row reads as
/// a value or NULL, exactly as [`ColumnStore::get`] resolves it.
pub(crate) struct FixedCells<'a, T: MmapPod> {
    store: &'a ColumnStore,
    data: &'a MmapOrVec<T>,
    nulls: &'a MmapOrVec<u8>,
}

impl ColumnStore {
    /// `key`'s column when it is a typed date column and nothing else can
    /// answer for a row — no mmap base or overflow bag to fall through to.
    /// `None` otherwise (any other column kind, an absent key), for the
    /// caller's general read.
    pub(crate) fn date_cells(&self, key: InternedKey) -> Option<FixedCells<'_, i32>> {
        self.fixed_cells(key, |column| match column {
            TypedColumn::Date { data, nulls } => Some((data, nulls)),
            _ => None,
        })
    }

    /// [`Self::date_cells`] for a typed timestamp column.
    pub(crate) fn timestamp_cells(&self, key: InternedKey) -> Option<FixedCells<'_, i64>> {
        self.fixed_cells(key, |column| match column {
            TypedColumn::Timestamp { data, nulls } => Some((data, nulls)),
            _ => None,
        })
    }

    fn fixed_cells<T: MmapPod>(
        &self,
        key: InternedKey,
        pick: impl FnOnce(&TypedColumn) -> Option<(&MmapOrVec<T>, &MmapOrVec<u8>)>,
    ) -> Option<FixedCells<'_, T>> {
        if self.has_mmap_base() || self.has_overflow() {
            return None;
        }
        let column = self.columns.get(self.schema.slot(key)? as usize)?;
        let (data, nulls) = pick(column)?;
        Some(FixedCells {
            store: self,
            data,
            nulls,
        })
    }

    /// The stored microseconds of a present cell of a typed timestamp column;
    /// `None` when the cell is NULL, tombstoned or out of range, or when
    /// anything but that column can answer for it — the caller's general
    /// read then decides.
    #[inline]
    pub(crate) fn timestamp_micros(&self, row_id: u32, key: InternedKey) -> Option<i64> {
        self.timestamp_cells(key)?.value(row_id)
    }
}

impl<T: MmapPod> FixedCells<'_, T> {
    /// The row's stored value (epoch days or epoch microseconds); `None` for
    /// NULL, a tombstoned row or a row past the store's end.
    #[inline]
    pub(crate) fn value(&self, row_id: u32) -> Option<T> {
        let idx = row_id as usize;
        if row_id >= self.store.row_count
            || self.store.tombstones.get(idx).copied().unwrap_or(false)
            || idx >= self.nulls.len()
            || self.nulls.get(idx) != 0
        {
            return None;
        }
        Some(self.data.get(idx))
    }
}
