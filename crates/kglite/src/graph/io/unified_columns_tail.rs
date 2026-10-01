//! Plan a type's column file from an mmap base, the `SET` values layered over
//! it and a tail store: each region is the base's bytes with the overlay's cells
//! written over them, followed by the tail's, so a save writes every column the
//! `SET` statements did not touch straight from its mapping and builds only the
//! touched columns and the tail's rows.
//!
//! Precondition (`ColumnStore::region_parts`): the base part is nothing but its
//! mapping and same-kind overlay columns, and every column the tail carries has
//! the base's kind for that key. A column one part lacks is written as nulls for
//! that part's rows.

use std::borrow::Cow;

use super::{
    fixed_width_parts, pack_str_column, IdentityRegions, Part, PlannedType, PropertyRegions,
    RegionPlanner,
};
use crate::graph::io::ntriples::{ColMapEntry, FixedColMeta, StrColMeta};
use crate::graph::schema::InternedKey;
use crate::graph::storage::column_store::RegionParts;
use crate::graph::storage::column_store::{ColumnStore, TypedColumn};
use crate::graph::storage::mapped::column_store::{ColRef, MmapColumnStore, Region, StrColumnMeta};
use crate::graph::storage::type_build_meta::ColType;

/// The base's bytes of a region, `None` when the file carries none.
fn base_bytes<'a>(ms: &'a MmapColumnStore, region: &Region) -> Option<&'a [u8]> {
    (region.len != 0).then(|| &ms.mmap[region.offset..region.offset + region.len])
}

/// `bytes` extended (or cut) to exactly `len`, padding with `pad`.
fn sized<'a>(bytes: Option<Cow<'a, [u8]>>, len: usize, pad: u8) -> Vec<Part<'a>> {
    match bytes {
        Some(bytes) if bytes.len() == len => vec![Part::Bytes(bytes)],
        Some(bytes) if bytes.len() < len => {
            let short = len - bytes.len();
            vec![Part::Bytes(bytes), Part::Fill(pad, short)]
        }
        Some(bytes) => vec![Part::Bytes(match bytes {
            Cow::Borrowed(slice) => Cow::Borrowed(&slice[..len]),
            Cow::Owned(mut owned) => {
                owned.truncate(len);
                Cow::Owned(owned)
            }
        })],
        None => vec![Part::Fill(pad, len)],
    }
}

/// Null flags for `rows` rows: the bytes, or all-null when there are none.
fn null_parts<'a>(bytes: Option<Cow<'a, [u8]>>, rows: usize) -> Vec<Part<'a>> {
    sized(bytes, rows, 1)
}

/// The fixed-width column of one part: `(data, nulls)` bytes for `rows` rows.
fn fixed_part<'a>(
    data: Option<Cow<'a, [u8]>>,
    nulls: Option<Cow<'a, [u8]>>,
    rows: usize,
    width: usize,
) -> (Vec<Part<'a>>, Vec<Part<'a>>) {
    (sized(data, rows * width, 0), null_parts(nulls, rows))
}

/// Bytes per value of a fixed-width column tagged `tag`.
fn tag_width(tag: &str) -> Option<usize> {
    ColType::from_type_tag(tag)?.value_size()
}

/// A string column's `(data, offsets, nulls)` parts over `base_rows` then
/// `tail_rows` rows. The tail's end offsets are shifted by the base's data
/// length so they index the concatenated data.
struct StrParts<'a> {
    data: Vec<Part<'a>>,
    offsets: Vec<Part<'a>>,
    nulls: Vec<Part<'a>>,
}

/// A base string column's `(data, offsets, nulls)` regions, each possibly absent.
type BaseRegions<'a> = (Option<&'a [u8]>, Option<&'a [u8]>, Option<&'a [u8]>);
/// The same, as bytes a part list can own or borrow.
type BaseStr<'a> = (
    Option<Cow<'a, [u8]>>,
    Option<Cow<'a, [u8]>>,
    Option<Cow<'a, [u8]>>,
);
/// The tail's string column packed as `(data, offsets, nulls)` bytes.
type TailStr<'a> = (Cow<'a, [u8]>, Cow<'a, [u8]>, Cow<'a, [u8]>);

fn str_parts<'a>(
    base: Option<BaseStr<'a>>,
    tail: Option<TailStr<'a>>,
    base_rows: usize,
    tail_rows: usize,
) -> Option<StrParts<'a>> {
    let (base_data, base_offsets, base_nulls) = base.unwrap_or((None, None, None));
    let base_data_len = base_data.as_ref().map_or(0, |bytes| bytes.len());
    // An absent offsets region stands for `base_rows` zero ends, which only
    // describes a base with no string bytes.
    if base_offsets.is_none() && base_data_len != 0 {
        return None;
    }
    let mut data = Vec::new();
    let mut offsets = Vec::new();
    if let Some(bytes) = base_data {
        data.push(Part::Bytes(bytes));
    }
    offsets.extend(sized(base_offsets, base_rows * 8, 0));
    let mut nulls = null_parts(base_nulls, base_rows);

    let (tail_data, tail_offsets, tail_nulls) = match tail {
        Some((d, o, n)) => (Some(d), Some(o), Some(n)),
        None => (None, None, None),
    };
    if let Some(bytes) = tail_data {
        data.push(Part::Bytes(bytes));
    }
    let ends: Vec<u64> = tail_offsets
        .as_deref()
        .map(|bytes| {
            bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect()
        })
        .unwrap_or_default();
    let last = ends.last().copied().unwrap_or(0);
    let mut shifted = Vec::with_capacity(tail_rows * 8);
    for row in 0..tail_rows {
        let end = ends.get(row).copied().unwrap_or(last);
        shifted.extend_from_slice(&(end + base_data_len as u64).to_le_bytes());
    }
    offsets.push(Part::Bytes(Cow::Owned(shifted)));
    nulls.extend(null_parts(tail_nulls, tail_rows));
    Some(StrParts {
        data,
        offsets,
        nulls,
    })
}

/// A base fixed-width column's `(data, nulls)` bytes, each possibly absent.
type FixedBase<'a> = (Option<Cow<'a, [u8]>>, Option<Cow<'a, [u8]>>);

/// A fixed-width column's base parts followed by its tail's: `(data, nulls)`.
/// A tail with no column of its own is nulls.
fn concat_fixed<'a>(
    base: FixedBase<'a>,
    tail: Option<(&'a [u8], &'a [u8])>,
    base_rows: usize,
    tail_rows: usize,
    width: usize,
) -> (Vec<Part<'a>>, Vec<Part<'a>>) {
    let (mut data, mut nulls) = fixed_part(base.0, base.1, base_rows, width);
    let (tail_data, tail_nulls) = fixed_part(
        tail.map(|(bytes, _)| Cow::Borrowed(bytes)),
        tail.map(|(_, bytes)| Cow::Borrowed(bytes)),
        tail_rows,
        width,
    );
    data.extend(tail_data);
    nulls.extend(tail_nulls);
    (data, nulls)
}

/// The tail's string column packed as file bytes; `None` for no column or one
/// of another kind.
fn tail_str(column: Option<&TypedColumn>) -> Option<TailStr<'_>> {
    match column {
        Some(TypedColumn::Str {
            offsets,
            data,
            nulls,
            relocated,
        }) => Some(pack_str_column(offsets, data, nulls, relocated)),
        _ => None,
    }
}

/// A base string column's regions as bytes a part list can borrow.
fn borrowed_str<'a>(ms: &'a MmapColumnStore, column: &StrColumnMeta) -> BaseStr<'a> {
    (
        base_bytes(ms, &column.data).map(Cow::Borrowed),
        base_bytes(ms, &column.offsets).map(Cow::Borrowed),
        base_bytes(ms, &column.nulls).map(Cow::Borrowed),
    )
}

pub(super) fn plan_regions<'a>(type_name: &str, parts: RegionParts<'a>) -> Option<PlannedType<'a>> {
    let RegionParts {
        base: ms,
        overlay,
        tail,
    } = parts;
    let base_rows = ms.row_count as usize;
    let tail_rows = tail.map_or(0, |tail| tail.row_count() as usize);
    let mut planner = RegionPlanner::default();
    let identity = plan_identity(&mut planner, ms, tail, base_rows, tail_rows)?;

    // Every key any part holds, in key order.
    let mut keys: Vec<InternedKey> = ms.col_map.keys().copied().collect();
    let mut known: std::collections::HashSet<InternedKey> = keys.iter().copied().collect();
    for store in [Some(overlay), tail].into_iter().flatten() {
        for (slot, key) in store.schema().iter() {
            if store.column(slot as usize).is_some() && known.insert(key) {
                keys.push(key);
            }
        }
    }
    keys.sort_by_key(|key| key.as_u64());
    let mut columns = PropertyRegions::default();
    for key in keys {
        let base_ref = ms.col_map.get(&key).copied();
        // What the base part's column of this key holds over its own rows.
        let overlay_column = overlay.column_for_plan(key, ms.column_kind(key));
        let kind = ms
            .column_kind(key)
            .or_else(|| overlay_column.map(TypedColumn::type_tag));
        let tail_column = tail.and_then(|tail| tail.column_for_plan(key, kind));
        let is_string = match (base_ref, overlay_column, tail_column) {
            (Some(ColRef::Str(_)), ..) => true,
            (Some(ColRef::Fixed(_)), ..) => false,
            (None, Some(column), _) | (None, None, Some(column)) => {
                matches!(column, TypedColumn::Str { .. })
            }
            (None, None, None) => false,
        };
        let rows = (base_rows, tail_rows);
        let column = KeyColumn {
            ms,
            base_ref,
            overlay: overlay_column,
            tail: tail_column,
            rows,
        };
        if is_string {
            let meta = plan_string_key(&mut planner, column)?;
            columns.col_map.push(ColMapEntry {
                key_u64: key.as_u64(),
                col_type_str: "string".into(),
                idx: columns.str_cols.len(),
            });
            columns.str_cols.push(meta);
        } else {
            let (tag, meta) = plan_fixed_key(&mut planner, column)?;
            columns.col_map.push(ColMapEntry {
                key_u64: key.as_u64(),
                col_type_str: tag.into(),
                idx: columns.fixed_cols.len(),
            });
            columns.fixed_cols.push(meta);
        }
    }
    let meta = identity.into_meta(type_name, (base_rows + tail_rows) as u32, columns, None);
    Some(planner.finish(meta))
}

/// What the three parts of one store hold for one property key.
struct KeyColumn<'a> {
    ms: &'a MmapColumnStore,
    base_ref: Option<ColRef>,
    /// The `SET` values over the base's rows, as the plan reads them.
    overlay: Option<&'a TypedColumn>,
    tail: Option<&'a TypedColumn>,
    /// Base rows, then tail rows.
    rows: (usize, usize),
}

/// A string key's regions: the base's bytes with the overlay over them, then
/// the tail's. `None` when the base's offsets cannot describe its rows.
fn plan_string_key<'a>(planner: &mut RegionPlanner<'a>, key: KeyColumn<'a>) -> Option<StrColMeta> {
    let KeyColumn {
        ms,
        base_ref,
        overlay,
        tail,
        rows: (base_rows, tail_rows),
    } = key;
    let base = match base_ref {
        Some(ColRef::Str(i)) => Some(&ms.str_cols[i]),
        _ => None,
    };
    let base = match overlay {
        Some(column) => {
            let regions = base.map(|column| {
                (
                    base_bytes(ms, &column.data),
                    base_bytes(ms, &column.offsets),
                    base_bytes(ms, &column.nulls),
                )
            });
            Some(merged_str(regions, column, base_rows))
        }
        None => base.map(|column| borrowed_str(ms, column)),
    };
    let parts = str_parts(base, tail_str(tail), base_rows, tail_rows)?;
    Some(StrColMeta {
        data: planner.push_parts(parts.data),
        offsets: planner.push_parts(parts.offsets),
        nulls: planner.push_parts(parts.nulls),
    })
}

/// A fixed-width key's regions, as for [`plan_string_key`]; also its type tag.
fn plan_fixed_key<'a>(
    planner: &mut RegionPlanner<'a>,
    key: KeyColumn<'a>,
) -> Option<(&'static str, FixedColMeta)> {
    let KeyColumn {
        ms,
        base_ref,
        overlay,
        tail,
        rows: (base_rows, tail_rows),
    } = key;
    let tail_parts = tail.and_then(fixed_width_parts);
    let (tag, width, base_data, base_nulls) = match (base_ref, overlay) {
        (Some(ColRef::Fixed(i)), _) => {
            let column = &ms.fixed_cols[i];
            (
                column.col_type.type_tag(),
                column.col_type.value_size()?,
                base_bytes(ms, &column.data),
                base_bytes(ms, &column.nulls),
            )
        }
        (_, Some(column)) => {
            let (tag, ..) = fixed_width_parts(column)?;
            (tag, tag_width(tag)?, None, None)
        }
        _ => {
            let (tag, ..) = tail_parts?;
            (tag, tag_width(tag)?, None, None)
        }
    };
    let base = match overlay {
        Some(column) => {
            let (data, nulls) = merged_fixed(base_data, base_nulls, column, base_rows, width)?;
            (Some(Cow::Owned(data)), Some(Cow::Owned(nulls)))
        }
        None => (base_data.map(Cow::Borrowed), base_nulls.map(Cow::Borrowed)),
    };
    let (data, nulls) = concat_fixed(
        base,
        tail_parts.map(|(_, data, nulls)| (data, nulls)),
        base_rows,
        tail_rows,
        width,
    );
    let meta = FixedColMeta {
        col_type_str: tag.into(),
        data: planner.push_parts(data),
        nulls: planner.push_parts(nulls),
    };
    Some((tag, meta))
}

/// A fixed-width base column with every cell the overlay holds written over
/// it: `(data, nulls)` for `rows` rows. A cell is the overlay's where the
/// overlay column is non-null there, the base's otherwise; a column the base
/// lacks starts all null.
fn merged_fixed(
    base_data: Option<&[u8]>,
    base_nulls: Option<&[u8]>,
    overlay: &TypedColumn,
    rows: usize,
    width: usize,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let (_, overlay_data, overlay_nulls) = fixed_width_parts(overlay)?;
    let mut data = vec![0u8; rows * width];
    if let Some(bytes) = base_data {
        let len = bytes.len().min(data.len());
        data[..len].copy_from_slice(&bytes[..len]);
    }
    let mut nulls = vec![1u8; rows];
    if let Some(bytes) = base_nulls {
        let len = bytes.len().min(rows);
        nulls[..len].copy_from_slice(&bytes[..len]);
    }
    for (row, flag) in overlay_nulls.iter().enumerate().take(rows) {
        let at = row * width;
        if *flag == 0 && overlay_data.len() >= at + width {
            data[at..at + width].copy_from_slice(&overlay_data[at..at + width]);
            nulls[row] = 0;
        }
    }
    Some((data, nulls))
}

/// A string base column with every cell the overlay holds written over it:
/// `(data, offsets, nulls)` in the file's end-offset form for `rows` rows. The
/// whole column is rebuilt (an overlaid string may differ in length from the
/// one it replaces), which costs that column's bytes and nothing else.
///
/// Rows the overlay leaves alone are copied a run at a time (one copy of the
/// base's bytes, its end offsets shifted by what the replacements before them
/// added), so a statement that changed a few rows costs a few row-sized
/// operations plus a pass over the offsets, not a per-row decode. A base whose
/// offsets are not what the file layout guarantees takes [`merged_str_rows`].
fn merged_str<'a>(
    base: Option<BaseRegions<'a>>,
    overlay: &TypedColumn,
    rows: usize,
) -> BaseStr<'a> {
    let (base_data, base_offsets, base_nulls) = base.unwrap_or((None, None, None));
    let data_len = base_data.map_or(0, <[u8]>::len);
    let ends: &[u8] = base_offsets.unwrap_or(&[]);
    let well_formed = base_offsets.is_none_or(|bytes| bytes.len() == rows * 8)
        && base_nulls.is_none_or(|bytes| bytes.len() == rows)
        && (base_offsets.is_some() || data_len == 0)
        && ends
            .as_chunks::<8>()
            .0
            .iter()
            .try_fold(0u64, |last, chunk| {
                let end = u64::from_le_bytes(*chunk);
                (end >= last && end as usize <= data_len).then_some(end)
            })
            .is_some();
    if !well_formed {
        return merged_str_rows(base_data, base_offsets, base_nulls, overlay, rows);
    }
    let end_of = |row: usize| -> u64 {
        ends.get(row * 8..row * 8 + 8).map_or(0, |bytes| {
            u64::from_le_bytes(bytes.try_into().expect("8 bytes"))
        })
    };
    let mut data: Vec<u8> = Vec::with_capacity(data_len);
    let mut offsets: Vec<u8> = Vec::with_capacity(rows * 8);
    let mut nulls = vec![1u8; rows];
    if let Some(bytes) = base_nulls {
        nulls.copy_from_slice(bytes);
    }
    let mut row = 0;
    while row < rows {
        let overlaid = (row..rows).find(|&r| overlay.is_present(r as u32));
        let run_end = overlaid.unwrap_or(rows);
        if run_end > row {
            // Rows `row..run_end` are the base's: its bytes in one copy, its end
            // offsets moved by the difference between where the run now starts
            // in `data` and where it started in the base.
            let from = if row == 0 { 0 } else { end_of(row - 1) } as usize;
            let to = end_of(run_end - 1) as usize;
            if let Some(bytes) = base_data {
                data.extend_from_slice(&bytes[from..to]);
            }
            let start_now = data.len() - (to - from);
            for r in row..run_end {
                let moved = end_of(r) as usize - from + start_now;
                offsets.extend_from_slice(&(moved as u64).to_le_bytes());
            }
        }
        if let Some(at) = overlaid {
            match overlay.get_str(at as u32) {
                Some(value) => {
                    data.extend_from_slice(value.as_bytes());
                    nulls[at] = 0;
                }
                None => nulls[at] = 1,
            }
            offsets.extend_from_slice(&(data.len() as u64).to_le_bytes());
        }
        row = overlaid.map_or(rows, |at| at + 1);
    }
    (
        Some(Cow::Owned(data)),
        Some(Cow::Owned(offsets)),
        Some(Cow::Owned(nulls)),
    )
}

/// [`merged_str`] one row at a time, bounds-checked throughout: a malformed
/// base reads as empty rows rather than a panic, as every string read does.
fn merged_str_rows<'a>(
    base_data: Option<&'a [u8]>,
    base_offsets: Option<&'a [u8]>,
    base_nulls: Option<&'a [u8]>,
    overlay: &TypedColumn,
    rows: usize,
) -> BaseStr<'a> {
    let end_of = |row: usize| -> Option<usize> {
        let bytes = base_offsets?.get(row * 8..row * 8 + 8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?) as usize)
    };
    let base_value = |row: usize| -> Option<&'a [u8]> {
        if base_nulls?.get(row).copied() != Some(0) {
            return None;
        }
        let start = if row == 0 { 0 } else { end_of(row - 1)? };
        base_data?.get(start..end_of(row)?)
    };
    let mut data: Vec<u8> = Vec::new();
    let mut offsets: Vec<u8> = Vec::with_capacity(rows * 8);
    let mut nulls = vec![1u8; rows];
    for (row, null) in nulls.iter_mut().enumerate() {
        let value = if overlay.is_present(row as u32) {
            overlay.get_str(row as u32).map(str::as_bytes)
        } else {
            base_value(row)
        };
        if let Some(bytes) = value {
            data.extend_from_slice(bytes);
            *null = 0;
        }
        offsets.extend_from_slice(&(data.len() as u64).to_le_bytes());
    }
    (
        Some(Cow::Owned(data)),
        Some(Cow::Owned(offsets)),
        Some(Cow::Owned(nulls)),
    )
}

/// Regions of the id and title columns: the base's bytes, then the tail's.
/// The tail has an identity column only where the base has one of the same
/// kind (`ColumnStore::region_parts`).
fn plan_identity<'a>(
    planner: &mut RegionPlanner<'a>,
    ms: &'a MmapColumnStore,
    tail: Option<&'a ColumnStore>,
    base_rows: usize,
    tail_rows: usize,
) -> Option<IdentityRegions> {
    let tail_id = tail.and_then(ColumnStore::id_column_ref);
    let tail_title = tail.and_then(ColumnStore::title_column_ref);
    let mut identity = IdentityRegions::absent(ms.id_is_string);
    let fixed_tail = |column: Option<&'a TypedColumn>| {
        column
            .and_then(fixed_width_parts)
            .map(|(_, data, nulls)| (data, nulls))
    };
    let base_fixed = |data: &Region, nulls: &Region| {
        (
            base_bytes(ms, data).map(Cow::Borrowed),
            base_bytes(ms, nulls).map(Cow::Borrowed),
        )
    };
    if ms.has_id_column() {
        if ms.id_is_string {
            let column = ms.id_str.as_ref()?;
            let parts = str_parts(
                Some(borrowed_str(ms, column)),
                tail_str(tail_id),
                base_rows,
                tail_rows,
            )?;
            identity.id_str_data = planner.push_parts(parts.data);
            identity.id_str_offsets = planner.push_parts(parts.offsets);
            identity.id_nulls = planner.push_parts(parts.nulls);
        } else {
            let column = ms.id_fixed.as_ref()?;
            let (data, nulls) = concat_fixed(
                base_fixed(&column.data, &column.nulls),
                fixed_tail(tail_id),
                base_rows,
                tail_rows,
                column.col_type.value_size()?,
            );
            identity.id_data = planner.push_parts(data);
            identity.id_nulls = planner.push_parts(nulls);
        }
    }
    if ms.has_title_column() {
        if ms.title_is_int() {
            let (data, nulls) = concat_fixed(
                base_fixed(&ms.title.data, &ms.title.nulls),
                fixed_tail(tail_title),
                base_rows,
                tail_rows,
                8,
            );
            identity.title_data = planner.push_parts(data);
            identity.title_nulls = planner.push_parts(nulls);
        } else {
            let parts = str_parts(
                Some(borrowed_str(ms, &ms.title)),
                tail_str(tail_title),
                base_rows,
                tail_rows,
            )?;
            identity.title_data = planner.push_parts(parts.data);
            identity.title_offsets = planner.push_parts(parts.offsets);
            identity.title_nulls = planner.push_parts(parts.nulls);
        }
    }
    Some(identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datatypes::values::Value;

    /// A base string column in the file's layout: `row_count` cumulative end
    /// offsets, null flags (0 = present) and the concatenated bytes. Rows with
    /// `i % 7 == 0` are null; the rest are `"base-<i>"` padded to varying length.
    fn base_column(rows: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (mut data, mut offsets, mut nulls) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..rows {
            if i % 7 == 0 {
                nulls.push(1);
            } else {
                nulls.push(0);
                data.extend_from_slice(format!("base-{i}{}", "x".repeat(i % 5)).as_bytes());
            }
            offsets.extend_from_slice(&(data.len() as u64).to_le_bytes());
        }
        (data, offsets, nulls)
    }

    /// An overlay string column: `Some(text)` where a `SET` wrote, null elsewhere.
    fn overlay_column(cells: &[Option<&str>]) -> TypedColumn {
        let mut column = TypedColumn::from_type_str("string");
        for cell in cells {
            match cell {
                Some(text) => column.push(&Value::String((*text).to_string())).unwrap(),
                None => column.push_null(),
            }
        }
        column
    }

    fn bytes(parts: BaseStr<'_>) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let take = |part: Option<Cow<'_, [u8]>>| part.map(Cow::into_owned).unwrap_or_default();
        (take(parts.0), take(parts.1), take(parts.2))
    }

    /// The run-at-a-time rebuild and the per-row one agree for overlays that
    /// replace nothing, a few rows, the first and last rows, and every row, with
    /// longer, shorter and empty cells, and a cell over a null base row.
    #[test]
    fn a_merged_string_column_is_the_same_whether_rebuilt_by_runs_or_by_rows() {
        let rows = 300;
        let (data, offsets, nulls) = base_column(rows);
        let regions = || (Some(&data[..]), Some(&offsets[..]), Some(&nulls[..]));
        let overlays: Vec<Vec<Option<&str>>> = {
            let nothing = vec![None; rows];
            let mut few = nothing.clone();
            few[3] = Some("replacement much longer than the base cell");
            few[14] = Some("");
            few[100] = Some("s");
            few[7] = Some("over a null base row");
            let mut edges = nothing.clone();
            edges[0] = Some("first");
            edges[rows - 1] = Some("last");
            let all: Vec<Option<&str>> = (0..rows).map(|_| Some("everything")).collect();
            vec![nothing, few, edges, all]
        };
        for cells in &overlays {
            let overlay = overlay_column(cells);
            let by_runs = bytes(merged_str(Some(regions()), &overlay, rows));
            let by_rows = bytes(merged_str_rows(
                Some(&data),
                Some(&offsets),
                Some(&nulls),
                &overlay,
                rows,
            ));
            assert_eq!(by_runs.0, by_rows.0, "data");
            assert_eq!(by_runs.1, by_rows.1, "offsets");
            assert_eq!(by_runs.2, by_rows.2, "nulls");
        }
        // No base at all: a column the overlay creates.
        let overlay = overlay_column(&[None, Some("a"), None, Some("bc")]);
        let created = bytes(merged_str(None, &overlay, 4));
        assert_eq!(created.0, b"abc");
        assert_eq!(created.2, vec![1, 0, 1, 0]);
        let expected_ends: Vec<u8> = [0u64, 1, 1, 3]
            .iter()
            .flat_map(|end| end.to_le_bytes())
            .collect();
        assert_eq!(created.1, expected_ends);
    }

    #[test]
    fn a_base_whose_offsets_are_not_the_layouts_takes_the_bounds_checked_path() {
        let rows = 10;
        let (data, mut offsets, nulls) = base_column(rows);
        // An end offset past the bytes the base holds.
        offsets[8..16].copy_from_slice(&(data.len() as u64 + 99).to_le_bytes());
        let overlay = overlay_column(&vec![None; rows]);
        let merged = bytes(merged_str(
            Some((Some(&data), Some(&offsets), Some(&nulls))),
            &overlay,
            rows,
        ));
        assert_eq!(merged.1.len(), rows * 8, "one end offset per row, no panic");
    }
}
