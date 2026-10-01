//! Carrying unchanged per-type column files from the generation a save replaces
//! into the stage it is writing.
//!
//! A generation's node types each live in one immutable file
//! (`seg_000/type_columns/<hex>.bin`), so a save that left a type alone can give
//! the next generation *the same file* — a hard link, which costs a directory
//! entry — instead of writing its bytes again. That is only sound for a type
//! nothing has touched: the live store must be a pure view of a file the
//! previous generation owns ([`ColumnStore::pure_mmap_store`], whose origin is
//! that file). Anything else — a `SET` overlay, a tail, a tombstone, a flattened
//! or renumbered store, a store mapped from some other generation or directory —
//! is written afresh by the ordinary path.
//!
//! Nothing ever writes through a carried file. Published column files are
//! mapped read-only, created with `create_new`, and never truncated, which is
//! what lets two generations share an inode without either being able to alter
//! the other. Besides the per-type column files, `id_indices.bin` and
//! `type_indices.bin` are carried when the live index is an unchanged view of
//! the previous generation's file ([`carry_unchanged_index`]); the CSR, node
//! slots and edge properties are still mapped writable by the loader and are
//! rewritten by every save.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::graph::io::columns_meta::{self, ColumnsMeta};
use crate::graph::io::ntriples::ColumnTypeMeta;
use crate::graph::storage::column_store::ColumnStore;

/// The per-type column files of the generation a save is replacing.
pub(crate) struct PreviousColumns {
    /// Directory the column metadata sits in; `files` entries resolve against it.
    seg_dir: PathBuf,
    meta: ColumnsMeta,
    by_type: HashMap<String, usize>,
}

/// A type whose current file the previous generation already holds.
pub(crate) struct Reusable {
    pub(crate) meta: ColumnTypeMeta,
    /// The file's name relative to the sidecar's directory, kept as it is so the
    /// carried file is addressed the same way in both generations.
    pub(crate) file: String,
    pub(crate) source: PathBuf,
}

#[cfg(test)]
thread_local! {
    static FORCE_COPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `body` with hard-linking refused on this thread, so the copy fallback
/// is what carries files.
#[cfg(test)]
pub(crate) fn with_linking_refused<T>(body: impl FnOnce() -> T) -> T {
    crate::graph::test_scope::scoped(&FORCE_COPY, true, false, body)
}

impl PreviousColumns {
    /// The per-type files of the generation at `snapshot_dir`, or `None` when it
    /// has none (a shared `columns.bin` layout, sidecars only, or no column
    /// metadata that reads).
    pub(crate) fn open(snapshot_dir: &Path) -> Option<Self> {
        let meta_path = columns_meta::locate(snapshot_dir)?;
        let meta = columns_meta::read(&meta_path).ok()?;
        if meta.files.is_empty() {
            return None;
        }
        let by_type = meta
            .types
            .iter()
            .enumerate()
            .map(|(index, ty)| (ty.type_name.clone(), index))
            .collect();
        Some(Self {
            seg_dir: meta_path.parent().unwrap_or(snapshot_dir).to_path_buf(),
            meta,
            by_type,
        })
    }

    /// The previous file `store` is an unchanged view of, with the metadata that
    /// describes it.
    pub(crate) fn reusable(&self, type_name: &str, store: &ColumnStore) -> Option<Reusable> {
        let mapped = store.pure_mmap_store()?;
        let origin = mapped.origin.as_ref()?;
        let file = self.meta.files.get(type_name)?;
        let source = columns_meta::resolve_type_file(&self.seg_dir, file).ok()?;
        if !same_file(&origin.path, &source) {
            return None;
        }
        let meta = self.meta.types.get(*self.by_type.get(type_name)?)?;
        if meta.row_count != mapped.row_count() {
            return None;
        }
        let on_disk = fs::metadata(&source).ok()?.len();
        if meta.extent() as u64 > on_disk {
            return None;
        }
        Some(Reusable {
            meta: meta.clone(),
            file: file.clone(),
            source,
        })
    }
}

/// Whether two paths name one file. Equal spellings — which a graph loaded and
/// saved under one path always has — skip the filesystem.
fn same_file(mapped: &Path, candidate: &Path) -> bool {
    if mapped == candidate {
        return true;
    }
    match (fs::canonicalize(mapped), fs::canonicalize(candidate)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Give the stage `relative` (under `stage_seg0`) as the file at `source`: a
/// hard link where the platform and filesystem allow one, else a copy.
pub(crate) fn carry(source: &Path, stage_seg0: &Path, relative: &str) -> io::Result<()> {
    let destination = columns_meta::resolve_type_file(stage_seg0, relative)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    link_or_copy(source, &destination)
}

fn link_or_copy(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    let refused = FORCE_COPY.with(|flag| flag.get());
    #[cfg(not(test))]
    let refused = false;
    // Windows copies: a link to a file another handle maps is not worth the
    // sharing rules it brings for a saving nobody measured there.
    if !refused && cfg!(not(windows)) && fs::hard_link(source, destination).is_ok() {
        return Ok(());
    }
    fs::copy(source, destination)?;
    Ok(())
}

/// Give the stage the previous generation's `name` (`id_indices.bin` or
/// `type_indices.bin`) when the live index is an unchanged view of exactly that
/// file: `origin` is the file the index maps, or `None` when it has changed or
/// maps nothing. `false` leaves the stage without the file, so the caller
/// writes it.
pub(crate) fn carry_unchanged_index(
    previous: Option<&Path>,
    name: &str,
    origin: Option<&Path>,
    stage: &Path,
) -> bool {
    let (Some(previous), Some(origin)) = (previous, origin) else {
        return false;
    };
    let source = previous.join(name);
    if !same_file(origin, &source) {
        return false;
    }
    link_or_copy(&source, &stage.join(name)).is_ok()
}
