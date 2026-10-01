//! Column-file writer for `ColumnStore`s.
//!
//! Produces one immutable `seg_000/type_columns/<key>.bin` per node type and
//! the `seg_000/columns_meta.json` envelope that lists them, matching the
//! region layout the ntriples builder emits and the loader's mmap fast path
//! expects (see [`crate::graph::io::ntriples::ColumnTypeMeta`]). A type is its
//! own file so a later save can hard-link the ones that did not change into the
//! next generation instead of rewriting them.
//!
//! Used by [`crate::graph::dir_graph::DirGraph::save_disk`], so saved DirGraphs
//! (carves, `save_subset`, mutation persists from a fresh in-memory build) load
//! with mmap-fast-path semantics rather than per-type-sidecar decompression.
//!
//! Layout strategy, per type:
//! 1. Plan: walk every (column, sub-array) once to assign region offsets
//!    within the type's file. A source is *borrowed* from the live store — a
//!    heap vec, a spill mapping or the previous generation's file — and only a
//!    `Str` column carrying a relocation overlay is packed into an owned
//!    buffer, so a save never holds every column's bytes twice.
//! 2. Write the sub-arrays' raw bytes in offset order into a new file
//!    (`create_new`: a published file is never rewritten).
//! 3. Emit `seg_000/columns_meta.json` with the per-type
//!    [`ColumnTypeMeta`] and the file each type lives in.
//!
//! Mixed properties and identity types unsupported by the mmap layout are
//! returned in `unhandled` so the caller falls back to the legacy zstd sidecar
//! for those.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use crate::graph::io::column_link::{self, Carried, PreviousColumns, Reusable};
use crate::graph::io::columns_meta::{self, ColumnsMeta};
use crate::graph::io::ntriples::{
    ColMapEntry, ColumnTypeMeta, FixedColMeta, RegionMeta, StrColMeta,
};
use crate::graph::schema::StringInterner;
use crate::graph::storage::column_store::{ColumnStore, TypedColumn};
use crate::graph::storage::mapped::column_store::{ColRef, MmapColumnStore, Region};
use crate::graph::storage::mapped::mmap_vec::{MmapBytes, MmapOrVec};
use rustc_hash::FxHashMap;

/// Result of a unified-columns write.
#[allow(dead_code)] // fields are part of the public API; consumed by save_disk in the future
pub struct WriteResult {
    /// Types successfully encoded into a `type_columns/` file. The caller
    /// should skip sidecar emission for these.
    pub written: HashSet<String>,
    /// Types with columns unrepresentable in the mmap layout, or with no bytes
    /// to map. Caller uses the legacy zstd sidecar path for these.
    pub unhandled: HashSet<String>,
    /// The subset of `written` whose file came from the previous generation
    /// instead of being written (see [`crate::graph::io::column_link`]).
    pub carried: HashMap<String, Carried>,
}

/// A run of one region's bytes: borrowed or owned data, or `len` copies of
/// one byte, which is how a column one part of a store lacks is written
/// without building it.
enum Part<'a> {
    Bytes(Cow<'a, [u8]>),
    Fill(u8, usize),
}

impl Part<'_> {
    fn len(&self) -> usize {
        match self {
            Part::Bytes(bytes) => bytes.len(),
            Part::Fill(_, len) => *len,
        }
    }
}

/// The bytes of one type's file, planned but not yet written.
struct PlannedType<'a> {
    type_name: String,
    meta: ColumnTypeMeta,
    /// Path relative to `seg_000/`, recorded in the sidecar.
    file: String,
    /// `(offset, bytes)` in ascending, gap-free offset order.
    sources: Vec<(usize, Part<'a>)>,
    /// Bytes the regions cover; a file with none is padded to one byte.
    len: usize,
}

/// Assigns consecutive regions of one type's file to borrowed or owned bytes.
#[derive(Default)]
struct RegionPlanner<'a> {
    cursor: usize,
    sources: Vec<(usize, Part<'a>)>,
}

impl<'a> RegionPlanner<'a> {
    fn push(&mut self, bytes: impl Into<Cow<'a, [u8]>>) -> RegionMeta {
        self.push_parts([Part::Bytes(bytes.into())])
    }

    /// One region made of consecutive parts.
    fn push_parts(&mut self, parts: impl IntoIterator<Item = Part<'a>>) -> RegionMeta {
        let offset = self.cursor;
        for part in parts {
            let start = self.cursor;
            self.cursor += part.len();
            self.sources.push((start, part));
        }
        RegionMeta {
            offset,
            len: self.cursor - offset,
        }
    }

    /// A region that is not present.
    fn absent() -> RegionMeta {
        RegionMeta { offset: 0, len: 0 }
    }

    fn finish(self, type_name: &str, meta: ColumnTypeMeta) -> PlannedType<'a> {
        PlannedType {
            type_name: type_name.to_string(),
            meta,
            file: String::new(),
            sources: self.sources,
            len: self.cursor,
        }
    }
}

/// Write every column store that fits the mmap layout into its own file under
/// `dir/seg_000/type_columns/`, plus `seg_000/columns_meta.json`.
///
/// A type whose store is an unchanged view of a file `previous` holds is carried
/// into the stage instead of written. Returns the set of types that landed in a
/// file (caller skips them during sidecar emission) plus the set that needs
/// sidecar fallback.
pub(crate) fn write_unified_columns<'s>(
    dir: &Path,
    column_stores: &'s HashMap<String, Arc<ColumnStore>>,
    _interner: &StringInterner,
    previous: Option<&PreviousColumns>,
) -> io::Result<WriteResult> {
    let seg0 = dir.join("seg_000");
    fs::create_dir_all(&seg0)?;

    let mut planned: Vec<PlannedType<'s>> = Vec::with_capacity(column_stores.len());
    let mut reused: Vec<(String, Reusable)> = Vec::new();
    let mut unhandled: HashSet<String> = HashSet::new();
    let mut used_files: HashSet<String> = HashSet::new();

    // Stable iteration order for deterministic file names and sidecar order.
    let mut type_names: Vec<&String> = column_stores.keys().collect();
    type_names.sort();

    for type_name in type_names {
        let store = &column_stores[type_name];
        if let Some(reuse) = previous.and_then(|prev| prev.reusable(type_name, store)) {
            // A carried file keeps its name, so it is reserved before any new
            // file is named: two names sharing a key must not swap files.
            used_files.insert(reuse.file.clone());
            reused.push((type_name.clone(), reuse));
            continue;
        }
        match plan_type(type_name, store) {
            Some(plan) => planned.push(plan),
            None => {
                unhandled.insert(type_name.clone());
            }
        }
    }
    for plan in &mut planned {
        plan.file = columns_meta::type_file_name(&plan.type_name, &mut used_files);
    }

    if planned.is_empty() && reused.is_empty() {
        let _ = fs::remove_file(seg0.join("columns_meta.json"));
        return Ok(WriteResult {
            written: HashSet::new(),
            unhandled,
            carried: HashMap::new(),
        });
    }

    let mut carried = HashMap::new();
    for (type_name, reuse) in &reused {
        carried.insert(
            type_name.clone(),
            column_link::carry(&reuse.source, &seg0, &reuse.file)?,
        );
    }
    for plan in &planned {
        write_type_file(&seg0, plan)?;
    }

    let mut entries: Vec<(&str, &ColumnTypeMeta, &str)> = planned
        .iter()
        .map(|plan| (plan.type_name.as_str(), &plan.meta, plan.file.as_str()))
        .chain(
            reused
                .iter()
                .map(|(name, reuse)| (name.as_str(), &reuse.meta, reuse.file.as_str())),
        )
        .collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let meta = ColumnsMeta {
        types: entries.iter().map(|(_, meta, _)| (*meta).clone()).collect(),
        files: entries
            .iter()
            .map(|(name, _, file)| ((*name).to_string(), (*file).to_string()))
            .collect(),
        sidecars: Default::default(),
    };
    columns_meta::publish_json_synced(&seg0, &meta)?;

    let written: HashSet<String> = entries
        .iter()
        .map(|(name, _, _)| (*name).to_string())
        .collect();
    Ok(WriteResult {
        written,
        unhandled,
        carried,
    })
}

/// Plan one type, or `None` when its store needs the sidecar.
fn plan_type<'s>(type_name: &str, store: &'s ColumnStore) -> Option<PlannedType<'s>> {
    // A store that is nothing but an mmap base re-emits its regions verbatim:
    // no flatten onto the heap, no per-row decode.
    if let Some(ms) = store.pure_mmap_store() {
        return Some(plan_mmap_store(type_name, ms));
    }
    // The base's regions with the `SET` cells written over them, then the
    // tail's, in one file.
    if let Some(parts) = store.region_parts() {
        return tail_plan::plan_regions(type_name, parts);
    }
    if store_needs_sidecar(store) {
        return None;
    }
    let mut planner = RegionPlanner::default();
    let identity = plan_identity(store, &mut planner);
    let (col_map, fixed_cols, str_cols) = plan_properties(store, &mut planner);
    let (overflow_offsets, overflow_data, has_overflow) = match store.effective_overflow_bytes() {
        Some((offsets, data)) => (planner.push(offsets), planner.push(data), true),
        None => (RegionPlanner::absent(), RegionPlanner::absent(), false),
    };
    let meta = ColumnTypeMeta {
        type_name: type_name.to_string(),
        row_count: store.row_count(),
        id_is_string: identity.id_is_string,
        id_data: identity.id_data,
        id_nulls: identity.id_nulls,
        id_str_data: identity.id_str_data,
        id_str_offsets: identity.id_str_offsets,
        title_data: identity.title_data,
        title_offsets: identity.title_offsets,
        title_nulls: identity.title_nulls,
        col_map,
        fixed_cols,
        str_cols,
        overflow_offsets,
        overflow_data,
        has_overflow,
    };
    Some(planner.finish(type_name, meta))
}

/// Region layout of a store's id and title columns.
struct IdentityRegions {
    id_is_string: bool,
    id_data: RegionMeta,
    id_nulls: RegionMeta,
    id_str_data: RegionMeta,
    id_str_offsets: RegionMeta,
    title_data: RegionMeta,
    title_offsets: RegionMeta,
    title_nulls: RegionMeta,
}

fn plan_identity<'a>(store: &'a ColumnStore, planner: &mut RegionPlanner<'a>) -> IdentityRegions {
    let absent = RegionPlanner::absent;
    let (id_is_string, id_data, id_nulls, id_str_data, id_str_offsets) = extract_id_column(store);
    let mut regions = IdentityRegions {
        id_is_string,
        id_data: absent(),
        id_nulls: absent(),
        id_str_data: absent(),
        id_str_offsets: absent(),
        title_data: absent(),
        title_offsets: absent(),
        title_nulls: absent(),
    };
    if id_is_string {
        regions.id_str_data = planner.push(id_str_data);
        regions.id_str_offsets = planner.push(id_str_offsets);
        regions.id_nulls = planner.push(id_nulls);
    } else if !id_data.is_empty() {
        regions.id_data = planner.push(id_data);
        regions.id_nulls = planner.push(id_nulls);
    }
    let (title_data, title_offsets, title_nulls) = extract_title_column(store);
    regions.title_data = planner.push(title_data);
    regions.title_offsets = planner.push(title_offsets);
    regions.title_nulls = planner.push(title_nulls);
    regions
}

/// Region layout of a store's per-schema-slot property columns.
fn plan_properties<'a>(
    store: &'a ColumnStore,
    planner: &mut RegionPlanner<'a>,
) -> (Vec<ColMapEntry>, Vec<FixedColMeta>, Vec<StrColMeta>) {
    let mut col_map: Vec<ColMapEntry> = Vec::new();
    let mut fixed_cols: Vec<FixedColMeta> = Vec::new();
    let mut str_cols: Vec<StrColMeta> = Vec::new();

    for (slot, ik) in store.schema().iter() {
        let Some(col) = store.column(slot as usize) else {
            continue;
        };
        if let TypedColumn::Str {
            offsets,
            data,
            nulls,
            relocated,
        } = col
        {
            let (data_bytes, offsets_bytes, nulls_bytes) =
                pack_str_column(offsets, data, nulls, relocated);
            let idx = str_cols.len();
            str_cols.push(StrColMeta {
                data: planner.push(data_bytes),
                offsets: planner.push(offsets_bytes),
                nulls: planner.push(nulls_bytes),
            });
            col_map.push(ColMapEntry {
                key_u64: ik.as_u64(),
                col_type_str: "string".into(),
                idx,
            });
            continue;
        }
        let (tag, data, nulls) = fixed_width_parts(col)
            .expect("Mixed is refused by store_needs_sidecar; Str is handled above");
        let idx = fixed_cols.len();
        fixed_cols.push(FixedColMeta {
            col_type_str: tag.into(),
            data: planner.push(Cow::Borrowed(data)),
            nulls: planner.push(Cow::Borrowed(nulls)),
        });
        col_map.push(ColMapEntry {
            key_u64: ik.as_u64(),
            col_type_str: tag.into(),
            idx,
        });
    }
    (col_map, fixed_cols, str_cols)
}

/// Plan a pure mmap-backed store's regions as borrowed slices of its own
/// mapping, re-based onto the new file.
fn plan_mmap_store<'a>(type_name: &str, ms: &'a MmapColumnStore) -> PlannedType<'a> {
    let mut planner = RegionPlanner::default();
    let mut copy = |region: &Region| -> RegionMeta {
        if region.len == 0 {
            return RegionPlanner::absent();
        }
        planner.push(Cow::Borrowed(
            &ms.mmap[region.offset..region.offset + region.len],
        ))
    };
    let empty = Region::EMPTY;
    let (id_data, id_nulls, id_str_data, id_str_offsets) = if ms.id_is_string {
        let sc = ms.id_str.as_ref();
        let data = copy(sc.map_or(&empty, |c| &c.data));
        let offsets = copy(sc.map_or(&empty, |c| &c.offsets));
        let nulls = copy(sc.map_or(&empty, |c| &c.nulls));
        (RegionPlanner::absent(), nulls, data, offsets)
    } else {
        let fc = ms.id_fixed.as_ref();
        let data = copy(fc.map_or(&empty, |c| &c.data));
        let nulls = copy(fc.map_or(&empty, |c| &c.nulls));
        (
            data,
            nulls,
            RegionPlanner::absent(),
            RegionPlanner::absent(),
        )
    };
    let title_data = copy(&ms.title.data);
    let title_offsets = copy(&ms.title.offsets);
    let title_nulls = copy(&ms.title.nulls);
    let fixed_cols: Vec<FixedColMeta> = ms
        .fixed_cols
        .iter()
        .map(|fc| FixedColMeta {
            col_type_str: fc.col_type.type_tag().into(),
            data: copy(&fc.data),
            nulls: copy(&fc.nulls),
        })
        .collect();
    let str_cols: Vec<StrColMeta> = ms
        .str_cols
        .iter()
        .map(|sc| StrColMeta {
            data: copy(&sc.data),
            offsets: copy(&sc.offsets),
            nulls: copy(&sc.nulls),
        })
        .collect();
    let overflow_offsets = copy(&ms.overflow_offsets);
    let overflow_data = copy(&ms.overflow_data);
    let mut col_map: Vec<ColMapEntry> = ms
        .col_map
        .iter()
        .map(|(key, column)| match column {
            ColRef::Fixed(idx) => ColMapEntry {
                key_u64: key.as_u64(),
                col_type_str: ms.fixed_cols[*idx].col_type.type_tag().into(),
                idx: *idx,
            },
            ColRef::Str(idx) => ColMapEntry {
                key_u64: key.as_u64(),
                col_type_str: "string".into(),
                idx: *idx,
            },
        })
        .collect();
    col_map.sort_by_key(|entry| entry.key_u64);
    let meta = ColumnTypeMeta {
        type_name: type_name.to_string(),
        row_count: ms.row_count,
        id_is_string: ms.id_is_string,
        id_data,
        id_nulls,
        id_str_data,
        id_str_offsets,
        title_data,
        title_offsets,
        title_nulls,
        col_map,
        fixed_cols,
        str_cols,
        overflow_offsets,
        overflow_data,
        has_overflow: ms.has_overflow,
    };
    planner.finish(type_name, meta)
}

/// Write `plan`'s bytes into a new file under `seg0`.
///
/// `create_new`: a file of a generation is written once and never reopened for
/// writing, so an existing one is an error rather than something to truncate.
fn write_type_file(seg0: &Path, plan: &PlannedType<'_>) -> io::Result<()> {
    let path = columns_meta::resolve_type_file(seg0, &plan.file)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut out = BufWriter::with_capacity(1 << 20, file);
    let mut position = 0usize;
    for (offset, part) in &plan.sources {
        debug_assert_eq!(*offset, position, "regions are planned gap-free");
        match part {
            Part::Bytes(bytes) => out.write_all(bytes)?,
            Part::Fill(byte, len) => {
                let block = [*byte; 1 << 16];
                let mut left = *len;
                while left > 0 {
                    let step = left.min(block.len());
                    out.write_all(&block[..step])?;
                    left -= step;
                }
            }
        }
        position += part.len();
    }
    debug_assert_eq!(position, plan.len);
    if position == 0 {
        // A type with no rows names no bytes, and a zero-length file cannot be
        // mapped: one byte no region refers to gives it a mapping all the same.
        out.write_all(&[0])?;
    }
    out.flush()
}

/// Pack a `Str` column into the file's `(data, offsets, nulls)` byte triple.
///
/// Two conventions are reconciled here.
///
/// *Offsets.* The file stores `row_count` cumulative **end** offsets — row
/// 0 starts at byte 0, row `i` starts at `offsets[i - 1]` (see
/// [`crate::graph::storage::mapped::column_store`]). An in-memory
/// `TypedColumn::Str` instead carries `row_count + 1` offsets with a leading
/// zero, which `str_at` reads as `offsets[i]..offsets[i + 1]`, while the
/// streaming carve's `TypeWriter` already emits the file form. Both are
/// accepted; the leading zero is stripped.
///
/// *The write overlay.* `TypedColumn::set` cannot shift `offsets` for a
/// replacement of a different length — that would move row `i + 1`'s start —
/// so it parks the new string in `relocated` and leaves `offsets`/`data`
/// holding the pre-`SET` bytes. The raw buffers are therefore **stale** for
/// every overlaid row. Reading them straight through, as this writer did,
/// silently dropped every differing-length string `SET` on a disk-mode graph:
/// the value read back after a reload as its pre-`SET` string, or as `""` when
/// the `SET` itself was what created the column.
/// [`TypedColumn::write_to`](crate::graph::storage::column_store::TypedColumn)
/// folds the overlay back for the packed sidecars; this is that fold in the
/// file's layout. Without an overlay every part is borrowed, not copied.
type PackedStr<'a> = (Cow<'a, [u8]>, Cow<'a, [u8]>, Cow<'a, [u8]>);

fn pack_str_column<'a>(
    offsets: &'a MmapOrVec<u64>,
    data: &'a MmapBytes,
    nulls: &'a MmapOrVec<u8>,
    relocated: &FxHashMap<u32, String>,
) -> PackedStr<'a> {
    let row_count = nulls.len();
    let nulls_bytes = Cow::Borrowed(nulls.as_raw_bytes());
    // `row_count + 1` offsets means the leading-zero form.
    let leading_zero = offsets.len() == row_count + 1;

    if relocated.is_empty() {
        let off_bytes = offsets.as_raw_bytes();
        let off_slice = if leading_zero {
            &off_bytes[8..]
        } else {
            off_bytes
        };
        return (
            Cow::Borrowed(data.as_raw_bytes()),
            Cow::Borrowed(off_slice),
            nulls_bytes,
        );
    }

    let offsets = offsets.as_slice();
    let source = data.as_raw_bytes();
    let mut new_data: Vec<u8> = Vec::with_capacity(source.len());
    let mut new_offsets: Vec<u8> = Vec::with_capacity(row_count * 8);
    for row in 0..row_count {
        // A null row contributes no bytes but still advances an offset, so a
        // reader's `offsets[i - 1] == offsets[i]` empty range lines up with the
        // null flag. Same rule as `TypedColumn::write_to`.
        if nulls.get(row) == 0 {
            match relocated.get(&(row as u32)) {
                Some(s) => new_data.extend_from_slice(s.as_bytes()),
                None => {
                    // Bounds-checked throughout, like `str_at`: a malformed
                    // offsets array yields an empty row rather than a panic.
                    let range = if leading_zero {
                        offsets.get(row).copied().zip(offsets.get(row + 1).copied())
                    } else if row == 0 {
                        offsets.first().copied().map(|end| (0, end))
                    } else {
                        offsets.get(row - 1).copied().zip(offsets.get(row).copied())
                    };
                    if let Some(bytes) =
                        range.and_then(|(start, end)| source.get(start as usize..end as usize))
                    {
                        new_data.extend_from_slice(bytes);
                    }
                }
            }
        }
        new_offsets.extend_from_slice(&(new_data.len() as u64).to_le_bytes());
    }
    (Cow::Owned(new_data), Cow::Owned(new_offsets), nulls_bytes)
}

/// The type tag and raw `(data, nulls)` bytes of a fixed-width column; `None`
/// for `Str` and `Mixed`.
fn fixed_width_parts(col: &TypedColumn) -> Option<(&'static str, &[u8], &[u8])> {
    Some(match col {
        TypedColumn::Int64 { data, nulls } => ("int64", data.as_raw_bytes(), nulls.as_raw_bytes()),
        TypedColumn::Float64 { data, nulls } => {
            ("float64", data.as_raw_bytes(), nulls.as_raw_bytes())
        }
        TypedColumn::UniqueId { data, nulls } => {
            ("uniqueid", data.as_raw_bytes(), nulls.as_raw_bytes())
        }
        TypedColumn::Bool { data, nulls } => ("bool", data.as_raw_bytes(), nulls.as_raw_bytes()),
        TypedColumn::Date { data, nulls } => ("date", data.as_raw_bytes(), nulls.as_raw_bytes()),
        TypedColumn::Timestamp { data, nulls } => {
            ("timestamp", data.as_raw_bytes(), nulls.as_raw_bytes())
        }
        TypedColumn::Str { .. } | TypedColumn::Mixed { .. } => return None,
    })
}

fn store_needs_sidecar(store: &ColumnStore) -> bool {
    if store.has_mmap_base() {
        return true;
    }
    if store
        .columns_ref()
        .any(|c| matches!(c, TypedColumn::Mixed { .. }))
    {
        return true;
    }
    if let Some(c) = store.id_column_ref() {
        if !matches!(
            c,
            TypedColumn::Str { .. } | TypedColumn::UniqueId { .. } | TypedColumn::Int64 { .. }
        ) {
            return true;
        }
    }
    if let Some(c) = store.title_column_ref() {
        if !matches!(c, TypedColumn::Str { .. } | TypedColumn::Int64 { .. }) {
            return true;
        }
    }
    false
}

type IdRegionBytes<'a> = (
    bool,
    Cow<'a, [u8]>,
    Cow<'a, [u8]>,
    Cow<'a, [u8]>,
    Cow<'a, [u8]>,
);

/// Extract the id column's raw bytes per the layout expected by the
/// loader. Returns `(id_is_string, fixed_data_bytes, nulls_bytes,
/// str_data_bytes, str_offsets_bytes)`. Empty slices are used for the
/// unused branch (fixed vs string).
fn extract_id_column(store: &ColumnStore) -> IdRegionBytes<'_> {
    let empty = || Cow::Borrowed(&[][..]);
    match store.id_column_ref() {
        Some(TypedColumn::Str {
            offsets,
            data,
            nulls,
            relocated,
        }) => {
            let (data_bytes, offsets_bytes, nulls_bytes) =
                pack_str_column(offsets, data, nulls, relocated);
            (true, empty(), nulls_bytes, data_bytes, offsets_bytes)
        }
        Some(TypedColumn::UniqueId { data, nulls }) => (
            false,
            Cow::Borrowed(data.as_raw_bytes()),
            Cow::Borrowed(nulls.as_raw_bytes()),
            empty(),
            empty(),
        ),
        Some(TypedColumn::Int64 { data, nulls }) => (
            false,
            Cow::Borrowed(data.as_raw_bytes()),
            Cow::Borrowed(nulls.as_raw_bytes()),
            empty(),
            empty(),
        ),
        _ => (false, empty(), empty(), empty(), empty()),
    }
}

/// Extract the title column's raw bytes. Returns `(data_bytes,
/// offsets_bytes, nulls_bytes)`; an `Int64` title is its i64 data with an
/// empty offsets region (see `MmapColumnStore::title_is_int`). Empty if no
/// title.
fn extract_title_column(store: &ColumnStore) -> PackedStr<'_> {
    let empty = || Cow::Borrowed(&[][..]);
    match store.title_column_ref() {
        Some(TypedColumn::Str {
            offsets,
            data,
            nulls,
            relocated,
        }) => pack_str_column(offsets, data, nulls, relocated),
        Some(TypedColumn::Int64 { data, nulls }) if !data.is_empty() => (
            Cow::Borrowed(data.as_raw_bytes()),
            empty(),
            Cow::Borrowed(nulls.as_raw_bytes()),
        ),
        _ => (empty(), empty(), empty()),
    }
}

#[path = "unified_columns_tail.rs"]
mod tail_plan;

#[cfg(test)]
#[path = "unified_columns_identity_tests.rs"]
mod identity_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datatypes::values::Value;

    /// Decode a packed `Str` column the way the mega-file's reader does:
    /// `offsets[row]` is the cumulative *end*, row 0 starts at 0, and row `i`
    /// starts at `offsets[i - 1]`.
    fn decode(data: &[u8], offsets: &[u8], nulls: &[u8]) -> Vec<Option<String>> {
        let ends: Vec<u64> = offsets
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect();
        assert_eq!(ends.len(), nulls.len(), "one end offset per row");
        let mut out = Vec::with_capacity(nulls.len());
        for (row, &is_null) in nulls.iter().enumerate() {
            if is_null != 0 {
                out.push(None);
                continue;
            }
            let start = if row == 0 { 0 } else { ends[row - 1] } as usize;
            let end = ends[row] as usize;
            out.push(Some(String::from_utf8(data[start..end].to_vec()).unwrap()));
        }
        out
    }

    /// Build the in-memory `Str` shape: `row_count + 1` offsets, leading zero.
    fn build(values: &[Option<&str>]) -> TypedColumn {
        let mut col = TypedColumn::from_type_str("string");
        for v in values {
            match v {
                Some(s) => col.push(&Value::String((*s).to_string())).unwrap(),
                None => col.push_null(),
            }
        }
        col
    }

    fn pack(col: &TypedColumn) -> Vec<Option<String>> {
        let TypedColumn::Str {
            offsets,
            data,
            nulls,
            relocated,
        } = col
        else {
            panic!("expected a Str column");
        };
        let (data_bytes, offsets_bytes, nulls_bytes) =
            pack_str_column(offsets, data, nulls, relocated);
        decode(&data_bytes, &offsets_bytes, &nulls_bytes)
    }

    #[test]
    fn packs_a_column_with_no_overlay() {
        let col = build(&[Some("a"), None, Some("ccc")]);
        assert_eq!(
            pack(&col),
            vec![Some("a".into()), None, Some("ccc".into())],
            "the leading zero must be stripped, not emitted as row 0's end"
        );
    }

    #[test]
    fn folds_a_differing_length_overwrite_back_into_the_layout() {
        // The regression this file's `pack_str_column` exists for: `set` parks
        // a differing-length value in `relocated` and leaves `offsets`/`data`
        // holding the pre-`SET` bytes, so a writer reading the raw buffers
        // emitted the stale string and lost the write.
        let mut col = build(&[Some("aa"), Some("bb"), Some("cc")]);
        col.set(1, &Value::String("a-much-longer-value".into()))
            .unwrap();
        assert_eq!(
            pack(&col),
            vec![
                Some("aa".into()),
                Some("a-much-longer-value".into()),
                Some("cc".into()),
            ],
            "the overlaid row must carry the new value and its neighbours the old ones"
        );
    }

    #[test]
    fn folds_a_shorter_overwrite_and_renumbers_the_tail() {
        let mut col = build(&[Some("aaaa"), Some("bbbb"), Some("cccc")]);
        col.set(0, &Value::String("z".into())).unwrap();
        assert_eq!(
            pack(&col),
            vec![Some("z".into()), Some("bbbb".into()), Some("cccc".into())],
            "shrinking row 0 must shift every later row's offsets down"
        );
    }

    #[test]
    fn folds_an_overlay_onto_a_column_whose_rows_are_all_null() {
        // The shape a brand-new key's column has: `ColumnStore::set` appends a
        // column of nulls and then writes one row. A dropped overlay left the
        // whole column empty, which read back as "" rather than null.
        let mut col = build(&[None, None, None]);
        col.set(2, &Value::String("xyz".into())).unwrap();
        assert_eq!(pack(&col), vec![None, None, Some("xyz".into())]);
    }

    #[test]
    fn a_null_row_advances_an_offset_without_contributing_bytes() {
        let mut col = build(&[Some("aa"), None, Some("cc")]);
        col.set(0, &Value::String("longer".into())).unwrap();
        assert_eq!(
            pack(&col),
            vec![Some("longer".into()), None, Some("cc".into())]
        );
    }

    #[test]
    fn folds_an_overlay_onto_the_cumulative_ends_offset_form() {
        // The streaming carve's `TypeWriter` emits `row_count` cumulative ends
        // with no leading zero. Both forms reach this writer, so the fold has
        // to read either one.
        let mut col = TypedColumn::Str {
            offsets: MmapOrVec::from_vec(vec![2u64, 4, 6]),
            data: {
                let mut d = MmapBytes::new();
                d.extend(b"aabbcc").unwrap();
                d
            },
            nulls: MmapOrVec::from_vec(vec![0u8, 0, 0]),
            relocated: FxHashMap::default(),
        };
        if let TypedColumn::Str { relocated, .. } = &mut col {
            relocated.insert(1, "BB-longer".to_string());
        }
        assert_eq!(
            pack(&col),
            vec![
                Some("aa".into()),
                Some("BB-longer".into()),
                Some("cc".into()),
            ]
        );
    }
}
