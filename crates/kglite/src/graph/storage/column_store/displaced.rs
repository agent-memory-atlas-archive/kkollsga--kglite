//! Recording the columns a type change replaces, so a rolled-back write can put
//! the column's *type* back and not only its cells.
//!
//! A write whose value a column cannot hold rebuilds the whole column: a typed
//! column becomes `Mixed`, or an all-null column is retyped for the value
//! ([`ColumnStore::widen_for`]); the first title write to an mmap-backed store
//! promotes the title to a dense `Mixed` column. Restoring the cell values
//! afterwards leaves the rebuilt column behind. In memory that is invisible; on
//! disk it is not, because a `Mixed` column has no file representation, and
//! the heap copy of a column the base serves stays resident.
//!
//! While a disk statement's undo is open, the staged-node flush and the node
//! append arm the log around their own writes
//! ([`ColumnStore::begin_displaced_log`]); the displaced column is moved into
//! it, not copied, so the log costs a refcount on the statement that changes a
//! column's type and nothing on any other.

use std::sync::Arc;

use super::{ColumnStore, TypedColumn};

/// Which column of a store a type change replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColumnRole {
    /// The property column at this schema slot.
    Property(u16),
    /// The reserved id column.
    Id,
    /// The reserved title column.
    Title,
}

/// A column a type change replaced, as it stood just before.
#[derive(Debug)]
pub(crate) struct DisplacedColumn {
    pub(crate) role: ColumnRole,
    /// `None` only for a title column that did not exist yet (the store read
    /// titles through its mmap base); always `Some` for a property or id column.
    pub(crate) prior: Option<Arc<TypedColumn>>,
}

impl ColumnStore {
    /// Start recording replaced columns. Idempotent.
    pub(crate) fn begin_displaced_log(&mut self) {
        self.displaced.get_or_insert_with(Vec::new);
    }

    /// Stop recording. Anything still logged is dropped.
    pub(crate) fn end_displaced_log(&mut self) {
        self.displaced = None;
    }

    /// Whether a column was replaced since the log was last drained.
    #[inline]
    pub(crate) fn has_displaced(&self) -> bool {
        self.displaced.as_ref().is_some_and(|log| !log.is_empty())
    }

    /// Drain the log, oldest replacement first.
    pub(crate) fn take_displaced(&mut self) -> Vec<DisplacedColumn> {
        self.displaced
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Put a replaced column back. The inverse of the replacement, for the
    /// rollback of the write that caused it.
    pub(crate) fn restore_displaced(&mut self, displaced: DisplacedColumn) {
        match (displaced.role, displaced.prior) {
            (ColumnRole::Property(slot), Some(prior)) => {
                if let Some(handle) = self.columns.get_mut(slot as usize) {
                    *handle = prior;
                }
            }
            (ColumnRole::Id, prior) => self.id_column = prior,
            (ColumnRole::Title, prior) => self.title_column = prior,
            (ColumnRole::Property(_), None) => {}
        }
    }

    /// Install `column` at `slot`, logging the one it replaces.
    pub(super) fn swap_column(&mut self, slot: usize, column: Arc<TypedColumn>) {
        let old = std::mem::replace(&mut self.columns[slot], column);
        if let Some(log) = self.displaced.as_mut() {
            log.push(DisplacedColumn {
                role: ColumnRole::Property(slot as u16),
                prior: Some(old),
            });
        }
    }

    /// [`Self::swap_column`] for the id column.
    pub(super) fn swap_id_column(&mut self, column: Option<Arc<TypedColumn>>) {
        let old = std::mem::replace(&mut self.id_column, column);
        if let Some(log) = self.displaced.as_mut() {
            log.push(DisplacedColumn {
                role: ColumnRole::Id,
                prior: old,
            });
        }
    }

    /// [`Self::swap_column`] for the title column.
    pub(super) fn swap_title_column(&mut self, column: Option<Arc<TypedColumn>>) {
        let old = std::mem::replace(&mut self.title_column, column);
        if let Some(log) = self.displaced.as_mut() {
            log.push(DisplacedColumn {
                role: ColumnRole::Title,
                prior: old,
            });
        }
    }
}
