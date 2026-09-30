//! Rows appended past an mmap base live in a **tail store**, not in the base.
//!
//! A reopened disk graph serves each type from its published column file. A
//! row appended to such a type used to move the whole type onto the heap or
//! into spill files first (every region copied, a tombstone byte per existing
//! row), so a 1 k-row append cost as much as the type. Now the base is never
//! touched: the new rows go into an ordinary owned [`ColumnStore`], the tail,
//! and a row id `>= base rows` resolves there (`row - base rows`).
//!
//! Only a store with an mmap base has a tail, so a store without one — every
//! in-memory and mapped-mode store — pays one `Option` test per routed call and
//! nothing else. That also makes every column-level fast path that is
//! disqualified by [`ColumnStore::has_mmap_base`] (`column_filter`,
//! `date_cells`, `timestamp_cells`, `gather_rows`, the describe column-major
//! scan, the subgraph gather) disqualified for a tail store too: they read the
//! base part's overlay columns, which cover `0..row_count` only.
//!
//! While a tail exists, `row_count` and every other field on [`ColumnStore`]
//! describe the base part; [`ColumnStore::row_count`] answers for the whole.

use super::{ColumnStore, TypedColumn};
use crate::graph::schema::{InternedKey, StringInterner, TypeSchema};
use std::collections::HashMap;
use std::sync::Arc;

impl ColumnStore {
    /// The tail and the row inside it, when `row_id` is past the base part.
    #[inline]
    pub(super) fn tail_for(&self, row_id: u32) -> Option<(&ColumnStore, u32)> {
        match self.tail.as_deref() {
            Some(tail) if row_id >= self.row_count => Some((tail, row_id - self.row_count)),
            _ => None,
        }
    }

    /// Mutable form of [`Self::tail_for`]. Privatises the tail (`Arc::make_mut`),
    /// which is the whole cost a fork's first append pays: the tail's own
    /// columns are shared per column, and the base is not involved.
    #[inline]
    pub(super) fn tail_for_mut(&mut self, row_id: u32) -> Option<(&mut ColumnStore, u32)> {
        let base = self.row_count;
        match self.tail.as_mut() {
            Some(tail) if row_id >= base => Some((Arc::make_mut(tail), row_id - base)),
            _ => None,
        }
    }

    /// The tail, created empty on demand: only a store that reads through an
    /// mmap base has one, and appending to it is the only reason to ask.
    ///
    /// `None` for any other store.
    #[inline]
    pub(super) fn tail_for_append(&mut self) -> Option<&mut ColumnStore> {
        self.mmap_store.as_ref()?;
        let tail = self.tail.get_or_insert_with(|| {
            Arc::new(ColumnStore::new(
                Arc::new(TypeSchema::new()),
                &HashMap::new(),
                &StringInterner::new(),
            ))
        });
        Some(Arc::make_mut(tail))
    }

    /// Give an mmap-backed store its tail, typed from the type's registered
    /// schema and declared property types exactly as a fresh store is.
    ///
    /// A no-op for a store without an mmap base or one that already has a tail.
    /// Optional: the first append creates an untyped tail on its own, which
    /// then takes each column's type from its first value.
    pub(crate) fn prepare_append(
        &mut self,
        schema: Arc<TypeSchema>,
        metadata: &HashMap<String, String>,
        interner: &StringInterner,
    ) {
        if self.mmap_store.is_some() && self.tail.is_none() {
            self.tail = Some(Arc::new(ColumnStore::new(schema, metadata, interner)));
        }
    }

    /// Rows in the tail.
    #[inline]
    pub(crate) fn tail_rows(&self) -> u32 {
        self.tail.as_ref().map_or(0, |tail| tail.row_count())
    }

    /// Whether any row has been appended past the base.
    #[inline]
    pub(crate) fn has_tail_rows(&self) -> bool {
        self.tail_rows() > 0
    }

    /// Drop a tail that holds no rows, so a store whose only append was rolled
    /// back is a pure mmap base again and is re-emitted from its mapping.
    pub(super) fn drop_empty_tail(&mut self) {
        if self.tail.as_ref().is_some_and(|tail| tail.row_count() == 0) {
            self.tail = None;
        }
    }

    /// The key of the base part's schema slot, for a slot-addressed call that
    /// lands on a tail row (the tail's schema is its own).
    pub(super) fn base_key_at(&self, slot: u16) -> Option<InternedKey> {
        self.schema
            .iter()
            .find_map(|(s, key)| (s == slot).then_some(key))
    }

    /// Whether a save can write the base regions and the tail regions of this
    /// store into one column file without decoding either onto the heap.
    ///
    /// The base part must be nothing but its mapping (no overlay, clears,
    /// tombstones or overflow bag) and each column the tail carries must be
    /// the same fixed-width or string kind as the base's column of that key;
    /// identity columns likewise. Anything else is flattened, which reads
    /// through the routed per-row accessors and so sees the union.
    pub(crate) fn tail_is_region_compatible(&self) -> bool {
        let (Some(ms), Some(tail)) = (self.mmap_store.as_ref(), self.tail.as_ref()) else {
            return false;
        };
        let base_untouched = self.columns.is_empty()
            && self.id_column.is_none()
            && self.title_column.is_none()
            && self.null_overrides.is_none()
            && self.overflow_offsets.is_none()
            && !self.tombstones.iter().any(|t| *t)
            && self.row_count == ms.row_count()
            && !ms.has_overflow;
        if !base_untouched || tail.mmap_store.is_some() || tail.has_overflow() {
            return false;
        }
        let kind_matches = |tail_column: &TypedColumn, base: Option<&'static str>| {
            !matches!(tail_column, TypedColumn::Mixed { .. })
                && base.is_none_or(|base| base == tail_column.type_tag())
        };
        for (slot, key) in tail.schema.iter() {
            let Some(column) = tail.columns.get(slot as usize) else {
                continue;
            };
            // A tail column with no value takes the base's kind whatever its
            // declared one was (`Self::column_for_plan`).
            let base = ms.column_kind(key);
            if !kind_matches(column, base)
                && !(base.is_some() && tail.column_holds_no_value(column))
            {
                return false;
            }
        }
        if let Some(column) = tail.id_column.as_deref() {
            if !ms.has_id_column() || !kind_matches(column, Some(ms.id_kind())) {
                return false;
            }
        }
        if let Some(column) = tail.title_column.as_deref() {
            if !ms.has_title_column() || !kind_matches(column, Some(ms.title_kind())) {
                return false;
            }
        }
        true
    }

    /// Whether every cell of `column` (one of this store's) is null.
    fn column_holds_no_value(&self, column: &TypedColumn) -> bool {
        !(0..column.len() as u32).any(|row| column.is_present(row))
    }

    /// The tail's column for `key`, as a save writes it beside a base column
    /// of kind `base`: `None` when the column has no value and is another kind
    /// than the base's, which the file then records as nulls of the base's kind.
    pub(crate) fn column_for_plan(
        &self,
        key: InternedKey,
        base: Option<&'static str>,
    ) -> Option<&TypedColumn> {
        let column = self.columns.get(self.schema.slot(key)? as usize)?;
        let other_kind = base.is_some_and(|kind| kind != column.type_tag());
        (!(other_kind && self.column_holds_no_value(column))).then_some(&**column)
    }

    /// The base mapping and the tail, when [`Self::tail_is_region_compatible`].
    pub(crate) fn base_and_tail(
        &self,
    ) -> Option<(
        &Arc<crate::graph::storage::mapped::column_store::MmapColumnStore>,
        &ColumnStore,
    )> {
        if !self.tail_is_region_compatible() || !self.has_tail_rows() {
            return None;
        }
        Some((self.mmap_store.as_ref()?, self.tail.as_deref()?))
    }
}
