//! The property columns of a store as `(key, kind)`, wherever they live.

use super::*;

/// One key's column kind in a layout, and whether the column holds no value.
struct LayoutEntry {
    key: InternedKey,
    kind: &'static str,
    empty: bool,
}

impl LayoutEntry {
    /// Fold in the same key's column of another part. The same kind stays; a
    /// column with no value takes the kind it meets, as it does when a save
    /// writes it beside the base's (`ColumnStore::column_for_plan`); any other
    /// pair is `mixed`, the one kind that holds a value of either.
    fn merge(&mut self, kind: &'static str, empty: bool) {
        if self.kind == kind {
            self.empty &= empty;
        } else if self.empty {
            self.kind = kind;
            self.empty = empty;
        } else if !empty {
            self.kind = "mixed";
        }
    }
}

impl ColumnStore {
    /// Every property column of this store with the kind a copy must give it:
    /// the heap columns in slot order, then the mmap base's columns that the
    /// heap does not shadow, in key order, then the tail's.
    ///
    /// [`Self::schema`] alone is not this: a store served from an mmap base
    /// keeps its columns in the mapping and reports an empty schema, so a
    /// writer that took its columns from the schema would send every property
    /// of such a store down the overflow bag. When parts hold the same key with
    /// different kinds the copy's column is `mixed`, unless one of them holds no
    /// value (see [`LayoutEntry::merge`]).
    pub fn property_layout(&self) -> Vec<(InternedKey, &'static str)> {
        self.layout_entries()
            .into_iter()
            .map(|entry| (entry.key, entry.kind))
            .collect()
    }

    fn layout_entries(&self) -> Vec<LayoutEntry> {
        let mut layout: Vec<LayoutEntry> = self
            .schema
            .iter()
            .filter_map(|(slot, key)| {
                let column = self.columns.get(slot as usize)?;
                Some(LayoutEntry {
                    key,
                    kind: column.type_tag(),
                    empty: column.holds_no_value(),
                })
            })
            .collect();
        let Some(base) = self.mmap_store.as_ref() else {
            return layout;
        };
        let heap_len = layout.len();
        let mut keys: Vec<InternedKey> = base.col_map.keys().copied().collect();
        keys.sort_by_key(|key| key.as_u64());
        for key in keys {
            let Some(kind) = base.column_kind(key) else {
                continue;
            };
            match layout[..heap_len].iter_mut().find(|entry| entry.key == key) {
                Some(entry) => entry.merge(kind, false),
                None => layout.push(LayoutEntry {
                    key,
                    kind,
                    empty: false,
                }),
            }
        }
        for tail_entry in self.tail.iter().flat_map(|tail| tail.layout_entries()) {
            match layout.iter_mut().find(|entry| entry.key == tail_entry.key) {
                Some(entry) => entry.merge(tail_entry.kind, tail_entry.empty),
                None => layout.push(tail_entry),
            }
        }
        layout
    }
}
