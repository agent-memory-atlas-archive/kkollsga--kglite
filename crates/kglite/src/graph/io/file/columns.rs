//! `.kgl` column-section loading — the reader half of the columnar save path.
//!
//! Split out of `io/file.rs` when that file passed its 2500-line ceiling. These
//! three functions are the only place a load installs column stores onto the
//! storage backend, which is their sole owner: the loader reads a section
//! (or a per-type sidecar), builds a `ColumnStore`, hands it to
//! `DirGraph::install_column_store`, and then points each node of the type at
//! its row.

use super::*;
use crate::graph::storage::mapped::column_store::ColumnFileOrigin;

/// Load the per-type zstd sidecars onto the storage backend.
/// Skips entries whose type is already loaded (from the mmap column files).
/// Used by both the earlier per-type layout and the additive path that covers
/// types the column files do not: those added post-build via `add_nodes`, and
/// those holding a column the mmap layout cannot represent.
///
/// A sidecar is found two ways. `sidecars` (from the column metadata) maps a
/// type name to its `columns/<hex>` directory; the type comes from the map, never
/// from the directory. A `columns/` subdirectory no entry names is a 0.19.0
/// layout, written before the map existed, whose directory name *is* the type
/// name — a directory entry name is a single path component, so it cannot lead
/// anywhere else.
pub(super) fn load_column_sidecars(
    dir: &std::path::Path,
    sidecars: &std::collections::BTreeMap<String, String>,
    graph: &mut crate::graph::dir_graph::DirGraph,
) -> io::Result<()> {
    use rayon::prelude::*;

    // Collect job descriptors so the heavy work (read + zstd decode +
    // ColumnStore::load_packed) can run in a rayon thread pool. On a
    // 17M-node Wikidata article-author carve with ~4,500 distinct
    // types, the previous sequential loop spent ~70 s in zstd alone;
    // parallelising drops it to a few seconds on a 16-core machine.
    struct Job {
        type_name: String,
        col_file: std::path::PathBuf,
        schema: Arc<crate::graph::schema::TypeSchema>,
        type_meta: std::collections::HashMap<String, String>,
    }

    let mut candidates: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut named: std::collections::HashSet<std::ffi::OsString> = std::collections::HashSet::new();
    for (type_name, relative) in sidecars {
        let type_dir = columns_meta::resolve_sidecar_dir(dir, relative)?;
        if let Some(name) = type_dir.file_name() {
            named.insert(name.to_os_string());
        }
        candidates.push((type_name.clone(), type_dir));
    }
    let columns_dir = dir.join(columns_meta::SIDECAR_DIR);
    if columns_dir.exists() {
        for entry in std::fs::read_dir(&columns_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || named.contains(&entry.file_name()) {
                continue;
            }
            let type_name = entry.file_name().to_string_lossy().to_string();
            candidates.push((type_name, entry.path()));
        }
    }

    let mut jobs: Vec<Job> = Vec::new();
    for (type_name, type_dir) in candidates {
        if graph.column_store(&type_name).is_some() {
            // the mmap column files already loaded this type.
            continue;
        }
        let col_file = type_dir.join("columns.zst");
        if !col_file.exists() {
            continue;
        }
        let schema = graph
            .type_schemas
            .get(&type_name)
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(crate::graph::schema::TypeSchema::new()));
        let type_meta = graph
            .node_type_metadata
            .get(&type_name)
            .cloned()
            .unwrap_or_default();
        jobs.push(Job {
            type_name,
            col_file,
            schema,
            type_meta,
        });
    }

    // Decompress + load_packed each sidecar in parallel.
    let interner = &graph.interner;
    let results: Vec<io::Result<(String, crate::graph::storage::column_store::ColumnStore)>> = jobs
        .into_par_iter()
        .map(
            |job| -> io::Result<(String, crate::graph::storage::column_store::ColumnStore)> {
                let compressed = std::fs::read(&job.col_file)?;
                let decoded = zstd_decompress(&compressed)?;
                // Current format: `KGLCOLv2` + row_count + Postcard-backed
                // mixed columns. Older sidecars require a pre-0.14 converter.
                if decoded.len() < 12 || &decoded[..8] != b"KGLCOLv2" {
                    return Err(pre_014_bincode_error("KGLCOLv1/raw column sidecar"));
                }
                let packed_slice = &decoded[12..];
                let row_count = u32::from_le_bytes(decoded[8..12].try_into().unwrap());
                let codec = serde_codec::CodecVersion::PostcardV1;
                let store =
                    crate::graph::storage::column_store::ColumnStore::load_packed_with_codec(
                        job.schema,
                        &job.type_meta,
                        interner,
                        packed_slice,
                        row_count,
                        None,
                        codec,
                    )?;
                Ok((job.type_name, store))
            },
        )
        .collect();

    for r in results {
        let (type_name, store) = r?;
        // A sidecar may name columns the rebuilt `type_schemas` entry does not
        // (a `load_ntriples` build persists no `node_type_metadata`, so its
        // types rebuild with an empty schema). `load_packed_with_codec` grows
        // the store's own schema to cover them; publish that schema as the
        // type's, or the graph keeps a narrower one than the store it just
        // installed.
        if graph
            .type_schemas
            .get(&type_name)
            .is_none_or(|schema| schema.len() != store.schema().len())
        {
            graph
                .type_schemas_mut()
                .insert(type_name.clone(), Arc::clone(store.schema()));
        }
        graph.install_column_store(&type_name, Arc::new(store));
    }
    Ok(())
}

/// Map a column file read-only.
///
/// Every file of a published generation is mapped this way and never written
/// through: a generation is immutable, and later saves may hard-link one file
/// into several generations, so a writable mapping would let one stray write
/// alter all of them.
fn map_column_file(path: &std::path::Path) -> io::Result<Arc<memmap2::Mmap>> {
    let file = std::fs::File::open(path).map_err(|e| io_context("opening", path, e))?;
    // SAFETY: a published generation is immutable — writers stage and publish a
    // new one, and `GraphDirectoryLock` serializes them — so this inode is never
    // truncated or rewritten while the mapping is live.
    let mmap =
        unsafe { memmap2::Mmap::map(&file) }.map_err(|e| io_context("memory-mapping", path, e))?;
    Ok(Arc::new(mmap))
}

/// Stores for every type a column sidecar describes, mapped from the files it
/// names.
///
/// `dir` is the directory holding the sidecar; per-type files resolve against
/// it, and types without a file read the shared `columns.bin` beside it (absent
/// on a directory whose types all have files). A type whose regions run past its
/// file is refused here rather than at the first read that indexes past the map.
fn open_column_stores(
    dir: &std::path::Path,
    meta: columns_meta::ColumnsMeta,
    validate_utf8: bool,
) -> io::Result<Vec<(String, crate::graph::storage::column_store::ColumnStore)>> {
    let shared_path = dir.join("columns.bin");
    let pin = crate::graph::storage::disk::generation::GenerationPin::containing(dir);
    let mut shared: Option<Arc<memmap2::Mmap>> = None;
    let mut stores = Vec::with_capacity(meta.types.len());
    for type_meta in &meta.types {
        let (mmap, path) = match meta.files.get(&type_meta.type_name) {
            Some(relative) => {
                let path = columns_meta::resolve_type_file(dir, relative)?;
                (map_column_file(&path)?, path)
            }
            None if shared_path.exists() => {
                if shared.is_none() {
                    shared = Some(map_column_file(&shared_path)?);
                }
                (Arc::clone(shared.as_ref().unwrap()), shared_path.clone())
            }
            // A sidecar-only type of a directory that has no shared file.
            None => continue,
        };
        if type_meta.extent() > mmap.len() {
            return Err(invalid_data(format!(
                "column file '{}' holds {} bytes but the metadata for type '{}' needs {}",
                path.display(),
                mmap.len(),
                type_meta.type_name,
                type_meta.extent()
            )));
        }
        let mut store = type_meta.to_mmap_store(mmap);
        if meta.files.contains_key(&type_meta.type_name) {
            store.origin = Some(Arc::new(ColumnFileOrigin {
                path: path.clone(),
                _pin: pin.clone(),
            }));
        }
        if validate_utf8 {
            store.validate_utf8(&type_meta.type_name)?;
        }
        stores.push((
            type_meta.type_name.clone(),
            crate::graph::storage::column_store::ColumnStore::from_mmap_store(Arc::new(store)),
        ));
    }
    Ok(stores)
}

/// Install a disk graph's column stores — the mmap-backed column files named
/// by `columns_meta` when present, then the per-type `columns/<dir>/columns.zst`
/// sidecars for whatever they do not cover. Cold load-time path.
pub(super) fn load_disk_column_stores(
    dir: &std::path::Path,
    graph: &mut crate::graph::dir_graph::DirGraph,
) -> io::Result<()> {
    use crate::graph::io::load_timing::{log_stage, stage_timer};

    let t = stage_timer();
    // The sidecar is searched for in `seg_000/` as well as the directory root: a
    // later layout moved these files into `seg_000/`, and without both locations
    // the load fell through to the per-type sidecar branch, which returned an
    // empty `column_stores` map and broke `MATCH (n:Type)` after a disk-mode
    // save + reload.
    let mut sidecars = std::collections::BTreeMap::new();
    if let Some(meta_path) = columns_meta::locate(dir) {
        // Read the metadata before mapping anything, so a directory laid out by
        // a newer build is refused by its declared format, not by what maps.
        let meta = columns_meta::read(&meta_path)?;
        let meta_dir = meta_path.parent().unwrap_or(dir);
        sidecars = meta.sidecars.clone();

        // Column-file bytes are untrusted disk input, but the hot string
        // readers use `from_utf8_unchecked` (see MmapColumnStore::read_str).
        // Validate every string column once here — load-time, amortized —
        // so the per-access unchecked conversion stays sound. Opt-out for
        // very large trusted graphs (validation touches every string byte,
        // forcing a full read of the files): KGLITE_SKIP_UTF8_VALIDATION=1.
        let skip_utf8 = std::env::var_os("KGLITE_SKIP_UTF8_VALIDATION").is_some();
        for (type_name, store) in open_column_stores(meta_dir, meta, !skip_utf8)? {
            graph.install_column_store(&type_name, Arc::new(store));
        }
    }
    // Additively load sidecars for types the column files do not cover: types
    // added post-`load_ntriples` via `add_nodes`, and types whose columns the
    // mmap layout cannot hold. The writer emits `columns/<hex>/columns.zst`
    // only for types NOT in `columns_meta`, and the loader skips a type that
    // already has a store.
    load_column_sidecars(dir, &sidecars, graph)?;
    log_stage("column_stores_load", t);
    Ok(())
}

/// After a generation publish, swap every live column store whose type landed in
/// the published column files for an mmap-backed store over those files, so the
/// heap (or workspace-spill) copies are released instead of staying resident
/// until the process exits. Types written as sidecars keep their live store.
///
/// All-or-nothing: no store is installed unless every file mapped. The files
/// were written by this process a moment ago, so load-time UTF-8 validation is
/// skipped. `written` is the metadata that save published in `seg_000/`; without
/// it the published sidecar is located and read.
pub(crate) fn remap_column_stores_to_generation(
    dir: &std::path::Path,
    graph: &mut crate::graph::dir_graph::DirGraph,
    written: Option<columns_meta::ColumnsMeta>,
) -> io::Result<()> {
    #[cfg(test)]
    if crate::graph::dir_graph::post_publish_failpoint("remap_column_stores") {
        return Err(io::Error::other("injected column-store remap failure"));
    }
    let (meta, meta_dir) = match written {
        Some(meta) => (meta, dir.join("seg_000")),
        None => {
            let Some(meta_path) = columns_meta::locate(dir) else {
                return Ok(());
            };
            let meta = columns_meta::read(&meta_path)?;
            (meta, meta_path.parent().unwrap_or(dir).to_path_buf())
        }
    };
    let stores = open_column_stores(&meta_dir, meta, false)?;
    for (type_name, store) in stores {
        graph.install_column_store(&type_name, Arc::new(store));
    }
    Ok(())
}

pub(super) fn attach_portable_column_stores(dir_graph: &mut DirGraph) {
    // `(type name, has id/title)` snapshot first: the loop below takes a
    // mutable node borrow, and the stores now live on the same backend.
    let types: Vec<(String, bool)> = dir_graph
        .column_stores_by_name()
        .into_iter()
        .map(|(name, store)| (name.to_string(), store.has_id_title_columns()))
        .collect();
    for (type_name, has_id_title) in types {
        let type_name = type_name.as_str();
        let Some(indices) = dir_graph.type_indices.get(type_name) else {
            continue;
        };
        for (row_id, node_idx) in indices.iter().enumerate() {
            let Some(node) = dir_graph.graph.node_weight_mut(node_idx) else {
                continue;
            };
            node.properties = PropertyStorage::Columnar(ColumnarRow::new(row_id as u32));
            if has_id_title {
                node.id = Value::Null;
                node.title = Value::Null;
            }
        }
    }
}
