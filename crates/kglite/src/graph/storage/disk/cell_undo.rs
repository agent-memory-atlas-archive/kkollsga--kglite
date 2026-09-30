//! The disk graph's half of the `DiskCells` statement checkpoint.
//!
//! A disk statement's rollback checkpoint used to be a whole-graph clone
//! sharing every column `Arc`, so the statement's first flush deep-copied each
//! touched column: O(rows of the type) per statement, however few cells it
//! wrote. `StatementCheckpoint::DiskCells` (`dir_graph::rollback`) drops the
//! snapshot's column stores instead, which leaves the live ones uniquely owned
//! and written in place, and records here what each write overwrote. The
//! record is the same [`UndoJournal`] entries a heap-backed statement journals,
//! replayed by the same code into the live stores.
//!
//! The flush is the one place disk cells change inside a statement: the
//! Cypher columnar master path refuses disk graphs, and every other write to a
//! disk graph's columns runs outside a statement window.

use crate::datatypes::Value;
use crate::graph::schema::{InternedKey, NodeData, PropertyStorage};
use crate::graph::storage::column_store::ColumnStore;
use crate::graph::storage::undo::UndoJournal;

use super::graph::DiskGraph;

impl DiskGraph {
    /// Start journaling the cell writes of one statement.
    pub(crate) fn begin_cell_undo(&mut self) {
        self.statement_undo = Some(Box::default());
    }

    /// Stop journaling and hand back what was recorded.
    pub(crate) fn take_cell_undo(&mut self) -> Option<Box<UndoJournal>> {
        self.statement_undo.take()
    }
}

/// Apply staged node writes to one type's store, journaling each overwritten
/// cell first when a statement's undo is open.
///
/// The capture precedes every write because afterwards the prior value exists
/// nowhere; replay runs newest-first, so a cell written twice is restored by
/// its earliest capture. A write that rebuilds a column (a typed column
/// demoted to `Mixed`) is journaled after its cell, so replay reinstates the
/// column before the cell entries restore into it.
pub(super) fn write_staged_rows(
    store: &mut ColumnStore,
    type_key: InternedKey,
    rows: Vec<(u32, bool, NodeData)>,
    mut undo: Option<&mut UndoJournal>,
) {
    if undo.is_some() {
        store.begin_displaced_log();
    }
    for (row_id, alive, nd) in rows {
        if !alive {
            // Tombstoned by `remove_node` — mark the row dead in the
            // ColumnStore so reloads skip it.
            if let Some(undo) = undo.as_deref_mut() {
                if !store.is_tombstoned(row_id) {
                    undo.note_columnar_tombstone(type_key, row_id);
                }
            }
            store.tombstone(row_id);
            continue;
        }
        // Avoid redundant title writes while preserving explicit Map clears.
        if matches!(nd.properties, PropertyStorage::Map(_)) || !matches!(nd.title, Value::Null) {
            let prior = store.get_title(row_id);
            if prior.as_ref().unwrap_or(&Value::Null) != &nd.title {
                if let Some(undo) = undo.as_deref_mut() {
                    undo.note_columnar_title(type_key, row_id, prior);
                }
                let _ = store.set_title(row_id, &nd.title);
            }
        }
        if let PropertyStorage::Map(map) = &nd.properties {
            for (key, value) in map {
                if let Some(undo) = undo.as_deref_mut() {
                    undo.note_columnar_cell(store, type_key, row_id, *key);
                }
                let _ = store.set(row_id, *key, value, None);
            }
        }
        if let Some(undo) = undo.as_deref_mut() {
            if store.has_displaced() {
                undo.note_columns_displaced(type_key, store.take_displaced());
            }
        }
    }
    if undo.is_some() {
        store.end_displaced_log();
    }
}
