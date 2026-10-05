//! Row gather: a fresh store holding a chosen subset of another store's rows,
//! built one column at a time — the bulk path behind subgraph copies.

use super::{next_spill_token, ColumnStore, TypedColumn};
use crate::datatypes::values::Value;
use crate::graph::storage::mapped::mmap_vec::{MmapBytes, MmapOrVec, MmapPod};
use rustc_hash::FxHashMap;
use std::sync::Arc;

impl ColumnStore {
    /// A new store whose row `i` is this store's row `rows[i]`, every column
    /// (and the id/title sidecars) gathered into fresh heap buffers sized by
    /// `rows` — nothing but the schema is shared with `self`, so the result's
    /// [`heap_bytes`](Self::heap_bytes) is its own rows' and a later write to
    /// either store never copies the other's. Each column keeps its stored
    /// kind (a column widened to `Mixed` stays `Mixed`), including columns
    /// that are NULL on every gathered row.
    ///
    /// `None` when a row could resolve through something other than the
    /// columns — an mmap base, a tail or an overflow bag — for the caller's per-row
    /// path. `rows` must name live rows; a row a column is short of reads as
    /// NULL, as it does through [`Self::get`].
    pub(crate) fn gather_rows(&self, rows: &[u32]) -> Option<ColumnStore> {
        if !self.columns_cover_rows() || self.has_overflow() {
            return None;
        }
        debug_assert!(rows
            .iter()
            .all(|&row| row < self.row_count && !self.is_tombstoned(row)));
        let gather = |column: &Arc<TypedColumn>| Arc::new(column.gather(rows));
        Some(ColumnStore {
            schema: Arc::clone(&self.schema),
            columns: self.columns.iter().map(gather).collect(),
            null_overrides: None,
            row_count: u32::try_from(rows.len()).expect("a store holds at most u32::MAX rows"),
            tombstones: vec![false; rows.len()],
            id_column: self.id_column.as_ref().map(gather),
            title_column: self.title_column.as_ref().map(gather),
            overflow_offsets: None,
            overflow_data: None,
            mmap_store: None,
            slot_scratch: Vec::new(),
            spill_token: next_spill_token(),
            spillable_growth: true,
            displaced: None,
            tail: None,
            append_tail: super::tail::AppendTail::Off,
        })
    }

    /// Every cell of this store that can hold an arbitrary [`Value`] (the
    /// `Mixed` columns, id and title included), for a caller that rewrites
    /// values in place — the subgraph copy snapshots node references with it.
    /// Privatises each `Mixed` column it visits.
    pub(crate) fn heterogeneous_cells_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.columns
            .iter_mut()
            .chain(self.id_column.iter_mut())
            .chain(self.title_column.iter_mut())
            .filter(|column| column.heterogeneous_cells().is_some())
            .flat_map(|column| match Arc::make_mut(column) {
                TypedColumn::Mixed { data } => data.iter_mut(),
                _ => Default::default(),
            })
    }
}

/// The fixed-width gather: the kept rows' values and null flags, a NULL row
/// storing the zero value exactly as a pushed NULL does.
fn gather_fixed<T: MmapPod>(
    data: &MmapOrVec<T>,
    nulls: &MmapOrVec<u8>,
    rows: &[u32],
) -> (MmapOrVec<T>, MmapOrVec<u8>) {
    let (data, nulls) = (data.as_slice(), nulls.as_slice());
    let mut out_data = Vec::with_capacity(rows.len());
    let mut out_nulls = Vec::with_capacity(rows.len());
    for &row in rows {
        let row = row as usize;
        match (nulls.get(row), data.get(row)) {
            (Some(0), Some(&value)) => {
                out_data.push(value);
                out_nulls.push(0);
            }
            _ => {
                out_data.push(T::default());
                out_nulls.push(1);
            }
        }
    }
    (
        MmapOrVec::from_vec(out_data),
        MmapOrVec::from_vec(out_nulls),
    )
}

impl TypedColumn {
    /// This column's `rows`, in order, as a new heap column of the same kind.
    /// A string column's relocation overlay is folded in (each row is read
    /// as [`Self::get_str`] resolves it), so the result has none.
    fn gather(&self, rows: &[u32]) -> TypedColumn {
        match self {
            TypedColumn::Int64 { data, nulls } => {
                let (data, nulls) = gather_fixed(data, nulls, rows);
                TypedColumn::Int64 { data, nulls }
            }
            TypedColumn::Float64 { data, nulls } => {
                let (data, nulls) = gather_fixed(data, nulls, rows);
                TypedColumn::Float64 { data, nulls }
            }
            TypedColumn::UniqueId { data, nulls } => {
                let (data, nulls) = gather_fixed(data, nulls, rows);
                TypedColumn::UniqueId { data, nulls }
            }
            TypedColumn::Bool { data, nulls } => {
                let (data, nulls) = gather_fixed(data, nulls, rows);
                TypedColumn::Bool { data, nulls }
            }
            TypedColumn::Date { data, nulls } => {
                let (data, nulls) = gather_fixed(data, nulls, rows);
                TypedColumn::Date { data, nulls }
            }
            TypedColumn::Timestamp { data, nulls } => {
                let (data, nulls) = gather_fixed(data, nulls, rows);
                TypedColumn::Timestamp { data, nulls }
            }
            TypedColumn::Str { .. } => {
                let mut offsets = Vec::with_capacity(rows.len() + 1);
                offsets.push(0u64);
                let mut bytes = Vec::new();
                let mut nulls = Vec::with_capacity(rows.len());
                for &row in rows {
                    match self.get_str(row) {
                        Some(s) => {
                            bytes.extend_from_slice(s.as_bytes());
                            nulls.push(0);
                        }
                        None => nulls.push(1),
                    }
                    offsets.push(bytes.len() as u64);
                }
                bytes.shrink_to_fit();
                TypedColumn::Str {
                    offsets: MmapOrVec::from_vec(offsets),
                    data: MmapBytes::Heap { data: bytes },
                    nulls: MmapOrVec::from_vec(nulls),
                    relocated: FxHashMap::default(),
                }
            }
            TypedColumn::Mixed { data } => TypedColumn::Mixed {
                data: rows
                    .iter()
                    .map(|&row| data.get(row as usize).cloned().unwrap_or(Value::Null))
                    .collect(),
            },
        }
    }
}
