//! Plan a type's column file from an mmap base plus a tail store: each region
//! is the base's bytes followed by the tail's, so a save writes the base
//! straight from its mapping and only the tail's rows are built.
//!
//! Precondition (`ColumnStore::base_and_tail`): the base is nothing but its
//! mapping, and every column the tail carries has the base's kind for that
//! key. A column one part lacks is written as nulls for that part's rows.

use std::borrow::Cow;

use super::{fixed_width_parts, pack_str_column, Part, PlannedType, RegionPlanner};
use crate::graph::io::ntriples::{
    ColMapEntry, ColumnTypeMeta, FixedColMeta, RegionMeta, StrColMeta,
};
use crate::graph::schema::InternedKey;
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
type BaseStr<'a> = (Option<&'a [u8]>, Option<&'a [u8]>, Option<&'a [u8]>);
/// The tail's string column packed as `(data, offsets, nulls)` bytes.
type TailStr<'a> = (Cow<'a, [u8]>, Cow<'a, [u8]>, Cow<'a, [u8]>);

fn str_parts<'a>(
    base: Option<BaseStr<'a>>,
    tail: Option<TailStr<'a>>,
    base_rows: usize,
    tail_rows: usize,
) -> Option<StrParts<'a>> {
    let (base_data, base_offsets, base_nulls) = base.unwrap_or((None, None, None));
    let base_data_len = base_data.map_or(0, <[u8]>::len);
    // An absent offsets region stands for `base_rows` zero ends, which only
    // describes a base with no string bytes.
    if base_offsets.is_none() && base_data_len != 0 {
        return None;
    }
    let mut data = Vec::new();
    let mut offsets = Vec::new();
    if let Some(bytes) = base_data {
        data.push(Part::Bytes(Cow::Borrowed(bytes)));
    }
    offsets.extend(sized(base_offsets.map(Cow::Borrowed), base_rows * 8, 0));
    let mut nulls = null_parts(base_nulls.map(Cow::Borrowed), base_rows);

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

pub(super) fn plan_base_and_tail<'a>(
    type_name: &str,
    ms: &'a MmapColumnStore,
    tail: &'a ColumnStore,
) -> Option<PlannedType<'a>> {
    let base_rows = ms.row_count as usize;
    let tail_rows = tail.row_count() as usize;
    let mut planner = RegionPlanner::default();
    let identity = plan_identity(&mut planner, ms, tail, base_rows, tail_rows)?;

    // Every key either part holds, in key order.
    let mut keys: Vec<InternedKey> = ms.col_map.keys().copied().collect();
    for (slot, key) in tail.schema().iter() {
        if tail.column(slot as usize).is_some() && !ms.col_map.contains_key(&key) {
            keys.push(key);
        }
    }
    keys.sort_by_key(|key| key.as_u64());
    let mut col_map = Vec::with_capacity(keys.len());
    let mut fixed_cols = Vec::new();
    let mut str_cols = Vec::new();
    for key in keys {
        let tail_column = tail.column_for_plan(key, ms.column_kind(key));
        let base_ref = ms.col_map.get(&key).copied();
        let is_string = match base_ref {
            Some(ColRef::Str(_)) => true,
            Some(ColRef::Fixed(_)) => false,
            None => matches!(tail_column, Some(TypedColumn::Str { .. })),
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
            let (tag, width, base_data, base_nulls) = match base_ref {
                Some(ColRef::Fixed(i)) => {
                    let column = &ms.fixed_cols[i];
                    (
                        column.col_type.type_tag(),
                        width_of(column.col_type),
                        base_bytes(ms, &column.data),
                        base_bytes(ms, &column.nulls),
                    )
                }
                _ => {
                    let (tag, ..) = tail_parts?;
                    (tag, tag_width(tag)?, None, None)
                }
            };
            let (mut data, mut nulls) = fixed_part(
                base_data.map(Cow::Borrowed),
                base_nulls.map(Cow::Borrowed),
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

/// Regions of the id and title columns: the base's bytes, then the tail's.
/// The tail has an identity column only where the base has one of the same
/// kind (`ColumnStore::tail_is_region_compatible`).
fn plan_identity<'a>(
    planner: &mut RegionPlanner<'a>,
    ms: &'a MmapColumnStore,
    tail: &'a ColumnStore,
    base_rows: usize,
    tail_rows: usize,
) -> Option<Identity> {
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
            let tail_packed = match tail.id_column_ref() {
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
                    base_bytes(ms, &column.data),
                    base_bytes(ms, &column.offsets),
                    base_bytes(ms, &column.nulls),
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
            let tail_parts = tail.id_column_ref().and_then(fixed_width_parts);
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
            let tail_parts = tail.title_column_ref().and_then(fixed_width_parts);
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
            let tail_packed = match tail.title_column_ref() {
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
                    base_bytes(ms, &ms.title.data),
                    base_bytes(ms, &ms.title.offsets),
                    base_bytes(ms, &ms.title.nulls),
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
