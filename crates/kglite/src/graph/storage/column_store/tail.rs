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

    /// Whether `key` is a property column of this store, wherever it lives: the
    /// schema, the mmap base, or the tail. A key that is none of these is read,
    /// where it is read at all, as a title or id alias.
    pub(crate) fn has_property_column(&self, key: InternedKey) -> bool {
        self.schema.slot(key).is_some()
            || self
                .mmap_store
                .as_ref()
                .is_some_and(|base| base.column_kind(key).is_some())
            || self
                .tail
                .as_deref()
                .is_some_and(|tail| tail.has_property_column(key))
    }

    /// The schema slot of `key` in the part that holds `row_id`: the tail's own
    /// schema for a row past the base, the store's for any other. A write to a
    /// tail row grows the tail's schema, whatever the base part's schema holds.
    #[inline]
    pub(crate) fn slot_for_row(&self, row_id: u32, key: InternedKey) -> Option<u16> {
        match self.tail_for(row_id) {
            Some((tail, _)) => tail.schema.slot(key),
            None => self.schema.slot(key),
        }
    }

    /// The pre-growth schema of the part that holds `row_id` — the tail's for a
    /// row past the base — with its column count and whether it is the tail's:
    /// what [`Self::restore_schema_of`] needs to undo a growth of that part.
    pub(crate) fn schema_pre_image_for_row(&self, row_id: u32) -> (Arc<TypeSchema>, usize, bool) {
        match self.tail_for(row_id) {
            Some((tail, _)) => (tail.schema_arc(), tail.column_count(), true),
            None => (self.schema_arc(), self.column_count(), false),
        }
    }

    /// The tail's schema and column count, when the store has a tail.
    pub(crate) fn tail_schema_pre_image(&self) -> Option<(Arc<TypeSchema>, usize)> {
        let tail = self.tail.as_deref()?;
        Some((tail.schema_arc(), tail.column_count()))
    }

    /// [`Self::restore_schema`] on the tail (`in_tail`) or on the store's own
    /// part. A no-op for a tail that is gone, which a rollback that removed its
    /// only rows has already dropped.
    pub(crate) fn restore_schema_of(
        &mut self,
        in_tail: bool,
        schema: Arc<TypeSchema>,
        column_count: usize,
    ) {
        if !in_tail {
            self.restore_schema(schema, column_count);
        } else if let Some(tail) = self.tail.as_mut() {
            Arc::make_mut(tail).restore_schema(schema, column_count);
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

    /// Whether the base part of this store is its mapping plus columns of
    /// `SET` values: no id or title overlay, explicit clears, tombstones or
    /// overflow bag, and every overlay column the same fixed-width or string
    /// kind as the base's column of that key (a key the base lacks is a new
    /// column).
    fn base_part_is_region_compatible(
        &self,
        ms: &crate::graph::storage::mapped::column_store::MmapColumnStore,
    ) -> bool {
        self.id_column.is_none()
            && self.title_column.is_none()
            && self.null_overrides.is_none()
            && self.overflow_offsets.is_none()
            && !self.tombstones.iter().any(|t| *t)
            && self.row_count == ms.row_count()
            && !ms.has_overflow
            && self.columns_match_base_kinds(ms)
    }

    /// Whether each column of this store (overlay or tail) is a kind the
    /// base's column of the same key can share a file region with.
    fn columns_match_base_kinds(
        &self,
        ms: &crate::graph::storage::mapped::column_store::MmapColumnStore,
    ) -> bool {
        let kind_matches = |column: &TypedColumn, base: Option<&'static str>| {
            !matches!(column, TypedColumn::Mixed { .. })
                && base.is_none_or(|base| base == column.type_tag())
        };
        for (slot, key) in self.schema.iter() {
            let Some(column) = self.columns.get(slot as usize) else {
                continue;
            };
            // A column with no value takes the base's kind whatever its
            // declared one was (`Self::column_for_plan`).
            let base = ms.column_kind(key);
            if !kind_matches(column, base)
                && !(base.is_some() && self.column_holds_no_value(column))
            {
                return false;
            }
        }
        true
    }

    /// Whether the tail can be written beside the base part: a store of its own
    /// whose columns, and identity columns where it has any, are the base's kinds.
    fn tail_is_region_compatible(
        &self,
        ms: &crate::graph::storage::mapped::column_store::MmapColumnStore,
        tail: &ColumnStore,
    ) -> bool {
        if tail.mmap_store.is_some() || tail.has_overflow() || !tail.columns_match_base_kinds(ms) {
            return false;
        }
        let identity_matches = |column: Option<&TypedColumn>, has: bool, kind: &'static str| {
            column.is_none_or(|column| {
                has && !matches!(column, TypedColumn::Mixed { .. }) && column.type_tag() == kind
            })
        };
        identity_matches(tail.id_column.as_deref(), ms.has_id_column(), ms.id_kind())
            && identity_matches(
                tail.title_column.as_deref(),
                ms.has_title_column(),
                ms.title_kind(),
            )
    }

    /// Whether the overlay and the tail give every key the base lacks the same
    /// kind. [`Self::columns_match_base_kinds`] compares each part with the
    /// base only, which a key the base lacks passes whatever the kind, and a
    /// file column has one kind: the plan would take the overlay's and write the
    /// tail's cells as nulls.
    fn agrees_with_tail_on_keys_the_base_lacks(
        &self,
        ms: &crate::graph::storage::mapped::column_store::MmapColumnStore,
        tail: &ColumnStore,
    ) -> bool {
        self.schema.iter().all(|(slot, key)| {
            if ms.column_kind(key).is_some() {
                return true;
            }
            let Some(overlay_kind) = self
                .columns
                .get(slot as usize)
                .map(|column| column.type_tag())
            else {
                return true;
            };
            tail.column_for_plan(key, Some(overlay_kind))
                .is_none_or(|column| column.type_tag() == overlay_kind)
        })
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

    /// What a save writes this store's column file from, when it can do so
    /// without flattening the store onto the heap: the base mapping, the
    /// overlay columns of `SET` values over it, and the tail (see
    /// [`RegionParts`]). `None` for a pure mapping (which a save re-emits
    /// verbatim), for a store without a base, and for anything
    /// the regions cannot hold: which is then flattened, reading through the
    /// routed per-row accessors, and so seeing the union.
    pub(crate) fn region_parts(&self) -> Option<RegionParts<'_>> {
        let base = self.mmap_store.as_ref()?;
        let tail = self.tail.as_deref().filter(|tail| tail.row_count() > 0);
        if (self.columns.is_empty() && tail.is_none()) || !self.base_part_is_region_compatible(base)
        {
            return None;
        }
        if tail.is_some_and(|tail| {
            !self.tail_is_region_compatible(base, tail)
                || !self.agrees_with_tail_on_keys_the_base_lacks(base, tail)
        }) {
            return None;
        }
        Some(RegionParts {
            base,
            overlay: self,
            tail,
        })
    }
}

/// The pieces of one mmap-backed store a save lays out as a single column file.
pub(crate) struct RegionParts<'a> {
    pub(crate) base: &'a Arc<crate::graph::storage::mapped::column_store::MmapColumnStore>,
    /// The store itself: its `columns` hold `SET` values over the base rows, a
    /// cell being the value only where the column is non-null there.
    pub(crate) overlay: &'a ColumnStore,
    pub(crate) tail: Option<&'a ColumnStore>,
}
