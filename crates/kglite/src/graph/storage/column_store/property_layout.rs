//! The property columns of a store as `(key, kind)`, wherever they live.

use super::*;
use crate::graph::storage::mapped::column_store::ColRef;

impl ColumnStore {
    /// Every property column of this store with the kind a copy must give it:
    /// the heap columns in slot order, then the mmap base's columns that the
    /// heap does not shadow, in key order.
    ///
    /// [`Self::schema`] alone is not this: a store served from an mmap base
    /// keeps its columns in the mapping and reports an empty schema, so a
    /// writer that took its columns from the schema would send every property
    /// of such a store down the overflow bag. When a heap column and the base
    /// hold the same key with different kinds the copy's column is `mixed`, the
    /// one kind that holds a value of either.
    pub fn property_layout(&self) -> Vec<(InternedKey, &'static str)> {
        let mut layout: Vec<(InternedKey, &'static str)> = self
            .schema
            .iter()
            .filter_map(|(slot, key)| Some((key, self.columns.get(slot as usize)?.type_tag())))
            .collect();
        let Some(base) = self.mmap_store.as_ref() else {
            return layout;
        };
        let heap_len = layout.len();
        let mut keys: Vec<InternedKey> = base.col_map.keys().copied().collect();
        keys.sort_by_key(|key| key.as_u64());
        for key in keys {
            let kind = match base.col_map[&key] {
                ColRef::Fixed(index) => base.fixed_cols[index].col_type.type_tag(),
                ColRef::Str(_) => "string",
            };
            match layout[..heap_len].iter_mut().find(|(k, _)| *k == key) {
                Some((_, heap_kind)) if *heap_kind != kind => *heap_kind = "mixed",
                Some(_) => {}
                None => layout.push((key, kind)),
            }
        }
        layout
    }
}
