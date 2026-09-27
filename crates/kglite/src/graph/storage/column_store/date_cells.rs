//! Row reads of a typed date column as epoch days, for the validity filter's
//! per-node bound checks.

use super::{ColumnStore, TypedColumn};
use crate::graph::schema::InternedKey;
use crate::graph::storage::mapped::mmap_vec::MmapOrVec;

/// A typed date column that alone answers for `key`: every row reads as a
/// date or NULL, exactly as [`ColumnStore::get`] resolves it.
pub(crate) struct DateCells<'a> {
    store: &'a ColumnStore,
    data: &'a MmapOrVec<i32>,
    nulls: &'a MmapOrVec<u8>,
}

impl ColumnStore {
    /// `key`'s column when it is a typed date column and nothing else can
    /// answer for a row — no mmap base or overflow bag to fall through to.
    /// `None` otherwise (any other column kind, an absent key), for the
    /// caller's general read.
    pub(crate) fn date_cells(&self, key: InternedKey) -> Option<DateCells<'_>> {
        if self.has_mmap_base() || self.has_overflow() {
            return None;
        }
        let column = self.columns.get(self.schema.slot(key)? as usize)?;
        match &**column {
            TypedColumn::Date { data, nulls } => Some(DateCells {
                store: self,
                data,
                nulls,
            }),
            _ => None,
        }
    }
}

impl DateCells<'_> {
    /// The row's date as days since 1970-01-01; `None` for NULL, a
    /// tombstoned row or a row past the store's end.
    #[inline]
    pub(crate) fn epoch_days(&self, row_id: u32) -> Option<i32> {
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
