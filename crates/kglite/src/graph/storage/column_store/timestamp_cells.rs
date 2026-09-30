//! Row reads of a typed timestamp column as epoch microseconds, for the
//! validity filter and the endpoint-index build, which compare bounds as
//! integers and would otherwise decode every cell to a `NaiveDateTime` and
//! encode it back.

use super::{ColumnStore, TypedColumn};
use crate::graph::schema::InternedKey;
use crate::graph::storage::mapped::mmap_vec::MmapOrVec;

/// A typed timestamp column that alone answers for `key`: every row reads as
/// a timestamp or NULL, exactly as [`ColumnStore::get`] resolves it.
pub(crate) struct TimestampCells<'a> {
    store: &'a ColumnStore,
    data: &'a MmapOrVec<i64>,
    nulls: &'a MmapOrVec<u8>,
}

impl ColumnStore {
    /// `key`'s column when it is a typed timestamp column and nothing else can
    /// answer for a row — no mmap base or overflow bag to fall through to.
    /// `None` otherwise (any other column kind, an absent key), for the
    /// caller's general read.
    pub(crate) fn timestamp_cells(&self, key: InternedKey) -> Option<TimestampCells<'_>> {
        if self.has_mmap_base() || self.has_overflow() {
            return None;
        }
        let column = self.columns.get(self.schema.slot(key)? as usize)?;
        match &**column {
            TypedColumn::Timestamp { data, nulls } => Some(TimestampCells {
                store: self,
                data,
                nulls,
            }),
            _ => None,
        }
    }
}

impl ColumnStore {
    /// The stored microseconds of a present cell of a typed timestamp column;
    /// `None` when the cell is NULL, tombstoned or out of range, or when
    /// anything but that column can answer for it — the caller's general
    /// read then decides.
    #[inline]
    pub(crate) fn timestamp_micros(&self, row_id: u32, key: InternedKey) -> Option<i64> {
        self.timestamp_cells(key)?.micros(row_id)
    }
}

impl TimestampCells<'_> {
    /// The row's timestamp as microseconds since the epoch; `None` for NULL,
    /// a tombstoned row or a row past the store's end.
    #[inline]
    pub(crate) fn micros(&self, row_id: u32) -> Option<i64> {
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
