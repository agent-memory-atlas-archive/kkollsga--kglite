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

use super::{fixed_width_parts, pack_str_column, Part, PlannedType, RegionPlanner};
use crate::graph::io::ntriples::{
    ColMapEntry, ColumnTypeMeta, FixedColMeta, RegionMeta, StrColMeta,
};
use crate::graph::schema::InternedKey;
use crate::graph::storage::column_store::RegionParts;
use crate::graph::storage::column_store::{ColumnStore, TypedColumn};
use crate::graph::storage::mapped::column_store::{ColRef, MmapColumnStore, Region};
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

fn tag_width(tag: &str) -> Option<usize> {
    Some(match tag {
        "int64" | "float64" | "timestamp" => 8,
        "uniqueid" | "date" => 4,
        "bool" => 1,
        _ => return None,
    })
}

fn width_of(col_type: ColType) -> usize {
    match col_type {
        ColType::Int64 | ColType::Float64 | ColType::Timestamp => 8,
        ColType::UniqueId | ColType::Date => 4,
        ColType::Bool => 1,
        ColType::Str => 0,
    }
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

struct Identity {
    id_is_string: bool,
    id_data: RegionMeta,
    id_nulls: RegionMeta,
    id_str_data: RegionMeta,
    id_str_offsets: RegionMeta,
    title_data: RegionMeta,
    title_offsets: RegionMeta,
    title_nulls: RegionMeta,
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
    let mut col_map = Vec::with_capacity(keys.len());
    let mut fixed_cols = Vec::new();
    let mut str_cols = Vec::new();
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
        if is_string {
            let base = match base_ref {
                Some(ColRef::Str(i)) => {
                    let column = &ms.str_cols[i];
                    Some((
                        base_bytes(ms, &column.data),
                        base_bytes(ms, &column.offsets),
                        base_bytes(ms, &column.nulls),
                    ))
                }
                _ => None,
            };
            let base = match overlay_column {
                Some(column) => Some(merged_str(base, column, base_rows)),
                None => base.map(|(d, o, n)| {
                    (
                        d.map(Cow::Borrowed),
                        o.map(Cow::Borrowed),
                        n.map(Cow::Borrowed),
                    )
                }),
            };
            let tail_packed = match tail_column {
                Some(TypedColumn::Str {
                    offsets,
                    data,
                    nulls,
                    relocated,
                }) => Some(pack_str_column(offsets, data, nulls, relocated)),
                _ => None,
            };
            let parts = str_parts(base, tail_packed, base_rows, tail_rows)?;
            col_map.push(ColMapEntry {
                key_u64: key.as_u64(),
                col_type_str: "string".into(),
                idx: str_cols.len(),
            });
            str_cols.push(StrColMeta {
                data: planner.push_parts(parts.data),
                offsets: planner.push_parts(parts.offsets),
                nulls: planner.push_parts(parts.nulls),
            });
        } else {
            let tail_parts = tail_column.and_then(fixed_width_parts);
            let (tag, width, base_data, base_nulls) = match (base_ref, overlay_column) {
                (Some(ColRef::Fixed(i)), _) => {
                    let column = &ms.fixed_cols[i];
                    (
                        column.col_type.type_tag(),
                        width_of(column.col_type),
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
            let (base_data, base_nulls) = match overlay_column {
                Some(column) => {
                    let (data, nulls) =
                        merged_fixed(base_data, base_nulls, column, base_rows, width)?;
                    (Some(Cow::Owned(data)), Some(Cow::Owned(nulls)))
                }
                None => (base_data.map(Cow::Borrowed), base_nulls.map(Cow::Borrowed)),
            };
            let (mut data, mut nulls) = fixed_part(base_data, base_nulls, base_rows, width);
            let (tail_data, tail_nulls) = fixed_part(
                tail_parts.map(|(_, bytes, _)| Cow::Borrowed(bytes)),
                tail_parts.map(|(_, _, bytes)| Cow::Borrowed(bytes)),
                tail_rows,
                width,
            );
            data.extend(tail_data);
            nulls.extend(tail_nulls);
            col_map.push(ColMapEntry {
                key_u64: key.as_u64(),
                col_type_str: tag.into(),
                idx: fixed_cols.len(),
            });
            fixed_cols.push(FixedColMeta {
                col_type_str: tag.into(),
                data: planner.push_parts(data),
                nulls: planner.push_parts(nulls),
            });
        }
    }

    let meta = ColumnTypeMeta {
        type_name: type_name.to_string(),
        row_count: (base_rows + tail_rows) as u32,
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
        overflow_offsets: RegionPlanner::absent(),
        overflow_data: RegionPlanner::absent(),
        has_overflow: false,
    };
    Some(planner.finish(type_name, meta))
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
fn merged_str<'a>(
    base: Option<BaseRegions<'a>>,
    overlay: &TypedColumn,
    rows: usize,
) -> BaseStr<'a> {
    let (base_data, base_offsets, base_nulls) = base.unwrap_or((None, None, None));
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
) -> Option<Identity> {
    let tail_id = tail.and_then(ColumnStore::id_column_ref);
    let tail_title = tail.and_then(ColumnStore::title_column_ref);
    let absent = RegionPlanner::absent;
    let mut identity = Identity {
        id_is_string: ms.id_is_string,
        id_data: absent(),
        id_nulls: absent(),
        id_str_data: absent(),
        id_str_offsets: absent(),
        title_data: absent(),
        title_offsets: absent(),
        title_nulls: absent(),
    };
    if ms.has_id_column() {
        if ms.id_is_string {
            let column = ms.id_str.as_ref()?;
            let tail_packed = match tail_id {
                Some(TypedColumn::Str {
                    offsets,
                    data,
                    nulls,
                    relocated,
                }) => Some(pack_str_column(offsets, data, nulls, relocated)),
                _ => None,
            };
            let parts = str_parts(
                Some((
                    base_bytes(ms, &column.data).map(Cow::Borrowed),
                    base_bytes(ms, &column.offsets).map(Cow::Borrowed),
                    base_bytes(ms, &column.nulls).map(Cow::Borrowed),
                )),
                tail_packed,
                base_rows,
                tail_rows,
            )?;
            identity.id_str_data = planner.push_parts(parts.data);
            identity.id_str_offsets = planner.push_parts(parts.offsets);
            identity.id_nulls = planner.push_parts(parts.nulls);
        } else {
            let column = ms.id_fixed.as_ref()?;
            let width = width_of(column.col_type);
            let tail_parts = tail_id.and_then(fixed_width_parts);
            let (mut data, mut nulls) = fixed_part(
                base_bytes(ms, &column.data).map(Cow::Borrowed),
                base_bytes(ms, &column.nulls).map(Cow::Borrowed),
                base_rows,
                width,
            );
            let (tail_data, tail_nulls) = fixed_part(
                tail_parts.map(|(_, bytes, _)| Cow::Borrowed(bytes)),
                tail_parts.map(|(_, _, bytes)| Cow::Borrowed(bytes)),
                tail_rows,
                width,
            );
            data.extend(tail_data);
            nulls.extend(tail_nulls);
            identity.id_data = planner.push_parts(data);
            identity.id_nulls = planner.push_parts(nulls);
        }
    }
    if ms.has_title_column() {
        if ms.title_is_int() {
            let tail_parts = tail_title.and_then(fixed_width_parts);
            let (mut data, mut nulls) = fixed_part(
                base_bytes(ms, &ms.title.data).map(Cow::Borrowed),
                base_bytes(ms, &ms.title.nulls).map(Cow::Borrowed),
                base_rows,
                8,
            );
            let (tail_data, tail_nulls) = fixed_part(
                tail_parts.map(|(_, bytes, _)| Cow::Borrowed(bytes)),
                tail_parts.map(|(_, _, bytes)| Cow::Borrowed(bytes)),
                tail_rows,
                8,
            );
            data.extend(tail_data);
            nulls.extend(tail_nulls);
            identity.title_data = planner.push_parts(data);
            identity.title_nulls = planner.push_parts(nulls);
        } else {
            let tail_packed = match tail_title {
                Some(TypedColumn::Str {
                    offsets,
                    data,
                    nulls,
                    relocated,
                }) => Some(pack_str_column(offsets, data, nulls, relocated)),
                _ => None,
            };
            let parts = str_parts(
                Some((
                    base_bytes(ms, &ms.title.data).map(Cow::Borrowed),
                    base_bytes(ms, &ms.title.offsets).map(Cow::Borrowed),
                    base_bytes(ms, &ms.title.nulls).map(Cow::Borrowed),
                )),
                tail_packed,
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
