//! Embedding stores in a v9 container: which stores leave the compressed
//! section for the mappable data region, and how a load attaches them back.
//!
//! A store written to the data region keeps its compressed-section record
//! (slot maps, metric, model id, text hashes) with `data` left empty; its
//! vectors and L2 norms are two `f32` sections named
//! `emb:<type>:<property>:data|norms` (node stores, owner `embeddings`) or
//! `edge_emb:<relationship>:<property>:data|norms` (owner `edge_embeddings`).
//! Small stores stay inline exactly as in v8, so a v9 file is a mix.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::sync::Arc;

use serde::{Serialize, Serializer};

use super::invalid_data;
use super::load_options::LoadOptions;
use super::mappable::{
    must_materialize_before_save, resolve_mapping, MapContext, MapEntry, MappableSection,
    MappableTable, MappedFile, MappedSection, Placement, SaveLayout, SectionDtype,
};
use crate::graph::schema::{DirGraph, EmbeddingStore};
use crate::graph::vector_buf::VectorBuf;

pub(super) const NODE_OWNER: &str = "embeddings";
pub(super) const EDGE_OWNER: &str = "edge_embeddings";
const NODE_SCOPE: &str = "emb";
const EDGE_SCOPE: &str = "edge_emb";

/// Section classes (`MapEntry::kind`) owned by this module.
pub(super) const KIND_VECTORS: u8 = 1;
pub(super) const KIND_NORMS: u8 = 2;

/// Writer floor: a store whose vectors are at least this many bytes moves to
/// the data region. Independent of the load-side `Auto` floor.
pub(crate) const WRITE_MIN_BYTES: usize = 4 * 1024 * 1024;

/// Load-side `Auto` floor: a store of at least this many bytes is mapped.
pub(crate) const AUTO_MAP_MIN_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn escape(part: &str) -> String {
    part.replace('%', "%25").replace(':', "%3A")
}

fn section_name(scope: &str, first: &str, second: &str, part: &str) -> String {
    format!("{scope}:{}:{}:{part}", escape(first), escape(second))
}

/// Whether a save under `layout` writes this store's vectors to the data
/// region. Needs consistent norms (they are written alongside) and a
/// little-endian host (the section bytes are the in-memory bytes).
pub(crate) fn store_goes_to_data_region(store: &EmbeddingStore, layout: SaveLayout) -> bool {
    layout == SaveLayout::Auto
        && cfg!(target_endian = "little")
        && store.norms.len() == store.len()
        && store.data.len().saturating_mul(4) >= WRITE_MIN_BYTES
}

/// A node store as serialized into `embeddings.zst`: whole, or as a shell
/// whose vectors are in the data region.
pub(super) enum NodeStoreRecord<'a> {
    Whole(&'a EmbeddingStore),
    Shell(Box<EmbeddingStore>),
}

impl<'a> NodeStoreRecord<'a> {
    pub(super) fn new(store: &'a EmbeddingStore, layout: SaveLayout) -> io::Result<Self> {
        // A whole store serializes its vectors through a slice, so a corrupt
        // mapped store must be refused before it is laundered into a new file.
        store.verify_storage()?;
        Ok(if store_goes_to_data_region(store, layout) {
            NodeStoreRecord::Shell(Box::new(store.persist_shell()))
        } else {
            NodeStoreRecord::Whole(store)
        })
    }
}

impl Serialize for NodeStoreRecord<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            NodeStoreRecord::Whole(store) => store.serialize(serializer),
            NodeStoreRecord::Shell(shell) => shell.serialize(serializer),
        }
    }
}

#[cfg(target_endian = "little")]
fn f32_bytes(values: &[f32]) -> Cow<'_, [u8]> {
    // SAFETY: any f32 slice is valid as bytes (no padding, align 1 <= 4); the
    // borrow keeps the slice alive and unchanged; little-endian hosts write
    // the format's byte order directly.
    Cow::Borrowed(unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    })
}

#[cfg(not(target_endian = "little"))]
fn f32_bytes(values: &[f32]) -> Cow<'_, [u8]> {
    Cow::Owned(values.iter().flat_map(|v| v.to_le_bytes()).collect())
}

fn push_store_sections<'g>(
    out: &mut Vec<MappableSection<'g>>,
    scope: &str,
    owner: &str,
    keys: (&str, &str),
    store: &'g EmbeddingStore,
) {
    for (part, kind, values) in [
        ("data", KIND_VECTORS, &store.data),
        ("norms", KIND_NORMS, &store.norms),
    ] {
        out.push(MappableSection {
            name: section_name(scope, keys.0, keys.1, part),
            owner: owner.to_string(),
            kind,
            dtype: SectionDtype::F32,
            elem_count: values.len() as u64,
            flags: 0,
            data: f32_bytes(values),
        });
    }
}

/// The embedding sections a save of `graph` writes under `layout`, in the
/// order the table will list them: node stores, then relationship stores,
/// each by key. Borrows the vectors; a mapped store streams from its mapping.
pub(super) fn collect_embedding_sections(
    graph: &DirGraph,
    layout: SaveLayout,
) -> io::Result<Vec<MappableSection<'_>>> {
    let mut out = Vec::new();
    if layout == SaveLayout::Compact {
        return Ok(out);
    }
    let node: BTreeMap<_, _> = graph.embeddings.iter().collect();
    for (key, store) in node {
        if store_goes_to_data_region(store, layout) {
            store.verify_storage()?;
            push_store_sections(&mut out, NODE_SCOPE, NODE_OWNER, (&key.0, &key.1), store);
        }
    }
    let edge: BTreeMap<_, _> = graph.edge_embeddings.iter().collect();
    for (key, store) in edge {
        let numeric = store.numeric();
        if store_goes_to_data_region(numeric, layout) {
            numeric.verify_storage()?;
            push_store_sections(&mut out, EDGE_SCOPE, EDGE_OWNER, (&key.0, &key.1), numeric);
        }
    }
    Ok(out)
}

/// Open the data-region table of a v9 container, deciding per embedding store
/// whether its sections are mapped or copied (`LoadOptions::embeddings`). The
/// decision is made once per store, from the vector section's length, so the
/// norms always follow the vectors. Column sections are placed per column by
/// `LoadOptions::columns`; sections of other classes go to the heap.
pub(super) fn open_table(
    entries: &[MapEntry],
    bytes: &[u8],
    base: u64,
    file: Option<&Arc<MappedFile>>,
    options: &LoadOptions,
) -> io::Result<MappableTable> {
    let lengths: HashMap<&str, u64> = entries.iter().map(|e| (e.name.as_str(), e.len)).collect();
    let columns = super::column_sections::ColumnPlacer::new(entries);
    MappableTable::open_placed(entries, bytes, base, file, options.verify, |entry| {
        if entry.kind == super::column_sections::KIND_COLUMN {
            return columns.place(entry, file, options.columns);
        }
        if entry.kind != KIND_VECTORS && entry.kind != KIND_NORMS {
            return Placement::Heap;
        }
        let vectors_len = entry
            .name
            .strip_suffix(":norms")
            .and_then(|stem| lengths.get(format!("{stem}:data").as_str()).copied())
            .unwrap_or(entry.len);
        let ctx = MapContext::host(file.map(|f| &**f), vectors_len);
        if resolve_mapping(options.embeddings, AUTO_MAP_MIN_BYTES, &ctx) {
            Placement::Mapped
        } else {
            Placement::Heap
        }
    })
}

fn f32_buffer(section: &MappedSection) -> VectorBuf {
    if section.is_mapped() {
        if let Some(buffer) = VectorBuf::mapped(section.clone()) {
            return buffer;
        }
    }
    VectorBuf::Heap(
        section
            .as_bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

fn expect_f32<'t>(
    table: &'t MappableTable,
    name: &str,
    kind: u8,
) -> io::Result<Option<&'t MappedSection>> {
    match table.get(name) {
        Some(section) if section.dtype != SectionDtype::F32 || section.kind != kind => {
            Err(invalid_data(format!(
                "mappable section '{name}' is not an f32 embedding section"
            )))
        }
        found => Ok(found),
    }
}

/// Give `store` the vectors and norms of its data-region sections, if it has
/// any. Returns `Some(true)` when both are mapped, so the caller may trust the
/// norms; `Some(false)` when they were copied (norms are rebuilt and checked
/// like a v8 load); `None` for an inline store.
fn install_store_sections(
    table: &MappableTable,
    claimed: &mut HashSet<String>,
    scope: &str,
    keys: (&str, &str),
    store: &mut EmbeddingStore,
) -> io::Result<Option<bool>> {
    let data_name = section_name(scope, keys.0, keys.1, "data");
    let norms_name = section_name(scope, keys.0, keys.1, "norms");
    let Some(data) = expect_f32(table, &data_name, KIND_VECTORS)? else {
        return Ok(None);
    };
    let norms = expect_f32(table, &norms_name, KIND_NORMS)?.ok_or_else(|| {
        invalid_data(format!(
            "mappable section '{data_name}' has no matching '{norms_name}' section"
        ))
    })?;
    if norms.elem_count != store.slot_to_node.len() as u64 {
        return Err(invalid_data(format!(
            "mappable section '{norms_name}' holds {} norms for {} slots",
            norms.elem_count,
            store.slot_to_node.len()
        )));
    }
    store.data = f32_buffer(data);
    store.norms = f32_buffer(norms);
    claimed.insert(data_name);
    claimed.insert(norms_name);
    Ok(Some(store.data.is_mapped() && store.norms.is_mapped()))
}

/// Refuse a table entry owned by `owner` that no store claimed.
fn check_all_claimed(
    table: &MappableTable,
    claimed: &HashSet<String>,
    owner: &str,
) -> io::Result<()> {
    match table
        .iter()
        .find(|s| s.owner == owner && !claimed.contains(&s.name))
    {
        Some(stray) => Err(invalid_data(format!(
            "mappable section '{}' names no store of '{owner}'",
            stray.name
        ))),
        None => Ok(()),
    }
}

/// Attach data-region vectors to the decoded node stores, then validate every
/// store: shape always; norms rebuilt-and-checked unless a mapped store
/// supplied its own.
pub(super) fn attach_and_validate_node_stores(
    embeddings: &mut HashMap<(String, String), EmbeddingStore>,
    table: &MappableTable,
) -> io::Result<()> {
    let mut claimed = HashSet::new();
    for (key, store) in embeddings.iter_mut() {
        let trusted =
            install_store_sections(table, &mut claimed, NODE_SCOPE, (&key.0, &key.1), store)?;
        store.validate_shape().map_err(invalid_data)?;
        if trusted != Some(true) {
            store
                .rebuild_norms_checked()
                .map_err(|error| invalid_data(error.to_string()))?;
        }
    }
    check_all_claimed(table, &claimed, NODE_OWNER)
}

/// The edge-store counterpart of [`install_store_sections`], shaped for
/// `validate_decoded_edge_embedding_stores_with`.
pub(super) fn install_edge_store(
    table: &MappableTable,
    claimed: &mut HashSet<String>,
    key: &(String, String),
    store: &mut EmbeddingStore,
) -> Result<bool, String> {
    install_store_sections(table, claimed, EDGE_SCOPE, (&key.0, &key.1), store)
        .map(|installed| installed == Some(true))
        .map_err(|error| error.to_string())
}

pub(super) fn check_edge_claimed(
    table: &MappableTable,
    claimed: &HashSet<String>,
) -> io::Result<()> {
    check_all_claimed(table, claimed, EDGE_OWNER)
}

/// The files `graph`'s mapped embedding and column buffers read from.
pub(crate) fn mapped_sources(graph: &DirGraph) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = graph
        .embeddings
        .values()
        .chain(graph.edge_embeddings.values().map(|s| s.numeric()))
        .filter_map(|store| store.data.mapped_source().map(std::path::Path::to_path_buf))
        .collect();
    paths.extend(super::column_sections::mapped_sources(graph));
    paths.sort();
    paths.dedup();
    paths
}

/// Copy every mapped embedding and column buffer of `graph` to the heap.
pub(crate) fn promote_all(graph: &mut DirGraph) {
    for store in graph.embeddings.values_mut() {
        store.promote_to_heap();
    }
    for store in graph.edge_embeddings.values_mut() {
        store.numeric_mut().promote_to_heap();
    }
    super::column_sections::promote_all(graph);
}

/// Before a save to `path`: where the platform cannot rename over a file this
/// graph has mapped ([`must_materialize_before_save`]), promote the graph's
/// mapped embeddings to the heap so the mapping is released.
pub(crate) fn release_mappings_of_destination(
    graph: &mut std::sync::Arc<DirGraph>,
    path: &str,
    windows: bool,
) {
    let held = mapped_sources(graph);
    if held.is_empty() {
        return;
    }
    let canonical =
        |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dest = canonical(std::path::Path::new(path));
    let held: Vec<std::path::PathBuf> = held.iter().map(|p| canonical(p)).collect();
    let held_refs: Vec<&std::path::Path> = held.iter().map(|p| p.as_path()).collect();
    if must_materialize_before_save(windows, &dest, &held_refs) {
        let dir = crate::graph::handle::make_dir_graph_mut_preserving_lineage(graph);
        promote_all(dir);
    }
}
